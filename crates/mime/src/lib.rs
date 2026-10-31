//! MIME parsing for Spoolbox. Two entry points:
//! - [`scan_headers`]: cheap top-level header scan used before a message is committed;
//! - [`index`]: full MIME tree with byte offsets, checksums and search text, run as a background job.
//!
//! Untrusted input is bounded by [`Limits`]; nested `message/rfc822` parts are treated as
//! leaf attachments (their inner offsets refer to decoded bytes, not to the stored message).

use std::fmt::Write as _;

use mail_parser::{
    Addr, Address as MpAddress, HeaderValue, Message, MessageParser, MessagePart, MimeHeaders,
    PartType,
};
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Parts beyond this count are not indexed.
    pub max_parts: usize,
    pub max_depth: usize,
    /// Bytes of the plain-text preview before `...` is appended.
    pub snippet_bytes: usize,
    /// Bytes of body text kept for full-text search.
    pub max_search_body: usize,
    /// Bytes of the header block examined by [`scan_headers`].
    pub max_header_block: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_parts: 500,
            max_depth: 32,
            snippet_bytes: 200,
            max_search_body: 1024 * 1024,
            max_header_block: 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Address {
    pub name: String,
    pub addr: String,
}

/// Top-level headers, decoded (RFC 2047) where applicable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderSummary {
    pub subject: String,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    /// Unix seconds from the `Date` header.
    pub date: Option<i64>,
    /// Without angle brackets.
    pub message_id: Option<String>,
    pub x_tags: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartInfo {
    /// IMAP-style section number (`1`, `1.2`, `1.2.2`); `0` is a multipart root.
    pub part_id: String,
    pub parent: Option<String>,
    /// Lower-case `type/subtype`.
    pub content_type: String,
    pub charset: Option<String>,
    pub disposition: Option<String>,
    pub filename: Option<String>,
    /// Without angle brackets.
    pub content_id: Option<String>,
    pub content_language: Option<String>,
    pub transfer_encoding: Option<String>,
    pub offset_header: u32,
    pub offset_body: u32,
    pub offset_end: u32,
    pub decoded_size: u64,
    pub is_multipart: bool,
    pub is_attachment: bool,
    /// Inline part with a filename or Content-ID (e.g. an embedded image).
    pub is_inline: bool,
    pub sha256: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
}

/// Text used to build the full-text index, split by field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFields {
    pub subject: String,
    pub addresses: String,
    pub body: String,
    pub filenames: String,
    pub headers: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageIndex {
    pub headers: HeaderSummary,
    pub parts: Vec<PartInfo>,
    pub text: Option<String>,
    pub html: Option<String>,
    pub snippet: String,
    pub search: SearchFields,
    pub attachment_count: u32,
    pub inline_count: u32,
    /// Non-fatal problems (encoding errors, truncation) for display.
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MimeError {
    /// mail-parser could not find any message structure.
    Unparseable,
}

impl std::fmt::Display for MimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MimeError::Unparseable => f.write_str("message could not be parsed"),
        }
    }
}

impl std::error::Error for MimeError {}

/// The header block: everything up to and including the first empty line, capped at `max` bytes.
/// Plain text rendering of an HTML body (used for snippets and code extraction).
pub fn html_to_text(html: &str) -> String {
    mail_parser::decoders::html::html_to_text(html)
}

pub fn header_block(raw: &[u8], max: usize) -> &[u8] {
    let window = &raw[..raw.len().min(max)];
    let crlf = find(window, b"\r\n\r\n").map(|i| i + 4);
    let lf = find(window, b"\n\n").map(|i| i + 2);
    let end = match (crlf, lf) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => window.len(),
    };
    &raw[..end]
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

pub fn scan_headers(raw: &[u8], limits: &Limits) -> HeaderSummary {
    let block = header_block(raw, limits.max_header_block);
    MessageParser::default()
        .parse_headers(block)
        .map(|m| summarize(&m))
        .unwrap_or_default()
}

