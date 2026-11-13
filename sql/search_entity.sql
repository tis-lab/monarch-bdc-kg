-- One search document per KG node: the input to `bdc-search index`, exported as JSONL by
-- scripts/export_search.py (`just search-index`). Fields follow Monarch's entity core
-- (monarch-app SearchResult).
--
-- Reads `search_nodes`, which the export script defines over `nodes` with the labels of
-- restricted sources blanked (varlib.yaml restricted_label_sources), so the index never
-- redistributes them.
SELECT
    id,
    category,
    name,
    full_name,
    symbol,
    description,
    xref,
    synonym,
    exact_synonym,
    broad_synonym,
    narrow_synonym,
    related_synonym,
    coalesce(lower(deprecated) = 'true', false) AS deprecated,
    in_taxon,
    in_taxon_label,
    iri,
    same_as,
    subsets,
    provided_by,
    file_source,
    namespace,
    type,
    -- Monarch's static boosts (monarch-app entity_boost): disease x1.3, phenotype x1.1,
    -- human gene x1.1, deprecated x0.1. Query-dependent boosts are applied by bdc-search.
    CASE
        WHEN category = 'biolink:Disease' THEN 1.3
        WHEN category = 'biolink:PhenotypicFeature' THEN 1.1
        WHEN category = 'biolink:Gene' AND in_taxon = 'NCBITaxon:9606' THEN 1.1
        ELSE 1.0
    END
    * CASE WHEN coalesce(lower(deprecated) = 'true', false) THEN 0.1 ELSE 1.0 END AS static_boost
FROM search_nodes
