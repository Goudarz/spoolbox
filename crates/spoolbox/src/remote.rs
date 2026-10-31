//! Commands that work against a running server: `wait`, `export`, `import`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::Client;

/// The server caps one wait request at five minutes; longer waits are split.
const MAX_WAIT_MS: u128 = 300_000;

#[derive(Debug)]
pub enum WaitResult {
    Found(Value),
    TimedOut,
}

/// Blocks until a message matching `query` arrives (`POST /api/v2/wait`).
pub async fn wait(
    client: &Client,
    query: &str,
    timeout: Duration,
    since: Option<i64>,
) -> Result<WaitResult, String> {
    let deadline = Instant::now() + timeout;
    // Fixing the starting point once keeps later chunks from missing mail that arrives
    // between two requests.
    let since = match since {
        Some(s) => s,
        None => current_seq(client).await?,
    };
    loop {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        let req = json!({ "query": query, "timeout_ms": left.min(MAX_WAIT_MS) as u64, "since_seq": since });
        let r = client
            .request(
                "POST",
                "api/v2/wait",
                Some((req.to_string().into_bytes(), "application/json")),
            )
            .await?;
        match r.status {
            200 => return Ok(WaitResult::Found(r.json()?)),
            408 if Instant::now() >= deadline => return Ok(WaitResult::TimedOut),
            408 => continue,
            _ => return Err(r.error()),
        }
    }
}

/// The latest event sequence number, so a wait only considers mail that arrives afterwards.
async fn current_seq(client: &Client) -> Result<i64, String> {
    let info: Value = client.get_json("api/v2/events/cursor").await?;
    info.get("seq")
        .and_then(Value::as_i64)
        .ok_or_else(|| "unexpected response from api/v2/events/cursor".to_string())
}

#[derive(Deserialize)]
struct Page {
    messages_count: u64,
    messages: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "ID")]
    id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Eml,
    Mbox,
}

/// IDs are ULIDs; anything else is refused so a server response can never pick a file path.
fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// mboxrd: `From ` lines (with any number of leading `>`) get one more `>`.
fn mbox_entry(raw: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(b"From spoolbox@localhost Thu Jan  1 00:00:00 1970\n");
    let text = raw.strip_suffix(b"\n").unwrap_or(raw);
    for line in text.split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let quoted = line.iter().position(|b| *b != b'>').map(|i| &line[i..]);
        if quoted.is_some_and(|rest| rest.starts_with(b"From ")) {
            out.push(b'>');
        }
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out.push(b'\n');
}

/// Writes every message (or those matching `query`), newest first. Returns the count.
pub async fn export(
    client: &Client,
    query: Option<&str>,
    out: &Path,
    format: Format,
) -> Result<usize, String> {
    let mut ids = Vec::new();
    loop {
        let start = ids.len();
        let path = match query {
            Some(q) => format!("api/v1/search?query={}&start={start}&limit=200", encode(q)),
            None => format!("api/v1/messages?start={start}&limit=200"),
        };
        let page: Page = client.get_json(&path).await?;
        let n = page.messages.len();
        ids.extend(page.messages.into_iter().map(|m| m.id));
        if n == 0 || ids.len() as u64 >= page.messages_count {
            break;
        }
    }
    let mut mbox = Vec::new();
    if format == Format::Eml {
        std::fs::create_dir_all(out)
            .map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    }
    for id in &ids {
        if !safe_id(id) {
            return Err(format!("unexpected message ID {id:?}"));
        }
        let r = client
            .request("GET", &format!("api/v1/message/{id}/raw"), None)
            .await?;
        if r.status == 404 {
            continue; // Deleted meanwhile.
        }
        if !r.ok() {
            return Err(r.error());
        }
        match format {
            Format::Eml => {
                let file = out.join(format!("{id}.eml"));
                std::fs::write(&file, &r.body)
                    .map_err(|e| format!("cannot write {}: {e}", file.display()))?;
            }
            Format::Mbox => mbox_entry(&r.body, &mut mbox),
        }
    }
    if format == Format::Mbox {
        let mut f = std::fs::File::create(out)
            .map_err(|e| format!("cannot create {}: {e}", out.display()))?;
        f.write_all(&mbox)
            .map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    }
    Ok(ids.len())
}

/// Splits an mbox (mboxrd or mboxo) into messages, removing one `>` from quoted `From ` lines.
pub fn split_mbox(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut current: Option<Vec<u8>> = None;
    for line in data.split_inclusive(|b| *b == b'\n') {
        if line.starts_with(b"From ") {
            if let Some(m) = current.take() {
                out.push(m);
            }
            current = Some(Vec::new());
            continue;
        }
        let Some(msg) = current.as_mut() else {
            continue;
        };
        let body = line.strip_suffix(b"\n").unwrap_or(line);
        let body = body.strip_suffix(b"\r").unwrap_or(body);
        let unquoted = match body.iter().position(|b| *b != b'>') {
            Some(i) if i > 0 && body[i..].starts_with(b"From ") => &body[1..],
            _ => body,
        };
        msg.extend_from_slice(unquoted);
        msg.extend_from_slice(b"\r\n");
    }
    if let Some(m) = current {
        out.push(m);
    }
    for m in &mut out {
        // The blank separator line before the next `From ` belongs to the mbox, not the message.
        if m.ends_with(b"\r\n\r\n") {
            m.truncate(m.len() - 2);
        }
    }
    out
}

/// Imports `.eml` files, directories of them, and mbox files. Returns (imported, skipped).
pub async fn import(client: &Client, paths: &[PathBuf]) -> Result<(usize, usize), String> {
    let mut files = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(p)
                .map_err(|e| format!("cannot read {}: {e}", p.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|f| f.is_file())
                .collect();
            entries.sort();
            files.extend(entries);
        } else {
            files.push(p.clone());
        }
    }
    let (mut imported, mut skipped) = (0, 0);
    for f in files {
        let data = std::fs::read(&f).map_err(|e| format!("cannot read {}: {e}", f.display()))?;
        let messages = if data.starts_with(b"From ") {
            split_mbox(&data)
        } else {
            vec![data]
        };
        for raw in messages {
            let r = client
                .request("POST", "api/v2/messages", Some((raw, "message/rfc822")))
                .await?;
            match r.status {
                201 => imported += 1,
                200 => skipped += 1,
                _ => return Err(format!("{}: {}", f.display(), r.error())),
            }
        }
    }
    Ok((imported, skipped))
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mbox_round_trip_quotes_from_lines() {
        let a = b"Subject: a\r\n\r\nFrom here\r\n>From there\r\nend\r\n".to_vec();
        let b = b"Subject: b\r\n\r\nsecond\r\n".to_vec();
        let mut mbox = Vec::new();
        mbox_entry(&a, &mut mbox);
        mbox_entry(&b, &mut mbox);
        let text = String::from_utf8_lossy(&mbox);
        assert!(
            text.contains("\n>From here\n") && text.contains("\n>>From there\n"),
            "{text}"
        );
        assert_eq!(split_mbox(&mbox), vec![a, b]);
    }

    #[test]
    fn ids_cannot_choose_paths() {
        assert!(safe_id("01M69RA6VY7Z01PXC31VVX1ZT6"));
        for bad in ["", "../x", "a/b", "a\\b", "C:", "x.eml"] {
            assert!(!safe_id(bad), "{bad}");
        }
        assert_eq!(
            encode("subject:سلام a"),
            "subject%3A%D8%B3%D9%84%D8%A7%D9%85%20a"
        );
    }
}
