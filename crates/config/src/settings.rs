use std::path::PathBuf;
use std::time::Duration;

use spoolbox_auth::Credentials;
use spoolbox_core::ListenAddr;

pub const MIB: u64 = 1024 * 1024;

/// Fully resolved configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub label: String,
    pub http: HttpSettings,
    pub smtp: SmtpSettings,
    pub storage: StorageSettings,
    pub tags: TagSettings,
    pub network: NetworkSettings,
    pub log: LogSettings,
    pub chaos: ChaosSettings,
    pub webhook: WebhookSettings,
    pub egress: EgressSettings,
    pub relay: RelaySettings,
    pub forward: ForwardSettings,
    /// Off unless `--pop3` is given.
    pub pop3: Option<Pop3Settings>,
}

/// `--pop3`. Users are mandatory: every message can be read and deleted through it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pop3Settings {
    pub listen: ListenAddr,
    pub auth: Credentials,
    /// Offered with STLS, and then required for clients on other machines.
    pub tls: Option<TlsSource>,
}

/// POST a message summary for every new message.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct WebhookSettings {
    pub url: Option<String>,
    /// Requests per second; 0 = no limit. Excess requests wait (they are not dropped).
    pub limit_per_second: u32,
    /// Wait this long after a message arrives before notifying.
    pub delay: Duration,
    /// HMAC-SHA256 key for the `X-Spoolbox-Signature` header.
    pub secret: Option<String>,
}

impl std::fmt::Debug for WebhookSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookSettings")
            .field("url", &self.url)
            .field("limit_per_second", &self.limit_per_second)
            .field("delay", &self.delay)
            .field("signed", &self.secret.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpSecurity {
    None,
    StartTls,
    Tls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpMechanism {
    Plain,
    Login,
}

#[derive(Clone, PartialEq, Eq)]
pub struct SmtpLogin {
    pub mechanism: SmtpMechanism,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for SmtpLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpLogin")
            .field("mechanism", &self.mechanism)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// An SMTP server that messages are sent on to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtpTarget {
    pub host: String,
    pub port: u16,
    pub security: SmtpSecurity,
    /// Accept invalid TLS certificates, and passwords without TLS.
    pub allow_insecure: bool,
    pub login: Option<SmtpLogin>,
    /// Envelope sender instead of the original one.
    pub return_path: Option<String>,
    /// Replaces the `From` header (and the envelope sender unless `return_path` is set).
    pub override_from: Option<String>,
    /// Keep `Message-ID`; by default each delivery gets a new one.
    pub preserve_message_ids: bool,
    /// 0 = no limit. Excess messages wait (they are not dropped).
    pub limit_per_minute: u32,
}

/// Off unless `target` is set. Releasing through the API works as soon as it is;
/// new messages are only sent on with `all` or `matching`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelaySettings {
    pub target: Option<SmtpTarget>,
    /// Requires `allowed_recipients`.
    pub all: bool,
    /// Regular expression (validated): relay new messages to the recipients that match.
    pub matching: Option<String>,
    /// Regular expressions (validated); apply to every delivery, case-insensitively.
    pub allowed_recipients: Option<String>,
    pub blocked_recipients: Option<String>,
}

/// Send a copy of every new message to fixed addresses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForwardSettings {
    pub target: Option<SmtpTarget>,
    pub to: Vec<String>,
}

