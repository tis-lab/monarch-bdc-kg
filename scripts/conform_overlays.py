#!/usr/bin/env python3
"""Conform overlay KGX files to the base graph's schema conventions.

Workarounds for gaps in `koza append` (see the `koza layer` verb proposal):

1. **List→scalar coercion**: the base graph stores some biolink-multivalued
   slots as scalars (monarch-kg forces `category`, `in_taxon`, `type` single-
   valued). Appending a jsonl array into a VARCHAR column stringifies it into
   a Python repr (`"['biolink:Procedure']"`), silently corrupting the value.
   We coerce each overlay column to the base column's type: first element for
   VARCHAR, list for VARCHAR[].
2. **Node collision policy (base wins)**: overlay files carry endpoint stubs
   (e.g. MONDO/HP nodes) that already exist in the base with richer records.
   Duplicate ids fan out the denormalized_edges view join, so overlay node
   rows whose id exists in the base — or in an earlier overlay — are dropped.
3. **Provenance stamping**: `koza append` does not stamp `provided_by`; we
   stamp it from the file stem, matching monarch-kg's convention.

Usage:
    conform_overlays.py --base data/base/monarch-kg.duckdb \
        --sources sources.yaml --input-dir data/sources --output-dir data/conformed
"""

import argparse
import csv
import json
import sys
from pathlib import Path

import duckdb
import yaml


def base_schema(con, table: str) -> dict[str, str]:
    return dict(
        con.execute(
            "SELECT column_name, data_type FROM information_schema.columns WHERE table_name = ?",
            [table],
        ).fetchall()
    )


def coerce(value, base_type: str | None):
    """Coerce a value to the base column's scalar/list convention."""
    if base_type is None:
        return value  # new column: schema evolution handles it
    is_list_type = base_type.endswith("[]")
    if isinstance(value, list) and not is_list_type:
        return value[0] if value else None
    if not isinstance(value, list) and is_list_type and value is not None:
        return [value]
    return value


def read_kgx(path: Path):
    if path.suffix == ".jsonl":
        with open(path) as fh:
            for line in fh:
                yield json.loads(line)
    elif path.suffix == ".tsv":
        with open(path) as fh:
            for row in csv.DictReader(fh, delimiter="\t"):
                yield {k: v for k, v in row.items() if v != ""}
    else:
        raise ValueError(f"unsupported KGX file type: {path}")


def write_jsonl(records, path: Path):
    with open(path, "w") as fh:
        for rec in records:
            fh.write(json.dumps(rec) + "\n")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--base", required=True, help="base graph duckdb")
    ap.add_argument("--sources", required=True, help="sources.yaml")
    ap.add_argument("--input-dir", required=True, help="local mirror of bucket sources/")
    ap.add_argument("--output-dir", required=True, help="conformed KGX output dir")
    args = ap.parse_args()

    in_dir, out_dir = Path(args.input_dir), Path(args.output_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    con = duckdb.connect(args.base, read_only=True)
    node_types = base_schema(con, "nodes")
    edge_types = base_schema(con, "edges")
    seen_ids = {r[0] for r in con.execute("SELECT id FROM nodes").fetchall()}
    print(f"base: {len(seen_ids):,} node ids")

    sources = yaml.safe_load(open(args.sources))["sources"]
    total_nodes = total_edges = 0

    for source in sources:
        name = source["name"]
        for file_rel in source.get("node_files", []):
            src = in_dir / name / file_rel
            stem = Path(file_rel).stem
            kept, dropped = [], 0
            for node in read_kgx(src):
                if node["id"] in seen_ids:
                    dropped += 1
                    continue  # base (or an earlier overlay) wins
                seen_ids.add(node["id"])
                node = {k: coerce(v, node_types.get(k)) for k, v in node.items()}
                node["provided_by"] = stem
                node.setdefault("namespace", node["id"].split(":")[0])
                kept.append(node)
            write_jsonl(kept, out_dir / f"{stem}.jsonl")  # stem already ends _nodes
            total_nodes += len(kept)
            print(f"{name}: {src.name} -> {len(kept):,} nodes ({dropped:,} already present, base wins)")

        for file_rel in source.get("edge_files", []):
            src = in_dir / name / file_rel
            stem = Path(file_rel).stem
            edges = []
            for edge in read_kgx(src):
                edge = {k: coerce(v, edge_types.get(k)) for k, v in edge.items()}
                edge["provided_by"] = stem
                edges.append(edge)
            write_jsonl(edges, out_dir / f"{stem}.jsonl")  # stem already ends _edges
            total_edges += len(edges)
            print(f"{name}: {src.name} -> {len(edges):,} edges")

    print(f"conformed: {total_nodes:,} nodes, {total_edges:,} edges -> {out_dir}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
