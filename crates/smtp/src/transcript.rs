//! Opt-in SMTP conversation transcripts (`--smtp-transcripts`) for answering "why does my
//! client fail?". Kept in memory for the most recent sessions only. AUTH exchanges are
//! redacted and message bodies are never recorded, only their outcome.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const MAX_LINES: usize = 1000;
const MAX_LINE_CHARS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptLine {
    /// Milliseconds since the connection was accepted.
    pub at_ms: u64,
    /// `C` client, `S` server, `*` note (TLS, body received, ...).
    pub from: char,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub id: u64,
    /// Unix milliseconds.
    pub started_at: i64,
    pub peer: String,
    pub lines: Vec<TranscriptLine>,
    /// IDs of messages stored during this session.
    pub message_ids: Vec<String>,
    /// More than the line limit was exchanged; later lines are missing.
    pub truncated: bool,
}

#[derive(Debug)]
pub struct Transcripts {
    sessions: Mutex<VecDeque<Arc<Transcript>>>,
    capacity: usize,
    next: AtomicU64,
}

impl Transcripts {
    pub fn new(capacity: usize) -> Self {
        Self {
            sessions: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity: capacity.max(1),
            next: AtomicU64::new(1),
        }
    }

    fn push(&self, t: Transcript) {
        let mut s = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if s.len() == self.capacity {
            s.pop_front();
        }
        s.push_back(Arc::new(t));
    }

    /// Newest first.
    pub fn list(&self) -> Vec<Arc<Transcript>> {
        let s = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        s.iter().rev().cloned().collect()
    }

    pub fn get(&self, id: u64) -> Option<Arc<Transcript>> {
        self.list().into_iter().find(|t| t.id == id)
    }

    /// The session in which a message was received.
    pub fn for_message(&self, message_id: &str) -> Option<Arc<Transcript>> {
        self.list()
            .into_iter()
            .find(|t| t.message_ids.iter().any(|m| m == message_id))
    }

    pub(crate) fn recorder(self: &Arc<Self>, peer: String) -> Recorder {
        let started_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();
        Recorder {
            sink: Some(self.clone()),
            start: Instant::now(),
            t: Transcript {
                id: self.next.fetch_add(1, Ordering::Relaxed),
                started_at,
                peer,
                ..Default::default()
            },
        }
    }
}

/// Records one connection; the transcript is stored when the recorder is dropped, so every
/// way a connection can end (QUIT, timeout, reset, error) is captured.
#[derive(Debug)]
pub(crate) struct Recorder {
    sink: Option<Arc<Transcripts>>,
    start: Instant,
    t: Transcript,
}

impl Recorder {
    pub(crate) fn disabled() -> Self {
        Self {
            sink: None,
            start: Instant::now(),
            t: Transcript::default(),
        }
    }

    fn add(&mut self, from: char, text: &str) {
        if self.sink.is_none() {
            return;
        }
        if self.t.lines.len() >= MAX_LINES {
            self.t.truncated = true;
            return;
        }
        let long = text.chars().nth(MAX_LINE_CHARS).is_some();
        let mut text: String = text.chars().take(MAX_LINE_CHARS).collect();
        if long {
            text.push('…');
        }
        self.t.lines.push(TranscriptLine {
            at_ms: self.start.elapsed().as_millis() as u64,
            from,
            text,
        });
    }

    pub(crate) fn client(&mut self, line: &str) {
        let first = line.split_whitespace().next().unwrap_or_default();
        if first.eq_ignore_ascii_case("AUTH") {
            let mechanism = line.split_whitespace().nth(1).unwrap_or_default();
            self.add('C', &format!("AUTH {mechanism} [redacted]"));
        } else {
            self.add('C', line);
        }
    }

    /// A line sent in reply to an AUTH challenge.
    pub(crate) fn client_secret(&mut self) {
        self.add('C', "[redacted]");
    }

    /// Multi-line replies are recorded line by line.
    pub(crate) fn server(&mut self, reply: &str) {
        for line in reply.split("\r\n") {
            self.add('S', line);
        }
    }

    pub(crate) fn note(&mut self, text: &str) {
        self.add('*', text);
    }

    pub(crate) fn stored(&mut self, id: &str) {
        if self.sink.is_some() && !id.is_empty() {
            self.t.message_ids.push(id.to_string());
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.push(std::mem::take(&mut self.t));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_redacts_and_keeps_recent_sessions() {
        let store = Arc::new(Transcripts::new(2));
        for n in 0..3 {
            let mut r = store.recorder(format!("10.0.0.{n}"));
            r.server("220 mx ESMTP");
            r.client("AUTH PLAIN dXNlcgB1c2VyAHNlY3JldA==");
            r.client_secret();
            r.server("250-mx\r\n250 SIZE 10");
            r.stored(&format!("ID{n}"));
        }
        let list = store.list();
        assert_eq!(list.len(), 2, "oldest dropped");
        assert_eq!(list[0].peer, "10.0.0.2");
        let lines: Vec<String> = list[0]
            .lines
            .iter()
            .map(|l| format!("{} {}", l.from, l.text))
            .collect();
        assert_eq!(
            lines,
            [
                "S 220 mx ESMTP",
                "C AUTH PLAIN [redacted]",
                "C [redacted]",
                "S 250-mx",
                "S 250 SIZE 10"
            ]
        );
        assert!(!format!("{list:?}").contains("dXNlcgB1c2VyAHNlY3JldA"));
        assert_eq!(
            store.for_message("ID1").map(|t| t.peer.clone()).as_deref(),
            Some("10.0.0.1")
        );
        assert!(store.for_message("ID0").is_none());
        let mut off = Recorder::disabled();
        off.client("HELO x");
        assert!(off.t.lines.is_empty());
    }
}
