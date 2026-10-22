# monarch-bdc-kg

The BDC knowledge graph: a released [monarch-kg](https://github.com/monarch-initiative/monarch-ingest)
as the base layer, with BDC-specific content appended on top, plus a coverage
dashboard tracking how well the graph meets the
[BDC KG Requirements & Gap Analysis](coverage-goals.yaml) rubric.

**[View the coverage dashboard →](https://tis-lab.github.io/monarch-bdc-kg/)**

Inputs are read from Google Cloud Storage and the built KG is published back
to `gs://monarch-bdc-kg/` — there is no GitHub release.

## Architecture

```
gs://data-public-monarchinitiative/monarch-kg-dev/latest/monarch-kg.duckdb   (base)
gs://monarch-bdc-kg/<ingest>/<YYYY-MM-DD>/*.{tsv,jsonl}                       (overlays, KGX)
        │
        ▼
conform (scalar coercion, base-wins node dedup, provided_by stamping)
        ▼
koza append  →  output/bdc-kg.duckdb
        ▼
verify (integrity gates) → coverage (goals vs actuals) → export (KGX TSV)
        ▼
gs://monarch-bdc-kg/kg/{YYYY-MM-DD}/ + kg/latest/
```

The conform step exists because `koza append` doesn't yet handle list→scalar
coercion, node collision policy, or provenance stamping — tracked upstream as
a proposed `koza layer` verb. As koza grows those capabilities, conform shrinks.

## Layers

Declared in [`sources.yaml`](sources.yaml). Current:

| layer | edges | what |
|---|---|---|
| semmeddb | ~28k | Procedure→Disease/Phenotype (diagnoses/treats), LLM-verified SemMedDB, built by [semmeddb-procedure-ingest](https://github.com/monarch-initiative/semmeddb-procedure-ingest); ncit tier is open, snomedct tier is RESTRICTED (labels) — keep the built KG private unless sources are rebuilt with OUTPUT_TIER=open |
| snomed-hasfocus | ~900 | high-precision Procedure→Disease/Phenotype from SNOMED CT Has-focus (SNOMEDCT: codes only, labels blank per license) |

## Usage

```bash
just setup      # uv sync
just all        # download → conform → layer → verify → coverage
just export     # KGX TSV
just upload     # publish to gs://monarch-bdc-kg/kg/{date}/ + latest/
```

Requires `gcloud` auth with access to the Monarch GCS buckets.

## Search API

[`search/`](search/) is `bdc-search`, a Rust service that answers Monarch v3 routes from a
[tantivy](https://github.com/quickwit-oss/tantivy) index of the KG's nodes. Because responses
match Monarch's, the BDC data portal middleware can point its `MONARCH_BASE_URL` at it.

| route | parameters |
|---|---|
| `GET /v3/api/autocomplete` | `q` (top 10) |
| `GET /v3/api/search` | `q` (default `*:*`), `category` (repeatable), `limit` (0-500, default 20), `offset` |
| `GET /v3/api/entity/{id}` | node fields only; no associations or hierarchy yet |
| `GET /health/live`, `/health/ready` | probes |

```bash
just search-index   # sql/search_entity.sql → data/search/entities.jsonl → data/search/index
just search-serve   # http://localhost:8080/v3/api/autocomplete?q=pulmonary%20hyper
just search-image   # linux/amd64 image; run with the index mounted at /index
```

The query mirrors monarch-app's autocomplete: Monarch's query fields and boosts, every
word required, and Monarch's category, deprecation and exact-synonym multipliers. Analyzers
follow monarch-ingest's Solr field types. Labels from restricted sources (the SemMedDB
snomedct tier) are blanked in `sql/search_entity.sql`, so the index never carries them.

## Coverage dashboard

Published at **<https://tis-lab.github.io/monarch-bdc-kg/>**.

`just coverage` writes `output/coverage.json` (full rubric matrix, judged
against the goals in `coverage-goals.yaml`) and copies it into
`dashboard/public/`. The dashboard is a small static Vite app deployed to
GitHub Pages on push to `main` by
[`.github/workflows/pages.yaml`](.github/workflows/pages.yaml).

```bash
just dashboard-dev
```
