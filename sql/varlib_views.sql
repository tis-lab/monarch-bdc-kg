-- Views that integrate the varlib tables with each other and with the KG.
-- Recreated by scripts/load_varlib.py after every load.

-- Which concept each source variable feeds. BDC-VarLib lists phvs where a spec names them, and
-- those are used as listed; concepts it lists none for are resolved through mapping provenance: a derivation of the same spec file, BDCHM
-- class and dataset as the concept read them. Measurements nested in a
-- MeasurementObservationSet count as MeasurementObservation, as BDC-VarLib files them. Join
-- keys (participant ids read only inside an expr) are excluded: they feed every concept in
-- their table without measuring any.
CREATE OR REPLACE VIEW varlib.variable_concept AS
WITH declaring AS (SELECT DISTINCT concept_id FROM varlib.concept_variable_declared)
SELECT DISTINCT ds.phv, cs.concept_id, 'mapping_provenance' AS via
FROM varlib.derivation_source ds
JOIN varlib.derivation d ON d.id = ds.derivation_id
JOIN varlib.concept_source cs ON cs.source_file = d.spec_path
JOIN varlib.concept c ON c.id = cs.concept_id
 AND (c.bdchm_class = d.bdchm_class
      OR (c.bdchm_class = 'MeasurementObservation' AND d.bdchm_class = 'MeasurementObservationSet'))
JOIN varlib.concept_dataset cd ON cd.concept_id = c.id AND cd.pht = d.pht
WHERE NOT coalesce(ds.via_expression, false)
  -- a concept that lists its phvs is taken at its word: one spec block can derive several
  -- concepts from one table (a case() over drug codes), and provenance alone would attach
  -- every phv the block reads to all of them
  AND c.id NOT IN (SELECT concept_id FROM declaring)
UNION
SELECT phv, concept_id, 'declared' AS via
FROM varlib.concept_variable_declared;

-- Concept ontology anchors, resolved against KG nodes. A curie the KG doesn't carry (OMOP,
-- ATC, RxCUI) gets one skos:exactMatch hop through the KG's mappings or the varlib bridges, e.g. OMOP:3006315 ->
-- LOINC:26444-0 via the monarch-mapping-commons OMOP bridges. `via` records which.
CREATE OR REPLACE VIEW varlib.concept_anchor AS
WITH candidates AS (
    SELECT concept_id, predicate, object_id AS mapped_id, object_id AS kg_id, 'direct' AS via
    FROM varlib.concept_mapping
    UNION
    SELECT m.concept_id, m.predicate, m.object_id,
           CASE WHEN x.subject_id = m.object_id THEN x.object_id ELSE x.subject_id END,
           x.mapping_source
    FROM varlib.concept_mapping m
    JOIN (SELECT subject_id, predicate_id, object_id, mapping_source FROM main.mappings
          UNION ALL
          SELECT subject_id, predicate_id, object_id, mapping_source FROM varlib.bridge_mapping) x
      ON x.predicate_id = 'skos:exactMatch'
     AND m.object_id IN (x.subject_id, x.object_id)
)
SELECT
    c.concept_id,
    c.predicate,
    c.mapped_id AS object_id,
    c.via,
    n.id IS NOT NULL AS in_kg,
    n.id AS node_id,
    n.name,
    n.category,
    n.exact_synonym
FROM candidates c
LEFT JOIN main.nodes n ON n.id = c.kg_id
-- keep unresolved direct rows (so coverage gaps stay visible), drop hops that miss the KG
WHERE c.via = 'direct' OR n.id IS NOT NULL;

