//! TOML configuration file. Unknown keys are rejected so that typos are reported.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{ConfigError, Origin};
use crate::key::Key;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileConfig {
    label: Option<String>,
    http: Option<Http>,
    smtp: Option<Smtp>,
    storage: Option<Storage>,
    tags: Option<Tags>,
    network: Option<Network>,
    log: Option<Log>,
    chaos: Option<ChaosFile>,
    webhook: Option<Webhook>,
    relay: Option<Relay>,
    forward: Option<Forward>,
    pop3: Option<Pop3>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Pop3 {
    listen: Option<String>,
    auth_file: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Webhook {
    url: Option<String>,
    limit_per_second: Option<u64>,
    delay_seconds: Option<u64>,
    secret: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Relay {
    /// YAML file with the server settings.
    config: Option<String>,
    all: Option<bool>,
    matching: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Forward {
    /// YAML file with the server settings.
    config: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ChaosFile {
    enabled: Option<bool>,
    triggers: Option<String>,
    rules: Option<Vec<RuleFile>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleFile {
    stage: String,
    #[serde(rename = "match")]
    pattern: Option<String>,
    code: Option<u16>,
    message: Option<String>,
    delay_ms: Option<u64>,
    disconnect: Option<bool>,
    times: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Http {
    listen: Option<String>,
    webroot: Option<String>,
    allowed_hosts: Option<Vec<String>>,
    auth_file: Option<String>,
    send_api_auth_file: Option<String>,
    send_api_auth_accept_any: Option<bool>,
    cors_origins: Option<Vec<String>>,
    compression: Option<bool>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    hide_delete_all_button: Option<bool>,
    block_remote_css_and_fonts: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Smtp {
    listen: Option<String>,
    max_recipients: Option<u64>,
    max_message_size_mib: Option<u64>,
    max_connections: Option<u64>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    require_starttls: Option<bool>,
    require_tls: Option<bool>,
    auth_accept_any: Option<bool>,
    auth_file: Option<String>,
    auth_allow_insecure: Option<bool>,
    transcripts: Option<bool>,
    allowed_recipients: Option<String>,
    ignore_rejected_recipients: Option<bool>,
    strict_rfc_headers: Option<bool>,
    hide_extensions: Option<Vec<String>>,
    accept_domains: Option<Vec<String>>,
    reject_domains: Option<Vec<String>>,
    discard_domains: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Storage {
    database: Option<String>,
    max_messages: Option<u64>,
    max_age: Option<String>,
    min_free_disk_mib: Option<u64>,
    compression: Option<u64>,
    wal: Option<bool>,
    auto_vacuum: Option<bool>,
    ignore_duplicate_ids: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Tags {
    disable: Option<Vec<String>>,
    title_case: Option<bool>,
    username: Option<bool>,
    rules: Option<Vec<TagRuleFile>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TagRuleFile {
    #[serde(rename = "match")]
    query: String,
    tags: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Network {
    allow_unauthenticated_network: Option<bool>,
    allow_internal_http_requests: Option<bool>,
    allow_untrusted_tls: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Log {
    level: Option<String>,
    format: Option<String>,
    file: Option<String>,
}

pub(crate) type Assignment = (Key, String, Origin);

/// Settings from the file, plus structured rules that are not expressible as one value.
#[derive(Debug, Default)]
pub(crate) struct Parsed {
    pub assignments: Vec<Assignment>,
    pub chaos_rules: Option<Vec<spoolbox_core::chaos::Rule>>,
    pub tag_rules: Vec<crate::TagRule>,
}

pub(crate) fn read(path: &Path) -> Result<Parsed, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::FileRead {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&text, path)
}

fn num(v: Option<u64>) -> Option<String> {
    v.map(|v| v.to_string())
}

fn flag(v: Option<bool>) -> Option<String> {
    v.map(|v| v.to_string())
}

fn not(v: Option<bool>) -> Option<String> {
    v.map(|v| (!v).to_string())
}

fn join(v: Option<Vec<String>>) -> Option<String> {
    v.map(|v| v.join(","))
}

fn rules(path: &Path, list: Vec<RuleFile>) -> Result<Vec<spoolbox_core::chaos::Rule>, ConfigError> {
    list.into_iter()
        .enumerate()
        .map(|(i, r)| {
            let invalid = |message: String| ConfigError::FileParse {
                path: path.to_path_buf(),
                message: format!("chaos.rules[{i}]: {message}"),
            };
            let rule = spoolbox_core::chaos::Rule {
                stage: spoolbox_core::chaos::RuleStage::parse(&r.stage).map_err(invalid)?,
                pattern: r.pattern,
                code: r.code,
                message: r.message,
                delay_ms: r.delay_ms.unwrap_or(0),
                disconnect: r.disconnect.unwrap_or(false),
                times: r.times,
            };
            rule.validate().map_err(invalid)?;
            Ok(rule)
        })
        .collect()
}

pub(crate) fn parse(text: &str, path: &Path) -> Result<Parsed, ConfigError> {
    let mut cfg: FileConfig = toml::from_str(text).map_err(|e| ConfigError::FileParse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let chaos = cfg.chaos.take().unwrap_or_default();
    let chaos_rules = chaos.rules.map(|r| rules(path, r)).transpose()?;
    let mut tag_rules = Vec::new();
    if let Some(list) = cfg.tags.as_mut().and_then(|t| t.rules.take()) {
        for (i, r) in list.into_iter().enumerate() {
            let tags = r
                .tags
                .iter()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            if r.query.trim().is_empty() {
                return Err(ConfigError::FileParse {
                    path: path.to_path_buf(),
                    message: format!("tags.rules[{i}]: `match` is empty"),
                });
            }
            tag_rules.push(crate::TagRule {
                query: r.query.trim().to_string(),
                tags,
            });
        }
    }
    let mut out = Vec::new();
    let mut push = |key: Key, name: &str, value: Option<String>| {
        if let Some(v) = value {
            out.push((
                key,
                v,
                Origin::File {
                    path: PathBuf::from(path),
                    key: name.to_string(),
                },
            ));
        }
    };
    push(Key::Label, "label", cfg.label);

    let http = cfg.http.unwrap_or_default();
    push(Key::HttpListen, "http.listen", http.listen);
    push(Key::Webroot, "http.webroot", http.webroot);
    push(
        Key::AllowedHosts,
        "http.allowed_hosts",
        join(http.allowed_hosts),
    );
    push(Key::UiAuthFile, "http.auth_file", http.auth_file);
    push(
        Key::SendApiAuthFile,
        "http.send_api_auth_file",
        http.send_api_auth_file,
    );
    push(
        Key::SendApiAuthAcceptAny,
        "http.send_api_auth_accept_any",
        flag(http.send_api_auth_accept_any),
    );
    push(Key::ApiCors, "http.cors_origins", join(http.cors_origins));
    push(
        Key::DisableHttpCompression,
        "http.compression",
        not(http.compression),
    );
    push(Key::UiTlsCert, "http.tls_cert", http.tls_cert);
    push(Key::UiTlsKey, "http.tls_key", http.tls_key);
    push(
        Key::HideDeleteAllButton,
        "http.hide_delete_all_button",
        flag(http.hide_delete_all_button),
    );
    push(
        Key::BlockRemoteCssAndFonts,
        "http.block_remote_css_and_fonts",
        flag(http.block_remote_css_and_fonts),
    );

    let smtp = cfg.smtp.unwrap_or_default();
    push(Key::SmtpListen, "smtp.listen", smtp.listen);
    push(
        Key::SmtpMaxRecipients,
        "smtp.max_recipients",
        num(smtp.max_recipients),
    );
    push(
        Key::MaxMessageSize,
        "smtp.max_message_size_mib",
        num(smtp.max_message_size_mib),
    );
    push(
        Key::SmtpMaxConnections,
        "smtp.max_connections",
        num(smtp.max_connections),
    );
    push(Key::SmtpTlsCert, "smtp.tls_cert", smtp.tls_cert);
    push(Key::SmtpTlsKey, "smtp.tls_key", smtp.tls_key);
    push(
        Key::SmtpRequireStarttls,
        "smtp.require_starttls",
        flag(smtp.require_starttls),
    );
    push(
        Key::SmtpRequireTls,
        "smtp.require_tls",
        flag(smtp.require_tls),
    );
    push(
        Key::SmtpAuthAcceptAny,
        "smtp.auth_accept_any",
        flag(smtp.auth_accept_any),
    );
    push(Key::SmtpAuthFile, "smtp.auth_file", smtp.auth_file);
    push(
        Key::SmtpAuthAllowInsecure,
        "smtp.auth_allow_insecure",
        flag(smtp.auth_allow_insecure),
    );
    push(
        Key::SmtpTranscripts,
        "smtp.transcripts",
        flag(smtp.transcripts),
    );
    push(
        Key::SmtpAllowedRecipients,
        "smtp.allowed_recipients",
        smtp.allowed_recipients,
    );
    push(
        Key::SmtpIgnoreRejectedRecipients,
        "smtp.ignore_rejected_recipients",
        flag(smtp.ignore_rejected_recipients),
    );
    push(
        Key::SmtpStrictRfcHeaders,
        "smtp.strict_rfc_headers",
        flag(smtp.strict_rfc_headers),
    );
    push(
        Key::SmtpHideExtensions,
        "smtp.hide_extensions",
        join(smtp.hide_extensions),
    );
    push(
        Key::SmtpAcceptDomains,
        "smtp.accept_domains",
        join(smtp.accept_domains),
    );
    push(
        Key::SmtpRejectDomains,
        "smtp.reject_domains",
        join(smtp.reject_domains),
    );
    push(
        Key::SmtpDiscardDomains,
        "smtp.discard_domains",
        join(smtp.discard_domains),
    );

    let storage = cfg.storage.unwrap_or_default();
    push(Key::Database, "storage.database", storage.database);
    push(
        Key::MaxMessages,
        "storage.max_messages",
        num(storage.max_messages),
    );
    push(Key::MaxAge, "storage.max_age", storage.max_age);
    push(
        Key::MinFreeDisk,
        "storage.min_free_disk_mib",
        num(storage.min_free_disk_mib),
    );
    push(
        Key::Compression,
        "storage.compression",
        num(storage.compression),
    );
    push(Key::DisableWal, "storage.wal", not(storage.wal));
    push(
        Key::DisableAutoVacuum,
        "storage.auto_vacuum",
        not(storage.auto_vacuum),
    );
    push(
        Key::IgnoreDuplicateIds,
        "storage.ignore_duplicate_ids",
        flag(storage.ignore_duplicate_ids),
    );

    let tags = cfg.tags.unwrap_or_default();
    push(Key::TagsDisable, "tags.disable", join(tags.disable));
    push(Key::TagsTitleCase, "tags.title_case", flag(tags.title_case));
    push(Key::TagsUsername, "tags.username", flag(tags.username));

    let network = cfg.network.unwrap_or_default();
    push(
        Key::AllowUnauthenticatedNetwork,
        "network.allow_unauthenticated_network",
        flag(network.allow_unauthenticated_network),
    );
    push(
        Key::AllowInternalHttpRequests,
        "network.allow_internal_http_requests",
        flag(network.allow_internal_http_requests),
    );
    push(
        Key::AllowUntrustedTls,
        "network.allow_untrusted_tls",
        flag(network.allow_untrusted_tls),
    );

    let webhook = cfg.webhook.unwrap_or_default();
    push(Key::WebhookUrl, "webhook.url", webhook.url);
    push(
        Key::WebhookLimit,
        "webhook.limit_per_second",
        num(webhook.limit_per_second),
    );
    push(
        Key::WebhookDelay,
        "webhook.delay_seconds",
        num(webhook.delay_seconds),
    );
    push(Key::WebhookSecret, "webhook.secret", webhook.secret);

    let relay = cfg.relay.unwrap_or_default();
    push(Key::SmtpRelayConfig, "relay.config", relay.config);
    push(Key::SmtpRelayAll, "relay.all", flag(relay.all));
    push(Key::SmtpRelayMatching, "relay.matching", relay.matching);
    let forward = cfg.forward.unwrap_or_default();
    push(Key::SmtpForwardConfig, "forward.config", forward.config);
    let pop3 = cfg.pop3.unwrap_or_default();
    push(Key::Pop3Listen, "pop3.listen", pop3.listen);
    push(Key::Pop3AuthFile, "pop3.auth_file", pop3.auth_file);
    push(Key::Pop3TlsCert, "pop3.tls_cert", pop3.tls_cert);
    push(Key::Pop3TlsKey, "pop3.tls_key", pop3.tls_key);

    let log = cfg.log.unwrap_or_default();
    push(Key::LogLevel, "log.level", log.level);
    push(Key::LogFormat, "log.format", log.format);
    push(Key::LogFile, "log.file", log.file);
    push(Key::EnableChaos, "chaos.enabled", flag(chaos.enabled));
    push(Key::ChaosTriggers, "chaos.triggers", chaos.triggers);
    Ok(Parsed {
        assignments: out,
        chaos_rules,
        tag_rules,
    })
}
