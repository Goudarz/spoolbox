//! SMTP session state machine without I/O. The connection driver feeds command
//! lines and performs the [`Next`] action returned with each reply.

use std::collections::HashSet;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use regex::Regex;
use spoolbox_auth::Credentials;
use spoolbox_core::Envelope;

use crate::command::{self, Command, PathError};
use spoolbox_core::chaos::{Chaos, RuleStage};

#[derive(Debug, Clone, Default)]
pub struct RecipientPolicy {
    /// Only matching recipients are accepted (`--smtp-allowed-recipients`).
    pub allowed: Option<Regex>,
    /// Answer 250 for rejected recipients but drop them (`--smtp-ignore-rejected-recipients`).
    pub ignore_rejected: bool,
    /// Lower-case domains; empty = all domains accepted.
    pub accept_domains: Vec<String>,
    pub reject_domains: Vec<String>,
    /// Accepted with 250 but never stored.
    pub discard_domains: Vec<String>,
}

/// SMTP AUTH mode (`--smtp-auth-*`).
#[derive(Debug, Clone, Default)]
pub enum SmtpAuth {
    #[default]
    Off,
    /// `AUTH` is offered and any credentials are accepted; authentication is optional.
    AcceptAny,
    /// Credentials are checked and authentication is required before MAIL.
    Credentials(Arc<Credentials>),
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub hostname: String,
    /// Shown in the banner, e.g. `Spoolbox` or `Spoolbox (label)`.
    pub app_name: String,
    pub max_recipients: usize,
    /// Bytes; 0 = unlimited.
    pub max_message_size: u64,
    pub auth: SmtpAuth,
    /// STARTTLS can be offered (a certificate is configured and TLS is not implicit).
    pub starttls_available: bool,
    /// MAIL/RCPT/DATA/RSET/AUTH require TLS first (RFC 3207 §4).
    pub require_tls: bool,
    /// Upper-case extension keywords not advertised in EHLO (and refused when used).
    pub hidden_extensions: HashSet<String>,
    pub recipients: RecipientPolicy,
    /// Chaos rules and random failures (`--enable-chaos`).
    pub chaos: Option<Arc<Chaos>>,
}

impl SessionConfig {
    pub fn new(hostname: impl Into<String>) -> Self {
        Self {
            hostname: hostname.into(),
            app_name: "Spoolbox".into(),
            max_recipients: 100,
            max_message_size: 50 * 1024 * 1024,
            auth: SmtpAuth::Off,
            starttls_available: false,
            require_tls: false,
            hidden_extensions: HashSet::new(),
            recipients: RecipientPolicy::default(),
            chaos: None,
        }
    }

    fn offers(&self, ext: &str) -> bool {
        !self.hidden_extensions.contains(ext)
    }
}

/// What the connection driver must do after sending the reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Continue,
    /// Read the message body; then call [`Session::data_result`].
    ReadData,
    /// Perform the TLS handshake; then call [`Session::tls_established`].
    StartTls,
    /// Read one more line and pass it to [`Session::auth_response`].
    AuthContinue,
    Close,
    /// Close the connection without sending the reply (chaos `disconnect`).
    Drop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    pub next: Next,
}

impl Reply {
    fn line(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            next: Next::Continue,
        }
    }

    fn then(text: impl Into<String>, next: Next) -> Self {
        Self {
            text: text.into(),
            next,
        }
    }

    pub fn code(&self) -> u16 {
        self.text.get(..3).and_then(|c| c.parse().ok()).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// No HELO/EHLO yet (also after STARTTLS).
    Connected,
    Ready,
    Mail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthStep {
    PlainResponse,
    LoginUser,
    LoginPassword,
}

/// Result of receiving the message body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataOutcome {
    Stored(String),
    /// All recipients were dropped by policy; acknowledge without storing.
    Dropped,
    TooLarge,
    /// A complete SMTP reply line, e.g. from the storage layer (`452 4.3.1 ...`).
    Rejected(String),
}

#[derive(Debug, Clone)]
pub struct Session {
    cfg: Arc<SessionConfig>,
    phase: Phase,
    tls: Option<String>,
    remote_ip: String,
    helo: String,
    auth_user: Option<String>,
    auth: Option<(AuthStep, String)>,
    from: String,
    params: Vec<(String, Option<String>)>,
    rcpts: Vec<String>,
    dropped: usize,
    /// Delay requested by a chaos rule, applied by the server before the next reply.
    pause: std::time::Duration,
}

