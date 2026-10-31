//! Shared state and helpers for the API handlers.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use spoolbox_app::StoreIngest;
use spoolbox_core::chaos::Chaos;
use spoolbox_store::Store;
use tokio::sync::broadcast;

/// Facts about this instance shown by `/api/v1/info` and `/api/v1/webui`.
#[derive(Debug, Clone, Default)]
pub struct InstanceInfo {
    pub label: String,
    /// Bytes; 0 = unlimited. Also the body limit of the send API.
    pub max_message_size: u64,
    pub duplicates_ignored: bool,
    pub hide_delete_all_button: bool,
    /// Message previews load no remote stylesheets or fonts.
    pub block_remote_css_and_fonts: bool,
}

/// Everything the API handlers need. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Api(pub(crate) Arc<Inner>);

#[derive(Debug)]
pub(crate) struct Inner {
    pub store: Store,
    pub ingest: Arc<StoreIngest>,
    pub chaos: Option<Arc<Chaos>>,
    pub info: InstanceInfo,
    pub started: Instant,
    /// Messages deleted through the API (`RuntimeStats.MessagesDeleted`).
    pub deleted: AtomicU64,
    /// Recent SMTP sessions (`--smtp-transcripts`).
    pub transcripts: Option<Arc<spoolbox_smtp::Transcripts>>,
    /// v1 WebSocket notifications (JSON text frames).
    pub ws: broadcast::Sender<Arc<str>>,
}

impl Api {
    pub fn new(
        store: Store,
        ingest: Arc<StoreIngest>,
        chaos: Option<Arc<Chaos>>,
        info: InstanceInfo,
    ) -> Self {
        let (ws, _) = broadcast::channel(1024);
        Self(Arc::new(Inner {
            store,
            ingest,
            chaos,
            info,
            started: Instant::now(),
            deleted: AtomicU64::new(0),
            transcripts: None,
            ws,
        }))
    }

    /// Enables `/api/v2/transcripts`. Call right after [`Api::new`].
    pub fn with_transcripts(mut self, t: Option<Arc<spoolbox_smtp::Transcripts>>) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.0) {
            inner.transcripts = t;
        }
        self
    }

    pub fn store(&self) -> &Store {
        &self.0.store
    }

    /// Runs blocking store work on the blocking pool.
    pub(crate) async fn run<T, F>(&self, f: F) -> Result<T, Response>
    where
        T: Send + 'static,
        F: FnOnce(&Store) -> Result<T, spoolbox_store::StoreError> + Send + 'static,
    {
        let store = self.0.store.clone();
        match tokio::task::spawn_blocking(move || f(&store)).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => {
                tracing::error!("store error: {e}");
                Err(text(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))
            }
            Err(e) => {
                tracing::error!("store task failed: {e}");
                Err(text(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))
            }
        }
    }
}

pub(crate) type Params = HashMap<String, String>;

pub(crate) fn text(status: StatusCode, body: impl Into<String>) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        Body::from(body.into()),
    )
        .into_response()
}

/// v1 errors: 400, `text/plain`.
pub(crate) fn bad_request(msg: impl Into<String>) -> Response {
    text(StatusCode::BAD_REQUEST, msg)
}

pub(crate) fn not_found() -> Response {
    text(StatusCode::NOT_FOUND, "404 page not found")
}

pub(crate) fn ok_text() -> Response {
    text(StatusCode::OK, "ok")
}

pub(crate) fn json<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(mut body) => {
            // Go's json.Encoder ends with a newline; clients compare bodies byte for byte rarely,
            // but some test suites do.
            body.push(b'\n');
            (
                status,
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                )],
                Body::from(body),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("cannot serialize response: {e}");
            text(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// RFC 3339 with milliseconds in UTC (e.g. `2026-10-24T22:43:20.123Z`), from Unix microseconds.
pub(crate) fn rfc3339(micros: i64) -> String {
    jiff::Timestamp::from_microsecond(micros)
        .map(|t| format!("{:.3}", t))
        .unwrap_or_default()
}

/// `Content-Disposition` value with an ASCII fallback and an RFC 8187 `filename*`.
pub(crate) fn disposition(kind: &str, filename: &str) -> String {
    let ascii: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if ascii == filename {
        return format!("{kind}; filename=\"{filename}\"");
    }
    let mut encoded = String::new();
    for b in filename.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("{kind}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// Media types a browser would render as active content; served as downloads instead.
pub(crate) fn is_active_content(content_type: &str) -> bool {
    let media = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    matches!(
        media.as_str(),
        "text/html"
            | "text/xml"
            | "application/xhtml+xml"
            | "image/svg+xml"
            | "application/xml"
            | "text/xsl"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition() {
        assert_eq!(disposition("inline", "a.png"), "inline; filename=\"a.png\"");
        assert_eq!(
            disposition("attachment", "گزارش.txt"),
            "attachment; filename=\"_____.txt\"; filename*=UTF-8''%DA%AF%D8%B2%D8%A7%D8%B1%D8%B4.txt"
        );
        assert_eq!(
            disposition("inline", "a\"b.txt"),
            "inline; filename=\"a_b.txt\"; filename*=UTF-8''a%22b.txt"
        );
    }

    #[test]
    fn timestamps() {
        assert_eq!(rfc3339(1_700_000_000_123_456), "2023-11-14T22:13:20.123Z");
    }
}
