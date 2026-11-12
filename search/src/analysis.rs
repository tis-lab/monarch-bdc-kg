//! Analyzers matching the field types in monarch-ingest's Solr configuration
//! (scripts/autocomplete-fieldtype.json, scripts/text-fieldtype.json).
//!
//! Reading guide: most of this file is tantivy's plumbing for plugging a filter into its
//! analyzer chain (the TokenFilter / Tokenizer / TokenStream impls). The behaviour is in two
//! places: `EdgeNgramStream::advance` (word prefixes, for autocomplete) and `minimal_stem`
//! (plural stripping, for text). Neither exists in tantivy in the form Monarch's Solr uses.
//!
//! Solr's StandardTokenizer is approximated by tantivy's SimpleTokenizer, which splits on
//! non-alphanumeric characters; the two differ on apostrophes and some punctuation, which is
//! close to what the text type's WordDelimiterGraphFilter produces anyway.

use tantivy::tokenizer::{
    AsciiFoldingFilter, LowerCaser, SimpleTokenizer, TextAnalyzer, Token, TokenFilter, TokenStream,
    Tokenizer, TokenizerManager,
};

/// Index side of Solr's `autocomplete` type: folded, lowercased, edge n-grams of 1-25 chars.
pub const AUTOCOMPLETE: &str = "autocomplete";
/// Query side of `autocomplete`: the same without n-grams, so a typed prefix is one gram.
pub const AUTOCOMPLETE_QUERY: &str = "autocomplete_query";
/// Solr's `text` type: folded, lowercased, minimal English stemming.
pub const TEXT: &str = "text";

pub fn register(manager: &TokenizerManager) {
    manager.register(
        AUTOCOMPLETE,
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(AsciiFoldingFilter)
            .filter(LowerCaser)
            .filter(EdgeNgram { min: 1, max: 25 })
            .build(),
    );
    manager.register(
        AUTOCOMPLETE_QUERY,
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(AsciiFoldingFilter)
            .filter(LowerCaser)
            .build(),
    );
    manager.register(
        TEXT,
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(AsciiFoldingFilter)
            .filter(LowerCaser)
            .filter(MinimalStem)
            .build(),
    );
}

/// The terms an analyzer produces for `text`.
pub fn terms(manager: &TokenizerManager, analyzer: &str, text: &str) -> Vec<String> {
    let mut analyzer = manager.get(analyzer).expect("analyzer is registered");
    let mut stream = analyzer.token_stream(text);
    let mut out = Vec::new();
    while stream.advance() {
        out.push(stream.token().text.clone());
    }
    out
}

/// Solr's EdgeNGramFilter: replaces each token with its prefixes of `min..=max` chars, all at
/// the token's position. Tokens shorter than `min` are dropped, as Solr does.
#[derive(Clone)]
pub struct EdgeNgram {
    pub min: usize,
    pub max: usize,
}

impl TokenFilter for EdgeNgram {
    type Tokenizer<T: Tokenizer> = EdgeNgramFilter<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        EdgeNgramFilter {
            tokenizer,
            min: self.min,
            max: self.max,
        }
    }
}

#[derive(Clone)]
pub struct EdgeNgramFilter<T> {
    tokenizer: T,
    min: usize,
    max: usize,
}

impl<T: Tokenizer> Tokenizer for EdgeNgramFilter<T> {
    type TokenStream<'a> = EdgeNgramStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        EdgeNgramStream {
            tail: self.tokenizer.token_stream(text),
            min: self.min,
            max: self.max,
            base: String::new(),
            gram_ends: Vec::new(),
            next: 0,
            token: Token::default(),
        }
    }
}

pub struct EdgeNgramStream<S> {
    tail: S,
    min: usize,
    max: usize,
    base: String,
    /// Byte offsets in `base` where each gram ends.
    gram_ends: Vec<usize>,
    next: usize,
    token: Token,
}

