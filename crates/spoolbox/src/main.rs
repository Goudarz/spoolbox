mod cli;
mod client;
mod doctor;
mod logging;
mod readyz;
mod remote;
mod sample;

use std::process::ExitCode;

use spoolbox_config::{DiagnosticLevel, Settings, Sources};
use spoolbox_http::{HostPolicy, HttpOptions, Readiness, SendAuth};

/// Exit code for configuration and usage errors (same as clap).
const EXIT_CONFIG: u8 = 2;
const EXIT_RUNTIME: u8 = 1;

fn main() -> ExitCode {
    let matches = cli::command().get_matches();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return fail(EXIT_RUNTIME, &format!("cannot start async runtime: {e}")),
    };
    match matches.subcommand() {
        Some(("version", _)) => {
            println!(
                "spoolbox {} ({}/{})",
                spoolbox_core::VERSION,
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            ExitCode::SUCCESS
        }
        Some(("readyz", m)) => runtime.block_on(run_readyz(m)),
        Some(("reindex", m)) => runtime.block_on(run_reindex(m)),
        Some(("send-sample", m)) => runtime.block_on(run_send_sample(m)),
        Some(("doctor", m)) => runtime.block_on(run_doctor(cli::given_flags(m))),
        Some(("wait", m)) => runtime.block_on(run_wait(m)),
        Some(("export", m)) => runtime.block_on(run_export(m)),
        Some(("import", m)) => runtime.block_on(run_import(m)),
        Some(("mcp", m)) => runtime.block_on(run_mcp(m)),
        _ => runtime.block_on(run_server(cli::given_flags(&matches))),
    }
}

fn process_env() -> Vec<(String, String)> {
    std::env::vars_os()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

fn fail(code: u8, message: &str) -> ExitCode {
    eprintln!("spoolbox: error: {message}");
    ExitCode::from(code)
}

async fn run_readyz(m: &clap::ArgMatches) -> ExitCode {
    let mut cli = Vec::new();
    for key in ["listen", "webroot"] {
        if let Some(v) = m.get_one::<String>(key) {
            cli.push((key.to_string(), Some(v.clone())));
        }
    }
    let loaded = match spoolbox_config::load(&Sources {
        cli,
        env: process_env(),
    }) {
        Ok(l) => l,
        Err(e) => return fail(EXIT_CONFIG, &e.to_string()),
    };
    let wait = if m.get_flag("wait") {
        let raw = m.get_one::<String>("timeout").map_or("30s", String::as_str);
        match readyz::parse_duration(raw) {
            Some(d) => Some(d),
            None => {
                return fail(
                    EXIT_CONFIG,
                    &format!("invalid --timeout {raw:?} (expected e.g. 500ms, 30s, 2m)"),
                );
            }
        }
    } else {
        None
    };
    let s = &loaded.settings.http;
    let https = m.get_flag("https") || s.tls.is_some();
    match readyz::check(s.listen.connect_addr(), &s.webroot, wait, https).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(EXIT_RUNTIME, &e),
    }
}

/// A client for the server described by the config, `--listen`/`--webroot` and `--auth`.
fn client_for(m: &clap::ArgMatches) -> Result<client::Client, ExitCode> {
    let mut cli = Vec::new();
    for key in ["listen", "webroot"] {
        if let Some(v) = m.get_one::<String>(key) {
            cli.push((key.to_string(), Some(v.clone())));
        }
    }
    let loaded = spoolbox_config::load(&Sources {
        cli,
        env: process_env(),
    })
    .map_err(|e| fail(EXIT_CONFIG, &e.to_string()))?;
    let auth = m.get_one::<String>("auth").cloned().or_else(|| {
        std::env::var("SPOOLBOX_AUTH")
            .ok()
            .filter(|a| !a.is_empty())
    });
    if auth.as_deref().is_some_and(|a| !a.contains(':')) {
        return Err(fail(EXIT_CONFIG, "--auth must be user:password"));
    }
    Ok(client::Client::new(
        &loaded.settings.http,
        m.get_flag("https"),
        auth.as_deref(),
    ))
}

async fn run_doctor(cli: Vec<(String, Option<String>)>) -> ExitCode {
    let loaded = match spoolbox_config::load(&Sources {
        cli,
        env: process_env(),
    }) {
        Ok(l) => l,
        Err(e) => {
            println!("[FAIL] config       {e}");
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    println!("spoolbox {}", spoolbox_core::VERSION);
    if doctor::print(&doctor::run(&loaded).await) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_RUNTIME)
    }
}

