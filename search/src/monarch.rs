//! Monarch's search rules, in one place.
//!
//! Each constant copies a function in monarch-app (backend/src/monarch_py/implementations/solr/
//! solr_query_utils.py) or a FastAPI route parameter, so responses rank and page like Monarch's.
//! When Monarch changes one of these, change it here.
//!
//! The static boosts (disease, phenotype, human gene, deprecated) depend only on the node, so
//! they are computed in sql/search_entity.sql as the `static_boost` column.

use crate::schema::SolrFieldType::{self, Autocomplete, Exact, Text};

/// `entity_query_fields()`: the fields a query word is matched against, how, and their boost.
/// Each word scores by its best field (edismax dismax, tie 0); every word must match some
/// field (q.op=AND, mm=100%).
pub const QUERY_FIELDS: [(&str, SolrFieldType, f32); 13] = [
    ("id", Exact, 100.0),
    ("name", Exact, 10.0),
    ("name_t", Text, 5.0),
    ("name_ac", Autocomplete, 1.0),
    ("symbol", Exact, 10.0),
    ("symbol_t", Text, 5.0),
    ("symbol_ac", Autocomplete, 1.0),
    ("synonym", Exact, 1.0),
    ("synonym_t", Text, 1.0),
    ("synonym_ac", Autocomplete, 1.0),
    ("in_taxon_label_t", Text, 1.0),
    ("description_t", Text, 1.0),
    ("xref", Exact, 1.0),
];

/// `exact_match_boost()`: multiplier when the whole query equals one of a node's exact synonyms.
pub const EXACT_SYNONYM_BOOST: f64 = 5.0;

/// `blank_search_boost()`: nodes shown first for an empty search, as site examples.
pub const BLANK_SEARCH_EXAMPLES: [&str; 10] = [
    "MONDO:0007523",  // Ehlers-Danlos syndrome, hypermobility type
    "MONDO:0019391",  // Fanconi anemia
    "MONDO:0018954",  // Loeys-Dietz syndrome
    "MONDO:0011518",  // Wiedemann-Steiner syndrome
    "HP:0001166",     // Arachnodactyly
    "HP:0001631",     // Atrial septal defect
    "UBERON:0000948", // heart
    "UBERON:0006585", // vestibular organ
    "HGNC:4851",      // HTT
    "HGNC:3603",      // FBN1
];

/// `blank_search_boost()` gives the i-th example `len - i + 2`: 12 for the first, 3 for the last.
pub fn blank_search_multiplier(position: usize) -> f64 {
    (BLANK_SEARCH_EXAMPLES.len() - position + 2) as f64
}

/// The query text Monarch treats as "everything".
pub const MATCH_ALL: &str = "*:*";

/// `build_autocomplete_query()`: autocomplete always returns 10.
pub const AUTOCOMPLETE_LIMIT: usize = 10;

/// The `/search` route's `PaginationParams`: limit 0-500, default 20.
pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 500;

/// Deepest page offset paging serves: offset + limit may not pass this. It is the bound the
/// portal middleware already applies, and Solr's and Elasticsearch's usual result window.
/// Deeper reads use cursor paging instead.
pub const MAX_RESULT_WINDOW: usize = 10_000;