-- One row per harmonized concept, with what it pools and what it points at.
CREATE OR REPLACE VIEW varlib.concept_doc AS
WITH members AS (
    SELECT concept_id, count(DISTINCT phv) AS variable_count
    FROM varlib.variable_concept GROUP BY concept_id
),
cohorts AS (
    SELECT concept_id, list(DISTINCT cohort ORDER BY cohort) AS cohorts
    FROM varlib.concept_source WHERE cohort IS NOT NULL GROUP BY concept_id
),
anchors AS (
    SELECT
        concept_id,
        list(DISTINCT object_id ORDER BY object_id) AS anchor_ids,
        list(DISTINCT node_id ORDER BY node_id) FILTER (WHERE node_id IS NOT NULL) AS anchor_node_ids,
        list(DISTINCT name ORDER BY name) FILTER (WHERE name IS NOT NULL) AS anchor_labels
    FROM varlib.concept_anchor GROUP BY concept_id
)
SELECT
    c.id,
    c.code,
    c.slot_name,
    coalesce(c.title, c.code) AS display_name,
    c.category,
    c.bdchm_class,
    coalesce(c.title, c.code) AS name,
    a.anchor_ids,
    a.anchor_node_ids,
    a.anchor_labels,
    co.cohorts,
    coalesce(m.variable_count, 0) AS variable_count
FROM varlib.concept c
LEFT JOIN members m ON m.concept_id = c.id
LEFT JOIN cohorts co ON co.concept_id = c.id
LEFT JOIN anchors a ON a.concept_id = c.id;

-- One row per source variable: the document a variable search index would ingest.
-- Anchor ancestors come from the KG closure, so a search for "diabetes" can reach an HbA1c
-- variable whose own text never says diabetes.
CREATE OR REPLACE VIEW varlib.variable_doc AS
WITH concepts AS (
    SELECT
        vc.phv,
        list(DISTINCT c.id ORDER BY c.id) AS concept_ids,
        list(DISTINCT coalesce(c.title, c.code)) AS concept_names,
        list(DISTINCT c.category) FILTER (WHERE c.category IS NOT NULL) AS categories
    FROM varlib.variable_concept vc
    JOIN varlib.concept c ON c.id = vc.concept_id
    GROUP BY vc.phv
),
anchors AS (
    SELECT
        vc.phv,
        list(DISTINCT a.node_id ORDER BY a.node_id) FILTER (WHERE a.node_id IS NOT NULL) AS anchor_ids,
        list(DISTINCT a.name) FILTER (WHERE a.name IS NOT NULL) AS anchor_labels
    FROM varlib.variable_concept vc
    JOIN varlib.concept_anchor a ON a.concept_id = vc.concept_id
    GROUP BY vc.phv
),
ancestors AS (
    SELECT
        vc.phv,
        list(DISTINCT n.name) FILTER (WHERE n.name IS NOT NULL) AS ancestor_labels
    FROM varlib.variable_concept vc
    JOIN varlib.concept_anchor a ON a.concept_id = vc.concept_id AND a.node_id IS NOT NULL
    JOIN main.closure cl ON cl.subject_id = a.node_id AND cl.predicate_id = 'rdfs:subClassOf'
    JOIN main.nodes n ON n.id = cl.object_id
    GROUP BY vc.phv
),
join_keys AS (
    -- read by specs only inside expressions (participant/visit ids): no measurement of their own
    SELECT phv
    FROM varlib.derivation_source
    GROUP BY phv
    HAVING bool_and(coalesce(via_expression, false))
),
codes AS (
    SELECT
        variable_id,
        list(indicator_meaning ORDER BY ordinal) FILTER (WHERE indicator_meaning IS NOT NULL) AS value_labels
    FROM varlib.variable_value
    WHERE value_kind = 'coded'
    GROUP BY variable_id
)
SELECT
    v.id,
    v.phv,
    v.pht,
    v.study_id,
    s.cohort,
    s.name AS study_name,
    v.variable_class,
    v.variable_name AS name,
    v.source_variable_description AS description,
    v.data_type,
    v.unit,
    v.file_name,
    jk.phv IS NOT NULL AS join_key_only,
    c.concept_ids,
    c.concept_names,
    c.categories,
    a.anchor_ids,
    a.anchor_labels,
    an.ancestor_labels,
    cd.value_labels
FROM varlib.variable v
LEFT JOIN varlib.study s ON s.id = v.study_id
LEFT JOIN concepts c ON c.phv = v.phv
LEFT JOIN anchors a ON a.phv = v.phv
LEFT JOIN ancestors an ON an.phv = v.phv
LEFT JOIN codes cd ON cd.variable_id = v.id
LEFT JOIN join_keys jk ON jk.phv = v.phv;

