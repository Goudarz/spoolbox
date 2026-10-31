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
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Network {
    allow_unauthenticated_network: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Log {
    level: Option<String>,
    format: Option<String>,
    file: Option<String>,
}

pub(crate) type Assignment = (Key, String, Origin);

pub(crate) fn read(path: &Path) -> Result<Vec<Assignment>, ConfigError> {
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

pub(crate) fn parse(text: &str, path: &Path) -> Result<Vec<Assignment>, ConfigError> {
    let cfg: FileConfig = toml::from_str(text).map_err(|e| ConfigError::FileParse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
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

    let log = cfg.log.unwrap_or_default();
    push(Key::LogLevel, "log.level", log.level);
    push(Key::LogFormat, "log.format", log.format);
    push(Key::LogFile, "log.file", log.file);
    Ok(out)
}