pub fn index(raw: &[u8], limits: &Limits) -> Result<MessageIndex, MimeError> {
    let msg = MessageParser::default()
        .parse(raw)
        .ok_or(MimeError::Unparseable)?;
    if msg.parts.is_empty() {
        return Err(MimeError::Unparseable);
    }
    let mut out = MessageIndex {
        headers: summarize(&msg),
        ..Default::default()
    };

    let mut walker = Walker {
        msg: &msg,
        limits,
        parts: Vec::new(),
        truncated: false,
        too_deep: false,
    };
    let root = &msg.parts[0];
    if matches!(root.body, PartType::Multipart(_)) {
        walker.visit(0, "0".into(), None, 0);
    } else {
        walker.visit(0, "1".into(), None, 0);
    }
    if walker.truncated {
        out.problems.push(format!(
            "only the first {} MIME parts were indexed",
            limits.max_parts
        ));
    }
    if walker.too_deep {
        out.problems.push(format!(
            "MIME nesting deeper than {} levels was not indexed",
            limits.max_depth
        ));
    }
    if msg.parts.iter().any(|p| p.is_encoding_problem) {
        out.problems.push(
            "one or more parts could not be decoded with their declared encoding or charset".into(),
        );
    }
    out.parts = walker.parts;

    out.text = msg
        .body_text(0)
        .filter(|_| msg.text_body_count() > 0)
        .map(|t| t.into_owned());
    out.html = msg
        .body_html(0)
        .filter(|_| msg.html_body_count() > 0)
        .map(|h| h.into_owned());
    // The HTML body wins over the text body for the snippet.
    let plain = match (&out.html, &out.text) {
        (Some(h), _) if !h.trim().is_empty() => mail_parser::decoders::html::html_to_text(h),
        (_, Some(t)) => t.clone(),
        (Some(h), None) => mail_parser::decoders::html::html_to_text(h),
        (None, None) => String::new(),
    };
    out.snippet = snippet(&plain, limits.snippet_bytes);
    // Search covers both bodies: the text part often holds content (e.g. bare URLs) that the
    // HTML part only has in attributes.
    let search_body = match (&out.text, &out.html) {
        (Some(t), Some(h)) => format!(
            "{t}
{}",
            mail_parser::decoders::html::html_to_text(h)
        ),
        _ => plain,
    };

    out.attachment_count = out.parts.iter().filter(|p| p.is_attachment).count() as u32;
    out.inline_count = out.parts.iter().filter(|p| p.is_inline).count() as u32;
    out.search = SearchFields {
        subject: out.headers.subject.clone(),
        addresses: addresses_text(&out.headers),
        body: truncate_bytes(&search_body, limits.max_search_body).to_string(),
        filenames: out
            .parts
            .iter()
            .filter_map(|p| p.filename.as_deref())
            .collect::<Vec<_>>()
            .join(" "),
        headers: header_text(&msg),
    };
    Ok(out)
}

struct Walker<'a, 'x> {
    msg: &'a Message<'x>,
    limits: &'a Limits,
    parts: Vec<PartInfo>,
    truncated: bool,
    too_deep: bool,
}

impl Walker<'_, '_> {
    fn visit(&mut self, idx: usize, part_id: String, parent: Option<String>, depth: usize) {
        if self.parts.len() >= self.limits.max_parts {
            self.truncated = true;
            return;
        }
        if depth > self.limits.max_depth {
            self.too_deep = true;
            return;
        }
        let Some(part) = self.msg.parts.get(idx) else {
            return;
        };
        let info = describe(part, part_id.clone(), parent);
        self.parts.push(info);
        if let PartType::Multipart(children) = &part.body {
            for (n, child) in children.iter().enumerate() {
                let child_id = if part_id == "0" {
                    (n + 1).to_string()
                } else {
                    format!("{part_id}.{}", n + 1)
                };
                self.visit(*child as usize, child_id, Some(part_id.clone()), depth + 1);
            }
        }
    }
}

