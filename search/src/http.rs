//! The HTTP API: Monarch's v3 routes and the /bdc harmonized-concept routes.
//!
//! Parameters and errors follow Monarch's FastAPI routes, so a Monarch client can't tell the
//! difference: invalid parameters are 422 with pydantic's message, unknown ids are 404.
//!
//! Paging comes in two kinds, as in Solr and Elasticsearch:
//!   offset   Monarch's `limit` + `offset`, up to monarch::MAX_RESULT_WINDOW deep
//!   cursor   `cursor=*`, then each response's `next_cursor`; any depth, at the same cost per
//!            page however deep (each page still scores every match)

use std::sync::Arc;

use anyhow::Result;
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::concepts::{HarmonizedConcepts, DEFAULT_MAX_MATCHES};
use crate::entity_index::{CursorError, EntityIndex, Page, SearchRequest, SearchResults};
use crate::monarch;

/// What the handlers share. Both indexes are read-only for the life of the process.
#[derive(Clone)]
pub struct App {
    pub entities: Arc<EntityIndex>,
    pub concepts: Option<Arc<HarmonizedConcepts>>,
    /// The release manifest packaged with the index (`manifest.json`), when there is one.
    pub release: Option<Arc<serde_json::Value>>,
}

impl App {
    /// What this server is serving: `/version`, and `bdc-search info`.
    pub fn info(&self) -> serde_json::Value {
        json!({
            "bdc_search": env!("CARGO_PKG_VERSION"),
            "schema_version": crate::schema::SCHEMA_VERSION,
            "index_version": self.entities.version(),
            "entities": self.entities.num_docs(),
            "concepts": self.concepts.as_ref().map(|c| c.len()),
            "release": self.release.as_deref(),
        })
    }

    /// Add BDC coverage to each hit that is a harmonized concept.
    pub fn with_coverage(&self, mut results: SearchResults) -> SearchResults {
        if let Some(concepts) = &self.concepts {
            for item in &mut results.items {
                item.bdc = concepts.coverage(&item.entity.id);
            }
        }
        results
    }
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/v3/api/autocomplete", get(autocomplete))
        .route("/v3/api/search", get(search))
        .route("/v3/api/entity/{id}", get(entity))
        .route("/bdc/harmonized-index.json", get(harmonized_index))
        .route("/bdc/concepts", get(search_concepts))
        .route("/bdc/concepts/{curie}", get(concept))
        // The indexes open before the listener binds, so serving at all means ready.
        .route("/health/live", get(|| async { "ok" }))
        .route("/health/ready", get(|| async { "ok" }))
        .route(
            "/version",
            get(|State(app): State<App>| async move { Json(app.info()) }),
        )
        .with_state(app)
}

/// Resolve on Ctrl-C, or on SIGTERM, which is how OpenShift stops a pod.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutting down");
}

// ── handlers ───────────────────────────────────────────────────────────────

async fn autocomplete(
    State(app): State<App>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let q = Params::parse(raw).required_text("q")?;
    let results = blocking({
        let app = app.clone();
        move || Ok(app.entities.autocomplete(&q)?)
    })
    .await?;
    Ok(Json(app.with_coverage(results)).into_response())
}

async fn search(State(app): State<App>, RawQuery(raw): RawQuery) -> Result<Response, ApiError> {
    let params = Params::parse(raw);
    let q = params.one("q").unwrap_or(monarch::MATCH_ALL).to_owned();
    let categories = params.all("category");
    let limit = params.count("limit", monarch::DEFAULT_LIMIT, Some(monarch::MAX_LIMIT))?;
    let page = page(&params, &q, &categories, limit, &app.entities)?;
    let results = blocking({
        let app = app.clone();
        move || {
            Ok(app.entities.search(&SearchRequest {
                q: &q,
                categories: &categories,
                limit,
                page,
            })?)
        }
    })
    .await?;
    Ok(Json(app.with_coverage(results)).into_response())
}