/// Policy for requests to URLs found in messages (link check).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EgressSettings {
    pub allow_internal: bool,
    pub allow_untrusted_tls: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChaosSettings {
    pub enabled: bool,
    pub triggers: spoolbox_core::chaos::Triggers,
    /// `[[chaos.rules]]` from the configuration file.
    pub rules: Vec<spoolbox_core::chaos::Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpSettings {
    pub listen: ListenAddr,
    /// Always starts and ends with `/`.
    pub webroot: String,
    /// Extra allowed `Host` header values (see `spoolbox-http` for the built-in defaults).
    pub allowed_hosts: Vec<String>,
    /// Basic authentication for the web UI and API.
    pub auth: Option<Credentials>,
    /// Separate credentials for `POST /api/v1/send`; `None` falls back to `auth`.
    pub send_auth: Option<Credentials>,
    /// The send endpoint needs no credentials even when `auth` is set.
    pub send_auth_accept_any: bool,
    /// Lower-case `host[:port]` origins allowed to call the API cross-origin; `*` allows all.
    pub cors_origins: Vec<String>,
    /// gzip responses when the client accepts it.
    pub compression: bool,
    /// HTTPS for the web UI and API.
    pub tls: Option<TlsSource>,
    pub hide_delete_all_button: bool,
    /// Remote stylesheets and fonts are not loaded in message previews.
    pub block_remote_css_and_fonts: bool,
    /// MCP clients may delete messages.
    pub mcp_allow_delete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsSource {
    /// Generate a self-signed certificate at startup.
    Auto,
    Files {
        cert: PathBuf,
        key: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtpSettings {
    pub listen: ListenAddr,
    pub max_recipients: u32,
    /// Bytes; 0 = unlimited.
    pub max_message_size: u64,
    pub max_connections: u32,
    pub tls: Option<TlsSource>,
    /// Commands other than EHLO/STARTTLS/NOOP/QUIT require STARTTLS first.
    pub require_starttls: bool,
    /// TLS from the first byte (`--smtp-require-tls`).
    pub implicit_tls: bool,
    pub auth_accept_any: bool,
    /// AUTH requires valid credentials and is required before MAIL.
    pub auth: Option<Credentials>,
    /// Allow PLAIN/LOGIN without TLS.
    pub auth_allow_insecure: bool,
    /// Keep redacted transcripts of recent sessions (`/api/v2/transcripts`).
    pub transcripts: bool,
    /// Regular expression recipients must match (validated).
    pub allowed_recipients: Option<String>,
    pub ignore_rejected_recipients: bool,
    /// Reject messages containing `\r\r\n` instead of storing them as received.
    pub strict_rfc_headers: bool,
    /// Upper-case EHLO keywords not to advertise.
    pub hide_extensions: Vec<String>,
    /// Lower-case domains.
    pub accept_domains: Vec<String>,
    pub reject_domains: Vec<String>,
    pub discard_domains: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageSettings {
    /// `None` = temporary database deleted on exit.
    pub database: Option<PathBuf>,
    /// 0 = unlimited.
    pub max_messages: u64,
    pub max_age: Option<Duration>,
    /// Bytes of free disk below which new messages are rejected; 0 disables the guard.
    pub min_free_disk: u64,
    /// 0 none, 1 fastest, 2 default, 3 best.
    pub compression: u8,
    pub wal: bool,
    /// Periodically return free pages to the file system.
    pub auto_vacuum: bool,
    /// Accept but do not store messages whose Message-ID is already stored.
    pub ignore_duplicate_ids: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRule {
    /// Search query, same syntax as the API.
    pub query: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagSettings {
    /// Tag from `user+tag@domain` addresses.
    pub plus_addresses: bool,
    /// Tag from the `X-Tags` header.
    pub x_tags: bool,
    /// Title Case tags from plus addresses and `X-Tags`.
    pub title_case: bool,
    /// Tag with the SMTP AUTH user name.
    pub username: bool,
    /// Automatic tags: messages matching `query` get `tags` once indexed.
    pub rules: Vec<TagRule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkSettings {
    pub allow_unauthenticated_network: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogSettings {
    pub level: LogLevel,
    pub format: LogFormat,
    pub file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Text,
    Json,
}

/// EHLO keywords that `--smtp-hide-extensions` accepts.
pub const HIDEABLE_EXTENSIONS: [&str; 7] = [
    "SIZE",
    "PIPELINING",
    "STARTTLS",
    "AUTH",
    "ENHANCEDSTATUSCODES",
    "8BITMIME",
    "SMTPUTF8",
];

impl Default for Settings {
    fn default() -> Self {
        Self {
            label: String::new(),
            http: HttpSettings {
                listen: ListenAddr::loopback(8025),
                webroot: "/".into(),
                allowed_hosts: Vec::new(),
                auth: None,
                send_auth: None,
                send_auth_accept_any: false,
                cors_origins: Vec::new(),
                compression: true,
                tls: None,
                hide_delete_all_button: false,
                block_remote_css_and_fonts: false,
                mcp_allow_delete: false,
            },
            smtp: SmtpSettings {
                listen: ListenAddr::loopback(1025),
                max_recipients: 100,
                max_message_size: 50 * MIB,
                max_connections: 1000,
                tls: None,
                require_starttls: false,
                implicit_tls: false,
                auth_accept_any: false,
                auth: None,
                auth_allow_insecure: false,
                transcripts: false,
                allowed_recipients: None,
                ignore_rejected_recipients: false,
                strict_rfc_headers: false,
                hide_extensions: Vec::new(),
                accept_domains: Vec::new(),
                reject_domains: Vec::new(),
                discard_domains: Vec::new(),
            },
            storage: StorageSettings {
                database: None,
                max_messages: 500,
                max_age: None,
                min_free_disk: 1024 * MIB,
                compression: 1,
                wal: true,
                auto_vacuum: true,
                ignore_duplicate_ids: false,
            },
            tags: TagSettings {
                plus_addresses: true,
                x_tags: true,
                title_case: false,
                username: false,
                rules: Vec::new(),
            },
            network: NetworkSettings {
                allow_unauthenticated_network: false,
            },
            chaos: ChaosSettings::default(),
            webhook: WebhookSettings::default(),
            egress: EgressSettings::default(),
            relay: RelaySettings::default(),
            forward: ForwardSettings::default(),
            pop3: None,
            log: LogSettings {
                level: LogLevel::Info,
                format: LogFormat::Text,
                file: None,
            },
        }
    }
}
