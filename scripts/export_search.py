"""Export the search documents bdc-search indexes, from the KG duckdb.

Writes, into OUT_DIR:
  entities.jsonl       one document per KG node (sql/search_entity.sql)
  concepts.jsonl       one harmonized concept per CURIE (sql/search_concept.sql), when the
                       duckdb has the varlib schema (`just varlib`)
  concepts-meta.json   the HV commit and date the concepts were built from

Both SQL files read `search_nodes`, a view this script defines over `nodes` with the labels of
restricted sources blanked (`restricted_label_sources` in varlib.yaml, the same list
scripts/export_kg_labels.py uses). Defining it once here means a restricted source can't be
blanked in one export and leaked by the other.
"""

import argparse
import json
from pathlib import Path

import duckdb
import yaml

ROOT = Path(__file__).resolve().parent.parent


def restricted_sources(config: Path) -> list[str]:
    return yaml.safe_load(config.read_text()).get("restricted_label_sources") or []


def define_search_nodes(con: duckdb.DuckDBPyConnection, restricted: list[str]):
    """`nodes`, with `name` blanked where the label comes from a restricted source."""
    con.execute("CREATE TEMP TABLE restricted_label_source (provided_by VARCHAR)")
    con.executemany("INSERT INTO restricted_label_source VALUES (?)", [[s] for s in restricted])
    con.execute(
        """
        CREATE TEMP VIEW search_nodes AS
        SELECT * REPLACE (
            CASE WHEN provided_by IN (SELECT provided_by FROM restricted_label_source) THEN NULL ELSE name END AS name
        )
        FROM nodes
        """
    )


def export_json(con: duckdb.DuckDBPyConnection, sql_file: Path, out: Path):
    con.execute(f"COPY ({sql_file.read_text()}) TO '{out}' (FORMAT json)")
    rows = con.execute(f"SELECT count(*) FROM read_json('{out}', format='newline_delimited')").fetchone()[0]
    print(f"  {out.name}: {rows} rows")


def has_varlib(con: duckdb.DuckDBPyConnection) -> bool:
    found = con.execute(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'varlib' AND table_name = 'concept'"
    ).fetchone()[0]
    return found > 0


def concepts_meta(con: duckdb.DuckDBPyConnection) -> dict:
    """The HV commit the concepts were built from, and when they were loaded.

    One load records one commit per source repo; if a database somehow holds several, the most
    recently loaded wins.
    """
    row = con.execute(
        """
        SELECT commit, strftime(loaded_at, '%Y-%m-%d')
        FROM varlib.load_manifest
        WHERE input = 'source_repo' AND repo = 'NHLBI-BDC-DMC-HV'
        ORDER BY loaded_at DESC
        LIMIT 1
        """
    ).fetchone()
    return {"revision": row[0], "generated": row[1]} if row else {"revision": None, "generated": None}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("db", type=Path)
    parser.add_argument("out_dir", type=Path)
    parser.add_argument("--config", type=Path, default=ROOT / "varlib.yaml")
    args = parser.parse_args()

    args.out_dir.mkdir(parents=True, exist_ok=True)
    con = duckdb.connect(str(args.db), read_only=True)
    define_search_nodes(con, restricted_sources(args.config))

    export_json(con, ROOT / "sql" / "search_entity.sql", args.out_dir / "entities.jsonl")
    if has_varlib(con):
        export_json(con, ROOT / "sql" / "search_concept.sql", args.out_dir / "concepts.jsonl")
        (args.out_dir / "concepts-meta.json").write_text(json.dumps(concepts_meta(con)))
    else:
        for stale in ("concepts.jsonl", "concepts-meta.json"):
            (args.out_dir / stale).unlink(missing_ok=True)
        print(f"  {args.db} has no varlib schema; skipping harmonized concepts (just varlib)")


if __name__ == "__main__":
    main()
