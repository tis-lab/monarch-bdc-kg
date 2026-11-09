"""Load the BDC variable library into the `varlib` schema of the KG duckdb.

Reads three teammate outputs (see varlib.yaml), each optional so a partial load still works:

  --variable-library    YAML from `dm-bip extract-variable-library` (linkml/dm-bip#376)
  --mapping-provenance  YAML from `dm-bip extract-mapping-provenance`
  --concepts-dir        schema modules from tis-lab/BDC-VarLib's scripts/build_bdc_varlib.py, run over
                        the same HV specs (one slot per concept code)
  --source-repos        the checkouts these came from, recorded with their commits
  --sssom               SSSOM TSVs appended to the KG's main.mappings table (e.g. the OMOP
                        bridges from monarch-mapping-commons#86), stamped with the file stem
                        as mapping_source like the base sets, and replaced on reload

Recreates every varlib table (sql/varlib_schema.sql), then the views (sql/varlib_views.sql).
"""

import argparse
import csv
import hashlib
import re
import subprocess
from pathlib import Path

import duckdb
import yaml

ROOT = Path(__file__).resolve().parent.parent
SPEC_PATH = re.compile(r"([A-Za-z0-9_]+-ingest/[^/]+\.yaml)$")
NUMERIC = re.compile(r"^-?\d+(\.\d+)?([eE][-+]?\d+)?$")
NOT_LISTED = "derived via expr"


def split_list(value) -> list[str]:
    """BDC-VarLib folds lists into comma-delimited annotation strings."""
    if value is None:
        return []
    return [part.strip() for part in str(value).split(",") if part.strip()]


def as_float(value):
    text = str(value).strip() if value is not None else ""
    return float(text) if NUMERIC.match(text) else None


def derived_class(derivation_id: str) -> str | None:
    """`Condition` from dmcprov:ARIC-ingest/afib/Condition/pht004037."""
    parts = derivation_id.removeprefix("dmcprov:").split("/")
    return parts[2] if len(parts) >= 4 else None


def spec_path(spec_id: str) -> str | None:
    """`ARIC-ingest/bmi.yaml` from a commit permalink or a dmcprov: id."""
    match = SPEC_PATH.search(spec_id.removeprefix("dmcprov:"))
    return match.group(1) if match else None


def git_commit(path: Path) -> str | None:
    try:
        out = subprocess.run(["git", "-C", str(path), "rev-parse", "HEAD"], capture_output=True, text=True, check=True)
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None
    return out.stdout.strip()


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


