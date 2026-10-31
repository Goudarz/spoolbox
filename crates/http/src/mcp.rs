//! Model Context Protocol server (`POST /mcp`, JSON-RPC 2.0, Streamable HTTP with JSON
//! responses). `spoolbox mcp` bridges stdio to this endpoint. Tools read mail, wait for new
//! mail and extract codes and links; deleting is only offered with `--mcp-allow-delete`.

use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};
use spoolbox_mime::view;
use spoolbox_store::{MessageRow, Page};

use crate::api::{Api, rfc3339};

/// Protocol revisions this server speaks, newest first.
const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const MAX_TEXT: usize = 20_000;
const MAX_RAW: usize = 200_000;

pub(crate) fn routes() -> Router<Api> {
    Router::new().route("/mcp", post(handle).get(no_stream).delete(no_stream))
}

/// This server never pushes messages, so there is no SSE stream to open.
async fn no_stream() -> Response {
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

async fn handle(State(api): State<Api>, body: Bytes) -> Response {
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return reply(error(Value::Null, -32700, &format!("parse error: {e}"))),
    };
    if let Value::Array(batch) = msg {
        let mut out = Vec::new();
        for m in batch {
            if let Some(r) = dispatch(&api, m).await {
                out.push(r);
            }
        }
        return if out.is_empty() {
            StatusCode::ACCEPTED.into_response()
        } else {
            reply(Value::Array(out))
        };
    }
    match dispatch(&api, msg).await {
        Some(r) => reply(r),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

fn reply(v: Value) -> Response {
    crate::api::json(StatusCode::OK, &v)
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Handles one JSON-RPC message; notifications (no `id`) get no response.
async fn dispatch(api: &Api, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = id else {
        // `notifications/initialized`, `notifications/cancelled`, ...: nothing to do.
        return None;
    };
    Some(match method {
        "initialize" => {
            let asked = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let version = VERSIONS
                .iter()
                .find(|v| **v == asked)
                .unwrap_or(&VERSIONS[0]);
            result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "spoolbox", "version": spoolbox_core::VERSION },
                    "instructions": "Spoolbox captures email sent by the application under test. Use wait_for_message after triggering an email (take event_cursor first to avoid races), then get_message, get_codes or get_links. Search syntax: from:, to:, subject:, tag:, is:unread, has:attachment, before:, after:, larger:, header:Name=value, plain words."
                }),
            )
        }
        "ping" => result(id, json!({})),
        "tools/list" => result(id, json!({ "tools": tools(api.0.mcp_allow_delete) })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call(api, name, &args).await {
                Ok(Some(v)) => result(id, content(&v, false)),
                Ok(None) => error(id, -32602, &format!("unknown tool {name:?}")),
                Err(e) => result(id, content(&json!({ "error": e }), true)),
            }
        }
        other => error(id, -32601, &format!("method not found: {other}")),
    })
}

/// Tool output as text (JSON) plus structured content.
fn content(v: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(v).unwrap_or_default();
    let mut out = json!({ "content": [{ "type": "text", "text": text }], "isError": is_error });
    if v.is_object() {
        out["structuredContent"] = v.clone();
    }
    out
}

fn schema(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false })
}

