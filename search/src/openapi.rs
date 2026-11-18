//! The API's OpenAPI description (`/openapi.json`) and its Swagger UI page (`/docs`).
//!
//! Each route's parameters and responses are declared by `#[utoipa::path]` on its handler in
//! http.rs, and the response types derive `ToSchema`. Handlers parse their query strings by
//! hand (ADR 2), so a route's declared parameters are documentation only: when a handler reads
//! a new parameter, add it to the handler's `params(...)` too.

use axum::Router;
use serde::Serialize;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityRequirement, SecurityScheme};
use utoipa::{OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;

use crate::auth::API_KEY_HEADER;
use crate::concepts::Concept;
use crate::http;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "BDC KG search",
        description = "Search over the BDC knowledge graph and the BDC harmonized concepts. \
            The /v3/api routes answer like Monarch's API, from the BDC KG."
    ),
    paths(
        http::autocomplete,
        http::search,
        http::entity,
        http::search_concepts,
        http::concept,
        http::words,
        http::harmonized_index
    ),
    tags(
        (name = "Monarch", description = "Monarch's v3 routes, answered from the BDC KG"),
        (name = "BDC", description = "Harmonized concepts from the BDC variable library")
    )
)]
struct ApiDoc;

/// `/openapi.json` and the `/docs` page. With an API key, the description says every route
/// needs it, and the page's Authorize button sends it on each call.
pub fn docs<S: Clone + Send + Sync + 'static>(api_key_required: bool) -> Router<S> {
    let mut spec = ApiDoc::openapi();
    // utoipa fills an empty license from Cargo.toml, which names none.
    spec.info.license = None;
    if api_key_required {
        spec.components
            .get_or_insert_with(Default::default)
            .add_security_scheme(
                "apiKey",
                SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new(API_KEY_HEADER))),
            );
        spec.security = Some(vec![SecurityRequirement::new(
            "apiKey",
            Vec::<String>::new(),
        )]);
    }
    SwaggerUi::new("/docs").url("/openapi.json", spec).into()
}

/// `/bdc/concepts`' response.
#[derive(Serialize, ToSchema)]
pub struct ConceptPage<'a> {
    pub total: usize,
    pub limit: usize,
    pub items: Vec<&'a Concept>,
}

/// 404: `{"detail": "Entity not found"}`.
#[derive(Serialize, ToSchema)]
pub struct NotFound {
    pub detail: String,
}

/// 422, in FastAPI's shape: one entry for the parameter that was refused.
#[derive(Serialize, ToSchema)]
pub struct Invalid {
    pub detail: Vec<InvalidParam>,
}

#[derive(Serialize, ToSchema)]
pub struct InvalidParam {
    /// `["query", <parameter>]`
    pub loc: Vec<String>,
    pub msg: String,
    pub input: String,
}

#[cfg(test)]
mod tests {
    use crate::auth::ApiKey;
    use crate::http::{router, tests::app};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use tower::ServiceExt;

    async fn get(api_key: Option<&str>, uri: &str) -> (StatusCode, Vec<u8>) {
        let key = api_key.map(|k| ApiKey::new(k).unwrap());
        let request = Request::get(uri).body(Body::empty()).unwrap();
        let response = router(app(), key).oneshot(request).await.unwrap();
        let status = response.status();
        (
            status,
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
    }

    #[tokio::test]
    async fn the_description_lists_every_api_route() {
        let (status, body) = get(None, "/openapi.json").await;
        assert_eq!(status, StatusCode::OK);
        let spec: Value = serde_json::from_slice(&body).unwrap();
        // serde_json sorts object keys.
        let paths: Vec<_> = spec["paths"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            paths,
            [
                "/bdc/concepts",
                "/bdc/concepts/{curie}",
                "/bdc/harmonized-index.json",
                "/bdc/words",
                "/v3/api/autocomplete",
                "/v3/api/entity/{id}",
                "/v3/api/search"
            ]
        );
        assert!(
            spec.get("security").is_none(),
            "no key, no security requirement"
        );
    }

    #[tokio::test]
    async fn with_a_key_the_docs_stay_open_and_say_the_key_is_needed() {
        let (status, body) = get(Some("s3cret"), "/openapi.json").await;
        assert_eq!(status, StatusCode::OK);
        let spec: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            spec["components"]["securitySchemes"]["apiKey"]["name"],
            "x-api-key"
        );
        assert_eq!(spec["security"][0]["apiKey"], serde_json::json!([]));
        let (status, _) = get(Some("s3cret"), "/docs/").await;
        assert_eq!(status, StatusCode::OK);
    }
}
