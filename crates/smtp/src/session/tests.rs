use super::*;

fn session(cfg: SessionConfig) -> Session {
    Session::new(Arc::new(cfg), "127.0.0.1", None)
}

fn codes(s: &mut Session, lines: &[&str]) -> Vec<u16> {
    lines.iter().map(|l| s.handle(l).code()).collect()
}

#[test]
fn happy_path_and_envelope() {
    let mut s = session(SessionConfig::new("mx.test"));
    assert_eq!(s.banner(), "220 mx.test Spoolbox ESMTP Service ready");
    assert_eq!(
        codes(
            &mut s,
            &[
                "EHLO client.test",
                "MAIL FROM:<a@x.test> SIZE=10",
                "RCPT TO:<b@y.test>",
                "RCPT TO:<c@y.test>"
            ]
        ),
        [250, 250, 250, 250]
    );
    let r = s.handle("DATA");
    assert_eq!((r.code(), r.next.clone()), (354, Next::ReadData));
    let env = s.envelope();
    assert_eq!(env.mail_from, "a@x.test");
    assert_eq!(env.rcpt_to, ["b@y.test", "c@y.test"]);
    assert_eq!(env.helo, "client.test");
    assert_eq!(
        env.mail_params,
        [("SIZE".to_string(), Some("10".to_string()))]
    );
    assert_eq!(
        s.data_result(DataOutcome::Stored("ID1".into())).text,
        "250 2.0.0 Ok: queued as ID1"
    );
    assert_eq!(
        codes(&mut s, &["DATA"]),
        [503],
        "transaction reset after DATA"
    );
}

#[test]
fn sequence_errors() {
    let mut s = session(SessionConfig::new("mx"));
    assert_eq!(
        codes(&mut s, &["MAIL FROM:<a@x>", "RCPT TO:<b@y>", "DATA"]),
        [503, 503, 503]
    );
    assert_eq!(
        codes(
            &mut s,
            &[
                "HELO c",
                "MAIL FROM:<a@x>",
                "MAIL FROM:<a@x>",
                "DATA",
                "RSET",
                "MAIL FROM:<a@x>"
            ]
        ),
        [250, 250, 503, 503, 250, 250]
    );
}

