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
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
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

/// Written beside an entity index when it is built, and checked when it is opened.
pub const BUILD_INFO_FILE: &str = "bdc-search-index.json";

/// What built an index: lets the server refuse an index it can't read, and lets a release
/// manifest say what it holds.
#[derive(Debug, Serialize, Deserialize)]
pub struct BuildInfo {
    pub schema_version: u32,
    pub built_by: String,
    pub documents: u64,
}

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
    let documents = write_entities(&index, entities, heap_bytes)?;
    let info = BuildInfo {
        schema_version: schema::SCHEMA_VERSION,
        built_by: format!("bdc-search {}", env!("CARGO_PKG_VERSION")),
        documents,
    };
    std::fs::write(dir.join(BUILD_INFO_FILE), serde_json::to_vec_pretty(&info)?)?;
    make_world_readable(dir)?;
    Ok(documents)
}

/// tantivy writes some files (`.managed.json`) readable by their owner only. A served index is
/// read by whatever user the container runs as (OpenShift assigns an arbitrary one), so every
/// file is made readable by all; nothing in it is secret beyond what the image already holds.
fn make_world_readable(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let mut permissions = std::fs::metadata(&path)?.permissions();
            permissions.set_mode(permissions.mode() | 0o444);
            std::fs::set_permissions(&path, permissions)?;
        }
    }
    Ok(())
}

/// Check that `dir` holds an index this build of bdc-search can serve.
fn check_build_info(dir: &Path) -> Result<BuildInfo> {
    let path = dir.join(BUILD_INFO_FILE);
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "{} has no {BUILD_INFO_FILE}; rebuild it with `just search-index`",
            dir.display()
        )
    })?;
    let info: BuildInfo =
        serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    anyhow::ensure!(
        info.schema_version == schema::SCHEMA_VERSION,
        "{} was built by {} for index layout {}, but this bdc-search reads layout {}; rebuild it with `just search-index`",
        dir.display(),
        info.built_by,
        info.schema_version,
        schema::SCHEMA_VERSION
    );
    Ok(info)
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
        writer.add_document(schema::to_document(&fields, &entity?)?)?;
        count += 1;
        if count % 250_000 == 0 {
            tracing::info!(count, "indexed");
        }
    }
    writer.commit()?;
    writer.wait_merging_threads()?;
    Ok(count)
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
    /// The offset asked for; 0 for cursor pages, which have no offset.
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
    /// The first page of a cursor walk (`cursor=*`).
    CursorStart,
    /// The page after the hit a previous page ended on (`cursor=<next_cursor>`).
    After(Cursor),
}

/// Where a cursor page ended. Clients see it as four hex numbers,
/// `<index version>.<query>.<score>.<tie break>`:
///   - index version: which build of the index ranked the hits (see `EntityIndex::version`)
///   - query: a fingerprint of `q` and the categories, so a cursor only continues its own search
///   - score and tie break: the last hit's place in the ranking (`RankKey`)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursor {
    index_version: u64,
    query: u64,
    rank: RankKey,
}

/// A hit's place in the ranking. The derived ordering compares the fields top to bottom, so
/// their order matters: higher score first, then, among equal scores, the hit stored earlier in
/// the index. Every hit has its own place, so cursor pages never overlap or skip.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
struct RankKey {
    score: f64,
    /// u64::MAX minus the hit's position (segment, document), so an earlier position is a
    /// higher key.
    tie_break: u64,
}

impl RankKey {
    fn new(score: f64, segment_ord: u32, doc: DocId) -> RankKey {
        let position = ((segment_ord as u64) << 32) | doc as u64;
        RankKey {
            score,
            tie_break: u64::MAX - position,
        }
    }

    /// Lower than every real hit. tantivy keeps the N highest keys it is shown and can't skip
    /// documents, so hits a cursor has already passed get this key: they lose to every hit not
    /// yet returned, and are dropped after collection.
    const PASSED: RankKey = RankKey {
        score: f64::NEG_INFINITY,
        tie_break: 0,
    };
}

