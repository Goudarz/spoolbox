//! `spoolbox send-sample`: sends the built-in sample corpus over SMTP (NEW-22).

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

#[derive(Debug)]
pub struct Sample {
    pub name: &'static str,
    pub raw: &'static str,
}

pub const SAMPLES: &[Sample] = &[
    Sample {
        name: "plain",
        raw: include_str!("../../../fixtures/samples/plain.eml"),
    },
    Sample {
        name: "persian-html",
        raw: include_str!("../../../fixtures/samples/persian-html.eml"),
    },
    Sample {
        name: "attachments",
        raw: include_str!("../../../fixtures/samples/attachments.eml"),
    },
    Sample {
        name: "multilingual",
        raw: include_str!("../../../fixtures/samples/multilingual.eml"),
    },
    Sample {
        name: "signup-otp",
        raw: include_str!("../../../fixtures/samples/signup-otp.eml"),
    },
];

const TIMEOUT: Duration = Duration::from_secs(30);

/// Delivers one sample and returns the server's final reply.
pub async fn send(addr: SocketAddr, to: &str, sample: &Sample) -> Result<String, String> {
    let stream = tokio::time::timeout(TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| format!("timed out connecting to {addr}"))?
        .map_err(|e| format!("cannot connect to {addr}: {e} (is the server running?)"))?;
    let mut io = BufReader::new(stream);
    expect(&mut io, "220").await?;
    for (line, code) in [
        ("EHLO spoolbox-sample".to_string(), "250"),
        ("MAIL FROM:<samples@spoolbox.local>".to_string(), "250"),
        (format!("RCPT TO:<{to}>"), "250"),
        ("DATA".to_string(), "354"),
    ] {
        io.get_mut()
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        expect(&mut io, code).await?;
    }
    let crlf = sample.raw.replace("\r\n", "\n").replace('\n', "\r\n");
    io.get_mut()
        .write_all(&spoolbox_smtp::data::encode(crlf.as_bytes()))
        .await
        .map_err(|e| e.to_string())?;
    let reply = expect(&mut io, "250").await?;
    let _ = io.get_mut().write_all(b"QUIT\r\n").await;
    Ok(reply)
}

async fn expect(io: &mut BufReader<TcpStream>, code: &str) -> Result<String, String> {
    loop {
        let mut line = String::new();
        let n = tokio::time::timeout(TIMEOUT, io.read_line(&mut line))
            .await
            .map_err(|_| "timed out".to_string())?
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("connection closed by server".into());
        }
        let line = line.trim_end().to_string();
        if line.as_bytes().get(3) == Some(&b'-') {
            continue;
        }
        return if line.starts_with(code) {
            Ok(line)
        } else {
            Err(format!("unexpected reply: {line}"))
        };
    }
}
