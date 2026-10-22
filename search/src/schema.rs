//! The entity index: Monarch's Solr entity core fields, plus a stored copy of each document
//! in Monarch's response shape and a precomputed boost prior.

use serde::{Deserialize, Serialize};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, FAST, STORED, STRING,
};

use crate::analysis::{AUTOCOMPLETE, TEXT};

/// One KG node, as `sql/search_entity.sql` exports it and as `/autocomplete` returns it.
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
}

impl Entity {
    /// Monarch's static boosts from `entity_boost`: category, human gene, deprecation.
    /// Query-dependent boosts (exact synonym, blank search) are applied at query time.
    pub fn prior(&self) -> f64 {
        let mut prior = match self.category.as_deref() {
            Some("biolink:PhenotypicFeature") => 1.1,
            Some("biolink:Disease") => 1.3,
            Some("biolink:Gene") if self.in_taxon.as_deref() == Some("NCBITaxon:9606") => 1.1,
            _ => 1.0,
        };
        if self.deprecated == Some(true) {
            prior *= 0.1;
        }
        prior
    }
}

/// How a query term is matched against a field, mirroring the Solr field types.
#[derive(Debug, Clone, Copy)]
pub enum Kind {
    /// Solr `string`: the whole value is one case-sensitive term.
    Raw,
    /// Solr `text` (`*_t`).
    Text,
    /// Solr `autocomplete` (`*_ac`).
    Autocomplete,
}

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
    pub prior: Field,
    pub doc: Field,
}

impl Fields {
    /// Monarch's `entity_query_fields()`, which autocomplete uses: the autocomplete fields
    /// plus description_t and xref.
    pub fn query_fields(&self) -> Vec<(Field, Kind, f32)> {
        vec![
            (self.id, Kind::Raw, 100.0),
            (self.name, Kind::Raw, 10.0),
            (self.name_t, Kind::Text, 5.0),
            (self.name_ac, Kind::Autocomplete, 1.0),
            (self.symbol, Kind::Raw, 10.0),
            (self.symbol_t, Kind::Text, 5.0),
            (self.symbol_ac, Kind::Autocomplete, 1.0),
            (self.synonym, Kind::Raw, 1.0),
            (self.synonym_t, Kind::Text, 1.0),
            (self.synonym_ac, Kind::Autocomplete, 1.0),
            (self.in_taxon_label_t, Kind::Text, 1.0),
            (self.description_t, Kind::Text, 1.0),
            (self.xref, Kind::Raw, 1.0),
        ]
    }
}

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
            // tantivy counts every gram toward field length, where Lucene's discountOverlaps
            // counts only word positions; without norms, long names and synonym lists aren't
            // penalized for their grams.
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
    builder.add_f64_field("prior", FAST);
    builder.add_text_field("doc", STORED);
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
            prior: f("prior")?,
            doc: f("doc")?,
        })
    }
}
