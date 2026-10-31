use std::path::PathBuf;
use std::time::Duration;

use crate::error::{ConfigError, Origin};
use crate::settings::{HIDEABLE_EXTENSIONS, LogFormat, LogLevel, MIB, Settings};

/// A setting that can be assigned from a string by any configuration source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    ConfigFile,
    Label,
    Database,
    MaxMessages,
    MaxAge,
    MaxMessageSize,
    MinFreeDisk,
    HttpListen,
    Webroot,
    AllowedHosts,
    SmtpListen,
    SmtpMaxRecipients,
    AllowUnauthenticatedNetwork,
    LogLevel,
    LogFormat,
    LogFile,
    Quiet,
    Verbose,
    SmtpTlsCert,
    SmtpTlsKey,
    SmtpRequireStarttls,
    SmtpRequireTls,
    SmtpAuthAcceptAny,
    SmtpAllowedRecipients,
    SmtpIgnoreRejectedRecipients,
    SmtpStrictRfcHeaders,
    SmtpHideExtensions,
    SmtpAcceptDomains,
    SmtpRejectDomains,
    SmtpDiscardDomains,
    SmtpMaxConnections,
    Compression,
    DisableWal,
    DisableAutoVacuum,
    IgnoreDuplicateIds,
    TagsDisable,
    TagsTitleCase,
    TagsUsername,
    UiAuthFile,
    /// Inline `user:password` entries (`MP_UI_AUTH`).
    UiAuth,
    SendApiAuthFile,
    SendApiAuth,
    SendApiAuthAcceptAny,
    SmtpAuthFile,
    SmtpAuth,
    SmtpAuthAllowInsecure,
    ApiCors,
    DisableHttpCompression,
    EnableChaos,
    ChaosTriggers,
    UiTlsCert,
    UiTlsKey,
    HideDeleteAllButton,
    BlockRemoteCssAndFonts,
    SmtpTranscripts,
    McpAllowDelete,
}

impl Key {
    pub fn is_bool(self) -> bool {
        matches!(
            self,
            Key::AllowUnauthenticatedNetwork
                | Key::Quiet
                | Key::Verbose
                | Key::SmtpRequireStarttls
                | Key::SmtpRequireTls
                | Key::SmtpAuthAcceptAny
                | Key::SmtpIgnoreRejectedRecipients
                | Key::SmtpStrictRfcHeaders
                | Key::DisableWal
                | Key::DisableAutoVacuum
                | Key::IgnoreDuplicateIds
                | Key::TagsTitleCase
                | Key::TagsUsername
                | Key::SendApiAuthAcceptAny
                | Key::SmtpAuthAllowInsecure
                | Key::DisableHttpCompression
                | Key::EnableChaos
                | Key::HideDeleteAllButton
                | Key::BlockRemoteCssAndFonts
                | Key::SmtpTranscripts
                | Key::McpAllowDelete
        )
    }
}

/// Accumulates assignments in precedence order; later assignments win.
#[derive(Debug, Default)]
pub(crate) struct Builder {
    pub settings: Settings,
    pub quiet: Option<Origin>,
    pub verbose: Option<Origin>,
    /// Resolved into `settings.smtp.tls` once all sources are applied.
    pub tls_cert: Option<(String, Origin)>,
    pub tls_key: Option<(String, Origin)>,
    /// Credentials are read and validated once all sources are applied.
    pub ui_auth: AuthSources,
    pub send_api_auth: AuthSources,
    pub smtp_auth: AuthSources,
    pub cors: Option<(String, Origin)>,
    pub ui_tls_cert: Option<(String, Origin)>,
    pub ui_tls_key: Option<(String, Origin)>,
}

/// An htpasswd file and/or inline entries; the file wins when both are set.
#[derive(Debug, Default)]
pub(crate) struct AuthSources {
    pub file: Option<(String, Origin)>,
    pub inline: Option<(String, Origin)>,
}

fn non_empty(v: &str, origin: &Origin) -> Option<(String, Origin)> {
    (!v.is_empty()).then(|| (v.to_string(), origin.clone()))
}

fn list(v: &str) -> Vec<String> {
    v.split(',')
        .map(|x| x.trim().to_ascii_lowercase())
        .filter(|x| !x.is_empty())
        .collect()
}

