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
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use clap::{Parser, Subcommand};
use serde::Deserialize;

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
        // The index is opened before the listener binds, so serving at all means ready.
        .route("/health/live", get(|| async { "ok" }))
        .route("/health/ready", get(|| async { "ok" }))
        .with_state(engine)
}

#[derive(Deserialize)]
struct AutocompleteParams {
    q: String,
}

async fn autocomplete(
    State(engine): State<Arc<Engine>>,
    Query(params): Query<AutocompleteParams>,
) -> Response {
    let result = tokio::task::spawn_blocking(move || engine.autocomplete(&params.q, &[])).await;
    match result {
        Ok(Ok(results)) => Json(results).into_response(),
        Ok(Err(e)) => internal_error(e),
        Err(e) => internal_error(e.into()),
    }
}

fn internal_error(e: anyhow::Error) -> Response {
    tracing::error!(error = %e, "request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "detail": e.to_string() })),
    )
        .into_response()
}