impl Session {
    pub fn new(cfg: Arc<SessionConfig>, remote_ip: impl Into<String>, tls: Option<String>) -> Self {
        Self {
            cfg,
            phase: Phase::Connected,
            tls,
            remote_ip: remote_ip.into(),
            helo: String::new(),
            auth_user: None,
            auth: None,
            from: String::new(),
            params: Vec::new(),
            rcpts: Vec::new(),
            dropped: 0,
            pause: std::time::Duration::ZERO,
        }
    }

    pub fn banner(&self) -> String {
        format!(
            "220 {} {} ESMTP Service ready",
            self.cfg.hostname, self.cfg.app_name
        )
    }

    pub fn timeout_reply(&self) -> String {
        format!(
            "421 4.4.2 {} {} ESMTP Service closing transmission channel after timeout exceeded",
            self.cfg.hostname, self.cfg.app_name
        )
    }

    pub fn max_message_size(&self) -> u64 {
        self.cfg.max_message_size
    }

    /// Envelope of the current transaction.
    pub fn envelope(&self) -> Envelope {
        Envelope {
            mail_from: self.from.clone(),
            mail_params: self.params.clone(),
            rcpt_to: self.rcpts.clone(),
            helo: self.helo.clone(),
            remote_ip: self.remote_ip.clone(),
            tls: self.tls.clone(),
            auth_user: self.auth_user.clone(),
        }
    }

    /// True when the message must be acknowledged but not stored.
    pub fn all_recipients_dropped(&self) -> bool {
        self.rcpts.is_empty() && self.dropped > 0
    }

    /// Applies a chaos rule or trigger: a delay is kept for [`Session::take_pause`]; a reply
    /// or a disconnect replaces the normal reply.
    fn chaos(&mut self, stage: RuleStage, subject: &str) -> Option<Reply> {
        let effect = self.cfg.chaos.as_ref()?.decide(stage, subject)?;
        self.pause += effect.delay;
        if effect.disconnect {
            return Some(Reply::then("", Next::Drop));
        }
        effect.reply.map(Reply::line)
    }

    /// Chaos for a new connection, before the greeting.
    pub fn connect_chaos(&mut self) -> Option<Reply> {
        let ip = self.remote_ip.clone();
        self.chaos(RuleStage::Connect, &ip)
    }

    /// Chaos after the body was received; the message is then not stored.
    pub fn message_chaos(&mut self) -> Option<Reply> {
        let from = self.from.clone();
        self.chaos(RuleStage::Message, &from)
    }

    /// The delay to wait before sending the next reply.
    pub fn take_pause(&mut self) -> std::time::Duration {
        std::mem::take(&mut self.pause)
    }

    fn reset_transaction(&mut self) {
        self.from.clear();
        self.params.clear();
        self.rcpts.clear();
        self.dropped = 0;
        if self.phase == Phase::Mail {
            self.phase = Phase::Ready;
        }
    }

    fn tls_gate(&self) -> Option<Reply> {
        (self.cfg.require_tls && self.tls.is_none())
            .then(|| Reply::line("530 5.7.0 Must issue a STARTTLS command first"))
    }

    /// TLS gate, then RFC 4954 section 4: with credentials configured, MAIL/RCPT/DATA need AUTH first.
    fn mail_gate(&self) -> Option<Reply> {
        self.tls_gate().or_else(|| {
            (matches!(self.cfg.auth, SmtpAuth::Credentials(_)) && self.auth_user.is_none())
                .then(|| Reply::line("530 5.7.0 Authentication required"))
        })
    }