/// Offset paging unless a `cursor` is given. Each paging rule and its message, in one place.
fn page(
    params: &Params,
    q: &str,
    categories: &[String],
    limit: usize,
    entities: &EntityIndex,
) -> Result<Page, ApiError> {
    let offset = params.count("offset", 0, None)?;
    let Some(cursor) = params.one("cursor") else {
        if offset + limit > monarch::MAX_RESULT_WINDOW {
            let msg = format!(
                "offset + limit must be at most {}; page deeper with cursor=* and next_cursor",
                monarch::MAX_RESULT_WINDOW
            );
            return Err(ApiError::invalid("offset", &offset.to_string(), &msg));
        }
        return Ok(Page::Offset(offset));
    };
    if offset > 0 {
        let msg = "cursor paging always starts at offset 0; leave offset out";
        return Err(ApiError::invalid("cursor", cursor, msg));
    }
    if cursor == "*" {
        return Ok(Page::CursorStart);
    }
    match entities.cursor(cursor, q, categories) {
        Ok(after) => Ok(Page::After(after)),
        Err(CursorError::Malformed) => Err(ApiError::invalid(
            "cursor",
            cursor,
            "Not a cursor from this API; start with cursor=*",
        )),
        Err(CursorError::Expired) => Err(ApiError::invalid(
            "cursor",
            cursor,
            "Cursor is from an earlier index build; start again with cursor=*",
        )),
        Err(CursorError::OtherQuery) => Err(ApiError::invalid(
            "cursor",
            cursor,
            "Cursor belongs to a different q or category; start again with cursor=*",
        )),
    }
}

async fn entity(State(app): State<App>, Path(id): Path<String>) -> Result<Response, ApiError> {
    let found = blocking(move || Ok(app.entities.entity(&id)?)).await?;
    match found {
        Some(entity) => Ok(Json(entity).into_response()),
        None => Err(ApiError::NotFound("Entity")),
    }
}

/// All concepts in the middleware's harmonized-index format, so it can load this URL in place
/// of its bundled file (HARMONIZED_INDEX=http://.../bdc/harmonized-index.json).
async fn harmonized_index(State(app): State<App>) -> Result<Response, ApiError> {
    let concepts = app.concepts.ok_or(ApiError::NotFound("Harmonized index"))?;
    // Bytes clone is a reference-count bump, not a copy of the document.
    let body = concepts.index_json();
    Ok(([(header::CONTENT_TYPE, "application/json")], body).into_response())
}

/// Harmonized concepts matching `q`, by the middleware's rules, most studies first.
async fn search_concepts(
    State(app): State<App>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let params = Params::parse(raw);
    let q = params.required_text("q")?;
    let limit = params.count("limit", DEFAULT_MAX_MATCHES, Some(monarch::MAX_LIMIT))?;
    let concepts = app.concepts.ok_or(ApiError::NotFound("Harmonized index"))?;
    let body = blocking(move || {
        let matches = concepts.search(&q, limit);
        Ok(json!({ "total": matches.total, "limit": limit, "items": matches.items }))
    })
    .await?;
    Ok(Json(body).into_response())
}

async fn concept(State(app): State<App>, Path(curie): Path<String>) -> Result<Response, ApiError> {
    let found = app.concepts.as_ref().and_then(|c| c.get(&curie).cloned());
    found
        .map(|c| Json(c).into_response())
        .ok_or(ApiError::NotFound("Concept"))
}

/// Run `f` on tokio's blocking thread pool: tantivy queries and the concept scan are
/// synchronous, CPU-bound work that would otherwise stall the threads serving other requests.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
}

// ── query parameters ───────────────────────────────────────────────────────

/// Query parameters, parsed by hand: axum's typed extractor can't take repeated keys
/// (`category=a&category=b`), which Monarch's FastAPI routes accept.
struct Params(Vec<(String, String)>);

