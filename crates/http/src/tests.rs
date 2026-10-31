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

#[tokio::test]
async fn serves_https_and_rejects_plain_http() {
    use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).expect("cert");
    let der: CertificateDer<'static> = cert.cert.der().clone();
    let key = PrivateKeyDer::try_from(cert.signing_key.serialize_der()).expect("key");
    let server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(vec![der.clone()], key)
        .expect("server config");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(serve_tls(
        listener,
        Arc::new(server),
        app("/", true),
        async move {
            let _ = rx.await;
        },
    ));

    // A silent client must not block the next one (handshakes run concurrently).
    let _idle = tokio::net::TcpStream::connect(addr).await.expect("idle");

    let mut roots = rustls::RootCertStore::empty();
    roots.add(der).expect("root");
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let mut tls = tokio_rustls::TlsConnector::from(Arc::new(client))
        .connect(ServerName::try_from("localhost").expect("name"), tcp)
        .await
        .expect("handshake");
    tls.write_all(b"GET /readyz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let mut buf = Vec::new();
    let _ = tls.read_to_end(&mut buf).await;
    assert!(String::from_utf8_lossy(&buf).starts_with("HTTP/1.1 200"));

    let mut plain = tokio::net::TcpStream::connect(addr).await.expect("plain");
    plain
        .write_all(b"GET /readyz HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .expect("write");
    let mut out = Vec::new();
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        plain.read_to_end(&mut out),
    )
    .await;
    assert!(
        !String::from_utf8_lossy(&out).contains("200"),
        "plain HTTP must not be served"
    );

    tx.send(()).expect("stop");
    task.await.expect("join").expect("serve");
}

#[tokio::test]
async fn ui_pages_get_a_base_href_and_a_strict_policy() {
    for (root, path) in [
        ("/", "/"),
        ("/", "/m/abc"),
        ("/", "/search"),
        ("/mail/", "/mail/"),
    ] {
        let (s, h, body) = get(app(root, true), path, "localhost").await;
        assert_eq!(s, StatusCode::OK, "{path}");
        assert!(
            body.contains(&format!("<base href=\"{root}\">")),
            "{path}: {body}"
        );
        let csp = h[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap_or_default();
        assert!(
            csp.contains("script-src 'self'") && !csp.contains("unsafe-eval"),
            "{csp}"
        );
        assert_eq!(h[header::CACHE_CONTROL], "no-cache");
    }
    let (s, _, _) = get(app("/", true), "/assets/missing.js", "localhost").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ui_pages_require_credentials_when_configured() {
    let r = Readiness::default();
    r.set(true);
    let (_core, api) = test_api(&spoolbox_config::Settings::default());
    let mut opts = HttpOptions::new("/mail/");
    opts.ui_auth = Some(std::sync::Arc::new(
        spoolbox_auth::Credentials::parse("u:p").expect("creds"),
    ));
    let app = router(&opts, r, api);
    for path in ["/mail/", "/mail/m/x", "/mail/api/v1/messages"] {
        let (s, _, _) = get(app.clone(), path, "localhost").await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{path}");
    }
    let (s, _, _) = get(app, "/mail/livez", "localhost").await;
    assert_eq!(s, StatusCode::OK);
}
