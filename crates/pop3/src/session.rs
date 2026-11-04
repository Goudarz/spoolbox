//! The POP3 state machine. Messages are numbered when the user logs in and keep their numbers
//! for the whole session. Deletions only take effect on `QUIT`, so a lost connection deletes
//! nothing (RFC 1939 section 6).

/// Failed logins before the connection is closed.
const MAX_FAILURES: u32 = 3;

/// One message of the maildrop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Stable unique ID, shown by `UIDL`.
    pub id: String,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionConfig {
    /// The server has a certificate.
    pub stls_available: bool,
    /// `USER` and `PASS` are refused until the connection is encrypted.
    pub require_tls: bool,
}

/// What the server must do next. Reply texts have no final line break.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Reply(String),
    /// Send this reply, then close the connection.
    Close(String),
    /// Send this reply, then do the TLS handshake and call [`Session::tls_established`].
    StartTls(String),
    /// Check the password and call [`Session::authenticated`] or [`Session::auth_failed`].
    Authenticate {
        user: String,
        password: String,
    },
    /// Send `+OK`, then the message from [`encode`] with this line limit for the body.
    Send {
        id: String,
        size: u64,
        lines: Option<usize>,
    },
    /// The session is over: delete these messages, then say goodbye and close.
    Update {
        delete: Vec<String>,
    },
}

#[derive(Debug)]
struct Message {
    entry: Entry,
    deleted: bool,
}

#[derive(Debug)]
enum State {
    Authorization { user: Option<String> },
    Transaction { messages: Vec<Message> },
}

#[derive(Debug)]
pub struct Session {
    cfg: SessionConfig,
    tls: bool,
    failures: u32,
    state: State,
}

fn err(text: &str) -> Step {
    Step::Reply(format!("-ERR {text}"))
}

impl Session {
    pub fn new(cfg: SessionConfig, tls: bool) -> Self {
        Self {
            cfg,
            tls,
            failures: 0,
            state: State::Authorization { user: None },
        }
    }

    pub fn banner(&self) -> String {
        "+OK Spoolbox POP3 server ready".into()
    }

    fn login_allowed(&self) -> bool {
        self.tls || !self.cfg.require_tls
    }

    /// Handles one command line, without its line ending.
    pub fn handle(&mut self, line: &str) -> Step {
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        let command = command.to_ascii_uppercase();
        let authorized = matches!(self.state, State::Transaction { .. });
        match command.as_str() {
            "CAPA" => Step::Reply(self.capabilities()),
            "QUIT" if !authorized => Step::Close("+OK bye".into()),
            "USER" | "PASS" | "STLS" if authorized => err("already logged in"),
            "USER" => self.user(rest),
            "PASS" => self.pass(rest),
            "STLS" => self.stls(),
            _ => match &mut self.state {
                State::Authorization { .. } => err("log in first"),
                State::Transaction { messages } => transaction(messages, &command, rest),
            },
        }
    }

    fn capabilities(&self) -> String {
        let mut lines = vec!["+OK capabilities follow", "TOP", "UIDL"];
        if matches!(self.state, State::Authorization { .. }) {
            if self.login_allowed() {
                lines.push("USER");
            }
            if self.cfg.stls_available && !self.tls {
                lines.push("STLS");
            }
        }
        lines.extend(["IMPLEMENTATION Spoolbox", "."]);
        lines.join("\r\n")
    }

    fn user(&mut self, rest: &str) -> Step {
        if !self.login_allowed() {
            return err("use STLS first");
        }
        let name = rest.trim();
        if name.is_empty() {
            return err("a user name is required");
        }
        self.state = State::Authorization {
            user: Some(name.to_string()),
        };
        // The same answer for every name, so users cannot be discovered.
        Step::Reply("+OK send the password".into())
    }

    fn pass(&mut self, rest: &str) -> Step {
        if !self.login_allowed() {
            return err("use STLS first");
        }
        let State::Authorization { user } = &mut self.state else {
            return err("already logged in");
        };
        // The whole rest of the line: passwords may contain spaces.
        match user.take() {
            Some(user) => Step::Authenticate {
                user,
                password: rest.to_string(),
            },
            None => err("send USER first"),
        }
    }

    fn stls(&mut self) -> Step {
        if !self.cfg.stls_available {
            return err("STLS is not available");
        }
        if self.tls {
            return err("already encrypted");
        }
        self.state = State::Authorization { user: None };
        Step::StartTls("+OK begin TLS".into())
    }

    pub fn tls_established(&mut self) {
        self.tls = true;
    }

