-- One (ancestor, descendant) row per entry of each harmonized concept's closure: the rows of
-- the cohort builder's catalog.term_closure (bdc-dp-middleware#24). Each concept is its own
-- descendant. Reads `search_concept`, the concepts scripts/export_search.py just exported.
SELECT DISTINCT ancestor, "conceptType" AS descendant
FROM search_concept, unnest(closure) AS t(ancestor)
ORDER BY ancestor, descendant
