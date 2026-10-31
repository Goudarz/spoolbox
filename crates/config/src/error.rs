use std::fmt;
use std::path::PathBuf;

/// Where a configuration value came from; used in every error and warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    File { path: PathBuf, key: String },
    Env(String),
    Cli(String),
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::File { path, key } => write!(f, "config file {} (key `{key}`)", path.display()),
            Origin::Env(name) => write!(f, "environment variable {name}"),
            Origin::Cli(long) => write!(f, "flag --{long}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid value {value:?} for {origin}: {reason}")]
    InvalidValue {
        origin: Origin,
        value: String,
        reason: String,
    },

    #[error(
        "{origin} is not supported yet (planned for phase {phase}); refusing to start because ignoring it would weaken security or change behaviour"
    )]
    NotYetSupported { origin: Origin, phase: &'static str },

    #[error("{origin} is not supported: {reason}")]
    Removed {
        origin: Origin,
        reason: &'static str,
    },

    #[error("{0}")]
    Conflict(String),

    #[error(
        "refusing to listen on {addr} ({what}): it is reachable from other machines and authentication is not available. \
         Bind to 127.0.0.1 / ::1, or pass --allow-unauthenticated-network (SPOOLBOX_ALLOW_UNAUTHENTICATED_NETWORK=true) \
         if this host is on a trusted network (e.g. inside a container)"
    )]
    UnsafeNetwork { what: &'static str, addr: String },

    #[error("cannot read config file {path}: {source}")]
    FileRead {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("invalid config file {path}: {message}")]
    FileParse { path: PathBuf, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticLevel {
    Info,
    Warn,
}

/// Non-fatal findings to be logged once logging is initialised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub level: DiagnosticLevel,
    pub message: String,
}

impl Diagnostic {
    pub fn warn(message: impl Into<String>) -> Self {
        Self {
            level: DiagnosticLevel::Warn,
            message: message.into(),
        }
    }

    pub fn info(message: impl Into<String>) -> Self {
        Self {
            level: DiagnosticLevel::Info,
            message: message.into(),
        }
    }
}
