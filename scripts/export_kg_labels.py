"""Write a CURIE<TAB>label file from the KG's nodes, for BDC-VarLib's build (--labels).

Nodes from sources listed under `restricted_label_sources` in varlib.yaml are left out, so a
restricted label (e.g. the SNOMED CT labels of the SemMedDB snomedct tier) never becomes a
concept name.
"""

import argparse
from pathlib import Path

import duckdb
import yaml


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("db", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("--config", type=Path, default=Path("varlib.yaml"))
    args = parser.parse_args()

    restricted = yaml.safe_load(args.config.read_text()).get("restricted_label_sources") or []
    con = duckdb.connect(str(args.db), read_only=True)
    con.execute(
        "COPY (SELECT id, name FROM nodes "
        "WHERE trim(coalesce(name, '')) <> '' AND NOT list_contains(?, coalesce(provided_by, ''))) "
        f"TO '{args.out}' (DELIMITER '\t', HEADER false)",
        [restricted],
    )
    print(f"  labels: {con.execute(f'SELECT count(*) FROM read_csv(?, header=false)', [str(args.out)]).fetchone()[0]} -> {args.out}")


if __name__ == "__main__":
    main()