pub(crate) fn describe(
    part: &MessagePart<'_>,
    part_id: String,
    parent: Option<String>,
) -> PartInfo {
    let ct = part.content_type();
    let content_type = match ct {
        Some(ct) => match &ct.c_subtype {
            Some(sub) => format!("{}/{}", ct.c_type, sub).to_ascii_lowercase(),
            None => ct.c_type.to_ascii_lowercase(),
        },
        None => match part.body {
            PartType::Html(_) => "text/html".into(),
            PartType::Multipart(_) => "multipart/mixed".into(),
            PartType::Message(_) => "message/rfc822".into(),
            PartType::Text(_) => "text/plain".into(),
            _ => "application/octet-stream".into(),
        },
    };
    let is_multipart = matches!(part.body, PartType::Multipart(_));
    let disposition = part
        .content_disposition()
        .map(|d| d.c_type.to_ascii_lowercase());
    let filename = part.attachment_name().map(str::to_string);
    let content_id = part.content_id().map(|c| {
        c.trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .to_string()
    });
    let language = match part.content_language() {
        HeaderValue::Text(t) => Some(t.to_string()),
        HeaderValue::TextList(l) => Some(l.join(", ")),
        _ => None,
    };
    let is_text_body = matches!(part.body, PartType::Text(_) | PartType::Html(_))
        && filename.is_none()
        && disposition.as_deref() != Some("attachment");
    // A part is listed when it has a filename or a Content-ID.
    let is_inline = !is_multipart
        && !is_text_body
        && disposition.as_deref() != Some("attachment")
        && content_id.is_some();
    let is_attachment = !is_multipart
        && !is_text_body
        && !is_inline
        && (filename.is_some()
            || content_id.is_some()
            || matches!(part.body, PartType::Message(_)));

    let (sha256, md5, sha1) = if is_multipart {
        (None, None, None)
    } else {
        let data = part.contents();
        (
            Some(hex(&Sha256::digest(data))),
            Some(hex(&Md5::digest(data))),
            Some(hex(&Sha1::digest(data))),
        )
    };
    PartInfo {
        part_id,
        parent,
        content_type,
        charset: ct.and_then(|c| c.attribute("charset")).map(str::to_string),
        disposition,
        filename,
        content_id,
        content_language: language,
        transfer_encoding: part
            .content_transfer_encoding()
            .map(|e| e.to_ascii_lowercase()),
        offset_header: part.offset_header,
        offset_body: part.offset_body,
        offset_end: part.offset_end,
        decoded_size: if is_multipart {
            0
        } else {
            part.contents().len() as u64
        },
        is_multipart,
        is_attachment,
        is_inline,
        sha256,
        md5,
        sha1,
    }
}

fn summarize(m: &Message<'_>) -> HeaderSummary {
    HeaderSummary {
        subject: m.subject().unwrap_or_default().to_string(),
        from: m.from().and_then(|a| a.first()).map(address),
        to: list(m.to()),
        cc: list(m.cc()),
        bcc: list(m.bcc()),
        reply_to: list(m.reply_to()),
        date: m.date().map(|d| d.to_timestamp()),
        message_id: m.message_id().map(str::to_string),
        x_tags: m
            .header_raw("X-Tags")
            .map(|v| {
                v.split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn address(a: &Addr<'_>) -> Address {
    Address {
        name: a.name.as_deref().unwrap_or_default().trim().to_string(),
        addr: a.address.as_deref().unwrap_or_default().trim().to_string(),
    }
}

fn list(a: Option<&MpAddress<'_>>) -> Vec<Address> {
    a.map(|a| a.iter().map(address).collect())
        .unwrap_or_default()
}

fn addresses_text(h: &HeaderSummary) -> String {
    let mut s = String::new();
    for a in h
        .from
        .iter()
        .chain(&h.to)
        .chain(&h.cc)
        .chain(&h.bcc)
        .chain(&h.reply_to)
    {
        let _ = write!(s, "{} {} ", a.name, a.addr);
    }
    s.trim_end().to_string()
}

/// `Name: value` lines of headers not covered by dedicated fields, capped at 8 KiB.
fn header_text(m: &Message<'_>) -> String {
    const SKIP: [&str; 8] = [
        "subject",
        "from",
        "to",
        "cc",
        "bcc",
        "reply-to",
        "received",
        "dkim-signature",
    ];
    let mut s = String::new();
    for (name, value) in m.headers_raw() {
        if SKIP.iter().any(|k| name.eq_ignore_ascii_case(k)) {
            continue;
        }
        let _ = writeln!(s, "{name}: {}", value.trim());
        if s.len() > 8192 {
            break;
        }
    }
    truncate_bytes(&s, 8192).to_string()
}

fn snippet(text: &str, max_bytes: usize) -> String {
    let collapsed = text
        .replace('\u{feff}', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.len() <= max_bytes {
        return collapsed;
    }
    format!("{}...", truncate_bytes(&collapsed, max_bytes))
}

fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

pub mod view;

#[cfg(test)]
mod tests;
