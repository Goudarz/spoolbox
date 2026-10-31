//! `POST /api/v1/send`. Addresses may also be plain strings
//! (`"Name <a@b>"` or `"a@b"`) instead of `{"Email", "Name"}` objects (NEW-17).

use axum::Router;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::post;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address as MbAddress;
use mail_builder::headers::text::Text;
use serde::Serialize;
use serde_json::Value;
use spoolbox_core::Envelope;

use super::field;
use crate::access::AuthUser;
use crate::api::{Api, json};

pub(super) fn routes() -> Router<Api> {
    Router::new().route("/api/v1/send", post(send))
}

#[derive(Serialize)]
struct SendError {
    #[serde(rename = "Error")]
    error: String,
}

#[derive(Serialize)]
struct Sent {
    #[serde(rename = "ID")]
    id: String,
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    json(status, &SendError { error: msg.into() })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Addr {
    name: String,
    email: String,
}

/// Accepted: `a@b`, `<a@b>`, `Name <a@b>`.
fn parse_address(raw: &str) -> Option<(String, String)> {
    let raw = raw.trim();
    let (name, addr) = match (raw.rfind('<'), raw.ends_with('>')) {
        (Some(i), true) => (
            raw[..i].trim().trim_matches('"').to_string(),
            &raw[i + 1..raw.len() - 1],
        ),
        _ => (String::new(), raw),
    };
    let (local, domain) = addr.rsplit_once('@')?;
    let ok = !local.is_empty()
        && !domain.is_empty()
        && !addr
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '<' || c == '>');
    ok.then(|| (name, addr.to_string()))
}

fn address(v: &Value, what: &str) -> Result<Addr, String> {
    let (name, email) = match v {
        Value::String(s) => (String::new(), s.clone()),
        Value::Object(_) => (
            field(v, "Name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            field(v, "Email")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        _ => return Err(format!("invalid {what} address")),
    };
    let (parsed_name, addr) =
        parse_address(&email).ok_or_else(|| format!("invalid {what} address: {email}"))?;
    Ok(Addr {
        name: if name.is_empty() { parsed_name } else { name },
        email: addr,
    })
}

fn addresses(v: Option<&Value>, what: &str) -> Result<Vec<Addr>, String> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items.iter().map(|i| address(i, what)).collect(),
        Some(_) => Err(format!("invalid {what} address list")),
    }
}

fn mb(list: &[Addr]) -> MbAddress<'static> {
    MbAddress::new_list(
        list.iter()
            .map(|a| {
                let name = (!a.name.is_empty()).then(|| a.name.clone());
                MbAddress::new_address(name, a.email.clone())
            })
            .collect(),
    )
}

