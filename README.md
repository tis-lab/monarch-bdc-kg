# monarch-bdc-kg

The BDC knowledge graph: a released [monarch-kg](https://github.com/monarch-initiative/monarch-ingest)
as the base layer, with BDC-specific content appended on top, plus a coverage
dashboard tracking how well the graph meets the
[BDC KG Requirements & Gap Analysis](coverage-goals.yaml) rubric.

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

## Coverage dashboard

`just coverage` writes `output/coverage.json` (full rubric matrix, judged
against the goals in `coverage-goals.yaml`) and copies it into
`dashboard/public/`. The dashboard is a small static Vite app deployed to
GitHub Pages on push to `main`.

```bash
just dashboard-dev
```
