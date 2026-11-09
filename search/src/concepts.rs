//! BDC harmonized concepts: one per concept code, as `sql/search_concept.sql` exports them.
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

/// File names inside the index directory, written by `just search-index`.
pub const CONCEPTS_FILE: &str = "concepts.jsonl";
pub const META_FILE: &str = "concepts-meta.json";

/// The middleware drops prefix matches shorter than this, so "hyp" doesn't match everything.
const MIN_PREFIX_LENGTH: usize = 5;
pub const DEFAULT_MAX_MATCHES: usize = 25;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMapping {
    pub study: String,
    pub bdchm_class: String,
    pub concept_slot: String,
    pub source_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Carrier {
    pub variable_name: String,
    pub variable_label: Option<String>,
    pub studies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Concept {
    pub concept_type: String,
    pub concept_label: Option<String>,
    pub bdchm_type: Option<String>,
    pub bdchm_classes: Vec<String>,
    pub source_mappings: Vec<SourceMapping>,
    pub study_count: usize,
    pub studies: Vec<String>,
    pub variables: Vec<Carrier>,
    /// Additions the middleware's parser ignores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kg_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kg_category: Option<String>,
}

impl Concept {
    /// The label, else the CURIE: never null and never a spec file name.
    pub fn display_label(&self) -> &str {
        self.concept_label
            .as_deref()
            .filter(|l| !l.trim().is_empty())
            .unwrap_or(&self.concept_type)
    }
}

/// A concept as the API returns it: the index row plus its display label.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConceptView<'a> {
    #[serde(flatten)]
    pub concept: &'a Concept,
    pub display_label: &'a str,
}

impl<'a> From<&'a Concept> for ConceptView<'a> {
    fn from(concept: &'a Concept) -> Self {
        ConceptView {
            concept,
            display_label: concept.display_label(),
        }
    }
}

/// What BDC has harmonized for one KG term: the middleware's `BdcCoverage`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    pub study_count: usize,
    pub studies: Vec<String>,
    pub variable_names: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Meta {
    revision: Option<String>,
    generated: Option<String>,
}

pub struct Concepts {
    /// In the middleware's search order: most studies first, then by CURIE.
    concepts: Vec<Concept>,
    tokens: Vec<HashSet<String>>,
    by_key: HashMap<String, usize>,
    meta: Meta,
}

/// The middleware's concept key: prefix upper-cased, local part as written.
pub fn concept_key(curie: &str) -> String {
    match curie.split_once(':') {
        Some((prefix, local)) => format!("{}:{}", prefix.to_uppercase(), local),
        None => curie.to_string(),
    }
}

fn is_curie(text: &str) -> bool {
    let Some((prefix, local)) = text.split_once(':') else {
        return false;
    };
    let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    prefix
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && prefix.chars().all(ok)
        && !local.is_empty()
        && local.chars().all(ok)
}

fn tokenize(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() > 1)
        .map(str::to_string)
        .collect()
}

fn prefix_match(a: &str, b: &str) -> bool {
    let (shorter, longer) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    shorter.chars().count() >= MIN_PREFIX_LENGTH && longer.starts_with(shorter)
}