impl Builder {
    pub fn apply(&mut self, key: Key, raw: &str, origin: &Origin) -> Result<(), ConfigError> {
        let invalid = |reason: &str| ConfigError::InvalidValue {
            origin: origin.clone(),
            value: raw.to_string(),
            reason: reason.to_string(),
        };
        let s = &mut self.settings;
        let v = raw.trim();
        match key {
            Key::ConfigFile => {}
            Key::Label => s.label = v.to_string(),
            Key::Database => {
                if v.starts_with("http://") || v.starts_with("https://") {
                    return Err(ConfigError::Removed {
                        origin: origin.clone(),
                        reason: "rqlite databases are not supported (ADR-016); use a local SQLite file path",
                    });
                }
                s.storage.database = (!v.is_empty()).then(|| PathBuf::from(v));
            }
            Key::MaxMessages => {
                s.storage.max_messages = v
                    .parse()
                    .map_err(|_| invalid("expected a non-negative integer"))?
            }
            Key::MaxAge => s.storage.max_age = parse_max_age(v).map_err(invalid)?,
            Key::MaxMessageSize => {
                s.smtp.max_message_size =
                    parse_mib(v).ok_or_else(|| invalid("expected a size in MiB"))?
            }
            Key::MinFreeDisk => {
                s.storage.min_free_disk =
                    parse_mib(v).ok_or_else(|| invalid("expected a size in MiB"))?
            }
            Key::HttpListen => {
                s.http.listen = v
                    .parse()
                    .map_err(|e: spoolbox_core::ListenAddrError| invalid(&e.to_string()))?
            }
            Key::SmtpListen => {
                s.smtp.listen = v
                    .parse()
                    .map_err(|e: spoolbox_core::ListenAddrError| invalid(&e.to_string()))?
            }
            Key::Webroot => s.http.webroot = normalize_webroot(v).map_err(invalid)?,
            Key::AllowedHosts => {
                s.http.allowed_hosts = v
                    .split(',')
                    .map(|h| h.trim().to_ascii_lowercase())
                    .filter(|h| !h.is_empty())
                    .collect();
            }
            Key::SmtpMaxRecipients => {
                let n: u32 = v
                    .parse()
                    .map_err(|_| invalid("expected a positive integer"))?;
                if n == 0 {
                    return Err(invalid(
                        "must be at least 1 (RFC 5321 requires accepting at least 100)",
                    ));
                }
                s.smtp.max_recipients = n;
            }
            Key::AllowUnauthenticatedNetwork => {
                s.network.allow_unauthenticated_network =
                    parse_bool(v).ok_or_else(|| invalid("expected true or false"))?
            }
            Key::LogLevel => {
                s.log.level = match v.to_ascii_lowercase().as_str() {
                    "error" => LogLevel::Error,
                    "warn" | "warning" => LogLevel::Warn,
                    "info" => LogLevel::Info,
                    "debug" => LogLevel::Debug,
                    "trace" => LogLevel::Trace,
                    _ => return Err(invalid("expected one of: error, warn, info, debug, trace")),
                }
            }
            Key::LogFormat => {
                s.log.format = match v.to_ascii_lowercase().as_str() {
                    "text" => LogFormat::Text,
                    "json" => LogFormat::Json,
                    _ => return Err(invalid("expected text or json")),
                }
            }
            Key::LogFile => s.log.file = (!v.is_empty()).then(|| PathBuf::from(v)),
            Key::Quiet | Key::Verbose => {
                let on = parse_bool(v).ok_or_else(|| invalid("expected true or false"))?;
                let slot = if key == Key::Quiet {
                    &mut self.quiet
                } else {
                    &mut self.verbose
                };
                *slot = on.then(|| origin.clone());
                if on {
                    s.log.level = if key == Key::Quiet {
                        LogLevel::Error
                    } else {
                        LogLevel::Debug
                    };
                }
            }
            Key::SmtpTlsCert => {
                self.tls_cert = (!v.is_empty()).then(|| (v.to_string(), origin.clone()))
            }
            Key::SmtpTlsKey => {
                self.tls_key = (!v.is_empty()).then(|| (v.to_string(), origin.clone()))
            }
            Key::SmtpAllowedRecipients => {
                if v.is_empty() {
                    s.smtp.allowed_recipients = None;
                } else {
                    regex::Regex::new(v)
                        .map_err(|e| invalid(&format!("invalid regular expression: {e}")))?;
                    s.smtp.allowed_recipients = Some(v.to_string());
                }
            }
            Key::SmtpHideExtensions => {
                let mut out = Vec::new();
                for e in v
                    .split(',')
                    .map(|x| x.trim().to_ascii_uppercase())
                    .filter(|x| !x.is_empty())
                {
                    if !HIDEABLE_EXTENSIONS.contains(&e.as_str()) {
                        return Err(invalid(&format!(
                            "unknown extension {e:?} (expected some of: {})",
                            HIDEABLE_EXTENSIONS.join(", ")
                        )));
                    }
                    out.push(e);
                }
                s.smtp.hide_extensions = out;
            }
            Key::UiAuthFile => self.ui_auth.file = non_empty(v, origin),
            Key::UiAuth => self.ui_auth.inline = non_empty(v, origin),
            Key::SendApiAuthFile => self.send_api_auth.file = non_empty(v, origin),
            Key::SendApiAuth => self.send_api_auth.inline = non_empty(v, origin),
            Key::SmtpAuthFile => self.smtp_auth.file = non_empty(v, origin),
            Key::SmtpAuth => self.smtp_auth.inline = non_empty(v, origin),
            Key::ApiCors => self.cors = non_empty(v, origin),
            Key::UiTlsCert => self.ui_tls_cert = non_empty(v, origin),
            Key::UiTlsKey => self.ui_tls_key = non_empty(v, origin),
            Key::ChaosTriggers => {
                if !v.is_empty() {
                    s.chaos.triggers =
                        spoolbox_core::chaos::Triggers::parse(v).map_err(|e| invalid(&e))?;
                    // Setting triggers enables chaos.
                    s.chaos.enabled = true;
                }
            }
            Key::SmtpAcceptDomains => s.smtp.accept_domains = list(v),
            Key::SmtpRejectDomains => s.smtp.reject_domains = list(v),
            Key::SmtpDiscardDomains => s.smtp.discard_domains = list(v),
            Key::SmtpMaxConnections => {
                let n: u32 = v
                    .parse()
                    .map_err(|_| invalid("expected a positive integer"))?;
                if n == 0 {
                    return Err(invalid("must be at least 1"));
                }
                s.smtp.max_connections = n;
            }
            Key::Compression => {
                s.storage.compression = match v.parse::<u8>() {
                    Ok(n @ 0..=3) => n,
                    _ => {
                        return Err(invalid(
                            "expected 0 (none), 1 (fastest), 2 (default) or 3 (best)",
                        ));
                    }
                }
            }
            Key::TagsDisable => {
                for t in list(v) {
                    match t.as_str() {
                        "plus-addresses" | "plus-address" | "plus" => s.tags.plus_addresses = false,
                        "x-tags" | "xtags" => s.tags.x_tags = false,
                        other => {
                            return Err(invalid(&format!(
                                "unknown tag source {other:?} (expected plus-addresses, x-tags)"
                            )));
                        }
                    }
                }
            }
            Key::SmtpRequireStarttls
            | Key::SmtpRequireTls
            | Key::SmtpAuthAcceptAny
            | Key::SmtpIgnoreRejectedRecipients
            | Key::SmtpStrictRfcHeaders
            | Key::DisableWal
            | Key::DisableAutoVacuum
            | Key::IgnoreDuplicateIds
            | Key::TagsTitleCase
            | Key::TagsUsername
            | Key::SendApiAuthAcceptAny
            | Key::SmtpAuthAllowInsecure
            | Key::DisableHttpCompression
            | Key::EnableChaos
            | Key::HideDeleteAllButton
            | Key::BlockRemoteCssAndFonts
            | Key::SmtpTranscripts
            | Key::McpAllowDelete => {
                let on = parse_bool(v).ok_or_else(|| invalid("expected true or false"))?;
                match key {
                    Key::SmtpRequireStarttls => s.smtp.require_starttls = on,
                    Key::SmtpRequireTls => s.smtp.implicit_tls = on,
                    Key::SmtpAuthAcceptAny => s.smtp.auth_accept_any = on,
                    Key::SmtpIgnoreRejectedRecipients => s.smtp.ignore_rejected_recipients = on,
                    Key::SmtpStrictRfcHeaders => s.smtp.strict_rfc_headers = on,
                    Key::DisableWal => s.storage.wal = !on,
                    Key::DisableAutoVacuum => s.storage.auto_vacuum = !on,
                    Key::IgnoreDuplicateIds => s.storage.ignore_duplicate_ids = on,
                    Key::TagsTitleCase => s.tags.title_case = on,
                    Key::SendApiAuthAcceptAny => s.http.send_auth_accept_any = on,
                    Key::SmtpAuthAllowInsecure => s.smtp.auth_allow_insecure = on,
                    Key::DisableHttpCompression => s.http.compression = !on,
                    Key::EnableChaos => s.chaos.enabled |= on,
                    Key::HideDeleteAllButton => s.http.hide_delete_all_button = on,
                    Key::BlockRemoteCssAndFonts => s.http.block_remote_css_and_fonts = on,
                    Key::SmtpTranscripts => s.smtp.transcripts = on,
                    Key::McpAllowDelete => s.http.mcp_allow_delete = on,
                    _ => s.tags.username = on,
                }
            }
        }
        Ok(())
    }
}

