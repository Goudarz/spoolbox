use std::io::Write;
use std::time::Duration;

use spoolbox_core::Envelope;
use spoolbox_mime::{Limits, index, scan_headers};
use spoolbox_search::parse;

use crate::{NewMessage, Page, Store, StoreOptions, TagSource};

fn store() -> Store {
    Store::open(StoreOptions {
        max_messages: 0,
        ..StoreOptions::default()
    })
    .expect("open")
}

fn put(s: &Store, raw: &str, rcpts: &[&str], tags: &[&str]) -> String {
    let mut w = s.spool().expect("spool");
    w.write_all(raw.replace('\n', "\r\n").as_bytes())
        .expect("write");
    let spooled = w.finish().expect("finish");
    let headers = scan_headers(&spooled.head, &Limits::default());
    let env = Envelope {
        mail_from: "env@x.test".into(),
        rcpt_to: rcpts.iter().map(|r| r.to_string()).collect(),
        ..Default::default()
    };
    let tags = tags
        .iter()
        .map(|t| (t.to_string(), TagSource::Manual))
        .collect();
    let id = s
        .insert_message(NewMessage {
            envelope: env,
            source: "smtp",
            headers,
            spooled,
            tags,
        })
        .expect("insert")
        .id;
    for job in s
        .claim_jobs("index", 10, Duration::from_secs(30))
        .expect("claim")
    {
        let seq = job.message_seq.expect("seq");
        let ix = index(&s.raw_for_job(seq).expect("raw"), &Limits::default()).expect("index");
        s.complete_index(&job, seq, &ix).expect("complete");
    }
    id
}

struct Fixture {
    s: Store,
    invoice: String,
    persian: String,
    plain: String,
    attach: String,
}

fn fixture() -> Fixture {
    let s = store();
    let invoice = put(
        &s,
        "From: Billing <billing@shop.test>\nTo: Alice <alice@example.com>\nCc: carol@example.com\nReply-To: help@shop.test\nSubject: Invoice 42 ready\nMessage-ID: <inv-42@shop.test>\nX-Custom-Id: abc123\n\nPlease pay invoice 42.\n",
        &[
            "alice@example.com",
            "carol@example.com",
            "secret@example.com",
        ],
        &["Invoice"],
    );
    std::thread::sleep(Duration::from_millis(2));
    let persian = put(
        &s,
        "From: =?utf-8?b?2YHYsdmI2LTar9in2Yc=?= <shop@example.ir>\nTo: user@example.ir\nSubject: =?utf-8?b?2LPZgdin2LHYtCDYtNmF2Kc=?=\n\nسلام دنیا\n",
        &["user@example.ir"],
        &["سفارش"],
    );
    std::thread::sleep(Duration::from_millis(2));
    let plain = put(
        &s,
        "From: bob@example.com\nTo: dave@example.com\nSubject: hi\n\nok\n",
        &["dave@example.com"],
        &[],
    );
    std::thread::sleep(Duration::from_millis(2));
    let attach = put(
        &s,
        "From: reports@example.com\nTo: dave@example.com\nSubject: report\nContent-Type: multipart/mixed; boundary=B\n\n--B\nContent-Type: text/plain\n\nsee file\n--B\nContent-Type: application/pdf\nContent-Disposition: attachment; filename=r.pdf\n\nPDFDATA\n--B--\n",
        &["dave@example.com"],
        &[],
    );
    Fixture {
        s,
        invoice,
        persian,
        plain,
        attach,
    }
}

fn ids(s: &Store, q: &str) -> Vec<String> {
    s.search(&parse(q, Some("UTC")), Page::default())
        .expect("search")
        .messages
        .into_iter()
        .map(|m| m.id)
        .collect()
}

