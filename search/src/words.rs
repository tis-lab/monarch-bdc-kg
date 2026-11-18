//! Word completion (`/bdc/words`): finishes the word being typed, from the vocabulary of KG
//! labels and BDC variable and concept text (`words.jsonl`, written from sql/search_words.sql).
//!
//! The words are held in an FST (the `fst` crate), a compact sorted set that finds every word
//! starting with a prefix without scanning the rest. With `fuzzy`, it also finds words whose
//! start is one typo away from the prefix, by walking the FST with a Levenshtein automaton.
//!
//! Ranking: words BDC text uses come first, by how many variables and concepts use them, then
//! by how many KG nodes do. Someone typing "hyp" in the portal wants "hypertension" before
//! "hypoplasia".

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use fst::automaton::{Automaton, Levenshtein, Str};
use fst::{IntoStreamer, Map, MapBuilder, Streamer};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub const WORDS_FILE: &str = "words.jsonl";
pub const DEFAULT_LIMIT: usize = 10;
pub const MAX_LIMIT: usize = 100;

/// Shorter prefixes are one typo away from too many words for fuzzy matches to help.
const MIN_FUZZY_PREFIX: usize = 4;

/// One word of the vocabulary, and how much it is used.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct Word {
    pub word: String,
    /// BDC variables whose description or value labels use the word
    pub variables: u64,
    /// Harmonized concepts whose KG anchor's label or synonyms use the word
    pub concepts: u64,
    /// KG nodes whose name or synonyms use the word
    pub kg_nodes: u64,
}

impl Word {
    /// Sorts best first.
    fn rank(&self) -> impl Ord + '_ {
        (
            std::cmp::Reverse(self.variables + self.concepts),
            std::cmp::Reverse(self.kg_nodes),
            &self.word,
        )
    }
}

/// One completion.
#[derive(Debug, Serialize, ToSchema)]
pub struct Completion {
    #[serde(flatten)]
    pub word: Word,
    /// The word starts one typo away from the prefix, not with the prefix itself
    pub fuzzy: bool,
}

/// `/bdc/words`' response.
#[derive(Debug, Serialize, ToSchema)]
pub struct Completions {
    /// The word being completed: the last word of the input, lowercased
    pub prefix: String,
    pub items: Vec<Completion>,
}

pub struct Words {
    /// Each word, to its position in `words`.
    fst: Map<Vec<u8>>,
    words: Vec<Word>,
}

