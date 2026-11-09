-- One harmonized concept per concept code: the input to bdc-search's /bdc routes, exported as
-- JSONL by `just search-index`. Rows follow the middleware's harmonized index (schemaVersion 2,
-- tis-lab/bdc-dp-middleware tools/build_harmonized_index.py) so it can load them unchanged:
-- one entry per CURIE across BDCHM classes, studies that assign it, and the spec files
-- (variableName) that carry it. kgNodeId and kgCategory are additions it ignores.
--
-- conceptLabel is the KG's label for the code. Labels from restricted sources are left out,
-- as in sql/search_entity.sql.
WITH classed AS (
    SELECT
        c.id,
        c.code,
        c.bdchm_class,
        CASE WHEN position(':' IN c.code) > 0
             THEN upper(split_part(c.code, ':', 1)) || substr(c.code, position(':' IN c.code))
             ELSE c.code END AS concept_key,
        CASE c.bdchm_class
            WHEN 'Condition' THEN 'condition'
            WHEN 'MeasurementObservation' THEN 'observation'
            WHEN 'Observation' THEN 'observation'
            WHEN 'SdohObservation' THEN 'observation'
            WHEN 'DrugExposure' THEN 'drugexposure'
            WHEN 'Procedure' THEN 'procedure'
            WHEN 'Exposure' THEN 'exposure'
        END AS bdchm_type,
        CASE c.bdchm_class
            WHEN 'Condition' THEN 'condition_concept'
            WHEN 'DrugExposure' THEN 'drug_concept'
            WHEN 'Procedure' THEN 'procedure_concept'
            WHEN 'Exposure' THEN 'exposure_concept'
            ELSE 'observation_type'
        END AS concept_slot
    FROM varlib.concept c
    -- CURIEs only, as the middleware's builder does (drops coded values like EDUCATIONAL_ATTAINMENT)
    WHERE regexp_matches(c.code, '^[A-Za-z][A-Za-z0-9._-]*:[A-Za-z0-9._-]+$')
),
sourced AS (
    SELECT
        k.concept_key,
        k.code,
        k.bdchm_class,
        k.concept_slot,
        cs.cohort AS study,
        'priority_variables_transform/' || cs.source_file AS source_file,
        regexp_extract(cs.source_file, '([^/]+)\.yaml$', 1) AS variable_name
    FROM classed k
    JOIN varlib.concept_source cs ON cs.concept_id = k.id
    WHERE cs.source_file IS NOT NULL AND cs.cohort IS NOT NULL
),
carriers AS (
    SELECT concept_key, variable_name, list(DISTINCT study ORDER BY study) AS studies
    FROM sourced
    GROUP BY concept_key, variable_name
),
labels AS (
    SELECT id, name, category
    FROM nodes
    WHERE trim(coalesce(name, '')) <> ''
      AND coalesce(provided_by, '') NOT IN ('procedure_to_disease_snomedct_nodes')
)
SELECT
    min(k.code) AS "conceptType",
    any_value(l.name) AS "conceptLabel",
    CASE WHEN count(DISTINCT k.bdchm_type) = 1 THEN any_value(k.bdchm_type) END AS "bdchmType",
    list(DISTINCT k.bdchm_class ORDER BY k.bdchm_class) AS "bdchmClasses",
    (SELECT list({'study': s.study, 'bdchmClass': s.bdchm_class, 'conceptSlot': s.concept_slot,
                  'sourceFile': s.source_file}
                 ORDER BY s.study, s.bdchm_class, s.concept_slot, s.source_file)
     FROM (SELECT DISTINCT study, bdchm_class, concept_slot, source_file
           FROM sourced s WHERE s.concept_key = k.concept_key) s) AS "sourceMappings",
    (SELECT count(DISTINCT study) FROM sourced s WHERE s.concept_key = k.concept_key) AS "studyCount",
    (SELECT list(DISTINCT study ORDER BY study) FROM sourced s WHERE s.concept_key = k.concept_key) AS "studies",
    (SELECT list({'variableName': v.variable_name, 'variableLabel': NULL::VARCHAR, 'studies': v.studies}
                 ORDER BY v.variable_name)
     FROM carriers v WHERE v.concept_key = k.concept_key) AS "variables",
    any_value(l.id) AS "kgNodeId",
    any_value(l.category) AS "kgCategory"
FROM classed k
LEFT JOIN labels l ON l.id = k.code
GROUP BY k.concept_key
HAVING "studyCount" > 0
ORDER BY "conceptType"
