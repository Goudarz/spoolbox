//! Periodic housekeeping: age-based retention, event-log trimming and returning
//! free database pages to the file system.

use std::time::Duration;

use spoolbox_store::Store;
use tokio::sync::watch;

#[derive(Debug, Clone)]
pub struct MaintenanceOptions {
    pub interval: Duration,
    pub max_age: Option<Duration>,
    /// Events kept for resuming live streams.
    pub keep_events: i64,
    pub auto_vacuum: bool,
}

impl Default for MaintenanceOptions {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(60),
            max_age: None,
            keep_events: 10_000,
            auto_vacuum: true,
        }
    }
}

/// Free-page ratio above which space is returned to the file system.
const VACUUM_THRESHOLD: f64 = 0.2;
const VACUUM_PAGES: u32 = 4096;

pub async fn run(store: Store, opts: MaintenanceOptions, mut shutdown: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(opts.interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = shutdown.wait_for(|s| *s) => return,
        }
        let store = store.clone();
        let opts = opts.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || once(&store, &opts)).await {
            tracing::error!("maintenance task failed: {e}");
        }
    }
}

/// One maintenance pass (also used by tests).
pub fn once(store: &Store, opts: &MaintenanceOptions) {
    if let Some(age) = opts.max_age {
        let cutoff = now_micros() - age.as_micros() as i64;
        match store.prune_older_than(cutoff) {
            Ok(0) => {}
            Ok(n) => tracing::info!(deleted = n, "deleted messages older than the maximum age"),
            Err(e) => tracing::error!("age-based pruning failed: {e}"),
        }
    }
    if let Err(e) = store.trim_events(opts.keep_events) {
        tracing::warn!("cannot trim events: {e}");
    }
    if opts.auto_vacuum {
        match store.free_ratio() {
            Ok(r) if r > VACUUM_THRESHOLD => {
                if let Err(e) = store.incremental_vacuum(VACUUM_PAGES) {
                    tracing::warn!("incremental vacuum failed: {e}");
                }
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("cannot read free page count: {e}"),
        }
    }
}

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or_default()
}
