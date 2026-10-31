//! Embeds the built web UI (`web/dist`, produced by `npm --prefix web run build`) into the
//! binary. Without a build the server shows a short page explaining how to make one.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn files(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, base, out);
        } else if let Ok(rel) = path.strip_prefix(base) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            out.push((rel, path));
        }
    }
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "json" | "map" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let dist = manifest.join("../../web/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    let mut list = Vec::new();
    files(&dist, &dist, &mut list);
    list.sort();
    let mut code = String::from("pub(crate) static ASSETS: &[Asset] = &[\n");
    for (rel, path) in &list {
        println!("cargo:rerun-if-changed={}", path.display());
        let abs = path.canonicalize().unwrap_or_else(|_| path.clone());
        let _ = writeln!(
            code,
            "    Asset {{ path: {rel:?}, body: include_bytes!({:?}), mime: {:?} }},",
            abs.display().to_string(),
            mime(rel)
        );
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("out dir")).join("ui_assets.rs");
    std::fs::write(out, code).expect("write ui_assets.rs");
}