    pub fn handle(&mut self, line: &str) -> Reply {
        match command::parse(line) {
            Command::Helo(name) => {
                self.reset_transaction();
                self.helo = name;
                self.phase = Phase::Ready;
                Reply::line(format!("250 {} greets {}", self.cfg.hostname, self.helo))
            }
            Command::Ehlo(name) => {
                self.reset_transaction();
                self.helo = name;
                self.phase = Phase::Ready;
                Reply::line(self.ehlo_reply())
            }
            Command::Mail { path, params } => self.mail(path, params),
            Command::Rcpt { path, params } => self.rcpt(path, params),
            Command::Data => self.data(),
            Command::Rset => {
                if let Some(r) = self.tls_gate() {
                    return r;
                }
                self.reset_transaction();
                Reply::line("250 2.0.0 Ok")
            }
            Command::Noop => Reply::line("250 2.0.0 Ok"),
            Command::Quit => Reply::then(
                format!(
                    "221 2.0.0 {} {} ESMTP Service closing transmission channel",
                    self.cfg.hostname, self.cfg.app_name
                ),
                Next::Close,
            ),
            Command::StartTls(args) => {
                if !args.is_empty() {
                    Reply::line("501 5.5.2 Syntax error (no parameters allowed)")
                } else if !self.cfg.starttls_available || !self.cfg.offers("STARTTLS") {
                    Reply::line("502 5.5.1 Command not implemented")
                } else if self.tls.is_some() {
                    Reply::line("503 5.5.1 Bad sequence of commands (TLS already in use)")
                } else {
                    Reply::then("220 2.0.0 Ready to start TLS", Next::StartTls)
                }
            }
            Command::Auth { mechanism, initial } => self.auth_start(&mechanism, initial),
            Command::Vrfy | Command::Expn | Command::Help => {
                Reply::line("502 5.5.1 Command not implemented")
            }
            Command::Unknown => Reply::line("500 5.5.2 Syntax error, command unrecognized"),
        }
    }

    fn ehlo_reply(&self) -> String {
        let c = &self.cfg;
        let mut ext = Vec::new();
        if c.offers("SIZE") {
            ext.push(format!("SIZE {}", c.max_message_size));
        }
        if c.offers("PIPELINING") {
            ext.push("PIPELINING".into());
        }
        if c.starttls_available && self.tls.is_none() && c.offers("STARTTLS") {
            ext.push("STARTTLS".into());
        }
        if self.auth_offered() {
            ext.push("AUTH PLAIN LOGIN".into());
        }
        for e in ["ENHANCEDSTATUSCODES", "8BITMIME", "SMTPUTF8"] {
            if c.offers(e) {
                ext.push(e.into());
            }
        }
        let mut out = format!("250-{} greets {}", c.hostname, self.helo);
        for (i, e) in ext.iter().enumerate() {
            out.push_str("\r\n");
            out.push_str(if i + 1 == ext.len() { "250 " } else { "250-" });
            out.push_str(e);
        }
        if ext.is_empty() {
            out.replace_range(3..4, " ");
        }
        out
    }

    fn auth_offered(&self) -> bool {
        // PLAIN/LOGIN are offered even without TLS; configuration decides
        // whether STARTTLS must come first (`--smtp-auth-allow-insecure`).
        !matches!(self.cfg.auth, SmtpAuth::Off) && self.cfg.offers("AUTH")
    }

    fn mail(
        &mut self,
        path: Result<String, PathError>,
        params: Result<Vec<(String, Option<String>)>, PathError>,
    ) -> Reply {
        if let Some(r) = self.mail_gate() {
            return r;
        }
        match self.phase {
            Phase::Connected => {
                return Reply::line(
                    "503 5.5.1 Bad sequence of commands (HELO/EHLO required before MAIL)",
                );
            }
            Phase::Mail => {
                return Reply::line(
                    "503 5.5.1 Bad sequence of commands (RSET/HELO/EHLO required before MAIL)",
                );
            }
            Phase::Ready => {}
        }
        let path = match path {
            Ok(p) => p,
            Err(e) => return path_error(e, "FROM"),
        };
        let Ok(params) = params else {
            return Reply::line("501 5.5.4 Syntax error in parameters or arguments");
        };
        for (k, v) in &params {
            match (k.as_str(), v.as_deref()) {
                ("SIZE", Some(v)) => match v.parse::<u64>() {
                    Ok(n) if self.cfg.max_message_size > 0 && n > self.cfg.max_message_size => {
                        return Reply::line(format!(
                            "552 5.3.4 Requested mail action aborted: exceeded storage allocation ({})",
                            self.cfg.max_message_size
                        ));
                    }
                    Ok(_) => {}
                    Err(_) => {
                        return Reply::line(
                            "501 5.5.4 Syntax error in parameters or arguments (invalid SIZE parameter)",
                        );
                    }
                },
                ("SIZE", None) => {
                    return Reply::line(
                        "501 5.5.4 Syntax error in parameters or arguments (invalid SIZE parameter)",
                    );
                }
                ("AUTH", v) if !v.is_some_and(command::valid_auth_param) => {
                    return Reply::line(
                        "501 5.5.4 Syntax error in parameters or arguments (invalid AUTH parameter)",
                    );
                }
                ("SMTPUTF8", _) if !self.cfg.offers("SMTPUTF8") => {
                    return Reply::line("555 5.5.4 SMTPUTF8 is not supported");
                }
                ("BODY", Some(b))
                    if b.eq_ignore_ascii_case("8BITMIME") && !self.cfg.offers("8BITMIME") =>
                {
                    return Reply::line("555 5.5.4 8BITMIME is not supported");
                }
                // Other parameters are accepted and recorded.
                _ => {}
            }
        }
        if let Some(r) = self.chaos(RuleStage::Sender, &path) {
            return r;
        }
        self.reset_transaction();
        self.from = path;
        self.params = params;
        self.phase = Phase::Mail;
        Reply::line("250 2.1.0 Ok")
    }

