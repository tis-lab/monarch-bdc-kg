//! The KG entity index: building it from sql/search_entity.sql rows, and answering Monarch's
//! search, autocomplete and entity lookups against it.
//!
//! A search is three steps:
//!   1. `text_query`: which documents match, and their text score (Monarch's edismax).
//!   2. `query_multipliers`: per-document multipliers that depend on the query (exact synonym,
//!      blank-search examples).
//!   3. `rank`: text score x static boost x multipliers, then the requested page.

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
use tantivy::{doc, DocAddress, DocId, Index, IndexReader, ReloadPolicy, Score, Searcher, Term};
use tantivy::{SegmentReader, TantivyDocument};

use crate::analysis::{self, AUTOCOMPLETE_QUERY, TEXT};
use crate::concepts::Coverage;
use crate::monarch;
use crate::readonly::ReadOnlyDirectory;
use crate::schema::{self, Entity, Fields, SolrFieldType};

/// tantivy queries are built from boxed parts; this names the type.
type BoxQuery = Box<dyn Query>;

/// Build an entity index in `dir` from JSONL rows of sql/search_entity.sql.
pub fn build_index(input: impl BufRead, dir: &Path, heap_bytes: usize) -> Result<u64> {
    std::fs::create_dir_all(dir)?;
    let index = Index::create_in_dir(dir, schema::build())
        .with_context(|| format!("creating index in {} (must be empty)", dir.display()))?;
    let entities = input
        .lines()
        .enumerate()
        .filter_map(|(n, line)| match line {
            Ok(line) if line.trim().is_empty() => None,
            Ok(line) => {
                Some(serde_json::from_str(&line).with_context(|| format!("line {}", n + 1)))
            }
            Err(e) => Some(Err(e.into())),
        });
    write_entities(&index, entities, heap_bytes)
}

/// Add entities to an empty index and commit.
pub fn write_entities(
    index: &Index,
    entities: impl Iterator<Item = Result<Entity>>,
    heap_bytes: usize,
) -> Result<u64> {
    analysis::register(index.tokenizers());
    let fields = Fields::new(&index.schema())?;
    let mut writer = index.writer(heap_bytes)?;
    let mut count = 0u64;
    for entity in entities {
        writer.add_document(to_document(&fields, &entity?)?)?;
        count += 1;
        if count % 250_000 == 0 {
            tracing::info!(count, "indexed");
        }
    }
    writer.commit()?;
    writer.wait_merging_threads()?;
    Ok(count)
}

/// One entity as an index document: each source column into its index fields (see schema.rs).
fn to_document(f: &Fields, e: &Entity) -> Result<TantivyDocument> {
    let mut d = doc!(f.id => e.id.clone(), f.static_boost => e.static_boost);
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
    d.add_text(f.stored_entity, serde_json::to_string(e)?);
    Ok(d)
}

/// One hit: the stored entity in Monarch's SearchResult shape.
#[derive(Debug, Serialize)]
pub struct SearchResult {
    #[serde(flatten)]
    pub entity: Entity,
    pub score: f64,
    /// What BDC has harmonized for this exact term, when anything; Monarch has no such field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bdc: Option<Coverage>,
}

/// Monarch's SearchResults envelope, plus `next_cursor` for cursor paging (Monarch clients
/// ignore it).
#[derive(Debug, Serialize)]
pub struct SearchResults {
    pub limit: usize,
    pub offset: usize,
    pub total: usize,
    pub items: Vec<SearchResult>,
    /// Pass back as `cursor` for the hits after this page; absent on the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Which page of the ranked hits to return.
#[derive(Debug, Clone, PartialEq)]
pub enum Page {
    /// Skip this many hits (Monarch's `offset`).
    Offset(usize),
    /// Start at the top (`cursor=*`), or after a hit a previous page ended on.
    Cursor(Option<Cursor>),
}

/// Where a page ended: a hit's place in the ranking, for an index that never changes while
/// served. Encoded for clients as `<index version>.<score>.<position>` in hex.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursor {
    index_version: u64,
    rank: RankKey,
}

/// A hit's place in the ranking: higher score first, then lower position (segment, doc) first,
/// so every hit has its own place and pages never overlap or skip.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
struct RankKey {
    score: f64,
    tie_break: u64, // u64::MAX - position, so that a lower position sorts higher
}

