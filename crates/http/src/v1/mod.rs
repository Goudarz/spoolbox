//! API v1 (ADR-010). JSON field names, status codes and error texts are stable;
//! see `api/openapi-v1.json` and `docs/spec/02-compatibility.md`.

mod message;
mod misc;
mod send;
mod thumb;
pub(crate) mod types;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, put};
use serde_json::Value;
use spoolbox_search::Query as SearchQuery;
use spoolbox_store::{MessageRow, Page, Store, StoreError};

use crate::api::{Api, Params, bad_request, json, ok_text};
pub(crate) use message::{view_html, view_text};
use types::{MessagesSummary, Summary};

pub(crate) fn routes() -> Router<Api> {
    Router::new()
        .route(
            "/api/v1/messages",
            get(list).put(set_read).delete(delete_messages),
        )
        .route("/api/v1/search", get(search).delete(delete_search))
        .route("/api/v1/tags", get(all_tags).put(set_tags))
        .route("/api/v1/tags/{tag}", put(rename_tag).delete(delete_tag))
        .merge(message::routes())
        .merge(misc::routes())
        .merge(send::routes())
}

/// Field lookup with Go's case-insensitive JSON matching.
pub(crate) fn field<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    let obj = v.as_object()?;
    obj.get(name).or_else(|| {
        obj.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    })
}

pub(crate) fn strings(v: Option<&Value>) -> Result<Vec<String>, String> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_str().map(str::to_string).ok_or_else(|| {
                    "json: cannot unmarshal into Go value of type string".to_string()
                })
            })
            .collect(),
        Some(_) => Err("json: cannot unmarshal into Go value of type []string".into()),
    }
}

/// Parses a JSON request body (errors: 400 with the decoder message).
pub(crate) fn body_json(body: &Bytes) -> Result<Value, Response> {
    serde_json::from_slice::<Value>(body).map_err(|e| bad_request(e.to_string()))
}

/// Paging: invalid values fall back to the defaults (start 0, limit 50).
fn page(p: &Params) -> Page {
    let offset = p
        .get("start")
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(0) as usize;
    let limit = p
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|n| *n >= 0)
        .unwrap_or(50) as usize;
    let before = p.get("before").filter(|b| !b.is_empty()).and_then(|b| {
        let zone = spoolbox_search::resolve_zone(p.get("tz").map(String::as_str));
        let ts = spoolbox_search::parse_date(b, &zone);
        if ts.is_none() {
            tracing::warn!("ignoring invalid before: date {b:?}");
        }
        ts
    });
    Page {
        offset,
        limit,
        before,
    }
}

fn query(p: &Params) -> Option<SearchQuery> {
    let q = p.get("query").map(|s| s.trim()).unwrap_or_default();
    (!q.is_empty()).then(|| spoolbox_search::parse(q, p.get("tz").map(String::as_str)))
}

/// Builds list entries: recipients and AUTH user names are loaded for the whole page at once.
pub(crate) fn summaries(
    store: &Store,
    rows: Vec<MessageRow>,
    search: bool,
) -> Result<Vec<Summary>, StoreError> {
    let seqs: Vec<i64> = rows.iter().map(|m| m.seq).collect();
    let mut rcpts = store.recipients_by_seq(&seqs)?;
    let users = store.auth_users_by_seq(&seqs)?;
    Ok(rows
        .into_iter()
        .map(|m| {
            let r = rcpts.remove(&m.seq).unwrap_or_default();
            let list = types::Recipients::from(&r);
            Summary::new(
                &m,
                &list,
                users.get(&m.seq).cloned().unwrap_or_default(),
                search,
            )
        })
        .collect())
}

struct Stats {
    total: u64,
    unread: u64,
    tags: Vec<String>,
}

fn stats(store: &Store) -> Result<Stats, StoreError> {
    let (total, unread) = store.count()?;
    Ok(Stats {
        total,
        unread,
        tags: store.tags()?,
    })
}

async fn list(State(api): State<Api>, Query(p): Query<Params>) -> Response {
    let page = page(&p);
    let result = api
        .run(move |s| {
            let rows = s.search(&SearchQuery::default(), page)?.messages;
            Ok((summaries(s, rows, false)?, stats(s)?))
        })
        .await;
    match result {
        Ok((messages, st)) => json(
            StatusCode::OK,
            &MessagesSummary {
                total: st.total,
                unread: st.unread,
                count: messages.len() as u64,
                messages_count: st.total,
                messages_unread: st.unread,
                start: page.offset,
                tags: st.tags,
                messages,
            },
        ),
        Err(r) => r,
    }
}

