//! Custom dictionary: standalone terms + correction pairs, applied dual-path —
//! (1) recognition biasing via an initial prompt where the engine supports it,
//! (2) post-transcription correction of close misspellings.
//!
//! Contract: NEVER insert a word the speaker didn't say. Fuzzy correction only
//! replaces an existing token that is close to a known term, and never when the
//! spoken token is itself a common English word (those need an explicit pair).
//! Behavioural contract: shared/dictionary-test-vectors.json.

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictEntry {
    pub id: i64,
    /// Canonical output form, exact casing ("Claude Code", "farsiight", "Aysha").
    pub term: String,
    /// Explicit misheard forms that always map to `term` ("cloud code").
    #[serde(default)]
    pub corrections: Vec<String>,
    /// Whether this entry was auto-learned from user edits.
    #[serde(default)]
    pub auto_learned: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedCorrection {
    pub from: String,
    pub to: String,
    /// Byte offset in the INPUT text where the replacement happened.
    pub at: usize,
}

pub struct Dictionary {
    entries: Vec<DictEntry>,
}

/// Similarity floor for fuzzy misspelling correction. High on purpose: a wrong
/// correction is worse than a missed one.
const FUZZY_THRESHOLD: f64 = 0.88;
/// Fuzzy correction only considers tokens of at least this many chars.
const FUZZY_MIN_LEN: usize = 4;

impl Dictionary {
    pub fn new(entries: Vec<DictEntry>) -> Self {
        Self { entries }
    }

    pub fn entries(&self) -> &[DictEntry] {
        &self.entries
    }

    /// Terms joined for use as an engine bias prompt (whisper initial_prompt).
    /// Kept short: long prompts degrade recognition and eat context.
    pub fn bias_prompt(&self, max_terms: usize) -> String {
        let terms: Vec<&str> = self
            .entries
            .iter()
            .filter(|e| e.enabled)
            .take(max_terms)
            .map(|e| e.term.as_str())
            .collect();
        if terms.is_empty() {
            String::new()
        } else {
            format!("Glossary: {}.", terms.join(", "))
        }
    }

    /// Post-transcription pass. Returns corrected text plus what changed.
    pub fn apply(&self, text: &str, fuzzy: bool) -> (String, Vec<AppliedCorrection>) {
        let text: String = text.nfc().collect();
        let mut corrections = Vec::new();
        let mut result = text.clone();

        // Pass 1: explicit correction pairs, longest source first so that
        // "cloud code pro" wins over "cloud code".
        let mut pairs: Vec<(&str, &str)> = self
            .entries
            .iter()
            .filter(|e| e.enabled)
            .flat_map(|e| e.corrections.iter().map(move |c| (c.as_str(), e.term.as_str())))
            .collect();
        pairs.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
        for (from, to) in pairs {
            result = replace_phrase(&result, from, to, &mut corrections);
        }

        // Pass 2: fuzzy single-token and phrase correction against terms.
        if fuzzy {
            result = self.fuzzy_pass(&result, &mut corrections);
        }

        (result, corrections)
    }

    fn fuzzy_pass(&self, text: &str, corrections: &mut Vec<AppliedCorrection>) -> String {
        let terms: Vec<&DictEntry> = self.entries.iter().filter(|e| e.enabled).collect();
        if terms.is_empty() {
            return text.to_string();
        }
        let tokens = tokenize_with_offsets(text);
        let mut edits: Vec<(usize, usize, String, String)> = Vec::new(); // (start, end, from, to)

        for entry in &terms {
            let term_words: Vec<&str> = entry.term.split_whitespace().collect();
            let n = term_words.len();
            if n == 0 {
                continue;
            }
            let term_joined = normalise_for_match(&entry.term);
            let mut i = 0;
            while i + n <= tokens.len() {
                let window = &tokens[i..i + n];
                // Skip windows overlapping an existing edit.
                let (w_start, w_end) = (window[0].1, window[n - 1].2);
                if edits.iter().any(|(s, e, _, _)| w_start < *e && *s < w_end) {
                    i += 1;
                    continue;
                }
                let spoken: String = window.iter().map(|(w, _, _)| *w).collect::<Vec<_>>().join(" ");
                let spoken_norm = normalise_for_match(&spoken);
                if spoken_norm == term_joined {
                    // Same word, maybe wrong casing: fix casing only.
                    if spoken != entry.term {
                        edits.push((w_start, w_end, spoken.clone(), entry.term.clone()));
                    }
                    i += n;
                    continue;
                }
                if spoken_norm.chars().count() >= FUZZY_MIN_LEN
                    && !window.iter().any(|(w, _, _)| is_common_word(&w.to_lowercase()))
                {
                    let score = strsim::jaro_winkler(&spoken_norm, &term_joined);
                    if score >= FUZZY_THRESHOLD {
                        edits.push((w_start, w_end, spoken.clone(), entry.term.clone()));
                        i += n;
                        continue;
                    }
                }
                i += 1;
            }
        }

        if edits.is_empty() {
            return text.to_string();
        }
        edits.sort_by_key(|(s, _, _, _)| *s);
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0;
        for (start, end, from, to) in edits {
            out.push_str(&text[cursor..start]);
            corrections.push(AppliedCorrection { from, to: to.clone(), at: start });
            out.push_str(&to);
            cursor = end;
        }
        out.push_str(&text[cursor..]);
        out
    }

    /// Warn when a term is dangerously close to a common English word, so the
    /// settings UI can flag likely false matches at entry time.
    pub fn false_match_warning(term: &str) -> Option<String> {
        let norm = normalise_for_match(term);
        for w in COMMON_WORDS {
            if strsim::jaro_winkler(&norm, w) >= FUZZY_THRESHOLD && norm != *w {
                return Some(format!(
                    "\"{term}\" is very close to the common word \"{w}\" and may auto-correct text you didn't intend. Consider adding explicit corrections instead."
                ));
            }
        }
        None
    }
}

/// Case-insensitive whole-phrase replacement, tolerant of hyphen/space variants.
fn replace_phrase(text: &str, from: &str, to: &str, corrections: &mut Vec<AppliedCorrection>) -> String {
    let from_norm = normalise_for_match(from);
    if from_norm.is_empty() {
        return text.to_string();
    }
    let tokens = tokenize_with_offsets(text);
    let from_words = from.split_whitespace().count().max(1);
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut i = 0;
    // Try both the exact word-count window and a 1-token window (hyphenated form).
    while i < tokens.len() {
        let mut matched = false;
        for n in [from_words, 1] {
            if i + n > tokens.len() {
                continue;
            }
            let window = &tokens[i..i + n];
            let spoken: String = window.iter().map(|(w, _, _)| *w).collect::<Vec<_>>().join(" ");
            if normalise_for_match(&spoken) == from_norm {
                let (start, end) = (window[0].1, window[n - 1].2);
                out.push_str(&text[cursor..start]);
                corrections.push(AppliedCorrection { from: spoken, to: to.to_string(), at: start });
                out.push_str(to);
                cursor = end;
                i += n;
                matched = true;
                break;
            }
        }
        if !matched {
            i += 1;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

/// Words with byte offsets, punctuation stripped from the edges but offsets
/// covering only the core word (so replacements keep surrounding punctuation).
fn tokenize_with_offsets(text: &str) -> Vec<(&str, usize, usize)> {
    let mut out = Vec::new();
    for (start, raw) in split_whitespace_indices(text) {
        let trimmed_start = raw.find(|c: char| c.is_alphanumeric()).unwrap_or(0);
        let trimmed_end = raw
            .rfind(|c: char| c.is_alphanumeric())
            .map(|p| p + raw[p..].chars().next().map(|c| c.len_utf8()).unwrap_or(1))
            .unwrap_or(raw.len());
        if trimmed_start < trimmed_end {
            out.push((&raw[trimmed_start..trimmed_end], start + trimmed_start, start + trimmed_end));
        }
    }
    out
}

fn split_whitespace_indices(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split_whitespace().map(move |w| {
        // Safe: split_whitespace yields subslices of text.
        let offset = w.as_ptr() as usize - text.as_ptr() as usize;
        (offset, w)
    })
}

/// Lowercase, hyphens/underscores collapsed to nothing, spaces collapsed —
/// so "Note Plan", "note-plan" and "noteplan" all match.
fn normalise_for_match(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn is_common_word(w: &str) -> bool {
    COMMON_WORDS.binary_search(&w).is_ok()
}

/// Sorted. The guard list for fuzzy correction: spoken tokens equal to any of
/// these are never fuzzy-replaced (explicit pairs still apply).
const COMMON_WORDS: &[&str] = &[
    "about", "after", "again", "all", "also", "and", "any", "are", "around", "back", "because",
    "been", "before", "being", "best", "better", "between", "both", "but", "call", "came", "can",
    "case", "check", "come", "could", "day", "did", "does", "down", "each", "end", "even", "every",
    "fact", "far", "few", "find", "first", "for", "form", "found", "from", "get", "give", "going",
    "good", "got", "great", "had", "hand", "has", "have", "her", "here", "him", "his", "home",
    "house", "how", "into", "its", "just", "keep", "kind", "know", "large", "last", "left", "life",
    "like", "line", "little", "long", "look", "made", "make", "man", "many", "may", "mean", "men",
    "might", "more", "most", "much", "must", "name", "need", "never", "new", "next", "not", "now",
    "off", "old", "once", "one", "only", "other", "our", "out", "over", "own", "part", "people",
    "place", "point", "put", "right", "said", "same", "saw", "say", "see", "seem", "she", "should",
    "show", "side", "since", "small", "some", "still", "such", "take", "than", "that", "the",
    "their", "them", "then", "there", "these", "they", "thing", "think", "this", "those", "three",
    "through", "time", "too", "two", "under", "until", "use", "used", "very", "want", "was", "way",
    "well", "went", "were", "what", "when", "where", "which", "while", "who", "why", "will",
    "with", "word", "work", "world", "would", "year", "you", "your",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(entries: Vec<(&str, Vec<&str>)>) -> Dictionary {
        Dictionary::new(
            entries
                .into_iter()
                .enumerate()
                .map(|(i, (term, corr))| DictEntry {
                    id: i as i64,
                    term: term.to_string(),
                    corrections: corr.into_iter().map(String::from).collect(),
                    auto_learned: false,
                    enabled: true,
                })
                .collect(),
        )
    }

    #[test]
    fn explicit_pair() {
        let d = dict(vec![("Claude Code", vec!["cloud code"])]);
        let (out, c) = d.apply("I opened cloud code this morning", true);
        assert_eq!(out, "I opened Claude Code this morning");
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn explicit_pair_case_insensitive() {
        let d = dict(vec![("Claude Code", vec!["cloud code"])]);
        let (out, _) = d.apply("Cloud Code is great", true);
        assert_eq!(out, "Claude Code is great");
    }

    #[test]
    fn fuzzy_misspelling_corrected() {
        let d = dict(vec![("farsiight", vec![])]);
        let (out, _) = d.apply("the farsight team shipped it", true);
        assert_eq!(out, "the farsiight team shipped it");
    }

    #[test]
    fn casing_fixed_for_exact_term() {
        let d = dict(vec![("NotePlan", vec![])]);
        let (out, _) = d.apply("open noteplan now", true);
        assert_eq!(out, "open NotePlan now");
    }

    #[test]
    fn hyphen_variant_matches() {
        let d = dict(vec![("NotePlan", vec![])]);
        let (out, _) = d.apply("open note-plan now", true);
        assert_eq!(out, "open NotePlan now");
    }

    #[test]
    fn common_word_never_fuzzy_replaced() {
        let d = dict(vec![("Chack", vec![])]);
        let (out, _) = d.apply("check the numbers", true);
        assert_eq!(out, "check the numbers");
    }

    #[test]
    fn unspoken_words_never_inserted() {
        let d = dict(vec![("Kubernetes", vec![])]);
        let (out, _) = d.apply("deploy the app today", true);
        assert_eq!(out, "deploy the app today");
    }

    #[test]
    fn punctuation_preserved_around_replacement() {
        let d = dict(vec![("Claude Code", vec!["cloud code"])]);
        let (out, _) = d.apply("Have you tried cloud code?", true);
        assert_eq!(out, "Have you tried Claude Code?");
    }

    #[test]
    fn fuzzy_disabled_leaves_misspellings() {
        let d = dict(vec![("farsiight", vec![])]);
        let (out, _) = d.apply("the farsight team", false);
        assert_eq!(out, "the farsight team");
    }

    #[test]
    fn bias_prompt_built() {
        let d = dict(vec![("Aysha", vec![]), ("farsiight", vec![])]);
        assert_eq!(d.bias_prompt(16), "Glossary: Aysha, farsiight.");
    }

    #[test]
    fn false_match_warning_fires() {
        assert!(Dictionary::false_match_warning("Chack").is_some());
        assert!(Dictionary::false_match_warning("Kubernetes").is_none());
    }
}
