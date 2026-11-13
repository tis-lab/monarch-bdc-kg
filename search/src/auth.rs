//! Optional API-key access control.
//!
//! With `BDC_SEARCH_API_KEY` set, every route except the health probes needs the key, sent as
//! either header:
//!
//!   X-API-Key: <key>
//!   Authorization: Bearer <key>
//!
//! Without it, the API is open. The probes stay open either way, so the cluster can check the
//! pod without knowing the key.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

pub const API_KEY_HEADER: &str = "x-api-key";

#[derive(Clone)]
pub struct ApiKey(Arc<str>);

impl ApiKey {
    pub fn new(key: &str) -> anyhow::Result<ApiKey> {
        anyhow::ensure!(!key.trim().is_empty(), "the API key is empty");
        Ok(ApiKey(key.into()))
    }

    /// Compares every byte whatever the first difference, so response time doesn't reveal how
    /// much of a guess was right.
    fn matches(&self, sent: &str) -> bool {
        let (key, sent) = (self.0.as_bytes(), sent.as_bytes());
        key.len() == sent.len() && key.iter().zip(sent).fold(0, |diff, (a, b)| diff | (a ^ b)) == 0
    }
}

/// The key a request sent, from either header.
fn sent_key(headers: &HeaderMap) -> Option<&str> {
    let header = |name| headers.get(name).and_then(|v| v.to_str().ok());
    header(API_KEY_HEADER)
        .or_else(|| header(header::AUTHORIZATION.as_str())?.strip_prefix("Bearer "))
}

/// Middleware: pass the request on when it carries the key, else 401.
pub async fn require_api_key(State(key): State<ApiKey>, request: Request, next: Next) -> Response {
    match sent_key(request.headers()) {
        Some(sent) if key.matches(sent) => next.run(request).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(json!({"detail": "Missing or invalid API key"})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{router, tests::app};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn status(api_key: Option<&str>, uri: &str, headers: &[(&str, &str)]) -> StatusCode {
        let mut request = Request::get(uri);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let key = api_key.map(|k| ApiKey::new(k).unwrap());
        router(app(), key)
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    const SEARCH: &str = "/v3/api/search?q=asthma";

    #[tokio::test]
    async fn without_a_configured_key_everything_is_open() {
        assert_eq!(status(None, SEARCH, &[]).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn with_a_key_every_api_route_needs_it() {
        for uri in [
            SEARCH,
            "/v3/api/autocomplete?q=as",
            "/bdc/concepts?q=a",
            "/version",
        ] {
            assert_eq!(
                status(Some("s3cret"), uri, &[]).await,
                StatusCode::UNAUTHORIZED,
                "{uri}"
            );
        }
    }

    #[tokio::test]
    async fn either_header_carries_the_key() {
        let key = Some("s3cret");
        assert_eq!(
            status(key, SEARCH, &[("X-API-Key", "s3cret")]).await,
            StatusCode::OK
        );
        assert_eq!(
            status(key, SEARCH, &[("Authorization", "Bearer s3cret")]).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn a_wrong_key_is_refused() {
        let key = Some("s3cret");
        for sent in [
            ("X-API-Key", "s3cre"),
            ("X-API-Key", "s3cret!"),
            ("Authorization", "s3cret"),
        ] {
            assert_eq!(
                status(key, SEARCH, &[sent]).await,
                StatusCode::UNAUTHORIZED,
                "{sent:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_health_probes_stay_open() {
        for uri in ["/health/live", "/health/ready"] {
            assert_eq!(
                status(Some("s3cret"), uri, &[]).await,
                StatusCode::OK,
                "{uri}"
            );
        }
    }

    #[test]
    fn an_empty_key_is_a_configuration_error() {
        assert!(ApiKey::new("  ").is_err());
    }
}
