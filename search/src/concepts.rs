//! BDC harmonized concepts: one per concept code, as sql/search_concept.sql exports them.
//!
//! Rows follow the harmonized index of tis-lab/bdc-dp-middleware (schemaVersion 2), so the
//! middleware can load `/bdc/harmonized-index.json` in place of its bundled file, and the
//! search and coverage rules here match its `HarmonizedVariableLibrary`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use axum::body::Bytes;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::parquet_table;

/// File names inside the index directory, written by scripts/export_search.py.
/// Written by scripts/export_search.py; its key/value metadata holds the HV `revision` and the
/// `generated` date.
pub const CONCEPTS_FILE: &str = "concept.parquet";

/// The middleware ignores prefix matches shorter than this, so "hyp" doesn't match everything.
const MIN_PREFIX_LENGTH: usize = 5;
pub const DEFAULT_MAX_MATCHES: usize = 25;

/// One study's spec assigning the concept: the middleware's `SourceMapping`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceMapping {
    pub study: String,
    pub bdchm_class: String,
    pub concept_slot: String,
    pub source_file: String,
}

/// A carrier: a spec file (by name) that assigns the concept, and in which studies. The
/// middleware's `BdcCarrier`, which its index calls a "variable" (`variableName` = file stem).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Carrier {
    pub variable_name: String,
    pub variable_label: Option<String>,
    pub studies: Vec<String>,
}

/// One harmonized concept. Field names on the wire are the middleware's (`conceptType`, ...).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Concept {
    /// The concept code, e.g. MONDO:0005044.
    #[serde(rename = "conceptType")]
    pub curie: String,
    pub concept_label: Option<String>,
    /// The label, else the CURIE: never null and never a file name. Computed in
    /// sql/search_concept.sql.
    pub display_label: String,
    pub bdchm_type: Option<String>,
    pub bdchm_classes: Vec<String>,
    pub source_mappings: Vec<SourceMapping>,
    pub study_count: usize,
    pub studies: Vec<String>,
    /// The carriers; "variables" in the middleware's format.
    pub variables: Vec<Carrier>,
    /// Additions the middleware's parser ignores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kg_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kg_category: Option<String>,
    /// The concept code, the KG nodes it is or maps to, and every superclass of those: the
    /// terms the concept is under. Sorted. Computed in sql/search_concept.sql.
    #[serde(default)]
    pub closure: Vec<String>,
    /// The labels of `closure`'s terms, where the KG has one. Sorted.
    #[serde(default)]
    pub closure_label: Vec<String>,
}

/// What BDC has harmonized for one KG term: the middleware's `BdcCoverage`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    pub study_count: usize,
    pub studies: Vec<String>,
    pub variable_names: Vec<String>,
}

/// The HV commit and date the concepts were built from (concept.parquet's metadata).
#[derive(Debug, Default)]
struct Meta {
    revision: Option<String>,
    generated: Option<String>,
}

/// Concepts matching a search: how many in all, and the first `limit` of them.
pub struct ConceptMatches<'a> {
    pub total: usize,
    pub items: Vec<&'a Concept>,
}

pub struct HarmonizedConcepts {
    /// In the middleware's search order: most studies first, then by CURIE.
    concepts: Vec<Concept>,
    /// Each concept's searchable words, parallel to `concepts`.
    words: Vec<HashSet<String>>,
    /// The words of each concept's closure labels, parallel to `concepts`.
    closure_words: Vec<HashSet<String>>,
    by_key: HashMap<String, usize>,
    /// Each term in some concept's closure (by concept key), to those concepts, in search order.
    by_ancestor: HashMap<String, Vec<usize>>,
    /// `/bdc/harmonized-index.json`, built once since the concepts never change while served.
    index_json: Bytes,
}

/// The middleware's concept key: prefix upper-cased, local part as written.
/// Keep in step with `concept_key` in sql/search_concept.sql.
pub fn concept_key(curie: &str) -> String {
    match curie.split_once(':') {
        Some((prefix, local)) => format!("{}:{}", prefix.to_uppercase(), local),
        None => curie.to_string(),
    }
}