/// Go's `http.DetectContentType` for the common cases.
fn sniff(data: &[u8]) -> &'static str {
    match data {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [b'%', b'P', b'D', b'F', b'-', ..] => "application/pdf",
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => "image/webp",
        [b'P', b'K', 3, 4, ..] => "application/zip",
        _ if std::str::from_utf8(data).is_ok() => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn valid_header_key(k: &str) -> bool {
    !k.is_empty() && k.bytes().all(|c| (33..=126).contains(&c) && c != b':')
}

const RESTRICTED: [&str; 9] = [
    "To",
    "From",
    "Cc",
    "Bcc",
    "Reply-To",
    "Date",
    "Subject",
    "Content-Type",
    "Mime-Version",
];

/// Builds the message; returns the envelope sender, recipients and the bytes.
fn build(v: &Value) -> Result<(String, Vec<String>, Vec<u8>), String> {
    let from = field(v, "From").ok_or("From is required")?;
    let from = address(from, "From")?;
    let to = addresses(field(v, "To"), "To")?;
    let cc = addresses(field(v, "Cc"), "Cc")?;
    let bcc = addresses(field(v, "Bcc"), "Bcc")?;
    let reply_to = addresses(field(v, "ReplyTo"), "Reply-To")?;
    let str_field = |name: &str| {
        field(v, name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let subject = str_field("Subject");
    let text = str_field("Text");
    let html = str_field("HTML");
    let tags = super::strings(field(v, "Tags"))?;

    let mut msg = MessageBuilder::new()
        .from(mb(std::slice::from_ref(&from)))
        .subject(subject)
        .text_body(text);
    if !html.is_empty() {
        msg = msg.html_body(html);
    }
    if !to.is_empty() {
        msg = msg.to(mb(&to));
    }
    if !cc.is_empty() {
        msg = msg.cc(mb(&cc));
    }
    if !bcc.is_empty() {
        msg = msg.bcc(mb(&bcc));
    }
    if !reply_to.is_empty() {
        msg = msg.reply_to(mb(&reply_to));
    }
    let mut restricted: Vec<&str> = RESTRICTED.to_vec();
    if !tags.is_empty() {
        msg = msg.header("X-Tags", Text::new(tags.join(", ")));
        restricted.push("X-Tags");
    }
    if let Some(Value::Object(headers)) = field(v, "Headers") {
        for (k, val) in headers {
            if !valid_header_key(k) {
                return Err(format!("invalid header key: {k:?}"));
            }
            if restricted.iter().any(|r| r.eq_ignore_ascii_case(k)) {
                return Err(format!("cannot overwrite header: \"{k}\""));
            }
            let val = val.as_str().unwrap_or_default().replace(['\r', '\n'], " ");
            // mail-builder only skips its own Message-ID for this exact spelling.
            let name = if k.eq_ignore_ascii_case("Message-ID") {
                "Message-ID".to_string()
            } else {
                k.clone()
            };
            msg = msg.header(name, Text::new(val));
        }
    }
    if let Some(Value::Array(items)) = field(v, "Attachments") {
        for a in items {
            let s = |n: &str| {
                field(a, n)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let (content, filename, ctype, cid) = (
                s("Content"),
                s("Filename"),
                s("ContentType"),
                s("ContentID"),
            );
            let b64 = content.rsplit(',').next().unwrap_or_default();
            let data = B64
                .decode(b64.trim())
                .map_err(|e| format!("error decoding base64 attachment \"{filename}\": {e}"))?;
            let ctype = if ctype.is_empty() {
                sniff(&data).to_string()
            } else {
                ctype
            };
            msg = if cid.is_empty() {
                msg.attachment(ctype, filename, data)
            } else {
                msg.inline(ctype, cid, data)
            };
        }
    }
    let raw = msg
        .write_to_vec()
        .map_err(|e| format!("error building message: {e}"))?;
    let rcpts = to
        .iter()
        .chain(&cc)
        .chain(&bcc)
        .map(|a| a.email.clone())
        .collect();
    Ok((from.email, rcpts, raw))
}

async fn send(State(api): State<Api>, req: Request) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<crate::Peer>>()
        .map(|c| c.0.0.ip().to_string())
        .unwrap_or_default();
    let user = req.extensions().get::<AuthUser>().map(|u| u.0.clone());
    let limit = match api.0.info.max_message_size {
        0 => usize::MAX,
        n => usize::try_from(n).unwrap_or(usize::MAX),
    };
    let body = match axum::body::to_bytes(req.into_body(), limit).await {
        Ok(b) => b,
        Err(_) => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "http: request body too large",
            );
        }
    };
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
    };
    let (mail_from, rcpt_to, raw) = match build(&v) {
        Ok(x) => x,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let envelope = Envelope {
        mail_from,
        mail_params: Vec::new(),
        rcpt_to,
        helo: String::new(),
        remote_ip: peer,
        tls: None,
        auth_user: user,
    };
    let ingest = api.0.ingest.clone();
    match tokio::task::spawn_blocking(move || ingest.store_raw(envelope, &raw, "send_api")).await {
        Ok(Ok(id)) => json(StatusCode::OK, &Sent { id }),
        Ok(Err(reply)) => error(StatusCode::BAD_REQUEST, reply),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_forms() {
        assert_eq!(
            parse_address("a@b.test"),
            Some((String::new(), "a@b.test".into()))
        );
        assert_eq!(
            parse_address("\"A B\" <a@b.test>"),
            Some(("A B".into(), "a@b.test".into()))
        );
        assert_eq!(parse_address("not an address"), None);
        assert_eq!(parse_address("a@"), None);
    }

    #[test]
    fn builds_message_and_refuses_restricted_headers() {
        let v = serde_json::json!({
            "From": {"Email": "api@example.com", "Name": "API"},
            "To": [{"Email": "to@example.com"}, "Other <other@example.com>"],
            "Bcc": ["bcc@example.com"],
            "Subject": "سلام",
            "Text": "hi",
            "Tags": ["sent"],
            "Headers": {"X-Test": "1"},
            "Attachments": [{"Content": "data:image/png;base64,iVBORw0KGgo=", "Filename": "a.png"}]
        });
        let (from, rcpts, raw) = build(&v).expect("build");
        assert_eq!(from, "api@example.com");
        assert_eq!(
            rcpts,
            ["to@example.com", "other@example.com", "bcc@example.com"]
        );
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("X-Tags: sent\r\n"), "{text}");
        assert!(text.contains("X-Test: 1\r\n"));
        assert!(text.contains("image/png"));
        let h = spoolbox_mime::scan_headers(&raw, &spoolbox_mime::Limits::default());
        assert_eq!(h.subject, "سلام");

        let bad = serde_json::json!({"From": {"Email": "a@b.test"}, "Headers": {"Subject": "x"}});
        assert_eq!(
            build(&bad).expect_err("refused"),
            "cannot overwrite header: \"Subject\""
        );
        let bad = serde_json::json!({"From": {"Email": "a@b.test"}, "To": [{"Email": "nope"}]});
        assert_eq!(
            build(&bad).expect_err("refused"),
            "invalid To address: nope"
        );
    }
}
