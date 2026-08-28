//! Tier-1 deterministic cleanup. No ML, no network, always available.
//!
//! Behavioural contract: shared/formatter-test-vectors.json. Change the vectors
//! first, then make this pass. The Windows build runs the same vectors.
//!
//! Pipeline: tokenize (byte offsets into raw) -> dictated punctuation ->
//! stutter/self-correction trimming -> filler removal -> locale spelling ->
//! render (spacing, capitalisation, terminal punctuation, paragraphs).
//! Every removed token records a TrimmedSpan against the RAW text so the UI
//! can highlight and restore.

use crate::settings::CleanupSettings;
use crate::types::{Segment, TrimReason, TrimmedSpan};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, PartialEq)]
enum TokKind {
    Word,
    /// Attaches to the previous word: , . ! ? : ; …
    PunctLeft(String),
    NewLine,
    NewParagraph,
}

#[derive(Debug, Clone)]
struct Token {
    kind: TokKind,
    /// Original text (Word only), with any attached trailing punctuation kept.
    text: String,
    /// Byte range in the raw input; (0,0) for synthesised tokens.
    start: usize,
    end: usize,
    removed: Option<TrimReason>,
}

impl Token {
    fn word(text: &str, start: usize, end: usize) -> Self {
        Self { kind: TokKind::Word, text: text.to_string(), start, end, removed: None }
    }
    /// Lower-case core of a word token: surrounding punctuation stripped.
    fn core(&self) -> String {
        self.text
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
            .to_lowercase()
    }
    /// Trailing sentence punctuation attached to this word, if any.
    fn trailing_punct(&self) -> Option<char> {
        self.text.chars().last().filter(|c| ",.!?;:".contains(*c))
    }
}

pub struct FormatOutput {
    pub text: String,
    pub trimmed: Vec<TrimmedSpan>,
}

/// Format a raw transcript. `segments` (optional) provide timing for
/// pause-based paragraph breaks; raw must equal the joined segment texts.
pub fn format(raw: &str, segments: &[Segment], cfg: &CleanupSettings, locale: &str) -> FormatOutput {
    let raw: String = raw.nfc().collect();
    if !cfg.enabled {
        return FormatOutput { text: raw.trim().to_string(), trimmed: vec![] };
    }

    let mut tokens = tokenize(&raw);

    if cfg.paragraph_on_long_pause && segments.len() > 1 {
        insert_pause_paragraphs(&mut tokens, &raw, segments, cfg.paragraph_pause_ms);
    }
    if cfg.dictated_punctuation {
        convert_dictated_punctuation(&mut tokens);
    }
    if cfg.trim_self_corrections {
        trim_stutters(&mut tokens);
        trim_corrections(&mut tokens);
    }
    if cfg.remove_fillers {
        remove_fillers(&mut tokens, cfg.remove_hedges);
    }
    if cfg.locale_spelling && !locale.is_empty() {
        apply_locale_spelling(&mut tokens, locale);
    }

    let trimmed = collect_trims(&tokens, &raw);
    let text = render(&tokens, cfg);
    FormatOutput { text, trimmed }
}

// ---------------------------------------------------------------------------
// Tokenizer

