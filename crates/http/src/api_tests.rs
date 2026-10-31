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
