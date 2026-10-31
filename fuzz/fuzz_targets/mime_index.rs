#![no_main]
//! Arbitrary messages must never panic the MIME indexer, and offsets must stay in bounds.

use libfuzzer_sys::fuzz_target;
use spoolbox_mime::{Limits, index, scan_headers};

fuzz_target!(|data: &[u8]| {
    let limits = Limits::default();
    let _ = scan_headers(data, &limits);
    if let Ok(ix) = index(data, &limits) {
        assert!(ix.parts.len() <= limits.max_parts);
        for p in &ix.parts {
            assert!(p.offset_header as usize <= data.len());
            assert!(p.offset_end as usize <= data.len());
        }
    }
});
