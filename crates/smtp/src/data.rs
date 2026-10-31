//! Decoder for the SMTP DATA stream (RFC 5321 §4.5.2): removes dot-stuffing and
//! detects the end-of-data marker.
//!
//! Only `<CRLF>.<CRLF>` ends the message, where the `<CRLF>` is a real CRLF (or the
//! start of DATA). A dot line after a bare LF does not terminate, which prevents
//! SMTP smuggling via `<LF>.<CRLF>` or `<LF>.<LF>`. Bare LF and bare CR inside the
//! message are passed through and counted.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// At the start of a line; `true` if the previous line ended with CRLF.
    LineStart(bool),
    /// A line started with `.`; `true` if it followed a CRLF.
    Dot(bool),
    /// `.` then CR at the start of a line.
    DotCr(bool),
    Body,
    /// CR seen inside a line.
    Cr,
    Done,
}

#[derive(Debug, Clone)]
pub struct DataDecoder {
    state: State,
    size: u64,
    limit: u64,
    bare_lf: u32,
    bare_cr: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Input bytes consumed. Bytes after the end marker are left for the next command.
    pub consumed: usize,
    pub done: bool,
}

impl DataDecoder {
    /// `limit` is the maximum decoded size in bytes; 0 means unlimited.
    pub fn new(limit: u64) -> Self {
        Self {
            state: State::LineStart(true),
            size: 0,
            limit,
            bare_lf: 0,
            bare_cr: 0,
        }
    }

    pub fn too_large(&self) -> bool {
        self.limit > 0 && self.size > self.limit
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn bare_line_endings(&self) -> (u32, u32) {
        (self.bare_lf, self.bare_cr)
    }

    /// Decodes `input`, appending message bytes to `out` (nothing is appended once
    /// the size limit is exceeded, but the input is still consumed up to the end marker).
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Progress {
        for (i, &b) in input.iter().enumerate() {
            self.step(b, out);
            if self.state == State::Done {
                return Progress {
                    consumed: i + 1,
                    done: true,
                };
            }
        }
        Progress {
            consumed: input.len(),
            done: false,
        }
    }

    fn emit(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        self.size += bytes.len() as u64;
        if !self.too_large() {
            out.extend_from_slice(bytes);
        }
    }

    fn step(&mut self, b: u8, out: &mut Vec<u8>) {
        self.state = match (self.state, b) {
            (State::Done, _) => State::Done,
            (State::LineStart(crlf), b'.') => State::Dot(crlf),
            (State::LineStart(_) | State::Body, b'\r') => State::Cr,
            (State::LineStart(_) | State::Body, b'\n') => {
                self.bare_lf += 1;
                self.emit(b"\n", out);
                State::LineStart(false)
            }
            (State::LineStart(_) | State::Body, other) => {
                self.emit(&[other], out);
                State::Body
            }
            // The leading dot of a line is removed (dot-stuffing).
            (State::Dot(crlf), b'\r') => State::DotCr(crlf),
            (State::Dot(_), b'\n') => {
                self.bare_lf += 1;
                self.emit(b"\n", out);
                State::LineStart(false)
            }
            (State::Dot(_), other) => {
                self.emit(&[other], out);
                State::Body
            }
            (State::DotCr(true), b'\n') => State::Done,
            (State::DotCr(false), b'\n') => {
                self.emit(b"\r\n", out);
                State::LineStart(true)
            }
            (State::DotCr(_), b'\r') => {
                self.bare_cr += 1;
                self.emit(b"\r", out);
                State::Cr
            }
            (State::DotCr(_), other) => {
                self.bare_cr += 1;
                self.emit(&[b'\r', other], out);
                State::Body
            }
            (State::Cr, b'\n') => {
                self.emit(b"\r\n", out);
                State::LineStart(true)
            }
            (State::Cr, b'\r') => {
                self.bare_cr += 1;
                self.emit(b"\r", out);
                State::Cr
            }
            (State::Cr, other) => {
                self.bare_cr += 1;
                self.emit(&[b'\r', other], out);
                State::Body
            }
        };
    }
}

/// Inverse of decoding, for tests and clients: dot-stuff `body` and append the end marker.
pub fn encode(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    let mut line_start = true;
    for &b in body {
        if line_start && b == b'.' {
            out.push(b'.');
        }
        out.push(b);
        line_start = b == b'\n';
    }
    if !out.is_empty() && !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn decode_all(input: &[u8], limit: u64) -> (Vec<u8>, Progress, DataDecoder) {
        let mut d = DataDecoder::new(limit);
        let mut out = Vec::new();
        let p = d.feed(input, &mut out);
        (out, p, d)
    }

    #[test]
    fn basic_and_dot_unstuffing() {
        let (out, p, _) = decode_all(b"Subject: x\r\n\r\n..leading dot\r\n.\r\nQUIT\r\n", 0);
        assert_eq!(out, b"Subject: x\r\n\r\n.leading dot\r\n");
        assert!(p.done);
        assert_eq!(
            p.consumed,
            b"Subject: x\r\n\r\n..leading dot\r\n.\r\n".len()
        );
    }

    #[test]
    fn empty_message() {
        let (out, p, _) = decode_all(b".\r\n", 0);
        assert!(p.done);
        assert!(out.is_empty());
    }

    #[test]
    fn smuggling_markers_do_not_terminate() {
        for input in [
            &b"a\n.\r\nMAIL FROM:<x@y>\r\n"[..],
            b"a\n.\nb\r\n",
            b"a\r.\r\nb\r\n",
            b"a\r\n.\nb\r\n",
        ] {
            let (_, p, d) = decode_all(input, 0);
            assert!(
                !p.done,
                "{:?} must not end DATA",
                String::from_utf8_lossy(input)
            );
            let (lf, cr) = d.bare_line_endings();
            assert!(lf + cr > 0);
        }
        let (out, p, _) = decode_all(b"a\n.\r\nb\r\n.\r\n", 0);
        assert!(p.done);
        assert_eq!(out, b"a\n\r\nb\r\n");
    }

    #[test]
    fn split_across_chunks() {
        let input = b"line\r\n..x\r\n.\r\n";
        for split in 0..input.len() {
            let mut d = DataDecoder::new(0);
            let mut out = Vec::new();
            let p1 = d.feed(&input[..split], &mut out);
            assert!(!p1.done);
            let p2 = d.feed(&input[split..], &mut out);
            assert!(p2.done, "split at {split}");
            assert_eq!(out, b"line\r\n.x\r\n");
        }
    }

    #[test]
    fn size_limit_stops_output_but_consumes_to_marker() {
        let (out, p, d) = decode_all(b"0123456789\r\n0123456789\r\n.\r\nNOOP\r\n", 15);
        assert!(p.done);
        assert!(d.too_large());
        assert!(out.len() <= 15);
        assert_eq!(p.consumed, 27);
    }

    proptest! {
        #[test]
        fn encode_then_decode_round_trips(lines in proptest::collection::vec("[ -~]{0,40}", 0..20)) {
            let body: Vec<u8> = lines.iter().flat_map(|l| format!("{l}\r\n").into_bytes()).collect();
            let (out, p, _) = decode_all(&encode(&body), 0);
            prop_assert!(p.done);
            prop_assert_eq!(out, body);
        }

        #[test]
        fn arbitrary_input_never_panics(data in proptest::collection::vec(any::<u8>(), 0..2048), limit in 0u64..64) {
            let _ = decode_all(&data, limit);
        }
    }
}
