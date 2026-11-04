//! End-to-end API behaviour through the router (no TCP), against a temporary store.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use base64::Engine;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use spoolbox_core::Envelope;
use tower::ServiceExt;

use super::*;
use crate::tests::test_api;

struct T {
    core: spoolbox_app::Core,
    app: Router,
}

const HTML_MSG: &str = "From: =?utf-8?b?2YHYsdiz2KrZhtiv2Yc=?= <fa@example.com>\r\nTo: bob+signup@example.com\r\nCc: carol@example.com\r\nReply-To: reply@example.com\r\nSubject: order 482913\r\nMessage-ID: <html-1@fixtures.test>\r\nX-Custom-ID: abc123\r\nList-Unsubscribe: <mailto:unsub@example.com>, <https://example.invalid/unsub>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=B\r\n\r\n--B\r\nContent-Type: multipart/related; boundary=R\r\n\r\n--R\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><head><base href=\"https://evil.test/\"></head><body><p>hi</p><img src=\"cid:logo1\"><a href=\"https://example.invalid/verify?code=482913\">verify</a></body></html>\r\n--R\r\nContent-Type: image/png\r\nContent-ID: <logo1>\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==\r\n--R--\r\n--B\r\nContent-Type: text/html\r\nContent-Disposition: attachment; filename=\"evil.html\"\r\n\r\n<script>alert(1)</script>\r\n--B--\r\n";
const PLAIN_MSG: &str =
    "From: plain@example.com\r\nTo: erin@example.com\r\nSubject: plain one\r\n\r\nbody text\r\n";

fn settings() -> spoolbox_config::Settings {
    spoolbox_config::Settings::default()
}

fn build(settings: &spoolbox_config::Settings, opts: HttpOptions) -> T {
    let (core, api) = test_api(settings);
    let r = Readiness::default();
    r.set(true);
    T {
        app: router(&opts, r, api),
        core,
    }
}

impl T {
    fn new() -> Self {
        build(&settings(), HttpOptions::new("/"))
    }

    async fn put(&self, raw: &str, rcpts: &[&str]) -> String {
        let env = Envelope {
            mail_from: "envelope@example.com".into(),
            rcpt_to: rcpts.iter().map(|r| r.to_string()).collect(),
            ..Envelope::default()
        };
        let ingest = self.core.ingest.clone();
        let raw = raw.as_bytes().to_vec();
        let id = tokio::task::spawn_blocking(move || ingest.store_raw(env, &raw, "smtp"))
            .await
            .expect("join")
            .expect("store");
        self.core.indexer.run_batch().await;
        id
    }

    async fn call(&self, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let res = self.app.clone().oneshot(req).await.expect("response");
        let (parts, body) = res.into_parts();
        let bytes = body.collect().await.expect("body").to_bytes();
        (parts.status, parts.headers, bytes.to_vec())
    }

