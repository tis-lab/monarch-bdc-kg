#!/usr/bin/env python3
"""BDC coverage report: full rubric matrix, goals vs actuals.

Computes the complete subject×object matrix over the BDC rubric node types
(coverage-goals.yaml), judged against the declared goals. Unlike the original
curated-pairs report, no populated pair is hidden; edges whose endpoint
categories fall outside the rubric are counted into an explicit `other` bucket
rather than silently ignored.

Output: a JSON document for the dashboard (and a markdown summary).
"""

import argparse
import json
import sys
from collections import defaultdict
from datetime import date

import duckdb
import yaml


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("database")
    ap.add_argument("--goals", required=True, help="coverage-goals.yaml")
    ap.add_argument("--out", required=True, help="coverage.json path")
    ap.add_argument("--md", help="optional markdown summary path")
    args = ap.parse_args()

    cfg = yaml.safe_load(open(args.goals))
    rubric = cfg["rubric"]
    goals = {(g["subject"], g["object"]): g for g in cfg["goals"]}
    class_to_type = {c: t for t, cs in rubric.items() for c in cs}
    type_order = list(rubric.keys())

    con = duckdb.connect(args.database, read_only=True)

    rows = con.execute(
        """
        SELECT
            replace(subject_category, 'biolink:', '') AS subj_class,
            replace(predicate, 'biolink:', '')        AS predicate,
            replace(object_category, 'biolink:', '')  AS obj_class,
            replace(primary_knowledge_source, 'infores:', '') AS source,
            count(*) AS n
        FROM denormalized_edges
        GROUP BY 1, 2, 3, 4
        """
    ).fetchall()

    # (subj_type, obj_type) -> {predicate -> {n, sources}}; unmapped -> "(other)"
    matrix = defaultdict(lambda: defaultdict(lambda: {"count": 0, "sources": set()}))
    other_edges = 0
    for subj_class, predicate, obj_class, source, n in rows:
        st = class_to_type.get(subj_class, "(other)")
        ot = class_to_type.get(obj_class, "(other)")
        if st == "(other)" and ot == "(other)":
            other_edges += n
            continue
        cell = matrix[(st, ot)][predicate]
        cell["count"] += n
        if source:
            cell["sources"].add(source)

    nodes_n = con.execute("SELECT count(*) FROM nodes").fetchone()[0]
    edges_n = con.execute("SELECT count(*) FROM edges").fetchone()[0]
    provided_by = [r[0] for r in con.execute(
        "SELECT DISTINCT provided_by FROM edges WHERE provided_by IS NOT NULL ORDER BY 1"
    ).fetchall()]

    pairs = []
    gaps = met = 0
    for st in type_order + ["(other)"]:
        for ot in type_order + ["(other)"]:
            if st == "(other)" and ot == "(other)":
                continue
            preds = matrix.get((st, ot), {})
            total = sum(p["count"] for p in preds.values())
            goal = goals.get((st, ot))
            status = "info"
            if goal:
                if goal["goal"] == "not-target":
                    status = "not-target"
                elif goal["goal"] == "target":
                    floor = goal.get("floor", 1)
                    status = "met" if total >= floor else "gap"
                    if status == "gap":
                        gaps += 1
                    else:
                        met += 1
            if total == 0 and not goal and (st == "(other)" or ot == "(other)"):
                continue  # don't emit empty other-rows
            pairs.append({
                "subject_type": st,
                "object_type": ot,
                "total": total,
                "status": status,
                "goal_note": goal.get("note") if goal else None,
                "predicates": sorted(
                    ({"predicate": p, "count": c["count"], "sources": sorted(c["sources"])}
                     for p, c in preds.items()),
                    key=lambda x: -x["count"],
                ),
            })

    doc = {
        "generated": date.today().isoformat(),
        "database": args.database,
        "nodes": nodes_n,
        "edges": edges_n,
        "provided_by": provided_by,
        "type_order": type_order,
        "targets_met": met,
        "targets_gap": gaps,
        "edges_outside_rubric_both_endpoints": other_edges,
        "pairs": pairs,
    }
    with open(args.out, "w") as fh:
        json.dump(doc, fh, indent=1)
    zero_targets = [p for p in pairs if p["status"] == "gap"]
    print(f"{len(pairs)} pairs -> {args.out} | targets met {met}, gaps {gaps}: "
          + ", ".join(f"{p['subject_type']}→{p['object_type']}" for p in zero_targets))

    if args.md:
        with open(args.md, "w") as fh:
            fh.write(f"# BDC KG coverage — {doc['generated']}\n\n")
            fh.write(f"{nodes_n:,} nodes / {edges_n:,} edges. ")
            fh.write(f"Targets met: {met}, gaps: {gaps}.\n\n")
            fh.write("| Subject | Object | Edges | Status | Top predicates (sources) |\n|---|---|---|---|---|\n")
            for p in pairs:
                if p["total"] == 0 and p["status"] == "info":
                    continue
                top = "; ".join(
                    f"{x['predicate']} {x['count']:,} ({', '.join(x['sources'])})"
                    for x in p["predicates"][:3]
                )
                fh.write(f"| {p['subject_type']} | {p['object_type']} | {p['total']:,} | {p['status']} | {top} |\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
