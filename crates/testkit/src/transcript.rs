//! SMTP transcripts: plain-text scripts of a client/server conversation, run
//! against a live server. Format (one directive per line, `#` starts a comment):
//!
//! ```text
//! S: 220 *           read one complete (possibly multi-line) reply; glob-match its last line
//! S? 8BITMIME        the previous reply has a line containing this text
//! S! PIPELINING      the previous reply has no line containing this text
//! C: EHLO client     send the text followed by CRLF
//! C~ a\r\n.\r\n       send exactly these bytes (escapes: \r \n \t \\ \xNN)
//! CLOSED             the server closes the connection
//! SLEEP 100          wait this many milliseconds
//! ```
//!
//! Glob patterns support `*` (any run of characters) and match case-sensitively.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Send(Vec<u8>),
    Expect(String),
    Contains(String),
    NotContains(String),
    Closed,
    Sleep(Duration),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub number: usize,
    pub step: Step,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub name: String,
    pub lines: Vec<Line>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("{name}:{line}: unknown directive {text:?}")]
    Unknown {
        name: String,
        line: usize,
        text: String,
    },
    #[error("{name}:{line}: invalid escape in {text:?}")]
    Escape {
        name: String,
        line: usize,
        text: String,
    },
    #[error("{name}:{line}: invalid SLEEP value")]
    Sleep { name: String, line: usize },
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("{name}:{line}: expected reply matching {expected:?}, got {got:?}")]
    Mismatch {
        name: String,
        line: usize,
        expected: String,
        got: Vec<String>,
    },
    #[error("{name}:{line}: reply {got:?} {why} {needle:?}")]
    Content {
        name: String,
        line: usize,
        needle: String,
        got: Vec<String>,
        why: &'static str,
    },
    #[error("{name}:{line}: expected the server to close the connection")]
    NotClosed { name: String, line: usize },
    #[error("{name}:{line}: connection closed unexpectedly")]
    UnexpectedClose { name: String, line: usize },
    #[error("{name}:{line}: timed out")]
    Timeout { name: String, line: usize },
    #[error("{name}:{line}: {source}")]
    Io {
        name: String,
        line: usize,
        source: std::io::Error,
    },
    #[error("{name}: {directive} at line {line} has no preceding reply")]
    NoReply {
        name: String,
        line: usize,
        directive: &'static str,
    },
}

