# 3. Blank restricted labels at export, per deployment

Status: accepted, 2026-10; amended 2026-11 (blanking is per deployment, off for the
controlled-access API)

## Context

Some KG layers carry labels under a licence that limits redistribution (the SemMedDB snomedct
tier's SNOMED CT labels). A search API can be a redistribution point, depending on who can
reach it.

The BDC API is served with controlled access: it runs as an internal service behind the portal
middleware, and the portal is behind BDC login. There, blanking would only cost: those nodes
(776 procedures today) couldn't be found by name.

## Decision

Restricted sources are a per-deployment setting, `restricted_label_sources` in `search.yaml`,
empty for the controlled-access API. `scripts/export_search.py` defines one view,
`search_nodes`, over `nodes` with every label and text column (name, full name, symbol,
description and all synonym lists) of the listed sources blanked. Both search exports read it, so the index
never contains that text. Ids, xrefs and categories are kept, so the nodes stay findable by id
and keep their KG links. tests/test_export_search.py checks both exports.

## Consequences

- A label can't leak from the index, the stored documents, or `harmonized-index.json`, whatever
  the serving code does, as long as the text lives in one of `LABEL_COLUMNS`; a new text column
  on `nodes` must be added there.
- A deployment without access control lists its restricted sources in `search.yaml` (or a copy
  passed as `just search_config=... search-index`) and re-exports.
- The controlled-access deployment depends on access control holding at every hop: the API
  stays cluster-internal, the portal stays behind login, and the index directory and any image
  with the index baked in get the same access as the KG duckdb.
- `varlib.yaml` keeps its own list, for a different purpose: concept names in the variable
  library are never taken from restricted labels.
- Unlabelled nodes answer with their id as `name`, which Monarch's response shape requires.
