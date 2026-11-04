//! Single-message endpoints and the `/view/{id}.html|.txt` pages.

use std::sync::LazyLock;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use regex::Regex;
use spoolbox_mime::view;
use spoolbox_store::{Store, StoreError};

use super::types::{Attachment, ListUnsubscribe, Message, Recipients, from_row};
use super::{resolve, thumb};
use crate::api::{
    Api, Params, bad_request, disposition, is_active_content, json, not_found, ok_text, rfc3339,
    text,
};

pub(super) fn routes() -> Router<Api> {
    Router::new()
        .route("/api/v1/message/{id}", get(message))
        .route("/api/v1/message/{id}/headers", get(headers))
        .route("/api/v1/message/{id}/raw", get(raw))
        .route("/api/v1/message/{id}/part/{part}", get(part))
        .route("/api/v1/message/{id}/part/{part}/thumb", get(thumbnail))
        .route("/api/v1/message/{id}/release", post(release))
        .route("/api/v1/message/{id}/html-check", get(not_implemented))
        .route("/api/v1/message/{id}/link-check", get(not_implemented))
}

/// Loads a message as the v1 API presents it, and marks it read.
fn load(s: &Store, id: &str, webroot: Option<&str>) -> Result<Option<Message>, StoreError> {
    let Some(m) = s.get(id)? else {
        return Ok(None);
    };
    let raw = s.raw(id)?;
    let rcpts = Recipients::from(&s.recipients(id)?);
    let env = s.envelope(id)?;
    let parts = s.parts(id)?;
    let headers = view::headers_v1(&raw);
    let first = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default()
    };
    let bodies = view::bodies(&raw);
    let from = from_row(&m);
    let return_path = env
        .as_ref()
        .map(|e| e.mail_from.clone())
        .filter(|p| !p.is_empty())
        .or_else(|| {
            let h = first("Return-Path");
            let h = h.trim().trim_start_matches('<').trim_end_matches('>');
            (!h.is_empty()).then(|| h.to_string())
        })
        .or_else(|| from.as_ref().map(|f| f.address.clone()))
        .unwrap_or_default();
    let lu_header = first("List-Unsubscribe");
    let list_unsubscribe = if lu_header.is_empty() {
        ListUnsubscribe {
            header: String::new(),
            links: Vec::new(),
            errors: String::new(),
            header_post: String::new(),
        }
    } else {
        let lu = view::list_unsubscribe(&lu_header, &first("List-Unsubscribe-Post"));
        ListUnsubscribe {
            header: lu.header,
            links: lu.links,
            errors: lu.errors,
            header_post: lu.header_post,
        }
    };
    let mut html = bodies.html;
    if let Some(root) = webroot {
        html = view::prepare_html(&html, &cid_urls(root, id, &parts));
    }
    s.set_read(&[id.to_string()], true)?;
    Ok(Some(Message {
        id: m.id.clone(),
        message_id: m.message_id.clone().unwrap_or_default(),
        from,
        to: rcpts.to,
        cc: rcpts.cc,
        bcc: rcpts.bcc,
        reply_to: rcpts.reply_to,
        return_path,
        subject: m.subject.clone(),
        list_unsubscribe,
        date: rfc3339(
            m.header_date
                .map(|secs| secs.saturating_mul(1_000_000))
                .unwrap_or(m.received_at),
        ),
        tags: m.tags.clone(),
        username: env.and_then(|e| e.auth_user).unwrap_or_default(),
        text: bodies.text,
        html,
        size: m.size,
        inline: parts
            .iter()
            .filter(|p| p.is_inline)
            .map(Attachment::from)
            .collect(),
        attachments: {
            // Shallower parts first: `2` before `1.2.2`.
            let mut a: Vec<&spoolbox_store::Part> =
                parts.iter().filter(|p| p.is_attachment).collect();
            a.sort_by_key(|p| p.part_id.matches('.').count());
            a.into_iter().map(Attachment::from).collect()
        },
    }))
}

fn cid_urls(webroot: &str, id: &str, parts: &[spoolbox_store::Part]) -> Vec<(String, String)> {
    parts
        .iter()
        .filter_map(|p| {
            let cid = p.content_id.clone()?;
            Some((
                cid,
                format!("{webroot}api/v1/message/{id}/part/{}", p.part_id),
            ))
        })
        .collect()
}