#[derive(Debug, PartialEq)]
pub enum CursorError {
    /// Not a cursor this API issued.
    Malformed,
    /// Issued by an earlier build of the index.
    Expired,
    /// Issued for a different `q` or `category`.
    OtherQuery,
}

impl Cursor {
    fn encode(&self) -> String {
        format!(
            "{:x}.{:x}.{:x}.{:x}",
            self.index_version,
            self.query,
            self.rank.score.to_bits(),
            self.rank.tie_break
        )
    }

    fn decode(text: &str, index_version: u64, query: u64) -> Result<Cursor, CursorError> {
        let parts: Vec<u64> = text
            .split('.')
            .map(|p| u64::from_str_radix(p, 16))
            .collect::<Result<_, _>>()
            .map_err(|_| CursorError::Malformed)?;
        let [version, query_of_cursor, score_bits, tie_break] = parts[..] else {
            return Err(CursorError::Malformed);
        };
        let score = f64::from_bits(score_bits);
        if !score.is_finite() {
            return Err(CursorError::Malformed);
        }
        if version != index_version {
            return Err(CursorError::Expired);
        }
        if query_of_cursor != query {
            return Err(CursorError::OtherQuery);
        }
        Ok(Cursor {
            index_version: version,
            query,
            rank: RankKey { score, tie_break },
        })
    }
}

/// A fingerprint of a search, so a cursor only continues the search it came from.
fn query_fingerprint(q: &str, categories: &[String]) -> u64 {
    let mut categories = categories.to_vec();
    categories.sort();
    fnv1a(format!("{}\u{1f}{}", q.trim(), categories.join("\u{1f}")).as_bytes())
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
    /// Identifies this build of the index: a hash of its segment ids, which are fixed when the
    /// index is built. Replicas serving copies of one index compute the same version, so a
    /// cursor from one works on another; a rebuilt index has new segments, so its cursors
    /// don't match and old ones are refused.
    version: u64,
}

