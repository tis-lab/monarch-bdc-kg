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

# ── variable library (exploratory) ────────────────────────────────────────
#
# Loads the BDC variable library into the `varlib` schema of {{db}}, from teammate
# tools pinned in varlib.yaml. See sql/varlib_schema.sql for the tables.

varlib_dir := justfile_directory() / "data/varlib"

# Check out each varlib.yaml repo at its pinned commit under data/varlib/src/
varlib-sources:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p {{varlib_dir}}/src
    uv run python -c 'import yaml; [print(n, r["url"], r["ref"], r["commit"]) for n, r in yaml.safe_load(open("varlib.yaml"))["repos"].items()]' |
    while read -r name url ref commit; do
        repo={{varlib_dir}}/src/$name
        [ -d "$repo/.git" ] || git clone -q "$url" "$repo"
        # fetch the pinned commit itself, so a force-pushed PR ref can't strand the pin
        git -C "$repo" fetch -q origin "$commit" || git -C "$repo" fetch -q origin "$ref"
        git -C "$repo" checkout -q "$commit"
        echo "$name @ $commit"
    done
    cd {{varlib_dir}}/src/dm-bip && uv sync -q

# Run dm-bip over the HV specs: mapping provenance for every cohort, and the
# variable library for each cohort in varlib.yaml (fetches dbGaP dictionaries once,
# then reads data/varlib/dbgap-cache); then build BDC-VarLib's concepts from the same specs
varlib-extract:
    #!/usr/bin/env bash
    set -euo pipefail
    out={{varlib_dir}}/out
    specs={{varlib_dir}}/src/NHLBI-BDC-DMC-HV/priority_variables_transform
    mkdir -p "$out"
    cd {{varlib_dir}}/src/dm-bip
    uv run dm-bip extract-mapping-provenance "$specs" -o "$out/mapping-provenance.yaml"
    uv run --project {{justfile_directory()}} python -c 'import yaml; [print(c["key"], c["spec_dir"]) for c in yaml.safe_load(open("{{justfile_directory()}}/varlib.yaml"))["cohorts"]]' |
    while read -r key spec_dir; do
        uv run dm-bip extract-variable-library "$specs/$spec_dir" --cohort "$key" \
            --dbgap-cache {{varlib_dir}}/dbgap-cache -o "$out/$key-variable-library.yaml"
    done
    # Concepts: BDC-VarLib's own build, run against the pinned HV checkout so concepts and
    # variables come from one commit, and named with the KG's labels rather than spec file names
    cd {{justfile_directory()}}
    uv run scripts/export_kg_labels.py {{db}} "$out/kg-labels.tsv"
    rm -rf "$out/bdc-varlib"
    cd {{varlib_dir}}/src/BDC-VarLib && uv sync -q && uv run python scripts/build_bdc_varlib.py \
        --repo-dir {{varlib_dir}}/src/NHLBI-BDC-DMC-HV --labels "$out/kg-labels.tsv" --outdir "$out/bdc-varlib"

# Load whatever varlib-extract produced (variables, provenance, concepts) into {{db}}
varlib-load:
    #!/usr/bin/env bash
    set -euo pipefail
    shopt -s nullglob  # inputs varlib-extract hasn't produced are skipped, not passed as literal globs
    provenance=({{varlib_dir}}/out/mapping-provenance.yaml)
    libraries=({{varlib_dir}}/out/*-variable-library.yaml)
    uv run scripts/load_varlib.py {{db}} \
        --mapping-provenance ${provenance[@]+"${provenance[@]}"} \
        --variable-library ${libraries[@]+"${libraries[@]}"} \
        --concepts-dir {{varlib_dir}}/out/bdc-varlib \
        --variable-schema {{varlib_dir}}/src/bdc-variable-library/src/bdc_variable_library/schema/bdc_variable_library.yaml \
        --source-repos {{varlib_dir}}/src/* \
        --sssom $(uv run python -c 'import yaml; print(" ".join("{{varlib_dir}}/src/" + p for p in yaml.safe_load(open("varlib.yaml"))["sssom"]))')

varlib: varlib-sources varlib-extract varlib-load

# ── search API ────────────────────────────────────────────────────────────
#
# bdc-search (search/): a Monarch v3 compatible search API over {{db}}.

search_dir := "data/search"

# Export search documents from {{db}} (scripts/export_search.py: one per KG node, plus the
# harmonized concepts when {{db}} has the varlib schema) and build the index beside them
search-index:
    #!/usr/bin/env bash
    set -euo pipefail
    uv run scripts/export_search.py {{db}} {{search_dir}}
    rm -rf {{search_dir}}/index
    cargo run --release --manifest-path search/Cargo.toml -- index \
        --input {{search_dir}}/entities.jsonl --index-dir {{search_dir}}/index
    for f in concepts.jsonl concepts-meta.json; do
        if [ -f {{search_dir}}/$f ]; then cp {{search_dir}}/$f {{search_dir}}/index/; fi
    done

# Serve the index on localhost:8080 (/v3/api/autocomplete, /search, /entity/{id}, /bdc/...)
search-serve:
    cargo run --release --manifest-path search/Cargo.toml -- serve \
        --index-dir {{search_dir}}/index --bind 127.0.0.1:8080

# Container image for Cloud Apps; run it with the index mounted at /index
search-image:
    docker build --platform linux/amd64 -t bdc-search:dev search

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
