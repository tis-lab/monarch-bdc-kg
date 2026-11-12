"""The search exports' contracts: what scripts/export_search.py and sql/search_*.sql produce.

Each test builds a small duckdb with only the tables the exports read, runs the real export
functions over the real SQL files, and checks the JSONL they write.
"""

import importlib.util
import json
from pathlib import Path

import duckdb
import pytest

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("export_search", ROOT / "scripts" / "export_search.py")
export_search = importlib.util.module_from_spec(spec)
spec.loader.exec_module(export_search)

RESTRICTED = "restricted_nodes"

NODES = """
CREATE TABLE nodes (
    id VARCHAR, category VARCHAR, name VARCHAR, full_name VARCHAR, symbol VARCHAR,
    description VARCHAR, xref VARCHAR[], synonym VARCHAR[], exact_synonym VARCHAR[],
    broad_synonym VARCHAR[], narrow_synonym VARCHAR[], related_synonym VARCHAR[],
    deprecated VARCHAR, in_taxon VARCHAR, in_taxon_label VARCHAR, iri VARCHAR,
    same_as VARCHAR[], subsets VARCHAR[], provided_by VARCHAR, file_source VARCHAR,
    namespace VARCHAR, type VARCHAR
)
"""

VARLIB = """
CREATE SCHEMA varlib;
CREATE TABLE varlib.concept (id VARCHAR, code VARCHAR, slot_name VARCHAR, category VARCHAR, module VARCHAR,
    bdchm_class VARCHAR, title VARCHAR, description VARCHAR, range VARCHAR);
CREATE TABLE varlib.concept_source (concept_id VARCHAR, cohort VARCHAR, source_file VARCHAR);
CREATE TABLE varlib.load_manifest (input VARCHAR, path VARCHAR, repo VARCHAR, commit VARCHAR, sha256 VARCHAR,
    row_count BIGINT, loaded_at TIMESTAMP DEFAULT current_timestamp);
"""


def node(con, id, category, name, provided_by="phenio_nodes", deprecated=None, in_taxon=None, **labels):
    con.execute(
        """INSERT INTO nodes (id, category, name, provided_by, deprecated, in_taxon, synonym, description)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?)""",
        [id, category, name, provided_by, deprecated, in_taxon, labels.get("synonym"), labels.get("description")],
    )


def concept(con, id, code, bdchm_class, *sources):
    con.execute("INSERT INTO varlib.concept (id, code, slot_name, bdchm_class) VALUES (?, ?, ?, ?)",
                [id, code, id, bdchm_class])
    for cohort, source_file in sources:
        con.execute("INSERT INTO varlib.concept_source VALUES (?, ?, ?)", [id, cohort, source_file])


def export(con, tmp_path, sql_file):
    export_search.define_search_nodes(con, [RESTRICTED])
    out = tmp_path / f"{sql_file}.jsonl"
    export_search.export_json(con, ROOT / "sql" / f"{sql_file}.sql", out)
    rows = [json.loads(line) for line in out.read_text().splitlines()]
    return {row.get("id") or row["conceptType"]: row for row in rows}


@pytest.fixture
def con():
    con = duckdb.connect()
    con.execute(NODES)
    return con


def test_restricted_labels_are_blank_in_both_exports(con, tmp_path):
    node(con, "SNOMEDCT:1", "biolink:Procedure", "licensed label", provided_by=RESTRICTED,
         synonym=["licensed synonym"], description="licensed text")
    con.execute(VARLIB)
    concept(con, "bdcvl:snomedct_1", "SNOMEDCT:1", "Procedure", ("ARIC", "ARIC-ingest/x.yaml"))

    entity = export(con, tmp_path, "search_entity")["SNOMEDCT:1"]
    assert (entity["name"], entity["synonym"], entity["description"]) == (None, None, None)

    harmonized = export(con, tmp_path, "search_concept")["SNOMEDCT:1"]
    assert harmonized["conceptLabel"] is None
    assert harmonized["displayLabel"] == "SNOMEDCT:1"
    assert harmonized["kgNodeId"] == "SNOMEDCT:1", "the KG link is kept"


def test_static_boosts_follow_monarch(con, tmp_path):
    node(con, "MONDO:1", "biolink:Disease", "a disease")
    node(con, "HP:1", "biolink:PhenotypicFeature", "a phenotype")
    node(con, "HGNC:1", "biolink:Gene", "a human gene", in_taxon="NCBITaxon:9606")
    node(con, "MGI:1", "biolink:Gene", "a mouse gene", in_taxon="NCBITaxon:10090")
    node(con, "MONDO:2", "biolink:Disease", "a deprecated disease", deprecated="True")
    boosts = {id: round(row["static_boost"], 6) for id, row in export(con, tmp_path, "search_entity").items()}
    assert boosts == {"MONDO:1": 1.3, "HP:1": 1.1, "HGNC:1": 1.1, "MGI:1": 1.0, "MONDO:2": 0.13}


def test_concepts_are_labelled_from_the_kg_and_keyed_case_insensitively(con, tmp_path):
    node(con, "MONDO:0005044", "biolink:Disease", "hypertensive disorder")
    con.execute(VARLIB)
    concept(con, "bdcvl:a", "MONDO:0005044", "Condition", ("ARIC", "ARIC-ingest/hypertension.yaml"))
    concept(con, "bdcvl:b", "mondo:0005044", "Condition", ("JHS", "JHS-ingest/hyperten.yaml"))
    concept(con, "bdcvl:c", "OMOP:1", "Condition", ("FHS", "FHS-ingest/x.yaml"))

    concepts = export(con, tmp_path, "search_concept")
    hypertension = concepts["MONDO:0005044"]
    assert hypertension["conceptLabel"] == "hypertensive disorder"
    assert hypertension["studies"] == ["ARIC", "JHS"], "both spellings group into one concept"
    assert concepts["OMOP:1"]["displayLabel"] == "OMOP:1", "no KG label: the CURIE"


def test_label_id_and_category_come_from_one_node(con, tmp_path):
    node(con, "MONDO:1", "biolink:Disease", None)
    node(con, "mondo:1", "biolink:PhenotypicFeature", "the labelled one")
    con.execute(VARLIB)
    concept(con, "bdcvl:a", "MONDO:1", "Condition", ("ARIC", "ARIC-ingest/a.yaml"))
    row = export(con, tmp_path, "search_concept")["MONDO:1"]
    assert (row["conceptLabel"], row["kgNodeId"], row["kgCategory"]) == (
        "the labelled one", "mondo:1", "biolink:PhenotypicFeature")


def test_studies_without_a_spec_file_still_count(con, tmp_path):
    con.execute(VARLIB)
    concept(con, "bdcvl:a", "MONDO:1", "Condition", ("ARIC", None), ("JHS", "JHS-ingest/a.yaml"))
    row = export(con, tmp_path, "search_concept")["MONDO:1"]
    assert row["studies"] == ["ARIC", "JHS"]
    assert [m["study"] for m in row["sourceMappings"]] == ["JHS"]
    assert [v["variableName"] for v in row["variables"]] == ["a"]


def test_a_partial_varlib_schema_is_skipped(con):
    con.execute("CREATE SCHEMA varlib; CREATE TABLE varlib.concept (id VARCHAR)")
    assert not export_search.has_varlib(con)
    con.execute(VARLIB.replace("CREATE SCHEMA varlib;", "").replace("CREATE TABLE varlib.concept ", "CREATE OR REPLACE TABLE varlib.concept "))
    assert export_search.has_varlib(con)
