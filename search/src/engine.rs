//! Building the entity index and answering Monarch's autocomplete query against it.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use tantivy::collector::{Count, DocSetCollector, TopDocs};
use tantivy::index::SegmentId;
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, ConstScoreQuery, DisjunctionMaxQuery, Occur, Query,
    TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption, Value};
use tantivy::{doc, DocId, Index, IndexReader, ReloadPolicy, Score, Term};
use tantivy::{SegmentReader, TantivyDocument};

use crate::analysis::{self, AUTOCOMPLETE_QUERY, TEXT};
use crate::readonly::ReadOnlyDirectory;
use crate::schema::{self, Entity, Fields, Kind};

/// Nodes Monarch boosts for a blank search, as site examples (`blank_search_boost`).
const BLANK_SEARCH_EXAMPLES: [&str; 10] = [
    "MONDO:0007523",
    "MONDO:0019391",
    "MONDO:0018954",
    "MONDO:0011518",
    "HP:0001166",
    "HP:0001631",
    "UBERON:0000948",
    "UBERON:0006585",
    "HGNC:4851",
    "HGNC:3603",
];

/// Monarch's multiplier when the query equals an exact synonym verbatim (`exact_match_boost`).
const EXACT_SYNONYM_BOOST: f64 = 5.0;

fn open_dir(dir: &Path) -> Result<Index> {
    let directory =
        ReadOnlyDirectory::open(dir).with_context(|| format!("opening index {}", dir.display()))?;
    let index = Index::open(directory)?;
    analysis::register(index.tokenizers());
    Ok(index)
}

/// Build an entity index in `dir` from JSONL rows of `sql/search_entity.sql`.
pub fn build_index(input: impl BufRead, dir: &Path, heap_bytes: usize) -> Result<u64> {
    std::fs::create_dir_all(dir)?;
    let index = Index::create_in_dir(dir, schema::build())
        .with_context(|| format!("creating index in {} (must be empty)", dir.display()))?;
    analysis::register(index.tokenizers());
    let f = Fields::new(&index.schema())?;
    let mut writer = index.writer(heap_bytes)?;
    let mut count = 0u64;
    for (n, line) in input.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entity: Entity =
            serde_json::from_str(&line).with_context(|| format!("line {}", n + 1))?;
        writer.add_document(to_document(&f, &entity)?)?;
        count += 1;
        if count % 250_000 == 0 {
            tracing::info!(count, "indexed");
        }
    }
    writer.commit()?;
    writer.wait_merging_threads()?;
    Ok(count)
}

fn to_document(f: &Fields, e: &Entity) -> Result<TantivyDocument> {
    let mut d = doc!(
        f.id => e.id.clone(),
        f.prior => e.prior(),
    );
    let mut add = |fields: &[Field], value: &str| {
        for &field in fields {
            d.add_text(field, value);
        }
    };
    if let Some(v) = &e.category {
        add(&[f.category], v);
    }
    if let Some(v) = &e.name {
        add(&[f.name, f.name_t, f.name_ac], v);
    }
    if let Some(v) = &e.symbol {
        add(&[f.symbol, f.symbol_t, f.symbol_ac], v);
    }
    for v in e.synonym.iter().flatten() {
        add(&[f.synonym, f.synonym_t, f.synonym_ac], v);
    }
    for v in e.exact_synonym.iter().flatten() {
        add(&[f.exact_synonym], v);
    }
    if let Some(v) = &e.in_taxon_label {
        add(&[f.in_taxon_label_t], v);
    }
    if let Some(v) = &e.description {
        add(&[f.description_t], v);
    }
    for v in e.xref.iter().flatten() {
        add(&[f.xref], v);
    }
    d.add_text(f.doc, serde_json::to_string(e)?);
    Ok(d)
}

/// One autocomplete or search hit: the stored entity in Monarch's SearchResult shape.
#[derive(Debug, Serialize)]
pub struct SearchResult {
    #[serde(flatten)]
    pub entity: Entity,
    pub score: f64,
}

/// Monarch's SearchResults envelope.
#[derive(Debug, Serialize)]
pub struct SearchResults {
    pub limit: usize,
    pub offset: usize,
    pub total: usize,
    pub items: Vec<SearchResult>,
}

pub struct Engine {
    reader: IndexReader,
    fields: Fields,
    index: Index,
}

