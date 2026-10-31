use proptest::prelude::*;

use super::*;

const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// Same structure as the snapshot message `fixture-html`:
/// mixed[ alternative[ text, related[ html, image(cid) ] ], attachment ].
fn persian_html() -> String {
    format!(
        "From: =?utf-8?b?2YHYsdiz2KrZhtiv2Ycg2KLYstmF2KfbjNi024w=?= <fa@example.com>\r\n\
To: bob+signup@example.com, =?utf-8?b?2qnYp9ix2KjYsQ==?= <user@example.com>\r\n\
Cc: carol@example.com\r\n\
Reply-To: reply@example.com\r\n\
Subject: =?utf-8?b?2LPZgdin2LHYtCDYtNmF2Kcg2KvYqNiqINi02K8=?=\r\n\
Message-ID: <html-1@fixtures.test>\r\n\
Date: Fri, 23 Oct 2026 10:00:00 +0000\r\n\
X-Tags: Invoice, urgent\r\n\
X-Custom-Id: abc123\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"MIX\"\r\n\
\r\n\
--MIX\r\n\
Content-Type: multipart/alternative; boundary=\"ALT\"\r\n\
\r\n\
--ALT\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: 8bit\r\n\
\r\n\
متن ساده پیام. کد تأیید: 482913\r\n\
--ALT\r\n\
Content-Type: multipart/related; boundary=\"REL\"\r\n\
\r\n\
--REL\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body dir=\"rtl\"><p>سلام <b>دنیا</b></p><img src=\"cid:logo1\"></body></html>\r\n\
--REL\r\n\
Content-Type: image/png\r\n\
Content-Transfer-Encoding: base64\r\n\
Content-ID: <logo1>\r\n\
Content-Disposition: inline; filename=\"logo.png\"\r\n\
\r\n\
{PNG}\r\n\
--REL--\r\n\
--ALT--\r\n\
--MIX\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Disposition: attachment; filename*=UTF-8''%DA%AF%D8%B2%D8%A7%D8%B1%D8%B4.txt\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
2LPZhNin2YU=\r\n\
--MIX--\r\n"
    )
}

#[test]
fn indexes_structure_with_imap_part_ids() {
    let raw = persian_html();
    let ix = index(raw.as_bytes(), &Limits::default()).expect("index");
    let ids: Vec<(&str, &str)> = ix
        .parts
        .iter()
        .map(|p| (p.part_id.as_str(), p.content_type.as_str()))
        .collect();
    assert_eq!(
        ids,
        [
            ("0", "multipart/mixed"),
            ("1", "multipart/alternative"),
            ("1.1", "text/plain"),
            ("1.2", "multipart/related"),
            ("1.2.1", "text/html"),
            ("1.2.2", "image/png"),
            ("2", "text/plain"),
        ]
    );
    let img = &ix.parts[5];
    assert!(img.is_inline && !img.is_attachment);
    assert_eq!(img.content_id.as_deref(), Some("logo1"));
    assert_eq!(img.decoded_size, 70);
    let att = &ix.parts[6];
    assert!(att.is_attachment);
    assert_eq!(
        att.filename.as_deref(),
        Some("گزارش.txt"),
        "RFC 2231 filename"
    );
    assert_eq!(att.decoded_size, "سلام".len() as u64);
    assert_eq!(ix.attachment_count, 1);
    assert_eq!(ix.inline_count, 1);
    // sha256("سلام" as UTF-8): checksums are over the decoded content.
    assert_eq!(
        att.sha256.as_deref(),
        Some("bda1fa48345336618741fd2c4bc02809eb099c49a9b02fb5056401ab6d4dc3e6")
    );
    assert_eq!(att.md5.as_ref().map(String::len), Some(32));
    assert_eq!(att.sha1.as_ref().map(String::len), Some(40));
}

#[test]
fn offsets_point_into_the_raw_message() {
    let raw = persian_html();
    let ix = index(raw.as_bytes(), &Limits::default()).expect("index");
    let html = ix
        .parts
        .iter()
        .find(|p| p.content_type == "text/html")
        .expect("html part");
    let body = &raw.as_bytes()[html.offset_body as usize..html.offset_end as usize];
    assert!(String::from_utf8_lossy(body).contains("cid:logo1"));
    assert!(html.offset_header < html.offset_body && html.offset_body <= html.offset_end);
}

