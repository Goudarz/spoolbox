//! Read-side helpers for the API: header maps, part contents, HTML preparation,
//! link and code extraction, List-Unsubscribe validation, thumbnails.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use mail_parser::{MessageParser, PartType};
use regex::Regex;

/// Headers as `GET /api/v1/message/{id}/headers` returns them: canonical
/// names (`Message-Id`), raw (undecoded) values, folded lines joined with a space.
pub fn headers_v1(raw: &[u8]) -> BTreeMap<String, Vec<String>> {
    let block = crate::header_block(raw, 1024 * 1024);
    let text = String::from_utf8_lossy(block);
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current: Option<(String, String)> = None;
    for line in text.split('\n').map(|l| l.trim_end_matches('\r')) {
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some((_, v)) = current.as_mut() {
                if !v.is_empty() {
                    v.push(' ');
                }
                v.push_str(line.trim());
            }
            continue;
        }
        if let Some((k, v)) = current.take() {
            out.entry(k).or_default().push(v);
        }
        if let Some((name, value)) = line.split_once(':') {
            current = Some((canonical_header(name.trim()), value.trim().to_string()));
        }
    }
    if let Some((k, v)) = current {
        out.entry(k).or_default().push(v);
    }
    out
}

/// Go's `textproto.CanonicalMIMEHeaderKey`.
pub fn canonical_header(name: &str) -> String {
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    {
        return name.to_string();
    }
    let mut upper = true;
    name.chars()
        .map(|c| {
            let out = if upper {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            };
            upper = c == '-';
            out
        })
        .collect()
}

/// Text and HTML bodies as the message view shows them: when there is no
/// text part, the text is derived from the HTML.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bodies {
    pub text: String,
    pub html: String,
}

pub fn bodies(raw: &[u8]) -> Bodies {
    let Some(msg) = MessageParser::default().parse(raw) else {
        return Bodies::default();
    };
    // mail-parser converts text to HTML when there is no HTML part; only a real one counts.
    let html = msg
        .html_body
        .iter()
        .filter_map(|i| msg.parts.get(*i as usize))
        .find_map(|p| match &p.body {
            PartType::Html(h) => Some(h.to_string()),
            _ => None,
        })
        .unwrap_or_default();
    let text = msg
        .body_text(0)
        .filter(|_| msg.text_body_count() > 0)
        .map(|t| t.into_owned())
        .unwrap_or_else(|| {
            if html.is_empty() {
                String::new()
            } else {
                mail_parser::decoders::html::html_to_text(&html)
            }
        });
    Bodies { text, html }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartContent {
    pub content_type: String,
    pub filename: Option<String>,
    pub content_id: Option<String>,
    pub data: Vec<u8>,
}

/// Decoded content of the part with IMAP-style `part_id` (same numbering as [`crate::index`]).
pub fn part_content(raw: &[u8], part_id: &str) -> Option<PartContent> {
    let msg = MessageParser::default().parse(raw)?;
    let root = msg.parts.first()?;
    let start = if matches!(root.body, PartType::Multipart(_)) {
        "0"
    } else {
        "1"
    };
    let mut stack = vec![(0u32, start.to_string(), 0usize)];
    while let Some((idx, id, depth)) = stack.pop() {
        let part = msg.parts.get(idx as usize)?;
        if id == part_id {
            let info = crate::describe(part, id, None);
            return Some(PartContent {
                content_type: info.content_type,
                filename: info.filename,
                content_id: info.content_id,
                data: part.contents().to_vec(),
            });
        }
        if depth > crate::Limits::default().max_depth {
            continue;
        }
        if let PartType::Multipart(children) = &part.body {
            for (n, child) in children.iter().enumerate().rev() {
                let child_id = if id == "0" {
                    (n + 1).to_string()
                } else {
                    format!("{id}.{}", n + 1)
                };
                stack.push((*child, child_id, depth + 1));
            }
        }
    }
    None
}

static BASE_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<base\b[^>]*>").expect("regex"));
static META_REFRESH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<meta\b[^>]*http-equiv\s*=\s*["']?\s*refresh[^>]*>"#).expect("regex")
});