    fn rcpt(
        &mut self,
        path: Result<String, PathError>,
        params: Result<Vec<(String, Option<String>)>, PathError>,
    ) -> Reply {
        if let Some(r) = self.mail_gate() {
            return r;
        }
        if self.phase != Phase::Mail {
            return Reply::line("503 5.5.1 Bad sequence of commands (MAIL required before RCPT)");
        }
        let path = match path {
            Ok(p) => p,
            Err(e) => return path_error(e, "TO"),
        };
        if params.is_err() {
            return Reply::line("501 5.5.4 Syntax error in parameters or arguments");
        }
        if self.rcpts.len() + self.dropped >= self.cfg.max_recipients {
            return Reply::line("452 4.5.3 Too many recipients");
        }
        if let Some(r) = self.chaos(RuleStage::Recipient, &path) {
            return r;
        }
        match self.recipient_decision(&path) {
            Decision::Accept => {
                self.rcpts.push(path);
                Reply::line("250 2.1.5 Ok")
            }
            Decision::Drop => {
                self.dropped += 1;
                Reply::line("250 2.1.5 Ok")
            }
            Decision::Reject => {
                Reply::line("550 5.1.0 Requested action not taken: mailbox unavailable")
            }
        }
    }

    fn recipient_decision(&self, addr: &str) -> Decision {
        let p = &self.cfg.recipients;
        let domain = addr
            .rsplit_once('@')
            .map(|(_, d)| d.to_ascii_lowercase())
            .unwrap_or_default();
        let in_list = |list: &[String]| {
            list.iter()
                .any(|d| domain == *d || domain.ends_with(&format!(".{d}")))
        };
        let rejected = (!p.accept_domains.is_empty() && !in_list(&p.accept_domains))
            || in_list(&p.reject_domains)
            || p.allowed.as_ref().is_some_and(|re| !re.is_match(addr));
        if rejected {
            return if p.ignore_rejected {
                Decision::Drop
            } else {
                Decision::Reject
            };
        }
        if in_list(&p.discard_domains) {
            Decision::Drop
        } else {
            Decision::Accept
        }
    }

    fn data(&mut self) -> Reply {
        if let Some(r) = self.mail_gate() {
            return r;
        }
        if self.phase != Phase::Mail || (self.rcpts.is_empty() && self.dropped == 0) {
            return Reply::line(
                "503 5.5.1 Bad sequence of commands (MAIL & RCPT required before DATA)",
            );
        }
        let from = self.from.clone();
        if let Some(r) = self.chaos(RuleStage::Data, &from) {
            return r;
        }
        Reply::then(
            "354 Start mail input; end with <CR><LF>.<CR><LF>",
            Next::ReadData,
        )
    }

    /// Called after the body was read; resets the transaction.
    pub fn data_result(&mut self, outcome: DataOutcome) -> Reply {
        self.reset_transaction();
        match outcome {
            // An empty ID means "accepted but intentionally not stored" (e.g. duplicate Message-ID).
            DataOutcome::Stored(id) if id.is_empty() => Reply::line("250 2.0.0 Ok: queued"),
            DataOutcome::Stored(id) => Reply::line(format!("250 2.0.0 Ok: queued as {id}")),
            DataOutcome::Dropped => Reply::line("250 2.0.0 Ok: queued"),
            DataOutcome::TooLarge => Reply::line(format!(
                "552 5.3.4 Requested mail action aborted: exceeded storage allocation ({})",
                self.cfg.max_message_size
            )),
            DataOutcome::Rejected(line) => Reply::line(line),
        }
    }

    /// RFC 3207 §4.2: discard all knowledge from before the handshake.
    pub fn tls_established(&mut self, protocol: String) {
        self.reset_transaction();
        self.helo.clear();
        self.phase = Phase::Connected;
        self.tls = Some(protocol);
    }