class Loader:
    def __init__(self, con: duckdb.DuckDBPyConnection):
        self.con = con

    def insert(self, table: str, rows: list[tuple]) -> int:
        if rows:
            placeholders = ", ".join("?" * len(rows[0]))
            self.con.executemany(f"INSERT INTO varlib.{table} VALUES ({placeholders})", rows)
        return len(rows)

    def manifest(self, input_name: str, path: Path, count: int, repo=None, commit=None, digest=None):
        self.con.execute(
            "INSERT INTO varlib.load_manifest (input, path, repo, commit, sha256, row_count) VALUES (?, ?, ?, ?, ?, ?)",
            [input_name, str(path), repo, commit, digest, count],
        )

    def variable_library(self, path: Path):
        doc = yaml.safe_load(path.read_text()) or {}
        variables, values, datasets = [], [], {}
        for key, cls in (
            ("single_continuous_variables", "SingleContinuousVariable"),
            ("single_categorical_variables", "SingleCategoricalVariable"),
        ):
            for entry in doc.get(key) or []:
                study = entry.get("associated_study")
                study_id = study.get("id") if isinstance(study, dict) else study
                variables.append(
                    (
                        entry["id"],
                        entry.get("source_id") or entry["id"].removeprefix("dbgap:"),
                        entry.get("file_id"),
                        study_id,
                        cls,
                        entry.get("variable_name"),
                        entry.get("source_variable_description"),
                        entry.get("variable_description"),
                        entry.get("variable_label"),
                        entry.get("concept_type"),
                        entry.get("data_type"),
                        entry.get("unit"),
                        as_float(entry.get("minimum_value")),
                        as_float(entry.get("maximum_value")),
                        entry.get("resolution"),
                        entry.get("comment"),
                        entry.get("file_name"),
                    )
                )
                if entry.get("file_id"):
                    datasets.setdefault(entry["file_id"], (entry["file_id"], study_id, entry.get("file_name")))
                for kind, slot in (("coded", "coded_values"), ("missing", "missing_value"), ("alert", "alert_values")):
                    for i, value in enumerate(entry.get(slot) or []):
                        values.append(
                            (
                                entry["id"],
                                kind,
                                i,
                                str(value.get("indicator_char")) if value.get("indicator_char") is not None else None,
                                value.get("indicator_meaning"),
                                value.get("indicator_type"),
                            )
                        )
        count = self.insert("variable", variables)
        self.insert("variable_value", values)
        if datasets:
            self.con.executemany(
                "INSERT INTO varlib.dataset VALUES (?, ?, ?) ON CONFLICT (pht) DO UPDATE SET "
                "study_id = coalesce(varlib.dataset.study_id, excluded.study_id), "
                "file_name = coalesce(varlib.dataset.file_name, excluded.file_name)",
                list(datasets.values()),
            )
        self.manifest("variable_library", path, count, digest=sha256(path))
        print(f"  {path.name}: {count} variables, {len(values)} values")

    def mapping_provenance(self, path: Path):
        records = yaml.safe_load(path.read_text()) or []
        studies, datasets, derivations, sources = {}, {}, [], []

        for record in records:
            if record.get("entity_type") != "study":
                continue
            study_id = record["id"]
            parts = record.get("has_part") or []
            specs = [p for p in parts if p.get("entity_type") == "transformation_spec"]
            cohort = None
            for spec in specs:
                found = spec_path(spec["id"]) or spec_path(spec.get("name", ""))
                if found:
                    cohort = found.split("-ingest/")[0]
                    break
            studies[study_id] = (study_id, study_id.rsplit("/", 1)[-1], cohort, record.get("name"))

            via_expr = set()
            for part in parts:
                if part.get("entity_type") == "dataset":
                    pht = part["id"].removeprefix("dbgap:")
                    datasets.setdefault(pht, (pht, study_id, None))
                    for var in part.get("has_part") or []:
                        if "(via expression)" in (var.get("description") or ""):
                            via_expr.add(var["id"].removeprefix("dbgap:"))

            for part in parts:
                derived_from = part.get("derived_from")
                if not derived_from:
                    continue
                spec_ids = [d for d in derived_from if spec_path(d)]
                phts = [d.removeprefix("dbgap:") for d in derived_from if d.startswith("dbgap:pht")]
                phvs = [d.removeprefix("dbgap:") for d in derived_from if d.startswith("dbgap:phv")]
                spec_id = spec_ids[0] if spec_ids else None
                derivations.append(
                    (
                        part["id"],
                        spec_id,
                        spec_path(spec_id) if spec_id else None,
                        derived_class(part["id"]),
                        study_id,
                        phts[0] if phts else None,
                        part.get("name"),
                    )
                )
                sources.extend((part["id"], phv, phv in via_expr) for phv in phvs)

        if studies:
            self.con.executemany(
                "INSERT INTO varlib.study VALUES (?, ?, ?, ?) ON CONFLICT (id) DO UPDATE SET "
                "cohort = coalesce(varlib.study.cohort, excluded.cohort), name = coalesce(varlib.study.name, excluded.name)",
                list(studies.values()),
            )
        if datasets:
            self.con.executemany(
                "INSERT INTO varlib.dataset VALUES (?, ?, ?) ON CONFLICT (pht) DO UPDATE SET "
                "study_id = coalesce(varlib.dataset.study_id, excluded.study_id)",
                list(datasets.values()),
            )
        count = self.insert("derivation", derivations)
        self.insert("derivation_source", sources)
        self.manifest("mapping_provenance", path, count, digest=sha256(path))
        print(f"  {path.name}: {len(studies)} studies, {count} derivations, {len(sources)} derivation sources")

    def concepts(self, schema_dir: Path):
        concepts, mappings, sources, datasets, declared, enum_values = [], [], [], [], [], []
        for module_path in sorted(schema_dir.glob("bdc_varlib_*.yaml")):
            module = yaml.safe_load(module_path.read_text()) or {}
            # module per BDCHM class: "BDC Variable Library: Condition"
            category = (module.get("title") or "").split(":")[-1].strip() or None
            for enum_name, enum in (module.get("enums") or {}).items():
                for value, pv in ((enum or {}).get("permissible_values") or {}).items():
                    pv = pv or {}
                    enum_values.append((enum_name, str(value), pv.get("meaning"), pv.get("description")))
            for slot_name, slot in (module.get("slots") or {}).items():
                concept_id = f"bdcvl:{slot_name}"
                notes = slot.get("annotations") or {}
                concepts.append(
                    (
                        concept_id,
                        notes.get("concept_id"),
                        slot_name,
                        category,
                        module.get("name"),
                        notes.get("bdchm_class"),
                        slot.get("title"),
                        slot.get("description"),
                        slot.get("range"),
                    )
                )
                for key, value in slot.items():
                    if key.endswith("_mappings"):
                        predicate = "skos:" + key.removesuffix("_mappings") + "Match"
                        mappings.extend((concept_id, predicate, curie) for curie in value or [])
                studies = split_list(notes.get("source_study"))
                for source_file in split_list(notes.get("source_file")):
                    cohort = source_file.split("-ingest/")[0] if "-ingest/" in source_file else None
                    sources.append((concept_id, cohort, source_file))
                if not notes.get("source_file"):
                    sources.extend((concept_id, study, None) for study in studies)
                datasets.extend((concept_id, pht) for pht in split_list(notes.get("dataset_accession")))
                accessions = str(notes.get("variable_accession") or "")
                if NOT_LISTED not in accessions:
                    declared.extend((concept_id, phv) for phv in split_list(accessions) if phv.startswith("phv"))
        count = self.insert("concept", concepts)
        self.insert("concept_mapping", mappings)
        self.insert("concept_source", sources)
        self.insert("concept_dataset", datasets)
        self.insert("concept_variable_declared", declared)
        self.insert("enum_value", enum_values)
        entry = schema_dir / "bdc_varlib.yaml"
        self.manifest("concepts", schema_dir, count, digest=sha256(entry) if entry.exists() else None)
        print(
            f"  BDC-VarLib: {count} concepts, {len(mappings)} mappings, {len(sources)} sources, "
            f"{len(declared)} declared phvs, {len(enum_values)} enum values"
        )


