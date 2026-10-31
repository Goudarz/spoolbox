//! HTTP server: health endpoints, API v1, the API v2 preview,
//! WebSocket and SSE events, HTTP Basic auth, CORS, the Host allowlist and security headers.

// Handlers return `Result<_, Response>` (axum's idiom); the error is moved straight into the reply.
#![allow(clippy::result_large_err)]

mod access;
mod api;
mod events;
mod host;
mod tls;
mod ui;
mod v1;
mod v2;

use std::future::Future;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::middleware;
use axum::response::Response;
use axum::routing::get;
use tower_http::compression::CompressionLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

pub use access::{AccessPolicy, SendAuth};
pub use api::{Api, InstanceInfo};
pub use events::spawn_bridge;
pub use host::HostPolicy;
pub use tls::{Peer, TlsListener};

/// Request bodies above this size are rejected.
/// The send API uses `--max-message-size` instead.
pub const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;

/// API responses must never be rendered as documents or framed by other sites.
const API_CSP: &str = "default-src 'none'; frame-ancestors 'self'";

#[derive(Debug, Clone)]
pub struct HttpOptions {
    /// Starts and ends with `/`.
    pub webroot: String,
    pub hosts: HostPolicy,
    pub ui_auth: Option<Arc<spoolbox_auth::Credentials>>,
    pub send_auth: SendAuth,
    /// Lower-case `host[:port]` origins; `*` allows any.
    pub cors: Vec<String>,
    pub compression: bool,
}

impl HttpOptions {
    pub fn new(webroot: impl Into<String>) -> Self {
        Self {
            webroot: webroot.into(),
            hosts: HostPolicy::default(),
            ui_auth: None,
            send_auth: SendAuth::default(),
            cors: Vec::new(),
            compression: true,
        }
    }
}

/// Shared readiness flag for `/readyz`.
#[derive(Debug, Clone, Default)]
pub struct Readiness(Arc<AtomicBool>);

impl Readiness {
    pub fn set(&self, ready: bool) {
        self.0.store(ready, Ordering::SeqCst);
    }

    pub fn is_ready(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct ViewState {
    api: Api,
    webroot: String,
}

pub fn router(opts: &HttpOptions, ready: Readiness, api: Api) -> Router {
    // Probes bypass the Host check and authentication: Kubernetes sends the pod IP.
    let health = Router::new()
        .route("/livez", get(|| async { StatusCode::OK }))
        .route("/readyz", get(readyz))
        .with_state(ready);

    let pages = Router::new()
        .route("/", get(ui::page))
        .route("/search", get(ui::page))
        .route("/m/{id}", get(ui::page_with_id))
        .with_state(opts.webroot.clone())
        .route("/assets/{*path}", get(asset))
        .route(
            "/favicon.svg",
            get(|| ui::file(Path("favicon.svg".to_string()))),
        );

    let view = Router::new()
        .route("/view/{file}", get(view))
        .with_state(ViewState {
            api: api.clone(),
            webroot: opts.webroot.clone(),
        });

    // Inside the nested router paths are seen without the webroot.
    let policy = Arc::new(AccessPolicy {
        webroot: "/".into(),
        hosts: opts.hosts.clone(),
        ui_auth: opts.ui_auth.clone(),
        send_auth: opts.send_auth.clone(),
        cors: opts.cors.clone(),
    });
    let app = v1::routes()
        .merge(v2::routes())
        .route("/api/events", get(events::websocket))
        .with_state(api)
        .merge(view)
        .merge(pages)
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(
            policy.clone(),
            access::guard,
        ));

    let inner = health.merge(app);
    let root = if opts.webroot == "/" {
        inner
    } else {
        // `/prefix` itself is answered by the UI page handler, which redirects to `/prefix/`.
        let prefix = opts.webroot.trim_end_matches('/').to_string();
        Router::new()
            .route(
                &opts.webroot,
                get(ui::page)
                    .with_state(opts.webroot.clone())
                    .layer(middleware::from_fn_with_state(policy, access::guard)),
            )
            .nest(&prefix, inner)
            .fallback(not_found)
    };

    let root = if opts.compression {
        root.layer(CompressionLayer::new().gzip(true))
    } else {
        root
    };
    root.layer(SetResponseHeaderLayer::overriding(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    ))
    .layer(SetResponseHeaderLayer::if_not_present(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    ))
    .layer(SetResponseHeaderLayer::if_not_present(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(API_CSP),
    ))
    .layer(SetResponseHeaderLayer::if_not_present(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("SAMEORIGIN"),
    ))
    .layer(TraceLayer::new_for_http())
}

async fn readyz(State(ready): State<Readiness>) -> StatusCode {
    if ready.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn not_found() -> Response {
    api::not_found()
}

async fn asset(Path(path): Path<String>) -> Response {
    ui::file(Path(format!("assets/{path}"))).await
}

/// Whether the web UI was embedded at build time.
pub fn ui_built() -> bool {
    ui::is_built()
}

/// `/view/{id}.html` and `/view/{id}.txt` (`id` may be `latest`).
async fn view(
    State(v): State<ViewState>,
    Path(file): Path<String>,
    Query(p): Query<api::Params>,
) -> Response {
    if let Some(id) = file.strip_suffix(".html") {
        v1::view_html(v.api, v.webroot, id.to_string(), p).await
    } else if let Some(id) = file.strip_suffix(".txt") {
        v1::view_text(v.api, id.to_string(), p).await
    } else {
        api::not_found()
    }
}

/// Serves until `shutdown` resolves, then drains in-flight requests.
pub async fn serve(
    listener: tokio::net::TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    axum::serve(listener, app.into_make_service_with_connect_info::<Peer>())
        .with_graceful_shutdown(shutdown)
        .await
}

/// Like [`serve`], over HTTPS.
pub async fn serve_tls(
    listener: tokio::net::TcpListener,
    tls: Arc<tokio_rustls::rustls::ServerConfig>,
    app: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    axum::serve(
        TlsListener::new(listener, tls)?,
        app.into_make_service_with_connect_info::<Peer>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

#[cfg(test)]
mod api_tests;
#[cfg(test)]
mod tests;
