//! Deterministic content normalisation and language evidence for semantic analysis.
//! Pipeline: NFC, drop categories Cc/Cf/Cs, casefold, collapse whitespace.

use std::collections::BTreeMap;

use icu_normalizer::ComposingNormalizerBorrowed;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_general_category::{get_general_category, GeneralCategory as G};

use crate::content_area::{
    extract_area_text, resolve_content_area, AUTO_STRATEGIES, DEFAULT_EXCLUDE_TAGS,
    TEXT_EXCLUDED_TAGS,
};
use crate::dom::Soup;
use crate::py_str::is_py_space;
use crate::types::{ContentAreaConfig, Json, Result};

/// Normaliser version tag.
pub const NORMALIZER_VERSION: &str = "content_normalizer.v1";
/// Minimum letters before a script claim is made.
pub const MIN_SCRIPT_LETTERS: usize = 20;
/// Share a script needs to be called dominant.
pub const DOMINANT_SCRIPT_SHARE: f64 = 0.85;
/// Latin code-point ranges (inclusive).
pub const LATIN_RANGES: [(u32, u32); 6] = [
    (65, 122),
    (192, 591),
    (7680, 7935),
    (11360, 11391),
    (42784, 43007),
    (43824, 43887),
];
/// Cyrillic code-point ranges (inclusive).
pub const CYRILLIC_RANGES: [(u32, u32); 4] =
    [(1024, 1327), (7296, 7304), (11744, 11775), (42560, 42655)];

/// The document's own `<html lang>` claim.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredLanguage {
    /// As written, `""` when absent.
    pub declared: String,
    /// Primary subtag, lower-cased, `""` when absent.
    pub declared_primary: String,
}

/// Letter-script profile of normalised text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptEvidence {
    /// `latin`, `cyrillic`, `mixed`, `other` or `undetermined`.
    pub script: String,
    /// Letters counted.
    pub script_letters: u64,
    /// `latin`/`cyrillic`/`other` -> share (4 places).
    pub script_shares: BTreeMap<String, f64>,
}

/// Result of [`normalize_document`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedDocument {
    /// Normalised text.
    pub text: String,
    /// Strategy name from content-area resolution.
    pub content_area_strategy: String,
    /// Declared language merged with script evidence.
    pub language: Json,
    /// SHA-256 of the HTML string consumed.
    pub input_sha256: String,
    /// SHA-256 of the normalised text.
    pub normalized_sha256: String,
    /// Characters in `text`.
    pub text_chars: u64,
    /// `\w{2,}` tokens in `text`.
    pub text_tokens: u64,
}

/// Input of [`prepare_items`] / [`manifest_entry`]: an inline `{url, html}` item or a scan
/// record. Open-ended on purpose (loosely typed JSON object); known keys are read by name.
pub type Item = crate::types::JsonMap;

