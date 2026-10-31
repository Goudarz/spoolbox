use std::io::Write;
use std::time::Duration;

use spoolbox_core::Envelope;
use spoolbox_mime::{Limits, index, scan_headers};

use super::*;

fn temp_store(max: u64) -> Store {
    Store::open(StoreOptions {
        max_messages: max,
        ..StoreOptions::default()
    })
    .expect("open")
}

fn envelope(rcpts: &[&str]) -> Envelope {
    Envelope {
        mail_from: "sender@example.com".into(),
        rcpt_to: rcpts.iter().map(|s| s.to_string()).collect(),
        helo: "client.test".into(),
        remote_ip: "127.0.0.1".into(),
        ..Default::default()
    }
}

fn put(store: &Store, raw: &[u8], rcpts: &[&str], tags: &[(&str, TagSource)]) -> Inserted {
    let mut w = store.spool().expect("spool");
    w.write_all(raw).expect("write");
    let spooled = w.finish().expect("finish");
    let headers = scan_headers(&spooled.head, &Limits::default());
    store
        .insert_message(NewMessage {
            envelope: envelope(rcpts),
            source: "smtp",
            headers,
            spooled,
            tags: tags.iter().map(|(t, s)| (t.to_string(), *s)).collect(),
        })
        .expect("insert")
}

fn run_index_jobs(store: &Store) -> usize {
    let jobs = store
        .claim_jobs("index", 100, Duration::from_secs(30))
        .expect("claim");
    for job in &jobs {
        let seq = job.message_seq.expect("seq");
        let raw = store.raw_for_job(seq).expect("raw");
        match index(&raw, &Limits::default()) {
            Ok(ix) => store.complete_index(job, seq, &ix).expect("complete"),
            Err(e) => store.fail_index(job, seq, &e.to_string()).expect("fail"),
        }
    }
    jobs.len()
}

const PERSIAN: &[u8] = "From: =?utf-8?b?2YHYsdiz2KrZhtiv2Yc=?= <fa@example.com>\r\nTo: user+signup@example.com\r\nSubject: =?utf-8?b?2LPZgdin2LHYtCDYtNmF2Kcg2KvYqNiqINi02K8=?=\r\nMessage-ID: <fa-1@test>\r\nX-Custom-Id: abc123\r\nX-Tags: Invoice\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nسلام دنیا، کد شما 482913 است\r\n".as_bytes();

#[test]
fn sqlite_supports_contentless_delete() {
    let v: Vec<u32> = Store::sqlite_version()
        .split('.')
        .filter_map(|p| p.parse().ok())
        .collect();
    assert!(
        v >= vec![3, 43],
        "SQLite {} is too old for contentless_delete",
        Store::sqlite_version()
    );
}

#[test]
fn raw_round_trip_is_byte_exact_for_every_codec() {
    for compression in [0, 1, 3] {
        let store = Store::open(StoreOptions {
            compression,
            ..StoreOptions::default()
        })
        .expect("open");
        let raw = b"Subject: x\r\n\r\nline\nbare lf and \r bare cr\r\n\x00\xff binary\r\n";
        let ins = put(&store, raw, &["a@example.com"], &[]);
        assert_eq!(
            store.raw(&ins.id).expect("raw"),
            raw,
            "compression {compression}"
        );
        let row = store.get(&ins.id).expect("get").expect("row");
        assert_eq!(row.size, raw.len() as u64);
        assert_eq!(row.raw_sha256.len(), 64);
        assert_eq!(row.status, "received");
    }
}

