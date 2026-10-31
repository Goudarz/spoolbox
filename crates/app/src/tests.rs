use std::time::Duration;

use spoolbox_config::Settings;
use spoolbox_smtp::Server;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::watch;

use super::*;

struct Running {
    core: Core,
    addr: String,
    _stop: watch::Sender<bool>,
}

async fn start(mut s: Settings) -> Running {
    s.storage.min_free_disk = s.storage.min_free_disk.min(1);
    start_exact(s).await
}

async fn start_exact(s: Settings) -> Running {
    let core = Core::open(&s).expect("core");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    let (tx, rx) = watch::channel(false);
    let cfg = session_config(&s, "mx.test", false).expect("session");
    tokio::spawn(Server::new(cfg, None, core.ingest.clone(), server_options(&s)).run(listener, rx));
    Running {
        core,
        addr,
        _stop: tx,
    }
}

/// Sends one message and returns the final reply line.
async fn send(addr: &str, rcpts: &[&str], body: &str) -> String {
    let s = TcpStream::connect(addr).await.expect("connect");
    let mut io = BufReader::new(s);
    let mut lines = vec![
        "EHLO c".to_string(),
        "MAIL FROM:<sender@example.com>".into(),
    ];
    lines.extend(rcpts.iter().map(|r| format!("RCPT TO:<{r}>")));
    lines.push("DATA".into());
    async fn reply(io: &mut BufReader<TcpStream>) -> String {
        loop {
            let mut l = String::new();
            tokio::time::timeout(Duration::from_secs(5), io.read_line(&mut l))
                .await
                .expect("timeout")
                .expect("read");
            if l.as_bytes().get(3) != Some(&b'-') {
                return l.trim_end().to_string();
            }
        }
    }
    reply(&mut io).await;
    for l in &lines {
        io.get_mut()
            .write_all(format!("{l}\r\n").as_bytes())
            .await
            .expect("write");
        let r = reply(&mut io).await;
        if !(r.starts_with('2') || r.starts_with('3')) {
            return r;
        }
    }
    let data = spoolbox_smtp::data::encode(body.replace('\n', "\r\n").as_bytes());
    io.get_mut().write_all(&data).await.expect("write body");
    reply(&mut io).await
}

fn queued_id(reply: &str) -> String {
    reply.rsplit(' ').next().expect("id").to_string()
}

#[tokio::test]
async fn smtp_to_store_to_index() {
    let srv = start(Settings::default()).await;
    let body = "From: Ali <ali+signup@example.com>\nTo: user@example.com\nSubject: Welcome سلام\nX-Tags: onboarding, Invoice\n\nYour code is 482913\n";
    let reply = send(&srv.addr, &["user@example.com", "hidden@example.com"], body).await;
    assert!(reply.starts_with("250 2.0.0 Ok: queued as "), "{reply}");
    let id = queued_id(&reply);

    let store = &srv.core.store;
    let row = store.get(&id).expect("get").expect("row");
    assert_eq!(row.status, "received", "stored before indexing (ADR-005)");
    assert_eq!(row.subject, "Welcome سلام");
    assert_eq!(row.tags, ["Invoice", "onboarding", "signup"]);
    assert_eq!(
        store.envelope(&id).expect("env").expect("some").rcpt_to,
        ["user@example.com", "hidden@example.com"]
    );

    assert_eq!(srv.core.indexer.run_batch().await, 1);
    let row = store.get(&id).expect("get").expect("row");
    assert_eq!(row.status, "indexed");
    assert!(row.snippet.contains("482913"));
    assert_eq!(
        store.search_text("482913", 10).expect("search"),
        std::slice::from_ref(&id)
    );
    assert_eq!(
        store.raw(&id).expect("raw"),
        body.replace('\n', "\r\n").as_bytes(),
        "byte-exact"
    );
}

#[tokio::test]
async fn duplicate_message_ids_are_acknowledged_but_not_stored() {
    let mut s = Settings::default();
    s.storage.ignore_duplicate_ids = true;
    let srv = start(s).await;
    let body = "Message-ID: <same@test>\nSubject: dup\n\nx\n";
    assert!(
        send(&srv.addr, &["a@x.test"], body)
            .await
            .contains("queued as")
    );
    assert_eq!(
        send(&srv.addr, &["a@x.test"], body).await,
        "250 2.0.0 Ok: queued"
    );
    assert_eq!(srv.core.store.count().expect("count").0, 1);
}