impl RankKey {
    fn new(score: f64, segment_ord: u32, doc: DocId) -> RankKey {
        let position = ((segment_ord as u64) << 32) | doc as u64;
        RankKey {
            score,
            tie_break: u64::MAX - position,
        }
    }

    /// Sorts below every real hit; marks hits a cursor has already passed.
    const PASSED: RankKey = RankKey {
        score: f64::NEG_INFINITY,
        tie_break: 0,
    };
}

#[derive(Debug, PartialEq)]
pub enum CursorError {
    Malformed,
    /// The index was rebuilt since the cursor was issued.
    Expired,
}

impl Cursor {
    pub fn encode(&self) -> String {
        format!(
            "{:x}.{:x}.{:x}",
            self.index_version,
            self.rank.score.to_bits(),
            self.rank.tie_break
        )
    }

    pub fn decode(text: &str, index_version: u64) -> Result<Cursor, CursorError> {
        let parts: Vec<u64> = text
            .split('.')
            .map(|p| u64::from_str_radix(p, 16))
            .collect::<Result<_, _>>()
            .map_err(|_| CursorError::Malformed)?;
        let [version, score, tie_break] = parts[..] else {
            return Err(CursorError::Malformed);
        };
        if version != index_version {
            return Err(CursorError::Expired);
        }
        Ok(Cursor {
            index_version: version,
            rank: RankKey {
                score: f64::from_bits(score),
                tie_break,
            },
        })
    }
}

/// What to search for: Monarch's `q` and `category` filter, and the page.
#[derive(Debug, Clone)]
pub struct SearchRequest<'a> {
    pub q: &'a str,
    pub categories: &'a [String],
    pub limit: usize,
    pub page: Page,
}

pub struct EntityIndex {
    reader: IndexReader,
    fields: Fields,
    index: Index,
    /// Identifies this build of the index, so cursors from an earlier build are refused.
    version: u64,
}

impl EntityIndex {
    /// Open an index directory for serving (read-only: see readonly.rs).
    pub fn open(dir: &Path) -> Result<EntityIndex> {
        let directory = ReadOnlyDirectory::open(dir)
            .with_context(|| format!("opening index {}", dir.display()))?;
        EntityIndex::from_index(Index::open(directory)?)
    }