-- Every surface string that names a variable or concept, and what it points at. This is
-- the input to a words-mode autocomplete: an FST build tokenizes `term`, and `weight`
-- ranks completions. Concept-level terms are weighted by how many variables they pool.
CREATE OR REPLACE VIEW varlib.lexicon AS
SELECT description AS term, 'variable_description' AS term_kind,
       'variable' AS target_kind, id AS target_id, 1.0 AS weight
FROM varlib.variable_doc WHERE description IS NOT NULL AND NOT join_key_only
UNION ALL
SELECT name, 'variable_name', 'variable', id, 1.0
FROM varlib.variable_doc WHERE name IS NOT NULL AND NOT join_key_only
UNION ALL
SELECT DISTINCT indicator_meaning, 'value_label', 'variable', variable_id, 0.5
FROM varlib.variable_value WHERE value_kind = 'coded' AND indicator_meaning IS NOT NULL
UNION ALL
SELECT display_name, 'concept_name', 'concept', id, 1.0 + variable_count
FROM varlib.concept_doc
UNION ALL
SELECT a.name, 'anchor_label', 'concept', a.concept_id, 1.0 + d.variable_count
FROM varlib.concept_anchor a JOIN varlib.concept_doc d ON d.id = a.concept_id
WHERE a.name IS NOT NULL
UNION ALL
SELECT DISTINCT syn, 'anchor_synonym', 'concept', a.concept_id, 0.5 * (1.0 + d.variable_count)
FROM varlib.concept_anchor a
JOIN varlib.concept_doc d ON d.id = a.concept_id,
     unnest(a.exact_synonym) AS t(syn)
WHERE syn IS NOT NULL;

-- Words with their document frequency across lexicon targets: the first cut at an FST's
-- (key, weight) pairs. Lowercased, split on anything non-alphanumeric; single characters
-- and English stopwords dropped. concept_name rows are left out: a concept's name is either its
-- KG label, already present as an anchor_label row, or a fallback to spec file names
-- (hist_hrtdis); variable_name rows are dbGaP VARNAMEs (HYPERT01). Both are codes whose
-- fragments would pollute word completion.
CREATE OR REPLACE VIEW varlib.lexicon_word AS
WITH words AS (
    SELECT l.target_kind, l.target_id, l.term_kind, l.weight, w AS word
    FROM varlib.lexicon l,
         unnest(regexp_split_to_array(lower(l.term), '[^a-z0-9]+')) AS t(w)
    WHERE l.term_kind NOT IN ('concept_name', 'variable_name')
      AND length(w) > 1
      AND w NOT IN ('a', 'an', 'and', 'are', 'as', 'at', 'be', 'by', 'for', 'from', 'has', 'in', 'is', 'it',
                    'of', 'on', 'or', 'that', 'the', 'to', 'was', 'were', 'with')
)
SELECT
    word,
    count(DISTINCT target_id) FILTER (WHERE target_kind = 'variable') AS variable_freq,
    count(DISTINCT target_id) FILTER (WHERE target_kind = 'concept') AS concept_freq,
    sum(weight)::DOUBLE AS weight,
    list(DISTINCT term_kind ORDER BY term_kind) AS term_kinds
FROM words
GROUP BY word;

-- bdc-variable-library variables to harmonized concepts, by the concept code the variable's
-- microschema record fixes (prefix compared case-insensitively). Integrated variables carry no
-- code in the schema yet, so only compound variables with a condition, drug or procedure code
-- link here.
CREATE OR REPLACE VIEW varlib.library_variable_concept AS
SELECT DISTINCT lc.variable_id, c.id AS concept_id, lc.code
FROM varlib.library_variable_code lc
JOIN varlib.concept c
  ON upper(split_part(c.code, ':', 1)) || substr(c.code, position(':' IN c.code))
   = upper(split_part(lc.code, ':', 1)) || substr(lc.code, position(':' IN lc.code));