/// Replaces `cid:` references with URLs, and removes `<base>` and `<meta http-equiv=refresh>`
/// (for `/view/{id}.html`).
pub fn prepare_html(html: &str, cids: &[(String, String)]) -> String {
    let mut out = META_REFRESH
        .replace_all(&BASE_TAG.replace_all(html, ""), "")
        .into_owned();
    for (cid, url) in cids {
        if cid.is_empty() {
            continue;
        }
        let pattern = format!(r#"(?i)(cid:){}(["'\s/>;)])"#, regex::escape(cid));
        if let Ok(re) = Regex::new(&pattern) {
            out = re
                .replace_all(&out, |c: &regex::Captures<'_>| format!("{url}{}", &c[2]))
                .into_owned();
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    pub text: Option<String>,
    /// `html_a`, `html_img`, `html_css` or `text`.
    pub source: &'static str,
    /// A 0–1 pixel image: probably an open-tracking beacon.
    pub tracking_pixel: bool,
}

static TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)<(a|img|link)\b([^>]*)>(?:([^<]*)</a>)?"#).expect("regex"));
static ATTR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)\b([a-z-]+)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#).expect("regex")
});
static TEXT_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)https?://[^\s<>"'()\[\]]+"#).expect("regex"));

/// Links in document order, de-duplicated, capped at 100.
pub fn links(html: Option<&str>, text: Option<&str>) -> Vec<Link> {
    let mut out: Vec<Link> = Vec::new();
    let mut push = |l: Link| {
        if out.len() < 100 && !out.iter().any(|o| o.url == l.url) {
            out.push(l);
        }
    };
    if let Some(html) = html {
        for cap in TAG.captures_iter(html) {
            let tag = cap[1].to_ascii_lowercase();
            let attrs: Vec<(String, String)> = ATTR
                .captures_iter(&cap[2])
                .map(|a| {
                    (
                        a[1].to_ascii_lowercase(),
                        a.get(2)
                            .or(a.get(3))
                            .or(a.get(4))
                            .map_or("", |m| m.as_str())
                            .to_string(),
                    )
                })
                .collect();
            let get = |k: &str| {
                attrs
                    .iter()
                    .find(|(n, _)| n == k)
                    .map(|(_, v)| decode_entities(v.trim()))
            };
            let (url, source) = match tag.as_str() {
                "a" => (get("href"), "html_a"),
                "img" => (get("src"), "html_img"),
                _ if get("rel").is_some_and(|r| r.eq_ignore_ascii_case("stylesheet")) => {
                    (get("href"), "html_css")
                }
                _ => (None, ""),
            };
            let Some(url) = url.filter(|u| {
                u.starts_with("http://") || u.starts_with("https://") || u.starts_with("mailto:")
            }) else {
                continue;
            };
            let tiny = |k: &str| {
                get(k).is_some_and(|v| {
                    v.trim_end_matches("px")
                        .trim()
                        .parse::<u32>()
                        .is_ok_and(|n| n <= 1)
                })
            };
            let text = cap
                .get(3)
                .map(|t| t.as_str().trim().to_string())
                .filter(|t| !t.is_empty());
            push(Link {
                url,
                text,
                source,
                tracking_pixel: tag == "img" && tiny("width") && tiny("height"),
            });
        }
    }
    if let Some(text) = text {
        for m in TEXT_URL.find_iter(text) {
            let url = m
                .as_str()
                .trim_end_matches(['.', ',', ';', ':', '!', '?'])
                .to_string();
            push(Link {
                url,
                text: None,
                source: "text",
                tracking_pixel: false,
            });
        }
    }
    out
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    pub value: String,
    /// `subject`, `text` or `html`.
    pub source: &'static str,
}

static DEFAULT_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^\d])(\d{4,8})(?:[^\d]|$)").expect("regex"));

