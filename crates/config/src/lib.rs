//! Layered configuration. Precedence, lowest to highest:
//! built-in defaults → TOML file (`--config` / `SPOOLBOX_CONFIG`) → `MP_*` (compatibility)
//! environment → `SPOOLBOX_*` environment → command-line flags.
//! Empty environment values are ignored.

mod error;
mod file;
mod key;
pub mod registry;
mod relay;
mod settings;
mod suggest;
mod tags;

use std::collections::HashMap;
use std::path::PathBuf;

pub use error::{ConfigError, Diagnostic, DiagnosticLevel, Origin};
pub use key::Key;
pub use relay::{Field as RelayField, is_plain_address};
pub use settings::{
    ChaosSettings, EgressSettings, ForwardSettings, HIDEABLE_EXTENSIONS, HttpSettings, LogFormat,
    LogLevel, LogSettings, MIB, NetworkSettings, RelaySettings, Settings, SmtpLogin, SmtpMechanism,
    SmtpSecurity, SmtpSettings, SmtpTarget, StorageSettings, TagRule, TagSettings, TlsSource,
    WebhookSettings,
};

use key::{AuthSources, Builder, parse_compat_bool};
use registry::{
    COMPAT_ENV, EnvTarget, FLAGS, FlagSpec, NATIVE_ENV_PREFIX, Severity, Status, flag_by_long,
    native_env_name,
};

pub const CONFIG_ENV: &str = "SPOOLBOX_CONFIG";

