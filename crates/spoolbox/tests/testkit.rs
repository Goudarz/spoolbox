//! Example: test an application's email with Spoolbox running inside the test process.

use std::time::Duration;

use spoolbox_testkit::server::{TestServer, send};

#[tokio::test(flavor = "multi_thread")]
async fn signup_email_arrives_with_a_code() {
    let server = TestServer::start().await.expect("start");

    // The application under test would send this through `server.smtp_addr()`.
    let mail = concat!(
        "From: Acme <noreply@acme.test>\r\n",
        "To: alice@example.com\r\n",
        "Subject: Your code is 482913\r\n",
        "\r\n",
        ".leading dot survives\r\n",
    );
    send(
        server.smtp_addr(),
        "noreply@acme.test",
        &["alice@example.com"],
        mail.as_bytes(),
    )
    .await
    .expect("send");

    let msg = server
        .wait_for("to:alice@example.com subject:code", Duration::from_secs(5))
        .await
        .expect("message arrives");
    assert_eq!(msg.subject, "Your code is 482913");
    let raw = server.raw(&msg.id).await.expect("raw");
    assert_eq!(raw, mail.as_bytes(), "stored exactly as sent");

    // The HTTP API is up too.
    assert!(server.http_url().starts_with("http://127.0.0.1:"));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chaos_rules_make_failures_reproducible() {
    use spoolbox_core::chaos::{Rule, RuleStage};
    let server = TestServer::start_with(|s| s.chaos.enabled = true)
        .await
        .expect("start");
    server
        .chaos()
        .expect("chaos")
        .set_rules(vec![Rule {
            stage: RuleStage::Recipient,
            pattern: None,
            code: Some(452),
            message: Some("4.2.2 Mailbox full".into()),
            delay_ms: 0,
            disconnect: false,
            times: Some(1),
        }])
        .expect("rules");
    let first = send(
        server.smtp_addr(),
        "a@x.test",
        &["b@y.test"],
        b"Subject: one\r\n\r\nx\r\n",
    )
    .await;
    assert!(
        first
            .expect_err("first is refused")
            .to_string()
            .contains("452 4.2.2 Mailbox full")
    );
    send(
        server.smtp_addr(),
        "a@x.test",
        &["b@y.test"],
        b"Subject: two\r\n\r\nx\r\n",
    )
    .await
    .expect("retry succeeds");
    assert!(
        server
            .wait_for("subject:two", Duration::from_secs(5))
            .await
            .is_some()
    );
}
