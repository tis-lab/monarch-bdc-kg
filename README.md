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

## Variable library (exploratory)

`just varlib` loads the BDC variable library into a `varlib` schema in the same
duckdb, beside the KG tables rather than in `nodes`/`edges`. Inputs are pinned by
commit in [`varlib.yaml`](varlib.yaml):

| input | from | gives |
|---|---|---|
| variable library | `dm-bip extract-variable-library` ([linkml/dm-bip#376](https://github.com/linkml/dm-bip/pull/376)) | per-phv `SingleContinuousVariable` / `SingleCategoricalVariable` entries of [bdc-variable-library](https://github.com/linkml/bdc-variable-library) |
| mapping provenance | `dm-bip extract-mapping-provenance` | which phvs each HV trans-spec reads |
| concepts | [tis-lab/BDC-VarLib](https://github.com/tis-lab/BDC-VarLib)'s `scripts/build_bdc_varlib.py`, run over the same pinned specs | one concept per code a BDCHM class uses (e.g. MONDO:0005044), named with the KG's label |

Tables are in [`sql/varlib_schema.sql`](sql/varlib_schema.sql), and views joining
them to each other and to the KG are in [`sql/varlib_views.sql`](sql/varlib_views.sql):

- `varlib.variable_concept`: phv → concept, as BDC-VarLib lists them or through mapping provenance (same spec file, BDCHM class and dataset)
- `varlib.concept_doc` and `varlib.variable_doc`: one search document per concept and per variable, with KG anchor labels and closure ancestors
- `varlib.lexicon` and `varlib.lexicon_word`: every surface string and word with a weight, the input to a words-mode autocomplete

## Search API

[`search/`](search/) is `bdc-search`, a Rust service that answers Monarch v3 routes from a
[tantivy](https://github.com/quickwit-oss/tantivy) index of the KG's nodes. Because responses
match Monarch's, the BDC data portal middleware can point its `MONARCH_BASE_URL` at it.

When the duckdb has the `varlib` schema, `just search-index` also writes the harmonized
concepts (`sql/search_concept.sql`: one per concept code, labelled from the KG) beside the
index. The `/bdc` routes serve them in the middleware's own index format, so its
`HARMONIZED_INDEX` can point at `/bdc/harmonized-index.json` instead of its bundled file, and
`/search` and `/autocomplete` hits carry the same `bdc` coverage the middleware computes.

| route | parameters |
|---|---|
| `GET /v3/api/autocomplete` | `q` (top 10) |
| `GET /v3/api/search` | `q` (default `*:*`), `category` (repeatable), `limit` (0-500, default 20), `offset` |
| `GET /v3/api/entity/{id}` | node fields only; no associations or hierarchy yet |
| `GET /bdc/harmonized-index.json` | the harmonized concepts in bdc-dp-middleware's index format (schemaVersion 2) |
| `GET /bdc/concepts` | `q` (text, or a CURIE for an exact lookup), `limit` (default 25) |
| `GET /bdc/concepts/{curie}` | one harmonized concept |
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
