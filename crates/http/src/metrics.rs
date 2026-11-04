//! Prometheus metrics (`--enable-prometheus`), written by hand to avoid a dependency.
//! Only counts and sizes: no addresses, subjects or tag names.

use std::fmt::Write;
use std::sync::atomic::Ordering;

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::api::Api;

/// The text exposition format.
const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// A router that serves only `/metrics`, for a separate listener.
pub fn metrics_router(api: Api) -> Router {
    routes().with_state(api)
}

pub(crate) fn routes() -> Router<Api> {
    Router::new().route("/metrics", get(metrics))
}

fn metric(out: &mut String, name: &str, kind: &str, help: &str, value: u64) {
    let _ = writeln!(
        out,
        "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {value}"
    );
}

/// Label values as the format requires them.
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

async fn metrics(State(api): State<Api>) -> Response {
    let result = api
        .run(|s| Ok((s.count()?, s.database_size()?, s.job_counts()?)))
        .await;
    let ((messages, unread), database_size, jobs) = match result {
        Ok(v) => v,
        Err(r) => return r,
    };
    let stats = api.0.ingest.stats();
    let mut out = String::with_capacity(2048);
    let _ = writeln!(
        out,
        "# HELP spoolbox_build_info Version of this server.\n# TYPE spoolbox_build_info gauge\nspoolbox_build_info{{version=\"{}\"}} 1",
        escape(spoolbox_core::VERSION)
    );
    metric(
        &mut out,
        "spoolbox_uptime_seconds",
        "gauge",
        "Seconds since the server started.",
        api.0.started.elapsed().as_secs(),
    );
    metric(
        &mut out,
        "spoolbox_messages",
        "gauge",
        "Messages stored.",
        messages,
    );
    metric(
        &mut out,
        "spoolbox_messages_unread",
        "gauge",
        "Stored messages that are unread.",
        unread,
    );
    metric(
        &mut out,
        "spoolbox_database_size_bytes",
        "gauge",
        "Size of the database.",
        database_size,
    );
    metric(
        &mut out,
        "spoolbox_messages_deleted_total",
        "counter",
        "Messages deleted through the API since the server started.",
        api.0.deleted.load(Ordering::Relaxed),
    );
    for (name, help, value) in [
        (
            "spoolbox_smtp_accepted_total",
            "Messages accepted since the server started.",
            stats.accepted.load(Ordering::Relaxed),
        ),
        (
            "spoolbox_smtp_accepted_bytes_total",
            "Bytes of messages accepted since the server started.",
            stats.accepted_size.load(Ordering::Relaxed),
        ),
        (
            "spoolbox_smtp_rejected_total",
            "Messages rejected since the server started.",
            stats.rejected.load(Ordering::Relaxed),
        ),
        (
            "spoolbox_smtp_ignored_total",
            "Duplicate messages ignored since the server started.",
            stats.ignored.load(Ordering::Relaxed),
        ),
    ] {
        metric(&mut out, name, "counter", help, value);
    }
    out.push_str(
        "# HELP spoolbox_jobs Background jobs (indexing, webhook, relay, forward) by state.\n# TYPE spoolbox_jobs gauge\n",
    );
    for (kind, state, count) in jobs {
        let _ = writeln!(
            out,
            "spoolbox_jobs{{kind=\"{}\",state=\"{}\"}} {count}",
            escape(&kind),
            escape(&state)
        );
    }
    (StatusCode::OK, [(header::CONTENT_TYPE, CONTENT_TYPE)], out).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_values_are_escaped() {
        assert_eq!(escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }
}