/// Strict boolean used for Spoolbox's own sources.
pub fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// `MP_*` booleans: only `1`, `true` and `yes` (any case) enable a flag.
pub fn parse_compat_bool(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes")
}

fn parse_mib(v: &str) -> Option<u64> {
    v.parse::<u64>().ok()?.checked_mul(MIB)
}

/// `<int>h` or `<int>d`; empty or `0` disables.
pub fn parse_max_age(v: &str) -> Result<Option<Duration>, &'static str> {
    if v.is_empty() || v == "0" {
        return Ok(None);
    }
    let lower = v.to_ascii_lowercase();
    let (num, mult) = if let Some(n) = lower.strip_suffix('h') {
        (n, 3600)
    } else if let Some(n) = lower.strip_suffix('d') {
        (n, 86_400)
    } else {
        return Err("expected hours or days, e.g. 12h or 3d");
    };
    let n: u64 = num
        .parse()
        .map_err(|_| "expected hours or days, e.g. 12h or 3d")?;
    if n == 0 {
        return Ok(None);
    }
    n.checked_mul(mult)
        .map(|s| Some(Duration::from_secs(s)))
        .ok_or("value too large")
}

/// Ensures a leading and trailing `/` and only URL-path-safe characters.
pub fn normalize_webroot(v: &str) -> Result<String, &'static str> {
    if v.is_empty() || v == "/" {
        return Ok("/".into());
    }
    if !v
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-._~/".contains(c))
    {
        return Err("may only contain letters, digits, '-', '.', '_', '~' and '/'");
    }
    if v.split('/').any(|seg| seg == "..") {
        return Err("must not contain '..' segments");
    }
    let trimmed = v.trim_matches('/');
    let mut out = String::with_capacity(trimmed.len() + 2);
    out.push('/');
    for seg in trimmed.split('/').filter(|s| !s.is_empty()) {
        out.push_str(seg);
        out.push('/');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_age() {
        assert_eq!(parse_max_age(""), Ok(None));
        assert_eq!(
            parse_max_age("12h"),
            Ok(Some(Duration::from_secs(12 * 3600)))
        );
        assert_eq!(
            parse_max_age("3D"),
            Ok(Some(Duration::from_secs(3 * 86_400)))
        );
        assert!(parse_max_age("3w").is_err());
        assert!(parse_max_age("h").is_err());
    }

    #[test]
    fn webroot() {
        assert_eq!(normalize_webroot("").as_deref(), Ok("/"));
        assert_eq!(normalize_webroot("mail").as_deref(), Ok("/mail/"));
        assert_eq!(normalize_webroot("/a//b/").as_deref(), Ok("/a/b/"));
        assert!(normalize_webroot("/a/../b").is_err());
        assert!(normalize_webroot("/a b").is_err());
    }

    #[test]
    fn compat_bool_semantics() {
        for v in ["1", "true", "TRUE", "yes"] {
            assert!(parse_compat_bool(v), "{v}");
        }
        for v in ["0", "false", "on", "", "y"] {
            assert!(!parse_compat_bool(v), "{v}");
        }
    }
}
