#!/usr/bin/env python3
"""Integrity gates for the layered graph. Non-zero exit on any failure.

Checks:
  1. no duplicate node ids (duplicates fan out the denormalized_edges view join)
  2. no dangling overlay edges (subject/object missing from nodes)
  3. overlay edges are visible in denormalized_edges with clean scalar
     subject/object categories (catches the append list-stringification bug)
"""

import argparse
import sys

import duckdb


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("database")
    args = ap.parse_args()
    con = duckdb.connect(args.database, read_only=True)
    failures = []

    dup = con.execute(
        "SELECT count(*) FROM (SELECT id FROM nodes GROUP BY id HAVING count(*) > 1)"
    ).fetchone()[0]
    print(f"duplicate node ids: {dup}")
    if dup:
        failures.append(f"{dup} duplicate node ids")

    n_pb = con.execute(
        "SELECT count(DISTINCT provided_by) FROM edges WHERE provided_by IS NOT NULL"
    ).fetchone()[0]
    print(f"edge provided_by values: {n_pb}")

    dangling = con.execute(
        """
        SELECT count(*) FROM edges e
        LEFT JOIN nodes s ON e.subject = s.id
        LEFT JOIN nodes o ON e.object = o.id
        WHERE s.id IS NULL OR o.id IS NULL
        """
    ).fetchone()[0]
    print(f"dangling edges (whole graph): {dangling}")
    if dangling:
        failures.append(f"{dangling} dangling edges")

    bad_cat = con.execute(
        """
        SELECT count(*) FROM nodes
        WHERE category LIKE '[%' OR category LIKE '%]' OR category NOT LIKE 'biolink:%'
        """
    ).fetchone()[0]
    print(f"malformed node categories: {bad_cat}")
    if bad_cat:
        failures.append(f"{bad_cat} malformed node categories (list-stringification?)")

    edges_n = con.execute("SELECT count(*) FROM edges").fetchone()[0]
    denorm_n = con.execute("SELECT count(*) FROM denormalized_edges").fetchone()[0]
    print(f"edges: {edges_n:,}  denormalized_edges: {denorm_n:,}")
    if edges_n != denorm_n:
        failures.append(f"denormalized_edges rowcount {denorm_n:,} != edges {edges_n:,} (join fan-out or loss)")

    if failures:
        print("\nFAIL:")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("\nOK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