impl Params {
    fn parse(raw: Option<String>) -> Params {
        let raw = raw.unwrap_or_default();
        Params(
            form_urlencoded::parse(raw.as_bytes())
                .into_owned()
                .collect(),
        )
    }

    fn one(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn all(&self, key: &str) -> Vec<String> {
        self.0
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .collect()
    }

    fn required_text(&self, key: &'static str) -> Result<String, ApiError> {
        match self.one(key) {
            None => Err(ApiError::invalid(key, "", "Field required")),
            Some(v) if v.trim().is_empty() => Err(ApiError::invalid(
                key,
                v,
                "String should have at least 1 character",
            )),
            Some(v) => Ok(v.to_owned()),
        }
    }

    /// A non-negative integer parameter, with FastAPI's messages when it isn't one.
    fn count(
        &self,
        key: &'static str,
        default: usize,
        max: Option<usize>,
    ) -> Result<usize, ApiError> {
        let Some(raw) = self.one(key) else {
            return Ok(default);
        };
        let n: i64 = raw.trim().parse().map_err(|_| {
            ApiError::invalid(
                key,
                raw,
                "Input should be a valid integer, unable to parse string as an integer",
            )
        })?;
        if n < 0 {
            return Err(ApiError::invalid(
                key,
                raw,
                "Input should be greater than or equal to 0",
            ));
        }
        match max {
            Some(max) if n as usize > max => Err(ApiError::invalid(
                key,
                raw,
                &format!("Input should be less than or equal to {max}"),
            )),
            _ => Ok(n as usize),
        }
    }
}

// ── errors ─────────────────────────────────────────────────────────────────

/// Every way a request can fail, and the response each gets.
#[derive(Debug)]
enum ApiError {
    /// 422: a query parameter is missing or not acceptable.
    Invalid {
        key: &'static str,
        input: String,
        msg: String,
    },
    /// 404 for an id or resource that isn't here.
    NotFound(&'static str),
    /// 500. The cause is logged, not returned: it can name server paths.
    Internal(anyhow::Error),
}

impl ApiError {
    fn invalid(key: &'static str, input: &str, msg: &str) -> ApiError {
        ApiError::Invalid {
            key,
            input: input.to_owned(),
            msg: msg.to_owned(),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> ApiError {
        ApiError::Internal(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            // FastAPI's shape for an invalid query parameter.
            ApiError::Invalid { key, input, msg } => {
                let detail = json!([{ "loc": ["query", key], "msg": msg, "input": input }]);
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({ "detail": detail })),
                )
                    .into_response()
            }
            ApiError::NotFound(what) => (
                StatusCode::NOT_FOUND,
                Json(json!({ "detail": format!("{what} not found") })),
            )
                .into_response(),
            ApiError::Internal(e) => {
                tracing::error!(error = %e, "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "detail": "Internal Server Error" })),
                )
                    .into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concepts::tests::sample;
    use crate::entity_index::tests::{entity, index_of};
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use serde_json::Value;
    use tower::ServiceExt;

    fn app() -> App {
        let mut entities: Vec<_> = (0..30)
            .map(|i| {
                entity(
                    &format!("MONDO:9{i:06}"),
                    &format!("asthma type {i}"),
                    "biolink:Disease",
                    1.3,
                )
            })
            .collect();
        entities.push(entity(
            "MONDO:0005044",
            "hypertensive disorder",
            "biolink:Disease",
            1.3,
        ));
        entities.push(entity(
            "HP:0002099",
            "asthma",
            "biolink:PhenotypicFeature",
            1.1,
        ));
        App {
            entities: Arc::new(index_of(entities)),
            concepts: Some(Arc::new(sample())),
            release: None,
        }
    }

    /// GET `uri` against `app`. Each `app()` builds its own index, and cursors are tied to the
    /// index they came from, so a cursor walk must reuse one app.
    async fn get_from(app: &App, uri: &str) -> (StatusCode, Value) {
        let request = Request::get(uri).body(Body::empty()).unwrap();
        let response = router(app.clone()).oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    async fn get(uri: &str) -> (StatusCode, Value) {
        get_from(&app(), uri).await
    }

    fn message(body: &Value) -> &str {
        body["detail"][0]["msg"].as_str().unwrap()
    }

    #[tokio::test]
    async fn missing_and_empty_q_are_422() {
        let (status, body) = get("/v3/api/autocomplete").await;
        assert_eq!(
            (status, message(&body)),
            (StatusCode::UNPROCESSABLE_ENTITY, "Field required")
        );
        let (status, body) = get("/v3/api/autocomplete?q=").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["detail"][0]["loc"], json!(["query", "q"]));
    }

    #[tokio::test]
    async fn limit_follows_fastapi_rules() {
        let (status, body) = get("/v3/api/search?q=asthma&limit=501").await;
        assert_eq!(
            (status, message(&body)),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "Input should be less than or equal to 500"
            )
        );
        let (_, body) = get("/v3/api/search?q=asthma&limit=many").await;
        assert!(message(&body).starts_with("Input should be a valid integer"));
    }

    #[tokio::test]
    async fn offset_paging_stops_at_the_result_window() {
        let (status, body) = get("/v3/api/search?q=asthma&offset=9990&limit=20").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["detail"][0]["loc"], json!(["query", "offset"]));
    }

    #[tokio::test]
    async fn cursor_pages_through_every_hit() {
        let app = app();
        let (status, first) = get_from(&app, "/v3/api/search?q=asthma&limit=10&cursor=*").await;
        assert_eq!(status, StatusCode::OK);
        let mut seen = first["items"].as_array().unwrap().len();
        let mut next = first["next_cursor"].as_str().map(str::to_owned);
        while let Some(cursor) = next {
            let uri = format!("/v3/api/search?q=asthma&limit=10&cursor={cursor}");
            let (_, page) = get_from(&app, &uri).await;
            seen += page["items"].as_array().unwrap().len();
            next = page["next_cursor"].as_str().map(str::to_owned);
        }
        assert_eq!(seen, first["total"].as_u64().unwrap() as usize);
    }

    #[tokio::test]
    async fn cursor_and_offset_together_are_422() {
        let (status, body) = get("/v3/api/search?q=asthma&cursor=*&offset=10").await;
        assert_eq!(
            (status, body["detail"][0]["loc"].clone()),
            (StatusCode::UNPROCESSABLE_ENTITY, json!(["query", "cursor"]))
        );
        let (status, _) = get("/v3/api/search?q=asthma&cursor=bogus").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn category_can_repeat() {
        let (_, body) =
            get("/v3/api/search?q=asthma&category=biolink:PhenotypicFeature&category=biolink:Gene")
                .await;
        assert_eq!(body["total"], 1);
        assert_eq!(body["items"][0]["id"], "HP:0002099");
    }

    #[tokio::test]
    async fn unknown_ids_are_404() {
        let (status, body) = get("/v3/api/entity/MONDO:404").await;
        assert_eq!(
            (status, body["detail"].as_str().unwrap()),
            (StatusCode::NOT_FOUND, "Entity not found")
        );
        let (status, _) = get("/bdc/concepts/MONDO:404").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn harmonized_hits_carry_bdc_coverage() {
        let (_, body) = get("/v3/api/autocomplete?q=hypertensive").await;
        assert_eq!(body["items"][0]["id"], "MONDO:0005044");
        assert_eq!(body["items"][0]["bdc"]["studyCount"], 2);
        let (_, body) = get("/v3/api/autocomplete?q=asthma").await;
        assert!(body["items"][0].get("bdc").is_none());
    }

    #[tokio::test]
    async fn concept_search_reports_the_full_total() {
        let (_, body) = get("/bdc/concepts?q=hypertension&limit=1").await;
        assert_eq!(
            (
                body["total"].as_u64(),
                body["items"].as_array().unwrap().len()
            ),
            (Some(3), 1)
        );
    }
}
