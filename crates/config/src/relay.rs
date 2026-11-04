//! Outgoing SMTP for relaying and forwarding, from a YAML file (`--smtp-relay-config`,
//! `--smtp-forward-config`) and `SPOOLBOX_SMTP_RELAY_*` / `SPOOLBOX_SMTP_FORWARD_*` or their
//! `MP_*` equivalents. Variables win over the file. Everything is off until a host is configured.

use std::path::Path;

use serde::Deserialize;
use spoolbox_core::ListenAddr;

use crate::settings::{
    ForwardSettings, RelaySettings, SmtpLogin, SmtpMechanism, SmtpSecurity, SmtpTarget,
};

/// Messages per minute unless `rate-limit` says otherwise; excess messages wait.
const DEFAULT_RATE_LIMIT: u32 = 60;
const DEFAULT_PORT: u16 = 25;

/// A setting of the outgoing SMTP target (`MP_SMTP_RELAY_*`, `MP_SMTP_FORWARD_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Host,
    Port,
    StartTls,
    Tls,
    AllowInsecure,
    Auth,
    Username,
    Password,
    ReturnPath,
    OverrideFrom,
    AllowedRecipients,
    BlockedRecipients,
    PreserveMessageIds,
    To,
    /// Messages per minute. No `MP_*` variable.
    RateLimit,
}

impl Field {
    pub(crate) fn is_bool(self) -> bool {
        matches!(
            self,
            Field::StartTls | Field::Tls | Field::AllowInsecure | Field::PreserveMessageIds
        )
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Addresses {
    One(String),
    Many(Vec<String>),
}

impl Default for Addresses {
    fn default() -> Self {
        Addresses::Many(Vec::new())
    }
}

/// One source of target settings; `None` means "not set here".
#[derive(Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct RawTarget {
    host: Option<String>,
    port: Option<u16>,
    starttls: Option<bool>,
    tls: Option<bool>,
    allow_insecure: Option<bool>,
    auth: Option<String>,
    username: Option<String>,
    password: Option<String>,
    return_path: Option<String>,
    override_from: Option<String>,
    /// Messages per minute; 0 = no limit.
    rate_limit: Option<u32>,
    allowed_recipients: Option<String>,
    blocked_recipients: Option<String>,
    preserve_message_ids: Option<bool>,
    to: Option<Addresses>,
    /// Delivery is asynchronous, so there is no SMTP session to report errors to.
    #[serde(rename = "forward-smtp-errors")]
    _forward_smtp_errors: Option<bool>,
}

impl std::fmt::Debug for RawTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawTarget")
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

impl RawTarget {
    /// Applies an `MP_*` variable. Booleans use the compatibility rules.
    pub(crate) fn set(&mut self, field: Field, value: &str) -> Result<(), &'static str> {
        let text = || Some(value.to_string());
        let flag = || Some(crate::key::parse_compat_bool(value));
        match field {
            Field::Host => self.host = text(),
            Field::Port => {
                self.port = Some(value.parse().map_err(|_| "expected a port number")?);
            }
            Field::StartTls => self.starttls = flag(),
            Field::Tls => self.tls = flag(),
            Field::AllowInsecure => self.allow_insecure = flag(),
            Field::Auth => self.auth = text(),
            Field::Username => self.username = text(),
            Field::Password => self.password = text(),
            Field::ReturnPath => self.return_path = text(),
            Field::OverrideFrom => self.override_from = text(),
            Field::AllowedRecipients => self.allowed_recipients = text(),
            Field::BlockedRecipients => self.blocked_recipients = text(),
            Field::PreserveMessageIds => self.preserve_message_ids = flag(),
            Field::To => self.to = Some(Addresses::One(value.to_string())),
            Field::RateLimit => {
                self.rate_limit = Some(
                    value
                        .parse()
                        .map_err(|_| "expected messages per minute (0 = no limit)")?,
                );
            }
        }
        Ok(())
    }

    /// Fills everything not set here from `base`.
    fn or(self, base: RawTarget) -> RawTarget {
        RawTarget {
            host: self.host.or(base.host),
            port: self.port.or(base.port),
            starttls: self.starttls.or(base.starttls),
            tls: self.tls.or(base.tls),
            allow_insecure: self.allow_insecure.or(base.allow_insecure),
            auth: self.auth.or(base.auth),
            username: self.username.or(base.username),
            password: self.password.or(base.password),
            return_path: self.return_path.or(base.return_path),
            override_from: self.override_from.or(base.override_from),
            rate_limit: self.rate_limit.or(base.rate_limit),
            allowed_recipients: self.allowed_recipients.or(base.allowed_recipients),
            blocked_recipients: self.blocked_recipients.or(base.blocked_recipients),
            preserve_message_ids: self.preserve_message_ids.or(base.preserve_message_ids),
            to: self.to.or(base.to),
            _forward_smtp_errors: None,
        }
    }