impl Transcript {
    pub fn parse(name: &str, text: &str) -> Result<Self, ParseError> {
        let mut lines = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let number = i + 1;
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let rest = |p: &str| {
                line.strip_prefix(p)
                    .map(|r| r.strip_prefix(' ').unwrap_or(r).to_string())
            };
            let step = if let Some(r) = rest("C:") {
                let mut b = r.into_bytes();
                b.extend_from_slice(b"\r\n");
                Step::Send(b)
            } else if let Some(r) = rest("C~") {
                Step::Send(unescape(&r).ok_or_else(|| ParseError::Escape {
                    name: name.into(),
                    line: number,
                    text: r.clone(),
                })?)
            } else if let Some(r) = rest("S:") {
                Step::Expect(r)
            } else if let Some(r) = rest("S?") {
                Step::Contains(r)
            } else if let Some(r) = rest("S!") {
                Step::NotContains(r)
            } else if line.trim() == "CLOSED" {
                Step::Closed
            } else if let Some(r) = rest("SLEEP") {
                let ms = r.trim().parse().map_err(|_| ParseError::Sleep {
                    name: name.into(),
                    line: number,
                })?;
                Step::Sleep(Duration::from_millis(ms))
            } else {
                return Err(ParseError::Unknown {
                    name: name.into(),
                    line: number,
                    text: line.to_string(),
                });
            };
            lines.push(Line { number, step });
        }
        Ok(Self {
            name: name.to_string(),
            lines,
        })
    }

    /// Runs the transcript on a fresh connection to `addr`. `timeout` applies to each read.
    pub async fn run(&self, addr: &str, timeout: Duration) -> Result<(), RunError> {
        let name = self.name.clone();
        let io = |line: usize| {
            let name = name.clone();
            move |source| RunError::Io { name, line, source }
        };
        let stream = TcpStream::connect(addr).await.map_err(io(0))?;
        let (r, mut w): (OwnedReadHalf, OwnedWriteHalf) = stream.into_split();
        let mut r = BufReader::new(r);
        let mut last: Option<Vec<String>> = None;

        for Line { number, step } in &self.lines {
            let n = *number;
            match step {
                Step::Send(bytes) => w.write_all(bytes).await.map_err(io(n))?,
                Step::Sleep(d) => tokio::time::sleep(*d).await,
                Step::Expect(pattern) => {
                    let reply = read_reply(&mut r, timeout, &self.name, n).await?;
                    let final_line = reply.last().map(String::as_str).unwrap_or_default();
                    if !glob(pattern, final_line) {
                        return Err(RunError::Mismatch {
                            name: self.name.clone(),
                            line: n,
                            expected: pattern.clone(),
                            got: reply,
                        });
                    }
                    last = Some(reply);
                }
                Step::Contains(needle) | Step::NotContains(needle) => {
                    let want = matches!(step, Step::Contains(_));
                    let reply = last.as_ref().ok_or_else(|| RunError::NoReply {
                        name: self.name.clone(),
                        line: n,
                        directive: if want { "S?" } else { "S!" },
                    })?;
                    if reply.iter().any(|l| l.contains(needle.as_str())) != want {
                        let why = if want {
                            "does not contain"
                        } else {
                            "unexpectedly contains"
                        };
                        return Err(RunError::Content {
                            name: self.name.clone(),
                            line: n,
                            needle: needle.clone(),
                            got: reply.clone(),
                            why,
                        });
                    }
                }
                Step::Closed => {
                    let mut buf = String::new();
                    match tokio::time::timeout(timeout, r.read_line(&mut buf)).await {
                        Ok(Ok(0)) | Ok(Err(_)) => {}
                        Ok(Ok(_)) | Err(_) => {
                            return Err(RunError::NotClosed {
                                name: self.name.clone(),
                                line: n,
                            });
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

async fn read_reply(
    r: &mut BufReader<OwnedReadHalf>,
    timeout: Duration,
    name: &str,
    line: usize,
) -> Result<Vec<String>, RunError> {
    let mut out = Vec::new();
    loop {
        let mut buf = String::new();
        let n = tokio::time::timeout(timeout, r.read_line(&mut buf))
            .await
            .map_err(|_| RunError::Timeout {
                name: name.into(),
                line,
            })?
            .map_err(|source| RunError::Io {
                name: name.into(),
                line,
                source,
            })?;
        if n == 0 {
            return Err(RunError::UnexpectedClose {
                name: name.into(),
                line,
            });
        }
        let text = buf.trim_end_matches(['\r', '\n']).to_string();
        // RFC 5321 §4.2.1: "250-" continues a multi-line reply, "250 " ends it.
        let more = text.len() > 3 && text.as_bytes()[3] == b'-';
        out.push(text);
        if !more {
            return Ok(out);
        }
    }
}

fn unescape(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut b = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            continue;
        }
        match chars.next()? {
            'r' => out.push(b'\r'),
            'n' => out.push(b'\n'),
            't' => out.push(b'\t'),
            '\\' => out.push(b'\\'),
            'x' => {
                let hex: String = [chars.next()?, chars.next()?].iter().collect();
                out.push(u8::from_str_radix(&hex, 16).ok()?);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// `*` matches any (possibly empty) run of characters.
pub fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first)
        || !text[first.len()..].ends_with(last)
        || text.len() < first.len() + last.len()
    {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matching() {
        assert!(glob("250 *", "250 2.0.0 Ok"));
        assert!(glob("*", ""));
        assert!(glob("250 2.0.0 Ok: queued*", "250 2.0.0 Ok: queued as abc"));
        assert!(glob("5*Syntax*", "500 5.5.2 Syntax error"));
        assert!(!glob("250 *", "550 no"));
        assert!(!glob("a*a", "a"));
        assert!(glob("exact", "exact"));
    }

    #[test]
    fn parses_all_directives() {
        let t = Transcript::parse(
            "t",
            "# c\nS: 220 *\nC: EHLO x\nS? SIZE\nS! XYZ\nC~ a\\r\\n\\x2e\\r\\n\nSLEEP 5\nCLOSED\n",
        )
        .expect("parse");
        assert_eq!(t.lines.len(), 7);
        assert_eq!(t.lines[1].step, Step::Send(b"EHLO x\r\n".to_vec()));
        assert_eq!(t.lines[4].step, Step::Send(b"a\r\n.\r\n".to_vec()));
        assert_eq!(t.lines[0].number, 2);
    }

    #[test]
    fn rejects_unknown_directive_and_bad_escape() {
        assert!(matches!(
            Transcript::parse("t", "X: hi"),
            Err(ParseError::Unknown { line: 1, .. })
        ));
        assert!(matches!(
            Transcript::parse("t", "C~ \\q"),
            Err(ParseError::Escape { .. })
        ));
    }

    /// Minimal scripted server to test the runner itself.
    async fn fake_server(script: &'static [&'static str]) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = l.local_addr().expect("addr").to_string();
        tokio::spawn(async move {
            let (s, _) = l.accept().await.expect("accept");
            let (r, mut w) = s.into_split();
            let mut r = BufReader::new(r);
            w.write_all(b"220 fake ESMTP\r\n").await.expect("w");
            for reply in script {
                let mut line = String::new();
                if r.read_line(&mut line).await.expect("r") == 0 {
                    return;
                }
                w.write_all(reply.as_bytes()).await.expect("w");
                if reply.starts_with("221") {
                    return;
                }
            }
        });
        addr
    }

    #[tokio::test]
    async fn runs_against_scripted_server() {
        let addr =
            fake_server(&["250-fake\r\n250-SIZE 0\r\n250 8BITMIME\r\n", "221 bye\r\n"]).await;
        let t = Transcript::parse(
            "ok",
            "S: 220 *\nC: EHLO me\nS: 250 *\nS? SIZE\nS! PIPELINING\nC: QUIT\nS: 221 *\nCLOSED\n",
        )
        .expect("parse");
        t.run(&addr, Duration::from_secs(2)).await.expect("run");
    }

    #[tokio::test]
    async fn reports_mismatch_with_line_number() {
        let addr = fake_server(&["550 nope\r\n"]).await;
        let t = Transcript::parse("bad", "S: 220 *\nC: NOOP\nS: 250 *\n").expect("parse");
        let err = t
            .run(&addr, Duration::from_secs(2))
            .await
            .expect_err("mismatch");
        assert!(matches!(err, RunError::Mismatch { line: 3, .. }), "{err}");
    }
}
