# 3. Blank restricted labels at export, not at serve time

Status: accepted, 2026-10

## Context

Some KG layers carry labels under a licence that forbids redistribution (the SemMedDB
snomedct tier's SNOMED CT labels). The search API is a redistribution point.

## Decision

`scripts/export_search.py` defines one view, `search_nodes`, over `nodes` with every label and
text column (name, full name, symbol, description and all synonym lists) of restricted sources
(`restricted_label_sources` in `varlib.yaml`) blanked. Both search exports read it, so the index
never contains that text. Ids, xrefs and categories are kept, so the nodes stay findable by id
and keep their KG links. tests/test_export_search.py checks both exports.

## Consequences

- A label can't leak from the index, the stored documents, or `harmonized-index.json`, whatever
  the serving code does, as long as the text lives in one of `LABEL_COLUMNS`; a new text column
  on `nodes` must be added there.
- Adding a restricted source means one line in `varlib.yaml` and a re-export.
- Unlabelled nodes answer with their id as `name`, which Monarch's response shape requires.