    /// The password was right; numbers the messages oldest first.
    pub fn authenticated(&mut self, entries: Vec<Entry>) -> String {
        let total: u64 = entries.iter().map(|e| e.size).sum();
        let reply = format!("+OK {} messages ({total} octets)", entries.len());
        self.state = State::Transaction {
            messages: entries
                .into_iter()
                .map(|entry| Message {
                    entry,
                    deleted: false,
                })
                .collect(),
        };
        reply
    }

    /// The password was wrong. The answer does not tell which part of the login failed.
    pub fn auth_failed(&mut self) -> Step {
        self.failures += 1;
        if self.failures >= MAX_FAILURES {
            Step::Close("-ERR too many failed logins".into())
        } else {
            err("invalid user name or password")
        }
    }
}

fn number(text: &str, messages: &[Message]) -> Result<usize, Step> {
    let n = text
        .parse::<usize>()
        .ok()
        .filter(|n| *n >= 1 && *n <= messages.len())
        .ok_or_else(|| err("no such message"))?;
    if messages[n - 1].deleted {
        return Err(err("message is deleted"));
    }
    Ok(n)
}

fn transaction(messages: &mut [Message], command: &str, rest: &str) -> Step {
    let args: Vec<&str> = rest.split_ascii_whitespace().collect();
    let live = || {
        messages
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.deleted)
            .map(|(i, m)| (i + 1, &m.entry))
    };
    match (command, args.as_slice()) {
        ("STAT", []) => {
            let total: u64 = live().map(|(_, e)| e.size).sum();
            Step::Reply(format!("+OK {} {total}", live().count()))
        }
        ("LIST", []) => listing(live().map(|(n, e)| format!("{n} {}", e.size))),
        ("UIDL", []) => listing(live().map(|(n, e)| format!("{n} {}", e.id))),
        ("LIST", [n]) => match number(n, messages) {
            Ok(n) => Step::Reply(format!("+OK {n} {}", messages[n - 1].entry.size)),
            Err(step) => step,
        },
        ("UIDL", [n]) => match number(n, messages) {
            Ok(n) => Step::Reply(format!("+OK {n} {}", messages[n - 1].entry.id)),
            Err(step) => step,
        },
        ("RETR", [n]) => match number(n, messages) {
            Ok(n) => send(&messages[n - 1].entry, None),
            Err(step) => step,
        },
        ("TOP", [n, lines]) => match (number(n, messages), lines.parse::<usize>()) {
            (Ok(n), Ok(lines)) => send(&messages[n - 1].entry, Some(lines)),
            (Err(step), _) => step,
            (_, Err(_)) => err("invalid number of lines"),
        },
        ("DELE", [n]) => match number(n, messages) {
            Ok(n) => {
                messages[n - 1].deleted = true;
                Step::Reply(format!("+OK message {n} deleted"))
            }
            Err(step) => step,
        },
        ("NOOP", []) => Step::Reply("+OK".into()),
        ("RSET", []) => {
            for m in messages.iter_mut() {
                m.deleted = false;
            }
            Step::Reply(format!("+OK {} messages", messages.len()))
        }
        ("QUIT", []) => Step::Update {
            delete: messages
                .iter()
                .filter(|m| m.deleted)
                .map(|m| m.entry.id.clone())
                .collect(),
        },
        ("STAT" | "LIST" | "UIDL" | "RETR" | "TOP" | "DELE" | "NOOP" | "RSET" | "QUIT", _) => {
            err("invalid arguments")
        }
        _ => err("unknown command"),
    }
}

fn send(entry: &Entry, lines: Option<usize>) -> Step {
    Step::Send {
        id: entry.id.clone(),
        size: entry.size,
        lines,
    }
}

fn listing(rows: impl Iterator<Item = String>) -> Step {
    let mut out = vec!["+OK".to_string()];
    out.extend(rows);
    out.push(".".into());
    Step::Reply(out.join("\r\n"))
}

