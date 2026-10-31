use proptest::prelude::*;

use super::*;

fn utc(q: &str) -> Query {
    parse(q, Some("UTC"))
}

fn filters(q: &str) -> Vec<(bool, Filter)> {
    utc(q)
        .terms
        .into_iter()
        .map(|t| (t.negate, t.filter))
        .collect()
}

#[test]
fn tokenizer_groups_quotes() {
    assert_eq!(
        tokenize(r#"a "b c" subject:"d e" -"f g""#),
        ["a", "b c", "subject:d e", "-f g"]
    );
    assert_eq!(tokenize("  spaced   out  "), ["spaced", "out"]);
    assert!(tokenize(r#""""#).is_empty());
}

#[test]
fn all_filters() {
    assert_eq!(
        filters(
            r#"from:Fa@Example.com to:bob cc:carol bcc:x reply-to:r addressed:any subject:"Hello World" message-id:<abc@x> tag:Invoice"#
        ),
        [
            (
                false,
                Filter::Address(AddressField::From, "fa@example.com".into())
            ),
            (false, Filter::Address(AddressField::To, "bob".into())),
            (false, Filter::Address(AddressField::Cc, "carol".into())),
            (false, Filter::Address(AddressField::Bcc, "x".into())),
            (false, Filter::Address(AddressField::ReplyTo, "r".into())),
            (false, Filter::Address(AddressField::Any, "any".into())),
            (false, Filter::Subject("Hello World".into())),
            (false, Filter::MessageId("abc@x".into())),
            (false, Filter::Tag("invoice".into())),
        ]
    );
    assert_eq!(
        filters("is:read !is:unread is:tagged has:attachments has:inline larger:1.5m smaller:2kb"),
        [
            (false, Filter::Read(true)),
            (true, Filter::Read(false)),
            (false, Filter::Tagged),
            (false, Filter::HasAttachment),
            (false, Filter::HasInline),
            (false, Filter::Larger(1_572_864)),
            (false, Filter::Smaller(2048)),
        ]
    );
}

#[test]
fn text_and_negation() {
    assert_eq!(
        filters(r#"Invoice -spam "reset your PASSWORD""#),
        [
            (false, Filter::Text("invoice".into())),
            (true, Filter::Text("spam".into())),
            (false, Filter::Text("reset your password".into())),
        ]
    );
    assert_eq!(
        filters("سلام دنیا"),
        [
            (false, Filter::Text("سلام".into())),
            (false, Filter::Text("دنیا".into()))
        ]
    );
    assert_eq!(
        filters("unknown:prefix"),
        [(false, Filter::Text("unknown:prefix".into()))],
        "unknown keys are text"
    );
    assert!(
        filters("- ! () <>").is_empty()
            || filters("- ! () <>")
                .iter()
                .all(|(_, f)| matches!(f, Filter::Text(_)))
    );
}

#[test]
fn header_filter() {
    assert_eq!(
        filters("header:X-Custom-Id=abc123"),
        [(false, Filter::Header("X-Custom-Id".into(), "abc123".into()))]
    );
    assert_eq!(
        filters(r#"header:"X-Mailer: Acme 2""#),
        [(false, Filter::Header("X-Mailer".into(), "Acme 2".into()))]
    );
    assert_eq!(utc("header:novalue").ignored, ["header:novalue"]);
}

#[test]
fn dates_and_time_zones() {
    let day = 1_790_294_400_000_000; // 2026-09-25T00:00:00Z
    assert_eq!(filters("after:2026-09-25"), [(false, Filter::After(day))]);
    assert_eq!(filters("before:2026/09/25"), [(false, Filter::Before(day))]);
    assert_eq!(
        filters("after:2026-09-25T00:00:00Z"),
        [(false, Filter::After(day))]
    );
    assert_eq!(
        filters(r#"after:"2026-09-25 01:30""#),
        [(false, Filter::After(day + 5_400_000_000))],
        "ISO datetime with space"
    );
    assert_eq!(
        parse("after:2026-09-25", Some("+03:30")).terms[0].filter,
        Filter::After(day - 12_600_000_000)
    );
    assert_eq!(
        parse("after:2026-09-25", Some("Asia/Tehran")).terms[0].filter,
        Filter::After(day - 12_600_000_000)
    );
    assert_eq!(
        utc("after:yesterday-ish").ignored,
        ["after:yesterday-ish"],
        "invalid dates are ignored"
    );
    assert!(utc("after:yesterday-ish").terms.is_empty());
}

#[test]
fn sizes() {
    assert_eq!(size_to_bytes("300"), Some(300));
    assert_eq!(size_to_bytes("2k"), Some(2048));
    assert_eq!(size_to_bytes("1.5MB"), None, "parse() lower-cases first");
    assert_eq!(size_to_bytes("1.5mb"), Some(1_572_864));
    assert_eq!(size_to_bytes("5gb"), None);
    assert_eq!(
        filters("larger:abc"),
        [(false, Filter::Text("larger:abc".into()))]
    );
}

proptest! {
    #[test]
    fn never_panics(s in ".{0,200}") {
        let _ = parse(&s, None);
        let _ = parse(&s, Some("Europe/Berlin"));
    }
}
