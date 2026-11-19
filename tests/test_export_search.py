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
CREATE TABLE varlib.concept_anchor (concept_id VARCHAR, node_id VARCHAR);
CREATE TABLE closure (subject_id VARCHAR, predicate_id VARCHAR, object_id VARCHAR);
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


def read(path):
    """A written export's rows as dicts: JSON lines, or Parquet (nested lists and structs kept)."""
    if path.suffix == ".parquet":
        result = duckdb.connect().execute(f"SELECT * FROM read_parquet('{path}')")
        columns = [d[0] for d in result.description]
        return [dict(zip(columns, row)) for row in result.fetchall()]
    return [json.loads(line) for line in path.read_text().splitlines()]


def export(con, tmp_path, sql_file):
    export_search.define_search_nodes(con, [RESTRICTED])
    out = tmp_path / (f"{sql_file}.jsonl" if sql_file == "search_entity" else f"{sql_file}.parquet")
    export_search.export(con, ROOT / "sql" / f"{sql_file}.sql", out)
    return {row.get("id") or row["conceptType"]: row for row in read(out)}


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


def test_closure_holds_the_code_its_anchors_and_their_superclasses(con, tmp_path):
    con.execute(VARLIB)
    concept(con, "bdcvl:a", "OMOP:1", "MeasurementObservation", ("ARIC", "ARIC-ingest/a.yaml"))
    concept(con, "bdcvl:b", "MONDO:2", "Condition", ("JHS", "JHS-ingest/b.yaml"))
    con.execute("""INSERT INTO varlib.concept_anchor VALUES
        ('bdcvl:a', 'LOINC:9'), ('bdcvl:b', 'MONDO:2'), ('bdcvl:b', NULL)""")
    con.execute("""INSERT INTO closure VALUES
        ('MONDO:2', 'rdfs:subClassOf', 'MONDO:2'), ('MONDO:2', 'rdfs:subClassOf', 'MONDO:1'),
        ('MONDO:2', 'BFO:0000050', 'UBERON:1')""")
    concepts = export(con, tmp_path, "search_concept")
    assert concepts["OMOP:1"]["closure"] == ["LOINC:9", "OMOP:1"], "an anchor without hierarchy still counts"
    assert concepts["MONDO:2"]["closure"] == ["MONDO:1", "MONDO:2"], "subclass only, not part-of"


def test_closure_labels_come_from_the_kg_and_respect_restricted_sources(con, tmp_path):
    node(con, "MONDO:1", "biolink:Disease", "cardiovascular disorder")
    node(con, "MONDO:2", "biolink:Disease", "hypertensive disorder")
    node(con, "SNOMEDCT:3", "biolink:Disease", "licensed label", provided_by=RESTRICTED)
    con.execute(VARLIB)
    concept(con, "bdcvl:b", "MONDO:2", "Condition", ("JHS", "JHS-ingest/b.yaml"))
    con.execute("INSERT INTO varlib.concept_anchor VALUES ('bdcvl:b', 'MONDO:2')")
    con.execute("""INSERT INTO closure VALUES
        ('MONDO:2', 'rdfs:subClassOf', 'MONDO:2'), ('MONDO:2', 'rdfs:subClassOf', 'MONDO:1'),
        ('MONDO:2', 'rdfs:subClassOf', 'SNOMEDCT:3')""")
    row = export(con, tmp_path, "search_concept")["MONDO:2"]
    assert row["closure"] == ["MONDO:1", "MONDO:2", "SNOMEDCT:3"]
    assert row["closureLabel"] == ["cardiovascular disorder", "hypertensive disorder"]


def test_concepts_carry_their_hv_commit_and_a_term_closure_table(con, tmp_path):
    node(con, "MONDO:1", "biolink:Disease", "disease")
    con.execute(VARLIB)
    concept(con, "bdcvl:b", "MONDO:2", "Condition", ("JHS", "JHS-ingest/b.yaml"))
    con.execute("INSERT INTO varlib.concept_anchor VALUES ('bdcvl:b', 'MONDO:2')")
    con.execute("INSERT INTO closure VALUES ('MONDO:2', 'rdfs:subClassOf', 'MONDO:1')")
    con.execute("""INSERT INTO varlib.load_manifest (input, repo, commit, loaded_at)
                   VALUES ('source_repo', 'NHLBI-BDC-DMC-HV', '3260ff3', '2026-11-12 10:00')""")
    export_search.define_search_nodes(con, [])
    export_search.export_concepts(con, tmp_path)

    metadata = dict(duckdb.connect().execute(
        f"SELECT decode(key), decode(value) FROM parquet_kv_metadata('{tmp_path / 'concept.parquet'}')").fetchall())
    assert metadata == {"revision": "3260ff3", "generated": "2026-11-12"}
    pairs = [(r["ancestor"], r["descendant"]) for r in read(tmp_path / "term_closure.parquet")]
    assert pairs == [("MONDO:1", "MONDO:2"), ("MONDO:2", "MONDO:2")], "each concept is its own descendant"


