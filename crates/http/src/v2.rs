//! API v2 preview (ADR-010, `api/openapi-v2.json`): snake_case JSON, RFC 9457 errors,
//! no side effects on GET.

use std::collections::{BTreeMap, VecDeque};
use std::convert::Infallible;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::Stream;
use serde::Serialize;
use serde_json::Value;
use spoolbox_mime::view;
use spoolbox_store::{MessageRow, Part};

use crate::api::{Api, Params, rfc3339};

pub(crate) fn routes() -> Router<Api> {
    Router::new()
        .route("/api/v2/messages", post(import))
        .route("/api/v2/wait", post(wait))
        .route("/api/v2/events", get(events))
        .route("/api/v2/events/cursor", get(cursor))
        .route("/api/v2/messages/{id}/envelope", get(envelope))
        .route("/api/v2/messages/{id}/mime", get(mime))
        .route("/api/v2/messages/{id}/links", get(links))
        .route("/api/v2/messages/{id}/codes", get(codes))
        .route("/api/v2/messages/{id}/transcript", get(message_transcript))
        .route("/api/v2/transcripts", get(transcripts))
        .route("/api/v2/transcripts/{id}", get(transcript))
}

#[derive(Serialize)]
struct Problem {
    #[serde(rename = "type")]
    kind: &'static str,
    title: &'static str,
    status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

pub(crate) fn problem(status: StatusCode, detail: impl Into<String>) -> Response {
    let body = Problem {
        kind: "about:blank",
        title: status.canonical_reason().unwrap_or("Error"),
        status: status.as_u16(),
        detail: Some(detail.into()),
    };
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    (
        status,
        [(header::CONTENT_TYPE, "application/problem+json")],
        bytes,
    )
        .into_response()
}

fn ok<T: Serialize>(v: &T) -> Response {
    crate::api::json(StatusCode::OK, v)
}

fn internal(r: Response) -> Response {
    if r.status() == StatusCode::INTERNAL_SERVER_ERROR {
        problem(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    } else {
        r
    }
}

fn not_found() -> Response {
    problem(StatusCode::NOT_FOUND, "message not found")
}

#[derive(Serialize)]
struct Address {
    name: String,
    address: String,
}

impl From<&spoolbox_mime::Address> for Address {
    fn from(a: &spoolbox_mime::Address) -> Self {
        Self {
            name: a.name.clone(),
            address: a.addr.clone(),
        }
    }
}

#[derive(Serialize)]
struct MessageSummary {
    id: String,
    seq: i64,
    received_at: String,
    subject: String,
    from: Address,
    to: Vec<Address>,
    size: u64,
    attachments: u32,
    tags: Vec<String>,
}

fn summary(m: MessageRow, to: Vec<Address>) -> MessageSummary {
    MessageSummary {
        received_at: rfc3339(m.received_at),
        from: Address::from(&m.from),
        id: m.id,
        seq: m.seq,
        subject: m.subject,
        to,
        size: m.size,
        attachments: m.attachment_count,
        tags: m.tags,
    }
}

#[derive(Serialize)]
struct Envelope {
    mail_from: String,
    mail_params: BTreeMap<String, String>,
    rcpt_to: Vec<String>,
    helo: String,
    remote_ip: String,
    tls: Option<String>,
    auth_user: Option<String>,
    source: String,
}

async fn envelope(State(api): State<Api>, Path(id): Path<String>) -> Response {
    let result = api
        .run(move |s| Ok(s.get(&id)?.zip(s.envelope(&id)?)))
        .await;
    match result {
        Ok(Some((m, e))) => ok(&Envelope {
            mail_from: e.mail_from,
            mail_params: e
                .mail_params
                .into_iter()
                .map(|(k, v)| (k, v.unwrap_or_default()))
                .collect(),
            rcpt_to: e.rcpt_to,
            helo: e.helo,
            remote_ip: e.remote_ip,
            tls: e.tls,
            auth_user: e.auth_user,
            source: m.source,
        }),
        Ok(None) => not_found(),
        Err(r) => internal(r),
    }
}

#[derive(Serialize)]
struct MimePart {
    part_id: String,
    content_type: String,
    charset: Option<String>,
    disposition: Option<String>,
    filename: Option<String>,
    content_id: Option<String>,
    content_language: Option<String>,
    transfer_encoding: Option<String>,
    size: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<MimePart>,
}

fn tree(parts: &[Part], parent: Option<&str>) -> Vec<MimePart> {
    parts
        .iter()
        .filter(|p| p.parent.as_deref() == parent)
        .map(|p| MimePart {
            part_id: p.part_id.clone(),
            content_type: p.content_type.clone(),
            charset: p.charset.clone(),
            disposition: p.disposition.clone(),
            filename: p.filename.clone(),
            content_id: p.content_id.clone(),
            content_language: p.content_language.clone(),
            transfer_encoding: p.transfer_encoding.clone(),
            size: p.decoded_size,
            children: tree(parts, Some(&p.part_id)),
        })
        .collect()
}

async fn mime(State(api): State<Api>, Path(id): Path<String>) -> Response {
    let result = api
        .run(move |s| Ok(s.get(&id)?.map(|m| (m, s.parts(&id)))))
        .await;
    match result {
        Ok(Some((m, Ok(parts)))) => {
            if let Some(root) = tree(&parts, None).into_iter().next() {
                return ok(&root);
            }
            let detail = if m.status == "received" {
                "the message has not been indexed yet; retry shortly"
            } else {
                "the message could not be parsed"
            };
            problem(StatusCode::CONFLICT, detail)
        }
        Ok(Some((_, Err(e)))) => {
            tracing::error!("cannot load parts: {e}");
            problem(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
        Ok(None) => not_found(),
        Err(r) => internal(r),
    }
}

async fn bodies(api: &Api, id: String) -> Result<(MessageRow, view::Bodies), Response> {
    let result = api
        .run(move |s| match s.get(&id)? {
            Some(m) => Ok(Some((m, view::bodies(&s.raw(&id)?)))),
            None => Ok(None),
        })
        .await;
    match result {
        Ok(Some(v)) => Ok(v),
        Ok(None) => Err(not_found()),
        Err(r) => Err(internal(r)),
    }
}

#[derive(Serialize)]
struct Link {
    url: String,
    text: Option<String>,
    source: &'static str,
    tracking_pixel: bool,
}

async fn links(State(api): State<Api>, Path(id): Path<String>) -> Response {
    let (_, b) = match bodies(&api, id).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let html = (!b.html.is_empty()).then_some(b.html.as_str());
    let text = (!b.text.is_empty()).then_some(b.text.as_str());
    let out: Vec<Link> = view::links(html, text)
        .into_iter()
        .map(|l| Link {
            url: l.url,
            text: l.text,
            source: l.source,
            tracking_pixel: l.tracking_pixel,
        })
        .collect();
    ok(&out)
}

#[derive(Serialize)]
struct Code {
    value: String,
    source: &'static str,
}

async fn codes(
    State(api): State<Api>,
    Path(id): Path<String>,
    Query(p): Query<Params>,
) -> Response {
    let (m, b) = match bodies(&api, id).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let html_text = (!b.html.is_empty()).then(|| mail_parser_text(&b.html));
    let text = (!b.text.is_empty()).then_some(b.text.as_str());
    match view::codes(
        &m.subject,
        text,
        html_text.as_deref(),
        p.get("pattern").map(String::as_str),
    ) {
        Ok(found) => ok(&found
            .into_iter()
            .map(|c| Code {
                value: c.value,
                source: c.source,
            })
            .collect::<Vec<_>>()),
        Err(e) => problem(StatusCode::BAD_REQUEST, e),
    }
}

fn mail_parser_text(html: &str) -> String {
    spoolbox_mime::html_to_text(html)
}

#[derive(Serialize)]
struct Imported {
    /// `None` when the message was dropped as a duplicate (`--ignore-duplicate-ids`).
    id: Option<String>,
}

/// `POST /api/v2/messages`: stores a raw RFC 5322 message (`spoolbox import`, test fixtures).
/// The envelope is taken from the headers: From as sender, To/Cc/Bcc as recipients.
async fn import(State(api): State<Api>, req: axum::extract::Request) -> Response {
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<crate::Peer>>()
        .map(|c| c.0.0.ip().to_string())
        .unwrap_or_default();
    let user = req
        .extensions()
        .get::<crate::access::AuthUser>()
        .map(|u| u.0.clone());
    let limit = match api.0.info.max_message_size {
        0 => usize::MAX,
        n => usize::try_from(n).unwrap_or(usize::MAX),
    };
    let Ok(raw) = axum::body::to_bytes(req.into_body(), limit).await else {
        return problem(
            StatusCode::PAYLOAD_TOO_LARGE,
            "message is larger than --max-message-size",
        );
    };
    if raw.iter().all(u8::is_ascii_whitespace) {
        return problem(
            StatusCode::BAD_REQUEST,
            "the request body must be a raw message (message/rfc822)",
        );
    }
    let h = spoolbox_mime::scan_headers(&raw, &spoolbox_mime::Limits::default());
    let envelope = spoolbox_core::Envelope {
        mail_from: h.from.map(|a| a.addr).unwrap_or_default(),
        rcpt_to: h
            .to
            .iter()
            .chain(&h.cc)
            .chain(&h.bcc)
            .map(|a| a.addr.clone())
            .collect(),
        remote_ip: peer,
        auth_user: user,
        ..Default::default()
    };
    let ingest = api.0.ingest.clone();
    match tokio::task::spawn_blocking(move || ingest.store_raw(envelope, &raw, "import")).await {
        Ok(Ok(id)) if id.is_empty() => crate::api::json(StatusCode::OK, &Imported { id: None }),
        Ok(Ok(id)) => crate::api::json(StatusCode::CREATED, &Imported { id: Some(id) }),
        Ok(Err(reply)) if reply.starts_with("452") => {
            problem(StatusCode::INSUFFICIENT_STORAGE, reply)
        }
        Ok(Err(reply)) if reply.starts_with("451") => {
            problem(StatusCode::SERVICE_UNAVAILABLE, reply)
        }
        Ok(Err(reply)) => problem(StatusCode::INTERNAL_SERVER_ERROR, reply),
        Err(_) => problem(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    }
}

#[derive(Serialize)]
struct TranscriptSummary {
    id: u64,
    started_at: String,
    peer: String,
    lines: usize,
    message_ids: Vec<String>,
    truncated: bool,
}

#[derive(Serialize)]
struct TranscriptLineJson {
    at_ms: u64,
    from: String,
    text: String,
}

#[derive(Serialize)]
struct TranscriptJson {
    id: u64,
    started_at: String,
    peer: String,
    message_ids: Vec<String>,
    truncated: bool,
    lines: Vec<TranscriptLineJson>,
}

fn transcript_json(t: &spoolbox_smtp::Transcript) -> TranscriptJson {
    TranscriptJson {
        id: t.id,
        started_at: rfc3339(t.started_at.saturating_mul(1000)),
        peer: t.peer.clone(),
        message_ids: t.message_ids.clone(),
        truncated: t.truncated,
        lines: t
            .lines
            .iter()
            .map(|l| TranscriptLineJson {
                at_ms: l.at_ms,
                from: l.from.to_string(),
                text: l.text.clone(),
            })
            .collect(),
    }
}

fn transcripts_off() -> Response {
    problem(
        StatusCode::NOT_FOUND,
        "SMTP transcripts are off; start the server with --smtp-transcripts",
    )
}

async fn transcripts(State(api): State<Api>) -> Response {
    let Some(store) = &api.0.transcripts else {
        return transcripts_off();
    };
    let list: Vec<TranscriptSummary> = store
        .list()
        .iter()
        .map(|t| TranscriptSummary {
            id: t.id,
            started_at: rfc3339(t.started_at.saturating_mul(1000)),
            peer: t.peer.clone(),
            lines: t.lines.len(),
            message_ids: t.message_ids.clone(),
            truncated: t.truncated,
        })
        .collect();
    ok(&list)
}

async fn transcript(State(api): State<Api>, Path(id): Path<u64>) -> Response {
    let Some(store) = &api.0.transcripts else {
        return transcripts_off();
    };
    match store.get(id) {
        Some(t) => ok(&transcript_json(&t)),
        None => problem(
            StatusCode::NOT_FOUND,
            "no such transcript (only recent sessions are kept)",
        ),
    }
}

async fn message_transcript(State(api): State<Api>, Path(id): Path<String>) -> Response {
    let Some(store) = &api.0.transcripts else {
        return transcripts_off();
    };
    match store.for_message(&id) {
        Some(t) => ok(&transcript_json(&t)),
        None => problem(
            StatusCode::NOT_FOUND,
            "no transcript for this message (not received over SMTP, or no longer kept)",
        ),
    }
}

const WAIT_DEFAULT_MS: u64 = 10_000;
const WAIT_MAX_MS: u64 = 300_000;

/// Long-polls until a message matching `query` is received after `since_seq` (an event sequence
/// number; default: now), checking new arrivals and re-checking them once they are indexed.
async fn wait(State(api): State<Api>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return problem(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")),
    };
    let Some(query) = v.get("query").and_then(Value::as_str).map(str::trim) else {
        return problem(StatusCode::BAD_REQUEST, "`query` is required");
    };
    let timeout = match v.get("timeout_ms") {
        None | Some(Value::Null) => WAIT_DEFAULT_MS,
        Some(t) => match t.as_u64() {
            Some(ms) if ms <= WAIT_MAX_MS => ms,
            _ => {
                return problem(
                    StatusCode::BAD_REQUEST,
                    "`timeout_ms` must be between 0 and 300000",
                );
            }
        },
    };
    let q = spoolbox_search::parse(query, v.get("tz").and_then(Value::as_str));
    let store = api.store().clone();
    let mut notify = store.subscribe();
    let mut cursor = match v.get("since_seq") {
        None | Some(Value::Null) => store.last_event_seq(),
        Some(s) => match s.as_i64() {
            Some(n) if n >= 0 => n,
            _ => {
                return problem(
                    StatusCode::BAD_REQUEST,
                    "`since_seq` must be a non-negative integer",
                );
            }
        },
    };
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout);
    loop {
        let (after, q2) = (cursor, q.clone());
        let found = api
            .run(move |s| {
                let mut last = after;
                let mut ids: Vec<String> = Vec::new();
                loop {
                    let batch = s.events_since(last, 1000)?;
                    let Some(e) = batch.last() else { break };
                    last = e.seq;
                    for e in &batch {
                        if matches!(e.kind.as_str(), "message.received" | "message.updated")
                            && let Some(id) = &e.message_id
                            && !ids.contains(id)
                        {
                            ids.push(id.clone());
                        }
                    }
                    if batch.len() < 1000 {
                        break;
                    }
                }
                let matched = s.filter_ids(&q2, &ids)?;
                let hit = match matched.first() {
                    Some(id) => s.get(id)?.map(|m| {
                        let to = s
                            .recipients(id)
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|r| r.kind == "to")
                            .map(|r| Address::from(&r.address))
                            .collect();
                        (m, to)
                    }),
                    None => None,
                };
                Ok((last, hit))
            })
            .await;
        match found {
            Ok((_, Some((m, to)))) => return ok(&summary(m, to)),
            // Messages seen but not matching are checked again when they are indexed,
            // so the cursor only moves past events that cannot change the answer.
            Ok((_last, None)) => {}
            Err(r) => return internal(r),
        }
        tokio::select! {
            changed = notify.changed() => if changed.is_err() {
                return problem(StatusCode::SERVICE_UNAVAILABLE, "shutting down");
            },
            () = tokio::time::sleep_until(deadline) => {
                return problem(StatusCode::REQUEST_TIMEOUT, "no matching message before the timeout");
            }
        }
        // Keep the cursor: an earlier message may match once indexed. Bound the rescans by
        // only moving forward when the window grows large.
        if store.last_event_seq() - cursor > 10_000 {
            cursor = store.last_event_seq() - 10_000;
        }
    }
}

#[derive(Serialize)]
struct Cursor {
    seq: i64,
}

/// The current end of the event log. Take it before triggering an email, then pass it as
/// `since_seq` to `/api/v2/wait`, so a message that arrives early is not missed.
async fn cursor(State(api): State<Api>) -> Response {
    ok(&Cursor {
        seq: api.store().last_event_seq(),
    })
}

#[derive(Serialize)]
struct EventJson {
    seq: i64,
    #[serde(rename = "type")]
    kind: String,
    message_id: Option<String>,
    data: Value,
}

struct Feed {
    api: Api,
    notify: tokio::sync::watch::Receiver<i64>,
    cursor: i64,
    buffer: VecDeque<EventJson>,
}

fn sse_event(e: &EventJson) -> SseEvent {
    SseEvent::default()
        .id(e.seq.to_string())
        .data(serde_json::to_string(e).unwrap_or_default())
}

/// `GET /api/v2/events?since=N` (or `Last-Event-ID`): ordered events without gaps. If the
/// position is no longer retained, the stream starts with a `resync` event.
async fn events(State(api): State<Api>, Query(p): Query<Params>, headers: HeaderMap) -> Response {
    let since = p.get("since").cloned().or_else(|| {
        headers
            .get("last-event-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    });
    let store = api.store().clone();
    let last = store.last_event_seq();
    let mut buffer = VecDeque::new();
    let cursor = match since {
        None => last,
        Some(s) => match s.trim().parse::<i64>() {
            Ok(n) if n >= 0 => {
                let oldest = api.run(|s| s.oldest_event_seq()).await.unwrap_or(0);
                if n + 1 < oldest {
                    buffer.push_back(EventJson {
                        seq: last,
                        kind: "resync".into(),
                        message_id: None,
                        data: Value::Null,
                    });
                    last
                } else {
                    n
                }
            }
            _ => {
                return problem(
                    StatusCode::BAD_REQUEST,
                    "`since` must be a non-negative integer",
                );
            }
        },
    };
    let feed = Feed {
        notify: store.subscribe(),
        api,
        cursor,
        buffer,
    };
    let mut res = Sse::new(stream(feed))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res
}

fn stream(feed: Feed) -> impl Stream<Item = Result<SseEvent, Infallible>> {
    futures_util::stream::unfold(feed, |mut f| async move {
        loop {
            if let Some(e) = f.buffer.pop_front() {
                let ev = sse_event(&e);
                return Some((Ok(ev), f));
            }
            let after = f.cursor;
            if let Ok(batch) = f.api.run(move |s| s.events_since(after, 500)).await
                && !batch.is_empty()
            {
                for e in batch {
                    f.cursor = e.seq;
                    f.buffer.push_back(EventJson {
                        seq: e.seq,
                        kind: e.kind,
                        message_id: e.message_id,
                        data: serde_json::from_str(&e.data).unwrap_or(Value::Null),
                    });
                }
                continue;
            }
            if f.notify.changed().await.is_err() {
                return None;
            }
        }
    })
}