async fn run_wait(m: &clap::ArgMatches) -> ExitCode {
    let client = match client_for(m) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let raw = m.get_one::<String>("timeout").map_or("30s", String::as_str);
    let Some(timeout) = readyz::parse_duration(raw) else {
        return fail(
            EXIT_CONFIG,
            &format!("invalid --timeout {raw:?} (expected e.g. 500ms, 30s, 2m)"),
        );
    };
    let query = m.get_one::<String>("query").map_or("", String::as_str);
    match remote::wait(&client, query, timeout, m.get_one::<i64>("since").copied()).await {
        Ok(remote::WaitResult::Found(summary)) => {
            if m.get_flag("json") {
                println!("{summary}");
            } else {
                println!("{}", summary["id"].as_str().unwrap_or_default());
            }
            ExitCode::SUCCESS
        }
        Ok(remote::WaitResult::TimedOut) => fail(
            EXIT_RUNTIME,
            &format!("no message matching {query:?} within {raw}"),
        ),
        Err(e) => fail(EXIT_RUNTIME, &e),
    }
}

async fn run_export(m: &clap::ArgMatches) -> ExitCode {
    let client = match client_for(m) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let out = std::path::PathBuf::from(m.get_one::<String>("out").map_or(".", String::as_str));
    let format = if m.get_one::<String>("format").map(String::as_str) == Some("mbox") {
        remote::Format::Mbox
    } else {
        remote::Format::Eml
    };
    match remote::export(
        &client,
        m.get_one::<String>("query").map(String::as_str),
        &out,
        format,
    )
    .await
    {
        Ok(n) => {
            eprintln!("exported {n} message(s) to {}", out.display());
            ExitCode::SUCCESS
        }
        Err(e) => fail(EXIT_RUNTIME, &e),
    }
}

/// Bridges newline-delimited JSON-RPC on stdin/stdout to the server's `/mcp` endpoint.
async fn run_mcp(m: &clap::ArgMatches) -> ExitCode {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let client = match client_for(m) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let response = match client
            .request(
                "POST",
                "mcp",
                Some((line.clone().into_bytes(), "application/json")),
            )
            .await
        {
            Ok(r) if r.status == 202 => continue,
            Ok(r) if r.ok() => r.text(),
            Ok(r) => mcp_error(&line, &r.error()),
            Err(e) => mcp_error(&line, &e),
        };
        if response.is_empty() {
            continue;
        }
        let line = format!("{response}\n");
        if out.write_all(line.as_bytes()).await.is_err() || out.flush().await.is_err() {
            break;
        }
    }
    ExitCode::SUCCESS
}

/// A JSON-RPC error for a request the server could not answer (notifications get none).
fn mcp_error(request: &str, message: &str) -> String {
    let id = serde_json::from_str::<serde_json::Value>(request)
        .ok()
        .and_then(|v| v.get("id").cloned());
    match id {
        Some(id) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32603, "message": format!("Spoolbox is not reachable: {message}") }
        })
        .to_string(),
        None => String::new(),
    }
}

async fn run_import(m: &clap::ArgMatches) -> ExitCode {
    let client = match client_for(m) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let paths: Vec<std::path::PathBuf> = m
        .get_many::<String>("paths")
        .into_iter()
        .flatten()
        .map(std::path::PathBuf::from)
        .collect();
    match remote::import(&client, &paths).await {
        Ok((imported, skipped)) => {
            eprintln!(
                "imported {imported} message(s){}",
                if skipped > 0 {
                    format!(", {skipped} duplicate(s) skipped")
                } else {
                    String::new()
                }
            );
            ExitCode::SUCCESS
        }
        Err(e) => fail(EXIT_RUNTIME, &e),
    }
}

async fn run_server(cli: Vec<(String, Option<String>)>) -> ExitCode {
    let loaded = match spoolbox_config::load(&Sources {
        cli,
        env: process_env(),
    }) {
        Ok(l) => l,
        Err(e) => return fail(EXIT_CONFIG, &e.to_string()),
    };
    if let Err(e) = logging::init(&loaded.settings.log) {
        return fail(EXIT_CONFIG, &format!("cannot initialise logging: {e}"));
    }
    if let Some(path) = &loaded.config_file {
        tracing::info!(path = %path.display(), "loaded configuration file");
    }
    for d in &loaded.diagnostics {
        match d.level {
            DiagnosticLevel::Info => tracing::info!("{}", d.message),
            DiagnosticLevel::Warn => tracing::warn!("{}", d.message),
        }
    }
    match serve(&loaded.settings).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::from(EXIT_RUNTIME)
        }
    }
}

