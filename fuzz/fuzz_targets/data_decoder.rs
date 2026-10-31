#![no_main]
//! The DATA decoder must never panic, must respect the size limit, and decoding an
//! encoded body must give the body back.

use libfuzzer_sys::fuzz_target;
use spoolbox_smtp::data::{DataDecoder, encode};

fuzz_target!(|data: &[u8]| {
    let limit = u64::from(data.first().copied().unwrap_or(0));
    let mut d = DataDecoder::new(limit);
    let mut out = Vec::new();
    let _ = d.feed(data, &mut out);
    if limit > 0 {
        assert!(out.len() as u64 <= limit);
    }

    // Round trip for CRLF-normalised input.
    let body: Vec<u8> = String::from_utf8_lossy(data).replace('\r', "").replace('\n', "\r\n").into_bytes();
    let body = if body.is_empty() || body.ends_with(b"\r\n") { body } else { [body, b"\r\n".to_vec()].concat() };
    let mut d = DataDecoder::new(0);
    let mut out = Vec::new();
    let p = d.feed(&encode(&body), &mut out);
    assert!(p.done);
    assert_eq!(out, body);
});