#[test]
fn decodes_headers_and_bodies() {
    let ix = index(persian_html().as_bytes(), &Limits::default()).expect("index");
    let h = &ix.headers;
    assert_eq!(h.subject, "سفارش شما ثبت شد");
    assert_eq!(
        h.from,
        Some(Address {
            name: "فرستنده آزمایشی".into(),
            addr: "fa@example.com".into()
        })
    );
    assert_eq!(h.to.len(), 2);
    assert_eq!(h.to[1].name, "کاربر");
    assert_eq!(h.message_id.as_deref(), Some("html-1@fixtures.test"));
    assert_eq!(h.date, Some(1_792_749_600));
    assert_eq!(h.x_tags, ["Invoice", "urgent"]);
    assert!(ix.text.as_deref().unwrap_or_default().contains("482913"));
    assert!(
        ix.html
            .as_deref()
            .unwrap_or_default()
            .contains("<b>دنیا</b>")
    );
    assert!(
        ix.snippet.starts_with("سلام دنیا"),
        "HTML wins over text: {}",
        ix.snippet
    );
    assert!(ix.search.addresses.contains("bob+signup@example.com"));
    assert!(ix.search.filenames.contains("گزارش.txt"));
    assert!(
        ix.search.headers.contains("X-Custom-Id: abc123"),
        "{}",
        ix.search.headers
    );
    assert!(!ix.search.headers.contains("Subject:"));
}

#[test]
fn scan_headers_matches_full_index_and_ignores_body() {
    let raw = persian_html();
    let quick = scan_headers(raw.as_bytes(), &Limits::default());
    let full = index(raw.as_bytes(), &Limits::default())
        .expect("index")
        .headers;
    assert_eq!(quick, full);
    assert_eq!(
        header_block(raw.as_bytes(), usize::MAX).len(),
        raw.find("\r\n\r\n").expect("blank") + 4
    );
}

#[test]
fn header_block_handles_lf_only_and_caps() {
    assert_eq!(
        header_block(b"A: 1\nB: 2\n\nbody", usize::MAX),
        b"A: 1\nB: 2\n\n"
    );
    assert_eq!(
        header_block(b"A: 1\r\n\r\nbody\n\nx", usize::MAX),
        b"A: 1\r\n\r\n"
    );
    assert_eq!(header_block(b"A: 1\r\nB: 2", 4), b"A: 1");
}

#[test]
fn plain_single_part_message() {
    let raw = b"From: a@example.com\r\nTo: b@example.com\r\nSubject: hi\r\n\r\nHello   world\r\nline two\r\n";
    let ix = index(raw, &Limits::default()).expect("index");
    assert_eq!(ix.parts.len(), 1);
    assert_eq!(ix.parts[0].part_id, "1");
    assert_eq!(ix.parts[0].content_type, "text/plain");
    assert_eq!(ix.snippet, "Hello world line two");
    assert_eq!(ix.attachment_count, 0);
}

#[test]
fn html_only_message_gets_text_snippet() {
    let raw =
        b"Subject: h\r\nContent-Type: text/html\r\n\r\n<p>Hi <b>there</b></p><script>x()</script>";
    let ix = index(raw, &Limits::default()).expect("index");
    assert!(ix.snippet.starts_with("Hi there"), "{}", ix.snippet);
    assert!(ix.text.is_none() || ix.html.is_some());
}

#[test]
fn legacy_charsets_are_decoded() {
    // "سلام" in windows-1256 is d3 e1 c7 e3.
    let raw = b"Subject: =?windows-1256?B?0+HH4w==?=\r\nContent-Type: text/plain; charset=windows-1256\r\n\r\n\xd3\xe1\xc7\xe3\r\n";
    let ix = index(raw, &Limits::default()).expect("index");
    assert_eq!(ix.headers.subject, "سلام");
    assert!(
        ix.text.as_deref().unwrap_or_default().contains("سلام"),
        "{:?}",
        ix.text
    );
}

