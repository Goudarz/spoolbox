//! Instance information, web UI configuration, chaos, swagger and CORS preflight.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Serialize;
use serde_json::Value;
use spoolbox_core::chaos::{Trigger, Triggers};

use super::{body_json, field};
use crate::api::{Api, bad_request, json};

pub(super) fn routes() -> Router<Api> {
    Router::new()
        .route("/api/v1/info", get(info))
        .route("/api/v1/webui", get(webui))
        .route("/api/v1/chaos", get(get_chaos).put(set_chaos))
        .route("/api/v1/swagger.json", get(swagger))
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct RuntimeStats {
    uptime: u64,
    memory: u64,
    messages_deleted: u64,
    #[serde(rename = "SMTPAccepted")]
    smtp_accepted: u64,
    #[serde(rename = "SMTPAcceptedSize")]
    smtp_accepted_size: u64,
    #[serde(rename = "SMTPRejected")]
    smtp_rejected: u64,
    #[serde(rename = "SMTPIgnored")]
    smtp_ignored: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct AppInformation {
    version: String,
    latest_version: &'static str,
    database: String,
    database_size: u64,
    messages: u64,
    unread: u64,
    tags: BTreeMap<String, u64>,
    runtime_stats: RuntimeStats,
}

/// Resident memory of this process in bytes, where the platform makes it cheap to read.
fn memory() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(statm) = std::fs::read_to_string("/proc/self/statm")
            && let Some(pages) = statm
                .split_whitespace()
                .nth(1)
                .and_then(|p| p.parse::<u64>().ok())
        {
            return pages * 4096;
        }
    }
    0
}

/// No request is made to the internet: `LatestVersion` is always `disabled`.
async fn info(State(api): State<Api>) -> Response {
    let result = api
        .run(|s| {
            let (messages, unread) = s.count()?;
            Ok((
                s.db_path().display().to_string(),
                s.database_size()?,
                messages,
                unread,
                s.tag_counts()?,
            ))
        })
        .await;
    let (database, database_size, messages, unread, tags) = match result {
        Ok(v) => v,
        Err(r) => return r,
    };
    let st = api.0.ingest.stats();
    json(
        StatusCode::OK,
        &AppInformation {
            version: format!("v{}", spoolbox_core::VERSION),
            latest_version: "disabled",
            database,
            database_size,
            messages,
            unread,
            tags: tags.into_iter().collect(),
            runtime_stats: RuntimeStats {
                uptime: api.0.started.elapsed().as_secs(),
                memory: memory(),
                messages_deleted: api.0.deleted.load(Ordering::Relaxed),
                smtp_accepted: st.accepted.load(Ordering::Relaxed),
                smtp_accepted_size: st.accepted_size.load(Ordering::Relaxed),
                smtp_rejected: st.rejected.load(Ordering::Relaxed),
                smtp_ignored: st.ignored.load(Ordering::Relaxed),
            },
        },
    )
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct MessageRelay {
    enabled: bool,
    #[serde(rename = "SMTPServer")]
    smtp_server: String,
    return_path: String,
    allowed_recipients: String,
    blocked_recipients: String,
    override_from: String,
    #[serde(rename = "PreserveMessageIDs")]
    preserve_message_ids: bool,
    recipient_allowlist: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct WebUiConfiguration {
    label: String,
    message_relay: MessageRelay,
    spam_assassin: bool,
    chaos_enabled: bool,
    duplicates_ignored: bool,
    hide_delete_all_button: bool,
}

/// What the web UI shows about the relay; never the credentials.
fn message_relay(api: &Api) -> MessageRelay {
    let settings = api.0.relay.as_ref().map(|r| r.settings());
    let target = settings.and_then(|s| s.target.as_ref());
    let allowed = settings
        .and_then(|s| s.allowed_recipients.clone())
        .unwrap_or_default();
    MessageRelay {
        enabled: target.is_some(),
        smtp_server: target.map_or(String::new(), |t| format!("{}:{}", t.host, t.port)),
        return_path: target
            .and_then(|t| t.return_path.clone())
            .unwrap_or_default(),
        allowed_recipients: allowed.clone(),
        blocked_recipients: settings
            .and_then(|s| s.blocked_recipients.clone())
            .unwrap_or_default(),
        override_from: target
            .and_then(|t| t.override_from.clone())
            .unwrap_or_default(),
        preserve_message_ids: target.is_some_and(|t| t.preserve_message_ids),
        recipient_allowlist: allowed,
    }
}

async fn webui(State(api): State<Api>) -> Response {
    json(
        StatusCode::OK,
        &WebUiConfiguration {
            label: api.0.info.label.clone(),
            message_relay: message_relay(&api),
            spam_assassin: false,
            chaos_enabled: api.0.chaos.is_some(),
            duplicates_ignored: api.0.info.duplicates_ignored,
            hide_delete_all_button: api.0.info.hide_delete_all_button,
        },
    )
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct TriggerJson {
    error_code: u16,
    probability: u8,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct TriggersJson {
    sender: TriggerJson,
    recipient: TriggerJson,
    authentication: TriggerJson,
}

fn triggers_json(t: Triggers) -> TriggersJson {
    let one = |t: Trigger| TriggerJson {
        error_code: t.error_code,
        probability: t.probability,
    };
    TriggersJson {
        sender: one(t.sender),
        recipient: one(t.recipient),
        authentication: one(t.authentication),
    }
}

async fn get_chaos(State(api): State<Api>) -> Response {
    match &api.0.chaos {
        Some(c) => json(StatusCode::OK, &triggers_json(c.triggers())),
        None => bad_request("Chaos is not enabled"),
    }
}

/// Omitted triggers are reset (probability 0, default code).
async fn set_chaos(State(api): State<Api>, body: Bytes) -> Response {
    let Some(chaos) = &api.0.chaos else {
        return bad_request("Chaos is not enabled");
    };
    let v = match body_json(&body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mut t = Triggers::default();
    for (key, default_code) in [("Sender", 451), ("Recipient", 451), ("Authentication", 535)] {
        let entry = field(&v, key);
        let num = |name: &str| {
            entry
                .and_then(|e| field(e, name))
                .and_then(Value::as_i64)
                .unwrap_or(0)
        };
        let code = match num("ErrorCode") {
            0 => default_code,
            c => c,
        };
        let prob = num("Probability");
        let (Ok(code), Ok(prob)) = (u16::try_from(code), u8::try_from(prob)) else {
            return bad_request(
                "error code must be between 400 and 599, probability between 0 and 100",
            );
        };
        if let Err(e) = t.set(key, code, prob) {
            return bad_request(e);
        }
    }
    chaos.set(t);
    json(StatusCode::OK, &triggers_json(t))
}

static SWAGGER: &str = include_str!("../../../../api/openapi-v1.json");

async fn swagger() -> Response {
    ([(header::CONTENT_TYPE, "application/json")], SWAGGER).into_response()
}