#[test]
fn envelope_is_stored_separately_from_headers() {
    let store = temp_store(0);
    let raw = b"From: a@example.com\r\nTo: to@example.com\r\nBcc: existing@example.com\r\nSubject: s\r\n\r\nbody\r\n";
    let ins = put(
        &store,
        raw,
        &[
            "to@example.com",
            "existing@example.com",
            "missing@example.com",
        ],
        &[],
    );
    let env = store.envelope(&ins.id).expect("env").expect("some");
    assert_eq!(
        env.rcpt_to,
        [
            "to@example.com",
            "existing@example.com",
            "missing@example.com"
        ]
    );
    assert_eq!(env.mail_from, "sender@example.com");
    let recips = store.recipients(&ins.id).expect("recipients");
    let bcc: Vec<&str> = recips
        .iter()
        .filter(|r| r.kind == "bcc")
        .map(|r| r.address.addr.as_str())
        .collect();
    assert_eq!(bcc, ["existing@example.com"], "headers are not rewritten");
    assert!(
        recips
            .iter()
            .any(|r| r.kind == "envelope" && r.address.addr == "missing@example.com"),
        "envelope-only recipient is kept"
    );
    assert_eq!(store.raw(&ins.id).expect("raw"), raw, "no headers injected");
}

#[test]
fn unicode_tags_are_kept_and_deduplicated() {
    let store = temp_store(0);
    let ins = put(
        &store,
        b"Subject: t\r\n\r\nx",
        &["a@x.test"],
        &[
            ("فارسی", TagSource::Manual),
            ("Invoice", TagSource::XTags),
            ("invoice", TagSource::Plus),
            ("  bad\u{7}  ", TagSource::Rule),
        ],
    );
    let row = store.get(&ins.id).expect("get").expect("row");
    assert_eq!(row.tags, ["bad", "Invoice", "فارسی"]);
    assert_eq!(
        ingest::clean_tag(&"x".repeat(150)).map(|t| t.chars().count()),
        Some(100)
    );
    assert_eq!(ingest::clean_tag(" \u{1} "), None);
}

#[test]
fn events_are_ordered_and_notified() {
    let store = temp_store(0);
    let mut rx = store.subscribe();
    let start = store.last_event_seq();
    let a = put(&store, b"Subject: a\r\n\r\nx", &["a@x.test"], &[]);
    assert!(rx.has_changed().expect("watch"));
    let b = put(&store, b"Subject: b\r\n\r\nx", &["a@x.test"], &[]);
    let ev = store.events_since(start, 100).expect("events");
    let kinds: Vec<(&str, Option<&str>)> = ev
        .iter()
        .map(|e| (e.kind.as_str(), e.message_id.as_deref()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("message.received", Some(a.id.as_str())),
            ("message.received", Some(b.id.as_str()))
        ]
    );
    assert!(ev[0].seq < ev[1].seq);
    assert_eq!(*rx.borrow_and_update(), ev[1].seq);
    store
        .set_read(std::slice::from_ref(&a.id), true)
        .expect("read");
    store
        .set_read(std::slice::from_ref(&a.id), true)
        .expect("read again");
    let more = store.events_since(ev[1].seq, 100).expect("events");
    assert_eq!(more.len(), 1, "unchanged read state produces no event");
    assert_eq!(more[0].kind, "message.updated");
}

#[test]
fn index_job_fills_parts_snippet_and_full_text() {
    let store = temp_store(0);
    let ins = put(
        &store,
        PERSIAN,
        &["user+signup@example.com"],
        &[("Invoice", TagSource::XTags)],
    );
    assert_eq!(store.pending_jobs().expect("jobs"), 1);
    assert_eq!(run_index_jobs(&store), 1);
    assert_eq!(store.pending_jobs().expect("jobs"), 0);

    let row = store.get(&ins.id).expect("get").expect("row");
    assert_eq!(row.status, "indexed");
    assert_eq!(row.subject, "سفارش شما ثبت شد");
    assert_eq!(row.from.name, "فرستنده");
    assert!(row.snippet.starts_with("سلام دنیا"));
    assert_eq!(store.parts(&ins.id).expect("parts").len(), 1);

    for q in [
        "دنیا",
        "ثبت",
        "482913",
        "abc123",
        "SIGNUP",
        "Signup@Example",
    ] {
        assert_eq!(
            store.search_text(q, 10).expect("search"),
            std::slice::from_ref(&ins.id),
            "query {q:?}"
        );
    }
    assert!(
        store
            .search_text("nothing-like-this", 10)
            .expect("search")
            .is_empty()
    );
    assert_eq!(
        store.search_text("شد", 10).expect("short"),
        std::slice::from_ref(&ins.id),
        "2-char fallback"
    );
}