fn tokenize(raw: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, ch) in raw.char_indices() {
        if ch == '\n' {
            if let Some(s) = start.take() {
                out.push(Token::word(&raw[s..i], s, i));
            }
            // Preserve explicit newlines in the source.
            out.push(Token { kind: TokKind::NewLine, text: String::new(), start: i, end: i + 1, removed: None });
        } else if ch.is_whitespace() {
            if let Some(s) = start.take() {
                out.push(Token::word(&raw[s..i], s, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push(Token::word(&raw[s..], s, raw.len()));
    }
    out
}

fn insert_pause_paragraphs(tokens: &mut Vec<Token>, raw: &str, segments: &[Segment], pause_ms: u64) {
    // Compute byte offsets where each segment starts inside `raw`, then insert a
    // paragraph break before the first token of a segment that follows a gap.
    let mut breaks: Vec<usize> = Vec::new();
    let mut cursor = 0usize;
    for pair in segments.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        // Find this segment's text going forward from the cursor.
        if let Some(pos) = raw[cursor..].find(a.text.trim()) {
            cursor += pos + a.text.trim().len();
        }
        if b.start_ms.saturating_sub(a.end_ms) >= pause_ms {
            breaks.push(cursor);
        }
    }
    if breaks.is_empty() {
        return;
    }
    let mut result: Vec<Token> = Vec::with_capacity(tokens.len() + breaks.len());
    let mut bi = 0;
    for t in tokens.drain(..) {
        while bi < breaks.len() && t.start >= breaks[bi] && matches!(t.kind, TokKind::Word) {
            result.push(Token {
                kind: TokKind::NewParagraph,
                text: String::new(),
                start: t.start,
                end: t.start,
                removed: None,
            });
            bi += 1;
        }
        result.push(t);
    }
    *tokens = result;
}

// ---------------------------------------------------------------------------
// Dictated punctuation

/// Multi-word commands first (longest match wins), then single words.
const PUNCT_CMDS: &[(&[&str], &str)] = &[
    (&["new", "paragraph"], "\u{2029}"),
    (&["new", "line"], "\u{2028}"),
    (&["full", "stop"], "."),
    (&["question", "mark"], "?"),
    (&["exclamation", "mark"], "!"),
    (&["exclamation", "point"], "!"),
    (&["dot", "dot", "dot"], "…"),
    (&["period"], "."),
    (&["comma"], ","),
    (&["colon"], ":"),
    (&["semicolon"], ";"),
    (&["ellipsis"], "…"),
];

/// Commands that render as a WORD-JOINING symbol with surrounding spaces kept
/// simple: "dash" -> " - " handled as PunctLeft would glue wrongly, so these
/// become standalone word tokens instead.
const WORD_SYMBOLS: &[(&str, &str)] = &[("dash", "-"), ("hyphen", "-")];

fn convert_dictated_punctuation(tokens: &mut Vec<Token>) {
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].removed.is_some() || tokens[i].kind != TokKind::Word {
            i += 1;
            continue;
        }
        // Escape: "literally comma" keeps the word "comma".
        if tokens[i].core() == "literally" {
            if let Some(next) = tokens.get(i + 1) {
                let nc = next.core();
                let is_cmd_word = PUNCT_CMDS.iter().any(|(words, _)| words.len() == 1 && words[0] == nc)
                    || WORD_SYMBOLS.iter().any(|(w, _)| *w == nc);
                if is_cmd_word {
                    // Drop "literally", keep the word literally.
                    tokens.remove(i);
                    i += 1;
                    continue;
                }
            }
        }
        // Standalone symbol words ("dash" -> "-").
        if tokens[i].trailing_punct().is_none() {
            let core = tokens[i].core();
            if let Some((_, sym)) = WORD_SYMBOLS.iter().find(|(w, _)| *w == core) {
                tokens[i].text = sym.to_string();
                i += 1;
                continue;
            }
        }
        let mut matched = false;
        for (words, sym) in PUNCT_CMDS {
            if word_run_matches(tokens, i, words) {
                let start = tokens[i].start;
                let end = tokens[i + words.len() - 1].end;
                // A dictated punctuation word only counts when it stands alone —
                // "comma" spoken mid-sentence as a noun usually carries context we
                // can't see, so we convert unconditionally only when the token has
                // no attached punctuation of its own.
                tokens.splice(
                    i..i + words.len(),
                    [match *sym {
                        "\u{2028}" => Token { kind: TokKind::NewLine, text: String::new(), start, end, removed: None },
                        "\u{2029}" => Token { kind: TokKind::NewParagraph, text: String::new(), start, end, removed: None },
                        s => Token { kind: TokKind::PunctLeft(s.to_string()), text: String::new(), start, end, removed: None },
                    }],
                );
                matched = true;
                break;
            }
        }
        if !matched {
            i += 1;
        }
    }
}

