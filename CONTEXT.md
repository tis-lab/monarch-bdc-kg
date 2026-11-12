# Context

Terms used across this repository, the BDC KG and its search API. Where a term has a different
meaning nearby (Monarch, the portal middleware, the variable library), the difference is noted.

## Knowledge graph

- **KG** — the BDC knowledge graph: a released monarch-kg with BDC layers appended, built into
  `output/bdc-kg.duckdb` (`nodes`, `edges`, `closure`, `mappings`).
- **KG node** / **entity** — one row of `nodes`. Monarch's API calls these entities; the search
  API's `/v3/api/*` routes return them in Monarch's `SearchResult` shape.
- **Layer** — a BDC source appended to the base KG (`sources.yaml`), e.g. SemMedDB procedures.
- **Restricted source** — a layer whose labels are licensed and must not be redistributed
  (`restricted_label_sources` in `varlib.yaml`). Its nodes stay in the graph and the index;
  only their labels are blanked on export.

## Variable library

- **phs / pht / phv** — dbGaP study / dataset (table) / variable (column) accessions.
- **HV spec** — an RTI transformation spec (`NHLBI-BDC-DMC-HV`, LinkML-Map YAML): one per
  cohort per topic, turning dbGaP columns into BDCHM records.
- **BDCHM** — the BDC Harmonized Model: the classes harmonized records conform to
  (`Condition`, `MeasurementObservation`, `DrugExposure`, ...). A model, not a variable.
- **Concept code** — the CURIE a spec writes into a BDCHM concept slot
  (`condition_concept: MONDO:0005044`). Usually also a KG node.
- **Harmonized concept** — a BDCHM class × concept code, pooled across studies. One row of
  `varlib.concept`; the middleware's `BdcConcept`; what portal users pick.
- **Mapping provenance** — which spec blocks read which phvs (from dm-bip). How a concept finds
  its dbGaP columns when the spec doesn't list them.
- **Library variable** — a variable class of linkml/bdc-variable-library
  (`IntegratedHeight001`, `CompoundAlbuminUrine001`), loaded as a row.

## Search

- **Coverage** — what BDC has harmonized for a KG term: studies and spec files. The `bdc`
  field on a search hit; the middleware's `BdcCoverage`.
- **Static boost** — Monarch's per-node ranking multiplier (disease ×1.3, phenotype ×1.1,
  human gene ×1.1, deprecated ×0.1), computed in `sql/search_entity.sql`.
- **Query multiplier** — a ranking multiplier that depends on the query: exact synonym ×5, or
  blank-search examples.
- **Result window** — how deep offset paging may go (offset + limit ≤ 10,000). Deeper reads use
  **cursor paging**: `cursor=*`, then each page's `next_cursor`.
- **Index version** — identifies one build of the entity index; cursors carry it, so a cursor
  from an earlier build is refused rather than misread.
