//! The web UI: static files embedded at build time (see `build.rs`) and the client-side
//! routes that all return `index.html`.

use axum::body::Body;
use axum::extract::{OriginalUri, Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};

pub(crate) struct Asset {
    pub path: &'static str,
    pub body: &'static [u8],
    pub mime: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));

/// Scripts and styles come from this server only; email HTML is shown in a sandboxed frame
/// that has its own, stricter policy (`/view/{id}.html`).
const UI_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self' ws: wss:; frame-src 'self'; object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'self'";

const NOT_BUILT: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Spoolbox</title></head>
<body style="font-family: system-ui, sans-serif; max-width: 40rem; margin: 3rem auto; padding: 0 1rem; line-height: 1.5">
<h1>Spoolbox is running</h1>
<p>This binary was built without the web UI. Build it with <code>npm --prefix web ci &amp;&amp; npm --prefix web run build</code>,
then rebuild Spoolbox. The API works already: <a href="api/v1/messages">api/v1/messages</a>.</p>
</body></html>"#;

fn find(path: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|a| a.path == path)
}

pub(crate) fn is_built() -> bool {
    find("index.html").is_some()
}

/// `index.html` with a `<base>` for the webroot, so relative asset and API URLs work under any prefix.
pub(crate) fn index(webroot: &str) -> Response {
    let html = match find("index.html") {
        Some(a) => String::from_utf8_lossy(a.body).into_owned(),
        None => NOT_BUILT.to_string(),
    };
    let base = format!("<head>\n<base href=\"{webroot}\">");
    let html = html.replacen("<head>", &base, 1);
    let mut res = (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        Body::from(html),
    )
        .into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(UI_CSP),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res
}

pub(crate) async fn page(State(webroot): State<String>, OriginalUri(uri): OriginalUri) -> Response {
    if webroot != "/" && uri.path() == webroot.trim_end_matches('/') {
        return Redirect::to(&webroot).into_response();
    }
    index(&webroot)
}

pub(crate) async fn page_with_id(
    State(webroot): State<String>,
    Path(_id): Path<String>,
) -> Response {
    index(&webroot)
}

/// Built files; names under `assets/` contain a content hash and are cached for a year.
pub(crate) async fn file(Path(path): Path<String>) -> Response {
    let Some(a) = find(&path).filter(|a| a.path != "index.html") else {
        return crate::api::not_found();
    };
    let cache = if a.path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, a.mime),
            (header::CACHE_CONTROL, cache),
        ],
        Body::from(a.body),
    )
        .into_response()
}