    fn is_empty(&self) -> bool {
        self.host.is_none()
            && self.port.is_none()
            && self.starttls.is_none()
            && self.tls.is_none()
            && self.allow_insecure.is_none()
            && self.auth.is_none()
            && self.username.is_none()
            && self.password.is_none()
            && self.return_path.is_none()
            && self.override_from.is_none()
            && self.rate_limit.is_none()
            && self.allowed_recipients.is_none()
            && self.blocked_recipients.is_none()
            && self.preserve_message_ids.is_none()
            && self.to.is_none()
    }
}

/// Used for return paths, `to` lists and API recipients: a bare `local@domain` that cannot
/// carry extra SMTP parameters or headers.
pub fn is_plain_address(addr: &str) -> bool {
    let Some((local, domain)) = addr.rsplit_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && !addr
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "<>,;:\"()[]\\".contains(c))
}

fn address(value: Option<String>, name: &str) -> Result<Option<String>, String> {
    match value {
        Some(v) if !is_plain_address(&v) => Err(format!("{name} is not a valid address: {v:?}")),
        other => Ok(other),
    }
}

fn regex(value: Option<String>, name: &str) -> Result<Option<String>, String> {
    if let Some(v) = &value {
        regex::Regex::new(v).map_err(|e| format!("{name}: invalid regular expression: {e}"))?;
    }
    Ok(value)
}

fn read_file(path: &str) -> Result<RawTarget, String> {
    let text =
        std::fs::read_to_string(Path::new(path)).map_err(|e| format!("cannot read {path}: {e}"))?;
    parse_yaml(&text).map_err(|e| format!("{path}: {e}"))
}