#[tokio::test]
async fn strict_rfc_headers_reject_cr_cr_lf() {
    let mut s = Settings::default();
    s.smtp.strict_rfc_headers = true;
    let srv = start(s).await;
    let r = send(&srv.addr, &["a@x.test"], "Subject: bad\r\r\nX: y\n\nbody\n").await;
    assert!(r.starts_with("550 5.6.0"), "{r}");
    assert_eq!(srv.core.store.count().expect("count").0, 0);
}

#[tokio::test]
async fn low_disk_space_is_refused_with_452() {
    let mut s = Settings::default();
    s.storage.min_free_disk = u64::MAX;
    let srv = start_exact(s).await;
    assert_eq!(
        send(&srv.addr, &["a@x.test"], "Subject: x\n\nx\n").await,
        "452 4.3.1 Insufficient system storage"
    );
}

#[tokio::test]
async fn retention_by_count_and_age() {
    let mut s = Settings::default();
    s.storage.max_messages = 2;
    let srv = start(s).await;
    for i in 0..4 {
        assert!(
            send(&srv.addr, &["a@x.test"], &format!("Subject: {i}\n\nx\n"))
                .await
                .starts_with("250")
        );
    }
    assert_eq!(srv.core.store.count().expect("count").0, 2);

    tokio::time::sleep(Duration::from_millis(20)).await;
    let opts = MaintenanceOptions {
        max_age: Some(Duration::from_millis(10)),
        ..MaintenanceOptions::default()
    };
    maintenance::once(&srv.core.store, &opts);
    assert_eq!(srv.core.store.count().expect("count").0, 0);
}

#[tokio::test]
async fn recipient_policy_from_settings() {
    let mut s = Settings::default();
    s.smtp.allowed_recipients = Some(r"@allowed\.test$".into());
    let srv = start(s).await;
    assert!(
        send(&srv.addr, &["u@other.test"], "Subject: x\n\nx\n")
            .await
            .starts_with("550 5.1.0")
    );
    assert!(
        send(&srv.addr, &["u@allowed.test"], "Subject: x\n\nx\n")
            .await
            .starts_with("250")
    );
}

#[tokio::test]
async fn username_tags_and_disabled_sources() {
    let mut s = Settings::default();
    s.tags.plus_addresses = false;
    s.tags.title_case = true;
    let srv = start(s).await;
    let r = send(
        &srv.addr,
        &["a@x.test"],
        "To: bob+ignored@example.com\nX-Tags: release notes\nSubject: t\n\nx\n",
    )
    .await;
    let row = srv
        .core
        .store
        .get(&queued_id(&r))
        .expect("get")
        .expect("row");
    assert_eq!(row.tags, ["Release Notes"]);
}

#[tokio::test]
async fn tag_rules_apply_after_indexing() {
    let mut s = Settings::default();
    s.tags.rules = vec![
        spoolbox_config::TagRule {
            query: "subject:invoice".into(),
            tags: vec!["Finance".into()],
        },
        // Full-text rules need the index, which is why rules run after indexing.
        spoolbox_config::TagRule {
            query: "482913".into(),
            tags: vec!["OTP".into(), "Finance".into()],
        },
    ];
    let srv = start(s).await;
    let hit = queued_id(
        &send(
            &srv.addr,
            &["a@example.com"],
            "Subject: Your invoice\n\nCode 482913\n",
        )
        .await,
    );
    let miss = queued_id(
        &send(
            &srv.addr,
            &["a@example.com"],
            "Subject: Hello\n\nNothing here\n",
        )
        .await,
    );
    srv.core.indexer.run_batch().await;
    let store = &srv.core.store;
    assert_eq!(
        store.get(&hit).expect("get").expect("row").tags,
        ["Finance", "OTP"]
    );
    assert!(store.get(&miss).expect("get").expect("row").tags.is_empty());
}
