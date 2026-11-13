# 1. Serve KG search from a tantivy index, not Solr or Python

Status: accepted, 2026-10

## Context

The portal middleware calls Monarch's search API. To serve the BDC KG instead, we need the same
routes and ranking over about 1.7M nodes, deployable on OpenShift with little to operate.

## Decision

Build a small Rust service (`search/`) on tantivy, a Lucene-like library, that indexes the
nodes exported from the KG duckdb and answers Monarch's v3 routes.

## Consequences

- One static binary and a read-only index directory (~430 MB): no Solr cluster or JVM to run,
  ~60 MB of memory, ~1.5 ms per query.
- Ranking has to be reproduced by hand (`monarch.rs`, `analysis.rs`), and tantivy differs from
  Lucene in places (field-length norms on edge n-grams); parity is measured against Monarch.
- The team reads Python and SQL more easily than Rust, so rules that depend only on the data
  live in SQL (`sql/search_*.sql`) and the Rust is kept to query and HTTP mechanics.
