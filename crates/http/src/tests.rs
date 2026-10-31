use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::*;

pub(crate) fn test_api(settings: &spoolbox_config::Settings) -> (spoolbox_app::Core, Api) {
    let core = spoolbox_app::Core::open(settings).expect("core");
    let api = Api::new(
        core.store.clone(),
        core.ingest.clone(),
        core.chaos.clone(),
        InstanceInfo::default(),
    );
    (core, api)
}

fn app(webroot: &str, ready: bool) -> Router {
    let r = Readiness::default();
    r.set(ready);
    let (_core, api) = test_api(&spoolbox_config::Settings::default());
    router(&HttpOptions::new(webroot), r, api)
}

async fn get(app: Router, path: &str, host: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let req = Request::get(path)
        .header(header::HOST, host)
        .body(Body::empty())
        .expect("request");
    let res = app.oneshot(req).await.expect("response");
    let (parts, body) = res.into_parts();
    let bytes = body.collect().await.expect("body").to_bytes();
    (
        parts.status,
        parts.headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

#[tokio::test]
async fn health_endpoints() {
    let (s, _, body) = get(app("/", true), "/livez", "localhost").await;
    assert_eq!((s, body.as_str()), (StatusCode::OK, ""));
    let (s, _, _) = get(app("/", true), "/readyz", "localhost").await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = get(app("/", false), "/readyz", "localhost").await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn health_endpoints_ignore_host_policy() {
    let (s, _, _) = get(app("/", true), "/livez", "pod.cluster.example.com").await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn disallowed_host_is_blocked() {
    let (s, h, body) = get(app("/", true), "/api/v1/messages", "evil.example.com").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(body, "Blocked due to disallowed Host header");
    assert_eq!(
        h.get(header::X_CONTENT_TYPE_OPTIONS).map(|v| v.as_bytes()),
        Some(&b"nosniff"[..])
    );
}

#[tokio::test]
async fn unknown_paths_404_with_security_headers() {
    let (s, h, body) = get(app("/", true), "/nope", "localhost:8025").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(body, "404 page not found");
    for name in [
        "x-content-type-options",
        "content-security-policy",
        "referrer-policy",
        "x-frame-options",
    ] {
        assert!(h.contains_key(name), "missing {name}");
    }
    let csp = h
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(csp.contains("frame-ancestors 'self'"), "{csp}");
}

#[tokio::test]
async fn webroot_prefix_and_redirect() {
    let (s, _, _) = get(app("/mail/", true), "/mail/livez", "localhost").await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = get(app("/mail/", true), "/livez", "localhost").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, h, _) = get(app("/mail/", true), "/mail", "localhost").await;
    assert!(s.is_redirection(), "{s}");
    assert_eq!(
        h.get(header::LOCATION).map(|v| v.as_bytes()),
        Some(&b"/mail/"[..])
    );
}

#[tokio::test]
async fn serves_over_tcp_and_shuts_down() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(serve(listener, app("/", true), async move {
        let _ = rx.await;
    }));

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.expect("connect");
    s.write_all(b"GET /readyz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let mut buf = String::new();
    s.read_to_string(&mut buf).await.expect("read");
    assert!(buf.starts_with("HTTP/1.1 200"), "{buf}");

    tx.send(()).expect("signal");
    server.await.expect("join").expect("serve");
}