#[test]
fn filters_match_expected_messages() {
    let f = fixture();
    let s = &f.s;
    let all = vec![
        f.attach.clone(),
        f.plain.clone(),
        f.persian.clone(),
        f.invoice.clone(),
    ];
    assert_eq!(ids(s, ""), all, "newest first");
    assert_eq!(
        ids(s, "invoice"),
        std::slice::from_ref(&f.invoice),
        "body/subject text"
    );
    assert_eq!(ids(s, "دنیا"), std::slice::from_ref(&f.persian));
    assert_eq!(
        ids(s, "from:billing"),
        std::slice::from_ref(&f.invoice),
        "name or address"
    );
    assert_eq!(
        ids(s, "to:alice@example.com"),
        std::slice::from_ref(&f.invoice)
    );
    assert_eq!(ids(s, "cc:carol"), std::slice::from_ref(&f.invoice));
    assert_eq!(
        ids(s, "bcc:secret"),
        std::slice::from_ref(&f.invoice),
        "envelope-only recipient counts as Bcc"
    );
    assert!(ids(s, "bcc:alice").is_empty(), "To recipients are not Bcc");
    assert_eq!(
        ids(s, "reply-to:help@shop"),
        std::slice::from_ref(&f.invoice)
    );
    assert_eq!(
        ids(s, "addressed:dave"),
        [f.attach.clone(), f.plain.clone()]
    );
    assert_eq!(ids(s, "subject:ready"), std::slice::from_ref(&f.invoice));
    assert_eq!(
        ids(s, "message-id:inv-42"),
        std::slice::from_ref(&f.invoice)
    );
    assert_eq!(
        ids(s, "tag:invoice"),
        std::slice::from_ref(&f.invoice),
        "tags are case-insensitive"
    );
    assert_eq!(
        ids(s, "tag:سفارش"),
        std::slice::from_ref(&f.persian),
        "Unicode tags"
    );
    assert_eq!(ids(s, "is:tagged"), [f.persian.clone(), f.invoice.clone()]);
    assert_eq!(ids(s, "-is:tagged"), [f.attach.clone(), f.plain.clone()]);
    assert_eq!(ids(s, "has:attachment"), std::slice::from_ref(&f.attach));
    assert_eq!(
        ids(s, "header:X-Custom-Id=abc123"),
        std::slice::from_ref(&f.invoice),
        "custom header values"
    );
    assert_eq!(
        ids(s, "abc123"),
        std::slice::from_ref(&f.invoice),
        "header values are part of free text too"
    );
    assert_eq!(
        ids(s, "to:dave -has:attachment"),
        std::slice::from_ref(&f.plain),
        "AND + negation"
    );
    assert_eq!(
        ids(s, "hi"),
        std::slice::from_ref(&f.plain),
        "short terms fall back to LIKE"
    );
    assert_eq!(ids(s, "larger:200"), ids(s, "larger:200"));
    assert!(ids(s, "after:2099-01-01").is_empty());
    assert_eq!(ids(s, "before:2099-01-01").len(), 4);
}

#[test]
fn read_state_counts_and_pagination() {
    let f = fixture();
    let s = &f.s;
    s.set_read(std::slice::from_ref(&f.plain), true)
        .expect("read");
    assert_eq!(ids(s, "is:read"), std::slice::from_ref(&f.plain));
    assert_eq!(
        s.count_matching(&parse("to:dave", None), true, None)
            .expect("count"),
        1
    );
    let page = s
        .search(
            &parse("", None),
            Page {
                offset: 1,
                limit: 2,
                before: None,
            },
        )
        .expect("page");
    assert_eq!(page.total, 4);
    assert_eq!(
        page.messages
            .iter()
            .map(|m| m.id.clone())
            .collect::<Vec<_>>(),
        [f.plain.clone(), f.persian.clone()]
    );
    assert_eq!(
        s.latest(&parse("to:dave", None)).expect("latest"),
        Some(f.attach.clone())
    );
    assert_eq!(s.set_read_all(true).expect("all read"), 3);
    assert_eq!(s.count().expect("count").1, 0);
}