fn tools(allow_delete: bool) -> Vec<Value> {
    let id = json!({ "type": "string", "description": "Message ID, or \"latest\"" });
    let mut list = vec![
        json!({
            "name": "list_messages",
            "title": "List or search messages",
            "description": "Newest first. Without a query, lists all messages.",
            "inputSchema": schema(json!({
                "query": { "type": "string", "description": "Search, e.g. 'to:alice@example.com subject:welcome is:unread'" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
            }), &[]),
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "wait_for_message",
            "title": "Wait for a message",
            "description": "Blocks until a message matching the query arrives (after since_seq, or after the call), then returns its summary.",
            "inputSchema": schema(json!({
                "query": { "type": "string" },
                "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 120, "default": 15 },
                "since_seq": { "type": "integer", "description": "From event_cursor, taken before triggering the email" }
            }), &["query"]),
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "event_cursor",
            "title": "Current event position",
            "description": "Take this before triggering an email and pass it as since_seq to wait_for_message, so an email that arrives quickly is not missed.",
            "inputSchema": schema(json!({}), &[]),
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "get_message",
            "title": "Read a message",
            "description": "Headers, recipients, text body (HTML converted to text when needed), attachments and the SMTP envelope. Does not mark the message read.",
            "inputSchema": schema(json!({ "id": id }), &["id"]),
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "get_codes",
            "title": "Extract verification codes",
            "description": "Codes in the subject and bodies; by default 4-8 digit numbers, or the first capture group of `pattern`.",
            "inputSchema": schema(json!({ "id": id, "pattern": { "type": "string" } }), &["id"]),
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "get_links",
            "title": "Extract links",
            "description": "Links in document order, with their text, and whether an image is a tracking pixel.",
            "inputSchema": schema(json!({ "id": id }), &["id"]),
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "get_raw",
            "title": "Raw message source",
            "description": "The message exactly as received (truncated after 200 kB).",
            "inputSchema": schema(json!({ "id": id }), &["id"]),
            "annotations": { "readOnlyHint": true }
        }),
    ];
    if allow_delete {
        list.push(json!({
            "name": "delete_messages",
            "title": "Delete messages",
            "description": "Deletes the given IDs, or every message matching `query`. One of the two is required.",
            "inputSchema": schema(json!({
                "ids": { "type": "array", "items": { "type": "string" } },
                "query": { "type": "string" }
            }), &[]),
            "annotations": { "destructiveHint": true }
        }));
    }
    list
}

fn arg_str<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn summary(m: &MessageRow) -> Value {
    json!({
        "id": m.id,
        "seq": m.seq,
        "received_at": rfc3339(m.received_at),
        "from": m.from.addr,
        "from_name": m.from.name,
        "subject": m.subject,
        "snippet": m.snippet,
        "tags": m.tags,
        "attachments": m.attachment_count,
        "read": m.read,
        "size": m.size,
    })
}

fn truncate(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

async fn resolve(api: &Api, id: &str) -> Result<String, String> {
    if id != "latest" {
        return Ok(id.to_string());
    }
    let q = spoolbox_search::Query::default();
    api.run(move |s| s.latest(&q))
        .await
        .map_err(|_| "internal error".to_string())?
        .ok_or_else(|| "there are no messages".to_string())
}

/// Runs a tool. `Ok(None)` = unknown tool; `Err` = a tool error shown to the model.
async fn call(api: &Api, name: &str, args: &Value) -> Result<Option<Value>, String> {
    let internal = |_| "internal error".to_string();
    Ok(Some(match name {
        "list_messages" => {
            let q = spoolbox_search::parse(arg_str(args, "query").unwrap_or_default(), None);
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .clamp(1, 100) as usize;
            let page = Page {
                offset: 0,
                limit,
                before: None,
            };
            let found = api
                .run(move |s| s.search(&q, page))
                .await
                .map_err(internal)?;
            json!({
                "total": found.total,
                "messages": found.messages.iter().map(summary).collect::<Vec<_>>(),
            })
        }
        "event_cursor" => json!({ "seq": api.store().last_event_seq() }),
        "wait_for_message" => {
            let query = arg_str(args, "query")
                .ok_or("query is required")?
                .to_string();
            let secs = args
                .get("timeout_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(15)
                .clamp(1, 120);
            let since = args.get("since_seq").and_then(Value::as_i64);
            match wait(api, &query, since, Duration::from_secs(secs)).await? {
                Some(m) => json!({ "found": true, "message": summary(&m) }),
                None => {
                    json!({ "found": false, "detail": format!("no message matching {query:?} within {secs} s") })
                }
            }
        }
        "get_message" => {
            let id = resolve(api, arg_str(args, "id").ok_or("id is required")?).await?;
            let found = api
                .run(move |s| {
                    let Some(m) = s.get(&id)? else {
                        return Ok(None);
                    };
                    let raw = s.raw(&id)?;
                    Ok(Some((
                        m,
                        s.recipients(&id)?,
                        s.envelope(&id)?,
                        s.parts(&id)?,
                        raw,
                    )))
                })
                .await
                .map_err(internal)?;
            let (m, rcpts, env, parts, raw) = found.ok_or("message not found")?;
            let b = view::bodies(&raw);
            let (text, cut) = truncate(&b.text, MAX_TEXT);
            let list = |kind: &str| -> Vec<String> {
                rcpts
                    .iter()
                    .filter(|r| r.kind == kind)
                    .map(|r| {
                        if r.address.name.is_empty() {
                            r.address.addr.clone()
                        } else {
                            format!("{} <{}>", r.address.name, r.address.addr)
                        }
                    })
                    .collect()
            };
            json!({
                "message": summary(&m),
                "message_id": m.message_id,
                "to": list("to"),
                "cc": list("cc"),
                "bcc": list("bcc"),
                "reply_to": list("reply_to"),
                "envelope": env.map(|e| json!({ "mail_from": e.mail_from, "rcpt_to": e.rcpt_to, "auth_user": e.auth_user, "tls": e.tls })),
                "has_html": !b.html.is_empty(),
                "text": text,
                "text_truncated": cut,
                "attachments": parts.iter().filter(|p| p.is_attachment || p.is_inline).map(|p| json!({
                    "part_id": p.part_id, "filename": p.filename, "content_type": p.content_type, "size": p.decoded_size,
                })).collect::<Vec<_>>(),
            })
        }
        "get_codes" | "get_links" | "get_raw" => {
            let id = resolve(api, arg_str(args, "id").ok_or("id is required")?).await?;
            let found = api
                .run(move |s| {
                    Ok(match s.get(&id)? {
                        Some(m) => Some((m, s.raw(&id)?)),
                        None => None,
                    })
                })
                .await
                .map_err(internal)?;
            let (m, raw) = found.ok_or("message not found")?;
            match name {
                "get_raw" => {
                    let (text, cut) = truncate(&String::from_utf8_lossy(&raw), MAX_RAW);
                    json!({ "id": m.id, "raw": text, "truncated": cut })
                }
                "get_links" => {
                    let b = view::bodies(&raw);
                    let links = view::links(
                        (!b.html.is_empty()).then_some(b.html.as_str()),
                        (!b.text.is_empty()).then_some(b.text.as_str()),
                    );
                    json!({ "id": m.id, "links": links.iter().map(|l| json!({
                        "url": l.url, "text": l.text, "source": l.source, "tracking_pixel": l.tracking_pixel,
                    })).collect::<Vec<_>>() })
                }
                _ => {
                    let b = view::bodies(&raw);
                    let html_text =
                        (!b.html.is_empty()).then(|| spoolbox_mime::html_to_text(&b.html));
                    let codes = view::codes(
                        &m.subject,
                        (!b.text.is_empty()).then_some(b.text.as_str()),
                        html_text.as_deref(),
                        arg_str(args, "pattern"),
                    )?;
                    json!({ "id": m.id, "codes": codes.iter().map(|c| json!({ "value": c.value, "source": c.source })).collect::<Vec<_>>() })
                }
            }
        }
        "delete_messages" if api.0.mcp_allow_delete => {
            let ids: Vec<String> = args
                .get("ids")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let query = arg_str(args, "query").map(str::to_string);
            let deleted = match (ids.is_empty(), query) {
                (false, None) => api.run(move |s| s.delete(&ids)).await.map_err(internal)?,
                (true, Some(q)) => {
                    let q = spoolbox_search::parse(&q, None);
                    if q.is_empty() {
                        return Err("the query matches every message; use explicit ids".into());
                    }
                    api.run(move |s| s.delete_matching(&q))
                        .await
                        .map_err(internal)?
                }
                _ => return Err("give either ids or query".into()),
            };
            json!({ "deleted": deleted })
        }
        _ => return Ok(None),
    }))
}

async fn wait(
    api: &Api,
    query: &str,
    since: Option<i64>,
    timeout: Duration,
) -> Result<Option<MessageRow>, String> {
    let store = api.store().clone();
    let mut notify = store.subscribe();
    let cursor = since.unwrap_or_else(|| store.last_event_seq());
    let q = spoolbox_search::parse(query, None);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let q2 = q.clone();
        let hit = api
            .run(move |s| {
                let mut ids: Vec<String> = Vec::new();
                let mut after = cursor;
                loop {
                    let batch = s.events_since(after, 1000)?;
                    let Some(last) = batch.last() else { break };
                    after = last.seq;
                    for e in &batch {
                        if let Some(id) = &e.message_id
                            && e.kind.starts_with("message.")
                            && !ids.contains(id)
                        {
                            ids.push(id.clone());
                        }
                    }
                    if batch.len() < 1000 {
                        break;
                    }
                }
                Ok(match s.filter_ids(&q2, &ids)?.first() {
                    Some(id) => s.get(id)?,
                    None => None,
                })
            })
            .await
            .map_err(|_| "internal error".to_string())?;
        if hit.is_some() {
            return Ok(hit);
        }
        tokio::select! {
            r = notify.changed() => if r.is_err() { return Ok(None) },
            () = tokio::time::sleep_until(deadline) => return Ok(None),
        }
    }
}
