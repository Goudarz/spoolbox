/// The SMTP envelope as received — the source of truth for who a message was
/// actually sent to, independent of its `To`/`Cc`/`Bcc` headers (ADR-006).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Envelope {
    /// Reverse-path without angle brackets; empty for the null path `<>`.
    pub mail_from: String,
    /// `MAIL FROM` parameters (e.g. `SIZE`, `BODY`, `SMTPUTF8`), names upper-cased.
    pub mail_params: Vec<(String, Option<String>)>,
    /// Accepted forward-paths, in order, without angle brackets.
    pub rcpt_to: Vec<String>,
    /// Name given in `HELO`/`EHLO`.
    pub helo: String,
    pub remote_ip: String,
    /// Negotiated TLS protocol, e.g. `TLSv1.3`; `None` for plaintext.
    pub tls: Option<String>,
    pub auth_user: Option<String>,
}
