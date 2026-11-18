-- The vocabulary for /bdc/words: every word in KG labels and in BDC variable and concept text,
-- with how many BDC variables, harmonized concepts and KG nodes use it. One row per word, in
-- byte order (bdc-search builds an FST from it).
--
-- Reads two views scripts/export_search.py defines:
--   search_nodes  KG nodes, with labels of restricted sources blanked
--   bdc_word      varlib.lexicon_word, or empty without the variable library
--
-- KG words come from names and synonyms of nodes that are human or have no taxon, so other
-- species' gene symbols stay out. A word found in a single KG label is mostly noise (typos,
-- one-off codes) unless BDC text uses it.
WITH labels AS (
    SELECT id, name AS label
    FROM search_nodes
    WHERE name IS NOT NULL AND (in_taxon IS NULL OR in_taxon = 'NCBITaxon:9606')
    UNION ALL
    SELECT id, s
    FROM search_nodes, unnest(synonym) AS t(s)
    WHERE in_taxon IS NULL OR in_taxon = 'NCBITaxon:9606'
),
kg AS (
    SELECT w AS word, count(DISTINCT id) AS kg_nodes
    FROM labels, unnest(regexp_split_to_array(lower(label), '[^a-z0-9]+')) AS t(w)
    GROUP BY w
),
words AS (
    SELECT
        coalesce(k.word, b.word) AS word,
        coalesce(b.variable_freq, 0) AS variables,
        coalesce(b.concept_freq, 0) AS concepts,
        coalesce(k.kg_nodes, 0) AS kg_nodes,
        -- anchor labels are KG labels: a word only they use must also be in a visible KG
        -- label, or a restricted source's label could leak through its concept
        coalesce(list_has_any(b.term_kinds, ['variable_description', 'value_label']), false)
            AS in_variable_text
    FROM kg k
    FULL JOIN bdc_word b ON b.word = k.word
)
SELECT word, variables, concepts, kg_nodes
FROM words
WHERE length(word) > 1
  AND regexp_matches(word, '^[a-z]')
  -- dbGaP VARNAME-style codes (HYPERT01) that leak into variable descriptions
  AND NOT regexp_matches(word, '^[a-z]{3,}[0-9]{2,}$')
  AND word NOT IN ('an', 'and', 'are', 'as', 'at', 'be', 'by', 'for', 'from', 'has', 'in', 'is',
                   'it', 'of', 'on', 'or', 'that', 'the', 'to', 'was', 'were', 'with')
  AND (kg_nodes >= 2 OR (variables + concepts > 0 AND (kg_nodes > 0 OR in_variable_text)))
ORDER BY word COLLATE "c"
