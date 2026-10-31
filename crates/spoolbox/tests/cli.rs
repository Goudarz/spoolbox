//! End-to-end tests of the `spoolbox` binary.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

fn bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spoolbox"));
    // Keep the developer's own MP_* / SPOOLBOX_* settings out of the tests.
    for (k, _) in std::env::vars() {
        if k.starts_with("MP_") || k.starts_with("SPOOLBOX_") {
            cmd.env_remove(k);
        }
    }
    cmd
}

fn run(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = bin();
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run spoolbox")
}

struct Server {
    child: Child,
    addr: String,
    smtp: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts the server on an ephemeral port and reads the bound address from the JSON log.
fn start(extra: &[&str]) -> Server {
    let mut cmd = bin();
    cmd.args([
        "--listen",
        "127.0.0.1:0",
        "--smtp",
        "127.0.0.1:0",
        "--log-format",
        "json",
    ])
    .args(extra);
    let mut child = cmd
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn");
    let stderr = child.stderr.take().expect("stderr");
    // From here on, Drop kills and reaps the child on every path, including panics.
    let mut server = Server {
        child,
        addr: String::new(),
        smtp: String::new(),
    };
    let mut lines = BufReader::new(stderr).lines();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let Some(Ok(line)) = lines.next() else { break };
        let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        if v["fields"]["message"] == "SMTP listening" {
            server.smtp = v["fields"]["addr"].as_str().expect("addr").to_string();
        }
        if v["fields"]["message"] == "HTTP listening" {
            server.addr = v["fields"]["addr"].as_str().expect("addr").to_string();
            // Keep draining stderr so the child never blocks on a full pipe.
            std::thread::spawn(move || for _ in lines {});
            return server;
        }
    }
    panic!("server did not report its listen address");
}

#[test]
fn version_subcommand() {
    let out = run(&["version"], &[]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("spoolbox "));
}