impl Words {
    /// The vocabulary in `dir`, when the index directory has one.
    pub fn open(dir: &Path) -> Result<Option<Words>> {
        let path = dir.join(WORDS_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let mut words = Vec::new();
        for (n, line) in BufReader::new(File::open(&path)?).lines().enumerate() {
            let line = line?;
            if !line.trim().is_empty() {
                let word = serde_json::from_str(&line)
                    .with_context(|| format!("{} line {}", path.display(), n + 1))?;
                words.push(word);
            }
        }
        Ok(Some(Words::new(words)?))
    }

    pub fn new(mut words: Vec<Word>) -> Result<Words> {
        // An FST is built from keys in byte order, each once.
        words.sort_by(|a, b| a.word.as_bytes().cmp(b.word.as_bytes()));
        words.dedup_by(|a, b| a.word == b.word);
        let mut builder = MapBuilder::memory();
        for (i, w) in words.iter().enumerate() {
            builder.insert(&w.word, i as u64)?;
        }
        let fst = Map::new(builder.into_inner()?)?;
        Ok(Words { fst, words })
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    /// The best `limit` completions of the last word in `input`: words starting with it, then,
    /// with `fuzzy`, words starting one typo away from it.
    pub fn complete(&self, input: &str, limit: usize, fuzzy: bool) -> Result<Completions> {
        let lowered = input.to_lowercase();
        let prefix = lowered
            .rsplit(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or_default()
            .to_owned();
        let mut items = Vec::new();
        if !prefix.is_empty() {
            let mut exact = self.matching(Str::new(&prefix).starts_with());
            exact.sort_by(|a, b| a.rank().cmp(&b.rank()));
            items.extend(exact.into_iter().map(|w| (w, false)));
            if fuzzy && items.len() < limit && prefix.chars().count() >= MIN_FUZZY_PREFIX {
                let mut near = self.matching(Levenshtein::new(&prefix, 1)?.starts_with());
                near.retain(|w| !w.word.starts_with(&prefix));
                near.sort_by(|a, b| a.rank().cmp(&b.rank()));
                items.extend(near.into_iter().map(|w| (w, true)));
            }
        }
        items.truncate(limit);
        let items = items
            .into_iter()
            .map(|(w, fuzzy)| Completion {
                word: w.clone(),
                fuzzy,
            })
            .collect();
        Ok(Completions { prefix, items })
    }

    /// Every word the automaton accepts.
    fn matching(&self, automaton: impl Automaton) -> Vec<&Word> {
        let mut found = Vec::new();
        let mut stream = self.fst.search(automaton).into_stream();
        while let Some((_, i)) = stream.next() {
            found.push(&self.words[i as usize]);
        }
        found
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn word(word: &str, variables: u64, concepts: u64, kg_nodes: u64) -> Word {
        Word {
            word: word.into(),
            variables,
            concepts,
            kg_nodes,
        }
    }

    pub(crate) fn sample() -> Words {
        Words::new(vec![
            word("hypoplasia", 0, 0, 1722),
            word("hypertension", 33, 2, 143),
            word("hypertrophy", 1, 2, 287),
            word("hypertensive", 1, 2, 25),
            word("pulmonary", 12, 3, 900),
            word("asthma", 20, 1, 80),
        ])
        .unwrap()
    }

    fn completed(words: &Words, input: &str, fuzzy: bool) -> Vec<(String, bool)> {
        let found = words.complete(input, 10, fuzzy).unwrap();
        found
            .items
            .into_iter()
            .map(|c| (c.word.word, c.fuzzy))
            .collect()
    }

    fn plain(words: &[&str]) -> Vec<(String, bool)> {
        words.iter().map(|w| (w.to_string(), false)).collect()
    }

    #[test]
    fn bdc_words_rank_before_kg_only_words() {
        assert_eq!(
            completed(&sample(), "hyp", false),
            plain(&["hypertension", "hypertrophy", "hypertensive", "hypoplasia"])
        );
    }

    #[test]
    fn the_last_word_of_the_input_is_completed_case_insensitively() {
        let found = sample().complete("Pulmonary HYPERT", 10, false).unwrap();
        assert_eq!(found.prefix, "hypert");
        assert_eq!(found.items.len(), 3);
    }

    #[test]
    fn fuzzy_adds_one_typo_matches_after_the_exact_ones() {
        assert_eq!(completed(&sample(), "astma", false), plain(&[]));
        assert_eq!(
            completed(&sample(), "astma", true),
            vec![("asthma".to_string(), true)]
        );
        assert_eq!(
            completed(&sample(), "pulm", true),
            plain(&["pulmonary"]),
            "nothing one typo away that isn't already an exact match"
        );
    }

    #[test]
    fn short_prefixes_are_not_fuzzy() {
        assert_eq!(completed(&sample(), "ast", true), plain(&["asthma"]));
        assert_eq!(completed(&sample(), "xst", true), plain(&[]));
    }

    #[test]
    fn input_without_a_word_completes_nothing() {
        assert_eq!(completed(&sample(), "hyp ", false), plain(&[]));
    }

    #[test]
    fn limit_keeps_the_best() {
        let found = sample().complete("hyp", 2, false).unwrap();
        let words: Vec<_> = found.items.iter().map(|c| c.word.word.as_str()).collect();
        assert_eq!(words, ["hypertension", "hypertrophy"]);
    }
}
