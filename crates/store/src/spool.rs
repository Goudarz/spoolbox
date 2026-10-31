//! Streams a message body to a temporary file while receiving it, compressing on
//! the fly and hashing the raw bytes. Memory use is constant per message.

use std::fs::File;
use std::io::{self, BufWriter, Write};

use sha2::{Digest, Sha256};
use tempfile::TempPath;

/// Bytes kept in memory for the pre-commit header scan.
const HEAD_BYTES: usize = 1024 * 1024;

enum Sink {
    Plain(BufWriter<File>),
    Zstd(zstd::stream::write::Encoder<'static, BufWriter<File>>),
}

pub struct SpoolWriter {
    sink: Option<Sink>,
    path: TempPath,
    hasher: Sha256,
    size: u64,
    head: Vec<u8>,
}

impl std::fmt::Debug for SpoolWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpoolWriter")
            .field("size", &self.size)
            .finish()
    }
}

/// A completely received body, still in its temporary file (deleted on drop).
#[derive(Debug)]
pub struct Spooled {
    pub(crate) path: TempPath,
    pub(crate) codec: &'static str,
    pub raw_size: u64,
    pub sha256: String,
    /// The first bytes of the message (at least the header block for normal messages).
    pub head: Vec<u8>,
}

impl SpoolWriter {
    pub(crate) fn create(dir: &std::path::Path, compression: u8) -> io::Result<Self> {
        let (file, path) = tempfile::Builder::new()
            .prefix("rx-")
            .tempfile_in(dir)?
            .into_parts();
        let buffered = BufWriter::with_capacity(64 * 1024, file);
        let sink = match compression {
            0 => Sink::Plain(buffered),
            level => Sink::Zstd(zstd::stream::write::Encoder::new(
                buffered,
                zstd_level(level),
            )?),
        };
        Ok(Self {
            sink: Some(sink),
            path,
            hasher: Sha256::new(),
            size: 0,
            head: Vec::new(),
        })
    }

    pub fn finish(mut self) -> io::Result<Spooled> {
        let codec = match self.sink.take() {
            Some(Sink::Plain(mut w)) => {
                w.flush()?;
                w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
                "none"
            }
            Some(Sink::Zstd(enc)) => {
                let w = enc.finish()?;
                w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
                "zstd"
            }
            None => return Err(io::Error::other("spool already finished")),
        };
        let sha256 = self
            .hasher
            .finalize_reset()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(Spooled {
            path: self.path,
            codec,
            raw_size: self.size,
            sha256,
            head: std::mem::take(&mut self.head),
        })
    }
}

impl Write for SpoolWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = match self.sink.as_mut() {
            Some(Sink::Plain(w)) => w.write(buf)?,
            Some(Sink::Zstd(w)) => w.write(buf)?,
            None => return Err(io::Error::other("spool already finished")),
        };
        self.hasher.update(&buf[..n]);
        self.size += n as u64;
        if self.head.len() < HEAD_BYTES {
            let take = n.min(HEAD_BYTES - self.head.len());
            self.head.extend_from_slice(&buf[..take]);
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.sink.as_mut() {
            Some(Sink::Plain(w)) => w.flush(),
            Some(Sink::Zstd(w)) => w.flush(),
            None => Ok(()),
        }
    }
}

/// Compression levels (0–3) mapped to zstd levels.
fn zstd_level(level: u8) -> i32 {
    match level {
        1 => 1,
        2 => 3,
        _ => 19,
    }
}