#[test]
fn serve_then_readyz_succeeds() {
    let srv = start(&[]);
    let out = run(
        &[
            "readyz",
            "--listen",
            &srv.addr,
            "--wait",
            "--timeout",
            "10s",
        ],
        &[],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn https_with_a_generated_certificate() {
    let srv = start(&["--ui-tls-cert", "auto"]);
    let ok = run(
        &[
            "readyz",
            "--listen",
            &srv.addr,
            "--https",
            "--wait",
            "--timeout",
            "10s",
        ],
        &[],
    );
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let plain = run(&["readyz", "--listen", &srv.addr], &[]);
    assert!(!plain.status.success(), "plain HTTP must not answer");
}

#[test]
fn readyz_honours_webroot_from_mp_env() {
    let srv = start(&["--webroot", "mail"]);
    let out = run(
        &["readyz", "--listen", &srv.addr],
        &[("MP_WEBROOT", "/mail/")],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn readyz_fails_when_nothing_listens() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    drop(listener);
    let out = run(&["readyz", "--listen", &addr], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot reach"));
}

#[test]
fn refuses_unsupported_security_option() {
    let out = run(&["--ui-auth-file", "users.htpasswd"], &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--ui-auth-file"));
}

#[test]
fn refuses_network_bind_without_opt_in() {
    let out = run(&["--listen", "0.0.0.0:0"], &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--allow-unauthenticated-network"));
}

#[test]
fn unknown_flag_is_a_usage_error() {
    let out = run(&["--no-such-flag"], &[]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn mp_env_alias_is_applied() {
    let out = run(&[], &[("MP_UI_BIND_ADDR", "0.0.0.0:0")]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "MP_UI_BIND_ADDR must reach the network safety check"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("0.0.0.0"));
}

#[test]
fn wait_import_and_export() {
    let srv = start(&[]);
    let dir = tempfile::tempdir().expect("tempdir");
    let eml = dir.path().join("hello.eml");
    std::fs::write(
        &eml,
        "From: a@example.com\r\nTo: b@example.com\r\nSubject: hello cli\r\n\r\nFrom the start\r\n",
    )
    .expect("write");

    // A waiter started before the message arrives sees it.
    let waiter = bin()
        .args([
            "wait",
            "subject:hello",
            "--timeout",
            "10s",
            "--listen",
            &srv.addr,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wait");
    std::thread::sleep(Duration::from_millis(500));
    let imported = run(
        &["import", eml.to_str().expect("path"), "--listen", &srv.addr],
        &[],
    );
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let out = waiter.wait_with_output().expect("wait output");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(id.len(), 26, "{id}");

    let timeout = run(
        &[
            "wait",
            "subject:never",
            "--timeout",
            "300ms",
            "--listen",
            &srv.addr,
        ],
        &[],
    );
    assert_eq!(timeout.status.code(), Some(1));

    let eml_dir = dir.path().join("out");
    let out = run(
        &[
            "export",
            eml_dir.to_str().expect("path"),
            "--listen",
            &srv.addr,
        ],
        &[],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let saved = std::fs::read(eml_dir.join(format!("{id}.eml"))).expect("exported file");
    assert_eq!(
        saved,
        std::fs::read(&eml).expect("original"),
        "byte-exact round trip"
    );

    let mbox = dir.path().join("all.mbox");
    let out = run(
        &[
            "export",
            mbox.to_str().expect("path"),
            "--format",
            "mbox",
            "--listen",
            &srv.addr,
        ],
        &[],
    );
    assert!(out.status.success());
    let text = std::fs::read_to_string(&mbox).expect("mbox");
    assert!(
        text.starts_with("From ") && text.contains("\n>From the start\n"),
        "{text}"
    );
    let again = run(
        &[
            "import",
            mbox.to_str().expect("path"),
            "--listen",
            &srv.addr,
        ],
        &[],
    );
    assert!(String::from_utf8_lossy(&again.stderr).contains("imported 1 message"));
}

fn free_port() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    l.local_addr().expect("addr").to_string()
}

#[test]
fn doctor_explains_port_problems() {
    let (http, smtp) = (free_port(), free_port());
    let out = run(&["doctor", "--listen", &http, "--smtp", &smtp], &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains(&format!("{http} is free")), "{text}");

    let srv = start(&[]);
    let out = run(&["doctor", "--listen", &srv.addr, "--smtp", &smtp], &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("served by a running Spoolbox"), "{text}");

    let other = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let taken = other.local_addr().expect("addr").to_string();
    let out = run(&["doctor", "--listen", &http, "--smtp", &taken], &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("used by another program"), "{text}");

    let out = run(&["doctor", "--max", "lots"], &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).contains("[FAIL] config"));
}

/// Minimal HTTP GET for tests (the server answers with `Connection: close`).
fn http_get(addr: &str, path: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).expect("connect");
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .expect("write");
    let mut buf = String::new();
    s.read_to_string(&mut buf).expect("read");
    let status = buf
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = buf
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

#[test]
fn smtp_transcripts_are_opt_in() {
    let off = start(&[]);
    assert_eq!(http_get(&off.addr, "/api/v2/transcripts").0, 404);

    let srv = start(&["--smtp-transcripts"]);
    let sent = run(
        &["send-sample", "--smtp", &srv.smtp, "--sample", "plain"],
        &[],
    );
    assert!(
        sent.status.success(),
        "{}",
        String::from_utf8_lossy(&sent.stderr)
    );
    let (status, body) = http_get(&srv.addr, "/api/v2/transcripts");
    assert_eq!(status, 200, "{body}");
    let list: serde_json::Value = serde_json::from_str(body.trim()).expect("json");
    let id = list[0]["message_ids"][0]
        .as_str()
        .expect("message id")
        .to_string();
    let (status, body) = http_get(&srv.addr, &format!("/api/v2/messages/{id}/transcript"));
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("\"text\":\"MAIL FROM:") && body.contains("message received"),
        "{body}"
    );
}

fn mcp_session(addr: &str, input: &str) -> Vec<serde_json::Value> {
    use std::io::Write;
    let mut child = bin()
        .args(["mcp", "--listen", addr])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write");
    let out = child.wait_with_output().expect("output");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).expect("one JSON-RPC message per line"))
        .collect()
}

#[test]
fn mcp_over_stdio() {
    let srv = start(&[]);
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "",
    ]
    .join("\n");
    let replies = mcp_session(&srv.addr, &input);
    assert_eq!(replies.len(), 2, "notifications get no reply: {replies:?}");
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "spoolbox");
    assert!(
        replies[1]["result"]["tools"]
            .as_array()
            .is_some_and(|t| t.len() >= 7)
    );

    let down = free_port();
    let replies = mcp_session(
        &down,
        "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n",
    );
    assert_eq!(replies[0]["id"], 7);
    assert_eq!(replies[0]["error"]["code"], -32603);
}