/// Canonical normalised form under `content_normalizer.v1`.
pub fn normalize_text(text: &str) -> String {
    let nfc = ComposingNormalizerBorrowed::new_nfc().normalize(text);
    // Whitespace controls survive the removal: they are word boundaries.
    let kept: String = nfc
        .chars()
        .filter(|&c| {
            is_py_space(c)
                || !matches!(
                    get_general_category(c),
                    G::Control | G::Format | G::Surrogate
                )
        })
        .collect();
    let folded = caseless::default_case_fold_str(&kept);
    folded
        .split(is_py_space)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Letter test for one code point: categories Lu, Ll, Lt, Lm and Lo.
pub fn py_isalpha(c: char) -> bool {
    matches!(
        get_general_category(c),
        G::UppercaseLetter
            | G::LowercaseLetter
            | G::TitlecaseLetter
            | G::ModifierLetter
            | G::OtherLetter
    )
}

/// Word-character test (Unicode): letters, numbers (Nd, Nl, No) and underscore.
pub fn py_is_word(c: char) -> bool {
    c == '_'
        || py_isalpha(c)
        || matches!(
            get_general_category(c),
            G::DecimalNumber | G::LetterNumber | G::OtherNumber
        )
}

/// Script family of one letter: `latin`, `cyrillic` or `other`.
fn script_of(c: char) -> usize {
    let cp = c as u32;
    if LATIN_RANGES.iter().any(|&(lo, hi)| (lo..=hi).contains(&cp)) {
        0
    } else if CYRILLIC_RANGES
        .iter()
        .any(|&(lo, hi)| (lo..=hi).contains(&cp))
    {
        1
    } else {
        2
    }
}

/// Round to `ndigits` decimals, ties to even on the exact binary value.
fn py_round(value: f64, ndigits: usize) -> f64 {
    format!("{value:.ndigits$}").parse().unwrap_or(value)
}

/// Lower-case hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Number of `\w{2,}` matches: maximal runs of word characters that are at least two long.
fn word_token_count(text: &str) -> u64 {
    let mut count = 0;
    let mut run = 0usize;
    for c in text.chars() {
        if py_is_word(c) {
            run += 1;
        } else {
            count += u64::from(run >= 2);
            run = 0;
        }
    }
    count + u64::from(run >= 2)
}

/// Letter-script profile of already-normalised text.
pub fn script_evidence(text: &str) -> ScriptEvidence {
    let mut counts = [0u64; 3];
    for c in text.chars().filter(|&c| py_isalpha(c)) {
        counts[script_of(c)] += 1;
    }
    let letters: u64 = counts.iter().sum();
    let names = ["latin", "cyrillic", "other"];
    let shares: [f64; 3] = if letters > 0 {
        counts.map(|n| py_round(n as f64 / letters as f64, 4))
    } else {
        [0.0; 3]
    };
    let script = if (letters as usize) < MIN_SCRIPT_LETTERS {
        "undetermined"
    } else {
        // The first maximal entry wins a tie.
        let mut dominant = 0;
        for i in 1..3 {
            if counts[i] > counts[dominant] {
                dominant = i;
            }
        }
        if counts[dominant] as f64 / letters as f64 >= DOMINANT_SCRIPT_SHARE {
            names[dominant]
        } else if shares[2] >= 0.5 {
            "other"
        } else {
            "mixed"
        }
    };
    ScriptEvidence {
        script: script.to_owned(),
        script_letters: letters,
        script_shares: names
            .iter()
            .zip(shares)
            .map(|(name, share)| ((*name).to_owned(), share))
            .collect::<BTreeMap<_, _>>(),
    }
}

/// The document's `<html lang>` claim as written plus its primary subtag.
/// Also called by `parser::parse_html`.
pub fn declared_language(soup: &Soup) -> DeclaredLanguage {
    let raw = crate::dom::find(soup, "html")
        .and_then(|html| html.value().attr("lang"))
        .map_or("", crate::py_str::py_strip)
        .to_owned();
    let primary = if raw.is_empty() {
        String::new()
    } else {
        let first = raw.split('-').next().unwrap_or("");
        caseless::default_case_fold_str(crate::py_str::py_strip(first))
    };
    DeclaredLanguage {
        declared: raw,
        declared_primary: primary,
    }
}

/// Normalise one decoded HTML document's content region.
pub fn normalize_document(
    html: &str,
    content_area: Option<&ContentAreaConfig>,
) -> Result<NormalizedDocument> {
    let soup = crate::dom::parse_document(html);
    let area = resolve_content_area(&soup, content_area)?;
    let text = normalize_text(&extract_area_text(&area));
    let mut language = match serde_json::to_value(declared_language(&soup))? {
        Json::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    if let Json::Object(evidence) = serde_json::to_value(script_evidence(&text))? {
        language.extend(evidence);
    }
    Ok(NormalizedDocument {
        content_area_strategy: area.strategy.clone(),
        language: Json::Object(language),
        input_sha256: sha256_hex(html.as_bytes()),
        normalized_sha256: sha256_hex(text.as_bytes()),
        text_chars: text.chars().count() as u64,
        text_tokens: word_token_count(&text),
        text,
    })
}

/// The recorded policy a normalised manifest was produced under.
pub fn normalization_policy(content_area: Option<&ContentAreaConfig>) -> Json {
    let configured = content_area
        .and_then(|config| serde_json::to_value(config).ok())
        .unwrap_or_else(|| Json::Object(serde_json::Map::new()));
    let mut selection_order = vec!["include_selector", "root_selector"];
    selection_order.extend(AUTO_STRATEGIES.iter().map(|(_, strategy)| *strategy));
    selection_order.push("body");
    serde_json::json!({
        "normalizer_version": NORMALIZER_VERSION,
        "text_pipeline": [
            "resolve_content_area",
            "extract_area_text",
            "unicode_nfc",
            "remove_categories_Cc_Cf_Cs",
            "casefold",
            "collapse_whitespace_runs",
        ],
        "content_area": {
            "configured": configured,
            "selection_order": selection_order,
            "default_exclude_tags": DEFAULT_EXCLUDE_TAGS,
            "non_text_tags": TEXT_EXCLUDED_TAGS,
        },
        "language": {
            "declared": "the document's own <html lang> attribute, as written",
            "detected": "letter-script shares over the normalized text",
            "min_script_letters": MIN_SCRIPT_LETTERS,
            "dominant_script_share": DOMINANT_SCRIPT_SHARE,
        },
    })
}

/// Truthiness of a JSON value, as the manifest code tests it.
fn truthy(value: Option<&Json>) -> bool {
    match value {
        None | Some(Json::Null) => false,
        Some(Json::Bool(flag)) => *flag,
        Some(Json::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
        Some(Json::String(text)) => !text.is_empty(),
        Some(Json::Array(items)) => !items.is_empty(),
        Some(Json::Object(map)) => !map.is_empty(),
    }
}

/// The first truthy value, else the fallback.
fn or_else_truthy(item: &Item, keys: &[&str]) -> Json {
    keys.iter()
        .find(|key| truthy(item.get(**key)))
        .and_then(|key| item.get(*key).cloned())
        .unwrap_or_else(|| Json::String(String::new()))
}

/// Plain-text form of a manifest value (string, bool, integer, null).
fn py_str_of(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        Json::Bool(true) => "True".to_owned(),
        Json::Bool(false) => "False".to_owned(),
        Json::Number(number) => number.to_string(),
        Json::Null => "None".to_owned(),
        other => other.to_string(),
    }
}

/// Public per-document manifest row: hashes and policy fields, never the text.
pub fn manifest_entry(item: &Item) -> Json {
    let state = if truthy(item.get("state")) {
        item.get("state").cloned().unwrap_or(Json::Null)
    } else if truthy(item.get("text")) {
        Json::String("normalized".to_owned())
    } else {
        Json::String("empty".to_owned())
    };
    let reason = if truthy(item.get("reason")) {
        py_str_of(item.get("reason").unwrap_or(&Json::Null))
    } else {
        String::new()
    };
    let field = |key: &str| item.get(key).cloned().unwrap_or(Json::Null);
    serde_json::json!({
        "url": or_else_truthy(item, &["url", "id"]),
        "state": state,
        "reason": reason,
        "representation": field("representation"),
        "indexable": field("indexable"),
        "content_area_strategy": field("content_area_strategy"),
        "language": field("language"),
        "body_sha256": field("body_sha256"),
        "input_sha256": field("input_sha256"),
        "normalized_sha256": field("normalized_sha256"),
        "text_chars": field("text_chars"),
        "text_tokens": field("text_tokens"),
    })
}

/// Normalise inline `{url, html}` items into the record shape a scan yields; an item without a
/// usable html string becomes an explicit `unavailable` entry.
pub fn prepare_items(
    items: &[Json],
    content_area: Option<&ContentAreaConfig>,
) -> Result<Vec<Json>> {
    let mut prepared = Vec::with_capacity(items.len());
    for value in items {
        let Some(item) = value.as_object() else {
            prepared.push(serde_json::json!({
                "url": "",
                "state": "unavailable",
                "reason": "item is not an object",
            }));
            continue;
        };
        let url = py_str_of(&or_else_truthy(item, &["url", "id"]));
        let html = match item.get("html") {
            Some(Json::String(text)) if !crate::py_str::py_strip(text).is_empty() => text,
            _ => {
                prepared.push(serde_json::json!({
                    "url": url,
                    "state": "unavailable",
                    "reason": "item supplies no html body",
                }));
                continue;
            }
        };
        let field = |key: &str| item.get(key).cloned().unwrap_or(Json::Null);
        let indexable = match item.get("indexable") {
            Some(Json::Bool(flag)) => Json::Bool(*flag),
            _ => Json::Null,
        };
        let document = normalize_document(html, content_area)?;
        let mut row = match serde_json::to_value(&document)? {
            Json::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        row.insert("id".to_owned(), Json::String(url.clone()));
        row.insert("url".to_owned(), Json::String(url));
        row.insert("state".to_owned(), Json::Null);
        row.insert("representation".to_owned(), field("representation"));
        row.insert("indexable".to_owned(), indexable);
        row.insert("body_sha256".to_owned(), field("body_sha256"));
        prepared.push(Json::Object(row));
    }
    Ok(prepared)
}