async fn serve(s: &Settings) -> Result<(), String> {
    tracing::info!(version = spoolbox_core::VERSION, "starting spoolbox");
    let settings = s.clone();
    let core = tokio::task::spawn_blocking(move || spoolbox_app::Core::open(&settings))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("cannot open the database: {e}"))?;
    if core.store.is_temporary() {
        tracing::info!(path = %core.store.db_path().display(), "using a temporary database (deleted on exit; use --database to keep messages)");
    } else {
        tracing::info!(path = %core.store.db_path().display(), "using database");
    }

    let hostname = spoolbox_app::hostname();
    let tls = spoolbox_app::load_tls(s, &hostname).map_err(|e| e.to_string())?;
    let ui_tls = spoolbox_app::load_ui_tls(s, &hostname).map_err(|e| e.to_string())?;
    if let (Some(t), Some(spoolbox_config::TlsSource::Auto)) = (&ui_tls, &s.http.tls) {
        tracing::info!(fingerprint = %t.fingerprint, "generated a self-signed HTTPS certificate (SHA-256)");
    }
    if let (Some(t), Some(spoolbox_config::TlsSource::Auto)) = (&tls, &s.smtp.tls) {
        tracing::info!(fingerprint = %t.fingerprint, "generated a self-signed SMTP certificate (SHA-256)");
    }
    let mut session =
        spoolbox_app::session_config(s, &hostname, tls.is_some()).map_err(|e| e.to_string())?;
    session.chaos = core.chaos.clone();
    let smtp_listener = tokio::net::TcpListener::bind(s.smtp.listen.to_socket_addr()).await.map_err(|e| {
        format!("cannot listen on {} for SMTP: {e} (is another mail server already running? use --smtp to choose another address)", s.smtp.listen)
    })?;
    let http_listener = tokio::net::TcpListener::bind(s.http.listen.to_socket_addr()).await.map_err(|e| {
        format!("cannot listen on {} for HTTP: {e} (is another mail server already running? use --listen to choose another address)", s.http.listen)
    })?;
    let smtp_local = smtp_listener.local_addr().map_err(|e| e.to_string())?;
    let http_local = http_listener.local_addr().map_err(|e| e.to_string())?;

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let smtp = spoolbox_smtp::Server::new(
        session,
        tls.map(|t| t.config),
        core.ingest.clone(),
        spoolbox_smtp::ServerOptions {
            transcripts: core.transcripts.clone(),
            ..spoolbox_app::server_options(s)
        },
    );
    let smtp_task = tokio::spawn(smtp.run(smtp_listener, stop_rx.clone()));
    let index_task = tokio::spawn(core.indexer.clone().run(stop_rx.clone()));
    let maint_task = tokio::spawn(spoolbox_app::maintenance::run(
        core.store.clone(),
        spoolbox_app::maintenance_options(s),
        stop_rx.clone(),
    ));

    let mode = match (&s.smtp.tls, s.smtp.implicit_tls, s.smtp.require_starttls) {
        (None, _, _) => "no encryption",
        (Some(_), true, _) => "implicit TLS",
        (Some(_), false, true) => "STARTTLS required",
        (Some(_), false, false) => "STARTTLS optional",
    };
    tracing::info!(addr = %smtp_local, mode, auth = if s.smtp.auth.is_some() { "credentials" } else if s.smtp.auth_accept_any { "accept any" } else { "off" }, "SMTP listening");
    tracing::info!(addr = %http_local, url = %format!("{}://{http_local}{}", if ui_tls.is_some() { "https" } else { "http" }, s.http.webroot), "HTTP listening");
    let open_http = !s.http.listen.is_loopback() && s.http.auth.is_none();
    let open_smtp = !s.smtp.listen.is_loopback() && s.smtp.auth.is_none();
    if open_http || open_smtp {
        tracing::warn!(
            "listening on a non-loopback interface without authentication (--allow-unauthenticated-network)"
        );
    }
    if s.http.auth.is_some() {
        tracing::info!("HTTP basic authentication enabled");
    }

    let ready = Readiness::default();
    let api = spoolbox_http::Api::new(
        core.store.clone(),
        core.ingest.clone(),
        core.chaos.clone(),
        spoolbox_http::InstanceInfo {
            label: s.label.clone(),
            max_message_size: s.smtp.max_message_size,
            duplicates_ignored: s.storage.ignore_duplicate_ids,
            hide_delete_all_button: s.http.hide_delete_all_button,
            block_remote_css_and_fonts: s.http.block_remote_css_and_fonts,
        },
    );
    let api = api
        .with_transcripts(core.transcripts.clone())
        .with_mcp_delete(s.http.mcp_allow_delete);
    let bridge_task = spoolbox_http::spawn_bridge(api.clone(), stop_rx.clone());
    let webhook_task = s.webhook.url.clone().map(|url| {
        spoolbox_http::spawn_webhooks(
            api.clone(),
            spoolbox_http::WebhookOptions {
                url,
                limit_per_second: s.webhook.limit_per_second,
                secret: s.webhook.secret.clone(),
                label: s.label.clone(),
            },
            stop_rx.clone(),
        )
    });
    let app = spoolbox_http::router(&http_options(s), ready.clone(), api);
    ready.set(true);
    let shutdown_ready = ready.clone();
    let shutdown = async move {
        shutdown_signal().await;
        shutdown_ready.set(false);
        tracing::info!("shutting down");
        let _ = stop_tx.send(true);
    };
    let result = match ui_tls {
        Some(t) => spoolbox_http::serve_tls(http_listener, t.config, app, shutdown).await,
        None => spoolbox_http::serve(http_listener, app, shutdown).await,
    }
    .map_err(|e| format!("HTTP server failed: {e}"));

    let drained = tokio::time::timeout(spoolbox_app::SHUTDOWN_GRACE, async {
        let _ = smtp_task.await;
        let _ = index_task.await;
        let _ = maint_task.await;
        let _ = bridge_task.await;
        if let Some(task) = webhook_task {
            let _ = task.await;
        }
    })
    .await;
    if drained.is_err() {
        tracing::warn!(
            "background tasks did not finish within {:?}",
            spoolbox_app::SHUTDOWN_GRACE
        );
    }
    drop(core);
    result
}