/// The middleware's CURIE pattern, `[A-Za-z][A-Za-z0-9._-]*:[A-Za-z0-9._-]+`.
/// Keep in step with the WHERE clause in sql/search_concept.sql.
fn is_curie(text: &str) -> bool {
    let Some((prefix, local)) = text.split_once(':') else {
        return false;
    };
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    prefix.starts_with(|c: char| c.is_ascii_alphabetic())
        && prefix.chars().all(allowed)
        && !local.is_empty()
        && local.chars().all(allowed)
}

/// Lower-cased words of two or more characters, as the middleware splits text.
fn words(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 1)
        .map(str::to_string)
        .collect()
}

/// The middleware's word match: equal, or one is a prefix of the other and 5+ characters long.
fn word_matches(a: &str, b: &str) -> bool {
    let (shorter, longer) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    a == b || (shorter.chars().count() >= MIN_PREFIX_LENGTH && longer.starts_with(shorter))
}

impl HarmonizedConcepts {
    /// Load the concepts written beside the entity index, if any were.
    pub fn open(dir: &Path) -> Result<Option<HarmonizedConcepts>> {
        let path = dir.join(CONCEPTS_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let table = parquet_table::read::<Concept>(&path)?;
        let meta = Meta {
            revision: table.metadata.get("revision").cloned(),
            generated: table.metadata.get("generated").cloned(),
        };
        if meta.revision.is_none() {
            tracing::warn!(path = %path.display(), "no HV revision in the concept metadata; harmonized-index.json will have none");
        }
        Ok(Some(HarmonizedConcepts::new(table.rows, meta)?))
    }

    fn new(mut concepts: Vec<Concept>, meta: Meta) -> Result<HarmonizedConcepts> {
        concepts.sort_by(|a, b| {
            b.study_count
                .cmp(&a.study_count)
                .then_with(|| a.curie.cmp(&b.curie))
        });
        let closure_words = concepts
            .iter()
            .map(|c| c.closure_label.iter().flat_map(|l| words(l)).collect())
            .collect();
        // The middleware searches spec file names and their labels; the KG label is added here.
        let words = concepts
            .iter()
            .map(|c| {
                let mut found = HashSet::new();
                found.extend(c.concept_label.iter().flat_map(|l| words(l)));
                for v in &c.variables {
                    found.extend(words(&v.variable_name));
                    found.extend(v.variable_label.iter().flat_map(|l| words(l)));
                }
                found
            })
            .collect();
        let by_key = concepts
            .iter()
            .enumerate()
            .map(|(i, c)| (concept_key(&c.curie), i))
            .collect();
        let mut by_ancestor: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, c) in concepts.iter().enumerate() {
            for ancestor in &c.closure {
                by_ancestor
                    .entry(concept_key(ancestor))
                    .or_default()
                    .push(i);
            }
        }
        let index_json = Bytes::from(serde_json::to_vec(&index_document(&concepts, &meta))?);
        Ok(HarmonizedConcepts {
            concepts,
            words,
            closure_words,
            by_key,
            by_ancestor,
            index_json,
        })
    }

    pub fn len(&self) -> usize {
        self.concepts.len()
    }

    pub fn get(&self, curie: &str) -> Option<&Concept> {
        self.by_key
            .get(&concept_key(curie.trim()))
            .map(|&i| &self.concepts[i])
    }

    pub fn coverage(&self, curie: &str) -> Option<Coverage> {
        self.get(curie).map(|c| Coverage {
            study_count: c.study_count,
            studies: c.studies.clone(),
            variable_names: c
                .variables
                .iter()
                .map(|v| v.variable_name.clone())
                .collect(),
        })
    }