#[test]
fn unparseable_message_keeps_raw_and_is_marked() {
    let store = temp_store(0);
    let ins = put(&store, b"", &["a@x.test"], &[]);
    run_index_jobs(&store);
    let row = store.get(&ins.id).expect("get").expect("row");
    assert_eq!(row.status, "parse_failed");
    assert!(row.parse_error.is_some());
    assert_eq!(store.raw(&ins.id).expect("raw"), b"");
}

#[test]
fn claimed_jobs_survive_a_crash() {
    let dir = tempfile::tempdir().expect("dir");
    let db = dir.path().join("crash.db");
    let opts = StoreOptions {
        database: Some(db.clone()),
        ..StoreOptions::default()
    };
    {
        let store = Store::open(opts.clone()).expect("open");
        put(&store, PERSIAN, &["a@x.test"], &[]);
        let jobs = store
            .claim_jobs("index", 10, Duration::from_secs(3600))
            .expect("claim");
        assert_eq!(jobs.len(), 1);
        assert!(
            store
                .claim_jobs("index", 10, Duration::from_secs(3600))
                .expect("claim")
                .is_empty(),
            "leased"
        );
        // Process "dies" here without completing the job.
    }
    let store = Store::open(opts).expect("reopen");
    assert_eq!(run_index_jobs(&store), 1, "job released on restart");
    assert_eq!(store.list(10, None).expect("list")[0].status, "indexed");
}

#[test]
fn reopen_keeps_data_and_rejects_tampered_schema() {
    let dir = tempfile::tempdir().expect("dir");
    let db = dir.path().join("a.db");
    let opts = StoreOptions {
        database: Some(db.clone()),
        ..StoreOptions::default()
    };
    let id = {
        let store = Store::open(opts.clone()).expect("open");
        put(&store, b"Subject: keep\r\n\r\nx", &["a@x.test"], &[]).id
    };
    {
        let store = Store::open(opts.clone()).expect("reopen");
        assert!(store.get(&id).expect("get").is_some());
        assert!(!store.is_temporary());
    }
    {
        let c = rusqlite::Connection::open(&db).expect("raw open");
        c.execute(
            "UPDATE schema_migrations SET checksum = 'x' WHERE version = 1",
            [],
        )
        .expect("tamper");
    }
    let err = Store::open(opts).expect_err("tampered");
    assert!(err.to_string().contains("migration 1"), "{err}");
}

#[test]
fn temporary_database_is_removed_on_drop() {
    let store = temp_store(0);
    let path = store.db_path().to_path_buf();
    assert!(path.exists());
    drop(store);
    assert!(!path.exists());
}

#[test]
fn max_messages_prunes_oldest_in_the_same_transaction() {
    let store = temp_store(3);
    let ids: Vec<String> = (0..5)
        .map(|i| {
            put(
                &store,
                format!("Subject: m{i}\r\n\r\nunique{i}word").as_bytes(),
                &["a@x.test"],
                &[],
            )
            .id
        })
        .collect();
    run_index_jobs(&store);
    let listed: Vec<String> = store
        .list(10, None)
        .expect("list")
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(listed, [ids[4].clone(), ids[3].clone(), ids[2].clone()]);
    assert_eq!(store.count().expect("count"), (3, 3));
    assert!(store.get(&ids[0]).expect("get").is_none());
    let pruned = store
        .events_since(0, 100)
        .expect("events")
        .into_iter()
        .filter(|e| e.kind == "messages.pruned")
        .count();
    assert_eq!(pruned, 2);
}

