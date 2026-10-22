-- One search document per KG node: the input to `bdc-search index`, exported as JSONL by
-- `just search-index`. Fields follow Monarch's entity core (monarch-app SearchResult).
--
-- Labels from restricted sources are blanked here so the index never redistributes them:
-- the SemMedDB snomedct tier carries SNOMED CT labels under the SNOMED license.
SELECT
    id,
    category,
    CASE WHEN provided_by IN ('procedure_to_disease_snomedct_nodes') THEN NULL ELSE name END AS name,
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
    type
FROM nodes
