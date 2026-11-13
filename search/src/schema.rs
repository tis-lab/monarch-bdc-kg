//! The entity index: Monarch's Solr entity core fields, plus a stored copy of each document in
//! Monarch's response shape and its static boost.
//!
//! Like Monarch's Solr copyFields, one source column feeds several index fields, one per way of
//! matching it:
//!
//!   name, symbol, synonym   ->  <col> (exact), <col>_t (text), <col>_ac (prefix)
//!   in_taxon_label, description  ->  <col>_t (text)
//!   id, category, exact_synonym, xref  ->  <col> (exact)
//!
//! The query side of that table, with the boosts, is monarch::QUERY_FIELDS.

use serde::{Deserialize, Serialize};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, FAST, STORED, STRING,
};
use tantivy::{doc, TantivyDocument};

use crate::analysis::{AUTOCOMPLETE, TEXT};

/// One KG node, as sql/search_entity.sql exports it and as `/autocomplete` returns it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub category: Option<String>,
    pub name: Option<String>,
    pub full_name: Option<String>,
    pub symbol: Option<String>,
    pub description: Option<String>,
    pub xref: Option<Vec<String>>,
    pub synonym: Option<Vec<String>>,
    pub exact_synonym: Option<Vec<String>>,
    pub broad_synonym: Option<Vec<String>>,
    pub narrow_synonym: Option<Vec<String>>,
    pub related_synonym: Option<Vec<String>>,
    pub deprecated: Option<bool>,
    pub in_taxon: Option<String>,
    pub in_taxon_label: Option<String>,
    pub iri: Option<String>,
    pub same_as: Option<Vec<String>>,
    pub subsets: Option<Vec<String>>,
    pub provided_by: Option<String>,
    pub file_source: Option<String>,
    pub namespace: Option<String>,
    #[serde(rename = "type")]
    pub type_: Option<String>,
    /// Monarch's node-level boosts, computed in sql/search_entity.sql. Read on export, kept in
    /// the index as a fast field, never returned.
    #[serde(default = "no_boost", skip_serializing)]
    pub static_boost: f64,
}

fn no_boost() -> f64 {
    1.0
}

/// How a query word is matched against a field, mirroring Monarch's Solr field types.
#[derive(Debug, Clone, Copy)]
pub enum SolrFieldType {
    /// Solr `string`: the whole value is one case-sensitive term.
    Exact,
    /// Solr `text` (`*_t`): words, folded, lower-cased, plurals stripped.
    Text,
    /// Solr `autocomplete` (`*_ac`): word prefixes.
    Autocomplete,
}

/// Handles to the index's fields, looked up by name once when the index is opened.
#[derive(Clone)]
pub struct Fields {
    pub id: Field,
    pub category: Field,
    pub name: Field,
    pub name_t: Field,
    pub name_ac: Field,
    pub symbol: Field,
    pub symbol_t: Field,
    pub symbol_ac: Field,
    pub synonym: Field,
    pub synonym_t: Field,
    pub synonym_ac: Field,
    pub exact_synonym: Field,
    pub in_taxon_label_t: Field,
    pub description_t: Field,
    pub xref: Field,
    pub static_boost: Field,
    /// The whole entity as JSON, returned as is.
    pub stored_entity: Field,
}

pub const STATIC_BOOST: &str = "static_boost";

/// The index layout this build of bdc-search reads. Bump it whenever fields change: the server
/// refuses an index built for another layout, with a message, instead of failing on a missing
/// field.
pub const SCHEMA_VERSION: u32 = 2;

pub fn build() -> Schema {
    let mut builder = Schema::builder();
    let text = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TEXT)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let autocomplete = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(AUTOCOMPLETE)
            .set_index_option(IndexRecordOption::WithFreqs)
            // tantivy counts every prefix toward a field's length, where Lucene's
            // discountOverlaps counts only word positions; without norms, long names and
            // synonym lists aren't penalized for their prefixes.
            .set_fieldnorms(false),
    );
    for name in [
        "id",
        "category",
        "name",
        "symbol",
        "synonym",
        "exact_synonym",
        "xref",
    ] {
        builder.add_text_field(name, STRING);
    }
    for name in [
        "name_t",
        "symbol_t",
        "synonym_t",
        "in_taxon_label_t",
        "description_t",
    ] {
        builder.add_text_field(name, text.clone());
    }
    for name in ["name_ac", "symbol_ac", "synonym_ac"] {
        builder.add_text_field(name, autocomplete.clone());
    }
    builder.add_f64_field(STATIC_BOOST, FAST);
    builder.add_text_field("stored_entity", STORED);
    builder.build()
}

impl Fields {
    pub fn new(schema: &Schema) -> tantivy::Result<Fields> {
        let f = |name: &str| schema.get_field(name);
        Ok(Fields {
            id: f("id")?,
            category: f("category")?,
            name: f("name")?,
            name_t: f("name_t")?,
            name_ac: f("name_ac")?,
            symbol: f("symbol")?,
            symbol_t: f("symbol_t")?,
            symbol_ac: f("symbol_ac")?,
            synonym: f("synonym")?,
            synonym_t: f("synonym_t")?,
            synonym_ac: f("synonym_ac")?,
            exact_synonym: f("exact_synonym")?,
            in_taxon_label_t: f("in_taxon_label_t")?,
            description_t: f("description_t")?,
            xref: f("xref")?,
            static_boost: f(STATIC_BOOST)?,
            stored_entity: f("stored_entity")?,
        })
    }
}

/// One entity as an index document: each source column into its index fields (the table at
/// the top of this file).
pub fn to_document(f: &Fields, e: &Entity) -> anyhow::Result<TantivyDocument> {
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