    /// The middleware's concept search: a CURIE is an exact lookup; otherwise every query word
    /// must match some word of the concept (see `word_matches`), most studies first. After
    /// those come concepts that match only once their closure labels count too: "cardiovascular"
    /// finds hypertension through its superclass "cardiovascular disorder". With a `term`, only
    /// concepts under it (the term is in their closure); without a query, all of those.
    pub fn search(
        &self,
        query: Option<&str>,
        term: Option<&str>,
        limit: usize,
    ) -> ConceptMatches<'_> {
        let under: Option<HashSet<usize>> = term.map(|t| {
            let key = concept_key(t.trim());
            self.by_ancestor
                .get(&key)
                .into_iter()
                .flatten()
                .copied()
                .collect()
        });
        let in_scope = |i: &usize| under.as_ref().is_none_or(|u| u.contains(i));
        let matching: Vec<usize> = match query.map(str::trim) {
            Some(q) if is_curie(q) => {
                let found = self.by_key.get(&concept_key(q)).copied();
                found.filter(in_scope).into_iter().collect()
            }
            Some(q) => {
                let wanted = words(q);
                // every query word matches a word in one of the sets
                let all_match = |sets: &[&HashSet<String>]| {
                    !wanted.is_empty()
                        && wanted.iter().all(|q| {
                            sets.iter()
                                .flat_map(|s| s.iter())
                                .any(|w| word_matches(q, w))
                        })
                };
                let (direct, via_closure): (Vec<usize>, Vec<usize>) = (0..self.concepts.len())
                    .filter(in_scope)
                    .filter(|&i| all_match(&[&self.words[i], &self.closure_words[i]]))
                    .partition(|&i| all_match(&[&self.words[i]]));
                direct.into_iter().chain(via_closure).collect()
            }
            None => (0..self.concepts.len()).filter(in_scope).collect(),
        };
        ConceptMatches {
            total: matching.len(),
            items: matching
                .into_iter()
                .take(limit)
                .map(|i| &self.concepts[i])
                .collect(),
        }
    }

    /// The whole set in the middleware's harmonized-index format, as JSON bytes.
    pub fn index_json(&self) -> Bytes {
        self.index_json.clone()
    }
}

