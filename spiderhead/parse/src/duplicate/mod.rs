//! Near-duplicate pages via SimHash + LSH with template damping and complete-linkage groups.
//! Pure. Hash values are stable across runs: FNV-1a 64 over UTF-8, banded LSH, SHA-1 content hash.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use sha1::{Digest, Sha1};
use unicode_general_category::{get_general_category, GeneralCategory};

use crate::types::{ParseError, Result};

mod complete_linkage_groups;
pub use complete_linkage_groups::*;

/// Default similarity threshold.
pub const DEFAULT_THRESHOLD: f64 = 0.92;

/// One input document: `{"id","text","indexable"?}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DuplicateItem {
    /// URL or any stable key.
    #[serde(default)]
    pub id: String,
    /// Text to compare.
    #[serde(default)]
    pub text: String,
    /// Default true; non-indexable items are dropped when `only_indexable`.
    #[serde(default = "yes")]
    pub indexable: bool,
}

fn yes() -> bool {
    true
}

/// A pair inside a cluster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DuplicatePair {
    /// First id.
    pub a: String,
    /// Second id.
    pub b: String,
    /// Rounded to 4 places.
    pub similarity: f64,
}

/// A complete-linkage near-duplicate cluster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DuplicateCluster {
    /// Member ids.
    pub members: Vec<String>,
    /// All pairs.
    pub pairs: Vec<DuplicatePair>,
    /// Lowest pair similarity.
    pub min_similarity: f64,
}

/// A group of exact duplicates (same content hash).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExactDuplicateGroup {
    /// SHA-1 content hash.
    pub hash: String,
    /// Member ids.
    pub members: Vec<String>,
}

/// Result of [`find_duplicates`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DuplicateReport {
    /// Always true.
    pub ok: bool,
    /// Compared documents.
    pub count: u64,
    /// Threshold used (absent when the input is empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    /// Near-duplicate clusters, best first.
    pub clusters: Vec<DuplicateCluster>,
    /// Exact-duplicate groups.
    pub exact_duplicates: Vec<ExactDuplicateGroup>,
    /// LSH candidate pairs compared.
    pub candidate_pairs_checked: u64,
    /// Dropped: non-indexable.
    pub excluded_non_indexable: u64,
    /// Dropped: no id.
    pub excluded_no_id: u64,
    /// Dropped: no usable text.
    pub excluded_no_text: u64,
    /// Dropped: repeated id.
    pub excluded_duplicate_id: u64,
    /// False when no pair was compared (unmeasured, not absent).
    pub measured: bool,
    /// Present when unmeasured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// id -> fingerprint, only with `with_fingerprints`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprints: Option<BTreeMap<String, u64>>,
}

/// Options of [`find_duplicates`] (`Default` = the default settings, plus a work budget).
#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateOptions {
    /// Similarity threshold in `[0, 1]`.
    pub threshold: f64,
    /// Shingle width.
    pub k: usize,
    /// Include fingerprints in the report.
    pub with_fingerprints: bool,
    /// Drop non-indexable items first.
    pub only_indexable: bool,
    /// Work budget: maximum total shingles (`None` = unbounded).
    pub max_shingles: Option<usize>,
    /// Work budget: maximum candidate comparisons. The default is
    /// [`DEFAULT_MAX_CANDIDATE_COMPARISONS`] because thresholds under ~0.5 collapse the LSH
    /// bands into near-single-bit buckets, which is quadratic on scan-size inputs.
    /// `None` removes the cap.
    pub max_candidate_comparisons: Option<usize>,
}

/// Default cap on candidate comparisons: far above any real scan's candidate set, below the
/// quadratic blow-up a degenerate band split produces. Hamming similarity is a few nanoseconds
/// per pair, so this bound does not change the outcome of a healthy run.
pub const DEFAULT_MAX_CANDIDATE_COMPARISONS: usize = 4_000_000;

impl Default for DuplicateOptions {
    fn default() -> Self {
        Self {
            threshold: DEFAULT_THRESHOLD,
            k: 3,
            with_fingerprints: false,
            only_indexable: true,
            max_shingles: None,
            max_candidate_comparisons: Some(DEFAULT_MAX_CANDIDATE_COMPARISONS),
        }
    }
}

/// Word character: alphanumeric (letters and numbers) or `_`.
pub fn is_word_char(c: char) -> bool {
    use GeneralCategory::*;
    c == '_'
        || matches!(
            get_general_category(c),
            UppercaseLetter
                | LowercaseLetter
                | TitlecaseLetter
                | ModifierLetter
                | OtherLetter
                | DecimalNumber
                | LetterNumber
                | OtherNumber
        )
}

/// Code points that gained a lowercase mapping in newer Unicode versions (Latin Extended-D
/// additions and Beria Erfe capitals). They are left unchanged, and they are never word
/// characters.
fn newer_than_python(c: char) -> bool {
    matches!(
        c,
        '\u{A7CE}' | '\u{A7D2}' | '\u{A7D4}' | '\u{16EA0}'..='\u{16EB8}'
    )
}

