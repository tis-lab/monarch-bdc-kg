//! bdc-search: a Monarch v3 compatible search API over the BDC knowledge graph.
//!
//!   bdc-search index --input entities.jsonl --index-dir index/
//!   bdc-search serve --index-dir index/
//!   bdc-search query --index-dir index/ hypertension
//!
//! Harmonized concepts (`concepts.jsonl`, written by scripts/export_search.py) are served from
//! the same index directory when present.
//!
//! Where things are:
//!   monarch.rs       Monarch's ranking and paging rules
//!   schema.rs        the index's fields
//!   analysis.rs      how text is split into searchable terms
//!   entity_index.rs  building the KG entity index and searching it
//!   concepts.rs      harmonized concepts, in the middleware's format
//!   http.rs          routes, parameters and error responses
//!   readonly.rs      serving an index from a read-only filesystem

mod analysis;
mod auth;
mod concepts;
mod entity_index;
mod http;
mod monarch;
mod readonly;
mod schema;

use std::fs::File;
use std::io::{self, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};

use auth::ApiKey;
use concepts::HarmonizedConcepts;
use entity_index::EntityIndex;
use http::App;

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
        /// Require this key on every route but the health probes (X-API-Key or Bearer)
        #[arg(long, env = "BDC_SEARCH_API_KEY", hide_env_values = true)]
        api_key: Option<String>,
    },
    /// Print what an index directory holds, as /version reports it (for release manifests)
    Info {
        #[arg(long)]
        index_dir: PathBuf,
    },
    /// Print /v3/api/autocomplete's response for a query
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
        // logs to stderr, so stdout is only the command's output (`info` and `query` print JSON)
        .with_writer(std::io::stderr)
        .init();

    match Cli::parse().command {
        Command::Index {
            input,
            index_dir,
            heap_mb,
        } => {
            let started = std::time::Instant::now();
            let heap_bytes = heap_mb << 20;
            let count = if input.as_os_str() == "-" {
                entity_index::build_index(io::stdin().lock(), &index_dir, heap_bytes)?
            } else {
                entity_index::build_index(
                    BufReader::new(File::open(&input)?),
                    &index_dir,
                    heap_bytes,
                )?
            };
            tracing::info!(count, seconds = started.elapsed().as_secs(), "index built");
        }
        Command::Info { index_dir } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&open(&index_dir)?.info())?
            );
        }
        Command::Query { index_dir, q } => {
            let app = open(&index_dir)?;
            let results = app.with_coverage(app.entities.autocomplete(&q)?);
            println!("{}", serde_json::to_string_pretty(&results)?);
        }
        Command::Serve {
            index_dir,
            bind,
            api_key,
        } => {
            let app = open(&index_dir)?;
            let api_key = api_key.as_deref().map(ApiKey::new).transpose()?;
            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!(%bind, api_key_required = api_key.is_some(), "listening");
            axum::serve(listener, http::router(app, api_key))
                .with_graceful_shutdown(http::shutdown_signal())
                .await?;
        }
    }
    Ok(())
}

/// Written into the index directory by `just search-release` (scripts/package_search.py).
const RELEASE_MANIFEST: &str = "manifest.json";

/// Open the entity index and, when present, the harmonized concepts and release manifest
/// beside it.
fn open(index_dir: &Path) -> Result<App> {
    let entities = Arc::new(EntityIndex::open(index_dir)?);
    tracing::info!(docs = entities.num_docs(), index = %index_dir.display(), "entity index opened");
    let concepts = HarmonizedConcepts::open(index_dir)?.map(Arc::new);
    match &concepts {
        Some(c) => tracing::info!(concepts = c.len(), "harmonized concepts loaded"),
        None => tracing::warn!(
            "no {} in the index directory; /bdc routes return 404",
            concepts::CONCEPTS_FILE
        ),
    }
    let release = match std::fs::read_to_string(index_dir.join(RELEASE_MANIFEST)) {
        Ok(text) => Some(Arc::new(serde_json::from_str(&text)?)),
        Err(_) => None,
    };
    Ok(App {
        entities,
        concepts,
        release,
    })
}
