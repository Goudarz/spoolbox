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
    };
    let mut lines = BufReader::new(stderr).lines();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let Some(Ok(line)) = lines.next() else { break };
        let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
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