/// Lower-case a string the way `str.lower()` does.
fn py_lowercase(text: &str) -> String {
    if text.chars().any(newer_than_python) {
        let masked: String = text
            .chars()
            .map(|c| if newer_than_python(c) { '\u{FFFF}' } else { c })
            .collect();
        return masked.to_lowercase();
    }
    text.to_lowercase()
}

/// Lowercase alphanumeric tokens of at least two characters.
pub fn tokenize(text: &str) -> Vec<String> {
    let lowered = py_lowercase(text);
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut length = 0usize;
    for c in lowered.chars() {
        if is_word_char(c) {
            current.push(c);
            length += 1;
        } else {
            if length >= 2 {
                tokens.push(std::mem::take(&mut current));
            }
            current.clear();
            length = 0;
        }
    }
    if length >= 2 {
        tokens.push(current);
    }
    tokens
}

/// Overlapping k-shingles, or one shingle when fewer than k tokens.
pub fn shingles(tokens: &[String], k: usize) -> Vec<Vec<String>> {
    if tokens.is_empty() {
        return Vec::new();
    }
    if tokens.len() <= k {
        return vec![tokens.to_vec()];
    }
    (0..=tokens.len() - k)
        .map(|i| tokens[i..i + k].to_vec())
        .collect()
}

/// Deterministic 64-bit FNV-1a over the UTF-8 bytes.
pub fn fnv1a_64(data: &str) -> u64 {
    let mut hash: u64 = 0xCBF2_9CE4_8422_2325;
    for byte in data.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// 64-bit SimHash of precomputed shingles; a shingle in `silenced` casts no vote.
fn weighted_simhash(pieces: &[Vec<String>], silenced: Option<&HashSet<&[String]>>) -> u64 {
    if pieces.is_empty() {
        return 0;
    }
    let mut votes = [0.0f64; 64];
    for piece in pieces {
        let weight = match silenced {
            Some(set) if set.contains(piece.as_slice()) => 0.0,
            _ => 1.0,
        };
        if weight == 0.0 {
            continue;
        }
        let hash = fnv1a_64(&piece.join(" "));
        for (i, vote) in votes.iter_mut().enumerate() {
            if (hash >> i) & 1 == 1 {
                *vote += weight;
            } else {
                *vote -= weight;
            }
        }
    }
    votes
        .iter()
        .enumerate()
        .fold(0u64, |fp, (i, v)| if *v > 0.0 { fp | (1 << i) } else { fp })
}

/// 64-bit SimHash; identical texts give the same hash.
pub fn simhash(text: &str, k: usize) -> u64 {
    weighted_simhash(&shingles(&tokenize(text), k), None)
}

/// Hamming distance of two fingerprints.
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// `1 - hamming / 64`.
pub fn similarity(a: u64, b: u64) -> f64 {
    1.0 - f64::from(hamming(a, b)) / 64.0
}

/// SHA-1 hex digest of `text`, for exact-duplicate grouping.
pub fn content_hash(text: &str) -> String {
    let digest = Sha1::digest(text.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// A shingle in at least this fraction of the corpus is the site's template.
const TEMPLATE_DOC_FREQ_RATIO: f64 = 0.9;
/// Below this many documents "shared by nearly everyone" is not a meaningful statistic.
const MIN_DOCS_FOR_TEMPLATE_FILTER: usize = 20;
const UNMEASURED_REASON: &str =
    "no pair of comparable documents was compared; duplicate content is unmeasured, not absent";

/// Fingerprint value of each LSH band.
fn band_keys(fp: u64, bands: usize, band_bits: usize) -> Vec<u64> {
    let mask = if band_bits >= 64 {
        u64::MAX
    } else {
        (1u64 << band_bits) - 1
    };
    (0..bands)
        .map(|i| {
            let shift = i * band_bits;
            if shift >= 64 {
                0
            } else {
                (fp >> shift) & mask
            }
        })
        .collect()
}

/// Shingles occurring in at least [`TEMPLATE_DOC_FREQ_RATIO`] of a large enough corpus.
fn template_shingles(lists: &[&Vec<Vec<String>>]) -> HashSet<Vec<String>> {
    let n = lists.len();
    if n < MIN_DOCS_FOR_TEMPLATE_FILTER {
        return HashSet::new();
    }
    let mut frequency: HashMap<&[String], usize> = HashMap::new();
    for list in lists {
        let unique: HashSet<&[String]> = list.iter().map(Vec::as_slice).collect();
        for piece in unique {
            *frequency.entry(piece).or_insert(0) += 1;
        }
    }
    let cutoff = n as f64 * TEMPLATE_DOC_FREQ_RATIO;
    frequency
        .into_iter()
        .filter(|(_, df)| *df as f64 >= cutoff)
        .map(|(piece, _)| piece.to_vec())
        .collect()
}
