//! Every command-line flag and environment variable Spoolbox recognises,
//! including the compatibility flags and `MP_*` variables. A compatibility option is
//! never silently ignored: it is applied, or reported as a
//! no-op, deprecated alias, planned feature, or removed feature.

use crate::key::Key;
use crate::relay::Field;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Start anyway and log a warning.
    Warn,
    /// Refuse to start: ignoring the option would weaken security or silently change behaviour.
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Active(Key),
    /// Accepted for compatibility; Spoolbox's behaviour already matches what the option asks for.
    NoOp(&'static str),
    /// Deprecated alias of another flag (by long name).
    Deprecated(&'static str),
    Planned {
        phase: &'static str,
        on_use: Severity,
    },
    Removed(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub struct FlagSpec {
    pub long: &'static str,
    pub short: Option<char>,
    pub takes_value: bool,
    pub help: &'static str,
    pub status: Status,
    /// Part of the compatibility flag set (may have a short option).
    pub compat: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum EnvTarget {
    Flag(&'static str),
    Only(Status),
}

#[derive(Debug, Clone, Copy)]
pub struct EnvAlias {
    pub name: &'static str,
    pub target: EnvTarget,
}

use Severity::Warn as W;
use Status::{Active, Deprecated, NoOp, Planned, Removed};

const fn flag(
    long: &'static str,
    short: Option<char>,
    takes_value: bool,
    help: &'static str,
    status: Status,
) -> FlagSpec {
    FlagSpec {
        long,
        short,
        takes_value,
        help,
        status,
        compat: true,
    }
}

const fn native(
    long: &'static str,
    takes_value: bool,
    help: &'static str,
    status: Status,
) -> FlagSpec {
    FlagSpec {
        long,
        short: None,
        takes_value,
        help,
        status,
        compat: false,
    }
}

const fn planned(
    long: &'static str,
    takes_value: bool,
    phase: &'static str,
    on_use: Severity,
) -> FlagSpec {
    flag(long, None, takes_value, "", Planned { phase, on_use })
}

const REMOVED_TENANT: &str = "database tenants are replaced by namespaces (ADR-016)";
const REMOVED_SA: &str = "SpamAssassin integration is planned after v1.0 (ADR-016)";

pub const FLAGS: &[FlagSpec] = &[
    // --- active ---
    flag(
        "database",
        Some('d'),
        true,
        "Database file to store persistent data (default: temporary database)",
        Active(Key::Database),
    ),
    flag(
        "label",
        None,
        true,
        "Optional label to identify this instance",
        Active(Key::Label),
    ),
    flag(
        "max",
        Some('m'),
        true,
        "Max number of messages to store (0 = unlimited)",
        Active(Key::MaxMessages),
    ),
    flag(
        "max-age",
        None,
        true,
        "Max age of messages in hours or days (e.g. 12h, 3d)",
        Active(Key::MaxAge),
    ),
    flag(
        "max-message-size",
        None,
        true,
        "Maximum message size in MiB (0 = unlimited)",
        Active(Key::MaxMessageSize),
    ),
    flag(
        "log-file",
        None,
        true,
        "Write logs to this file instead of stderr",
        Active(Key::LogFile),
    ),
    flag(
        "quiet",
        Some('q'),
        false,
        "Quiet logging (errors only)",
        Active(Key::Quiet),
    ),
    flag(
        "verbose",
        Some('v'),
        false,
        "Verbose logging",
        Active(Key::Verbose),
    ),
    flag(
        "listen",
        Some('l'),
        true,
        "HTTP bind address for the web UI and API",
        Active(Key::HttpListen),
    ),
    flag(
        "allowed-hosts",
        None,
        true,
        "Additional allowed Host header values, comma-separated",
        Active(Key::AllowedHosts),
    ),
    flag(
        "webroot",
        None,
        true,
        "Base path for the web UI and API",
        Active(Key::Webroot),
    ),
    flag(
        "smtp",
        Some('s'),
        true,
        "SMTP bind address",
        Active(Key::SmtpListen),
    ),
    flag(
        "smtp-max-recipients",
        None,
        true,
        "Maximum SMTP recipients per message",
        Active(Key::SmtpMaxRecipients),
    ),
    native(
        "config",
        true,
        "TOML configuration file",
        Active(Key::ConfigFile),
    ),
    native(
        "log-level",
        true,
        "Log level: error, warn, info, debug, trace",
        Active(Key::LogLevel),
    ),
    native(
        "log-format",
        true,
        "Log format: text or json",
        Active(Key::LogFormat),
    ),
    native(
        "webhook-secret",
        true,
        "Sign webhook requests with HMAC-SHA256 (X-Spoolbox-Signature header)",
        Active(Key::WebhookSecret),
    ),
    native(
        "mcp-allow-delete",
        false,
        "Let MCP clients (AI assistants) delete messages",
        Active(Key::McpAllowDelete),
    ),
    native(
        "smtp-transcripts",
        false,
        "Keep redacted transcripts of the last 200 SMTP sessions (api/v2/transcripts)",
        Active(Key::SmtpTranscripts),
    ),
    native(
        "allow-unauthenticated-network",
        false,
        "Allow binding to non-loopback interfaces without authentication",
        Active(Key::AllowUnauthenticatedNetwork),
    ),
    native(
        "min-free-disk",
        true,
        "Reject new messages when free disk space drops below this many MiB (0 disables)",
        Active(Key::MinFreeDisk),
    ),
    native(
        "smtp-hide-extensions",
        true,
        "Do not advertise these EHLO extensions, comma-separated (e.g. PIPELINING,SMTPUTF8)",
        Active(Key::SmtpHideExtensions),
    ),
    native(
        "smtp-accept-domains",
        true,
        "Only accept recipients in these domains, comma-separated",
        Active(Key::SmtpAcceptDomains),
    ),
    native(
        "smtp-reject-domains",
        true,
        "Reject recipients in these domains, comma-separated",
        Active(Key::SmtpRejectDomains),
    ),
    native(
        "smtp-discard-domains",
        true,
        "Accept but do not store mail for these domains, comma-separated",
        Active(Key::SmtpDiscardDomains),
    ),
    native(
        "smtp-max-connections",
        true,
        "Maximum concurrent SMTP connections (default 1000)",
        Active(Key::SmtpMaxConnections),
    ),
    // --- no-ops ---
    flag(
        "disable-version-check",
        None,
        false,
        "",
        NoOp("Spoolbox never checks for new versions"),
    ),
    flag(
        "smtp-disable-rdns",
        None,
        false,
        "",
        NoOp("Spoolbox never performs reverse DNS lookups by default"),
    ),
    flag(
        "disable-html-check",
        None,
        false,
        "",
        NoOp("deprecated and has no effect"),
    ),
    // --- deprecated aliases ---
    flag("db-file", None, true, "", Deprecated("database")),
    flag("ui-ssl-cert", None, true, "", Deprecated("ui-tls-cert")),
    flag("ui-ssl-key", None, true, "", Deprecated("ui-tls-key")),
    flag("smtp-ssl-cert", None, true, "", Deprecated("smtp-tls-cert")),
    flag("smtp-ssl-key", None, true, "", Deprecated("smtp-tls-key")),
    flag(
        "smtp-tls-required",
        None,
        false,
        "",
        Deprecated("smtp-require-starttls"),
    ),
    // --- removed ---
    flag("tenant-id", None, true, "", Removed(REMOVED_TENANT)),
    flag("enable-spamassassin", None, true, "", Removed(REMOVED_SA)),
    // --- planned: refusing to start is safer than ignoring ---
    flag(
        "ui-auth-file",
        None,
        true,
        "A password file for web UI and API authentication (htpasswd format)",
        Active(Key::UiAuthFile),
    ),
    flag(
        "ui-tls-cert",
        None,
        true,
        "TLS certificate (PEM) for HTTPS, or `auto` for a self-signed one",
        Active(Key::UiTlsCert),
    ),
    flag(
        "ui-tls-key",
        None,
        true,
        "TLS private key (PEM) for HTTPS",
        Active(Key::UiTlsKey),
    ),
    flag(
        "send-api-auth-file",
        None,
        true,
        "A password file for the send API (defaults to the UI credentials)",
        Active(Key::SendApiAuthFile),
    ),
    flag(
        "smtp-auth-file",
        None,
        true,
        "A password file for SMTP authentication (htpasswd format)",
        Active(Key::SmtpAuthFile),
    ),
    flag(
        "smtp-tls-cert",
        None,
        true,
        "TLS certificate (PEM) for SMTP STARTTLS, or `auto` for a self-signed one",
        Active(Key::SmtpTlsCert),
    ),
    flag(
        "smtp-tls-key",
        None,
        true,
        "TLS private key (PEM) for SMTP",
        Active(Key::SmtpTlsKey),
    ),
    flag(
        "smtp-require-starttls",
        None,
        false,
        "Require clients to use STARTTLS before sending mail",
        Active(Key::SmtpRequireStarttls),
    ),
    flag(
        "smtp-require-tls",
        None,
        false,
        "Accept only implicit TLS (SMTPS) connections",
        Active(Key::SmtpRequireTls),
    ),
    flag(
        "smtp-allowed-recipients",
        None,
        true,
        "Only accept recipients matching this regular expression",
        Active(Key::SmtpAllowedRecipients),
    ),
    flag(
        "pop3-auth-file",
        None,
        true,
        "Password file of the POP3 users (required with --pop3)",
        Active(Key::Pop3AuthFile),
    ),
    flag(
        "pop3-tls-cert",
        None,
        true,
        "TLS certificate for POP3 STLS, or `auto` for a self-signed one",
        Active(Key::Pop3TlsCert),
    ),
    flag(
        "pop3-tls-key",
        None,
        true,
        "TLS private key for POP3 STLS",
        Active(Key::Pop3TlsKey),
    ),
    // --- planned: safe to ignore for now ---
    flag(
        "smtp-auth-accept-any",
        None,
        false,
        "Offer SMTP AUTH and accept any username and password",
        Active(Key::SmtpAuthAcceptAny),
    ),
    flag(
        "smtp-auth-allow-insecure",
        None,
        false,
        "Allow SMTP AUTH PLAIN/LOGIN without STARTTLS or TLS",
        Active(Key::SmtpAuthAllowInsecure),
    ),
    flag(
        "smtp-strict-rfc-headers",
        None,
        false,
        "Reject messages containing <CR><CR><LF>",
        Active(Key::SmtpStrictRfcHeaders),
    ),
    flag(
        "smtp-ignore-rejected-recipients",
        None,
        false,
        "Answer 250 for rejected recipients and drop them silently",
        Active(Key::SmtpIgnoreRejectedRecipients),
    ),
    flag(
        "compression",
        None,
        true,
        "Compression of stored messages: 0 none, 1 fastest, 2 default, 3 best",
        Active(Key::Compression),
    ),
    flag(
        "disable-wal",
        None,
        false,
        "Disable SQLite WAL (for network file systems)",
        Active(Key::DisableWal),
    ),
    flag(
        "disable-auto-vacuum",
        None,
        false,
        "Do not return free database pages to the file system",
        Active(Key::DisableAutoVacuum),
    ),
    flag(
        "ignore-duplicate-ids",
        None,
        false,
        "Do not store messages whose Message-ID is already stored",
        Active(Key::IgnoreDuplicateIds),
    ),
    planned("use-message-dates", false, "P4", W),
    flag(
        "tags-disable",
        None,
        true,
        "Disable automatic tags: plus-addresses, x-tags",
        Active(Key::TagsDisable),
    ),
    flag(
        "tags-title-case",
        None,
        false,
        "Title Case tags from plus addresses and X-Tags",
        Active(Key::TagsTitleCase),
    ),
    flag(
        "tags-username",
        None,
        false,
        "Tag messages with the SMTP AUTH user name",
        Active(Key::TagsUsername),
    ),
    flag(
        "api-cors",
        None,
        true,
        "Allowed API CORS origins, comma-separated host[:port], or *",
        Active(Key::ApiCors),
    ),
    flag(
        "disable-http-compression",
        None,
        false,
        "Do not gzip HTTP responses",
        Active(Key::DisableHttpCompression),
    ),
    flag(
        "send-api-auth-accept-any",
        None,
        false,
        "The send API accepts requests without credentials",
        Active(Key::SendApiAuthAcceptAny),
    ),
    flag(
        "enable-chaos",
        None,
        false,
        "Enable chaos testing (random SMTP errors, configurable via the API)",
        Active(Key::EnableChaos),
    ),
    flag(
        "chaos-triggers",
        None,
        true,
        "Chaos triggers, e.g. Sender:451:10,Recipient:452:5,Authentication:535:0 (implies --enable-chaos)",
        Active(Key::ChaosTriggers),
    ),
    flag(
        "block-remote-css-and-fonts",
        None,
        false,
        "Do not load remote stylesheets and fonts in message previews",
        Active(Key::BlockRemoteCssAndFonts),
    ),
    flag(
        "hide-delete-all-button",
        None,
        false,
        "Hide the delete-all button in the web UI",
        Active(Key::HideDeleteAllButton),
    ),
    flag(
        "pop3",
        None,
        true,
        "Serve messages over POP3 on this address (off by default)",
        Active(Key::Pop3Listen),
    ),
    flag(
        "smtp-relay-config",
        None,
        true,
        "YAML file describing the SMTP server for relaying and message release",
        Active(Key::SmtpRelayConfig),
    ),
    flag(
        "smtp-relay-all",
        None,
        false,
        "Relay every new message to its recipients (needs allowed-recipients)",
        Active(Key::SmtpRelayAll),
    ),
    flag(
        "smtp-relay-matching",
        None,
        true,
        "Relay new messages to the recipients matching this regular expression",
        Active(Key::SmtpRelayMatching),
    ),
    flag(
        "smtp-forward-config",
        None,
        true,
        "YAML file describing the SMTP server and addresses to forward new messages to",
        Active(Key::SmtpForwardConfig),
    ),
    flag(
        "tag",
        Some('t'),
        true,
        "Tag messages matching searches: 'Tag1,Tag2=query; Tag3=query'",
        Active(Key::Tag),
    ),
    flag(
        "tags-config",
        None,
        true,
        "YAML file of tag rules (filters: [{match: query, tags: 'a, b'}])",
        Active(Key::TagsConfig),
    ),
    flag(
        "webhook-url",
        None,
        true,
        "POST a JSON summary of every new message to this URL",
        Active(Key::WebhookUrl),
    ),
    flag(
        "webhook-limit",
        None,
        true,
        "Webhook requests per second (0 = no limit; excess requests wait)",
        Active(Key::WebhookLimit),
    ),
    flag(
        "webhook-delay",
        None,
        true,
        "Seconds to wait before sending a webhook",
        Active(Key::WebhookDelay),
    ),
    flag(
        "enable-prometheus",
        None,
        true,
        "Serve Prometheus metrics: `true` for /metrics on the main port, or an address",
        Active(Key::EnablePrometheus),
    ),
    flag(
        "allow-internal-http-requests",
        None,
        false,
        "Let link checks reach private, loopback and other internal addresses",
        Active(Key::AllowInternalHttpRequests),
    ),
    flag(
        "allow-untrusted-tls",
        None,
        false,
        "Accept invalid TLS certificates in link checks",
        Active(Key::AllowUntrustedTls),
    ),
    flag(
        "disable-link-check-rate-limit",
        None,
        false,
        "Do not space out link check requests to the same host",
        Active(Key::DisableLinkCheckRateLimit),
    ),
    native(
        "disable-link-check",
        false,
        "Turn off link checks, so the server never fetches URLs found in messages",
        Active(Key::DisableLinkCheck),
    ),
];

/// Short options only exist for compatibility flags; native flags use long names.
pub fn flag_by_long(long: &str) -> Option<&'static FlagSpec> {
    FLAGS.iter().find(|f| f.long == long)
}

const fn alias(name: &'static str, flag: &'static str) -> EnvAlias {
    EnvAlias {
        name,
        target: EnvTarget::Flag(flag),
    }
}

const fn env_only(name: &'static str, status: Status) -> EnvAlias {
    EnvAlias {
        name,
        target: EnvTarget::Only(status),
    }
}

const NO_CRAM_MD5: Status = Removed("CRAM-MD5 is not supported; use plain or login");
const NO_SMTP_ERRORS: Status =
    NoOp("messages are sent on in the background, so failures are retried and logged instead");

/// Compatibility environment variables (`MP_*`) and what they map to.
pub const COMPAT_ENV: &[EnvAlias] = &[
    alias("MP_DATABASE", "database"),
    alias("MP_DATA_FILE", "db-file"),
    alias("MP_DISABLE_WAL", "disable-wal"),
    alias("MP_DISABLE_VERSION_CHECK", "disable-version-check"),
    alias("MP_DISABLE_AUTO_VACUUM", "disable-auto-vacuum"),
    alias("MP_COMPRESSION", "compression"),
    alias("MP_TENANT_ID", "tenant-id"),
    alias("MP_LABEL", "label"),
    alias("MP_MAX_MESSAGES", "max"),
    alias("MP_MAX_AGE", "max-age"),
    alias("MP_MAX_MESSAGE_SIZE", "max-message-size"),
    alias("MP_USE_MESSAGE_DATES", "use-message-dates"),
    alias("MP_IGNORE_DUPLICATE_IDS", "ignore-duplicate-ids"),
    alias("MP_LOG_FILE", "log-file"),
    alias("MP_QUIET", "quiet"),
    alias("MP_VERBOSE", "verbose"),
    alias("MP_UI_BIND_ADDR", "listen"),
    alias("MP_ALLOWED_HOSTS", "allowed-hosts"),
    alias("MP_WEBROOT", "webroot"),
    alias("MP_UI_AUTH_FILE", "ui-auth-file"),
    env_only("MP_UI_AUTH", Active(Key::UiAuth)),
    alias("MP_UI_TLS_CERT", "ui-tls-cert"),
    alias("MP_UI_TLS_KEY", "ui-tls-key"),
    alias("MP_UI_SSL_CERT", "ui-ssl-cert"),
    alias("MP_UI_SSL_KEY", "ui-ssl-key"),
    alias("MP_API_CORS", "api-cors"),
    alias(
        "MP_BLOCK_REMOTE_CSS_AND_FONTS",
        "block-remote-css-and-fonts",
    ),
    alias(
        "MP_ALLOW_INTERNAL_HTTP_REQUESTS",
        "allow-internal-http-requests",
    ),
    alias(
        "MP_DISABLE_LINK_CHECK_RATE_LIMIT",
        "disable-link-check-rate-limit",
    ),
    alias("MP_ENABLE_SPAMASSASSIN", "enable-spamassassin"),
    alias("MP_ALLOW_UNTRUSTED_TLS", "allow-untrusted-tls"),
    alias("MP_DISABLE_HTTP_COMPRESSION", "disable-http-compression"),
    alias("MP_HIDE_DELETE_ALL_BUTTON", "hide-delete-all-button"),
    alias("MP_DISABLE_HTML_CHECK", "disable-html-check"),
    alias("MP_SEND_API_AUTH_FILE", "send-api-auth-file"),
    env_only("MP_SEND_API_AUTH", Active(Key::SendApiAuth)),
    alias("MP_SEND_API_AUTH_ACCEPT_ANY", "send-api-auth-accept-any"),
    alias("MP_SMTP_BIND_ADDR", "smtp"),
    alias("MP_SMTP_AUTH_FILE", "smtp-auth-file"),
    env_only("MP_SMTP_AUTH", Active(Key::SmtpAuth)),
    alias("MP_SMTP_AUTH_ACCEPT_ANY", "smtp-auth-accept-any"),
    alias("MP_SMTP_AUTH_ALLOW_INSECURE", "smtp-auth-allow-insecure"),
    alias("MP_SMTP_TLS_CERT", "smtp-tls-cert"),
    alias("MP_SMTP_TLS_KEY", "smtp-tls-key"),
    alias("MP_SMTP_SSL_CERT", "smtp-ssl-cert"),
    alias("MP_SMTP_SSL_KEY", "smtp-ssl-key"),
    // The deprecated key is also read from this misspelled name; accept both.
    alias("MP_SMTP_SMTP_KEY", "smtp-ssl-key"),
    alias("MP_SMTP_REQUIRE_STARTTLS", "smtp-require-starttls"),
    alias("MP_SMTP_TLS_REQUIRED", "smtp-tls-required"),
    alias("MP_SMTP_REQUIRE_TLS", "smtp-require-tls"),
    alias("MP_SMTP_STRICT_RFC_HEADERS", "smtp-strict-rfc-headers"),
    alias("MP_STRICT_RFC_HEADERS", "smtp-strict-rfc-headers"),
    alias("MP_SMTP_MAX_RECIPIENTS", "smtp-max-recipients"),
    alias("MP_SMTP_ALLOWED_RECIPIENTS", "smtp-allowed-recipients"),
    alias(
        "MP_SMTP_IGNORE_REJECTED_RECIPIENTS",
        "smtp-ignore-rejected-recipients",
    ),
    alias("MP_SMTP_DISABLE_RDNS", "smtp-disable-rdns"),
    alias("MP_SMTP_RELAY_CONFIG", "smtp-relay-config"),
    alias("MP_SMTP_RELAY_ALL", "smtp-relay-all"),
    alias("MP_SMTP_RELAY_MATCHING", "smtp-relay-matching"),
    env_only("MP_SMTP_RELAY_HOST", Active(Key::Relay(Field::Host))),
    env_only("MP_SMTP_RELAY_PORT", Active(Key::Relay(Field::Port))),
    env_only(
        "MP_SMTP_RELAY_STARTTLS",
        Active(Key::Relay(Field::StartTls)),
    ),
    env_only("MP_SMTP_RELAY_TLS", Active(Key::Relay(Field::Tls))),
    env_only(
        "MP_SMTP_RELAY_ALLOW_INSECURE",
        Active(Key::Relay(Field::AllowInsecure)),
    ),
    env_only("MP_SMTP_RELAY_AUTH", Active(Key::Relay(Field::Auth))),
    env_only(
        "MP_SMTP_RELAY_USERNAME",
        Active(Key::Relay(Field::Username)),
    ),
    env_only(
        "MP_SMTP_RELAY_PASSWORD",
        Active(Key::Relay(Field::Password)),
    ),
    env_only("MP_SMTP_RELAY_SECRET", NO_CRAM_MD5),
    env_only(
        "MP_SMTP_RELAY_RETURN_PATH",
        Active(Key::Relay(Field::ReturnPath)),
    ),
    env_only(
        "MP_SMTP_RELAY_OVERRIDE_FROM",
        Active(Key::Relay(Field::OverrideFrom)),
    ),
    env_only(
        "MP_SMTP_RELAY_ALLOWED_RECIPIENTS",
        Active(Key::Relay(Field::AllowedRecipients)),
    ),
    env_only(
        "MP_SMTP_RELAY_BLOCKED_RECIPIENTS",
        Active(Key::Relay(Field::BlockedRecipients)),
    ),
    env_only(
        "MP_SMTP_RELAY_PRESERVE_MESSAGE_IDS",
        Active(Key::Relay(Field::PreserveMessageIds)),
    ),
    env_only("MP_SMTP_RELAY_FWD_SMTP_ERRORS", NO_SMTP_ERRORS),
    alias("MP_SMTP_FORWARD_CONFIG", "smtp-forward-config"),
    env_only("MP_SMTP_FORWARD_HOST", Active(Key::Forward(Field::Host))),
    env_only("MP_SMTP_FORWARD_PORT", Active(Key::Forward(Field::Port))),
    env_only(
        "MP_SMTP_FORWARD_STARTTLS",
        Active(Key::Forward(Field::StartTls)),
    ),
    env_only("MP_SMTP_FORWARD_TLS", Active(Key::Forward(Field::Tls))),
    env_only(
        "MP_SMTP_FORWARD_ALLOW_INSECURE",
        Active(Key::Forward(Field::AllowInsecure)),
    ),
    env_only("MP_SMTP_FORWARD_AUTH", Active(Key::Forward(Field::Auth))),
    env_only(
        "MP_SMTP_FORWARD_USERNAME",
        Active(Key::Forward(Field::Username)),
    ),
    env_only(
        "MP_SMTP_FORWARD_PASSWORD",
        Active(Key::Forward(Field::Password)),
    ),
    env_only("MP_SMTP_FORWARD_SECRET", NO_CRAM_MD5),
    env_only(
        "MP_SMTP_FORWARD_RETURN_PATH",
        Active(Key::Forward(Field::ReturnPath)),
    ),
    env_only(
        "MP_SMTP_FORWARD_OVERRIDE_FROM",
        Active(Key::Forward(Field::OverrideFrom)),
    ),
    env_only("MP_SMTP_FORWARD_TO", Active(Key::Forward(Field::To))),
    env_only("MP_SMTP_FORWARD_FWD_SMTP_ERRORS", NO_SMTP_ERRORS),
    alias("MP_ENABLE_CHAOS", "enable-chaos"),
    alias("MP_CHAOS_TRIGGERS", "chaos-triggers"),
    alias("MP_POP3_BIND_ADDR", "pop3"),
    alias("MP_POP3_AUTH_FILE", "pop3-auth-file"),
    env_only("MP_POP3_AUTH", Active(Key::Pop3Auth)),
    alias("MP_POP3_TLS_CERT", "pop3-tls-cert"),
    alias("MP_POP3_TLS_KEY", "pop3-tls-key"),
    alias("MP_TAG", "tag"),
    alias("MP_TAGS_CONFIG", "tags-config"),
    alias("MP_TAGS_TITLE_CASE", "tags-title-case"),
    alias("MP_TAGS_DISABLE", "tags-disable"),
    alias("MP_TAGS_USERNAME", "tags-username"),
    alias("MP_ENABLE_PROMETHEUS", "enable-prometheus"),
    alias("MP_WEBHOOK_URL", "webhook-url"),
    alias("MP_WEBHOOK_LIMIT", "webhook-limit"),
    alias("MP_WEBHOOK_DELAY", "webhook-delay"),
    env_only("MP_DEMO_MODE", Removed("demo mode is not supported")),
];

pub const NATIVE_ENV_PREFIX: &str = "SPOOLBOX_";

/// `SPOOLBOX_` + upper-snake long name, for active flags only.
pub fn native_env_name(long: &str) -> String {
    format!(
        "{NATIVE_ENV_PREFIX}{}",
        long.to_ascii_uppercase().replace('-', "_")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn short_options_only_on_compat_flags() {
        for f in FLAGS {
            assert!(
                f.short.is_none() || f.compat,
                "--{} has a short option",
                f.long
            );
        }
        assert!(COMPAT_ENV.iter().all(|a| a.name.starts_with("MP_")));
    }

    #[test]
    fn aliases_and_deprecations_point_to_registered_flags() {
        for a in COMPAT_ENV {
            if let EnvTarget::Flag(long) = a.target {
                assert!(
                    flag_by_long(long).is_some(),
                    "{} -> --{long} missing",
                    a.name
                );
            }
        }
        for f in FLAGS {
            if let Status::Deprecated(target) = f.status {
                assert!(
                    flag_by_long(target).is_some(),
                    "--{} -> --{target} missing",
                    f.long
                );
            }
        }
    }

    #[test]
    fn long_and_short_names_are_unique() {
        let mut longs = HashSet::new();
        let mut shorts = HashSet::new();
        for f in FLAGS {
            assert!(longs.insert(f.long), "duplicate --{}", f.long);
            if let Some(s) = f.short {
                assert!(shorts.insert(s), "duplicate -{s}");
            }
        }
    }
}
