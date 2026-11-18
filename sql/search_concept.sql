-- One harmonized concept per concept code: the input to bdc-search's /bdc routes, exported as
-- JSONL by `just search-index`. Rows follow the middleware's harmonized index (schemaVersion 2,
-- tis-lab/bdc-dp-middleware tools/build_harmonized_index.py) so it can load them unchanged:
-- one entry per CURIE across BDCHM classes, studies that assign it, and the spec files
-- (variableName) that carry it. kgNodeId and kgCategory are additions it ignores.
--
-- conceptLabel is the KG's label for the code, read from `search_nodes` (defined by
-- scripts/export_search.py) so restricted labels are blank while the KG link is kept.
-- displayLabel is the label, else the CURIE: never null and never a spec file name.
--
-- closure is the concept code, the KG nodes it is or maps to by exactMatch (its anchors,
-- varlib.concept_anchor), and every superclass of those (rdfs:subClassOf in the KG closure).
-- A concept is "under" each of these terms, the shape of the cohort builder's term_closure
-- (bdc-dp-middleware#24): one (ancestor, descendant) pair per entry. closureLabel is their
-- labels, from `search_nodes` like conceptLabel, as Monarch pairs *_closure with
-- *_closure_label.
--
-- The concept key (prefix upper-cased) and the CURIE pattern must match search/src/concepts.rs.
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
    -- a study can be listed without a spec file; it still counts toward studies
    WHERE cs.cohort IS NOT NULL
),
carriers AS (
    SELECT concept_key, variable_name, list(DISTINCT study ORDER BY study) AS studies
    FROM sourced
    WHERE variable_name IS NOT NULL
    GROUP BY concept_key, variable_name
),
anchors AS (
    SELECT DISTINCT k.concept_key, a.node_id
    FROM classed k
    JOIN varlib.concept_anchor a ON a.concept_id = k.id AND a.node_id IS NOT NULL
),
anchored AS (
    -- the anchors themselves, even where the KG has no hierarchy for them (LOINC)
    SELECT concept_key, node_id AS ancestor FROM anchors
    UNION
    SELECT a.concept_key, cl.object_id
    FROM anchors a
    JOIN main.closure cl ON cl.subject_id = a.node_id AND cl.predicate_id = 'rdfs:subClassOf'
),
closure_ids AS (
    SELECT concept_key, min(code) AS id FROM classed GROUP BY concept_key
    UNION
    SELECT concept_key, ancestor FROM anchored
),
closures AS (
    SELECT
        c.concept_key,
        list(DISTINCT c.id ORDER BY c.id) AS ids,
        coalesce(list(DISTINCT n.name ORDER BY n.name) FILTER (WHERE nullif(trim(n.name), '') IS NOT NULL), [])
            AS labels
    FROM closure_ids c
    LEFT JOIN search_nodes n ON n.id = c.id
    GROUP BY c.concept_key
),
kg AS (
    SELECT
        upper(split_part(id, ':', 1)) || substr(id, position(':' IN id)) AS concept_key,
        id,
        nullif(trim(name), '') AS name,
        category
    FROM search_nodes
    WHERE position(':' IN id) > 0
    -- one node per key, so label, id and category always come from the same node: a labelled
    -- node first, then the lowest id
    QUALIFY row_number() OVER (PARTITION BY concept_key ORDER BY nullif(trim(name), '') IS NULL, id) = 1
)
SELECT
    min(k.code) AS "conceptType",
    any_value(l.name) AS "conceptLabel",
    coalesce(any_value(l.name), min(k.code)) AS "displayLabel",
    CASE WHEN count(DISTINCT k.bdchm_type) = 1 THEN any_value(k.bdchm_type) END AS "bdchmType",
    list(DISTINCT k.bdchm_class ORDER BY k.bdchm_class) AS "bdchmClasses",
    (SELECT list({'study': s.study, 'bdchmClass': s.bdchm_class, 'conceptSlot': s.concept_slot,
                  'sourceFile': s.source_file}
                 ORDER BY s.study, s.bdchm_class, s.concept_slot, s.source_file)
     FROM (SELECT DISTINCT study, bdchm_class, concept_slot, source_file
           FROM sourced s WHERE s.concept_key = k.concept_key AND s.source_file IS NOT NULL) s) AS "sourceMappings",
    (SELECT count(DISTINCT study) FROM sourced s WHERE s.concept_key = k.concept_key) AS "studyCount",
    (SELECT list(DISTINCT study ORDER BY study) FROM sourced s WHERE s.concept_key = k.concept_key) AS "studies",
    (SELECT list({'variableName': v.variable_name, 'variableLabel': NULL::VARCHAR, 'studies': v.studies}
                 ORDER BY v.variable_name)
     FROM carriers v WHERE v.concept_key = k.concept_key) AS "variables",
    any_value(l.id) AS "kgNodeId",
    any_value(l.category) AS "kgCategory",
    any_value(cl.ids) AS "closure",
    any_value(cl.labels) AS "closureLabel"
FROM classed k
LEFT JOIN kg l ON l.concept_key = k.concept_key
LEFT JOIN closures cl ON cl.concept_key = k.concept_key
GROUP BY k.concept_key
HAVING "studyCount" > 0
ORDER BY "conceptType"
