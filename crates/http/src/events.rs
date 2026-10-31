//! v1 WebSocket notifications (`/api/events`), produced from the store's event log.
//! Message types: `new` (a message summary), `update` (`{ID, Read}` or `{ID, Tags}`),
//! `delete` (`{ID}`), `truncate`, `prune` (reload), and `stats` (at most four per second).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use serde_json::{Value, json};
use spoolbox_store::Event;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::api::Api;
use crate::v1::summaries;

const STATS_INTERVAL: Duration = Duration::from_millis(250);

fn note(kind: &str, data: Value) -> Arc<str> {
    json!({ "Type": kind, "Data": data }).to_string().into()
}

/// Translates new store events into WebSocket notifications until `stop` is set.
pub fn spawn_bridge(api: Api, mut stop: watch::Receiver<bool>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let store = api.store().clone();
        let mut notify = store.subscribe();
        let mut cursor = store.last_event_seq();
        let mut stats_due: Option<Instant> = None;
        loop {
            let wait_stats = async {
                match stats_due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                changed = notify.changed() => if changed.is_err() { break },
                _ = stop.changed() => break,
                () = wait_stats => {
                    stats_due = None;
                    send_stats(&api).await;
                    continue;
                }
            }
            if api.0.ws.receiver_count() == 0 {
                cursor = store.last_event_seq();
                continue;
            }
            loop {
                let after = cursor;
                let batch = api.run(move |s| s.events_since(after, 500)).await;
                let Ok(batch) = batch else { break };
                let Some(last) = batch.last() else { break };
                cursor = last.seq;
                let full = batch.len() == 500;
                publish(&api, batch).await;
                stats_due.get_or_insert_with(|| Instant::now() + STATS_INTERVAL);
                if !full {
                    break;
                }
            }
        }
    })
}

async fn send_stats(api: &Api) {
    if api.0.ws.receiver_count() == 0 {
        return;
    }
    if let Ok((total, unread)) = api.run(|s| s.count()).await {
        let _ = api.0.ws.send(note(
            "stats",
            json!({ "Total": total, "Unread": unread, "Version": format!("v{}", spoolbox_core::VERSION) }),
        ));
    }
}

async fn publish(api: &Api, events: Vec<Event>) {
    let ws = &api.0.ws;
    for e in events {
        let data: Value = serde_json::from_str(&e.data).unwrap_or(Value::Null);
        let id = e.message_id.clone().unwrap_or_default();
        match e.kind.as_str() {
            "message.updated" if data.get("status").is_some() => {
                // A message is announced once it is indexed (attachments and snippet known).
                let lookup = id.clone();
                let summary = api
                    .run(move |s| match s.get(&lookup)? {
                        Some(row) => Ok(summaries(s, vec![row], false)?.pop()),
                        None => Ok(None),
                    })
                    .await;
                if let Ok(Some(summary)) = summary
                    && let Ok(v) = serde_json::to_value(summary)
                {
                    let _ = ws.send(note("new", v));
                }
            }
            "message.updated" if data.get("read").is_some() => {
                let _ = ws.send(note("update", json!({ "ID": id, "Read": data["read"] })));
            }
            "message.updated" if data.get("tags").is_some() => {
                let _ = ws.send(note("update", json!({ "ID": id, "Tags": data["tags"] })));
            }
            "message.deleted" => {
                let _ = ws.send(note("delete", json!({ "ID": id })));
            }
            "mailbox.truncated" => {
                let _ = ws.send(note("truncate", Value::Null));
            }
            "tags.changed" | "messages.updated" => {
                let _ = ws.send(note("prune", Value::Null));
            }
            _ => {}
        }
    }
}

pub(crate) async fn websocket(State(api): State<Api>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| client(api, socket))
}

async fn client(api: Api, mut socket: WebSocket) {
    let mut rx = api.0.ws.subscribe();
    send_stats(&api).await;
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(text) => {
                    if socket.send(Message::Text(text.as_ref().into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Too slow to keep up: ask the UI to reload rather than miss changes.
                    if socket.send(Message::Text(note("prune", Value::Null).as_ref().into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            },
        }
    }
}
