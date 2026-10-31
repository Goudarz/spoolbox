use std::fs::OpenOptions;
use std::io;
use std::sync::Mutex;

use spoolbox_config::{LogFormat, LogLevel, LogSettings};
use tracing::level_filters::LevelFilter;

pub fn init(cfg: &LogSettings) -> io::Result<()> {
    let level = match cfg.level {
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Trace => LevelFilter::TRACE,
    };
    let builder = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false);
    let json = cfg.format == LogFormat::Json;
    let result = match &cfg.file {
        Some(path) => {
            let file = Mutex::new(OpenOptions::new().create(true).append(true).open(path)?);
            let b = builder.with_ansi(false).with_writer(file);
            if json {
                b.json().try_init()
            } else {
                b.try_init()
            }
        }
        None => {
            let b = builder.with_writer(io::stderr);
            if json {
                b.with_ansi(false).json().try_init()
            } else {
                b.try_init()
            }
        }
    };
    result.map_err(|e| io::Error::other(e.to_string()))
}