impl Engine {
    pub fn open(dir: &Path) -> Result<Engine> {
        let index = open_dir(dir)?;
        let fields = Fields::new(&index.schema())?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(Engine {
            reader,
            fields,
            index,
        })
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// Monarch's `/autocomplete`: the top 10 for `q`, optionally restricted to categories.
    pub fn autocomplete(&self, q: &str, categories: &[String]) -> Result<SearchResults> {
        self.search(q, categories, 10, 0)
    }

    pub fn search(
        &self,
        q: &str,
        categories: &[String],
        limit: usize,
        offset: usize,
    ) -> Result<SearchResults> {
        let q = q.trim();
        let blank = q.is_empty() || q == "*:*";
        let searcher = self.reader.searcher();

        let text_query: Box<dyn Query> = if blank {
            Box::new(AllQuery)
        } else {
            self.text_query(q)
        };
        let query = match self.category_filter(categories) {
            Some(filter) => Box::new(BooleanQuery::new(vec![
                (Occur::Must, text_query),
                (Occur::Must, filter),
            ])) as Box<dyn Query>,
            None => text_query,
        };

        // Query-dependent multipliers, keyed by segment and doc.
        let mut boosts: HashMap<(SegmentId, DocId), f64> = HashMap::new();
        let mut boost_docs = |term_query: TermQuery, multiplier: f64| -> Result<()> {
            for addr in searcher.search(&term_query, &DocSetCollector)? {
                let segment = searcher.segment_reader(addr.segment_ord).segment_id();
                *boosts.entry((segment, addr.doc_id)).or_insert(1.0) *= multiplier;
            }
            Ok(())
        };
        if blank {
            let n = BLANK_SEARCH_EXAMPLES.len();
            for (i, id) in BLANK_SEARCH_EXAMPLES.iter().enumerate() {
                let term = Term::from_field_text(self.fields.id, id);
                boost_docs(
                    TermQuery::new(term, IndexRecordOption::Basic),
                    (n - i + 2) as f64,
                )?;
            }
        } else {
            let term = Term::from_field_text(self.fields.exact_synonym, q);
            boost_docs(
                TermQuery::new(term, IndexRecordOption::Basic),
                EXACT_SYNONYM_BOOST,
            )?;
        }

        let prior_field = "prior";
        let top = TopDocs::with_limit(limit.max(1))
            .and_offset(offset)
            .tweak_score(move |segment: &SegmentReader| {
                let prior = segment
                    .fast_fields()
                    .f64(prior_field)
                    .expect("prior is a fast field");
                let segment_id = segment.segment_id();
                let boosts: HashMap<DocId, f64> = boosts
                    .iter()
                    .filter(|((s, _), _)| *s == segment_id)
                    .map(|((_, d), m)| (*d, *m))
                    .collect();
                move |doc: DocId, score: Score| {
                    let base = if blank { 1.0 } else { score as f64 };
                    base * prior.first(doc).unwrap_or(1.0)
                        * boosts.get(&doc).copied().unwrap_or(1.0)
                }
            });
        let (hits, total) = searcher.search(&query, &(top, Count))?;

        let mut items = Vec::with_capacity(hits.len());
        if limit > 0 {
            for (score, addr) in hits {
                let stored: TantivyDocument = searcher.doc(addr)?;
                let json = stored
                    .get_first(self.fields.doc)
                    .and_then(|v| v.as_str())
                    .context("stored doc missing")?;
                let mut entity: Entity = serde_json::from_str(json)?;
                // Monarch requires a name; restricted and unlabelled nodes fall back to the id.
                entity.name.get_or_insert_with(|| entity.id.clone());
                items.push(SearchResult { entity, score });
            }
        }
        Ok(SearchResults {
            limit,
            offset,
            total,
            items,
        })
    }

    /// Monarch's edismax query: every whitespace-separated token must match (q.op=AND, mm=100%),
    /// each scored by its best field (dismax, tie 0), with per-field boosts from qf.
    fn text_query(&self, q: &str) -> Box<dyn Query> {
        let tokenizers = self.index.tokenizers();
        let clauses: Vec<(Occur, Box<dyn Query>)> = q
            .split_whitespace()
            .map(|token| {
                let disjuncts: Vec<Box<dyn Query>> = self
                    .fields
                    .query_fields()
                    .into_iter()
                    .filter_map(|(field, kind, boost)| {
                        let terms = match kind {
                            Kind::Raw => vec![token.to_string()],
                            Kind::Text => analysis::terms(tokenizers, TEXT, token),
                            Kind::Autocomplete => {
                                analysis::terms(tokenizers, AUTOCOMPLETE_QUERY, token)
                            }
                        };
                        field_query(field, &terms)
                            .map(|q| Box::new(BoostQuery::new(q, boost)) as Box<dyn Query>)
                    })
                    .collect();
                (
                    Occur::Must,
                    Box::new(DisjunctionMaxQuery::with_tie_breaker(disjuncts, 0.0))
                        as Box<dyn Query>,
                )
            })
            .collect();
        Box::new(BooleanQuery::new(clauses))
    }

    fn category_filter(&self, categories: &[String]) -> Option<Box<dyn Query>> {
        if categories.is_empty() {
            return None;
        }
        let any: Vec<(Occur, Box<dyn Query>)> = categories
            .iter()
            .map(|c| {
                let term = Term::from_field_text(self.fields.category, c);
                (
                    Occur::Should,
                    Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
                )
            })
            .collect();
        Some(Box::new(ConstScoreQuery::new(
            Box::new(BooleanQuery::new(any)),
            0.0,
        )))
    }
}

/// A field's query for one user token: its single term, or all of them when the analyzer
/// splits the token (e.g. `x-linked`).
fn field_query(field: Field, terms: &[String]) -> Option<Box<dyn Query>> {
    let term_query = |t: &String| -> Box<dyn Query> {
        Box::new(TermQuery::new(
            Term::from_field_text(field, t),
            IndexRecordOption::WithFreqs,
        ))
    };
    match terms {
        [] => None,
        [one] => Some(term_query(one)),
        many => Some(Box::new(BooleanQuery::intersection(
            many.iter().map(term_query).collect(),
        ))),
    }
}
