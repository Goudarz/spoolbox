mod cli;
mod logging;
mod readyz;
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
    if m.get_flag("https") {
        return fail(
            EXIT_CONFIG,
            "--https is not supported yet (HTTPS for the web UI is planned for phase P4)",
        );
    }
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
    match readyz::check(s.listen.connect_addr(), &s.webroot, wait).await {
        Ok(()) => ExitCode::SUCCESS,
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
        spoolbox_app::server_options(s),
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
    tracing::info!(addr = %http_local, url = %format!("http://{http_local}{}", s.http.webroot), "HTTP listening");
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
        },
    );
    let bridge_task = spoolbox_http::spawn_bridge(api.clone(), stop_rx.clone());
    let app = spoolbox_http::router(&http_options(s), ready.clone(), api);
    ready.set(true);
    let shutdown_ready = ready.clone();
    let result = spoolbox_http::serve(http_listener, app, async move {
        shutdown_signal().await;
        shutdown_ready.set(false);
        tracing::info!("shutting down");
        let _ = stop_tx.send(true);
    })
    .await
    .map_err(|e| format!("HTTP server failed: {e}"));

    let drained = tokio::time::timeout(spoolbox_app::SHUTDOWN_GRACE, async {
        let _ = smtp_task.await;
        let _ = index_task.await;
        let _ = maint_task.await;
        let _ = bridge_task.await;
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