#[test]
fn part_limit_is_enforced() {
    let mut raw =
        String::from("Subject: many\r\nContent-Type: multipart/mixed; boundary=B\r\n\r\n");
    for i in 0..600 {
        raw.push_str(&format!("--B\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=f{i}.txt\r\n\r\nx\r\n"));
    }
    raw.push_str("--B--\r\n");
    let ix = index(raw.as_bytes(), &Limits::default()).expect("index");
    assert_eq!(ix.parts.len(), 500);
    assert!(ix.problems.iter().any(|p| p.contains("500")));
}

#[test]
fn depth_limit_is_enforced() {
    let limits = Limits {
        max_depth: 5,
        ..Limits::default()
    };
    let mut raw = String::from("Subject: deep\r\n");
    for d in 0..10 {
        raw.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=B{d}\r\n\r\n--B{d}\r\n"
        ));
    }
    raw.push_str("Content-Type: text/plain\r\n\r\nleaf\r\n");
    for d in (0..10).rev() {
        raw.push_str(&format!("--B{d}--\r\n"));
    }
    let ix = index(raw.as_bytes(), &limits).expect("index");
    assert!(
        ix.problems.iter().any(|p| p.contains("nesting")),
        "{:?}",
        ix.problems
    );
    assert!(ix.parts.len() <= 6);
}

#[test]
fn empty_input_is_unparseable() {
    assert_eq!(index(b"", &Limits::default()), Err(MimeError::Unparseable));
}

#[test]
fn malformed_inputs_do_not_fail() {
    for raw in [
        &b"no headers at all, just text"[..],
        b"Content-Type: multipart/mixed; boundary=X\r\n\r\n--X\r\nunterminated",
        b"Subject: a\r\nSubject: b\r\n\r\nbody",
        b"Content-Transfer-Encoding: base64\r\n\r\n!!!not base64!!!",
        b"From: \r\nTo: ,,,\r\nDate: not a date\r\n\r\n",
        b"Subject: \r\r\n\r\nbody",
    ] {
        let _ = index(raw, &Limits::default());
        let _ = scan_headers(raw, &Limits::default());
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn never_panics_on_arbitrary_bytes(raw in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = index(&raw, &Limits::default());
        let _ = scan_headers(&raw, &Limits::default());
    }

    #[test]
    fn offsets_are_always_in_bounds(body in "[ -~\r\n]{0,512}") {
        let raw = format!("Subject: p\r\nContent-Type: multipart/mixed; boundary=Q\r\n\r\n--Q\r\n\r\n{body}\r\n--Q--\r\n");
        if let Ok(ix) = index(raw.as_bytes(), &Limits::default()) {
            for p in ix.parts {
                prop_assert!(p.offset_header as usize <= raw.len());
                prop_assert!(p.offset_end as usize <= raw.len());
            }
        }
    }
}

#[test]
fn snippet_is_cut_at_200_bytes_on_a_char_boundary() {
    let body = "س".repeat(150);
    let raw = format!("Subject: s\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n");
    let ix = index(raw.as_bytes(), &Limits::default()).expect("index");
    assert!(ix.snippet.ends_with("..."));
    assert_eq!(ix.snippet.len(), 200 + 3, "100 two-byte characters + ...");
}

#[test]
fn search_body_includes_text_and_html_parts() {
    let raw = b"Content-Type: multipart/alternative; boundary=A\r\n\r\n--A\r\nContent-Type: text/plain\r\n\r\nvisit https://x.test/?token=abc123\r\n--A\r\nContent-Type: text/html\r\n\r\n<p><a href=\"https://x.test/?token=abc123\">Confirm</a></p>\r\n--A--\r\n";
    let ix = index(raw, &Limits::default()).expect("index");
    assert!(ix.search.body.contains("abc123"), "{}", ix.search.body);
    assert!(ix.search.body.contains("Confirm"));
    assert_eq!(ix.snippet, "Confirm", "snippet still prefers HTML");
}