fn http_options(s: &Settings) -> HttpOptions {
    let mut o = HttpOptions::new(s.http.webroot.clone());
    o.hosts = HostPolicy::new(&s.http.allowed_hosts);
    o.ui_auth = s.http.auth.clone().map(std::sync::Arc::new);
    o.send_auth = match (&s.http.send_auth, s.http.send_auth_accept_any) {
        (Some(c), _) => SendAuth::Credentials(std::sync::Arc::new(c.clone())),
        (None, true) => SendAuth::AcceptAny,
        (None, false) => SendAuth::SameAsUi,
    };
    o.cors = s.http.cors_origins.clone();
    o.compression = s.http.compression;
    o
}

async fn run_reindex(m: &clap::ArgMatches) -> ExitCode {
    let Some(db) = m.get_one::<String>("database") else {
        return fail(EXIT_CONFIG, "a database path is required");
    };
    if !std::path::Path::new(db).exists() {
        return fail(EXIT_CONFIG, &format!("database {db} does not exist"));
    }
    let mut settings = Settings::default();
    settings.storage.database = Some(db.into());
    settings.storage.max_messages = 0;
    let core = match spoolbox_app::Core::open(&settings) {
        Ok(c) => c,
        Err(e) => return fail(EXIT_RUNTIME, &format!("cannot open {db}: {e}")),
    };
    let queued = match core.store.requeue_index_all() {
        Ok(n) => n,
        Err(e) => return fail(EXIT_RUNTIME, &e.to_string()),
    };
    let mut done = 0;
    loop {
        let n = core.indexer.run_batch().await;
        if n == 0 {
            break;
        }
        done += n;
    }
    println!("reindexed {done} messages ({queued} queued)");
    ExitCode::SUCCESS
}

async fn run_send_sample(m: &clap::ArgMatches) -> ExitCode {
    let mut cli = Vec::new();
    if let Some(v) = m.get_one::<String>("smtp") {
        cli.push(("smtp".to_string(), Some(v.clone())));
    }
    let loaded = match spoolbox_config::load(&Sources {
        cli,
        env: process_env(),
    }) {
        Ok(l) => l,
        Err(e) => return fail(EXIT_CONFIG, &e.to_string()),
    };
    let addr = loaded.settings.smtp.listen.connect_addr();
    let to = m
        .get_one::<String>("to")
        .map_or("developer@example.com", String::as_str);
    let which = m.get_one::<String>("sample").map_or("all", String::as_str);
    let selected: Vec<&sample::Sample> = sample::SAMPLES
        .iter()
        .filter(|s| which == "all" || s.name == which)
        .collect();
    if selected.is_empty() {
        let names: Vec<&str> = sample::SAMPLES.iter().map(|s| s.name).collect();
        return fail(
            EXIT_CONFIG,
            &format!(
                "unknown sample {which:?}; available: all, {}",
                names.join(", ")
            ),
        );
    }
    for s in selected {
        match sample::send(addr, to, s).await {
            Ok(reply) => println!("{:<14} {reply}", s.name),
            Err(e) => return fail(EXIT_RUNTIME, &format!("{}: {e}", s.name)),
        }
    }
    ExitCode::SUCCESS
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
}