async fn message(
    State(api): State<Api>,
    Path(id): Path<String>,
    Query(p): Query<Params>,
) -> Response {
    let id = match resolve(&api, id, &p).await {
        Ok(id) => id,
        Err(r) => return r,
    };
    match api.run(move |s| load(s, &id, None)).await {
        Ok(Some(m)) => json(StatusCode::OK, &m),
        Ok(None) => not_found(),
        Err(r) => r,
    }
}

async fn raw_of(api: &Api, id: String) -> Result<Vec<u8>, Response> {
    let store = api.store().clone();
    match tokio::task::spawn_blocking(move || store.raw(&id)).await {
        Ok(Ok(raw)) => Ok(raw),
        Ok(Err(StoreError::NotFound)) => Err(not_found()),
        Ok(Err(e)) => {
            tracing::error!("cannot read message: {e}");
            Err(text(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))
        }
        Err(_) => Err(text(StatusCode::INTERNAL_SERVER_ERROR, "internal error")),
    }
}

async fn headers(
    State(api): State<Api>,
    Path(id): Path<String>,
    Query(p): Query<Params>,
) -> Response {
    let id = match resolve(&api, id, &p).await {
        Ok(id) => id,
        Err(r) => return r,
    };
    match raw_of(&api, id).await {
        Ok(raw) => json(StatusCode::OK, &view::headers_v1(&raw)),
        Err(r) => r,
    }
}

/// The message exactly as received (byte-exact, ADR-006).
async fn raw(State(api): State<Api>, Path(id): Path<String>, Query(p): Query<Params>) -> Response {
    let id = match resolve(&api, id, &p).await {
        Ok(id) => id,
        Err(r) => return r,
    };
    match raw_of(&api, id.clone()).await {
        Ok(raw) => {
            let mut res = (
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                Body::from(raw),
            )
                .into_response();
            if p.get("dl").map(String::as_str) == Some("1")
                && let Ok(v) =
                    HeaderValue::from_str(&disposition("attachment", &format!("{id}.eml")))
            {
                res.headers_mut().insert(header::CONTENT_DISPOSITION, v);
            }
            res
        }
        Err(r) => r,
    }
}

