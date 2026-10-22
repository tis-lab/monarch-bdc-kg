//! bdc-search: a Monarch v3 compatible search API over the BDC knowledge graph.
//!
//!   bdc-search index --input entities.jsonl --index-dir index/
//!   bdc-search serve --index-dir index/
//!   bdc-search query --index-dir index/ hypertension

mod analysis;
mod engine;
mod readonly;
mod schema;

use std::fs::File;
use std::io::{self, BufReader};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use clap::{Parser, Subcommand};

use engine::Engine;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build an entity index from JSONL rows of sql/search_entity.sql
    Index {
        /// JSONL file, or - for stdin
        #[arg(long)]
        input: PathBuf,
        /// Empty or missing directory to create the index in
        #[arg(long)]
        index_dir: PathBuf,
        /// Indexing memory budget in MB
        #[arg(long, default_value_t = 1024)]
        heap_mb: usize,
    },
    /// Serve the HTTP API
    Serve {
        #[arg(long, env = "INDEX_DIR")]
        index_dir: PathBuf,
        #[arg(long, env = "BIND", default_value = "0.0.0.0:8080")]
        bind: SocketAddr,
    },
    /// Print autocomplete results for a query, as the API would return them
    Query {
        #[arg(long)]
        index_dir: PathBuf,
        q: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    match Cli::parse().command {
        Command::Index {
            input,
            index_dir,
            heap_mb,
        } => {
            let started = std::time::Instant::now();
            let count = if input.as_os_str() == "-" {
                engine::build_index(io::stdin().lock(), &index_dir, heap_mb << 20)?
            } else {
                engine::build_index(
                    BufReader::new(File::open(&input)?),
                    &index_dir,
                    heap_mb << 20,
                )?
            };
            tracing::info!(count, seconds = started.elapsed().as_secs(), "index built");
        }
        Command::Query { index_dir, q } => {
            let engine = Engine::open(&index_dir)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&engine.autocomplete(&q, &[])?)?
            );
        }
        Command::Serve { index_dir, bind } => {
            let engine = Arc::new(Engine::open(&index_dir)?);
            tracing::info!(docs = engine.num_docs(), index = %index_dir.display(), "index opened");
            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!(%bind, "listening");
            axum::serve(listener, router(engine))
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
        }
    }
    Ok(())
}

fn router(engine: Arc<Engine>) -> Router {
    Router::new()
        .route("/v3/api/autocomplete", get(autocomplete))
        .route("/v3/api/search", get(search))
        .route("/v3/api/entity/{id}", get(entity))
        // The index is opened before the listener binds, so serving at all means ready.
        .route("/health/live", get(|| async { "ok" }))
        .route("/health/ready", get(|| async { "ok" }))
        .with_state(engine)
}

/// Monarch's page size bounds for `/search`.
const MAX_LIMIT: usize = 500;
const DEFAULT_LIMIT: usize = 20;

/// Query parameters, parsed by hand so repeated keys (`category=a&category=b`) work as they
/// do in Monarch's FastAPI routes.
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

    fn count(&self, key: &str, default: usize, max: Option<usize>) -> Result<usize, Box<Response>> {
        let Some(raw) = self.one(key) else {
            return Ok(default);
        };
        match raw.parse::<usize>() {
            Ok(n) if max.is_none_or(|max| n <= max) => Ok(n),
            _ => Err(Box::new(unprocessable(key, raw, max))),
        }
    }
}

async fn autocomplete(State(engine): State<Arc<Engine>>, RawQuery(raw): RawQuery) -> Response {
    let params = Params::parse(raw);
    let Some(q) = params.one("q").map(str::to_owned) else {
        return unprocessable("q", "", None);
    };
    blocking(move || {
        engine
            .autocomplete(&q, &[])
            .map(|r| Json(r).into_response())
    })
    .await
}

async fn search(State(engine): State<Arc<Engine>>, RawQuery(raw): RawQuery) -> Response {
    let params = Params::parse(raw);
    let q = params.one("q").unwrap_or("*:*").to_owned();
    let categories = params.all("category");
    let limit = match params.count("limit", DEFAULT_LIMIT, Some(MAX_LIMIT)) {
        Ok(n) => n,
        Err(response) => return *response,
    };
    let offset = match params.count("offset", 0, None) {
        Ok(n) => n,
        Err(response) => return *response,
    };
    blocking(move || {
        engine
            .search(&q, &categories, limit, offset)
            .map(|r| Json(r).into_response())
    })
    .await
}

async fn entity(State(engine): State<Arc<Engine>>, Path(id): Path<String>) -> Response {
    blocking(move || {
        Ok(match engine.entity(&id)? {
            Some(entity) => Json(entity).into_response(),
            None => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "detail": "Entity not found" })),
            )
                .into_response(),
        })
    })
    .await
}

/// Run a search off the async runtime: tantivy queries are synchronous and CPU-bound.
async fn blocking(f: impl FnOnce() -> Result<Response> + Send + 'static) -> Response {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(response)) => response,
        Ok(Err(e)) => internal_error(e),
        Err(e) => internal_error(e.into()),
    }
}

/// FastAPI's 422 shape for an invalid query parameter, which is what Monarch returns.
fn unprocessable(key: &str, input: &str, max: Option<usize>) -> Response {
    let msg = match (input.is_empty(), max) {
        (true, _) => "Field required".to_string(),
        (false, Some(max)) => format!("Input should be an integer from 0 to {max}"),
        (false, None) => "Input should be a non-negative integer".to_string(),
    };
    let detail = serde_json::json!([{ "loc": ["query", key], "msg": msg, "input": input }]);
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(serde_json::json!({ "detail": detail })),
    )
        .into_response()
}

fn internal_error(e: anyhow::Error) -> Response {
    tracing::error!(error = %e, "request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "detail": e.to_string() })),
    )
        .into_response()
}