/// Raw inputs. `cli` holds only flags given on the command line, by long name;
/// boolean flags have `None` as value.
#[derive(Debug, Default, Clone)]
pub struct Sources {
    pub cli: Vec<(String, Option<String>)>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct Loaded {
    pub settings: Settings,
    pub config_file: Option<PathBuf>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn load(sources: &Sources) -> Result<Loaded, ConfigError> {
    let env: HashMap<&str, &str> = sources
        .env
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let mut b = Builder::default();
    let mut diags = Vec::new();

    let config_file = sources
        .cli
        .iter()
        .rev()
        .find(|(k, _)| k == "config")
        .and_then(|(_, v)| v.clone())
        .or_else(|| env.get(CONFIG_ENV).map(|v| v.to_string()))
        .map(PathBuf::from);
    if let Some(path) = &config_file {
        let parsed = file::read(path)?;
        for (key, value, origin) in parsed.assignments {
            b.apply(key, &value, &origin)?;
        }
        b.settings.tags.rules = parsed.tag_rules;
        if let Some(rules) = parsed.chaos_rules {
            // Rules only make sense with chaos on, so configuring them enables it.
            b.settings.chaos.enabled |= !rules.is_empty();
            b.settings.chaos.rules = rules;
        }
    }

    for alias in COMPAT_ENV {
        let Some(&value) = env.get(alias.name) else {
            continue;
        };
        let origin = Origin::Env(alias.name.to_string());
        match alias.target {
            EnvTarget::Flag(long) => {
                let spec = flag_by_long(long).ok_or_else(|| missing(long))?;
                let value = if spec.takes_value {
                    value.to_string()
                } else if parse_compat_bool(value) {
                    "true".to_string()
                } else {
                    continue;
                };
                assign(&mut b, &mut diags, spec, &value, origin)?;
            }
            EnvTarget::Only(Status::Active(key)) => b.apply(key, value, &origin)?,
            EnvTarget::Only(status) => resolve_status(&mut diags, status, &origin, alias.name)?,
        }
    }

    for spec in FLAGS
        .iter()
        .filter(|f| matches!(f.status, Status::Active(_)))
    {
        let name = native_env_name(spec.long);
        if let Some(&value) = env.get(name.as_str()) {
            assign(&mut b, &mut diags, spec, value, Origin::Env(name))?;
        }
    }

    for (name, key) in native_target_env() {
        let Some(&value) = env.get(name.as_str()) else {
            continue;
        };
        let origin = Origin::Env(name);
        let value = match key {
            Key::Relay(f) | Key::Forward(f) if f.is_bool() => match key::parse_bool(value) {
                Some(on) => on.to_string(),
                None => {
                    return Err(ConfigError::InvalidValue {
                        origin,
                        value: value.to_string(),
                        reason: "expected true or false".into(),
                    });
                }
            },
            _ => value.to_string(),
        };
        b.apply(key, &value, &origin)?;
    }

    for (long, value) in &sources.cli {
        let spec = flag_by_long(long).ok_or_else(|| missing(long))?;
        assign(
            &mut b,
            &mut diags,
            spec,
            value.as_deref().unwrap_or("true"),
            Origin::Cli(long.clone()),
        )?;
    }

    warn_unknown_env(&env, &mut diags);
    finish(b, config_file, diags)
}

/// `SPOOLBOX_SMTP_RELAY_*` and `SPOOLBOX_SMTP_FORWARD_*`: the relay settings have no flags.
fn native_target_env() -> impl Iterator<Item = (String, Key)> {
    COMPAT_ENV
        .iter()
        .filter_map(|alias| match alias.target {
            EnvTarget::Only(Status::Active(key @ (Key::Relay(_) | Key::Forward(_)))) => {
                Some((alias.name.replacen("MP_", NATIVE_ENV_PREFIX, 1), key))
            }
            _ => None,
        })
        .chain(
            [
                ("RELAY", Key::Relay(RelayField::RateLimit)),
                ("FORWARD", Key::Forward(RelayField::RateLimit)),
            ]
            .map(|(what, key)| (format!("{NATIVE_ENV_PREFIX}SMTP_{what}_RATE_LIMIT"), key)),
        )
}

fn missing(long: &str) -> ConfigError {
    ConfigError::Conflict(format!("internal error: flag --{long} is not registered"))
}

fn assign(
    b: &mut Builder,
    diags: &mut Vec<Diagnostic>,
    spec: &FlagSpec,
    value: &str,
    origin: Origin,
) -> Result<(), ConfigError> {
    match spec.status {
        Status::Active(key) => b.apply(key, value, &origin),
        Status::Deprecated(target) => {
            diags.push(Diagnostic::warn(format!(
                "{origin} is deprecated; use --{target} instead"
            )));
            let target = flag_by_long(target).ok_or_else(|| missing(target))?;
            assign(b, diags, target, value, origin)
        }
        status => {
            // A boolean option explicitly set to false is the same as not using it.
            if !spec.takes_value && key::parse_bool(value) == Some(false) {
                return Ok(());
            }
            resolve_status(diags, status, &origin, spec.long)
        }
    }
}

fn resolve_status(
    diags: &mut Vec<Diagnostic>,
    status: Status,
    origin: &Origin,
    name: &str,
) -> Result<(), ConfigError> {
    match status {
        Status::Active(_) | Status::Deprecated(_) => Err(missing(name)),
        Status::NoOp(reason) => {
            diags.push(Diagnostic::info(format!(
                "{origin} has no effect: {reason}"
            )));
            Ok(())
        }
        Status::Planned {
            phase,
            on_use: Severity::Warn,
        } => {
            diags.push(Diagnostic::warn(format!(
                "{origin} is not implemented yet (planned for phase {phase}) and is ignored"
            )));
            Ok(())
        }
        Status::Planned {
            phase,
            on_use: Severity::Error,
        } => Err(ConfigError::NotYetSupported {
            origin: origin.clone(),
            phase,
        }),
        Status::Removed(reason) => Err(ConfigError::Removed {
            origin: origin.clone(),
            reason,
        }),
    }
}

fn warn_unknown_env(env: &HashMap<&str, &str>, diags: &mut Vec<Diagnostic>) {
    let native: Vec<String> = FLAGS
        .iter()
        .filter(|f| matches!(f.status, Status::Active(_)))
        .map(|f| native_env_name(f.long))
        .chain(native_target_env().map(|(name, _)| name))
        .chain([CONFIG_ENV.to_string()])
        .collect();
    let mut names: Vec<&&str> = env.keys().collect();
    names.sort();
    for name in names {
        let known = if name.starts_with("MP_") {
            COMPAT_ENV.iter().any(|a| a.name == *name)
        } else if name.starts_with(NATIVE_ENV_PREFIX) {
            native.iter().any(|n| n == name)
        } else {
            continue;
        };
        if known {
            continue;
        }
        let hint = if name.starts_with("MP_") {
            suggest::closest(name, COMPAT_ENV.iter().map(|a| a.name))
        } else {
            suggest::closest(name, native.iter().map(String::as_str))
        };
        diags.push(Diagnostic::warn(match hint {
            Some(h) => {
                format!("unknown environment variable {name} is ignored (did you mean {h}?)")
            }
            None => format!("unknown environment variable {name} is ignored"),
        }));
    }
}

fn credentials(
    src: AuthSources,
    what: &str,
    diags: &mut Vec<Diagnostic>,
) -> Result<Option<spoolbox_auth::Credentials>, ConfigError> {
    let invalid = |origin: &Origin, e: spoolbox_auth::AuthError| {
        ConfigError::Conflict(format!("{origin}: {e}"))
    };
    match (src.file, src.inline) {
        (Some((path, origin)), inline) => {
            if let Some((_, other)) = inline {
                diags.push(Diagnostic::warn(format!(
                    "{other} is ignored because {origin} is set ({what})"
                )));
            }
            spoolbox_auth::Credentials::from_file(std::path::Path::new(&path))
                .map(Some)
                .map_err(|e| invalid(&origin, e))
        }
        (None, Some((text, origin))) => spoolbox_auth::Credentials::parse(&text)
            .map(Some)
            .map_err(|e| invalid(&origin, e)),
        (None, None) => Ok(None),
    }
}

/// `host[:port]` entries separated by commas or spaces, `*` for any.
fn cors_origins(raw: &str, origin: &Origin, diags: &mut Vec<Diagnostic>) -> Vec<String> {
    let mut out = Vec::new();
    for entry in raw
        .split([',', ' '])
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        if entry == "*" {
            return vec!["*".into()];
        }
        let rest = entry
            .strip_prefix("http://")
            .or_else(|| entry.strip_prefix("https://"))
            .unwrap_or(entry);
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if host.is_empty() || host.contains('*') || host.starts_with(':') {
            diags.push(Diagnostic::warn(format!(
                "invalid CORS origin {entry:?} in {origin} is ignored"
            )));
            continue;
        }
        out.push(host.to_ascii_lowercase());
    }
    out.sort();
    out.dedup();
    out
}

/// `auto` (self-signed) or a certificate and key file pair.
fn tls_source(
    cert: Option<(String, Origin)>,
    key: Option<(String, Origin)>,
    key_flag: &str,
    cert_flag: &str,
) -> Result<Option<TlsSource>, ConfigError> {
    match (cert, key) {
        (Some((cert, _)), _) if cert.eq_ignore_ascii_case("auto") => Ok(Some(TlsSource::Auto)),
        (Some((cert, _)), Some((key, _))) => Ok(Some(TlsSource::Files {
            cert: cert.into(),
            key: key.into(),
        })),
        (Some((_, origin)), None) => Err(ConfigError::Conflict(format!(
            "{origin} requires {key_flag} (or use `auto` for a self-signed certificate)"
        ))),
        (None, Some((_, origin))) => Err(ConfigError::Conflict(format!(
            "{origin} requires {cert_flag}"
        ))),
        (None, None) => Ok(None),
    }
}

fn finish(
    b: Builder,
    config_file: Option<PathBuf>,
    mut diagnostics: Vec<Diagnostic>,
) -> Result<Loaded, ConfigError> {
    if let (Some(q), Some(v)) = (&b.quiet, &b.verbose) {
        return Err(ConfigError::Conflict(format!(
            "quiet ({q}) and verbose ({v}) logging cannot both be enabled"
        )));
    }
    let mut s = b.settings;
    if let Some((path, origin)) = &b.tags_config {
        let rules = tags::read_yaml(std::path::Path::new(path))
            .map_err(|e| ConfigError::Conflict(format!("{origin}: {e}")))?;
        s.tags.rules.extend(rules);
    }
    if let Some((spec, origin)) = &b.tag_spec {
        let rules =
            tags::parse_spec(spec).map_err(|e| ConfigError::Conflict(format!("{origin}: {e}")))?;
        s.tags.rules.extend(rules);
    }
    s.http.tls = tls_source(b.ui_tls_cert, b.ui_tls_key, "--ui-tls-key", "--ui-tls-cert")?;
    s.smtp.tls = match (b.tls_cert, b.tls_key) {
        (Some((cert, _)), _) if cert.eq_ignore_ascii_case("auto") => Some(TlsSource::Auto),
        (Some((cert, _)), Some((key, _))) => Some(TlsSource::Files {
            cert: cert.into(),
            key: key.into(),
        }),
        (Some((_, origin)), None) => {
            return Err(ConfigError::Conflict(format!(
                "{origin} requires --smtp-tls-key (or use --smtp-tls-cert auto)"
            )));
        }
        (None, Some((_, origin))) => {
            return Err(ConfigError::Conflict(format!(
                "{origin} requires --smtp-tls-cert"
            )));
        }
        (None, None) => None,
    };
    if s.smtp.tls.is_none() && (s.smtp.require_starttls || s.smtp.implicit_tls) {
        return Err(ConfigError::Conflict(
            "--smtp-require-starttls and --smtp-require-tls need a certificate: set --smtp-tls-cert/--smtp-tls-key or --smtp-tls-cert auto".into(),
        ));
    }
    if s.smtp.require_starttls && s.smtp.implicit_tls {
        return Err(ConfigError::Conflict(
            "--smtp-require-starttls and --smtp-require-tls cannot be used together".into(),
        ));
    }
    if s.smtp.auth_allow_insecure && (s.smtp.require_starttls || s.smtp.implicit_tls) {
        return Err(ConfigError::Conflict(
            "TLS cannot be required together with --smtp-auth-allow-insecure".into(),
        ));
    }

    s.http.auth = credentials(b.ui_auth, "web UI authentication", &mut diagnostics)?;
    s.http.send_auth = credentials(b.send_api_auth, "send API authentication", &mut diagnostics)?;
    s.smtp.auth = credentials(b.smtp_auth, "SMTP authentication", &mut diagnostics)?;
    if s.http.send_auth.is_some() && s.http.send_auth_accept_any {
        return Err(ConfigError::Conflict(
            "send API authentication cannot use both credentials and --send-api-auth-accept-any"
                .into(),
        ));
    }
    if s.smtp.auth.is_some() && s.smtp.auth_accept_any {
        return Err(ConfigError::Conflict(
            "SMTP authentication cannot use both credentials and --smtp-auth-accept-any".into(),
        ));
    }
    if s.smtp.tls.is_none()
        && (s.smtp.auth.is_some() || s.smtp.auth_accept_any)
        && !s.smtp.auth_allow_insecure
    {
        return Err(ConfigError::Conflict(
            "SMTP authentication requires STARTTLS or TLS: set --smtp-tls-cert (e.g. `auto`), or pass --smtp-auth-allow-insecure to allow PLAIN/LOGIN without encryption".into(),
        ));
    }
    // RFC 4954 section 4: no plaintext passwords before the session is protected.
    if s.smtp.auth.is_some() && !s.smtp.auth_allow_insecure && !s.smtp.implicit_tls {
        s.smtp.require_starttls = true;
    }
    s.relay = relay::resolve_relay(
        b.relay_config.as_deref(),
        b.relay_env,
        b.relay_all,
        b.relay_matching,
        &s.smtp.listen,
    )
    .map_err(|e| ConfigError::Conflict(format!("SMTP relay: {e}")))?;
    s.forward = relay::resolve_forward(b.forward_config.as_deref(), b.forward_env, &s.smtp.listen)
        .map_err(|e| ConfigError::Conflict(format!("SMTP forward: {e}")))?;
    if let Some((raw, origin)) = &b.cors {
        s.http.cors_origins = cors_origins(raw, origin, &mut diagnostics);
    }

    if !s.network.allow_unauthenticated_network {
        for (what, addr, protected) in [
            ("HTTP", &s.http.listen, s.http.auth.is_some()),
            ("SMTP", &s.smtp.listen, s.smtp.auth.is_some()),
        ] {
            if !addr.is_loopback() && !protected {
                return Err(ConfigError::UnsafeNetwork {
                    what,
                    addr: addr.to_string(),
                });
            }
        }
    }
    // Port 0 asks the OS for any free port, so two such listeners never collide.
    if s.http.listen.port != 0 && s.http.listen.to_socket_addr() == s.smtp.listen.to_socket_addr() {
        return Err(ConfigError::Conflict(format!(
            "HTTP and SMTP cannot both listen on {}",
            s.http.listen
        )));
    }
    Ok(Loaded {
        settings: s,
        config_file,
        diagnostics,
    })
}

#[cfg(test)]
mod tests;