def test_a_partial_varlib_schema_is_skipped(con):
    con.execute("CREATE SCHEMA varlib; CREATE TABLE varlib.concept (id VARCHAR)")
    assert not export_search.has_varlib(con)
    con.execute(VARLIB.replace("CREATE SCHEMA varlib;", "").replace("CREATE TABLE varlib.concept ", "CREATE OR REPLACE TABLE varlib.concept "))
    assert export_search.has_varlib(con)


def test_the_controlled_access_deployment_blanks_nothing(con, tmp_path):
    """search.yaml lists no restricted sources: the API is behind access control (ADR 3)."""
    assert export_search.restricted_sources(ROOT / "search.yaml") == []
    node(con, "SNOMEDCT:1", "biolink:Procedure", "licensed label", provided_by=RESTRICTED)
    export_search.define_search_nodes(con, export_search.restricted_sources(ROOT / "search.yaml"))
    out = tmp_path / "entities.jsonl"
    export_search.export(con, ROOT / "sql" / "search_entity.sql", out)
    assert json.loads(out.read_text())["name"] == "licensed label"


def words(con, tmp_path):
    export_search.define_bdc_word(con, export_search.has_varlib(con))
    out = tmp_path / "word.parquet"
    export_search.export(con, ROOT / "sql" / "search_words.sql", out)
    return {row["word"]: row for row in read(out)}


LEXICON = """
    CREATE SCHEMA varlib;
    CREATE TABLE varlib.concept (id VARCHAR);
    CREATE TABLE varlib.concept_source (id VARCHAR);
    CREATE TABLE varlib.load_manifest (id VARCHAR);
    CREATE TABLE varlib.concept_anchor (id VARCHAR);
    CREATE TABLE varlib.lexicon_word (word VARCHAR, variable_freq BIGINT, concept_freq BIGINT,
        term_kinds VARCHAR[]);
"""


def test_words_come_from_kg_labels_in_two_or_more_nodes(con, tmp_path):
    node(con, "MONDO:1", "biolink:Disease", "pulmonary hypertension")
    node(con, "MONDO:2", "biolink:Disease", "pulmonary embolism", synonym=["lung clot"])
    node(con, "MGI:1", "biolink:Gene", "pulmonary mouse gene", in_taxon="NCBITaxon:10090")
    export_search.define_search_nodes(con, [])
    found = words(con, tmp_path)
    assert set(found) == {"pulmonary"}, "one-label words and other species' labels stay out"
    assert found["pulmonary"]["kg_nodes"] == 2


def test_variable_text_words_join_kg_words_and_codes_stay_out(con, tmp_path):
    node(con, "MONDO:1", "biolink:Disease", "hypertension")
    con.execute(LEXICON)
    con.execute("""INSERT INTO varlib.lexicon_word VALUES
        ('hypertension', 33, 2, ['anchor_label', 'variable_description']),
        ('hypert01', 1, 0, ['variable_description']),
        ('diastolic', 4, 0, ['variable_description']),
        ('the', 9, 0, ['variable_description'])""")
    export_search.define_search_nodes(con, [])
    found = words(con, tmp_path)
    assert set(found) == {"hypertension", "diastolic"}
    assert (found["hypertension"]["variables"], found["hypertension"]["kg_nodes"]) == (33, 1)


def test_a_word_only_a_restricted_label_uses_stays_out(con, tmp_path):
    node(con, "SNOMEDCT:1", "biolink:Procedure", "licensedword", provided_by=RESTRICTED)
    con.execute(LEXICON)
    con.execute("INSERT INTO varlib.lexicon_word VALUES ('licensedword', 0, 1, ['anchor_label'])")
    export_search.define_search_nodes(con, [RESTRICTED])
    assert words(con, tmp_path) == {}