def load_sssom(con: duckdb.DuckDBPyConnection, path: Path):
    """Append one SSSOM TSV to main.mappings, replacing any earlier load of the same file."""
    source = path.name.removesuffix(".tsv")  # omop2loinc.sssom, matching the base sets' convention
    columns = [row[0] for row in con.execute("DESCRIBE main.mappings").fetchall()]
    lines = [line for line in path.read_text().splitlines() if line and not line.startswith("#")]
    rows = list(csv.DictReader(lines, delimiter="\t"))
    con.execute("DELETE FROM main.mappings WHERE mapping_source = ?", [source])
    values = [[source if c == "mapping_source" else (row.get(c) or None) for c in columns] for row in rows]
    if values:
        con.executemany(f"INSERT INTO main.mappings VALUES ({', '.join('?' * len(columns))})", values)
    con.execute(
        "INSERT INTO varlib.load_manifest (input, path, sha256, row_count) VALUES ('sssom', ?, ?, ?)",
        [str(path), sha256(path), len(values)],
    )
    print(f"  {path.name}: {len(values)} mappings into main.mappings as {source}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("db", type=Path)
    parser.add_argument("--variable-library", type=Path, nargs="*", default=[])
    parser.add_argument("--mapping-provenance", type=Path, nargs="*", default=[])
    parser.add_argument("--concepts-dir", type=Path)
    parser.add_argument("--sssom", type=Path, nargs="*", default=[])
    parser.add_argument("--source-repos", type=Path, nargs="*", default=[])
    args = parser.parse_args()

    con = duckdb.connect(str(args.db))
    con.execute((ROOT / "sql" / "varlib_schema.sql").read_text())
    loader = Loader(con)
    print(f"Loading varlib into {args.db}")
    for path in args.mapping_provenance:
        loader.mapping_provenance(path)
    for path in args.variable_library:
        loader.variable_library(path)
    if args.concepts_dir:
        loader.concepts(args.concepts_dir)
    for path in args.sssom:
        load_sssom(con, path)
    for repo in args.source_repos:
        commit = git_commit(repo)
        loader.manifest("source_repo", repo, 0, repo=repo.name, commit=commit)
        print(f"  {repo.name} @ {commit}")
    con.execute((ROOT / "sql" / "varlib_views.sql").read_text())
    con.close()


if __name__ == "__main__":
    main()
