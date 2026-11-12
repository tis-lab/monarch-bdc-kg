//! BDC harmonized concepts: one per concept code, as sql/search_concept.sql exports them.
//!
//! Rows follow the harmonized index of tis-lab/bdc-dp-middleware (schemaVersion 2), so the
//! middleware can load `/bdc/harmonized-index.json` in place of its bundled file, and the
//! search and coverage rules here match its `HarmonizedVariableLibrary`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// File names inside the index directory, written by scripts/export_search.py.
pub const CONCEPTS_FILE: &str = "concepts.jsonl";
pub const META_FILE: &str = "concepts-meta.json";

/// The middleware ignores prefix matches shorter than this, so "hyp" doesn't match everything.
const MIN_PREFIX_LENGTH: usize = 5;
pub const DEFAULT_MAX_MATCHES: usize = 25;

/// One study's spec assigning the concept: the middleware's `SourceMapping`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMapping {
    pub study: String,
    pub bdchm_class: String,
    pub concept_slot: String,
    pub source_file: String,
}

/// A spec file (by name) that assigns the concept, and in which studies: the middleware's
/// `BdcCarrier`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConceptVariable {
    pub variable_name: String,
    pub variable_label: Option<String>,
    pub studies: Vec<String>,
}

/// One harmonized concept. Field names on the wire are the middleware's (`conceptType`, ...).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Concept {
    /// The concept code, e.g. MONDO:0005044.
    #[serde(rename = "conceptType")]
    pub curie: String,
    pub concept_label: Option<String>,
    /// The label, else the CURIE (computed in the SQL): never null and never a file name.
    #[serde(default)]
    pub display_label: String,
    pub bdchm_type: Option<String>,
    pub bdchm_classes: Vec<String>,
    pub source_mappings: Vec<SourceMapping>,
    pub study_count: usize,
    pub studies: Vec<String>,
    pub variables: Vec<ConceptVariable>,
    /// Additions the middleware's parser ignores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kg_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kg_category: Option<String>,
}

/// What BDC has harmonized for one KG term: the middleware's `BdcCoverage`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    pub study_count: usize,
    pub studies: Vec<String>,
    pub variable_names: Vec<String>,
}

/// The HV commit and date the concepts were built from (concepts-meta.json).
#[derive(Debug, Default, Deserialize)]
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
    by_key: HashMap<String, usize>,
    /// `/bdc/harmonized-index.json`, built once since the concepts never change while served.
    index_json: Vec<u8>,
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
        let mut concepts = Vec::new();
        for (n, line) in BufReader::new(File::open(&path)?).lines().enumerate() {
            let line = line?;
            if !line.trim().is_empty() {
                let concept = serde_json::from_str(&line)
                    .with_context(|| format!("{} line {}", path.display(), n + 1))?;
                concepts.push(concept);
            }
        }
        let meta = read_meta(&dir.join(META_FILE));
        Ok(Some(HarmonizedConcepts::new(concepts, meta)?))
    }

    fn new(mut concepts: Vec<Concept>, meta: Meta) -> Result<HarmonizedConcepts> {
        for c in &mut concepts {
            if c.display_label.trim().is_empty() {
                c.display_label = c.concept_label.clone().unwrap_or_else(|| c.curie.clone());
            }
        }
        concepts.sort_by(|a, b| {
            b.study_count
                .cmp(&a.study_count)
                .then_with(|| a.curie.cmp(&b.curie))
        });
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
        let index_json = serde_json::to_vec(&index_document(&concepts, &meta))?;
        Ok(HarmonizedConcepts {
            concepts,
            words,
            by_key,
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
    /// must match some word of the concept (see `word_matches`). Most studies first.
    pub fn search(&self, query: &str, limit: usize) -> ConceptMatches<'_> {
        let query = query.trim();
        if is_curie(query) {
            let items: Vec<_> = self.get(query).into_iter().collect();
            return ConceptMatches {
                total: items.len(),
                items,
            };
        }
        let wanted = words(query);
        if wanted.is_empty() {
            return ConceptMatches {
                total: 0,
                items: Vec::new(),
            };
        }
        let matching: Vec<&Concept> = self
            .concepts
            .iter()
            .zip(&self.words)
            .filter(|(_, have)| {
                wanted
                    .iter()
                    .all(|q| have.iter().any(|w| word_matches(q, w)))
            })
            .map(|(c, _)| c)
            .collect();
        ConceptMatches {
            total: matching.len(),
            items: matching.into_iter().take(limit).collect(),
        }
    }

    /// The whole set in the middleware's harmonized-index format, as JSON bytes.
    pub fn index_json(&self) -> &[u8] {
        &self.index_json
    }
}

fn read_meta(path: &Path) -> Meta {
    let parsed = std::fs::read_to_string(path)
        .map_err(anyhow::Error::from)
        .and_then(|text| Ok(serde_json::from_str::<Meta>(&text)?));
    match parsed {
        Ok(meta) => meta,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "no concept metadata; harmonized-index.json will have no revision");
            Meta::default()
        }
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
            display_label: String::new(),
            bdchm_type: Some("condition".into()),
            bdchm_classes: vec!["Condition".into()],
            source_mappings: vec![],
            study_count: studies.len(),
            studies: studies.clone(),
            variables: files
                .iter()
                .map(|f| ConceptVariable {
                    variable_name: f.to_string(),
                    variable_label: None,
                    studies: studies.clone(),
                })
                .collect(),
            kg_node_id: None,
            kg_category: None,
        }
    }

    pub fn sample() -> HarmonizedConcepts {
        HarmonizedConcepts::new(
            vec![
                concept(
                    "MONDO:0005149",
                    Some("pulmonary hypertension"),
                    &["FHS"],
                    &["pulm_htn"],
                ),
                concept(
                    "MONDO:0005044",
                    Some("hypertensive disorder"),
                    &["ARIC", "JHS"],
                    &["hyperten"],
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
            curies(&concepts.search("hypertension", 25)),
            ["MONDO:0005044", "HP:0000822", "MONDO:0005149"]
        );
    }

    #[test]
    fn total_counts_all_matches_not_the_page() {
        let concepts = sample();
        let page = concepts.search("hypertension", 1);
        assert_eq!((page.total, page.items.len()), (3, 1));
    }

    #[test]
    fn short_prefixes_do_not_match() {
        assert_eq!(sample().search("hyp", 25).total, 0);
        assert_eq!(sample().search("hypert", 25).total, 3);
    }

    #[test]
    fn labels_are_searchable() {
        assert_eq!(curies(&sample().search("pulmonary", 25)), ["MONDO:0005149"]);
    }

    #[test]
    fn curies_look_up_case_insensitively_by_prefix() {
        let concepts = sample();
        assert_eq!(concepts.search("mondo:0005044", 25).total, 1);
        assert_eq!(concepts.coverage("MONDO:0005044").unwrap().study_count, 2);
        assert!(concepts.coverage("MONDO:9999999").is_none());
    }

    #[test]
    fn display_label_falls_back_to_curie() {
        assert_eq!(
            sample().get("HP:0000822").unwrap().display_label,
            "HP:0000822"
        );
    }

    #[test]
    fn index_json_uses_the_middleware_field_names() {
        let doc: serde_json::Value = serde_json::from_slice(sample().index_json()).unwrap();
        assert_eq!(doc["schemaVersion"], 2);
        assert_eq!(doc["concepts"][0]["conceptType"], "MONDO:0005044");
        assert_eq!(doc["concepts"][0]["displayLabel"], "hypertensive disorder");
    }
}