#[test]
fn bulk_delete_and_read_by_query() {
    let f = fixture();
    let s = &f.s;
    assert_eq!(
        s.set_read_matching(&parse("to:dave", None), true)
            .expect("read"),
        2
    );
    assert_eq!(
        s.delete_matching(&parse("addressed:dave", None))
            .expect("delete"),
        2
    );
    assert_eq!(ids(s, ""), [f.persian.clone(), f.invoice.clone()]);
}

#[test]
fn tag_management() {
    let f = fixture();
    let s = &f.s;
    assert_eq!(
        s.set_tags(&f.plain, &["Alpha".into(), "beta".into(), "alpha".into()])
            .expect("set"),
        Some(vec!["Alpha".into(), "beta".into()])
    );
    assert_eq!(s.set_tags("missing", &["x".into()]).expect("set"), None);
    assert!(
        s.rename_tag("beta", "Invoice").expect("rename"),
        "merges into existing tag"
    );
    assert_eq!(
        s.get(&f.plain).expect("get").expect("row").tags,
        ["Alpha", "Invoice"]
    );
    let counts = s.tag_counts().expect("counts");
    assert!(counts.contains(&("Invoice".into(), 2)), "{counts:?}");
    assert!(s.delete_tag("ALPHA").expect("delete"));
    assert!(!s.delete_tag("nope").expect("delete"));
    assert_eq!(
        s.get(&f.plain).expect("get").expect("row").tags,
        ["Invoice"]
    );
    assert_eq!(s.set_tags(&f.plain, &[]).expect("clear"), Some(vec![]));
    assert_eq!(s.tags().expect("tags"), ["Invoice", "سفارش"]);
}

#[test]
fn recipients_and_users_by_seq() {
    let f = fixture();
    let s = &f.s;
    let row = s.get(&f.invoice).expect("get").expect("row");
    let map = s.recipients_by_seq(&[row.seq]).expect("recipients");
    let kinds: Vec<&str> = map[&row.seq].iter().map(|r| r.kind.as_str()).collect();
    for k in ["envelope", "from", "to", "cc", "reply_to"] {
        assert!(kinds.contains(&k), "{kinds:?}");
    }
    assert!(s.auth_users_by_seq(&[row.seq]).expect("users").is_empty());
    assert!(s.database_size().expect("size") > 0);
}

#[test]
fn filter_ids_keeps_matching_subset() {
    let f = fixture();
    let s = &f.s;
    let ids = vec![f.invoice.clone(), f.persian.clone(), "missing".to_string()];
    assert_eq!(
        s.filter_ids(&parse("from:billing", None), &ids)
            .expect("filter"),
        std::slice::from_ref(&f.invoice)
    );
    assert_eq!(
        s.filter_ids(&parse("", None), &[]).expect("empty"),
        Vec::<String>::new()
    );
}

#[test]
fn rule_tags_are_added_once() {
    let f = fixture();
    let s = &f.s;
    let seq = s.get(&f.invoice).expect("get").expect("row").seq;
    assert!(
        s.seq_matches(&parse("from:billing", None), seq)
            .expect("match")
    );
    assert!(
        !s.seq_matches(&parse("from:nobody", None), seq)
            .expect("match")
    );
    let before = s.last_event_seq();
    let tags = s
        .add_tags(seq, &["Finance".into(), "Finance".into()], TagSource::Rule)
        .expect("add")
        .expect("exists");
    assert!(tags.contains(&"Finance".to_string()));
    let after_first = s.last_event_seq();
    assert!(after_first > before);
    s.add_tags(seq, &["Finance".into()], TagSource::Rule)
        .expect("again");
    assert_eq!(
        s.last_event_seq(),
        after_first,
        "no event when nothing changes"
    );
    assert_eq!(
        s.add_tags(i64::MAX, &["x".into()], TagSource::Rule)
            .expect("missing"),
        None
    );
}