    pub fn from_index(index: Index) -> Result<EntityIndex> {
        analysis::register(index.tokenizers());
        let fields = Fields::new(&index.schema())?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let mut segments: Vec<String> = index
            .searchable_segment_ids()?
            .iter()
            .map(SegmentId::uuid_string)
            .collect();
        segments.sort();
        let version = fnv1a(segments.join(",").as_bytes());
        Ok(EntityIndex {
            reader,
            fields,
            index,
            version,
        })
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// Monarch's `/autocomplete`: the top 10 for `q`.
    pub fn autocomplete(&self, q: &str) -> Result<SearchResults> {
        self.search(&SearchRequest {
            q,
            categories: &[],
            limit: monarch::AUTOCOMPLETE_LIMIT,
            page: Page::Offset(0),
        })
    }

    pub fn search(&self, request: &SearchRequest) -> Result<SearchResults> {
        let q = request.q.trim();
        let blank = q.is_empty() || q == monarch::MATCH_ALL;
        let searcher = self.reader.searcher();

        let matching: BoxQuery = if blank {
            Box::new(AllQuery)
        } else {
            self.text_query(q)
        };
        let query = match self.category_filter(request.categories) {
            Some(filter) => Box::new(BooleanQuery::new(vec![
                (Occur::Must, matching),
                (Occur::Must, filter),
            ])),
            None => matching,
        };
        let multipliers = self.query_multipliers(&searcher, q, blank)?;
        self.rank(&searcher, &query, multipliers, blank, request)
    }

    /// Monarch's edismax query: every whitespace-separated word must match (q.op=AND, mm=100%),
    /// each scored by its best field (dismax, tie 0), with the boosts of monarch::QUERY_FIELDS.
    fn text_query(&self, q: &str) -> BoxQuery {
        let schema = self.index.schema();
        let tokenizers = self.index.tokenizers();
        let words: Vec<(Occur, BoxQuery)> = q
            .split_whitespace()
            .map(|word| {
                let per_field: Vec<BoxQuery> = monarch::QUERY_FIELDS
                    .iter()
                    .filter_map(|&(name, field_type, boost)| {
                        let field = schema
                            .get_field(name)
                            .expect("query fields are in the schema");
                        let terms = match field_type {
                            SolrFieldType::Exact => vec![word.to_string()],
                            SolrFieldType::Text => analysis::terms(tokenizers, TEXT, word),
                            SolrFieldType::Autocomplete => {
                                analysis::terms(tokenizers, AUTOCOMPLETE_QUERY, word)
                            }
                        };
                        let query = all_terms(field, &terms)?;
                        Some(Box::new(BoostQuery::new(query, boost)) as BoxQuery)
                    })
                    .collect();
                let best_field: BoxQuery =
                    Box::new(DisjunctionMaxQuery::with_tie_breaker(per_field, 0.0));
                (Occur::Must, best_field)
            })
            .collect();
        Box::new(BooleanQuery::new(words))
    }

    /// Monarch's `category` filter: any of the categories, without changing scores.
    fn category_filter(&self, categories: &[String]) -> Option<BoxQuery> {
        if categories.is_empty() {
            return None;
        }
        let any: Vec<(Occur, BoxQuery)> = categories
            .iter()
            .map(|c| {
                let term = Term::from_field_text(self.fields.category, c);
                let query: BoxQuery = Box::new(TermQuery::new(term, IndexRecordOption::Basic));
                (Occur::Should, query)
            })
            .collect();
        Some(Box::new(ConstScoreQuery::new(
            Box::new(BooleanQuery::new(any)),
            0.0,
        )))
    }

    /// Multipliers that depend on the query, per index segment and document: the exact-synonym
    /// boost, or for a blank search the site examples.
    fn query_multipliers(
        &self,
        searcher: &Searcher,
        q: &str,
        blank: bool,
    ) -> Result<HashMap<SegmentId, HashMap<DocId, f64>>> {
        let mut multipliers: HashMap<SegmentId, HashMap<DocId, f64>> = HashMap::new();
        let mut multiply = |field: Field, value: &str, by: f64| -> Result<()> {
            let query = TermQuery::new(
                Term::from_field_text(field, value),
                IndexRecordOption::Basic,
            );
            for addr in searcher.search(&query, &DocSetCollector)? {
                let segment = searcher.segment_reader(addr.segment_ord).segment_id();
                *multipliers
                    .entry(segment)
                    .or_default()
                    .entry(addr.doc_id)
                    .or_insert(1.0) *= by;
            }
            Ok(())
        };
        if blank {
            for (i, id) in monarch::BLANK_SEARCH_EXAMPLES.iter().enumerate() {
                multiply(self.fields.id, id, monarch::blank_search_multiplier(i))?;
            }
        } else {
            multiply(self.fields.exact_synonym, q, monarch::EXACT_SYNONYM_BOOST)?;
        }
        Ok(multipliers)
    }

    /// Score every match as Monarch does (text score x static boost x multipliers; edismax's
    /// `boost=` is multiplicative, so it can't be a BoostQuery) and return the requested page.
    fn rank(
        &self,
        searcher: &Searcher,
        query: &BoxQuery,
        multipliers: HashMap<SegmentId, HashMap<DocId, f64>>,
        blank: bool,
        request: &SearchRequest,
    ) -> Result<SearchResults> {
        let segment_ords: HashMap<SegmentId, u32> = searcher
            .segment_readers()
            .iter()
            .enumerate()
            .map(|(ord, reader)| (reader.segment_id(), ord as u32))
            .collect();
        let after = match &request.page {
            Page::Cursor(Some(cursor)) => Some(cursor.rank),
            _ => None,
        };
        let offset = match request.page {
            Page::Offset(n) => n,
            Page::Cursor(_) => 0,
        };

        // tantivy calls the outer closure once per index segment, to load that segment's static
        // boosts and multipliers, and the inner one once per matching document in it.
        let scored_by_monarch = move |segment: &SegmentReader| {
            let static_boost = segment
                .fast_fields()
                .f64(schema::STATIC_BOOST)
                .expect("static_boost is a fast field");
            let segment_ord = segment_ords[&segment.segment_id()];
            let multipliers = multipliers
                .get(&segment.segment_id())
                .cloned()
                .unwrap_or_default();
            move |doc: DocId, text_score: Score| {
                let base = if blank { 1.0 } else { text_score as f64 };
                let score = base
                    * static_boost.first(doc).unwrap_or(1.0)
                    * multipliers.get(&doc).copied().unwrap_or(1.0);
                let rank = RankKey::new(score, segment_ord, doc);
                match after {
                    Some(cursor) if rank >= cursor => RankKey::PASSED,
                    _ => rank,
                }
            }
        };
        // One extra hit tells us whether there is a next page.
        let top = TopDocs::with_limit(request.limit + 1)
            .and_offset(offset)
            .tweak_score(scored_by_monarch);
        let (hits, total) = searcher.search(query, &(top, Count))?;

        let mut hits: Vec<(RankKey, DocAddress)> = hits
            .into_iter()
            .filter(|(rank, _)| *rank != RankKey::PASSED)
            .collect();
        let has_more = hits.len() > request.limit;
        hits.truncate(request.limit);

        let next_cursor = match (has_more, hits.last()) {
            (true, Some((rank, _))) => Some(
                Cursor {
                    index_version: self.version,
                    rank: *rank,
                }
                .encode(),
            ),
            _ => None,
        };
        let items = hits
            .into_iter()
            .map(|(rank, addr)| {
                Ok(SearchResult {
                    entity: self.load(searcher, addr)?,
                    score: rank.score,
                    bdc: None,
                })
            })
            .collect::<Result<_>>()?;
        Ok(SearchResults {
            limit: request.limit,
            offset,
            total,
            items,
            next_cursor,
        })
    }

    /// Monarch's `/entity/{id}`, from the stored document.
    pub fn entity(&self, id: &str) -> Result<Option<Entity>> {
        let searcher = self.reader.searcher();
        let query = TermQuery::new(
            Term::from_field_text(self.fields.id, id),
            IndexRecordOption::Basic,
        );
        let found = searcher.search(&query, &TopDocs::with_limit(1).order_by_score())?;
        found
            .first()
            .map(|(_, addr)| self.load(&searcher, *addr))
            .transpose()
    }

    /// Decode a client's cursor against this index.
    pub fn cursor(&self, text: &str) -> Result<Cursor, CursorError> {
        Cursor::decode(text, self.version)
    }

    fn load(&self, searcher: &Searcher, addr: DocAddress) -> Result<Entity> {
        let stored: TantivyDocument = searcher.doc(addr)?;
        let json = stored
            .get_first(self.fields.stored_entity)
            .and_then(|v| v.as_str())
            .context("stored entity missing")?;
        let mut entity: Entity = serde_json::from_str(json)?;
        // Monarch requires a name. Restricted and unlabelled nodes fall back to their id here,
        // at read time: indexing the id as a name would put CURIE text in the name fields.
        if entity.name.as_deref().is_none_or(|n| n.trim().is_empty()) {
            entity.name = Some(entity.id.clone());
        }
        Ok(entity)
    }
}

/// One field's query for one word: its single term, or all of them when the analyzer splits
/// the word (e.g. `x-linked`).
fn all_terms(field: Field, terms: &[String]) -> Option<BoxQuery> {
    let term_query = |t: &String| -> BoxQuery {
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

/// FNV-1a: a small, stable hash for the index version (std's hasher may change between builds).
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, b| {
        (hash ^ *b as u64).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn entity(id: &str, name: &str, category: &str, static_boost: f64) -> Entity {
        Entity {
            id: id.into(),
            name: Some(name.into()),
            category: Some(category.into()),
            static_boost,
            ..Default::default()
        }
    }

    /// An in-memory index of the given entities.
    pub fn index_of(entities: Vec<Entity>) -> EntityIndex {
        let index = Index::create_in_ram(schema::build());
        write_entities(&index, entities.into_iter().map(Ok), 15_000_000).unwrap();
        EntityIndex::from_index(index).unwrap()
    }

    fn ids(results: &SearchResults) -> Vec<&str> {
        results.items.iter().map(|r| r.entity.id.as_str()).collect()
    }

    fn search(
        index: &EntityIndex,
        q: &str,
        categories: &[String],
        limit: usize,
        page: Page,
    ) -> SearchResults {
        index
            .search(&SearchRequest {
                q,
                categories,
                limit,
                page,
            })
            .unwrap()
    }

    #[test]
    fn disease_outranks_phenotype_with_the_same_name() {
        let index = index_of(vec![
            entity("HP:1", "asthma", "biolink:PhenotypicFeature", 1.1),
            entity("MONDO:1", "asthma", "biolink:Disease", 1.3),
        ]);
        assert_eq!(
            ids(&index.autocomplete("asthma").unwrap()),
            ["MONDO:1", "HP:1"]
        );
    }

    #[test]
    fn deprecated_nodes_sink() {
        let index = index_of(vec![
            entity("MONDO:1", "asthma", "biolink:Disease", 1.3 * 0.1),
            entity("MONDO:2", "asthma", "biolink:Disease", 1.3),
        ]);
        assert_eq!(
            ids(&index.autocomplete("asthma").unwrap()),
            ["MONDO:2", "MONDO:1"]
        );
    }

    #[test]
    fn exact_synonym_multiplies_by_five() {
        let mut named = entity("MONDO:1", "asthma attack", "biolink:Disease", 1.3);
        named.synonym = Some(vec!["asthma".into()]);
        let mut exact = named.clone();
        exact.id = "MONDO:2".into();
        exact.exact_synonym = Some(vec!["asthma".into()]);
        let index = index_of(vec![named, exact]);
        let results = index.autocomplete("asthma").unwrap();
        assert_eq!(ids(&results), ["MONDO:2", "MONDO:1"]);
        let ratio = results.items[0].score / results.items[1].score;
        assert!((ratio - 5.0).abs() < 1e-6, "ratio {ratio}");
    }

    #[test]
    fn every_word_must_match() {
        let index = index_of(vec![
            entity("MONDO:1", "heart failure", "biolink:Disease", 1.3),
            entity("MONDO:2", "heart attack", "biolink:Disease", 1.3),
        ]);
        assert_eq!(ids(&index.autocomplete("heart fail").unwrap()), ["MONDO:1"]);
    }

    #[test]
    fn category_filters_without_changing_scores() {
        let index = index_of(vec![
            entity("MONDO:1", "asthma", "biolink:Disease", 1.3),
            entity("HP:1", "asthma", "biolink:PhenotypicFeature", 1.1),
        ]);
        let all = search(&index, "asthma", &[], 10, Page::Offset(0));
        let diseases = search(
            &index,
            "asthma",
            &["biolink:Disease".into()],
            10,
            Page::Offset(0),
        );
        assert_eq!(ids(&diseases), ["MONDO:1"]);
        assert_eq!(diseases.items[0].score, all.items[0].score);
    }

    #[test]
    fn unnamed_nodes_answer_with_their_id() {
        let mut unnamed = entity("SNOMEDCT:1", "", "biolink:Procedure", 1.0);
        unnamed.name = None;
        let index = index_of(vec![unnamed]);
        let found = index.entity("SNOMEDCT:1").unwrap().unwrap();
        assert_eq!(found.name.as_deref(), Some("SNOMEDCT:1"));
    }

    #[test]
    fn cursor_pages_match_offset_pages() {
        let entities = (0..25)
            .map(|i| {
                entity(
                    &format!("MONDO:{i}"),
                    &format!("asthma type {i}"),
                    "biolink:Disease",
                    1.3,
                )
            })
            .collect();
        let index = index_of(entities);
        let mut by_cursor = Vec::new();
        let mut page = Page::Cursor(None);
        loop {
            let results = search(&index, "asthma", &[], 7, page);
            by_cursor.extend(ids(&results).iter().map(|s| s.to_string()));
            match results.next_cursor {
                Some(next) => page = Page::Cursor(Some(index.cursor(&next).unwrap())),
                None => break,
            }
        }
        let by_offset: Vec<String> = (0..4)
            .flat_map(|p| {
                let results = search(&index, "asthma", &[], 7, Page::Offset(p * 7));
                ids(&results)
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(by_cursor.len(), 25);
        assert_eq!(by_cursor, by_offset);
    }

    #[test]
    fn cursors_from_another_build_are_refused() {
        let index = index_of(vec![entity("MONDO:1", "asthma", "biolink:Disease", 1.3)]);
        assert_eq!(index.cursor("1.2.3"), Err(CursorError::Expired));
        assert_eq!(index.cursor("nonsense"), Err(CursorError::Malformed));
    }
}
