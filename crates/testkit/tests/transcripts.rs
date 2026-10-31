//! Parses every transcript in `fixtures/smtp/`, and runs them against the server at
//! `SPOOLBOX_TEST_SMTP_ADDR` when that variable is set.

use std::path::PathBuf;
use std::time::Duration;

use spoolbox_testkit::transcript::Transcript;

fn transcripts() -> Vec<Transcript> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/smtp");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no transcripts in {}", dir.display());
    files
        .iter()
        .map(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let text = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{name}: {e}"));
            Transcript::parse(&name, &text).unwrap_or_else(|e| panic!("{e}"))
        })
        .collect()
}

#[test]
fn all_transcripts_parse() {
    assert!(transcripts().len() >= 4);
}

#[tokio::test]
async fn transcripts_pass_against_live_server() {
    let Ok(addr) = std::env::var("SPOOLBOX_TEST_SMTP_ADDR") else {
        eprintln!("SPOOLBOX_TEST_SMTP_ADDR not set; skipping live run");
        return;
    };
    let mut failures = Vec::new();
    for t in transcripts() {
        if let Err(e) = t.run(&addr, Duration::from_secs(5)).await {
            failures.push(e.to_string());
        }
    }
    assert!(
        failures.is_empty(),
        "{} transcript(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
