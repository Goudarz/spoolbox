//! Search query language
//! (`internal/storage/search.go`) plus `header:Name=value`.
//!
//! Terms are separated by whitespace and combined with AND. Double quotes group
//! words (`"reset password"`, `subject:"two words"`). A leading `-` or `!` negates
//! a term. Unknown `key:` prefixes are searched as plain text.

use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressField {
    From,
    To,
    Cc,
    Bcc,
    ReplyTo,
    /// Any of the above (`addressed:`).
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// Substring of subject, addresses, body, attachment names or other headers.
    Text(String),
    Address(AddressField, String),
    Subject(String),
    MessageId(String),
    /// Exact tag name, case-insensitive.
    Tag(String),
    Read(bool),
    Tagged,
    HasAttachment,
    HasInline,
    /// Unix microseconds, inclusive.
    After(i64),
    Before(i64),
    /// Bytes, exclusive.
    Larger(u64),
    Smaller(u64),
    /// Header name (case-insensitive) and value substring.
    Header(String, String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub negate: bool,
    pub filter: Filter,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub terms: Vec<Term>,
    /// Terms that were ignored (e.g. an unparseable date), for diagnostics.
    pub ignored: Vec<String>,
}

impl Query {
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
}

/// Parses a query. `tz` (IANA name or `+03:30`) applies to dates without an explicit
/// offset; by default the system time zone is used.
pub fn parse(input: &str, tz: Option<&str>) -> Query {
    let zone = resolve_zone(tz);
    let mut q = Query::default();
    for token in tokenize(input) {
        let (negate, body) = match token.strip_prefix(['-', '!']) {
            Some(rest) if !rest.is_empty() => (true, rest.to_string()),
            _ => (false, token.clone()),
        };
        let lower = body.to_lowercase();
        let value = |prefix: &str| clean(&body[prefix.len()..]);
        let filter = if let Some(f) = address_prefix(&lower) {
            let v = value(f.1);
            if v.is_empty() {
                continue;
            }
            Filter::Address(f.0, v)
        } else if lower.starts_with("subject:") {
            // Subject values are not cleaned.
            let v = body["subject:".len()..].to_string();
            if v.is_empty() {
                continue;
            }
            Filter::Subject(v)
        } else if lower.starts_with("message-id:") {
            Filter::MessageId(value("message-id:"))
        } else if lower.starts_with("tag:") {
            Filter::Tag(value("tag:"))
        } else if lower.starts_with("header:") {
            let rest = &body["header:".len()..];
            match rest.split_once(['=', ':']) {
                Some((name, v)) if !name.trim().is_empty() => {
                    Filter::Header(name.trim().to_string(), v.trim().to_string())
                }
                _ => {
                    q.ignored.push(token);
                    continue;
                }
            }
        } else if lower == "is:read" {
            Filter::Read(true)
        } else if lower == "is:unread" {
            Filter::Read(false)
        } else if lower == "is:tagged" {
            Filter::Tagged
        } else if lower == "has:attachment" || lower == "has:attachments" {
            Filter::HasAttachment
        } else if lower == "has:inline" || lower == "has:inlines" {
            Filter::HasInline
        } else if let Some(rest) = lower.strip_prefix("after:") {
            match parse_date(rest, &zone) {
                Some(ts) => Filter::After(ts),
                None => {
                    q.ignored.push(token);
                    continue;
                }
            }
        } else if let Some(rest) = lower.strip_prefix("before:") {
            match parse_date(rest, &zone) {
                Some(ts) => Filter::Before(ts),
                None => {
                    q.ignored.push(token);
                    continue;
                }
            }
        } else if let Some(n) = lower.strip_prefix("larger:").and_then(size_to_bytes) {
            Filter::Larger(n)
        } else if let Some(n) = lower.strip_prefix("smaller:").and_then(size_to_bytes) {
            Filter::Smaller(n)
        } else {
            let v = clean(&body);
            if v.is_empty() {
                continue;
            }
            Filter::Text(v)
        };
        if matches!(&filter, Filter::MessageId(v) | Filter::Tag(v) if v.is_empty()) {
            continue;
        }
        q.terms.push(Term { negate, filter });
    }
    q
}

fn address_prefix(lower: &str) -> Option<(AddressField, &'static str)> {
    [
        ("to:", AddressField::To),
        ("from:", AddressField::From),
        ("cc:", AddressField::Cc),
        ("bcc:", AddressField::Bcc),
        ("reply-to:", AddressField::ReplyTo),
        ("addressed:", AddressField::Any),
    ]
    .into_iter()
    .find(|(p, _)| lower.starts_with(p))
    .map(|(p, f)| (f, p))
}

/// Splits on whitespace; double quotes group words and are removed.
pub fn tokenize(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut had_quotes = false;
    for c in input.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                had_quotes = true;
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() || had_quotes {
                    out.push(std::mem::take(&mut cur));
                }
                had_quotes = false;
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.retain(|t| !t.is_empty());
    out
}

/// Drops `<>",;()`, collapses whitespace, lower-cases.
fn clean(s: &str) -> String {
    let replaced: String = s
        .chars()
        .map(|c| {
            if "\r\n\t<>\",;()\u{feff}".contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    replaced
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// `2k`, `1.5m`, `300`: k/kb and m/mb units.
pub fn size_to_bytes(v: &str) -> Option<u64> {
    let v = v.trim();
    let split = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(v.len());
    let (num, unit) = v.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mult = match unit.trim() {
        "" => 1.0,
        "k" | "kb" => 1024.0,
        "m" | "mb" => 1024.0 * 1024.0,
        _ => return None,
    };
    let bytes = n * mult;
    (bytes > 0.0).then_some(bytes as u64)
}

/// IANA name or `+03:30`; falls back to the system time zone.
pub fn resolve_zone(tz: Option<&str>) -> TimeZone {
    match tz.map(str::trim).filter(|t| !t.is_empty()) {
        Some(name) => {
            if let Ok(z) = TimeZone::get(name) {
                return z;
            }
            if let Some(offset) = parse_offset(name) {
                return TimeZone::fixed(offset);
            }
            TimeZone::system()
        }
        None => TimeZone::system(),
    }
}

/// `+03:30`, `-0500`, `+02`.
fn parse_offset(s: &str) -> Option<jiff::tz::Offset> {
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    if !digits.chars().all(|c| c.is_ascii_digit()) || !(digits.len() == 2 || digits.len() == 4) {
        return None;
    }
    let hours: i32 = digits[..2].parse().ok()?;
    let minutes: i32 = digits.get(2..).map_or(Some(0), |m| m.parse().ok())?;
    jiff::tz::Offset::from_seconds(sign * (hours * 3600 + minutes * 60)).ok()
}

/// Dates as accepted by search: ISO dates and date-times, with or without
/// offset, `/` separators, and RFC 2822 dates.
pub fn parse_date(raw: &str, zone: &TimeZone) -> Option<i64> {
    let s = raw.trim().replace('/', "-");
    if let Ok(ts) = s.to_uppercase().parse::<Timestamp>() {
        return Some(ts.as_microsecond());
    }
    if let Ok(dt) = s.to_uppercase().replace(' ', "T").parse::<DateTime>() {
        return dt
            .to_zoned(zone.clone())
            .ok()
            .map(|z| z.timestamp().as_microsecond());
    }
    if let Ok(d) = s.parse::<Date>() {
        return d
            .to_zoned(zone.clone())
            .ok()
            .map(|z| z.timestamp().as_microsecond());
    }
    if let Ok(z) = jiff::fmt::rfc2822::parse(raw.trim()) {
        return Some(z.timestamp().as_microsecond());
    }
    None
}

#[cfg(test)]
mod tests;
