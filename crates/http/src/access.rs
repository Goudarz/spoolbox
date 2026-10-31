//! Request gatekeeping, in this order: Host allowlist,
//! CORS origin check, then HTTP Basic authentication.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use spoolbox_auth::Credentials;

use crate::HostPolicy;
use crate::api::text;

/// Who may use `POST /api/v1/send`.
#[derive(Debug, Clone, Default)]
pub enum SendAuth {
    /// The web UI credentials (if any) apply.
    #[default]
    SameAsUi,
    /// No credentials needed (`--send-api-auth-accept-any`).
    AcceptAny,
    /// Only these credentials (`--send-api-auth-file`).
    Credentials(Arc<Credentials>),
}

#[derive(Debug, Clone, Default)]
pub struct AccessPolicy {
    /// Starts and ends with `/`.
    pub webroot: String,
    pub hosts: HostPolicy,
    pub ui_auth: Option<Arc<Credentials>>,
    pub send_auth: SendAuth,
    /// Lower-case `host[:port]`; `*` allows any origin.
    pub cors: Vec<String>,
}

/// The user name from HTTP Basic auth, for messages stored through the send API.
#[derive(Debug, Clone)]
pub struct AuthUser(pub String);

pub(crate) async fn guard(
    State(policy): State<Arc<AccessPolicy>>,
    mut req: Request,
    next: Next,
) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if !policy.hosts.allows(host.as_deref()) {
        tracing::warn!(
            host = host.as_deref().unwrap_or_default(),
            "blocked request with disallowed Host header"
        );
        return text(
            StatusCode::FORBIDDEN,
            "Blocked due to disallowed Host header",
        );
    }

    let path = req.uri().path().to_string();
    let under_api = path.starts_with(&format!("{}api/", policy.webroot));
    let html_view =
        path.starts_with(&format!("{}view/", policy.webroot)) && path.ends_with(".html");
    let mut allow_origin = None;
    if (under_api || html_view)
        && let Some(origin) = req.headers().get(header::ORIGIN)
    {
        let origin_host = origin_host(origin.to_str().unwrap_or_default());
        let same = origin_host
            .as_deref()
            .zip(host.as_deref())
            .is_some_and(|(o, h)| o.eq_ignore_ascii_case(h));
        let listed = origin_host.as_deref().is_some_and(|o| {
            policy
                .cors
                .iter()
                .any(|c| c == "*" || c.eq_ignore_ascii_case(o))
        });
        if !same && !listed {
            tracing::warn!(
                origin = origin_host.as_deref().unwrap_or("?"),
                "blocked request from unauthorized origin"
            );
            return text(StatusCode::FORBIDDEN, "Blocked due to CORS violation");
        }
        allow_origin = Some(origin.clone());
    }

    let is_send = req.method() == Method::POST && path == format!("{}api/v1/send", policy.webroot);
    let cors_preflight = !policy.cors.is_empty() && req.method() == Method::OPTIONS;
    let required = if is_send {
        match &policy.send_auth {
            SendAuth::AcceptAny => None,
            SendAuth::Credentials(c) => Some(c),
            SendAuth::SameAsUi => policy.ui_auth.as_ref(),
        }
    } else if cors_preflight {
        // Browsers send preflight requests without credentials.
        None
    } else {
        policy.ui_auth.as_ref()
    };
    let basic = basic_credentials(req.headers());
    if let Some(creds) = required {
        match &basic {
            Some((user, pass)) if creds.verify(user, pass) => {}
            _ => return unauthorized(),
        }
    }
    if let Some((user, _)) = basic {
        req.extensions_mut().insert(AuthUser(user));
    }

    // OPTIONS is answered for every `/api/v1/` path (CORS preflight).
    let mut res = if req.method() == Method::OPTIONS
        && path.starts_with(&format!("{}api/v1/", policy.webroot))
    {
        text(StatusCode::OK, "")
    } else {
        next.run(req).await
    };
    if let Some(origin) = allow_origin {
        let h = res.headers_mut();
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, DELETE, PUT, OPTIONS"),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("*"),
        );
        h.append(header::VARY, HeaderValue::from_static("Origin"));
    }
    res
}

fn unauthorized() -> Response {
    let mut res = text(StatusCode::UNAUTHORIZED, "Unauthorized.\n");
    res.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"Login\""),
    );
    res.into_response()
}

/// `host[:port]` of an `Origin` header value.
fn origin_host(origin: &str) -> Option<String> {
    let rest = origin.split_once("://").map_or(origin, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, encoded) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = B64.decode(encoded.trim()).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (user, pass) = decoded.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_helpers() {
        assert_eq!(
            origin_host("https://App.test:3000").as_deref(),
            Some("app.test:3000")
        );
        assert_eq!(origin_host("null").as_deref(), Some("null"));
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic dXNlcjpwOmFzcw=="),
        );
        assert_eq!(basic_credentials(&h), Some(("user".into(), "p:ass".into())));
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer x"));
        assert_eq!(basic_credentials(&h), None);
    }
}