fn index_document(concepts: &[Concept], meta: &Meta) -> serde_json::Value {
    let studies: BTreeSet<&str> = concepts
        .iter()
        .flat_map(|c| c.studies.iter().map(String::as_str))
        .collect();
    serde_json::json!({
        "schemaVersion": 2,
        "generated": meta.generated,
        "source": {
            "repository": "RTIInternational/NHLBI-BDC-DMC-HV",
            "ingest": "priority_variables_transform/*-ingest/*.yaml",
            "revision": meta.revision,
            "builder": "tis-lab/BDC-VarLib scripts/build_bdc_varlib.py, loaded into the BDC KG duckdb (tis-lab/monarch-bdc-kg)",
            "representation": "concept index served by bdc-search; conceptLabel is the KG label",
        },
        "counts": {
            "studies": studies.len(),
            "concepts": concepts.len(),
            "conceptsWithALabel": concepts.iter().filter(|c| c.concept_label.is_some()).count(),
            "conceptsWithABdchmType": concepts.iter().filter(|c| c.bdchm_type.is_some()).count(),
        },
        "warnings": [],
        "concepts": concepts,
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn concept(curie: &str, label: Option<&str>, studies: &[&str], files: &[&str]) -> Concept {
        let studies: Vec<String> = studies.iter().map(|s| s.to_string()).collect();
        Concept {
            curie: curie.into(),
            concept_label: label.map(Into::into),
            display_label: label.unwrap_or(curie).into(),
            bdchm_type: Some("condition".into()),
            bdchm_classes: vec!["Condition".into()],
            source_mappings: vec![],
            study_count: studies.len(),
            studies: studies.clone(),
            variables: files
                .iter()
                .map(|f| Carrier {
                    variable_name: f.to_string(),
                    variable_label: None,
                    studies: studies.clone(),
                })
                .collect(),
            kg_node_id: None,
            kg_category: None,
            closure: vec![curie.into()],
            closure_label: label.map(Into::into).into_iter().collect(),
        }
    }

    fn under(mut c: Concept, ancestors: &[(&str, &str)]) -> Concept {
        c.closure
            .extend(ancestors.iter().map(|(id, _)| id.to_string()));
        c.closure.sort();
        c.closure_label
            .extend(ancestors.iter().map(|(_, label)| label.to_string()));
        c.closure_label.sort();
        c
    }

    pub fn sample() -> HarmonizedConcepts {
        HarmonizedConcepts::new(
            vec![
                under(
                    concept(
                        "MONDO:0005149",
                        Some("pulmonary hypertension"),
                        &["FHS"],
                        &["pulm_htn"],
                    ),
                    &[
                        ("MONDO:0005044", "hypertensive disorder"),
                        ("MONDO:0000001", "disease"),
                    ],
                ),
                under(
                    concept(
                        "MONDO:0005044",
                        Some("hypertensive disorder"),
                        &["ARIC", "JHS"],
                        &["hyperten"],
                    ),
                    &[
                        ("MONDO:0004995", "cardiovascular disorder"),
                        ("MONDO:0000001", "disease"),
                    ],
                ),
                concept("HP:0000822", None, &["FHS"], &["hypert_trt"]),
            ],
            Meta::default(),
        )
        .unwrap()
    }

    fn curies(m: &ConceptMatches) -> Vec<String> {
        m.items.iter().map(|c| c.curie.clone()).collect()
    }

    #[test]
    fn orders_by_study_count_then_curie() {
        let concepts = sample();
        assert_eq!(
            curies(&concepts.search(Some("hypertension"), None, 25)),
            ["MONDO:0005044", "HP:0000822", "MONDO:0005149"]
        );
    }

    #[test]
    fn total_counts_all_matches_not_the_page() {
        let concepts = sample();
        let page = concepts.search(Some("hypertension"), None, 1);
        assert_eq!((page.total, page.items.len()), (3, 1));
    }

    #[test]
    fn short_prefixes_do_not_match() {
        assert_eq!(sample().search(Some("hyp"), None, 25).total, 0);
        assert_eq!(sample().search(Some("hypert"), None, 25).total, 3);
    }

    #[test]
    fn labels_are_searchable() {
        assert_eq!(
            curies(&sample().search(Some("pulmonary"), None, 25)),
            ["MONDO:0005149"]
        );
    }

    #[test]
    fn curies_look_up_case_insensitively_by_prefix() {
        let concepts = sample();
        assert_eq!(concepts.search(Some("mondo:0005044"), None, 25).total, 1);
        assert_eq!(concepts.coverage("MONDO:0005044").unwrap().study_count, 2);
        assert!(concepts.coverage("MONDO:9999999").is_none());
    }

    #[test]
    fn a_term_finds_every_concept_under_it_including_itself() {
        let concepts = sample();
        let under = |term, query| curies(&concepts.search(query, Some(term), 25));
        assert_eq!(
            under("MONDO:0005044", None),
            ["MONDO:0005044", "MONDO:0005149"]
        );
        assert_eq!(
            under("mondo:0000001", None),
            ["MONDO:0005044", "MONDO:0005149"]
        );
        assert_eq!(under("MONDO:0005044", Some("pulmonary")), ["MONDO:0005149"]);
        assert_eq!(
            under("MONDO:0005044", Some("HP:0000822")),
            Vec::<String>::new()
        );
        assert_eq!(under("MONDO:9999999", None), Vec::<String>::new());
    }

    #[test]
    fn closure_labels_match_after_the_concepts_own_words() {
        let concepts = sample();
        let found = |q| curies(&concepts.search(Some(q), None, 25));
        assert_eq!(found("cardiovascular"), ["MONDO:0005044"]);
        assert_eq!(
            found("hypertensive"),
            ["MONDO:0005044", "HP:0000822", "MONDO:0005149"],
            "own label and file name (hypert_trt) first, then pulmonary hypertension through its superclass"
        );
        assert_eq!(
            found("pulmonary hypertensive"),
            ["MONDO:0005149"],
            "words may come from the label and the closure together"
        );
    }

    #[test]
    fn opens_the_exported_parquet_with_its_nested_fields_and_hv_revision() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
        let concepts = HarmonizedConcepts::open(&dir).unwrap().unwrap();
        assert_eq!(concepts.len(), 2);
        let c = concepts.get("MONDO:0005044").unwrap();
        assert_eq!(c.source_mappings[0].concept_slot, "condition_concept");
        assert_eq!(c.variables[0].studies, ["ARIC", "JHS"]);
        assert_eq!(c.variables[0].variable_label, None);
        assert_eq!(c.closure_label, ["disease", "hypertensive disorder"]);
        assert_eq!(concepts.get("OMOP:1").unwrap().concept_label, None);
        let doc: serde_json::Value = serde_json::from_slice(&concepts.index_json()).unwrap();
        assert_eq!(doc["source"]["revision"], "3260ff3");
        assert_eq!(doc["generated"], "2026-11-12");
    }

    #[test]
    fn index_json_uses_the_middleware_field_names() {
        let doc: serde_json::Value = serde_json::from_slice(&sample().index_json()).unwrap();
        assert_eq!(doc["schemaVersion"], 2);
        assert_eq!(doc["concepts"][0]["conceptType"], "MONDO:0005044");
        assert_eq!(doc["concepts"][0]["displayLabel"], "hypertensive disorder");
    }
}