/// A message as a multi-line response: CRLF line endings, dot-stuffed, with the final `.` line.
/// With `lines` (`TOP`), the headers, the blank line and that many body lines.
pub fn encode(raw: &[u8], lines: Option<usize>) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 32 + 5);
    let mut body_lines: Option<usize> = None;
    let body = raw.strip_suffix(b"\n").unwrap_or(raw);
    if !raw.is_empty() {
        for line in body.split(|b| *b == b'\n') {
            if let (Some(limit), Some(seen)) = (lines, body_lines)
                && seen >= limit
            {
                break;
            }
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.first() == Some(&b'.') {
                out.push(b'.');
            }
            out.extend_from_slice(line);
            out.extend_from_slice(b"\r\n");
            body_lines = match body_lines {
                Some(seen) => Some(seen + 1),
                None if line.is_empty() => Some(0),
                None => None,
            };
        }
    }
    out.extend_from_slice(b".\r\n");
    out
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const OPEN: SessionConfig = SessionConfig {
        stls_available: false,
        require_tls: false,
    };

    fn entries() -> Vec<Entry> {
        [("id-1", 100), ("id-2", 20), ("id-3", 3)]
            .map(|(id, size)| Entry {
                id: id.into(),
                size,
            })
            .to_vec()
    }

    fn logged_in() -> Session {
        let mut s = Session::new(OPEN, false);
        s.handle("USER u");
        assert!(matches!(s.handle("PASS p"), Step::Authenticate { .. }));
        assert_eq!(s.authenticated(entries()), "+OK 3 messages (123 octets)");
        s
    }

    fn reply(step: Step) -> String {
        match step {
            Step::Reply(text) => text,
            other => panic!("expected a reply, got {other:?}"),
        }
    }

    #[test]
    fn nothing_is_readable_before_login() {
        let mut s = Session::new(OPEN, false);
        for command in [
            "STAT", "LIST", "LIST 1", "RETR 1", "TOP 1 1", "DELE 1", "UIDL", "NOOP", "RSET",
        ] {
            assert!(reply(s.handle(command)).starts_with("-ERR"), "{command}");
        }
    }

    #[test]
    fn login_needs_user_then_password_and_hides_which_was_wrong() {
        let mut s = Session::new(OPEN, false);
        assert!(reply(s.handle("PASS p")).starts_with("-ERR"));
        assert!(reply(s.handle("USER")).starts_with("-ERR"));
        assert_eq!(reply(s.handle("user known")), "+OK send the password");
        assert_eq!(
            s.handle("Pass with spaces inside"),
            Step::Authenticate {
                user: "known".into(),
                password: "with spaces inside".into()
            }
        );
        assert_eq!(
            s.auth_failed(),
            Step::Reply("-ERR invalid user name or password".into())
        );
        assert!(
            reply(s.handle("PASS p")).starts_with("-ERR"),
            "USER is needed again"
        );
        s.handle("USER x");
        s.handle("PASS y");
        assert!(matches!(s.auth_failed(), Step::Reply(_)));
        assert!(matches!(s.auth_failed(), Step::Close(_)), "third failure");
    }

    #[test]
    fn numbers_stay_fixed_and_deletions_wait_for_quit() {
        let mut s = logged_in();
        assert_eq!(reply(s.handle("STAT")), "+OK 3 123");
        assert_eq!(reply(s.handle("LIST 2")), "+OK 2 20");
        assert_eq!(reply(s.handle("UIDL 3")), "+OK 3 id-3");
        assert_eq!(
            s.handle("RETR 2"),
            Step::Send {
                id: "id-2".into(),
                size: 20,
                lines: None
            }
        );
        assert_eq!(
            s.handle("TOP 1 5"),
            Step::Send {
                id: "id-1".into(),
                size: 100,
                lines: Some(5)
            }
        );

        assert_eq!(reply(s.handle("DELE 2")), "+OK message 2 deleted");
        assert_eq!(reply(s.handle("STAT")), "+OK 2 103");
        assert_eq!(
            reply(s.handle("LIST")),
            "+OK\r\n1 100\r\n3 3\r\n.",
            "the third message is still number 3"
        );
        assert_eq!(reply(s.handle("UIDL")), "+OK\r\n1 id-1\r\n3 id-3\r\n.");
        for command in ["RETR 2", "DELE 2", "LIST 2", "UIDL 2", "TOP 2 1"] {
            assert_eq!(
                reply(s.handle(command)),
                "-ERR message is deleted",
                "{command}"
            );
        }

        assert_eq!(reply(s.handle("RSET")), "+OK 3 messages");
        assert_eq!(reply(s.handle("STAT")), "+OK 3 123");
        s.handle("DELE 1");
        s.handle("DELE 3");
        assert_eq!(
            s.handle("QUIT"),
            Step::Update {
                delete: vec!["id-1".into(), "id-3".into()]
            }
        );
    }

    #[test]
    fn bad_arguments_are_refused() {
        let mut s = logged_in();
        for command in [
            "RETR",
            "RETR 0",
            "RETR 4",
            "RETR x",
            "RETR -1",
            "DELE 99",
            "LIST 0",
            "TOP 1",
            "TOP 1 x",
            "STAT 1",
            "NOOP x",
            "QUIT x",
            "FOO",
            "",
            "APOP a b",
            "AUTH PLAIN",
        ] {
            assert!(reply(s.handle(command)).starts_with("-ERR"), "{command:?}");
        }
        assert!(reply(s.handle("USER x")).starts_with("-ERR"));
        assert!(reply(s.handle("STLS")).starts_with("-ERR"));
    }

    #[test]
    fn tls_can_be_required_for_the_password() {
        let cfg = SessionConfig {
            stls_available: true,
            require_tls: true,
        };
        let mut s = Session::new(cfg, false);
        assert_eq!(reply(s.handle("USER u")), "-ERR use STLS first");
        assert_eq!(reply(s.handle("PASS p")), "-ERR use STLS first");
        let capa = reply(s.handle("CAPA"));
        assert!(capa.contains("STLS") && !capa.contains("USER"), "{capa}");
        assert!(matches!(s.handle("STLS"), Step::StartTls(_)));
        s.tls_established();
        let capa = reply(s.handle("CAPA"));
        assert!(!capa.contains("STLS") && capa.contains("USER"), "{capa}");
        assert!(reply(s.handle("STLS")).starts_with("-ERR"));
        assert_eq!(reply(s.handle("USER u")), "+OK send the password");
    }

    #[test]
    fn stls_forgets_the_plaintext_user() {
        let cfg = SessionConfig {
            stls_available: true,
            require_tls: false,
        };
        let mut s = Session::new(cfg, false);
        s.handle("USER injected");
        assert!(matches!(s.handle("STLS"), Step::StartTls(_)));
        s.tls_established();
        assert!(reply(s.handle("PASS p")).starts_with("-ERR"));
        let mut plain = Session::new(OPEN, false);
        assert_eq!(reply(plain.handle("STLS")), "-ERR STLS is not available");
    }

    #[test]
    fn encoding() {
        assert_eq!(encode(b"", None), b".\r\n");
        assert_eq!(encode(b"a\r\n", None), b"a\r\n.\r\n");
        assert_eq!(encode(b"a", None), b"a\r\n.\r\n", "final line ending added");
        assert_eq!(encode(b"a\nb\n", None), b"a\r\nb\r\n.\r\n", "bare LF fixed");
        assert_eq!(
            encode(b"S: x\r\n\r\n.\r\n..y\r\nz\r\n", None),
            b"S: x\r\n\r\n..\r\n...y\r\nz\r\n.\r\n"
        );
        let raw = b"A: 1\r\nB: 2\r\n\r\none\r\ntwo\r\nthree\r\n";
        assert_eq!(encode(raw, Some(0)), b"A: 1\r\nB: 2\r\n\r\n.\r\n");
        assert_eq!(
            encode(raw, Some(2)),
            b"A: 1\r\nB: 2\r\n\r\none\r\ntwo\r\n.\r\n"
        );
        assert_eq!(encode(raw, Some(99)), encode(raw, None));
    }

    proptest! {
        /// A client that follows RFC 1939 gets the message back and never an early end.
        #[test]
        fn encoded_messages_decode(lines in prop::collection::vec("[.a-z ]{0,6}", 0..12)) {
            let raw = lines.join("\r\n").into_bytes();
            let encoded = encode(&raw, None);
            let text = String::from_utf8(encoded).expect("utf8");
            let mut wire: Vec<&str> = text.split("\r\n").collect();
            prop_assert_eq!(wire.pop(), Some(""));
            prop_assert_eq!(wire.pop(), Some("."));
            prop_assert!(wire.iter().all(|l| *l != "."), "early end: {:?}", wire);
            let decoded: Vec<&str> = wire.iter().map(|l| l.strip_prefix('.').unwrap_or(l)).collect();
            let expected = lines.join("\r\n");
            let expected = expected.strip_suffix("\r\n").unwrap_or(&expected);
            prop_assert_eq!(decoded.join("\r\n"), expected.to_string());
        }

        /// Whatever a message contains, including bare CR and LF, it cannot end the response.
        #[test]
        fn messages_cannot_end_early(raw in prop::collection::vec(prop::sample::select(vec![b'.', b'\r', b'\n', b'a']), 0..40)) {
            let encoded = encode(&raw, None);
            let body = &encoded[..encoded.len() - 3];
            prop_assert!(!body.starts_with(b".\r\n"));
            prop_assert!(!body.windows(5).any(|w| w == b"\r\n.\r\n"), "{:?}", encoded);
        }
    }
}