#[test]
fn delete_cascades_and_cleans_tags_and_index() {
    let store = temp_store(0);
    let a = put(
        &store,
        PERSIAN,
        &["a@x.test"],
        &[("only-on-a", TagSource::Manual)],
    );
    let b = put(
        &store,
        b"Subject: other\r\n\r\nkeepme",
        &["a@x.test"],
        &[("shared", TagSource::Manual)],
    );
    run_index_jobs(&store);
    assert_eq!(
        store
            .delete(&[a.id.clone(), "missing".into()])
            .expect("delete"),
        1
    );
    assert!(store.raw(&a.id).is_err());
    assert!(store.parts(&a.id).expect("parts").is_empty());
    assert!(store.recipients(&a.id).expect("recipients").is_empty());
    assert!(
        store.search_text("دنیا", 10).expect("search").is_empty(),
        "full-text row removed"
    );
    assert_eq!(
        store.tags().expect("tags"),
        ["shared"],
        "orphan tag removed"
    );
    assert!(
        store
            .events_since(0, 100)
            .expect("events")
            .iter()
            .any(|e| e.kind == "message.deleted" && e.message_id.as_deref() == Some(a.id.as_str()))
    );

    assert_eq!(store.delete_all().expect("all"), 1);
    assert_eq!(store.count().expect("count"), (0, 0));
    assert!(store.tags().expect("tags").is_empty());
    assert!(store.search_text("keepme", 10).expect("search").is_empty());
    let _ = b;
}

#[test]
fn prune_older_than_cutoff() {
    let store = temp_store(0);
    put(&store, b"Subject: old\r\n\r\nx", &["a@x.test"], &[]);
    let cutoff = now_micros() + 1;
    std::thread::sleep(Duration::from_millis(2));
    let keep = put(&store, b"Subject: new\r\n\r\nx", &["a@x.test"], &[]);
    assert_eq!(store.prune_older_than(cutoff).expect("prune"), 1);
    assert_eq!(
        store
            .list(10, None)
            .expect("list")
            .iter()
            .map(|m| m.id.clone())
            .collect::<Vec<_>>(),
        [keep.id]
    );
}

#[test]
fn keyset_pagination() {
    let store = temp_store(0);
    let ids: Vec<String> = (0..5)
        .map(|i| {
            put(
                &store,
                format!("Subject: {i}\r\n\r\nx").as_bytes(),
                &["a@x.test"],
                &[],
            )
            .id
        })
        .collect();
    let page1 = store.list(2, None).expect("p1");
    let page2 = store.list(2, Some(page1[1].seq)).expect("p2");
    let page3 = store.list(2, Some(page2[1].seq)).expect("p3");
    let all: Vec<String> = page1
        .iter()
        .chain(&page2)
        .chain(&page3)
        .map(|m| m.id.clone())
        .collect();
    assert_eq!(all, ids.iter().rev().cloned().collect::<Vec<_>>());
}

#[test]
fn duplicate_message_id_lookup() {
    let store = temp_store(0);
    put(&store, PERSIAN, &["a@x.test"], &[]);
    assert!(store.message_id_exists("fa-1@test").expect("exists"));
    assert!(!store.message_id_exists("other@test").expect("exists"));
}

#[test]
fn incremental_vacuum_after_delete() {
    let store = temp_store(0);
    let big = vec![b'x'; 2 * 1024 * 1024];
    let mut raw = b"Subject: big\r\n\r\n".to_vec();
    raw.extend_from_slice(&big);
    let store_uncompressed = Store::open(StoreOptions {
        compression: 0,
        max_messages: 0,
        ..StoreOptions::default()
    })
    .expect("open");
    let id = put(&store_uncompressed, &raw, &["a@x.test"], &[]).id;
    store_uncompressed.delete(&[id]).expect("delete");
    assert!(store_uncompressed.free_ratio().expect("ratio") > 0.5);
    store_uncompressed
        .incremental_vacuum(10_000)
        .expect("vacuum");
    assert!(store_uncompressed.free_ratio().expect("ratio") < 0.1);
    let _ = store;
}
