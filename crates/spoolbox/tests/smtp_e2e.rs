//! End-to-end tests of the binary with SMTP and a persistent database, including the
//! ADR-005 crash test: every message acknowledged with 250 must survive a hard kill.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spoolbox_store::{Store, StoreOptions};

fn bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spoolbox"));
    for (k, _) in std::env::vars() {
        if k.starts_with("MP_") || k.starts_with("SPOOLBOX_") {
            cmd.env_remove(k);
        }
    }
    cmd
}

struct Server {
    child: Child,
    smtp: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(db: &Path) -> Server {
    let mut child = bin()
        .args([
            "--listen",
            "127.0.0.1:0",
            "--smtp",
            "127.0.0.1:0",
            "--log-format",
            "json",
            "--max",
            "0",
            "--min-free-disk",
            "0",
        ])
        .arg("--database")
        .arg(db)
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn");
    let stderr = child.stderr.take().expect("stderr");
    let mut server = Server {
        child,
        smtp: String::new(),
    };
    let mut lines = BufReader::new(stderr).lines();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let Some(Ok(line)) = lines.next() else { break };
        let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        match v["fields"]["message"].as_str() {
            Some("SMTP listening") => {
                server.smtp = v["fields"]["addr"].as_str().unwrap_or_default().to_string()
            }
            Some("HTTP listening") => {
                std::thread::spawn(move || for _ in lines {});
                assert!(!server.smtp.is_empty());
                return server;
            }
            _ => {}
        }
    }
    panic!("server did not start");
}

/// Minimal blocking SMTP client. Returns the final reply line.
fn deliver(addr: &str, body: &str) -> std::io::Result<String> {
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut r = BufReader::new(s.try_clone()?);
    let read = |r: &mut BufReader<TcpStream>| -> std::io::Result<String> {
        loop {
            let mut l = String::new();
            if r.read_line(&mut l)? == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            if l.as_bytes().get(3) != Some(&b'-') {
                return Ok(l.trim_end().to_string());
            }
        }
    };
    read(&mut r)?;
    for cmd in [
        "EHLO crash.test",
        "MAIL FROM:<a@crash.test>",
        "RCPT TO:<b@crash.test>",
        "DATA",
    ] {
        s.write_all(format!("{cmd}\r\n").as_bytes())?;
        read(&mut r)?;
    }
    s.write_all(format!("{body}\r\n.\r\n").as_bytes())?;
    let reply = read(&mut r)?;
    let _ = s.write_all(b"QUIT\r\n");
    Ok(reply)
}

fn body(n: usize) -> String {
    format!(
        "Subject: crash test {n}\r\nMessage-ID: <crash-{n}@test>\r\n\r\n{}",
        "payload line\r\n".repeat(50 + n % 200)
    )
}

#[test]
fn acknowledged_messages_survive_hard_kill() {
    let iterations: usize = std::env::var("SPOOLBOX_CRASH_ITERATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let dir = tempfile::tempdir().expect("dir");
    let db = dir.path().join("crash.db");
    let mut acknowledged: Vec<(String, String)> = Vec::new();
    let mut counter = 0usize;

    for round in 0..iterations {
        let server = start(&db);
        let acked = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let senders: Vec<_> = (0..4)
            .map(|t| {
                let (addr, acked, stop) = (server.smtp.clone(), acked.clone(), stop.clone());
                let base = counter + t * 100_000;
                std::thread::spawn(move || {
                    let mut i = 0;
                    while !stop.load(Ordering::SeqCst) {
                        let b = body(base + i);
                        i += 1;
                        // Errors are expected once the server is killed; only 250 counts.
                        if let Ok(reply) = deliver(&addr, &b)
                            && let Some(id) = reply.strip_prefix("250 2.0.0 Ok: queued as ")
                        {
                            acked.lock().expect("lock").push((id.to_string(), b));
                        }
                    }
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(150 + (round as u64 * 37) % 250));
        let mut server = server;
        server.child.kill().expect("kill"); // TerminateProcess / SIGKILL: no graceful shutdown
        let _ = server.child.wait();
        stop.store(true, Ordering::SeqCst);
        for s in senders {
            let _ = s.join();
        }
        counter += 1_000_000;
        acknowledged.extend(acked.lock().expect("lock").drain(..));
    }

    assert!(
        acknowledged.len() > iterations,
        "too few messages were sent ({}) to make the test meaningful",
        acknowledged.len()
    );
    let store = Store::open(StoreOptions {
        database: Some(db),
        max_messages: 0,
        ..StoreOptions::default()
    })
    .expect("reopen");
    let mut missing = Vec::new();
    for (id, sent) in &acknowledged {
        match store.raw(id) {
            Ok(raw) if raw == format!("{sent}\r\n").into_bytes() => {}
            Ok(_) => missing.push(format!("{id}: content differs")),
            Err(e) => missing.push(format!("{id}: {e}")),
        }
    }
    assert!(
        missing.is_empty(),
        "{} of {} acknowledged messages lost after {iterations} kills:\n{}",
        missing.len(),
        acknowledged.len(),
        missing.join("\n")
    );
    eprintln!(
        "verified {} acknowledged messages across {iterations} hard kills",
        acknowledged.len()
    );
}

#[test]
fn send_sample_end_to_end_and_reindex() {
    let dir = tempfile::tempdir().expect("dir");
    let db = dir.path().join("samples.db");
    {
        let server = start(&db);
        let out = bin()
            .args(["send-sample", "--smtp", &server.smtp])
            .output()
            .expect("send-sample");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            stdout
                .lines()
                .filter(|l| l.contains("250 2.0.0 Ok: queued as"))
                .count(),
            5,
            "{stdout}"
        );
        // The server is killed here, possibly before indexing finished; `reindex` completes it.
    }
    let out = bin().arg("reindex").arg(&db).output().expect("reindex");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let store = Store::open(StoreOptions {
        database: Some(db),
        max_messages: 0,
        ..StoreOptions::default()
    })
    .expect("open");
    let rows = store.list(10, None).expect("list");
    assert_eq!(rows.len(), 5);
    assert!(
        rows.iter().all(|r| r.status == "indexed"),
        "{:?}",
        rows.iter()
            .map(|r| (&r.subject, &r.status))
            .collect::<Vec<_>>()
    );
    let persian = rows
        .iter()
        .find(|r| r.subject.starts_with("سفارش"))
        .expect("persian sample");
    assert_eq!(persian.tags, ["sample", "سفارش"]);
    assert_eq!(persian.inline_count, 1);
    let att = rows
        .iter()
        .find(|r| r.subject.starts_with("Monthly"))
        .expect("attachments sample");
    assert_eq!(att.attachment_count, 3);
    let parts = store.parts(&att.id).expect("parts");
    assert!(
        parts
            .iter()
            .any(|p| p.filename.as_deref() == Some("گزارش-ماهانه.txt")),
        "{parts:?}"
    );
    let otp = rows
        .iter()
        .find(|r| r.subject.contains("482913"))
        .expect("otp sample");
    assert_eq!(otp.tags, ["signup"]);
    assert_eq!(
        store.search_text("abc123", 10).expect("search"),
        std::slice::from_ref(&otp.id)
    );
    let mut raw = Vec::new();
    std::io::Cursor::new(store.raw(&otp.id).expect("raw"))
        .read_to_end(&mut raw)
        .expect("read");
    assert!(String::from_utf8_lossy(&raw).contains("List-Unsubscribe-Post"));
}