impl EntityIndex {
    /// Open an index directory for serving (read-only: see readonly.rs).
    pub fn open(dir: &Path) -> Result<EntityIndex> {
        check_build_info(dir)?;
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

    /// This build of the index, as cursors and `/version` show it.
    pub fn version(&self) -> String {
        format!("{:x}", self.version)
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
    ) -> Result<HashMap<DocAddress, f64>> {
        let mut multipliers: HashMap<DocAddress, f64> = HashMap::new();
        let mut multiply = |field: Field, value: &str, by: f64| -> Result<()> {
            let query = TermQuery::new(
                Term::from_field_text(field, value),
                IndexRecordOption::Basic,
            );
            for addr in searcher.search(&query, &DocSetCollector)? {
                *multipliers.entry(addr).or_insert(1.0) *= by;
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
        multipliers: HashMap<DocAddress, f64>,
        blank: bool,
        request: &SearchRequest,
    ) -> Result<SearchResults> {
        // Segments are numbered by their place in the searcher; the cursor's tie break and the
        // multipliers' DocAddress both use that number.
        let segment_ords: HashMap<SegmentId, u32> = searcher
            .segment_readers()
            .iter()
            .enumerate()
            .map(|(ord, reader)| (reader.segment_id(), ord as u32))
            .collect();
        let after = match &request.page {
            Page::After(cursor) => Some(cursor.rank),
            _ => None,
        };
        let offset = match request.page {
            Page::Offset(n) => n,
            Page::CursorStart | Page::After(_) => 0,
        };
        let multipliers = Arc::new(multipliers);

        // tantivy calls the outer closure once per index segment, to load that segment's static
        // boosts, and the inner one once per matching document in it.
        //
        // Cursor pages: tantivy keeps only the N highest keys it is shown, so every hit at or
        // above the cursor's place gets RankKey::PASSED; the N highest of the rest are the next
        // page. Every page still scores every match; only the kept list stays small, so a deep
        // page costs the same as the first.
        let scored_by_monarch = move |segment: &SegmentReader| {
            let static_boost = segment
                .fast_fields()
                .f64(schema::STATIC_BOOST)
                .expect("static_boost is a fast field");
            let segment_ord = segment_ords[&segment.segment_id()];
            let multipliers = Arc::clone(&multipliers);
            move |doc: DocId, text_score: Score| {
                let base = if blank { 1.0 } else { text_score as f64 };
                let multiplier = multipliers.get(&DocAddress::new(segment_ord, doc));
                let score = base
                    * static_boost.first(doc).unwrap_or(1.0)
                    * multiplier.copied().unwrap_or(1.0);
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
                    query: query_fingerprint(request.q, request.categories),
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

    /// Decode a client's cursor for this index and this search.
    pub fn cursor(
        &self,
        text: &str,
        q: &str,
        categories: &[String],
    ) -> Result<Cursor, CursorError> {
        Cursor::decode(text, self.version, query_fingerprint(q, categories))
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
        let mut page = Page::CursorStart;
        loop {
            let results = search(&index, "asthma", &[], 7, page);
            by_cursor.extend(ids(&results).iter().map(|s| s.to_string()));
            match results.next_cursor {
                Some(next) => page = Page::After(index.cursor(&next, "asthma", &[]).unwrap()),
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
    fn cursors_only_continue_their_own_search_on_their_own_index() {
        let index = index_of(vec![
            entity("MONDO:1", "asthma", "biolink:Disease", 1.3),
            entity("MONDO:2", "allergic asthma", "biolink:Disease", 1.3),
        ]);
        let page = search(&index, "asthma", &[], 1, Page::CursorStart);
        let cursor = page.next_cursor.unwrap();
        assert!(index.cursor(&cursor, "asthma", &[]).is_ok());
        assert_eq!(
            index.cursor(&cursor, "heart", &[]),
            Err(CursorError::OtherQuery)
        );
        assert_eq!(
            index.cursor("1.2.3.4", "asthma", &[]),
            Err(CursorError::Expired)
        );
        assert_eq!(
            index.cursor("nonsense", "asthma", &[]),
            Err(CursorError::Malformed)
        );
        let version_and_query = cursor.rsplitn(3, '.').nth(2).unwrap();
        let nan = format!("{version_and_query}.{:x}.0", f64::NAN.to_bits());
        assert_eq!(
            index.cursor(&nan, "asthma", &[]),
            Err(CursorError::Malformed)
        );
    }

    #[test]
    fn an_index_from_another_layout_is_refused_with_a_reason() {
        let dir = std::env::temp_dir().join(format!("bdc-search-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        build_index(
            "{\"id\":\"MONDO:1\",\"name\":\"asthma\"}\n".as_bytes(),
            &dir,
            15_000_000,
        )
        .unwrap();
        assert!(EntityIndex::open(&dir).is_ok());
        #[cfg(unix)]
        for entry in std::fs::read_dir(&dir).unwrap() {
            use std::os::unix::fs::PermissionsExt;
            let entry = entry.unwrap();
            let mode = entry.metadata().unwrap().permissions().mode();
            assert_eq!(
                mode & 0o444,
                0o444,
                "{} isn't world-readable",
                entry.path().display()
            );
        }

        let stale = BuildInfo {
            schema_version: 1,
            built_by: "bdc-search 0.0.1".into(),
            documents: 1,
        };
        std::fs::write(
            dir.join(BUILD_INFO_FILE),
            serde_json::to_vec(&stale).unwrap(),
        )
        .unwrap();
        let error = EntityIndex::open(&dir).err().unwrap().to_string();
        assert!(
            error.contains("layout 1") && error.contains("just search-index"),
            "{error}"
        );

        std::fs::remove_file(dir.join(BUILD_INFO_FILE)).unwrap();
        assert!(EntityIndex::open(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
