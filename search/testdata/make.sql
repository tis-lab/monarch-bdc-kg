-- Regenerates the Parquet fixtures in this directory, in the shapes scripts/export_search.py
-- writes (column names and types as sql/search_concept.sql and sql/search_words.sql produce):
--   cd search/testdata && duckdb < make.sql
COPY (
    SELECT
        'MONDO:0005044' AS "conceptType",
        'hypertensive disorder' AS "conceptLabel",
        'hypertensive disorder' AS "displayLabel",
        'condition' AS "bdchmType",
        ['Condition'] AS "bdchmClasses",
        [{'study': 'ARIC', 'bdchmClass': 'Condition', 'conceptSlot': 'condition_concept',
          'sourceFile': 'priority_variables_transform/ARIC-ingest/hyperten.yaml'}] AS "sourceMappings",
        2::BIGINT AS "studyCount",
        ['ARIC', 'JHS'] AS studies,
        [{'variableName': 'hyperten', 'variableLabel': NULL::VARCHAR, 'studies': ['ARIC', 'JHS']}] AS variables,
        'MONDO:0005044' AS "kgNodeId",
        'biolink:Disease' AS "kgCategory",
        ['MONDO:0000001', 'MONDO:0005044'] AS closure,
        ['disease', 'hypertensive disorder'] AS "closureLabel"
    UNION ALL
    SELECT 'OMOP:1', NULL, 'OMOP:1', NULL, ['MeasurementObservation'], [], 1, ['FHS'],
           [{'variableName': 'sbp', 'variableLabel': NULL, 'studies': ['FHS']}], NULL, NULL, ['OMOP:1'], []
) TO 'concept.parquet' (FORMAT parquet, COMPRESSION snappy,
                        KV_METADATA {revision: '3260ff3', generated: '2026-11-12'});

COPY (
    SELECT word, variables::BIGINT AS variables, concepts::BIGINT AS concepts, kg_nodes::BIGINT AS kg_nodes
    FROM (VALUES ('hypertension', 33, 2, 143), ('hypoplasia', 0, 0, 1722))
        AS t(word, variables, concepts, kg_nodes)
) TO 'word.parquet' (FORMAT parquet, COMPRESSION snappy);