impl Concepts {
    /// Load concepts written beside the entity index, if any were.
    pub fn open(dir: &Path) -> Result<Option<Concepts>> {
        let path = dir.join(CONCEPTS_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let mut concepts = Vec::new();
        for (n, line) in BufReader::new(File::open(&path)?).lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            concepts.push(
                serde_json::from_str::<Concept>(&line)
                    .with_context(|| format!("{} line {}", path.display(), n + 1))?,
            );
        }
        let meta = match std::fs::read_to_string(dir.join(META_FILE)) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Meta::default(),
        };
        Ok(Some(Concepts::new(concepts, meta)))
    }

    fn new(mut concepts: Vec<Concept>, meta: Meta) -> Concepts {
        concepts.sort_by(|a, b| {
            b.study_count
                .cmp(&a.study_count)
                .then_with(|| a.concept_type.cmp(&b.concept_type))
        });
        // The middleware searches variable names and labels; the KG label is added here.
        let tokens = concepts
            .iter()
            .map(|c| {
                let mut t: HashSet<String> = HashSet::new();
                if let Some(label) = &c.concept_label {
                    t.extend(tokenize(label));
                }
                for v in &c.variables {
                    t.extend(tokenize(&v.variable_name));
                    if let Some(label) = &v.variable_label {
                        t.extend(tokenize(label));
                    }
                }
                t
            })
            .collect();
        let by_key = concepts
            .iter()
            .enumerate()
            .map(|(i, c)| (concept_key(&c.concept_type), i))
            .collect();
        Concepts {
            concepts,
            tokens,
            by_key,
            meta,
        }
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

    /// The middleware's search: a CURIE is an exact lookup; otherwise every query token must
    /// equal, or share a prefix of 5+ characters with, some token of the concept.
    pub fn search(&self, query: &str, max: usize) -> Vec<&Concept> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        if is_curie(query) {
            return self.get(query).into_iter().collect();
        }
        let wanted = tokenize(query);
        if wanted.is_empty() {
            return Vec::new();
        }
        self.concepts
            .iter()
            .zip(&self.tokens)
            .filter(|(_, have)| {
                wanted
                    .iter()
                    .all(|q| have.iter().any(|t| q == t || prefix_match(q, t)))
            })
            .map(|(c, _)| c)
            .take(max)
            .collect()
    }

    /// The whole set in the middleware's harmonized-index format.
    pub fn index_document(&self) -> serde_json::Value {
        let labelled = self
            .concepts
            .iter()
            .filter(|c| c.concept_label.is_some())
            .count();
        let typed = self
            .concepts
            .iter()
            .filter(|c| c.bdchm_type.is_some())
            .count();
        let studies: BTreeSet<&str> = self
            .concepts
            .iter()
            .flat_map(|c| c.studies.iter().map(String::as_str))
            .collect();
        serde_json::json!({
            "schemaVersion": 2,
            "generated": self.meta.generated,
            "source": {
                "repository": "RTIInternational/NHLBI-BDC-DMC-HV",
                "ingest": "priority_variables_transform/*-ingest/*.yaml",
                "revision": self.meta.revision,
                "builder": "tis-lab/BDC-VarLib scripts/build_bdc_varlib.py, loaded into the BDC KG duckdb (tis-lab/monarch-bdc-kg)",
                "representation": "concept index served by bdc-search; conceptLabel is the KG label",
            },
            "counts": {
                "studies": studies.len(),
                "concepts": self.concepts.len(),
                "conceptsWithALabel": labelled,
                "conceptsWithABdchmType": typed,
            },
            "warnings": [],
            "concepts": self.concepts.iter().map(ConceptView::from).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn concept(curie: &str, label: Option<&str>, studies: &[&str], vars: &[&str]) -> Concept {
        Concept {
            concept_type: curie.into(),
            concept_label: label.map(Into::into),
            bdchm_type: Some("condition".into()),
            bdchm_classes: vec!["Condition".into()],
            source_mappings: vec![],
            study_count: studies.len(),
            studies: studies.iter().map(|s| s.to_string()).collect(),
            variables: vars
                .iter()
                .map(|v| Carrier {
                    variable_name: v.to_string(),
                    variable_label: None,
                    studies: studies.iter().map(|s| s.to_string()).collect(),
                })
                .collect(),
            kg_node_id: None,
            kg_category: None,
        }
    }

    fn sample() -> Concepts {
        Concepts::new(
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
    }

    #[test]
    fn orders_by_study_count() {
        let concepts = sample();
        let found = concepts.search("hypertension", 25);
        let ids: Vec<_> = found.iter().map(|c| c.concept_type.as_str()).collect();
        // ties in study count fall back to CURIE order, as in the middleware
        assert_eq!(ids, ["MONDO:0005044", "HP:0000822", "MONDO:0005149"]);
    }

    #[test]
    fn short_prefixes_do_not_match() {
        assert!(sample().search("hyp", 25).is_empty());
        assert_eq!(sample().search("hypert", 25).len(), 3);
    }

    #[test]
    fn labels_are_searchable() {
        let ids: Vec<_> = sample()
            .search("pulmonary", 25)
            .iter()
            .map(|c| c.concept_type.clone())
            .collect();
        assert_eq!(ids, ["MONDO:0005149"]);
    }

    #[test]
    fn curies_look_up_case_insensitively_by_prefix() {
        let concepts = sample();
        assert_eq!(concepts.search("mondo:0005044", 25).len(), 1);
        assert_eq!(concepts.coverage("MONDO:0005044").unwrap().study_count, 2);
        assert!(concepts.coverage("MONDO:9999999").is_none());
    }

    #[test]
    fn display_label_falls_back_to_curie() {
        let concepts = sample();
        assert_eq!(
            concepts.get("HP:0000822").unwrap().display_label(),
            "HP:0000822"
        );
    }
}