    async fn req(
        &self,
        method: Method,
        path: &str,
        body: Option<&str>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "localhost:8025")
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .expect("request");
        self.call(req).await
    }

    async fn get_json(&self, path: &str) -> Value {
        let (s, h, b) = self.req(Method::GET, path, None).await;
        assert_eq!(s, StatusCode::OK, "{path}: {}", String::from_utf8_lossy(&b));
        assert_eq!(h[header::CONTENT_TYPE], "application/json", "{path}");
        serde_json::from_slice(&b).expect("json")
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[tokio::test]
async fn list_search_and_message_shapes() {
    let t = T::new();
    let plain = t
        .put(PLAIN_MSG, &["erin@example.com", "hidden@example.com"])
        .await;
    let html = t
        .put(HTML_MSG, &["bob+signup@example.com", "carol@example.com"])
        .await;

    let list = t.get_json("/api/v1/messages").await;
    assert_eq!(
        (
            list["total"].as_u64(),
            list["unread"].as_u64(),
            list["count"].as_u64()
        ),
        (Some(2), Some(2), Some(2))
    );
    assert_eq!(list["messages_count"], 2);
    assert_eq!(list["tags"], json!(["signup"]));
    let first = &list["messages"][0];
    assert_eq!(first["ID"], html.as_str(), "newest first");
    assert_eq!(first["From"]["Name"], "فرستنده");
    assert_eq!(
        first["Bcc"],
        Value::Null,
        "empty lists are null in summaries"
    );
    assert_eq!(
        first["ReplyTo"],
        json!([{"Name": "", "Address": "reply@example.com"}])
    );
    assert_eq!(first["Attachments"], 1);
    let second = &list["messages"][1];
    assert_eq!(
        second["Bcc"],
        json!([{"Name": "", "Address": "hidden@example.com"}]),
        "envelope-only recipient"
    );
    assert_eq!(second["Cc"], Value::Null);
    assert_eq!(
        second["ReplyTo"],
        json!([]),
        "lists use [] (search results use null)"
    );

    let page = t.get_json("/api/v1/messages?start=1&limit=1").await;
    assert_eq!(
        (page["start"].as_u64(), page["count"].as_u64()),
        (Some(1), Some(1))
    );

    let found = t
        .get_json("/api/v1/search?query=from%3Afa%40example.com")
        .await;
    assert_eq!(
        (found["messages_count"].as_u64(), found["total"].as_u64()),
        (Some(1), Some(2))
    );
    let unread = t.get_json("/api/v1/search?query=is%3Aunread").await;
    assert_eq!(
        unread["messages"][1]["ReplyTo"],
        Value::Null,
        "null in search results"
    );
    let (s, _, b) = t.req(Method::GET, "/api/v1/search", None).await;
    assert_eq!(
        (s, text(&b).as_str()),
        (StatusCode::BAD_REQUEST, "Error: no search query")
    );

    let m = t.get_json(&format!("/api/v1/message/{html}")).await;
    assert_eq!(m["MessageID"], "html-1@fixtures.test");
    assert_eq!(m["ReturnPath"], "envelope@example.com");
    assert_eq!(m["Bcc"], json!([]), "empty lists are [] in the message");
    assert_eq!(
        m["ListUnsubscribe"]["Links"],
        json!(["mailto:unsub@example.com", "https://example.invalid/unsub"])
    );
    assert_eq!(m["Inline"][0]["ContentID"], "logo1");
    assert_eq!(m["Attachments"][0]["FileName"], "evil.html");
    assert_eq!(
        m["Attachments"][0]["Checksums"]["SHA256"]
            .as_str()
            .map(str::len),
        Some(64)
    );
    assert!(
        m["HTML"].as_str().unwrap_or_default().contains("cid:logo1"),
        "API returns HTML unchanged"
    );
    let after = t.get_json("/api/v1/messages").await;
    assert_eq!(after["unread"], 1, "GET message marks it read");

    let latest = t.get_json("/api/v1/message/latest").await;
    assert_eq!(latest["ID"], html.as_str());
    let (s, _, b) = t
        .req(
            Method::GET,
            "/api/v1/message/latest?query=subject%3Anothing",
            None,
        )
        .await;
    assert_eq!(
        (s, text(&b).as_str()),
        (StatusCode::NOT_FOUND, "Message not found")
    );
    let (s, _, b) = t
        .req(Method::GET, "/api/v1/message/doesnotexist", None)
        .await;
    assert_eq!(
        (s, text(&b).as_str()),
        (StatusCode::NOT_FOUND, "404 page not found")
    );

    let h = t.get_json(&format!("/api/v1/message/{html}/headers")).await;
    assert_eq!(h["Message-Id"], json!(["<html-1@fixtures.test>"]));
    assert_eq!(h["X-Custom-Id"], json!(["abc123"]));

    let (s, hdr, b) = t
        .req(
            Method::GET,
            &format!("/api/v1/message/{plain}/raw?dl=1"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b, PLAIN_MSG.as_bytes(), "byte-exact raw message");
    assert_eq!(
        hdr[header::CONTENT_DISPOSITION],
        format!("attachment; filename=\"{plain}.eml\"").as_str()
    );
}

#[tokio::test]
async fn parts_thumbnails_and_views_are_safe() {
    let t = T::new();
    let html = t.put(HTML_MSG, &["bob+signup@example.com"]).await;
    let plain = t.put(PLAIN_MSG, &["erin@example.com"]).await;

    let (s, h, b) = t
        .req(Method::GET, &format!("/api/v1/message/{html}/part/2"), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        h[header::CONTENT_TYPE],
        "application/octet-stream",
        "HTML attachments are never rendered"
    );
    assert_eq!(
        h[header::CONTENT_DISPOSITION],
        "attachment; filename=\"evil.html\""
    );
    assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(text(&b), "<script>alert(1)</script>");

    let (s, h, b) = t
        .req(
            Method::GET,
            &format!("/api/v1/message/{html}/part/1.2/thumb"),
            None,
        )
        .await;
    assert_eq!(
        (s, h[header::CONTENT_TYPE].to_str().unwrap_or_default()),
        (StatusCode::OK, "image/jpeg")
    );
    assert_eq!(b.get(..2), Some(&[0xFF, 0xD8][..]));
    let (s, _, _) = t
        .req(
            Method::GET,
            &format!("/api/v1/message/{html}/part/99"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (s, h, b) = t
        .req(Method::GET, &format!("/view/{html}.html?embed=1"), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    let body = text(&b);
    assert!(
        body.contains(&format!("/api/v1/message/{html}/part/1.2")),
        "{body}"
    );
    assert!(!body.contains("<base"), "{body}");
    assert!(body.contains("target=\"_blank\""));
    let csp = h[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap_or_default();
    assert!(
        csp.contains("sandbox") && csp.contains("script-src 'nonce-"),
        "{csp}"
    );
    let (_, h, _) = t
        .req(Method::GET, &format!("/view/{html}.html?remote=0"), None)
        .await;
    let csp = h[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap_or_default();
    assert!(
        csp.contains("img-src 'self' data: blob:"),
        "remote images blocked: {csp}"
    );
    let (s, _, b) = t
        .req(Method::GET, &format!("/view/{plain}.html"), None)
        .await;
    assert_eq!(
        (s, text(&b).as_str()),
        (
            StatusCode::NOT_FOUND,
            "This message does not contain a HTML part"
        )
    );
    let (s, _, b) = t
        .req(Method::GET, &format!("/view/{plain}.txt"), None)
        .await;
    assert_eq!((s, text(&b).as_str()), (StatusCode::OK, "body text\r\n"));
}

#[tokio::test]
async fn read_delete_and_tags() {
    let t = T::new();
    let a = t.put(PLAIN_MSG, &["erin@example.com"]).await;
    let b = t.put(HTML_MSG, &["bob+signup@example.com"]).await;

    let (s, _, body) = t
        .req(
            Method::PUT,
            "/api/v1/messages",
            Some(&json!({"Read": true, "IDs": [a], "Search": "x"}).to_string()),
        )
        .await;
    assert_eq!(
        (s, text(&body).as_str()),
        (
            StatusCode::BAD_REQUEST,
            "You may specify either IDs or a search query, not both"
        )
    );
    let (s, _, _) = t
        .req(Method::PUT, "/api/v1/messages", Some(r#"{"read": true}"#))
        .await;
    assert_eq!(s, StatusCode::OK, "Go-style case-insensitive fields");
    assert_eq!(t.get_json("/api/v1/messages").await["unread"], 0);

    let (s, _, _) = t
        .req(
            Method::PUT,
            "/api/v1/tags",
            Some(&json!({"IDs": [a], "Tags": ["فارسی", "ascii-ok"]}).to_string()),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        t.get_json(&format!("/api/v1/message/{a}")).await["Tags"],
        json!(["ascii-ok", "فارسی"]),
        "Unicode tags are kept"
    );
    let (s, _, _) = t
        .req(
            Method::PUT,
            "/api/v1/tags/ascii-ok",
            Some(r#"{"Name":"renamed"}"#),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = t.req(Method::DELETE, "/api/v1/tags/renamed", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t.get_json("/api/v1/tags").await, json!(["signup", "فارسی"]));

    let (s, _, _) = t
        .req(Method::DELETE, "/api/v1/messages", Some("{oops"))
        .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "invalid JSON never deletes everything"
    );
    assert_eq!(t.get_json("/api/v1/messages").await["total"], 2);
    let (s, _, _) = t
        .req(
            Method::DELETE,
            "/api/v1/messages",
            Some(&json!({"IDs": [b]}).to_string()),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t.get_json("/api/v1/messages").await["total"], 1);
    let (s, _, _) = t
        .req(Method::DELETE, "/api/v1/search?query=subject%3Aplain", None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t.get_json("/api/v1/messages").await["total"], 0);
    assert_eq!(
        t.get_json("/api/v1/info").await["RuntimeStats"]["MessagesDeleted"],
        2
    );
}

#[tokio::test]
async fn send_api() {
    let t = T::new();
    let body = json!({"From": {"Email": "api@example.com", "Name": "API"}, "To": [{"Email": "to@example.com"}],
        "Bcc": ["bcc@example.com"], "Subject": "fixture-send", "Text": "hi", "Tags": ["sent"], "Headers": {"X-Test": "1"}});
    let (s, h, b) = t
        .req(Method::POST, "/api/v1/send", Some(&body.to_string()))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", text(&b));
    assert_eq!(h[header::CONTENT_TYPE], "application/json");
    let id = serde_json::from_slice::<Value>(&b).expect("json")["ID"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    t.core.indexer.run_batch().await;
    let m = t.get_json(&format!("/api/v1/message/{id}")).await;
    assert_eq!(m["Subject"], "fixture-send");
    assert_eq!(m["Tags"], json!(["sent"]));
    assert_eq!(
        m["Bcc"],
        json!([{"Name": "", "Address": "bcc@example.com"}])
    );
    let env = t.get_json(&format!("/api/v2/messages/{id}/envelope")).await;
    assert_eq!(env["source"], "send_api");
    assert_eq!(env["rcpt_to"], json!(["to@example.com", "bcc@example.com"]));

    let (s, _, b) = t
        .req(
            Method::POST,
            "/api/v1/send",
            Some(r#"{"From":{"Email":"a@b.test"},"Headers":{"Subject":"x"}}"#),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        serde_json::from_slice::<Value>(&b).expect("json"),
        json!({"Error": "cannot overwrite header: \"Subject\""})
    );
    let (s, _, b) = t.req(Method::POST, "/api/v1/send", Some("{not json")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(serde_json::from_slice::<Value>(&b).expect("json")["Error"].is_string());
}

fn basic(user: &str, pass: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
    )
}

#[tokio::test]
async fn basic_auth_and_send_api_auth() {
    let mut opts = HttpOptions::new("/");
    opts.ui_auth = Some(Arc::new(
        spoolbox_auth::Credentials::parse("ui:secret").expect("creds"),
    ));
    opts.send_auth = SendAuth::AcceptAny;
    let t = build(&settings(), opts);

    let (s, h, b) = t.req(Method::GET, "/api/v1/messages", None).await;
    assert_eq!(
        (s, text(&b).as_str()),
        (StatusCode::UNAUTHORIZED, "Unauthorized.\n")
    );
    assert_eq!(h[header::WWW_AUTHENTICATE], "Basic realm=\"Login\"");
    let (s, _, _) = t.req(Method::GET, "/livez", None).await;
    assert_eq!(s, StatusCode::OK, "probes need no credentials");
    for (cred, want) in [
        (basic("ui", "secret"), StatusCode::OK),
        (basic("ui", "wrong"), StatusCode::UNAUTHORIZED),
    ] {
        let req = Request::get("/api/v1/messages")
            .header(header::HOST, "localhost")
            .header(header::AUTHORIZATION, cred)
            .body(Body::empty())
            .expect("req");
        assert_eq!(t.call(req).await.0, want);
    }
    let (s, _, b) = t
        .req(
            Method::POST,
            "/api/v1/send",
            Some(r#"{"From":{"Email":"a@b.test"},"To":[{"Email":"c@d.test"}]}"#),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::OK,
        "--send-api-auth-accept-any: {}",
        text(&b)
    );

    let mut opts = HttpOptions::new("/");
    opts.ui_auth = Some(Arc::new(
        spoolbox_auth::Credentials::parse("ui:secret").expect("creds"),
    ));
    opts.send_auth = SendAuth::Credentials(Arc::new(
        spoolbox_auth::Credentials::parse("sender:pw").expect("creds"),
    ));
    let t = build(&settings(), opts);
    let send = |cred: String| {
        Request::post("/api/v1/send")
            .header(header::HOST, "localhost")
            .header(header::AUTHORIZATION, cred)
            .body(Body::from(
                r#"{"From":{"Email":"a@b.test"},"To":[{"Email":"c@d.test"}]}"#,
            ))
            .expect("req")
    };
    assert_eq!(
        t.call(send(basic("ui", "secret"))).await.0,
        StatusCode::UNAUTHORIZED,
        "only send credentials"
    );
    let (s, _, b) = t.call(send(basic("sender", "pw"))).await;
    assert_eq!(s, StatusCode::OK);
    let id = serde_json::from_slice::<Value>(&b).expect("json")["ID"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let req = Request::get(format!("/api/v1/message/{id}"))
        .header(header::HOST, "localhost")
        .header(header::AUTHORIZATION, basic("ui", "secret"))
        .body(Body::empty())
        .expect("req");
    let (_, _, b) = t.call(req).await;
    assert_eq!(
        serde_json::from_slice::<Value>(&b).expect("json")["Username"],
        "sender"
    );
}

#[tokio::test]
async fn cors_blocks_foreign_origins() {
    let origin = |t: &T, o: &'static str| {
        let req = Request::get("/api/v1/messages")
            .header(header::HOST, "localhost:8025")
            .header(header::ORIGIN, o)
            .body(Body::empty())
            .expect("req");
        let app = t.app.clone();
        async move {
            let res = app.oneshot(req).await.expect("res");
            (
                res.status(),
                res.headers()
                    .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                    .cloned(),
            )
        }
    };
    let t = T::new();
    assert_eq!(
        origin(&t, "http://evil.test").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        origin(&t, "http://localhost:8025").await.0,
        StatusCode::OK,
        "same origin"
    );
    let mut opts = HttpOptions::new("/");
    opts.cors = vec!["app.test:3000".into()];
    let t = build(&settings(), opts);
    let (s, acao) = origin(&t, "https://app.test:3000").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        acao.as_ref().map(|v| v.as_bytes()),
        Some(&b"https://app.test:3000"[..])
    );
    assert_eq!(
        origin(&t, "https://app.test").await.0,
        StatusCode::FORBIDDEN,
        "port must match"
    );
}

#[tokio::test]
async fn info_webui_and_chaos() {
    let t = T::new();
    let info = t.get_json("/api/v1/info").await;
    assert_eq!(info["LatestVersion"], "disabled");
    assert!(info["RuntimeStats"]["Uptime"].is_u64());
    let ui = t.get_json("/api/v1/webui").await;
    assert_eq!(
        (
            ui["ChaosEnabled"].as_bool(),
            ui["MessageRelay"]["Enabled"].as_bool()
        ),
        (Some(false), Some(false))
    );
    let (s, _, b) = t.req(Method::GET, "/api/v1/chaos", None).await;
    assert_eq!(
        (s, text(&b).as_str()),
        (StatusCode::BAD_REQUEST, "Chaos is not enabled")
    );
    let (s, _, _) = t.req(Method::OPTIONS, "/api/v1/messages", None).await;
    assert_eq!(s, StatusCode::OK);
    let swagger = t.get_json("/api/v1/swagger.json").await;
    assert!(swagger.get("paths").is_some());

    let mut s = settings();
    s.chaos.enabled = true;
    let t = build(&s, HttpOptions::new("/"));
    let c = t.get_json("/api/v1/chaos").await;
    assert_eq!(c["Sender"], json!({"ErrorCode": 451, "Probability": 0}));
    let (st, _, b) = t
        .req(
            Method::PUT,
            "/api/v1/chaos",
            Some(r#"{"Recipient":{"ErrorCode":452,"Probability":50}}"#),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let c: Value = serde_json::from_slice(&b).expect("json");
    assert_eq!(c["Recipient"], json!({"ErrorCode": 452, "Probability": 50}));
    assert_eq!(
        c["Authentication"],
        json!({"ErrorCode": 535, "Probability": 0})
    );
    let (st, _, _) = t
        .req(
            Method::PUT,
            "/api/v1/chaos",
            Some(r#"{"Sender":{"ErrorCode":200,"Probability":1}}"#),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn v2_endpoints() {
    let t = T::new();
    let html = t.put(HTML_MSG, &["bob+signup@example.com"]).await;

    let mime = t.get_json(&format!("/api/v2/messages/{html}/mime")).await;
    assert_eq!(mime["content_type"], "multipart/mixed");
    assert_eq!(mime["children"][0]["children"][1]["content_id"], "logo1");
    let links = t.get_json(&format!("/api/v2/messages/{html}/links")).await;
    assert_eq!(
        links[0]["url"],
        "https://example.invalid/verify?code=482913"
    );
    let codes = t.get_json(&format!("/api/v2/messages/{html}/codes")).await;
    assert_eq!(codes[0], json!({"value": "482913", "source": "subject"}));
    let (s, h, _) = t
        .req(
            Method::GET,
            &format!("/api/v2/messages/{html}/codes?pattern=(unclosed"),
            None,
        )
        .await;
    assert_eq!(
        (s, h[header::CONTENT_TYPE].to_str().unwrap_or_default()),
        (StatusCode::BAD_REQUEST, "application/problem+json")
    );
    let (s, h, b) = t
        .req(Method::GET, "/api/v2/messages/nope/envelope", None)
        .await;
    assert_eq!(
        (s, h[header::CONTENT_TYPE].to_str().unwrap_or_default()),
        (StatusCode::NOT_FOUND, "application/problem+json")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&b).expect("json")["status"],
        404
    );

    let (s, _, _) = t
        .req(
            Method::POST,
            "/api/v2/wait",
            Some(r#"{"query":"subject:never","timeout_ms":50}"#),
        )
        .await;
    assert_eq!(s, StatusCode::REQUEST_TIMEOUT);
    let (s, _, _) = t
        .req(Method::POST, "/api/v2/wait", Some(r#"{"timeout_ms":50}"#))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // A message that arrives while waiting.
    let app = t.app.clone();
    let waiter = tokio::spawn(async move {
        let req = Request::post("/api/v2/wait")
            .header(header::HOST, "localhost")
            .body(Body::from(r#"{"query":"subject:plain","timeout_ms":5000}"#))
            .expect("req");
        let res = app.oneshot(req).await.expect("res");
        let status = res.status();
        let b = res.into_body().collect().await.expect("body").to_bytes();
        (
            status,
            serde_json::from_slice::<Value>(&b).unwrap_or(Value::Null),
        )
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let plain = t.put(PLAIN_MSG, &["erin@example.com"]).await;
    let (s, v) = waiter.await.expect("join");
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["id"], plain.as_str());
    assert_eq!(
        v["to"],
        json!([{"name": "", "address": "erin@example.com"}])
    );
}

#[tokio::test]
async fn sse_replays_from_since() {
    let t = T::new();
    let id = t.put(PLAIN_MSG, &["erin@example.com"]).await;
    let req = Request::get("/api/v2/events?since=0")
        .header(header::HOST, "localhost")
        .body(Body::empty())
        .expect("req");
    let res = t.app.clone().oneshot(req).await.expect("res");
    assert_eq!(res.headers()[header::CONTENT_TYPE], "text/event-stream");
    let mut body = res.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
        .await
        .expect("timely")
        .expect("frame")
        .expect("ok");
    let chunk = text(frame.data_ref().expect("data"));
    assert!(chunk.starts_with("id: 1\n"), "{chunk}");
    assert!(
        chunk.contains("\"type\":\"message.received\"") && chunk.contains(&id),
        "{chunk}"
    );
}

#[tokio::test]
async fn import_raw_messages() {
    let t = T::new();
    let (s, h, b) = t
        .req(Method::POST, "/api/v2/messages", Some(HTML_MSG))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{}", text(&b));
    assert_eq!(h[header::CONTENT_TYPE], "application/json");
    let id = serde_json::from_slice::<Value>(&b).expect("json")["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    t.core.indexer.run_batch().await;
    let env = t.get_json(&format!("/api/v2/messages/{id}/envelope")).await;
    assert_eq!(env["source"], "import");
    assert_eq!(env["mail_from"], "fa@example.com");
    assert_eq!(
        env["rcpt_to"],
        json!(["bob+signup@example.com", "carol@example.com"])
    );
    let (s, _, raw) = t
        .req(Method::GET, &format!("/api/v1/message/{id}/raw"), None)
        .await;
    assert_eq!(
        (s, raw.as_slice()),
        (StatusCode::OK, HTML_MSG.as_bytes()),
        "stored byte-exact"
    );
    let (s, h, _) = t.req(Method::POST, "/api/v2/messages", Some("  ")).await;
    assert_eq!(
        (s, h[header::CONTENT_TYPE].to_str().unwrap_or_default()),
        (StatusCode::BAD_REQUEST, "application/problem+json")
    );
}

#[tokio::test]
async fn cursor_then_wait_does_not_miss_early_mail() {
    let t = T::new();
    let cursor = t.get_json("/api/v2/events/cursor").await["seq"]
        .as_i64()
        .expect("seq");
    // The email arrives before anyone waits for it.
    let id = t.put(PLAIN_MSG, &["erin@example.com"]).await;
    let body =
        json!({"query": "subject:plain", "timeout_ms": 1000, "since_seq": cursor}).to_string();
    let (s, _, b) = t.req(Method::POST, "/api/v2/wait", Some(&body)).await;
    assert_eq!(s, StatusCode::OK, "{}", text(&b));
    assert_eq!(
        serde_json::from_slice::<Value>(&b).expect("json")["id"],
        id.as_str()
    );
}

#[tokio::test]
async fn chaos_rules_api() {
    let t = T::new();
    let (s, _, _) = t.req(Method::GET, "/api/v2/chaos/rules", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "off unless --enable-chaos");

    let mut settings = settings();
    settings.chaos.enabled = true;
    let t = build(&settings, HttpOptions::new("/"));
    let rules = json!([
        {"stage": "recipient", "match": "@flaky[.]test$", "code": 452, "message": "4.2.2 Mailbox full", "times": 2},
        {"stage": "connect", "delay_ms": 250}
    ]);
    let (s, _, b) = t
        .req(Method::PUT, "/api/v2/chaos/rules", Some(&rules.to_string()))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", text(&b));
    let got = t.get_json("/api/v2/chaos/rules").await;
    assert_eq!(got[0]["fired"], 0);
    assert_eq!(got[1]["stage"], "connect");
    let (s, _, b) = t
        .req(
            Method::PUT,
            "/api/v2/chaos/rules",
            Some(r#"[{"stage":"data"}]"#),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(text(&b).contains("needs a code"), "{}", text(&b));
    let (s, _, _) = t.req(Method::DELETE, "/api/v2/chaos/rules", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(t.get_json("/api/v2/chaos/rules").await, json!([]));
}

async fn rpc(t: &T, body: Value) -> (StatusCode, Value) {
    let (s, _, b) = t.req(Method::POST, "/mcp", Some(&body.to_string())).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

#[tokio::test]
async fn mcp_tools() {
    let t = T::new();
    let (s, init) = rpc(&t, json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "spoolbox");
    let (s, _) = rpc(
        &t,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);

    let (_, list) = rpc(
        &t,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|x| x["name"].as_str())
        .collect();
    assert!(names.contains(&"wait_for_message") && names.contains(&"get_codes"));
    assert!(
        !names.contains(&"delete_messages"),
        "deleting needs --mcp-allow-delete"
    );

    let (_, cursor) = rpc(
        &t,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "event_cursor", "arguments": {}}}),
    )
    .await;
    let seq = cursor["result"]["structuredContent"]["seq"]
        .as_i64()
        .expect("seq");
    let id = t.put(HTML_MSG, &["bob+signup@example.com"]).await;
    let (_, waited) = rpc(&t, json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {"name": "wait_for_message", "arguments": {"query": "subject:order", "since_seq": seq, "timeout_seconds": 2}}})).await;
    assert_eq!(
        waited["result"]["structuredContent"]["message"]["id"],
        id.as_str(),
        "{waited}"
    );

    let (_, codes) = rpc(
        &t,
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": {"name": "get_codes", "arguments": {"id": "latest"}}}),
    )
    .await;
    assert_eq!(
        codes["result"]["structuredContent"]["codes"][0]["value"],
        "482913"
    );
    let (_, msg) = rpc(
        &t,
        json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call",
        "params": {"name": "get_message", "arguments": {"id": id}}}),
    )
    .await;
    let m = &msg["result"]["structuredContent"];
    assert_eq!(m["envelope"]["mail_from"], "envelope@example.com");
    assert_eq!(
        m["message"]["read"], false,
        "reading through MCP has no side effects"
    );

    let (_, missing) = rpc(
        &t,
        json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {"name": "get_raw", "arguments": {"id": "nope"}}}),
    )
    .await;
    assert_eq!(missing["result"]["isError"], true);
    let (_, unknown) = rpc(
        &t,
        json!({"jsonrpc": "2.0", "id": 8, "method": "resources/list"}),
    )
    .await;
    assert_eq!(unknown["error"]["code"], -32601);

    let req = Request::post("/mcp")
        .header(header::HOST, "localhost:8025")
        .header(header::ORIGIN, "https://evil.test")
        .body(Body::from(
            json!({"jsonrpc": "2.0", "id": 9, "method": "ping"}).to_string(),
        ))
        .expect("req");
    assert_eq!(
        t.call(req).await.0,
        StatusCode::FORBIDDEN,
        "foreign origins are refused"
    );
}

#[tokio::test]
async fn mcp_delete_only_when_allowed() {
    let (core, api) = test_api(&settings());
    let api = api.with_mcp_delete(true);
    let r = Readiness::default();
    r.set(true);
    let t = T {
        app: router(&HttpOptions::new("/"), r, api),
        core,
    };
    let id = t.put(PLAIN_MSG, &["erin@example.com"]).await;
    let (_, all) = rpc(
        &t,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "delete_messages", "arguments": {"query": ""}}}),
    )
    .await;
    assert_eq!(
        all["result"]["isError"], true,
        "an empty query never deletes everything"
    );
    let (_, del) = rpc(
        &t,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "delete_messages", "arguments": {"ids": [id]}}}),
    )
    .await;
    assert_eq!(del["result"]["structuredContent"]["deleted"], 1);
}

#[tokio::test]
async fn webhooks_post_signed_summaries() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let hook = listener.local_addr().expect("addr");
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4);
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let mut buf = vec![0u8; 64 * 1024];
            let mut got = Vec::new();
            // Read until the JSON body is complete (headers + Content-Length bytes).
            loop {
                let n = s.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if body.len() >= len {
                        break;
                    }
                }
            }
            let _ = s
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await;
            let _ = tx.send(String::from_utf8_lossy(&got).to_string()).await;
        }
    });

    let mut s = settings();
    s.webhook.url = Some(format!("http://{hook}/hook"));
    let (core, api) = test_api(&s);
    let (_stop_tx, stop) = tokio::sync::watch::channel(false);
    let _worker = spawn_webhooks(
        api.clone(),
        WebhookOptions {
            url: format!("http://{hook}/hook"),
            limit_per_second: 0,
            secret: Some("topsecret".into()),
            label: "dev".into(),
            allow_internal: false,
            allow_untrusted_tls: false,
        },
        stop,
    );
    let r = Readiness::default();
    r.set(true);
    let t = T {
        app: router(&HttpOptions::new("/"), r, api),
        core,
    };
    let id = t.put(PLAIN_MSG, &["erin@example.com"]).await;

    let request = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("webhook in time")
        .expect("request");
    let (head, body) = request.split_once("\r\n\r\n").expect("http request");
    assert!(head.starts_with("POST /hook HTTP/1.1"), "{head}");
    let lower = head.to_ascii_lowercase();
    assert!(lower.contains("x-spoolbox-label: dev"), "{head}");
    let summary: Value = serde_json::from_str(body).expect("json body");
    assert_eq!(summary["ID"], id.as_str());
    let sig = head
        .lines()
        .find_map(|l| {
            l.strip_prefix("x-spoolbox-signature: ")
                .or_else(|| l.strip_prefix("X-Spoolbox-Signature: "))
        })
        .expect("signature header");
    let t_value: u64 = sig
        .split(',')
        .find_map(|p| p.strip_prefix("t="))
        .and_then(|v| v.parse().ok())
        .expect("timestamp");
    assert_eq!(
        sig,
        webhook_signature("topsecret", t_value, body.as_bytes()),
        "receivers can verify the body"
    );
    // The job is done and removed.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(t.core.store.pending_jobs().expect("jobs"), 0);
}

#[tokio::test]
async fn release_is_refused_without_a_relay_and_checks_recipients_with_one() {
    async fn post(app: &axum::Router, uri: &str, ctype: &str, body: &str) -> (StatusCode, String) {
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::post(uri)
                    .header("content-type", ctype)
                    .body(axum::body::Body::from(body.to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    // Off by default.
    let t = T::new();
    let id = t.put(PLAIN_MSG, &["erin@example.com"]).await;
    let (status, body) = post(
        &t.app,
        &format!("/api/v1/message/{id}/release"),
        "application/json",
        r#"{"To":["dev@example.com"]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("not configured"), "{body}");

    let mut s = settings();
    s.relay.target = Some(spoolbox_config::SmtpTarget {
        host: "smtp.example.com".into(),
        port: 587,
        security: spoolbox_config::SmtpSecurity::StartTls,
        allow_insecure: false,
        login: Some(spoolbox_config::SmtpLogin {
            mechanism: spoolbox_config::SmtpMechanism::Plain,
            username: "relay-user".into(),
            password: "s3cret".into(),
        }),
        return_path: None,
        override_from: None,
        preserve_message_ids: false,
        limit_per_minute: 0,
    });
    s.relay.allowed_recipients = Some(r"@example\.com$".into());
    let (core, api) = test_api(&s);
    let api = api.with_relay(core.relay.clone());
    let r = Readiness::default();
    r.set(true);
    let t = T {
        app: router(&HttpOptions::new("/"), r, api),
        core,
    };
    let id = t.put(PLAIN_MSG, &["erin@example.com"]).await;
    let uri = format!("/api/v1/message/{id}/release");
    let good = r#"{"To":["dev@example.com"]}"#;

    // A cross-site form can only send these content types.
    for ctype in [
        "text/plain",
        "application/x-www-form-urlencoded",
        "multipart/form-data",
    ] {
        let (status, _) = post(&t.app, &uri, ctype, good).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{ctype}");
    }
    for (body, expected) in [
        (r#"{"To":[]}"#, "no recipients"),
        ("{}", "no recipients"),
        ("not json", "expected"),
        (r#"{"To":["dev@attacker.test"]}"#, "not allowed"),
        (
            r#"{"To":["dev@example.com","dev@attacker.test"]}"#,
            "not allowed",
        ),
        (
            r#"{"To":["dev@attacker.test\r\nRCPT TO:<x@example.com>"]}"#,
            "not allowed",
        ),
    ] {
        let (status, text) = post(&t.app, &uri, "application/json", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(text.contains(expected), "{body}: {text}");
    }
    let job_count = || {
        t.core
            .store
            .claim_jobs("relay", 10, Duration::ZERO)
            .expect("claim")
            .len()
    };
    assert_eq!(job_count(), 0, "nothing refused was queued");

    let (status, _) = post(
        &t.app,
        "/api/v1/message/missing/release",
        "application/json",
        good,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, text) = post(&t.app, &uri, "application/json; charset=utf-8", good).await;
    assert_eq!((status, text.as_str()), (StatusCode::OK, "ok"));
    let (status, _) = post(
        &t.app,
        "/api/v1/message/latest/release",
        "application/json",
        good,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(job_count(), 2);

    let ui = t.get_json("/api/v1/webui").await;
    assert_eq!(ui["MessageRelay"]["Enabled"], true);
    assert_eq!(ui["MessageRelay"]["SMTPServer"], "smtp.example.com:587");
    assert_eq!(ui["MessageRelay"]["AllowedRecipients"], r"@example\.com$");
    let text = ui.to_string();
    assert!(
        !text.contains("s3cret") && !text.contains("relay-user"),
        "credentials stay private: {text}"
    );
}

#[tokio::test]
async fn link_check_reports_statuses_and_refuses_internal_addresses() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let site = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).await.unwrap_or(0);
            let reply: &[u8] = if buf[..n].starts_with(b"HEAD /gone") {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else if buf[..n].starts_with(b"HEAD /moved") {
                b"HTTP/1.1 302 Found\r\nLocation: /ok\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else {
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            };
            let _ = s.write_all(reply).await;
        }
    });
    let message = format!(
        "From: a@example.com\r\nSubject: links\r\nContent-Type: text/html\r\n\r\n<a href=\"http://{site}/ok\">ok</a> <a href=\"http://{site}/gone\">gone</a> <a href=\"http://{site}/moved\">moved</a> <a href=\"mailto:x@example.com\">mail</a>\r\n"
    );

    // The default policy: the message cannot reach this machine.
    let t = T::new();
    let id = t.put(&message, &["b@example.com"]).await;
    let report = t
        .get_json(&format!("/api/v1/message/{id}/link-check"))
        .await;
    assert_eq!(report["Errors"], 3, "{report}");
    let first = &report["Links"][0];
    assert_eq!(first["StatusCode"], 0);
    assert!(
        first["Status"]
            .as_str()
            .expect("status")
            .contains("internal address"),
        "{report}"
    );

    // With --allow-internal-http-requests.
    let (core, api) = test_api(&settings());
    let api = api.with_link_check(spoolbox_app::linkcheck::LinkChecker::from_settings(
        &spoolbox_config::EgressSettings {
            allow_internal: true,
            link_check_rate_limit: false,
            ..spoolbox_config::EgressSettings::default()
        },
    ));
    let r = Readiness::default();
    r.set(true);
    let t = T {
        app: router(&HttpOptions::new("/"), r, api),
        core,
    };
    let id = t.put(&message, &["b@example.com"]).await;
    let report = t
        .get_json(&format!("/api/v1/message/{id}/link-check"))
        .await;
    assert_eq!(report["Errors"], 1, "{report}");
    let links = report["Links"].as_array().expect("links");
    assert_eq!(links.len(), 3, "mailto is left out: {report}");
    assert_eq!(links[0]["URL"], format!("http://{site}/ok"));
    assert_eq!(links[0]["StatusCode"], 200);
    assert_eq!(links[0]["Status"], "OK");
    assert_eq!(links[1]["StatusCode"], 404);
    assert_eq!(links[2]["StatusCode"], 302);

    let report = t
        .get_json("/api/v1/message/latest/link-check?follow=true")
        .await;
    assert_eq!(report["Links"][2]["StatusCode"], 200, "{report}");
    let (status, _, _) = t
        .req(Method::GET, "/api/v1/message/missing/link-check", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // With --disable-link-check.
    let (core, api) = test_api(&settings());
    let api = api.with_link_check(None);
    let r = Readiness::default();
    r.set(true);
    let t = T {
        app: router(&HttpOptions::new("/"), r, api),
        core,
    };
    let id = t.put(&message, &["b@example.com"]).await;
    let (status, _, _) = t
        .req(
            Method::GET,
            &format!("/api/v1/message/{id}/link-check"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn metrics_are_opt_in_and_in_the_prometheus_format() {
    let t = T::new();
    let (status, _, _) = t.req(Method::GET, "/metrics", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "off by default");

    let mut opts = HttpOptions::new("/");
    opts.metrics = true;
    let t = build(&settings(), opts);
    let id = t
        .put(
            "From: secret-sender@example.com\r\nSubject: private subject\r\nX-Tags: private-tag\r\n\r\nbody\r\n",
            &["secret-rcpt@example.com"],
        )
        .await;
    t.core
        .store
        .enqueue("relay", None, "{}", Duration::from_secs(3600))
        .expect("enqueue");
    let (status, headers, body) = t.req(Method::GET, "/metrics", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers[header::CONTENT_TYPE],
        "text/plain; version=0.0.4; charset=utf-8"
    );
    let body = text(&body);
    for expected in [
        "spoolbox_messages 1\n",
        "spoolbox_messages_unread 1\n",
        "# TYPE spoolbox_messages_deleted_total counter\n",
        "spoolbox_build_info{version=\"",
        "spoolbox_jobs{kind=\"relay\",state=\"pending\"} 1\n",
    ] {
        assert!(body.contains(expected), "{expected} missing in\n{body}");
    }
    for private in ["secret-sender", "secret-rcpt", "private", id.as_str()] {
        assert!(!body.contains(private), "{private} leaked in\n{body}");
    }
    // Every line is a comment or `name[{labels}] value`.
    for line in body.lines().filter(|l| !l.starts_with('#')) {
        let (name, value) = line.rsplit_once(' ').expect(line);
        assert!(name.starts_with("spoolbox_"), "{line}");
        assert!(value.parse::<f64>().is_ok(), "{line}");
    }

    // Behind the same authentication as everything else.
    let mut opts = HttpOptions::new("/");
    opts.metrics = true;
    opts.ui_auth = Some(Arc::new(
        spoolbox_auth::Credentials::parse("dev:secret").expect("credentials"),
    ));
    let t = build(&settings(), opts);
    let (status, _, _) = t.req(Method::GET, "/metrics", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