/// Verification codes. `pattern` is a regular expression whose first capture group (or
/// whole match) is the code; the default finds 4–8 digit numbers. The regex engine runs in
/// linear time, so user patterns cannot cause catastrophic backtracking.
pub fn codes(
    subject: &str,
    text: Option<&str>,
    html_text: Option<&str>,
    pattern: Option<&str>,
) -> Result<Vec<Code>, String> {
    let custom;
    let re = match pattern {
        Some(p) if p.len() > 200 => return Err("pattern is longer than 200 characters".into()),
        Some(p) => {
            custom = Regex::new(p).map_err(|e| format!("invalid pattern: {e}"))?;
            &custom
        }
        None => &*DEFAULT_CODE,
    };
    let mut out: Vec<Code> = Vec::new();
    for (source, hay) in [
        ("subject", Some(subject)),
        ("text", text),
        ("html", html_text),
    ] {
        let Some(hay) = hay else { continue };
        for c in re.captures_iter(hay) {
            let value = c.get(1).or(c.get(0)).map_or("", |m| m.as_str()).to_string();
            if !value.is_empty() && !out.iter().any(|o| o.value == value) {
                out.push(Code { value, source });
            }
            if out.len() >= 20 {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListUnsubscribe {
    pub header: String,
    pub links: Vec<String>,
    /// Empty when the header is valid.
    pub errors: String,
    pub header_post: String,
}

/// Validates `List-Unsubscribe` (RFC 2369).
pub fn list_unsubscribe(header: &str, post: &str) -> ListUnsubscribe {
    let mut out = ListUnsubscribe {
        header: header.to_string(),
        header_post: post.to_string(),
        ..Default::default()
    };
    // Drop RFC 5322 comments.
    let mut v = String::new();
    let mut depth = 0usize;
    for c in header.chars() {
        match c {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => v.push(c),
            _ => {}
        }
    }
    let v = v.trim();
    let entries: Vec<&str> = v
        .split('<')
        .skip(1)
        .filter_map(|s| s.split_once('>').map(|(inside, _)| inside))
        .collect();
    if entries.is_empty() {
        out.errors = format!("\"{v}\" no valid unsubscribe links found");
        return out;
    }
    if !(v.starts_with('<') && v.ends_with('>')) {
        out.errors = format!("\"{v}\" should be enclosed in <>");
        return out;
    }
    let separators_ok = v
        .split('>')
        .skip(1)
        .filter_map(|s| s.split_once('<').map(|(between, _)| between))
        .all(|b| b.trim() == ",");
    if !separators_ok {
        out.errors = format!("\"{v}\" <> should be split with a comma and optional spaces");
        return out;
    }
    let mut errors = Vec::new();
    if entries.len() > 2 {
        errors.push(format!(
            "\"{v}\" should include a maximum of one email and one HTTP link"
        ));
    } else {
        let (mut mailto, mut http) = (false, false);
        for e in entries {
            if e.chars().any(char::is_whitespace) {
                errors.push(format!("\"{e}\" should not contain spaces"));
            } else if e.starts_with("mailto:") && e.len() > 7 {
                if mailto {
                    errors.push(format!("\"{e}\" should only contain one mailto:"));
                } else {
                    mailto = true;
                    out.links.push(e.to_string());
                }
            } else if e.to_ascii_lowercase().starts_with("http://")
                || e.to_ascii_lowercase().starts_with("https://")
            {
                if http {
                    errors.push(format!("\"{e}\" should only contain one HTTP link"));
                } else {
                    http = true;
                    out.links.push(e.to_string());
                }
            } else {
                errors.push(format!("\"{e}\" is not a valid unsubscribe link"));
            }
        }
    }
    out.errors = errors.join(", ");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_map_matches_go_canonical_form() {
        let raw = b"Message-ID: <a@b>\r\nX-CUSTOM-id: 1\r\nSubject: =?utf-8?b?2LPZhNin2YU=?=\r\n folded\r\nReceived: one\r\nReceived: two\r\n\r\nbody";
        let h = headers_v1(raw);
        assert_eq!(h["Message-Id"], ["<a@b>"]);
        assert_eq!(h["X-Custom-Id"], ["1"]);
        assert_eq!(
            h["Subject"],
            ["=?utf-8?b?2LPZhNin2YU=?= folded"],
            "raw, unfolded"
        );
        assert_eq!(h["Received"], ["one", "two"]);
        assert_eq!(canonical_header("content-TYPE"), "Content-Type");
        assert_eq!(canonical_header("bad header"), "bad header");
    }

    #[test]
    fn part_content_by_id() {
        let raw = b"Content-Type: multipart/mixed; boundary=B\r\n\r\n--B\r\nContent-Type: text/plain\r\n\r\nhello\r\n--B\r\nContent-Type: application/octet-stream\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"a.bin\"\r\n\r\nAAEC\r\n--B--\r\n";
        let p = part_content(raw, "2").expect("part");
        assert_eq!(p.data, [0, 1, 2]);
        assert_eq!(p.filename.as_deref(), Some("a.bin"));
        assert_eq!(part_content(raw, "1").expect("text").data, b"hello");
        assert!(part_content(raw, "9").is_none());
        assert_eq!(
            part_content(b"Subject: x\r\n\r\nplain", "1")
                .expect("single")
                .data,
            b"plain"
        );
    }

    #[test]
    fn html_preparation() {
        let html = r#"<html><head><base href="https://evil.test/"><META HTTP-EQUIV="Refresh" content="0;url=https://evil.test"></head><body><img src="cid:logo1"><img src='CID:logo1'>cid:logo1x</body></html>"#;
        let out = prepare_html(
            html,
            &[("logo1".into(), "/api/v1/message/X/part/1.2".into())],
        );
        assert!(!out.to_lowercase().contains("<base"));
        assert!(!out.to_lowercase().contains("refresh"));
        assert_eq!(out.matches("/api/v1/message/X/part/1.2").count(), 2);
        assert!(
            out.contains("cid:logo1x"),
            "only exact Content-IDs are replaced"
        );
    }

    #[test]
    fn link_extraction() {
        let html = r##"<a href="https://acme.example/verify?token=abc&amp;u=1">Confirm</a><img src="https://t.example/o.gif" width="1" height="1"><link rel="stylesheet" href="https://cdn.example/s.css"><a href="#top">x</a><a href="mailto:a@b">m</a>"##;
        let l = links(
            Some(html),
            Some(
                "Or visit https://acme.example/verify?token=abc&u=1. Also http://other.example/x.",
            ),
        );
        assert_eq!(
            l[0],
            Link {
                url: "https://acme.example/verify?token=abc&u=1".into(),
                text: Some("Confirm".into()),
                source: "html_a",
                tracking_pixel: false
            }
        );
        assert!(l[1].tracking_pixel);
        assert_eq!(l[2].source, "html_css");
        assert_eq!(l[3].url, "mailto:a@b");
        assert_eq!(
            l.last().map(|x| x.url.as_str()),
            Some("http://other.example/x")
        );
        assert_eq!(l.len(), 5, "duplicates and #anchors dropped: {l:?}");
    }

    #[test]
    fn code_extraction() {
        let c = codes(
            "Your code is 482913",
            Some("Code: 482913. Order 2026-10-25"),
            None,
            None,
        )
        .expect("codes");
        assert_eq!(
            c[0],
            Code {
                value: "482913".into(),
                source: "subject"
            }
        );
        assert!(c.iter().all(|x| x.value.len() >= 4));
        let c = codes(
            "",
            Some("token=ABC-123-XYZ"),
            None,
            Some(r"token=([A-Z0-9-]+)"),
        )
        .expect("custom");
        assert_eq!(
            c,
            [Code {
                value: "ABC-123-XYZ".into(),
                source: "text"
            }]
        );
        assert!(codes("", None, None, Some("(unclosed")).is_err());
        assert!(codes("", None, None, Some(&"a".repeat(201))).is_err());
    }

    #[test]
    fn list_unsubscribe_validation() {
        let ok = list_unsubscribe(
            "<mailto:unsub@example.com>, <https://example.invalid/unsub>",
            "List-Unsubscribe=One-Click",
        );
        assert_eq!(
            ok.links,
            ["mailto:unsub@example.com", "https://example.invalid/unsub"]
        );
        assert_eq!(ok.errors, "");
        assert!(
            list_unsubscribe("mailto:x@y", "")
                .errors
                .contains("no valid")
        );
        assert!(
            list_unsubscribe("<mailto:a@b> <https://x>", "")
                .errors
                .contains("comma")
        );
        assert!(
            list_unsubscribe("<mailto:a@b>, <mailto:c@d>", "")
                .errors
                .contains("one mailto")
        );
        assert!(
            list_unsubscribe("<a>, <b>, <c>", "")
                .errors
                .contains("maximum")
        );
        assert_eq!(
            list_unsubscribe("(comment) <https://x.example/u>", "").links,
            ["https://x.example/u"]
        );
    }
}