fn parse_yaml(text: &str) -> Result<RawTarget, String> {
    // Only the first line: the rest quotes the file, which holds a password.
    serde_saphyr::from_str(text).map_err(|e| {
        let message = e.to_string();
        message.lines().next().unwrap_or("invalid YAML").to_string()
    })
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn login(raw: &mut RawTarget) -> Result<Option<SmtpLogin>, String> {
    let mechanism = match raw.auth.as_deref().map(str::to_ascii_lowercase).as_deref() {
        None if raw.username.is_none() && raw.password.is_none() => return Ok(None),
        None | Some("plain") => SmtpMechanism::Plain,
        Some("login") => SmtpMechanism::Login,
        Some("none") => {
            if raw.username.is_some() || raw.password.is_some() {
                return Err("auth is none but a username or password is set".into());
            }
            return Ok(None);
        }
        Some("cram-md5") => return Err("CRAM-MD5 is not supported; use plain or login".into()),
        Some(other) => {
            return Err(format!(
                "unknown auth {other:?} (expected none, plain or login)"
            ));
        }
    };
    match (raw.username.take(), raw.password.take()) {
        (Some(username), Some(password)) => Ok(Some(SmtpLogin {
            mechanism,
            username,
            password,
        })),
        _ => Err("authentication needs both a username and a password".into()),
    }
}

fn target(raw: &mut RawTarget, listen: &ListenAddr) -> Result<Option<SmtpTarget>, String> {
    let Some(host) = raw.host.take().filter(|h| !h.trim().is_empty()) else {
        return if raw.is_empty() {
            Ok(None)
        } else {
            Err("a host is required".into())
        };
    };
    let port = raw.port.unwrap_or(DEFAULT_PORT);
    if port == listen.port && is_loopback_host(&host) {
        return Err(format!(
            "{host}:{port} is this server's own SMTP port, so every message would come back"
        ));
    }
    let security = match (raw.starttls.unwrap_or(false), raw.tls.unwrap_or(false)) {
        (true, true) => return Err("starttls and tls cannot be used together".into()),
        (true, false) => SmtpSecurity::StartTls,
        (false, true) => SmtpSecurity::Tls,
        (false, false) => SmtpSecurity::None,
    };
    let allow_insecure = raw.allow_insecure.unwrap_or(false);
    let login = login(raw)?;
    if login.is_some()
        && security == SmtpSecurity::None
        && !allow_insecure
        && !is_loopback_host(&host)
    {
        return Err(
            "the password would be sent unencrypted: set starttls or tls, or allow-insecure to accept that"
                .into(),
        );
    }
    Ok(Some(SmtpTarget {
        host,
        port,
        security,
        allow_insecure,
        login,
        return_path: address(raw.return_path.take(), "return-path")?,
        override_from: address(raw.override_from.take(), "override-from")?,
        preserve_message_ids: raw.preserve_message_ids.unwrap_or(false),
        limit_per_minute: raw.rate_limit.unwrap_or(DEFAULT_RATE_LIMIT),
    }))
}

/// Combines the file and the variables, then applies `--smtp-relay-all` and
/// `--smtp-relay-matching`. Relaying everything is only possible with an explicit allowlist.
pub(crate) fn resolve_relay(
    file: Option<&str>,
    env: RawTarget,
    all: bool,
    matching: Option<String>,
    listen: &ListenAddr,
) -> Result<RelaySettings, String> {
    let mut raw = match file {
        Some(path) => env.or(read_file(path)?),
        None => env,
    };
    if raw.to.is_some() {
        return Err("to is only for forwarding (--smtp-forward-config)".into());
    }
    let allowed_recipients = regex(raw.allowed_recipients.take(), "allowed-recipients")?;
    let blocked_recipients = regex(raw.blocked_recipients.take(), "blocked-recipients")?;
    let matching = regex(matching, "--smtp-relay-matching")?;
    let target = target(&mut raw, listen)?;
    if all && matching.is_some() {
        return Err("--smtp-relay-all and --smtp-relay-matching cannot be used together".into());
    }
    if target.is_none() && (all || matching.is_some()) {
        return Err("automatic relaying needs a host".into());
    }
    if all && allowed_recipients.is_none() {
        return Err(
            "--smtp-relay-all needs allowed-recipients, so captured mail cannot reach arbitrary addresses"
                .into(),
        );
    }
    Ok(RelaySettings {
        target,
        all,
        matching,
        allowed_recipients,
        blocked_recipients,
    })
}

pub(crate) fn resolve_forward(
    file: Option<&str>,
    env: RawTarget,
    listen: &ListenAddr,
) -> Result<ForwardSettings, String> {
    let mut raw = match file {
        Some(path) => env.or(read_file(path)?),
        None => env,
    };
    if raw.allowed_recipients.is_some() || raw.blocked_recipients.is_some() {
        return Err("allowed-recipients and blocked-recipients are only for relaying".into());
    }
    let to: Vec<String> = match raw.to.take().unwrap_or_default() {
        Addresses::One(list) => list
            .split(',')
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect(),
        Addresses::Many(list) => list,
    };
    if let Some(bad) = to.iter().find(|a| !is_plain_address(a)) {
        return Err(format!("to is not a valid address: {bad:?}"));
    }
    let target = target(&mut raw, listen)?;
    match (&target, to.is_empty()) {
        (Some(_), true) => return Err("forwarding needs at least one address in to".into()),
        (None, false) => return Err("a host is required".into()),
        _ => {}
    }
    Ok(ForwardSettings { target, to })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_addresses_only() {
        assert!(is_plain_address("a.b+c@example.com"));
        for bad in [
            "",
            "a",
            "@b",
            "a@",
            "a b@c",
            "a@b\r\nRCPT TO:<x@y>",
            "<a@b>",
            "a@b,c@d",
            "A <a@b>",
        ] {
            assert!(!is_plain_address(bad), "{bad:?}");
        }
    }

    #[test]
    fn yaml_keys() {
        let raw = parse_yaml(
            "host: smtp.example.com\nport: 587\nstarttls: true\nallow-insecure: false\nauth: login\nusername: u\npassword: p\nreturn-path: b@example.com\noverride-from: f@example.com\nrate-limit: 5\nallowed-recipients: '@example\\.com$'\nblocked-recipients: ceo@\npreserve-message-ids: true\nforward-smtp-errors: true\n",
        )
        .expect("yaml");
        assert_eq!(raw.port, Some(587));
        assert_eq!(raw.rate_limit, Some(5));
        assert!(
            parse_yaml("hostt: x\n").is_err(),
            "unknown keys are refused"
        );
    }

    #[test]
    fn yaml_errors_do_not_show_the_password() {
        for text in [
            "host: h\npassword: s3cret\nport: nope\n",
            "host: h\npassword: [s3cret\n",
            "password: s3cret\nhostt: x\n",
            "host: h\npassword: s3cret\n  bad indent: x\n",
        ] {
            let err = parse_yaml(text).expect_err(text);
            assert!(!err.contains("s3cret"), "{text:?} shows: {err}");
        }
    }
}
