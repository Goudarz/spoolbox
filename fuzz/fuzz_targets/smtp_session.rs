#![no_main]
//! Arbitrary command lines (including AUTH continuations) must never panic the session.

use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use spoolbox_smtp::session::{DataOutcome, Next, Session, SessionConfig, SmtpAuth};

fuzz_target!(|data: &[u8]| {
    let mut cfg = SessionConfig::new("fuzz.test");
    if data.first().is_some_and(|b| b & 1 == 1) {
        cfg.auth = SmtpAuth::AcceptAny;
    }
    cfg.starttls_available = data.first().is_some_and(|b| b & 2 == 2);
    cfg.max_recipients = 3;
    let mut s = Session::new(Arc::new(cfg), "127.0.0.1", None);
    for line in String::from_utf8_lossy(data).split("\r\n") {
        let reply = s.handle(line);
        assert!(reply.text.len() >= 3);
        match reply.next {
            Next::AuthContinue => {
                let _ = s.auth_response(line);
            }
            Next::ReadData => {
                let _ = s.data_result(DataOutcome::Stored("FUZZ".into()));
            }
            Next::StartTls => s.tls_established("TLSv1.3".into()),
            Next::Close | Next::Drop | Next::Continue => {}
        }
    }
});