fn word_run_matches(tokens: &[Token], i: usize, words: &[&str]) -> bool {
    if i + words.len() > tokens.len() {
        return false;
    }
    for (k, w) in words.iter().enumerate() {
        let t = &tokens[i + k];
        if t.kind != TokKind::Word || t.removed.is_some() {
            return false;
        }
        if t.core() != *w {
            return false;
        }
        // All but the last command word must carry no attached punctuation
        // ("new. Line" is not a command).
        if k + 1 < words.len() && t.trailing_punct().is_some() {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Stutters: exact immediately-repeated word runs ("I think I think", "the the").

fn trim_stutters(tokens: &mut [Token]) {
    let idx: Vec<usize> = live_word_indices(tokens);
    let mut removed_any = true;
    while removed_any {
        removed_any = false;
        let idx: Vec<usize> = idx.iter().copied().filter(|&i| tokens[i].removed.is_none()).collect();
        for n in (1..=4usize).rev() {
            let mut k = 0;
            while k + 2 * n <= idx.len() {
                let first = &idx[k..k + n];
                let second = &idx[k + n..k + 2 * n];
                let all_match = first.iter().zip(second.iter()).all(|(&a, &b)| {
                    tokens[a].core() == tokens[b].core() && !tokens[a].core().is_empty()
                });
                // Don't collapse intentional doubles like "very very" or numbers.
                let intentional = n == 1 && INTENTIONAL_DOUBLES.contains(&tokens[first[0]].core().as_str());
                // Only treat as a stutter when the first occurrence carries no
                // sentence-ending punctuation ("Yes. Yes." is intentional).
                let first_ends_sentence = first
                    .iter()
                    .any(|&a| matches!(tokens[a].trailing_punct(), Some('.') | Some('!') | Some('?')));
                if all_match && !intentional && !first_ends_sentence {
                    for &a in first {
                        tokens[a].removed = Some(TrimReason::SelfCorrection);
                    }
                    removed_any = true;
                    break;
                }
                k += 1;
            }
            if removed_any {
                break;
            }
        }
    }
}

const INTENTIONAL_DOUBLES: &[&str] = &["very", "really", "no", "so", "many", "much", "far", "long"];

// ---------------------------------------------------------------------------
// Self-corrections: "X, no wait, Y" -> "Y" when X and Y are the same kind.

const CORRECTION_MARKERS: &[&[&str]] = &[
    &["no", "wait"],
    &["no", "actually"],
    &["actually", "no"],
    &["no", "sorry"],
    &["or", "rather"],
    &["i", "mean"],
    &["sorry"],
    &["wait"],
];

/// Markers that kill everything back to the previous sentence boundary.
const SENTENCE_KILL_MARKERS: &[&[&str]] = &[
    &["scratch", "that"],
    &["delete", "that"],
    &["strike", "that"],
];

fn trim_corrections(tokens: &mut [Token]) {
    // Sentence-kill markers first.
    let idx = live_word_indices(tokens);
    let mut k = 0;
    while k < idx.len() {
        for marker in SENTENCE_KILL_MARKERS {
            if marker_matches(tokens, &idx, k, marker) {
                // Remove from the previous sentence boundary through the marker.
                let mut j = k;
                while j > 0 {
                    let prev = &tokens[idx[j - 1]];
                    if matches!(prev.trailing_punct(), Some('.') | Some('!') | Some('?')) {
                        break;
                    }
                    j -= 1;
                }
                for &t in &idx[j..k + marker.len()] {
                    tokens[t].removed = Some(TrimReason::SelfCorrection);
                }
                break;
            }
        }
        k += 1;
    }

    // Same-kind replacement corrections.
    let idx = live_word_indices(tokens);
    let mut k = 0;
    while k < idx.len() {
        let mut advanced = false;
        for marker in CORRECTION_MARKERS {
            if !marker_matches(tokens, &idx, k, marker) {
                continue;
            }
            let after = k + marker.len();
            // Try phrase lengths 3..=1 on both sides of the marker.
            'outer: for n in (1..=3usize).rev() {
                if k < n || after + n > idx.len() {
                    continue;
                }
                let before = &idx[k - n..k];
                let replacement = &idx[after..after + n];
                let similar = before.iter().zip(replacement.iter()).all(|(&a, &b)| {
                    same_kind(&tokens[a].core(), &tokens[b].core())
                });
                if similar {
                    for &t in before.iter().chain(idx[k..after].iter()) {
                        tokens[t].removed = Some(TrimReason::SelfCorrection);
                    }
                    advanced = true;
                    break 'outer;
                }
            }
            // Bare "sorry"/"wait"/"i mean" with no similar pair: leave it alone —
            // it's probably real content ("Sorry I'm late").
            if advanced {
                break;
            }
        }
        k += 1;
        let _ = advanced;
    }
}

fn live_word_indices(tokens: &[Token]) -> Vec<usize> {
    tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == TokKind::Word && t.removed.is_none())
        .map(|(i, _)| i)
        .collect()
}

fn marker_matches(tokens: &[Token], idx: &[usize], k: usize, marker: &[&str]) -> bool {
    if k + marker.len() > idx.len() {
        return false;
    }
    marker
        .iter()
        .enumerate()
        .all(|(j, w)| tokens[idx[k + j]].core() == *w)
}

const WEEKDAYS: &[&str] = &["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
const MONTHS: &[&str] = &[
    "january", "february", "march", "april", "may", "june", "july", "august", "september",
    "october", "november", "december",
];
const NUMBER_WORDS: &[&str] = &[
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen",
    "nineteen", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    "hundred", "thousand", "million",
];

/// Are two words plausibly interchangeable (so "X marker Y" is a correction)?
fn same_kind(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() || a == b {
        return false;
    }
    let is_num = |s: &str| s.chars().all(|c| c.is_ascii_digit() || c == ':' || c == '.') && s.chars().any(|c| c.is_ascii_digit());
    (WEEKDAYS.contains(&a) && WEEKDAYS.contains(&b))
        || (MONTHS.contains(&a) && MONTHS.contains(&b))
        || (NUMBER_WORDS.contains(&a) && NUMBER_WORDS.contains(&b))
        || (is_num(a) && is_num(b))
        || strsim::jaro_winkler(a, b) > 0.84
}

// ---------------------------------------------------------------------------
// Fillers

const FILLERS: &[&str] = &["um", "uh", "uhm", "umm", "ah", "er", "erm", "hmm", "mhm", "mmm"];
const HEDGES: &[&[&str]] = &[&["you", "know"], &["i", "mean"], &["sort", "of"], &["kind", "of"], &["basically"]];

fn remove_fillers(tokens: &mut [Token], hedges: bool) {
    let n = tokens.len();
    for i in 0..n {
        if tokens[i].kind != TokKind::Word || tokens[i].removed.is_some() {
            continue;
        }
        if FILLERS.contains(&tokens[i].core().as_str()) {
            tokens[i].removed = Some(TrimReason::Filler);
        }
    }
    if hedges {
        let idx = live_word_indices(tokens);
        let mut k = 0;
        while k < idx.len() {
            for phrase in HEDGES {
                if marker_matches(tokens, &idx, k, phrase) {
                    // Hedge phrases at the very start of the text often carry
                    // meaning ("I mean it") — require a preceding word.
                    if *phrase == ["i", "mean"] && k == 0 {
                        continue;
                    }
                    for j in 0..phrase.len() {
                        tokens[idx[k + j]].removed = Some(TrimReason::Filler);
                    }
                    break;
                }
            }
            k += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Locale spelling (US -> AU/GB). Applied only when the user enables it.

/// (US, AU/GB) stems. Applied whole-word, case-preserved, plus common suffixes.
const US_TO_GB: &[(&str, &str)] = &[
    ("color", "colour"), ("colors", "colours"), ("colored", "coloured"), ("coloring", "colouring"),
    ("favorite", "favourite"), ("favorites", "favourites"), ("favor", "favour"), ("favors", "favours"),
    ("behavior", "behaviour"), ("behaviors", "behaviours"), ("neighbor", "neighbour"), ("neighbors", "neighbours"),
    ("honor", "honour"), ("honors", "honours"), ("labor", "labour"), ("flavor", "flavour"), ("flavors", "flavours"),
    ("humor", "humour"), ("rumor", "rumour"), ("rumors", "rumours"),
    ("organize", "organise"), ("organizes", "organises"), ("organized", "organised"), ("organizing", "organising"),
    ("organization", "organisation"), ("organizations", "organisations"),
    ("recognize", "recognise"), ("recognized", "recognised"), ("recognizes", "recognises"),
    ("realize", "realise"), ("realized", "realised"), ("realizes", "realises"), ("realizing", "realising"),
    ("apologize", "apologise"), ("apologized", "apologised"), ("analyze", "analyse"), ("analyzed", "analysed"),
    ("analyzing", "analysing"), ("prioritize", "prioritise"), ("prioritized", "prioritised"),
    ("optimize", "optimise"), ("optimized", "optimised"), ("optimizing", "optimising"),
    ("customize", "customise"), ("customized", "customised"), ("summarize", "summarise"), ("summarized", "summarised"),
    ("finalize", "finalise"), ("finalized", "finalised"), ("capitalize", "capitalise"),
    ("center", "centre"), ("centers", "centres"), ("centered", "centred"), ("meter", "metre"), ("meters", "metres"),
    ("liter", "litre"), ("liters", "litres"), ("theater", "theatre"), ("theaters", "theatres"),
    ("defense", "defence"), ("offense", "offence"), ("license", "licence"), ("pretense", "pretence"),
    ("traveling", "travelling"), ("traveled", "travelled"), ("traveler", "traveller"), ("travelers", "travellers"),
    ("canceled", "cancelled"), ("canceling", "cancelling"), ("modeling", "modelling"), ("modeled", "modelled"),
    ("labeled", "labelled"), ("labeling", "labelling"), ("catalog", "catalogue"), ("catalogs", "catalogues"),
    ("dialog", "dialogue"), ("dialogs", "dialogues"), ("gray", "grey"), ("program", "program"), // 'program' stays for software in AU
    ("aluminum", "aluminium"), ("checkbook", "chequebook"), ("check", "cheque"), // only via dictionary; see note
    ("jewelry", "jewellery"), ("pajamas", "pyjamas"), ("mustache", "moustache"), ("plow", "plough"),
    ("skeptical", "sceptical"), ("skeptic", "sceptic"), ("enrollment", "enrolment"), ("fulfill", "fulfil"),
    ("installment", "instalment"), ("judgment", "judgement"), ("acknowledgment", "acknowledgement"),
];

/// Ambiguous entries excluded from automatic conversion ("check" is usually a verb).
const LOCALE_EXCLUDE: &[&str] = &["check", "program"];

fn apply_locale_spelling(tokens: &mut [Token], locale: &str) {
    let to_gb = matches!(locale, "en-AU" | "en-GB" | "en-NZ" | "en-IE");
    if !to_gb {
        return;
    }
    for t in tokens.iter_mut() {
        if t.kind != TokKind::Word || t.removed.is_some() {
            continue;
        }
        let core = t.core();
        if LOCALE_EXCLUDE.contains(&core.as_str()) {
            continue;
        }
        if let Some((_, gb)) = US_TO_GB.iter().find(|(us, _)| *us == core) {
            t.text = replace_core_preserving_case(&t.text, &core, gb);
        }
    }
}

fn replace_core_preserving_case(original: &str, core_lower: &str, replacement: &str) -> String {
    // Find the core inside the original (it may carry punctuation around it).
    let lower = original.to_lowercase();
    if let Some(pos) = lower.find(core_lower) {
        let prefix = &original[..pos];
        let suffix = &original[pos + core_lower.len()..];
        let orig_core = &original[pos..pos + core_lower.len()];
        let cased = if orig_core.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            let mut c = replacement.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        } else {
            replacement.to_string()
        };
        format!("{prefix}{cased}{suffix}")
    } else {
        original.to_string()
    }
}

// ---------------------------------------------------------------------------
// Trim collection & rendering

fn collect_trims(tokens: &[Token], raw: &str) -> Vec<TrimmedSpan> {
    let mut out: Vec<TrimmedSpan> = Vec::new();
    for t in tokens {
        let Some(reason) = t.removed else { continue };
        // Merge with the previous span when contiguous (only whitespace between).
        if let Some(last) = out.last_mut() {
            if last.reason == reason
                && raw[last.end..t.start].chars().all(|c| c.is_whitespace())
                && t.start >= last.end
            {
                last.end = t.end;
                last.text = raw[last.start..last.end].to_string();
                continue;
            }
        }
        out.push(TrimmedSpan { start: t.start, end: t.end, text: t.text.clone(), reason });
    }
    out
}

fn render(tokens: &[Token], cfg: &CleanupSettings) -> String {
    let mut out = String::new();
    let mut capitalise_next = cfg.capitalise_sentences;
    let mut last_was_open_quote = false;

    for t in tokens {
        if t.removed.is_some() {
            continue;
        }
        match &t.kind {
            TokKind::NewLine => {
                trim_trailing_space(&mut out);
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                capitalise_next = cfg.capitalise_sentences;
            }
            TokKind::NewParagraph => {
                trim_trailing_space(&mut out);
                strip_trailing_comma(&mut out);
                if cfg.ensure_terminal_punctuation {
                    ensure_sentence_end(&mut out);
                }
                if !out.is_empty() {
                    while out.ends_with('\n') {
                        out.pop();
                    }
                    out.push_str("\n\n");
                }
                capitalise_next = cfg.capitalise_sentences;
            }
            TokKind::PunctLeft(p) => {
                trim_trailing_space(&mut out);
                // Collapse doubled punctuation ("hello,." -> "hello,").
                if out.ends_with([',', '.', '!', '?', ';', ':']) {
                    out.pop();
                }
                if out.is_empty() {
                    continue; // punctuation with nothing before it is dropped
                }
                out.push_str(p);
                out.push(' ');
                if matches!(p.as_str(), "." | "!" | "?" | "…") {
                    capitalise_next = cfg.capitalise_sentences;
                }
            }
            TokKind::Word => {
                let mut w = t.text.clone();
                if capitalise_next {
                    w = capitalise_first(&w);
                    capitalise_next = false;
                }
                if w == "i" || w.starts_with("i'") {
                    w = capitalise_first(&w);
                }
                if !out.is_empty() && !out.ends_with('\n') && !out.ends_with(' ') && !last_was_open_quote {
                    out.push(' ');
                }
                last_was_open_quote = w.ends_with(['"', '“', '(', '[']);
                if matches!(t.trailing_punct(), Some('.') | Some('!') | Some('?')) {
                    capitalise_next = cfg.capitalise_sentences;
                }
                out.push_str(&w);
            }
        }
    }

    trim_trailing_space(&mut out);
    let mut text = out.trim().to_string();
    if cfg.ensure_terminal_punctuation {
        ensure_sentence_end(&mut text);
    }
    text
}

fn trim_trailing_space(s: &mut String) {
    while s.ends_with(' ') {
        s.pop();
    }
}

fn strip_trailing_comma(s: &mut String) {
    while s.ends_with([',', ';', ':']) {
        s.pop();
    }
}

fn ensure_sentence_end(s: &mut String) {
    while s.ends_with(' ') {
        s.pop();
    }
    if let Some(c) = s.chars().last() {
        if c.is_alphanumeric() || matches!(c, ')' | ']' | '"' | '\'' | '”') {
            s.push('.');
        } else if c == ',' || c == ';' || c == ':' {
            s.pop();
            s.push('.');
        }
    }
}

fn capitalise_first(w: &str) -> String {
    let mut cs = w.chars();
    match cs.next() {
        Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::CleanupSettings;

    fn fmt(raw: &str) -> String {
        format(raw, &[], &CleanupSettings::default(), "").text
    }

    #[test]
    fn fillers_removed() {
        assert_eq!(fmt("um so I think, uh, we should ship it"), "So I think, we should ship it.");
    }

    #[test]
    fn weekday_correction() {
        assert_eq!(fmt("let's meet Thursday no actually Wednesday after lunch"), "Let's meet Wednesday after lunch.");
    }

    #[test]
    fn number_correction() {
        assert_eq!(fmt("send it at 3 no wait 4"), "Send it at 4.");
    }

    #[test]
    fn stutter_collapsed() {
        assert_eq!(fmt("I think I think we should go"), "I think we should go.");
    }

    #[test]
    fn intentional_double_kept() {
        assert_eq!(fmt("this is very very good"), "This is very very good.");
    }

    #[test]
    fn scratch_that_kills_sentence() {
        assert_eq!(fmt("send the report tomorrow. tell John scratch that tell Sarah about the launch"), "Send the report tomorrow. Tell Sarah about the launch.");
    }

    #[test]
    fn dictated_punctuation() {
        assert_eq!(fmt("hi dana comma hope you're well period"), "Hi dana, hope you're well.");
    }

    #[test]
    fn literal_escape() {
        assert_eq!(fmt("the word literally comma is overused"), "The word comma is overused.");
    }

    #[test]
    fn new_paragraph_command() {
        assert_eq!(fmt("first point new paragraph second point"), "First point.\n\nSecond point.");
    }

    #[test]
    fn sorry_without_pair_is_kept() {
        assert_eq!(fmt("sorry I'm late to this"), "Sorry I'm late to this.");
    }

    #[test]
    fn trims_reported_with_offsets() {
        let out = format("um hello there", &[], &CleanupSettings::default(), "");
        assert_eq!(out.text, "Hello there.");
        assert_eq!(out.trimmed.len(), 1);
        assert_eq!(out.trimmed[0].text, "um");
        assert_eq!(out.trimmed[0].start, 0);
        assert_eq!(out.trimmed[0].reason, TrimReason::Filler);
    }

    #[test]
    fn locale_spelling_au() {
        let mut cfg = CleanupSettings::default();
        cfg.locale_spelling = true;
        let out = format("my favorite color is gray", &[], &cfg, "en-AU");
        assert_eq!(out.text, "My favourite colour is grey.");
    }

    #[test]
    fn disabled_is_passthrough() {
        let mut cfg = CleanupSettings::default();
        cfg.enabled = false;
        let out = format("um hello there", &[], &cfg, "");
        assert_eq!(out.text, "um hello there");
        assert!(out.trimmed.is_empty());
    }

    #[test]
    fn pause_paragraphs() {
        let segs = vec![
            Segment { text: "First thought here.".into(), start_ms: 0, end_ms: 2000, confidence: 1.0 },
            Segment { text: "Second thought entirely.".into(), start_ms: 5000, end_ms: 7000, confidence: 1.0 },
        ];
        let raw = "First thought here. Second thought entirely.";
        let out = format(raw, &segs, &CleanupSettings::default(), "");
        assert_eq!(out.text, "First thought here.\n\nSecond thought entirely.");
    }

    #[test]
    fn i_capitalised() {
        assert_eq!(fmt("yes i think i'll go"), "Yes I think I'll go.");
    }

    #[test]
    fn existing_punctuation_not_doubled() {
        assert_eq!(fmt("Hello, world."), "Hello, world.");
    }
}
