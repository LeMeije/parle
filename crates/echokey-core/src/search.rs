//! Fuzzy matching for the history palette (Raycast/Alfred-grade), built on
//! nucleo-matcher (the Helix editor's matcher).

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use parking_lot::Mutex;

static MATCHER: Mutex<Option<Matcher>> = Mutex::new(None);

/// Score `haystack` against the user's `query`. None = no match.
/// Higher is better. Word-boundary and consecutive-run bonuses come from nucleo.
pub fn fuzzy_score(haystack: &str, query: &str) -> Option<u32> {
    let mut guard = MATCHER.lock();
    let matcher = guard.get_or_insert_with(|| Matcher::new(Config::DEFAULT));
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let hay = Utf32Str::new(haystack, &mut buf);
    pattern.score(hay, matcher)
}

/// Indices of matched characters, for highlight rendering in the UI.
pub fn fuzzy_indices(haystack: &str, query: &str) -> Option<Vec<u32>> {
    let mut guard = MATCHER.lock();
    let matcher = guard.get_or_insert_with(|| Matcher::new(Config::DEFAULT));
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let hay = Utf32Str::new(haystack, &mut buf);
    let mut indices = Vec::new();
    let score = pattern.indices(hay, matcher, &mut indices);
    score.map(|_| {
        indices.sort_unstable();
        indices.dedup();
        indices
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_subsequences() {
        assert!(fuzzy_score("Claude Code history", "clcode").is_some());
        assert!(fuzzy_score("unrelated text", "zzzqqq").is_none());
    }

    #[test]
    fn better_match_scores_higher() {
        let exact = fuzzy_score("deploy notes", "deploy").unwrap();
        let scattered = fuzzy_score("deep learning policy", "deploy").unwrap_or(0);
        assert!(exact > scattered);
    }

    #[test]
    fn indices_cover_query() {
        let idx = fuzzy_indices("Claude Code", "code").unwrap();
        assert!(!idx.is_empty());
    }
}