#[test]
fn ehlo_extensions_and_hiding() {
    let mut cfg = SessionConfig::new("mx");
    cfg.starttls_available = true;
    cfg.auth = SmtpAuth::AcceptAny;
    let mut s = session(cfg.clone());
    let ehlo = s.handle("EHLO c").text;
    for ext in [
        "SIZE 52428800",
        "PIPELINING",
        "STARTTLS",
        "AUTH PLAIN LOGIN",
        "ENHANCEDSTATUSCODES",
        "8BITMIME",
        "250 SMTPUTF8",
    ] {
        assert!(ehlo.contains(ext), "{ext} missing in\n{ehlo}");
    }
    assert!(ehlo.starts_with("250-mx greets c\r\n"));

    cfg.hidden_extensions = ["PIPELINING", "SMTPUTF8", "STARTTLS", "AUTH"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut s = session(cfg);
    let ehlo = s.handle("EHLO c").text;
    for gone in ["PIPELINING", "SMTPUTF8", "STARTTLS", "AUTH"] {
        assert!(!ehlo.contains(gone), "{gone} should be hidden\n{ehlo}");
    }
    assert!(ehlo.ends_with("250 8BITMIME"), "{ehlo}");
    assert_eq!(
        codes(
            &mut s,
            &["STARTTLS", "AUTH PLAIN", "MAIL FROM:<a@x> SMTPUTF8"]
        ),
        [502, 502, 555]
    );
}

#[test]
fn ehlo_without_extensions_is_single_line() {
    let mut cfg = SessionConfig::new("mx");
    cfg.hidden_extensions = [
        "SIZE",
        "PIPELINING",
        "ENHANCEDSTATUSCODES",
        "8BITMIME",
        "SMTPUTF8",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(session(cfg).handle("EHLO c").text, "250 mx greets c");
}

#[test]
fn size_limits() {
    let mut cfg = SessionConfig::new("mx");
    cfg.max_message_size = 100;
    let mut s = session(cfg);
    s.handle("EHLO c");
    assert_eq!(
        codes(
            &mut s,
            &[
                "MAIL FROM:<a@x> SIZE=101",
                "MAIL FROM:<a@x> SIZE=abc",
                "MAIL FROM:<a@x> SIZE=100"
            ]
        ),
        [552, 501, 250]
    );
    s.handle("RCPT TO:<b@y>");
    s.handle("DATA");
    assert_eq!(s.data_result(DataOutcome::TooLarge).code(), 552);
}

#[test]
fn max_recipients() {
    let mut cfg = SessionConfig::new("mx");
    cfg.max_recipients = 2;
    let mut s = session(cfg);
    assert_eq!(
        codes(
            &mut s,
            &[
                "EHLO c",
                "MAIL FROM:<a@x>",
                "RCPT TO:<1@y>",
                "RCPT TO:<2@y>",
                "RCPT TO:<3@y>"
            ]
        ),
        [250, 250, 250, 250, 452]
    );
}

#[test]
fn recipient_policies() {
    let mut cfg = SessionConfig::new("mx");
    cfg.recipients.allowed = Some(Regex::new(r"@allowed\.test$").expect("regex"));
    let mut s = session(cfg.clone());
    assert_eq!(
        codes(
            &mut s,
            &[
                "EHLO c",
                "MAIL FROM:<a@x>",
                "RCPT TO:<u@allowed.test>",
                "RCPT TO:<u@other.test>"
            ]
        ),
        [250, 250, 250, 550]
    );

    cfg.recipients.ignore_rejected = true;
    let mut s = session(cfg);
    assert_eq!(
        codes(
            &mut s,
            &[
                "EHLO c",
                "MAIL FROM:<a@x>",
                "RCPT TO:<u@other.test>",
                "DATA"
            ]
        ),
        [250, 250, 250, 354]
    );
    assert!(s.all_recipients_dropped());

    let mut cfg = SessionConfig::new("mx");
    cfg.recipients.reject_domains = vec!["blocked.test".into()];
    cfg.recipients.discard_domains = vec!["void.test".into()];
    let mut s = session(cfg);
    assert_eq!(
        codes(
            &mut s,
            &[
                "EHLO c",
                "MAIL FROM:<a@x>",
                "RCPT TO:<u@sub.blocked.test>",
                "RCPT TO:<u@void.test>",
                "RCPT TO:<u@ok.test>"
            ]
        ),
        [250, 250, 550, 250, 250]
    );
    assert_eq!(s.envelope().rcpt_to, ["u@ok.test"]);

    let mut cfg = SessionConfig::new("mx");
    cfg.recipients.accept_domains = vec!["only.test".into()];
    let mut s = session(cfg);
    assert_eq!(
        codes(
            &mut s,
            &[
                "EHLO c",
                "MAIL FROM:<a@x>",
                "RCPT TO:<u@only.test>",
                "RCPT TO:<u@else.test>"
            ]
        ),
        [250, 250, 250, 550]
    );
}

#[test]
fn address_errors() {
    let mut s = session(SessionConfig::new("mx"));
    s.handle("EHLO c");
    assert_eq!(
        codes(
            &mut s,
            &[
                "MAIL FROM:a@x",
                "MAIL FROM:<a b@x>",
                "MAIL FROM:<a@x> AUTH=bad=1",
                "MAIL FROM:<>"
            ]
        ),
        [501, 553, 501, 250]
    );
    assert_eq!(
        codes(&mut s, &["RCPT TO:<>", "RCPT TO:<Postmaster>"]),
        [553, 250]
    );
}

fn auth_cfg() -> SessionConfig {
    let mut cfg = SessionConfig::new("mx");
    cfg.auth = SmtpAuth::AcceptAny;
    cfg
}

#[test]
fn auth_plain_inline_and_challenge() {
    let mut s = session(auth_cfg());
    s.handle("EHLO c");
    // "\0user\0pass"
    assert_eq!(s.handle("AUTH PLAIN AHVzZXIAcGFzcw==").code(), 235);
    assert_eq!(s.envelope().auth_user.as_deref(), Some("user"));
    assert_eq!(s.handle("AUTH PLAIN").code(), 503, "already authenticated");

    let mut s = session(auth_cfg());
    s.handle("EHLO c");
    let r = s.handle("AUTH PLAIN");
    assert_eq!((r.text.as_str(), r.next), ("334 ", Next::AuthContinue));
    assert_eq!(s.auth_response("AHVzZXIAcGFzcw==").code(), 235);
}

#[test]
fn auth_login_and_cancel() {
    let mut s = session(auth_cfg());
    s.handle("EHLO c");
    assert_eq!(s.handle("AUTH LOGIN").text, "334 VXNlcm5hbWU6");
    assert_eq!(s.auth_response("dXNlcg==").text, "334 UGFzc3dvcmQ6");
    assert_eq!(s.auth_response("cGFzcw==").code(), 235);
    assert_eq!(s.envelope().auth_user.as_deref(), Some("user"));

    let mut s = session(auth_cfg());
    s.handle("EHLO c");
    s.handle("AUTH LOGIN");
    assert_eq!(s.auth_response("*").code(), 501);
    assert_eq!(s.handle("AUTH PLAIN !!!").code(), 501);
    assert_eq!(
        s.handle("AUTH CRAM-MD5").code(),
        504,
        "CRAM-MD5 removed (ADR-016)"
    );
    assert_eq!(s.handle("AUTH").code(), 501);
}

#[test]
fn auth_not_offered_or_during_transaction() {
    let mut s = session(SessionConfig::new("mx"));
    s.handle("EHLO c");
    assert_eq!(s.handle("AUTH PLAIN AHVzZXIAcGFzcw==").code(), 502);
    let mut s = session(auth_cfg());
    s.handle("EHLO c");
    s.handle("MAIL FROM:<a@x>");
    assert_eq!(s.handle("AUTH PLAIN AHVzZXIAcGFzcw==").code(), 503);
}

#[test]
fn starttls_requirements_and_reset() {
    let mut cfg = SessionConfig::new("mx");
    cfg.starttls_available = true;
    cfg.require_tls = true;
    let mut s = session(cfg);
    s.handle("EHLO c");
    assert_eq!(
        codes(&mut s, &["MAIL FROM:<a@x>", "RSET", "STARTTLS x"]),
        [530, 530, 501]
    );
    let r = s.handle("STARTTLS");
    assert_eq!((r.code(), r.next), (220, Next::StartTls));
    s.tls_established("TLSv1.3".into());
    assert_eq!(
        codes(&mut s, &["MAIL FROM:<a@x>"]),
        [503],
        "EHLO required again after STARTTLS"
    );
    let ehlo = s.handle("EHLO c").text;
    assert!(!ehlo.contains("STARTTLS"));
    assert_eq!(codes(&mut s, &["STARTTLS", "MAIL FROM:<a@x>"]), [503, 250]);
    assert_eq!(s.envelope().tls.as_deref(), Some("TLSv1.3"));
}

#[test]
fn misc_commands() {
    let mut s = session(SessionConfig::new("mx"));
    assert_eq!(
        codes(
            &mut s,
            &["NOOP", "VRFY x", "EXPN x", "HELP", "XYZZY", "STARTTLS"]
        ),
        [250, 502, 502, 502, 500, 502]
    );
    let q = s.handle("QUIT");
    assert_eq!((q.code(), q.next), (221, Next::Close));
}

#[test]
fn auth_credentials_are_checked_and_required() {
    let creds = spoolbox_auth::Credentials::parse("user:pass").expect("creds");
    let mut cfg = SessionConfig::new("mx");
    cfg.auth = SmtpAuth::Credentials(Arc::new(creds));
    let cfg = Arc::new(cfg);

    let mut s = Session::new(cfg.clone(), "127.0.0.1", None);
    assert!(s.handle("EHLO c").text.contains("AUTH PLAIN LOGIN"));
    assert_eq!(
        s.handle("MAIL FROM:<a@b.test>").text,
        "530 5.7.0 Authentication required"
    );
    // "\0user\0wrong"
    assert_eq!(
        s.handle("AUTH PLAIN AHVzZXIAd3Jvbmc=").text,
        "535 5.7.8 Authentication credentials invalid"
    );
    assert_eq!(s.envelope().auth_user, None);
    s.handle("AUTH LOGIN");
    s.auth_response("dXNlcg==");
    assert_eq!(s.auth_response("cGFzcw==").code(), 235);
    assert_eq!(
        codes(
            &mut s,
            &["MAIL FROM:<a@b.test>", "RCPT TO:<c@d.test>", "DATA"]
        ),
        [250, 250, 354]
    );

    // With STARTTLS required (the default when credentials are set), TLS comes first.
    let mut cfg = (*cfg).clone();
    cfg.require_tls = true;
    cfg.starttls_available = true;
    let mut s = Session::new(Arc::new(cfg), "127.0.0.1", None);
    s.handle("EHLO c");
    assert_eq!(
        s.handle("AUTH PLAIN AHVzZXIAcGFzcw==").text,
        "530 5.7.0 Must issue a STARTTLS command first"
    );
}

#[test]
fn chaos_triggers_fail_commands() {
    let mut cfg = SessionConfig::new("mx");
    cfg.auth = SmtpAuth::AcceptAny;
    cfg.chaos = Some(Arc::new(spoolbox_core::chaos::Chaos::new(
        spoolbox_core::chaos::Triggers::parse("sender:451:100,auth:535:100").expect("triggers"),
    )));
    let mut s = session(cfg.clone());
    s.handle("EHLO c");
    assert_eq!(
        s.handle("AUTH PLAIN AHVzZXIAcGFzcw==").text,
        "535 Chaos authentication error"
    );
    assert_eq!(
        s.handle("MAIL FROM:<a@b.test>").text,
        "451 Chaos sender error"
    );
    cfg.chaos = Some(Arc::new(spoolbox_core::chaos::Chaos::new(
        spoolbox_core::chaos::Triggers::parse("recipient:452:100").expect("triggers"),
    )));
    let mut s = session(cfg);
    s.handle("EHLO c");
    assert_eq!(
        codes(&mut s, &["MAIL FROM:<a@b.test>", "RCPT TO:<c@d.test>"]),
        [250, 452]
    );
}
