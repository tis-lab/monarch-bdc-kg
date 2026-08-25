# monarch-bdc-kg build pipeline
#
# Layers BDC-specific content on top of a released monarch-kg and publishes
# the result (plus a coverage report) to a GCS bucket — no GitHub release.
#
#   just all        # download → conform → layer → verify → coverage
#   just upload     # push output/ to gs://monarch-bdc-kg/kg/{date}/ + latest/

bucket := "gs://monarch-bdc-kg"
# base graph: monarch-kg-dev tracks the in-progress upstream; switch to
# .../monarch-kg for building on tagged releases
monarch_kg := "gs://data-public-monarchinitiative/monarch-kg-dev"
base_release := "latest"
db := "output/bdc-kg.duckdb"

_default:
    @just --list

setup:
    uv sync

# ── inputs ────────────────────────────────────────────────────────────────

# The base layer: a released monarch-kg duckdb (already a koza graph database)
download-base:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -f data/base/monarch-kg.duckdb ]; then
        echo "data/base/monarch-kg.duckdb exists — 'just clean-base' to re-download"
    else
        gcloud storage cp {{monarch_kg}}/{{base_release}}/monarch-kg.duckdb data/base/monarch-kg.duckdb
    fi

# BDC overlay sources: each ingest publishes dated releases to
# {{bucket}}/<name>/<YYYY-MM-DD>/; mirror the release each source resolves to
# (newest, or the `release:` pin in sources.yaml)
download-sources:
    uv run scripts/download_sources.py --bucket {{bucket}} \
        --sources sources.yaml --output-dir data/sources

download: download-base download-sources

# ── build ─────────────────────────────────────────────────────────────────

# Conform overlay KGX to the base graph's conventions (scalar category,
# base-wins node dedup, provided_by stamping). Workarounds for koza append
# gaps — see the `koza layer` verb proposal.
conform:
    rm -rf data/conformed && mkdir -p data/conformed
    uv run scripts/conform_overlays.py --base data/base/monarch-kg.duckdb \
        --sources sources.yaml --input-dir data/sources --output-dir data/conformed

# base copy + koza append = the layered graph
layer:
    cp data/base/monarch-kg.duckdb {{db}}
    uv run koza append {{db}} --input-dir data/conformed -q

# integrity gates: duplicate node ids, dangling edges, denormalized visibility
verify:
    uv run scripts/verify_layer.py {{db}}

# ── report ────────────────────────────────────────────────────────────────

# Full rubric-matrix coverage, goals vs actuals.
# Writes output/coverage.json (+ .md) and a copy into dashboard/public/.
coverage:
    uv run scripts/coverage_report.py {{db}} --goals coverage-goals.yaml \
        --out output/coverage.json --md output/coverage.md
    cp output/coverage.json dashboard/public/coverage.json

# ── outputs ───────────────────────────────────────────────────────────────

# KGX TSV export of the layered graph
export:
    uv run koza export {{db}} -o output -f tsv

# Publish output/ to the bucket: kg/{today}/ and kg/latest/
upload:
    #!/usr/bin/env bash
    set -euo pipefail
    release=$(date +%Y-%m-%d)
    gcloud storage rsync -r output {{bucket}}/kg/$release
    gcloud storage rsync -r output {{bucket}}/kg/latest
    echo "released {{bucket}}/kg/$release (+ latest)"

all: download conform layer verify coverage

# ── dashboard ─────────────────────────────────────────────────────────────

dashboard-dev:
    cd dashboard && npm install && npm run dev

dashboard-build:
    cd dashboard && npm install && npm run build

# ── maintenance ───────────────────────────────────────────────────────────

clean:
    rm -rf data/conformed/* output/*

clean-base:
    rm -f data/base/monarch-kg.duckdb

test:
    uv run pytest tests -q
