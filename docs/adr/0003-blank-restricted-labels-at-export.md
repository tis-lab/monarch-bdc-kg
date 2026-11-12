# 3. Blank restricted labels at export, not at serve time

Status: accepted, 2026-10

## Context

Some KG layers carry labels under a licence that forbids redistribution (the SemMedDB
snomedct tier's SNOMED CT labels). The search API is a redistribution point.

## Decision

`scripts/export_search.py` defines one view, `search_nodes`, over `nodes` with the labels of
restricted sources (`restricted_label_sources` in `varlib.yaml`) blanked. Both search exports
read it, so the index never contains those labels. The nodes themselves are kept: they are
findable by id and keep their KG links.

## Consequences

- A label can't leak from the index, the stored documents, or `harmonized-index.json`, whatever
  the serving code does.
- Adding a restricted source means one line in `varlib.yaml` and a re-export.
- Unlabelled nodes answer with their id as `name`, which Monarch's response shape requires.