impl<S: TokenStream> TokenStream for EdgeNgramStream<S> {
    fn advance(&mut self) -> bool {
        loop {
            if let Some(&end) = self.gram_ends.get(self.next) {
                self.next += 1;
                self.token.text.clear();
                self.token.text.push_str(&self.base[..end]);
                return true;
            }
            if !self.tail.advance() {
                return false;
            }
            let (min, max) = (self.min, self.max);
            self.token = self.tail.token().clone();
            self.base.clear();
            self.base.push_str(&self.token.text);
            self.gram_ends = self
                .base
                .char_indices()
                .map(|(i, c)| i + c.len_utf8())
                .enumerate()
                .filter(|(n, _)| (min..=max).contains(&(n + 1)))
                .map(|(_, end)| end)
                .collect();
            self.next = 0;
        }
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

/// Lucene's EnglishMinimalStemmer: strips plural endings only (ies -> y, es -> e, s -> "").
#[derive(Clone)]
pub struct MinimalStem;

impl TokenFilter for MinimalStem {
    type Tokenizer<T: Tokenizer> = MinimalStemFilter<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        MinimalStemFilter { tokenizer }
    }
}

#[derive(Clone)]
pub struct MinimalStemFilter<T> {
    tokenizer: T,
}

impl<T: Tokenizer> Tokenizer for MinimalStemFilter<T> {
    type TokenStream<'a> = MinimalStemStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        MinimalStemStream {
            tail: self.tokenizer.token_stream(text),
        }
    }
}

pub struct MinimalStemStream<S> {
    tail: S,
}

impl<S: TokenStream> TokenStream for MinimalStemStream<S> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        let text = &mut self.tail.token_mut().text;
        if text.is_ascii() {
            let stemmed = minimal_stem(text.as_bytes());
            if stemmed.len() != text.len() || stemmed.as_slice() != text.as_bytes() {
                *text = String::from_utf8(stemmed).expect("ascii in, ascii out");
            }
        }
        true
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

fn minimal_stem(s: &[u8]) -> Vec<u8> {
    let len = s.len();
    if len < 3 || s[len - 1] != b's' {
        return s.to_vec();
    }
    match s[len - 2] {
        b'u' | b's' => s.to_vec(),
        b'e' => {
            if len > 3 && s[len - 3] == b'i' && s[len - 4] != b'a' && s[len - 4] != b'e' {
                let mut out = s[..len - 3].to_vec();
                out.push(b'y');
                out
            } else if matches!(s[len - 3], b'i' | b'a' | b'o' | b'e') {
                s.to_vec()
            } else {
                s[..len - 1].to_vec()
            }
        }
        _ => s[..len - 1].to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze(analyzer: &str, text: &str) -> Vec<String> {
        let manager = TokenizerManager::default();
        register(&manager);
        terms(&manager, analyzer, text)
    }

    #[test]
    fn edge_ngrams_cover_each_word_prefix() {
        assert_eq!(
            analyze(AUTOCOMPLETE, "Hyper-Tension"),
            [
                "h", "hy", "hyp", "hype", "hyper", "t", "te", "ten", "tens", "tensi", "tensio",
                "tension"
            ]
        );
    }

    #[test]
    fn edge_ngrams_stop_at_max() {
        let grams = analyze(AUTOCOMPLETE, &"a".repeat(30));
        assert_eq!(grams.len(), 25);
    }

    #[test]
    fn edge_ngrams_fold_accents() {
        assert_eq!(analyze(AUTOCOMPLETE, "Sjögren").last().unwrap(), "sjogren");
    }

    #[test]
    fn minimal_stem_matches_lucene() {
        assert_eq!(analyze(TEXT, "diseases"), ["disease"]);
        assert_eq!(analyze(TEXT, "allergies"), ["allergy"]);
        assert_eq!(analyze(TEXT, "status"), ["status"]);
        assert_eq!(analyze(TEXT, "glass"), ["glass"]);
        assert_eq!(analyze(TEXT, "toes"), ["toes"]);
        assert_eq!(analyze(TEXT, "kidneys"), ["kidney"]);
    }
}