async fn part(
    State(api): State<Api>,
    Path((id, part)): Path<(String, String)>,
    Query(p): Query<Params>,
) -> Response {
    let id = match resolve(&api, id, &p).await {
        Ok(id) => id,
        Err(r) => return r,
    };
    let raw = match raw_of(&api, id).await {
        Ok(raw) => raw,
        Err(r) => return r,
    };
    let Some(c) = view::part_content(&raw, &part) else {
        return not_found();
    };
    let name = c.filename.or(c.content_id).unwrap_or_default();
    let (ctype, kind) = if is_active_content(&c.content_type) {
        ("application/octet-stream".to_string(), "attachment")
    } else {
        (c.content_type, "inline")
    };
    let mut res = Body::from(c.data).into_response();
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&ctype) {
        h.insert(header::CONTENT_TYPE, v);
    }
    if let Ok(v) = HeaderValue::from_str(&disposition(kind, &name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    res
}

async fn thumbnail(State(api): State<Api>, Path((id, part)): Path<(String, String)>) -> Response {
    let raw = match raw_of(&api, id).await {
        Ok(raw) => raw,
        Err(_) => return bad_request("message not found"),
    };
    let Some(c) = view::part_content(&raw, &part) else {
        return bad_request("part not found");
    };
    let name = c
        .filename
        .clone()
        .or(c.content_id.clone())
        .unwrap_or_default();
    let is_image = c.content_type.starts_with("image/");
    let jpeg = tokio::task::spawn_blocking(move || {
        if is_image {
            thumb::generate(&c.data)
        } else {
            thumb::blank()
        }
    })
    .await
    .unwrap_or_else(|_| thumb::blank());
    let mut res = ([(header::CONTENT_TYPE, "image/jpeg")], Body::from(jpeg)).into_response();
    let stem = std::path::Path::new(&name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let jpg = if stem.is_empty() {
        "thumb.jpg".to_string()
    } else {
        format!("{stem}.jpg")
    };
    let value = disposition("inline", &jpg);
    if let Ok(v) = HeaderValue::from_str(value.trim_start_matches("inline; ")) {
        res.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    res
}

#[derive(serde::Deserialize)]
struct ReleaseBody {
    #[serde(rename = "To", default)]
    to: Vec<String>,
}

/// Queues the message for the configured relay server and returns at once; delivery is retried
/// in the background. A JSON content type is required so that a plain cross-site form, which
/// a browser sends without asking, cannot make this server mail a message to an attacker.
async fn release(
    State(api): State<Api>,
    Path(id): Path<String>,
    Query(p): Query<Params>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(relay) = api.0.relay.clone() else {
        return bad_request("message relay is not configured");
    };
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
    if !is_json {
        return bad_request("expected application/json");
    }
    let to = match serde_json::from_slice::<ReleaseBody>(&body) {
        Ok(b) => b.to,
        Err(e) => return bad_request(e.to_string()),
    };
    if let Err(e) = relay.check_release(&to) {
        return bad_request(e);
    }
    let id = match resolve(&api, id, &p).await {
        Ok(id) => id,
        Err(r) => return r,
    };
    let queued = api
        .run(move |s| {
            let Some(message) = s.get(&id)? else {
                return Ok(false);
            };
            relay.release(s, message.seq, to).map(|()| true)
        })
        .await;
    match queued {
        Ok(true) => ok_text(),
        Ok(false) => not_found(),
        Err(r) => r,
    }
}

/// HTML and link checks are planned after the MVP (roadmap: P2 checks).
async fn not_implemented() -> Response {
    text(StatusCode::NOT_IMPLEMENTED, "not implemented yet")
}

static ANCHOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<a\b").expect("regex"));

fn nonce() -> String {
    let mut b = [0u8; 16];
    if getrandom::fill(&mut b).is_err() {
        return "spoolbox".into();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `GET /view/{id}.html`: the HTML body with `cid:` links rewritten. Served with a sandboxing
/// CSP; with `embed=1` links open in a new tab and a nonce'd script reports the height.
pub(crate) async fn view_html(api: Api, webroot: String, id: String, p: Params) -> Response {
    let id = match resolve(&api, id, &p).await {
        Ok(id) => id,
        Err(r) => return r,
    };
    let root = webroot.clone();
    let msg = match api.run(move |s| load(s, &id, Some(&root))).await {
        Ok(Some(m)) => m,
        Ok(None) => return text(StatusCode::NOT_FOUND, "Message not found"),
        Err(r) => return r,
    };
    if msg.html.is_empty() {
        return text(
            StatusCode::NOT_FOUND,
            "This message does not contain a HTML part",
        );
    }
    let mut html = msg.html;
    let embed = p.get("embed").map(String::as_str) == Some("1");
    let nonce = nonce();
    if embed {
        html = ANCHOR
            .replace_all(&html, r#"<a target="_blank" rel="noreferrer noopener""#)
            .into_owned();
        let script = format!(
            r#"<script nonce="{nonce}">if (typeof window.parent == "object") {{ window.addEventListener('load', function () {{ window.parent.postMessage({{ messageHeight: document.body.scrollHeight }}, "*") }}) }}</script>"#
        );
        html = if html.contains("</body>") {
            html.replacen("</body>", &format!("{script}</body>"), 1)
        } else {
            format!("{html}{script}")
        };
    }
    let scripts = if embed {
        format!("script-src 'nonce-{nonce}'; ")
    } else {
        String::new()
    };
    let sandbox = if embed {
        "sandbox allow-scripts allow-popups allow-popups-to-escape-sandbox"
    } else {
        "sandbox allow-popups allow-popups-to-escape-sandbox"
    };
    // `remote=0` blocks remote images for this view (tracking pixels); inline parts still load.
    let images = if p.get("remote").map(String::as_str) == Some("0") {
        "'self' data: blob:"
    } else {
        "* data: blob:"
    };
    let (styles, fonts) = if api.0.info.block_remote_css_and_fonts {
        ("'self' 'unsafe-inline'", "'self' data:")
    } else {
        ("* 'unsafe-inline'", "* data:")
    };
    let csp = format!(
        "default-src 'none'; {scripts}style-src {styles}; img-src {images}; font-src {fonts}; media-src *; frame-ancestors 'self'; base-uri 'none'; form-action 'none'; {sandbox}"
    );
    let mut res = (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        Body::from(html),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&csp) {
        res.headers_mut().insert(header::CONTENT_SECURITY_POLICY, v);
    }
    res
}

pub(crate) async fn view_text(api: Api, id: String, p: Params) -> Response {
    let id = match resolve(&api, id, &p).await {
        Ok(id) => id,
        Err(r) => return r,
    };
    match api.run(move |s| load(s, &id, None)).await {
        Ok(Some(m)) => text(StatusCode::OK, m.text),
        Ok(None) => text(StatusCode::NOT_FOUND, "Message not found"),
        Err(r) => r,
    }
}
