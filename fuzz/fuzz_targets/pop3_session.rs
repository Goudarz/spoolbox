#![no_main]
//! Arbitrary command lines must never panic the session, and an encoded message must never
//! end early.

use libfuzzer_sys::fuzz_target;
use spoolbox_pop3::{Entry, Session, SessionConfig, Step, encode};

fuzz_target!(|data: &[u8]| {
    let cfg = SessionConfig {
        stls_available: data.first().is_some_and(|b| b & 1 == 1),
        require_tls: data.first().is_some_and(|b| b & 2 == 2),
    };
    let mut s = Session::new(cfg, false);
    for line in String::from_utf8_lossy(data).split("\r\n") {
        match s.handle(line) {
            Step::Reply(text) | Step::Close(text) => assert!(text.starts_with(['+', '-'])),
            Step::StartTls(_) => s.tls_established(),
            Step::Authenticate { user, .. } => {
                if user.len() % 2 == 0 {
                    s.authenticated(vec![
                        Entry { id: "a".into(), size: 1 },
                        Entry { id: "b".into(), size: 2 },
                    ]);
                } else {
                    s.auth_failed();
                }
            }
            Step::Send { .. } | Step::Update { .. } => {}
        }
    }

    for lines in [None, Some(usize::from(data.first().copied().unwrap_or(0)))] {
        let encoded = encode(data, lines);
        assert!(encoded.ends_with(b".\r\n"));
        let body = &encoded[..encoded.len() - 3];
        assert!(body.is_empty() || body.ends_with(b"\r\n"));
        assert!(!body.windows(5).any(|w| w == b"\r\n.\r\n"), "early end");
        assert!(!body.starts_with(b".\r\n"), "early end");
    }
});
