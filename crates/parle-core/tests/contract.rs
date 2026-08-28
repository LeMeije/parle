//! Behavioural-contract runner: executes shared/*.json against this build.
//! The Windows build runs the exact same vectors — see docs/WINDOWS_HANDOFF.md.

use parle_core::dictionary::{DictEntry, Dictionary};
use parle_core::formatter;
use parle_core::settings::CleanupSettings;
use serde::Deserialize;

fn shared_path(file: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../shared")
        .join(file)
}

#[derive(Deserialize)]
struct FormatterVectors {
    cases: Vec<FormatterCase>,
}

#[derive(Deserialize)]
struct FormatterCase {
    name: String,
    input: String,
    expect: String,
    #[serde(default)]
    settings: Option<serde_json::Value>,
    #[serde(default)]
    locale: Option<String>,
}

#[test]
fn formatter_contract() {
    let raw = std::fs::read_to_string(shared_path("formatter-test-vectors.json")).unwrap();
    let vectors: FormatterVectors = serde_json::from_str(&raw).unwrap();
    let mut failures = Vec::new();
    for case in &vectors.cases {
        let mut cfg = serde_json::to_value(CleanupSettings::default()).unwrap();
        if let Some(overrides) = &case.settings {
            for (k, v) in overrides.as_object().unwrap() {
                cfg[k] = v.clone();
            }
        }
        let cfg: CleanupSettings = serde_json::from_value(cfg).unwrap();
        let locale = case.locale.as_deref().unwrap_or("");
        let out = formatter::format(&case.input, &[], &cfg, locale);
        if out.text != case.expect {
            failures.push(format!(
                "  {}: input={:?}\n    expect={:?}\n    got   ={:?}",
                case.name, case.input, case.expect, out.text
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} formatter contract case(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[derive(Deserialize)]
struct DictVectors {
    cases: Vec<DictCase>,
}

#[derive(Deserialize)]
struct DictCase {
    name: String,
    entries: Vec<DictCaseEntry>,
    input: String,
    expect: String,
    #[serde(default = "default_true")]
    fuzzy: bool,
}

#[derive(Deserialize)]
struct DictCaseEntry {
    term: String,
    #[serde(default)]
    corrections: Vec<String>,
}

fn default_true() -> bool {
    true
}

#[test]
fn dictionary_contract() {
    let raw = std::fs::read_to_string(shared_path("dictionary-test-vectors.json")).unwrap();
    let vectors: DictVectors = serde_json::from_str(&raw).unwrap();
    let mut failures = Vec::new();
    for case in &vectors.cases {
        let dict = Dictionary::new(
            case.entries
                .iter()
                .enumerate()
                .map(|(i, e)| DictEntry {
                    id: i as i64,
                    term: e.term.clone(),
                    corrections: e.corrections.clone(),
                    auto_learned: false,
                    enabled: true,
                })
                .collect(),
        );
        let (out, _) = dict.apply(&case.input, case.fuzzy);
        if out != case.expect {
            failures.push(format!(
                "  {}: input={:?}\n    expect={:?}\n    got   ={:?}",
                case.name, case.input, case.expect, out
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} dictionary contract case(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