async fn search(State(api): State<Api>, Query(p): Query<Params>) -> Response {
    let Some(q) = query(&p) else {
        return bad_request("Error: no search query");
    };
    let page = page(&p);
    let result = api
        .run(move |s| {
            let found = s.search(&q, page)?;
            let unread = s.count_matching(&q, true, page.before)?;
            Ok((
                found.total,
                unread,
                summaries(s, found.messages, true)?,
                stats(s)?,
            ))
        })
        .await;
    match result {
        Ok((matched, matched_unread, messages, st)) => json(
            StatusCode::OK,
            &MessagesSummary {
                total: st.total,
                unread: st.unread,
                count: messages.len() as u64,
                messages_count: matched,
                messages_unread: matched_unread,
                start: page.offset,
                tags: st.tags,
                messages,
            },
        ),
        Err(r) => r,
    }
}

async fn delete_search(State(api): State<Api>, Query(p): Query<Params>) -> Response {
    let Some(q) = query(&p) else {
        return bad_request("Error: no search query");
    };
    match api.run(move |s| s.delete_matching(&q)).await {
        Ok(n) => {
            count_deleted(&api, n);
            ok_text()
        }
        Err(r) => r,
    }
}

async fn set_read(State(api): State<Api>, Query(p): Query<Params>, body: Bytes) -> Response {
    let v = match body_json(&body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let read = field(&v, "Read").and_then(Value::as_bool).unwrap_or(false);
    let ids = match strings(field(&v, "IDs")) {
        Ok(ids) => ids,
        Err(e) => return bad_request(e),
    };
    let search = field(&v, "Search")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !ids.is_empty() && !search.is_empty() {
        return bad_request("You may specify either IDs or a search query, not both");
    }
    let tz = p.get("tz").cloned();
    let result = api
        .run(move |s| {
            if !search.is_empty() {
                s.set_read_matching(&spoolbox_search::parse(&search, tz.as_deref()), read)
            } else if ids.is_empty() {
                s.set_read_all(read)
            } else {
                s.set_read(&ids, read)
            }
        })
        .await;
    match result {
        Ok(_) => ok_text(),
        Err(r) => r,
    }
}

/// A body that is not valid JSON is rejected with 400
/// instead of deleting every message. An empty body or `{}` still deletes everything.
async fn delete_messages(State(api): State<Api>, body: Bytes) -> Response {
    let ids = if body.iter().all(u8::is_ascii_whitespace) {
        Vec::new()
    } else {
        let v = match body_json(&body) {
            Ok(v) => v,
            Err(r) => return r,
        };
        match strings(field(&v, "IDs")) {
            Ok(ids) => ids,
            Err(e) => return bad_request(e),
        }
    };
    let result = api
        .run(move |s| {
            if ids.is_empty() {
                s.delete_all()
            } else {
                s.delete(&ids)
            }
        })
        .await;
    match result {
        Ok(n) => {
            count_deleted(&api, n);
            ok_text()
        }
        Err(r) => r,
    }
}

async fn all_tags(State(api): State<Api>) -> Response {
    match api.run(|s| s.tags()).await {
        Ok(tags) => json(StatusCode::OK, &tags),
        Err(r) => r,
    }
}

/// Replaces the tags of the given messages. Tags may use any script.
async fn set_tags(State(api): State<Api>, body: Bytes) -> Response {
    let v = match body_json(&body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let (tags, ids) = match (strings(field(&v, "Tags")), strings(field(&v, "IDs"))) {
        (Ok(t), Ok(i)) => (t, i),
        (Err(e), _) | (_, Err(e)) => return bad_request(e),
    };
    let result = api
        .run(move |s| {
            for id in &ids {
                s.set_tags(id, &tags)?;
            }
            Ok(())
        })
        .await;
    match result {
        Ok(()) => ok_text(),
        Err(r) => r,
    }
}

async fn rename_tag(State(api): State<Api>, Path(tag): Path<String>, body: Bytes) -> Response {
    let v = match body_json(&body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let name = field(&v, "Name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if spoolbox_store::clean_tag(&name).is_none() {
        return bad_request("invalid tag name");
    }
    match api.run(move |s| s.rename_tag(&tag, &name)).await {
        Ok(true) => ok_text(),
        Ok(false) => bad_request("tag not found"),
        Err(r) => r,
    }
}

async fn delete_tag(State(api): State<Api>, Path(tag): Path<String>) -> Response {
    match api.run(move |s| s.delete_tag(&tag)).await {
        Ok(_) => ok_text(),
        Err(r) => r,
    }
}

fn count_deleted(api: &Api, n: usize) {
    api.0
        .deleted
        .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
}

/// Resolves `latest` (optionally filtered by `?query=`) to a message ID.
pub(crate) async fn resolve(api: &Api, id: String, p: &Params) -> Result<String, Response> {
    if id != "latest" {
        return Ok(id);
    }
    let q = query(p).unwrap_or_default();
    match api.run(move |s| s.latest(&q)).await? {
        Some(id) => Ok(id),
        None => Err(crate::api::text(StatusCode::NOT_FOUND, "Message not found")),
    }
}