    fn auth_start(&mut self, mechanism: &str, initial: Option<String>) -> Reply {
        if let Some(r) = self.tls_gate() {
            return r;
        }
        if !self.auth_offered() {
            return Reply::line("502 5.5.1 Command not implemented");
        }
        if self.auth_user.is_some() {
            return Reply::line(
                "503 5.5.1 Bad sequence of commands (already authenticated for this session)",
            );
        }
        if self.phase == Phase::Mail {
            return Reply::line(
                "503 5.5.1 Bad sequence of commands (AUTH not permitted during mail transaction)",
            );
        }
        if mechanism.is_empty() {
            return Reply::line("501 5.5.4 Malformed AUTH input (argument required)");
        }
        if !matches!(mechanism, "PLAIN" | "LOGIN") {
            return Reply::line("504 5.5.4 Unrecognized authentication type");
        }
        if let Some(r) = self.chaos(RuleStage::Auth, "") {
            return r;
        }
        match (mechanism, initial) {
            ("PLAIN", Some(resp)) => self.auth_plain(&resp),
            ("PLAIN", None) => {
                self.auth = Some((AuthStep::PlainResponse, String::new()));
                Reply::then("334 ", Next::AuthContinue)
            }
            ("LOGIN", Some(user)) => match decode(&user) {
                Some(u) => {
                    self.auth = Some((AuthStep::LoginPassword, u));
                    Reply::then("334 UGFzc3dvcmQ6", Next::AuthContinue)
                }
                None => Reply::line("501 5.5.2 Syntax error (unable to decode)"),
            },
            ("LOGIN", None) => {
                self.auth = Some((AuthStep::LoginUser, String::new()));
                Reply::then("334 VXNlcm5hbWU6", Next::AuthContinue)
            }
            _ => Reply::line("504 5.5.4 Unrecognized authentication type"),
        }
    }

    pub fn auth_response(&mut self, line: &str) -> Reply {
        let Some((step, user)) = self.auth.take() else {
            return Reply::line("503 5.5.1 Bad sequence of commands");
        };
        if line.trim() == "*" {
            return Reply::line("501 5.7.0 Authentication cancelled");
        }
        match step {
            AuthStep::PlainResponse => self.auth_plain(line.trim()),
            AuthStep::LoginUser => match decode(line.trim()) {
                Some(u) => {
                    self.auth = Some((AuthStep::LoginPassword, u));
                    Reply::then("334 UGFzc3dvcmQ6", Next::AuthContinue)
                }
                None => Reply::line("501 5.5.2 Syntax error (unable to decode)"),
            },
            AuthStep::LoginPassword => match decode(line.trim()) {
                Some(password) => self.authenticated(user, &password),
                None => Reply::line("501 5.5.2 Syntax error (unable to decode)"),
            },
        }
    }

    fn auth_plain(&mut self, resp: &str) -> Reply {
        let Some(raw) = B64.decode(resp).ok() else {
            return Reply::line("501 5.5.2 Syntax error (unable to decode)");
        };
        let fields: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
        if fields.len() != 3 {
            return Reply::line("501 5.5.2 Syntax error (unable to parse)");
        }
        self.authenticated(
            String::from_utf8_lossy(fields[1]).into_owned(),
            &String::from_utf8_lossy(fields[2]),
        )
    }

    fn authenticated(&mut self, user: String, password: &str) -> Reply {
        let ok = match &self.cfg.auth {
            SmtpAuth::Off => false,
            SmtpAuth::AcceptAny => true,
            SmtpAuth::Credentials(c) => c.verify(&user, password),
        };
        if !ok {
            return Reply::line("535 5.7.8 Authentication credentials invalid");
        }
        self.auth_user = Some(user);
        Reply::line("235 2.7.0 Authentication successful")
    }
}

enum Decision {
    Accept,
    Drop,
    Reject,
}

fn decode(s: &str) -> Option<String> {
    B64.decode(s)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn path_error(e: PathError, keyword: &str) -> Reply {
    Reply::line(match e {
        PathError::Syntax => format!(
            "501 5.5.4 Syntax error in parameters or arguments (invalid {keyword} parameter)"
        ),
        PathError::InvalidAddress => "553 5.1.3 The address is not a valid RFC 5321 address".into(),
        PathError::TooLong => "500 5.5.4 The address is too long".into(),
    })
}

#[cfg(test)]
mod tests;
