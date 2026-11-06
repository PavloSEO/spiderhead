//! Optional, local-first semantic similarity over normalized retained content.
//!
//! No model ships here and lexical overlap never becomes a semantic score: a
//! caller supplies an embedding adapter and explicitly declares whether it is
//! local or may transmit text to a provider. Only bounded vectors persist in a
//! local SQLite cache, and the returned evidence keeps semantic groups distinct
//! from exact and near-duplicate findings.

use std::collections::HashMap;
use std::path::Path;

use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use spiderhead_core::common::canonical_json;

/// The document bound of one analysis.
pub const MAX_DOCUMENTS: usize = 10_000;
/// The pairwise-comparison bound before the analysis reports a partial budget.
pub const MAX_CANDIDATE_COMPARISONS: i64 = 250_000;
/// The accepted embedding dimensionality bound.
pub const MAX_VECTOR_DIMENSIONS: usize = 8_192;
/// The cache identity version; changing it invalidates every stored vector.
pub const CACHE_SCHEMA_VERSION: &str = "semantic_cache.v1";

/// The adapter boundary failures: a validation error or a permission refusal.
#[derive(Debug, thiserror::Error)]
pub enum SemanticError {
    /// A Python `ValueError` at this boundary.
    #[error("{0}")]
    Value(String),
    /// A Python `PermissionError` at this boundary.
    #[error("{0}")]
    Permission(String),
}

/// The small model boundary; callers own model loading and provider credentials.
pub trait EmbeddingAdapter {
    /// The declaration used for cache identity and data-transfer provenance.
    fn describe(&self) -> Value;
    /// Embed the supplied texts.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>, SemanticError>;
    /// Whether embedding vectors must be supplied per document (no model call).
    fn is_declared(&self) -> bool {
        false
    }
}

/// A caller-provided local embedder, declared without importing an ML runtime.
pub struct LocalEmbeddingAdapter<F> {
    model_id: String,
    model_version: String,
    model_path: String,
    embedder: F,
    settings: Option<Value>,
}

impl<F> LocalEmbeddingAdapter<F>
where
    F: Fn(&[String]) -> Vec<Vec<f64>>,
{
    /// Declare a local embedder with its model identity.
    pub fn new(
        model_id: &str,
        model_version: &str,
        model_path: &str,
        embedder: F,
        settings: Option<Value>,
    ) -> Self {
        Self {
            model_id: model_id.to_owned(),
            model_version: model_version.to_owned(),
            model_path: model_path.to_owned(),
            embedder,
            settings,
        }
    }
}

impl<F> EmbeddingAdapter for LocalEmbeddingAdapter<F>
where
    F: Fn(&[String]) -> Vec<Vec<f64>>,
{
    fn describe(&self) -> Value {
        json!({
            "kind": "local",
            "model_id": self.model_id,
            "model_version": self.model_version,
            "model_path": self.model_path,
            "settings": self.settings.clone().unwrap_or_else(|| json!({})),
            "data_transfer": "none",
            "paid": false,
        })
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>, SemanticError> {
        Ok((self.embedder)(texts))
    }
}

/// An explicit wrapper for a separately configured provider integration.
///
/// It makes no request by itself; `external_authorized` is intentionally false
/// unless the caller passes a literal `true`, so a provider cannot receive
/// retained bodies merely because a provider-shaped adapter was constructed.
pub struct ProviderEmbeddingAdapter<F> {
    provider: String,
    model_id: String,
    model_version: String,
    embedder: F,
    settings: Option<Value>,
    allow_external: Value,
    paid: Value,
}

impl<F> ProviderEmbeddingAdapter<F>
where
    F: Fn(&[String]) -> Vec<Vec<f64>>,
{
    /// Declare a provider embedder that is not authorized by default.
    pub fn new(
        provider: &str,
        model_id: &str,
        model_version: &str,
        embedder: F,
        settings: Option<Value>,
    ) -> Self {
        Self {
            provider: provider.to_owned(),
            model_id: model_id.to_owned(),
            model_version: model_version.to_owned(),
            embedder,
            settings,
            allow_external: Value::Bool(false),
            paid: Value::Bool(false),
        }
    }

    /// Set the literal external-authorization flag (only `true` counts).
    pub fn with_external_authorized(mut self, authorized: Value) -> Self {
        self.allow_external = authorized;
        self
    }

    /// Set the paid flag of the declaration.
    pub fn with_paid(mut self, paid: Value) -> Self {
        self.paid = paid;
        self
    }

    fn authorized(&self) -> bool {
        self.allow_external == Value::Bool(true)
    }
}

impl<F> EmbeddingAdapter for ProviderEmbeddingAdapter<F>
where
    F: Fn(&[String]) -> Vec<Vec<f64>>,
{
    fn describe(&self) -> Value {
        json!({
            "kind": "provider",
            "provider": self.provider,
            "model_id": self.model_id,
            "model_version": self.model_version,
            "settings": self.settings.clone().unwrap_or_else(|| json!({})),
            "data_transfer": "external",
            "paid": self.paid,
            "external_authorized": self.allow_external,
        })
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>, SemanticError> {
        if !self.authorized() {
            return Err(SemanticError::Permission(
                "external semantic provider is not explicitly authorized".to_owned(),
            ));
        }
        Ok((self.embedder)(texts))
    }
}

/// Vectors supplied by the calling agent, without loading or calling a model.
///
/// The declaration is still required for cache invalidation and data-transfer
/// provenance; the document analysis consumes each document's `embedding`
/// field before ever reaching [`EmbeddingAdapter::embed`].
pub struct DeclaredEmbeddingAdapter {
    declaration: Value,
}

impl DeclaredEmbeddingAdapter {
    /// Wrap the caller's adapter declaration.
    pub fn new(declaration: &Value) -> Self {
        Self {
            declaration: declaration.clone(),
        }
    }
}

impl EmbeddingAdapter for DeclaredEmbeddingAdapter {
    fn describe(&self) -> Value {
        self.declaration.clone()
    }

    fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f64>>, SemanticError> {
        Err(SemanticError::Value(
            "an embedding is missing from the supplied semantic input".to_owned(),
        ))
    }

    fn is_declared(&self) -> bool {
        true
    }
}

/// The reproducible identity that scopes a cached embedding.
pub fn adapter_identity(adapter: &dyn EmbeddingAdapter) -> Result<Value, SemanticError> {
    let declared = adapter.describe();
    let required = [
        "kind",
        "model_id",
        "model_version",
        "settings",
        "data_transfer",
    ];
    let object = declared.as_object().ok_or_else(|| {
        SemanticError::Value("embedding adapter declaration is missing: ".to_owned())
    })?;
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|name| !object.contains_key(*name))
        .collect();
    if !missing.is_empty() {
        return Err(SemanticError::Value(format!(
            "embedding adapter declaration is missing: {}",
            missing.join(", ")
        )));
    }
    let kind = &object["kind"];
    if kind != "local" && kind != "provider" {
        return Err(SemanticError::Value(
            "embedding adapter kind must be local or provider".to_owned(),
        ));
    }
    let kind = kind.as_str().unwrap_or_default();
    if !object["settings"].is_object() {
        return Err(SemanticError::Value(
            "embedding adapter settings must be an object".to_owned(),
        ));
    }
    let expected_transfer = if kind == "local" { "none" } else { "external" };
    if object["data_transfer"] != json!(expected_transfer) {
        return Err(SemanticError::Value(format!(
            "{kind} adapter must declare data_transfer='{expected_transfer}'"
        )));
    }
    if kind == "provider" && object.get("external_authorized") != Some(&Value::Bool(true)) {
        return Err(SemanticError::Permission(
            "external semantic provider is not explicitly authorized".to_owned(),
        ));
    }
    // Keep cache/report identity reproducible without leaking local model paths,
    // credential references or arbitrary adapter implementation fields.
    let mut identity = Map::new();
    identity.insert("cache_schema".to_owned(), json!(CACHE_SCHEMA_VERSION));
    for name in [
        "kind",
        "provider",
        "model_id",
        "model_version",
        "settings",
        "data_transfer",
        "paid",
        "external_authorized",
    ] {
        if let Some(value) = object.get(name) {
            identity.insert(name.to_owned(), value.clone());
        }
    }
    Ok(Value::Object(identity))
}

/// Bind a vector to the normalized source, model and all declared settings.
pub fn cache_key(source_sha256: &str, identity: &Value) -> Result<String, SemanticError> {
    if source_sha256.chars().count() != 64 {
        return Err(SemanticError::Value(
            "source_sha256 must be a SHA-256 hex digest".to_owned(),
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(format!("{source_sha256}:{}", canonical_json(identity)));
    Ok(hex(&hasher.finalize()))
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Validate one supplied JSON embedding: bounded dimensions, finite values,
/// non-zero magnitude.
fn validate_vector(value: &Value) -> Result<Vec<f64>, SemanticError> {
    validate_vector_parts(json_vector(value)?)
}

/// Validate already-parsed components (adapter-returned vectors can carry
/// non-finite parts a JSON round-trip would silently null out).
fn validate_vector_parts(vector: Vec<f64>) -> Result<Vec<f64>, SemanticError> {
    if vector.is_empty() || vector.len() > MAX_VECTOR_DIMENSIONS {
        return Err(SemanticError::Value(
            "embedding vector has an invalid dimension".to_owned(),
        ));
    }
    if vector.iter().any(|part| !part.is_finite()) {
        return Err(SemanticError::Value(
            "embedding vector contains a non-finite value".to_owned(),
        ));
    }
    if vector.iter().all(|part| *part == 0.0) {
        return Err(SemanticError::Value(
            "embedding vector has zero magnitude".to_owned(),
        ));
    }
    Ok(vector)
}

/// Python's `[float(part) for part in value]` over a JSON sequence.
fn json_vector(value: &Value) -> Result<Vec<f64>, SemanticError> {
    fn scalar(part: &Value) -> Result<f64, SemanticError> {
        match part {
            Value::Bool(flag) => Ok(if *flag { 1.0 } else { 0.0 }),
            Value::Number(number) => number
                .as_f64()
                .ok_or_else(|| SemanticError::Value("could not convert to float".to_owned())),
            Value::String(text) => python_float(text),
            other => Err(SemanticError::Value(format!(
                "float() argument must be a string or a real number, not '{}'",
                python_type_name(other)
            ))),
        }
    }
    match value {
        Value::Array(parts) => parts.iter().map(scalar).collect(),
        // Iterating a Python str or dict yields single characters / keys.
        Value::String(text) => text.chars().map(|c| python_float(&c.to_string())).collect(),
        Value::Object(map) => map.keys().map(|key| python_float(key)).collect(),
        other => Err(SemanticError::Value(format!(
            "'{}' object is not iterable",
            python_type_name(other)
        ))),
    }
}

/// Python `float(text)` (Rust's parser accepts the same inf/nan spellings).
fn python_float(text: &str) -> Result<f64, SemanticError> {
    text.trim()
        .parse::<f64>()
        .map_err(|_| SemanticError::Value(format!("could not convert string to float: '{text}'")))
}

fn python_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(number) => {
            if number.is_i64() || number.is_u64() {
                "int"
            } else {
                "float"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// A bounded, corruption-tolerant SQLite cache; vectors never enter reports.
pub struct EmbeddingCache {
    path: String,
    max_entries: usize,
}

impl EmbeddingCache {
    /// Open (creating if needed) the cache at `path` with the document bound.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, SemanticError> {
        Self::bounded(path, MAX_DOCUMENTS)
    }

    /// Open the cache with an explicit entry quota; eviction is oldest-first.
    pub fn bounded(path: impl AsRef<Path>, max_entries: usize) -> Result<Self, SemanticError> {
        if max_entries < 1 {
            return Err(SemanticError::Value(
                "max_entries must be positive".to_owned(),
            ));
        }
        // The writer's busy timeout (5 s) covers the concurrent-create window
        // that the oracle answers with a retry loop.
        let connection = connect(&path.as_ref().to_string_lossy())
            .map_err(|error| SemanticError::Value(error.to_string()))?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE IF NOT EXISTS semantic_embeddings (
                     cache_key TEXT PRIMARY KEY, source_sha256 TEXT NOT NULL, identity_json TEXT NOT NULL,
                     vector_json TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)",
            )
            .map_err(|error| SemanticError::Value(error.to_string()))?;
        Ok(Self {
            path: path.as_ref().to_string_lossy().into_owned(),
            max_entries,
        })
    }

    fn connect(&self) -> Result<rusqlite::Connection, SemanticError> {
        connect(&self.path).map_err(|error| SemanticError::Value(error.to_string()))
    }

    /// The cached vector, or `None` when the entry is absent or corrupt
    /// (a corrupt entry is deleted in place).
    pub fn get(&self, key: &str) -> Option<Vec<f64>> {
        let connection = self.connect().ok()?;
        let row: Option<String> = connection
            .query_row(
                "SELECT vector_json FROM semantic_embeddings WHERE cache_key=?",
                [key],
                |row| row.get(0),
            )
            .ok();
        let raw = row?;
        match serde_json::from_str::<Value>(&raw)
            .map_err(|error| SemanticError::Value(error.to_string()))
            .and_then(|value| validate_vector(&value))
        {
            Ok(vector) => Some(vector),
            Err(_) => {
                let _ =
                    connection.execute("DELETE FROM semantic_embeddings WHERE cache_key=?", [key]);
                None
            }
        }
    }

    /// Store one validated vector; the oldest entries beyond the quota evict.
    pub fn put(
        &self,
        key: &str,
        source_sha256: &str,
        identity: &Value,
        vector: &[f64],
    ) -> Result<(), SemanticError> {
        let encoded = canonical_json(&json!(validate_vector_parts(vector.to_vec())?));
        let connection = self.connect()?;
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| SemanticError::Value(error.to_string()))?;
        transaction
            .execute(
                "INSERT OR REPLACE INTO semantic_embeddings \
                 (cache_key,source_sha256,identity_json,vector_json) VALUES (?,?,?,?)",
                rusqlite::params![key, source_sha256, canonical_json(identity), encoded],
            )
            .map_err(|error| SemanticError::Value(error.to_string()))?;
        let count: i64 = transaction
            .query_row("SELECT COUNT(*) FROM semantic_embeddings", [], |row| {
                row.get(0)
            })
            .map_err(|error| SemanticError::Value(error.to_string()))?;
        let excess = count - self.max_entries as i64;
        if excess > 0 {
            transaction
                .execute(
                    "DELETE FROM semantic_embeddings WHERE rowid IN (\
                     SELECT rowid FROM semantic_embeddings \
                     ORDER BY created_at, cache_key LIMIT ?)",
                    [excess],
                )
                .map_err(|error| SemanticError::Value(error.to_string()))?;
        }
        transaction
            .commit()
            .map_err(|error| SemanticError::Value(error.to_string()))
    }
}

fn connect(path: &str) -> rusqlite::Result<rusqlite::Connection> {
    spiderhead_core::sqlite::open_writer(
        Path::new(path),
        spiderhead_core::sqlite::DEFAULT_TIMEOUT,
        &["PRAGMA busy_timeout=5000"],
    )
}

/// Cosine similarity scaled by each vector's largest component, so extreme
/// finite inputs cannot overflow into a false clean result.
pub fn cosine(left: &[f64], right: &[f64]) -> Result<f64, SemanticError> {
    if left.len() != right.len() {
        return Err(SemanticError::Value(
            "embedding vectors have inconsistent dimensions".to_owned(),
        ));
    }
    let left_scale = left
        .iter()
        .fold(0.0f64, |scale, value| scale.max(value.abs()));
    let right_scale = right
        .iter()
        .fold(0.0f64, |scale, value| scale.max(value.abs()));
    if left_scale == 0.0 || right_scale == 0.0 {
        return Err(SemanticError::Value(
            "embedding vector has zero magnitude".to_owned(),
        ));
    }
    let left: Vec<f64> = left.iter().map(|value| value / left_scale).collect();
    let right: Vec<f64> = right.iter().map(|value| value / right_scale).collect();
    let denominator = py_hypot(&left) * py_hypot(&right);
    let dot: f64 = left.iter().zip(right.iter()).map(|(a, b)| a * b).sum();
    Ok((-1.0f64).max(1.0f64.min(dot / denominator)))
}

fn py_hypot(vector: &[f64]) -> f64 {
    vector.iter().map(|value| value * value).sum::<f64>().sqrt()
}

/// Python's `round(value, 6)`: correctly rounded, ties to even.
fn round6(value: f64) -> f64 {
    format!("{value:.6}").parse().unwrap_or(value)
}

/// Resolve bounded, normalized inputs through the shared adapter/cache boundary.
fn embed_documents(
    documents: &[Value],
    adapter: &dyn EmbeddingAdapter,
    cache: &EmbeddingCache,
    identity: &Value,
    coverage: &mut Map<String, Value>,
) -> Result<Vec<(Value, Vec<f64>)>, SemanticError> {
    let mut usable: Vec<(Value, Vec<f64>)> = Vec::new();
    let mut pending: Vec<(Value, String)> = Vec::new();
    for document in documents {
        let source_hash = document.get("normalized_sha256").and_then(Value::as_str);
        let text = document.get("text").and_then(Value::as_str);
        let (Some(source_hash), Some(text)) = (source_hash, text) else {
            omit(coverage, "normalized retained content is unavailable");
            continue;
        };
        if text.is_empty() {
            omit(coverage, "normalized retained content is unavailable");
            continue;
        }
        let key = cache_key(source_hash, identity)?;
        match cache.get(&key) {
            Some(cached) => usable.push((document.clone(), cached)),
            None => match document.get("embedding") {
                Some(supplied) if !supplied.is_null() => {
                    let validated = validate_vector(supplied)?;
                    cache.put(key.as_str(), source_hash, identity, &validated)?;
                    usable.push((document.clone(), validated));
                }
                _ => pending.push((document.clone(), key)),
            },
        }
    }
    if !pending.is_empty() {
        let texts: Vec<String> = pending
            .iter()
            .map(|(document, _)| document["text"].as_str().unwrap_or_default().to_owned())
            .collect();
        let vectors = adapter.embed(&texts)?;
        if vectors.len() != pending.len() {
            return Err(SemanticError::Value(
                "embedding adapter returned a different number of vectors".to_owned(),
            ));
        }
        for ((document, key), vector) in pending.iter().zip(vectors.iter()) {
            let validated = validate_vector_parts(vector.clone())?;
            cache.put(
                key.as_str(),
                document["normalized_sha256"].as_str().unwrap_or_default(),
                identity,
                &validated,
            )?;
            usable.push((document.clone(), validated));
        }
    }
    coverage.insert("analyzed_documents".to_owned(), json!(usable.len() as i64));
    let omitted = coverage
        .get("omitted_documents")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    coverage.insert(
        "state".to_owned(),
        json!(if omitted > 0 { "partial" } else { "complete" }),
    );
    Ok(usable)
}

fn omit(coverage: &mut Map<String, Value>, reason: &str) {
    let omitted = coverage
        .get("omitted_documents")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    coverage.insert("omitted_documents".to_owned(), json!(omitted + 1));
    let reasons = coverage
        .entry("omission_reasons".to_owned())
        .or_insert_with(|| json!({}));
    if let Some(map) = reasons.as_object_mut() {
        let count = map.get(reason).and_then(Value::as_i64).unwrap_or(0);
        map.insert(reason.to_owned(), json!(count + 1));
    }
}

/// Group adapter-measured topical candidates with bounded, inspectable evidence.
///
/// `documents` are normalized retained inputs held by the caller; each needs
/// `url`, `text` and `normalized_sha256`. Missing inputs and invalid vectors
/// surface as omissions. With `adapter` absent nothing is called and no lexical
/// heuristic substitutes for embeddings.
pub fn analyze_semantic_documents(
    documents: &[Value],
    adapter: Option<&dyn EmbeddingAdapter>,
    cache: &EmbeddingCache,
    threshold: f64,
    max_candidate_comparisons: i64,
) -> Result<Value, SemanticError> {
    analyze_semantic_documents_bounded(
        documents,
        adapter,
        cache,
        threshold,
        max_candidate_comparisons,
        MAX_DOCUMENTS,
    )
}

/// [`analyze_semantic_documents`] with an explicit document bound (the oracle's
/// monkeypatchable `MAX_DOCUMENTS`).
pub fn analyze_semantic_documents_bounded(
    documents: &[Value],
    adapter: Option<&dyn EmbeddingAdapter>,
    cache: &EmbeddingCache,
    threshold: f64,
    max_candidate_comparisons: i64,
    max_documents: usize,
) -> Result<Value, SemanticError> {
    if !(0.0 < threshold && threshold <= 1.0) {
        return Err(SemanticError::Value(
            "semantic threshold must be in (0, 1]".to_owned(),
        ));
    }
    if max_candidate_comparisons < 0 {
        return Err(SemanticError::Value(
            "max_candidate_comparisons cannot be negative".to_owned(),
        ));
    }
    let total = documents.len();
    let truncated = total > max_documents;
    let selected: Vec<Value> = documents.iter().take(max_documents).cloned().collect();
    let mut coverage = json!({
        "eligible_documents": total as i64,
        "analyzed_documents": 0,
        "omitted_documents": 0,
        "omission_reasons": {},
    });
    let Some(adapter) = adapter else {
        if let Some(map) = coverage.as_object_mut() {
            map.insert("state".to_owned(), json!("unavailable"));
        }
        return Ok(json!({
            "ok": false,
            "reason": "no semantic embedding adapter is selected",
            "coverage": coverage,
            "groups": [],
        }));
    };
    let identity = adapter_identity(adapter)?;
    let coverage_map = coverage.as_object_mut().expect("coverage object");
    if truncated {
        let omitted = (total - selected.len()) as i64;
        for _ in 0..omitted {
            omit(coverage_map, "semantic document limit exceeded");
        }
    }
    let usable = embed_documents(&selected, adapter, cache, &identity, coverage_map)?;

    // Union-find with path halving over the candidate edges above the threshold.
    let mut parent: Vec<usize> = (0..usable.len()).collect();
    fn root(parent: &mut [usize], mut index: usize) -> usize {
        while parent[index] != index {
            parent[index] = parent[parent[index]];
            index = parent[index];
        }
        index
    }
    let mut comparisons: i64 = 0;
    let mut edges: Vec<Value> = Vec::new();
    for left in 0..usable.len() {
        for right in left + 1..usable.len() {
            if comparisons >= max_candidate_comparisons {
                let mut map = coverage.as_object().cloned().unwrap_or_default();
                map.insert("state".to_owned(), json!("partial"));
                return Ok(json!({
                    "ok": false,
                    "reason": "semantic candidate-comparison budget exceeded",
                    "adapter": identity,
                    "coverage": Value::Object(map),
                    "groups": [],
                }));
            }
            comparisons += 1;
            let similarity = match cosine(&usable[left].1, &usable[right].1) {
                Ok(value) => value,
                Err(error) => {
                    let mut map = coverage.as_object().cloned().unwrap_or_default();
                    map.insert("state".to_owned(), json!("partial"));
                    return Ok(json!({
                        "ok": false,
                        "reason": error.to_string(),
                        "adapter": identity,
                        "coverage": Value::Object(map),
                        "groups": [],
                    }));
                }
            };
            if similarity >= threshold {
                let (left_root, right_root) = (root(&mut parent, left), root(&mut parent, right));
                if left_root != right_root {
                    parent[right_root] = left_root;
                }
                edges.push(json!({
                    "left": usable[left].0["url"],
                    "right": usable[right].0["url"],
                    "similarity": round6(similarity),
                }));
            }
        }
    }
    let mut groups: IndexMap<usize, Vec<Value>> = IndexMap::new();
    for (index, (document, _)) in usable.iter().enumerate() {
        groups
            .entry(root(&mut parent, index))
            .or_default()
            .push(document.clone());
    }
    let candidates: Vec<Value> = edges
        .iter()
        .map(|edge| {
            json!({
                "source_url": edge["left"],
                "target_url": edge["right"],
                "similarity": edge["similarity"],
                "kind": "semantic_internal_link_review_candidate",
                "conclusion": "review whether a contextual internal link is useful; similarity alone does not prove relevance or demand",
            })
        })
        .collect();
    let mut group_documents: Vec<Value> = Vec::new();
    for members in groups.into_values() {
        if members.len() > 1 {
            group_documents.push(json!({
                "kind": "semantic_similarity_candidate",
                "conclusion": "review topical similarity; this is not a duplicate or cannibalization finding",
                "members": members.iter().map(|item| json!({
                    "url": item["url"],
                    "source_sha256": item["normalized_sha256"],
                    "language": item.get("language").cloned().unwrap_or(Value::Null),
                    "excerpt": excerpt(item),
                })).collect::<Vec<Value>>(),
                "evidence": edges.iter().filter(|edge| {
                    let urls: std::collections::HashSet<&str> = members
                        .iter()
                        .filter_map(|item| item["url"].as_str())
                        .collect();
                    let (Some(left), Some(right)) =
                        (edge["left"].as_str(), edge["right"].as_str())
                    else {
                        return false;
                    };
                    urls.contains(left) && urls.contains(right)
                }).cloned().collect::<Vec<Value>>(),
            }));
        }
    }
    Ok(json!({
        "ok": true,
        "adapter": identity,
        "coverage": coverage,
        "candidate_comparisons": comparisons,
        "internal_link_candidates": candidates,
        "groups": group_documents,
    }))
}

fn excerpt(item: &Value) -> String {
    item["text"]
        .as_str()
        .map(|text| text.chars().take(240).collect())
        .unwrap_or_default()
}

/// Compare each supplied title/H1 to its own retained body, using embeddings only.
///
/// Body vectors reuse the same cache as semantic grouping. Missing headings are
/// omissions, never low-relevance findings; every score stays review evidence.
pub fn analyze_semantic_relevance(
    documents: &[Value],
    adapter: &dyn EmbeddingAdapter,
    cache: &EmbeddingCache,
    threshold: f64,
    fields: &[&str],
) -> Result<Value, SemanticError> {
    if !(0.0..=1.0).contains(&threshold) {
        return Err(SemanticError::Value(
            "semantic relevance threshold must be in [0, 1]".to_owned(),
        ));
    }
    if documents.len() > MAX_DOCUMENTS {
        return Err(SemanticError::Value(
            "semantic document limit exceeded".to_owned(),
        ));
    }
    let mut unique_fields: Vec<&str> = Vec::new();
    for field in fields {
        if *field != "title" && *field != "h1" {
            return Err(SemanticError::Value(
                "semantic relevance fields must select title or h1".to_owned(),
            ));
        }
        if !unique_fields.contains(field) {
            unique_fields.push(field);
        }
    }
    if unique_fields.is_empty() {
        return Err(SemanticError::Value(
            "semantic relevance fields must select title or h1".to_owned(),
        ));
    }
    let identity = adapter_identity(adapter)?;
    let mut inputs: Vec<Value> = Vec::new();
    let mut missing: Vec<(&str, i64)> = unique_fields.iter().map(|field| (*field, 0)).collect();
    let mut missing_vectors: Vec<(&str, i64)> =
        unique_fields.iter().map(|field| (*field, 0)).collect();
    for item in documents {
        let mut body = item.as_object().cloned().unwrap_or_default();
        body.insert("field".to_owned(), json!("body"));
        inputs.push(Value::Object(body));
        for (field, count) in missing.iter_mut() {
            let raw = item.get(*field).cloned().unwrap_or(Value::Null);
            let text = if py_truthy(&raw) {
                spiderhead_parse::text_normalize::normalize_text(&py_str(&raw))
            } else {
                String::new()
            };
            if text.is_empty() {
                *count += 1;
                continue;
            }
            let mut heading = json!({
                "url": item.get("url").cloned().unwrap_or(Value::Null),
                "field": field,
                "text": text,
                "normalized_sha256": hex(&Sha256::digest(text.as_bytes())),
            });
            let embedding_field = format!("{field}_embedding");
            let supplied = item.get(embedding_field.as_str()).cloned();
            match supplied {
                Some(embedding) if !embedding.is_null() => {
                    heading["embedding"] = embedding;
                }
                _ => {
                    let key = cache_key(
                        heading["normalized_sha256"].as_str().unwrap_or_default(),
                        &identity,
                    )?;
                    if adapter.is_declared() && cache.get(&key).is_none() {
                        if let Some((_, count)) =
                            missing_vectors.iter_mut().find(|(name, _)| name == field)
                        {
                            *count += 1;
                        }
                        continue;
                    }
                }
            }
            inputs.push(heading);
        }
    }
    let mut coverage = json!({"omitted_documents": 0, "omission_reasons": {}});
    let usable = embed_documents(
        &inputs,
        adapter,
        cache,
        &identity,
        coverage.as_object_mut().expect("coverage object"),
    )?;
    let mut vectors: HashMap<(String, String), Vec<f64>> = HashMap::new();
    for (item, vector) in usable {
        let url = item["url"].as_str().unwrap_or_default().to_owned();
        let field = item["field"].as_str().unwrap_or_default().to_owned();
        vectors.insert((url, field), vector);
    }
    let mut scores: Vec<Value> = Vec::new();
    for item in documents {
        let Some(url) = item["url"].as_str() else {
            continue;
        };
        let Some(body) = vectors.get(&(url.to_owned(), "body".to_owned())) else {
            continue;
        };
        for (field, _) in missing.iter() {
            if let Some(heading) = vectors.get(&(url.to_owned(), (*field).to_owned())) {
                let similarity = cosine(body, heading)?;
                scores.push(json!({
                    "url": url,
                    "field": field,
                    "similarity": round6(similarity),
                    "threshold": threshold,
                    "low_relevance": similarity < threshold,
                    "source_sha256": item["normalized_sha256"],
                }));
            }
        }
    }
    let eligible_pairs = (documents.len() * fields.len()) as i64;
    let analyzed_pairs = scores.len() as i64;
    let mut missing_headings = Map::new();
    for (field, count) in &missing {
        missing_headings.insert(field.to_string(), json!(count));
    }
    let mut unavailable_vectors = Map::new();
    for (field, count) in &missing_vectors {
        unavailable_vectors.insert(field.to_string(), json!(count));
    }
    Ok(json!({
        "adapter": identity,
        "scores": scores,
        "missing_headings": missing_headings,
        "unavailable_vectors": unavailable_vectors,
        "coverage": {
            "eligible_pairs": eligible_pairs,
            "analyzed_pairs": analyzed_pairs,
            "state": if analyzed_pairs == eligible_pairs { "complete" } else { "partial" },
        },
    }))
}

/// Python truthiness over JSON scalars and containers.
fn py_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().map(|v| v != 0.0).unwrap_or(true),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// Python `str(value)` over JSON scalar headings (strings pass through).
fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        Value::Bool(flag) => {
            if *flag {
                "True".to_owned()
            } else {
                "False".to_owned()
            }
        }
        Value::Number(number) => number.to_string(),
        other => canonical_json(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn documents() -> Vec<Value> {
        vec![
            json!({
                "url": "https://example.test/pumps",
                "text": "pump stations for industrial water supply",
                "normalized_sha256": "a".repeat(64),
                "language": {"declared_primary": "en"},
            }),
            json!({
                "url": "https://example.test/water",
                "text": "industrial water supply pump equipment",
                "normalized_sha256": "b".repeat(64),
                "language": {"declared_primary": "en"},
            }),
            json!({
                "url": "https://example.test/cakes",
                "text": "cakes and pastries for a birthday party",
                "normalized_sha256": "c".repeat(64),
                "language": {"declared_primary": "en"},
            }),
        ]
    }

    type Calls = Rc<RefCell<Vec<Vec<String>>>>;

    fn topical_adapter(
        calls: &Calls,
    ) -> LocalEmbeddingAdapter<impl Fn(&[String]) -> Vec<Vec<f64>>> {
        let calls = Rc::clone(calls);
        LocalEmbeddingAdapter::new(
            "fixture",
            "1",
            "/models/fixture",
            move |texts: &[String]| {
                calls.borrow_mut().push(texts.to_vec());
                texts
                    .iter()
                    .map(|text| {
                        if text.contains("pump") || text.contains("water") {
                            vec![1.0, 0.0]
                        } else {
                            vec![0.0, 1.0]
                        }
                    })
                    .collect()
            },
            None,
        )
    }

    // from tests/test_semantic_similarity.py::test_local_adapter_groups_only_supplied_embedding_evidence_and_reuses_cache
    #[test]
    fn local_adapter_groups_only_supplied_embedding_evidence_and_reuses_cache() {
        let dir = tempfile::tempdir().unwrap();
        let calls: Calls = Rc::new(RefCell::new(Vec::new()));
        let cache = EmbeddingCache::new(dir.path().join("semantic.sqlite")).unwrap();
        let first = {
            let adapter = topical_adapter(&calls);
            analyze_semantic_documents(
                &documents(),
                Some(&adapter),
                &cache,
                0.9,
                MAX_CANDIDATE_COMPARISONS,
            )
            .unwrap()
        };
        let second = {
            let adapter = topical_adapter(&calls);
            analyze_semantic_documents(
                &documents(),
                Some(&adapter),
                &cache,
                0.9,
                MAX_CANDIDATE_COMPARISONS,
            )
            .unwrap()
        };

        assert_eq!(first["ok"], json!(true));
        assert_eq!(
            first["groups"][0]["kind"],
            json!("semantic_similarity_candidate")
        );
        let mut member_urls: Vec<&str> = first["groups"][0]["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|member| member["url"].as_str().unwrap())
            .collect();
        member_urls.sort_unstable();
        assert_eq!(
            member_urls,
            vec!["https://example.test/pumps", "https://example.test/water",]
        );
        assert!(first["groups"][0]["conclusion"]
            .as_str()
            .unwrap()
            .contains("duplicate"));
        assert_eq!(
            first["internal_link_candidates"][0]["kind"],
            json!("semantic_internal_link_review_candidate")
        );
        assert_eq!(calls.borrow().len(), 1);
        assert_eq!(second["coverage"]["state"], json!("complete"));
    }

    // from tests/test_semantic_similarity.py::test_missing_adapter_is_unavailable_and_never_invents_a_lexical_score
    #[test]
    fn missing_adapter_is_unavailable_and_never_invents_a_lexical_score() {
        let dir = tempfile::tempdir().unwrap();
        let cache = EmbeddingCache::new(dir.path().join("cache.sqlite")).unwrap();
        let result =
            analyze_semantic_documents(&documents(), None, &cache, 0.82, MAX_CANDIDATE_COMPARISONS)
                .unwrap();

        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["coverage"]["state"], json!("unavailable"));
        assert_eq!(result["groups"], json!([]));
    }

    // from tests/test_semantic_similarity.py::test_provider_authorization_requires_literal_true
    #[test]
    fn provider_authorization_requires_literal_true() {
        for authorization in [
            json!(false),
            json!("false"),
            json!("true"),
            json!(1),
            json!(null),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let cache = EmbeddingCache::new(dir.path().join("cache.sqlite")).unwrap();
            let adapter = ProviderEmbeddingAdapter::new(
                "fixture",
                "fixture",
                "1",
                |_texts: &[String]| unreachable!("unauthorized embed call"),
                None,
            )
            .with_external_authorized(authorization);
            let error = analyze_semantic_documents(
                &documents(),
                Some(&adapter),
                &cache,
                0.82,
                MAX_CANDIDATE_COMPARISONS,
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("not explicitly authorized"),
                "{error}"
            );
            let error = adapter
                .embed(&["private retained text".to_owned()])
                .unwrap_err();
            assert!(error.to_string().contains("not explicitly authorized"));
        }
    }

    // from tests/test_semantic_similarity.py::test_provider_is_opt_in_before_any_embed_call
    #[test]
    fn provider_is_opt_in_before_any_embed_call() {
        let dir = tempfile::tempdir().unwrap();
        let cache = EmbeddingCache::new(dir.path().join("cache.sqlite")).unwrap();
        let adapter = ProviderEmbeddingAdapter::new(
            "fixture",
            "fixture",
            "1",
            |_texts: &[String]| panic!("called"),
            None,
        );
        let error = analyze_semantic_documents(
            &documents(),
            Some(&adapter),
            &cache,
            0.82,
            MAX_CANDIDATE_COMPARISONS,
        )
        .unwrap_err();
        assert!(error.to_string().contains("not explicitly authorized"));
    }

    // from tests/test_semantic_similarity.py::test_cache_invalidates_model_configuration_and_discards_corrupt_vectors
    #[test]
    fn cache_invalidates_model_configuration_and_discards_corrupt_vectors() {
        let dir = tempfile::tempdir().unwrap();
        let calls: Calls = Rc::new(RefCell::new(Vec::new()));
        let cache = EmbeddingCache::bounded(dir.path().join("cache.sqlite"), 3).unwrap();
        let docs = &documents()[..1];
        for model_version in ["1", "2"] {
            let adapter = unit_adapter(&calls, model_version);
            analyze_semantic_documents(
                docs,
                Some(&adapter),
                &cache,
                0.82,
                MAX_CANDIDATE_COMPARISONS,
            )
            .unwrap();
        }
        assert_eq!(calls.borrow().len(), 2);

        let connection = rusqlite::Connection::open(dir.path().join("cache.sqlite")).unwrap();
        connection
            .execute("UPDATE semantic_embeddings SET vector_json='not-json'", [])
            .unwrap();
        let adapter = unit_adapter(&calls, "2");
        analyze_semantic_documents(
            docs,
            Some(&adapter),
            &cache,
            0.82,
            MAX_CANDIDATE_COMPARISONS,
        )
        .unwrap();
        assert_eq!(calls.borrow().len(), 3);
    }

    fn unit_adapter(
        calls: &Calls,
        model_version: &str,
    ) -> LocalEmbeddingAdapter<impl Fn(&[String]) -> Vec<Vec<f64>>> {
        let calls = Rc::clone(calls);
        let version = model_version.to_owned();
        LocalEmbeddingAdapter::new(
            "m",
            &version,
            "/m",
            move |texts: &[String]| {
                calls.borrow_mut().push(texts.to_vec());
                texts.iter().map(|_| vec![1.0, 0.0]).collect()
            },
            None,
        )
    }

    // from tests/test_semantic_similarity.py::test_candidate_budget_and_invalid_documents_are_explicit
    #[test]
    fn candidate_budget_and_invalid_documents_are_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let calls: Calls = Rc::new(RefCell::new(Vec::new()));
        let adapter = unit_adapter(&calls, "1");
        let cache = EmbeddingCache::new(dir.path().join("cache.sqlite")).unwrap();
        let mut partial_documents = documents()[..2].to_vec();
        partial_documents.push(json!({"url": "https://example.test/missing"}));
        let partial =
            analyze_semantic_documents(&partial_documents, Some(&adapter), &cache, 0.82, 0)
                .unwrap();

        assert_eq!(partial["ok"], json!(false));
        assert_eq!(
            partial["reason"],
            json!("semantic candidate-comparison budget exceeded")
        );
        assert_eq!(partial["coverage"]["omitted_documents"], json!(1));
    }

    // from tests/test_semantic_similarity.py::test_document_bound_is_partial_and_adapter_identity_does_not_leak_a_local_path
    #[test]
    fn document_bound_is_partial_and_adapter_identity_does_not_leak_a_local_path() {
        let dir = tempfile::tempdir().unwrap();
        let calls: Calls = Rc::new(RefCell::new(Vec::new()));
        let adapter = LocalEmbeddingAdapter::new(
            "m",
            "1",
            "/private/model",
            move |texts: &[String]| {
                calls.borrow_mut().push(texts.to_vec());
                texts.iter().map(|_| vec![1.0]).collect()
            },
            None,
        );
        let cache = EmbeddingCache::new(dir.path().join("cache.sqlite")).unwrap();
        let result = analyze_semantic_documents_bounded(
            &documents(),
            Some(&adapter),
            &cache,
            0.82,
            MAX_CANDIDATE_COMPARISONS,
            2,
        )
        .unwrap();

        assert_eq!(result["coverage"]["state"], json!("partial"));
        assert_eq!(
            result["coverage"]["omission_reasons"],
            json!({"semantic document limit exceeded": 1})
        );
        assert!(result["adapter"].get("model_path").is_none());
    }

    // from tests/test_semantic_similarity.py::test_cache_handles_concurrent_local_writers
    #[test]
    fn cache_handles_concurrent_local_writers() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("shared.sqlite");
        std::thread::scope(|scope| {
            for index in 1..=8i64 {
                let path = cache_path.clone();
                scope.spawn(move || {
                    let cache = EmbeddingCache::new(&path).unwrap();
                    cache
                        .put(
                            &index.to_string().repeat(64),
                            &"a".repeat(64),
                            &json!({"model": "fixture"}),
                            &[index as f64, 1.0],
                        )
                        .unwrap();
                });
            }
        });

        let connection = rusqlite::Connection::open(&cache_path).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM semantic_embeddings", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 8);
    }

    // from tests/test_semantic_similarity.py::test_cache_entry_quota_evicts_oldest_vectors
    #[test]
    fn cache_entry_quota_evicts_oldest_vectors() {
        let dir = tempfile::tempdir().unwrap();
        let cache = EmbeddingCache::bounded(dir.path().join("bounded.sqlite"), 1).unwrap();
        cache
            .put(
                &"a".repeat(64),
                &"a".repeat(64),
                &json!({"model": "fixture"}),
                &[1.0],
            )
            .unwrap();
        cache
            .put(
                &"b".repeat(64),
                &"b".repeat(64),
                &json!({"model": "fixture"}),
                &[2.0],
            )
            .unwrap();

        assert_eq!(cache.get(&"a".repeat(64)), None);
        assert_eq!(cache.get(&"b".repeat(64)), Some(vec![2.0]));
    }

    // from tests/test_semantic_similarity.py::test_relevance_reuses_body_vectors_and_reports_missing_headings
    #[test]
    fn relevance_reuses_body_vectors_and_reports_missing_headings() {
        let dir = tempfile::tempdir().unwrap();
        let calls: Calls = Rc::new(RefCell::new(Vec::new()));
        let cache = EmbeddingCache::new(dir.path().join("relevance.sqlite")).unwrap();
        let mut docs = vec![documents()[0].clone()];
        docs[0]["title"] = json!("A pump title");
        docs[0]["h1"] = json!("");
        {
            let adapter = unit_adapter(&calls, "1");
            analyze_semantic_documents(
                &docs,
                Some(&adapter),
                &cache,
                0.82,
                MAX_CANDIDATE_COMPARISONS,
            )
            .unwrap();
        }
        let make_adapter = || unit_adapter(&calls, "1");
        let first =
            analyze_semantic_relevance(&docs, &make_adapter(), &cache, 0.35, &["title", "h1"])
                .unwrap();
        let second =
            analyze_semantic_relevance(&docs, &make_adapter(), &cache, 0.35, &["title", "h1"])
                .unwrap();
        assert_eq!(first, second);
        assert_eq!(first["missing_headings"], json!({"title": 0, "h1": 1}));
        assert_eq!(first["scores"][0]["low_relevance"], json!(false));
        let calls = calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].len(), 1);
    }

    // from tests/test_semantic_similarity.py::test_single_document_invalid_vector_never_reports_complete
    #[test]
    fn single_document_invalid_vector_never_reports_complete() {
        for vector in [vec![0.0], vec![f64::INFINITY], vec![]] {
            let dir = tempfile::tempdir().unwrap();
            let calls: Calls = Rc::new(RefCell::new(Vec::new()));
            let vector = std::cell::RefCell::new(vector);
            let adapter = LocalEmbeddingAdapter::new(
                "m",
                "1",
                "/m",
                move |texts: &[String]| {
                    calls.borrow_mut().push(texts.to_vec());
                    let vector = vector.borrow().clone();
                    texts.iter().map(|_| vector.clone()).collect()
                },
                None,
            );
            let cache = EmbeddingCache::new(dir.path().join("cache.sqlite")).unwrap();
            let error = analyze_semantic_documents(
                &documents()[..1],
                Some(&adapter),
                &cache,
                0.82,
                MAX_CANDIDATE_COMPARISONS,
            )
            .unwrap_err();
            assert!(error.to_string().contains("embedding vector"), "{error}");
        }
    }

    // from tests/test_semantic_similarity.py::test_extreme_finite_vectors_do_not_overflow_into_false_clean_results
    #[test]
    fn extreme_finite_vectors_do_not_overflow_into_false_clean_results() {
        let dir = tempfile::tempdir().unwrap();
        let calls: Calls = Rc::new(RefCell::new(Vec::new()));
        let adapter = LocalEmbeddingAdapter::new(
            "m",
            "1",
            "/m",
            move |texts: &[String]| {
                calls.borrow_mut().push(texts.to_vec());
                texts.iter().map(|_| vec![1e308, 1e308]).collect()
            },
            None,
        );
        let cache = EmbeddingCache::new(dir.path().join("cache.sqlite")).unwrap();
        let result = analyze_semantic_documents(
            &documents()[..2],
            Some(&adapter),
            &cache,
            0.82,
            MAX_CANDIDATE_COMPARISONS,
        )
        .unwrap();
        assert_eq!(result["ok"], json!(true));
        assert_eq!(
            result["groups"][0]["evidence"][0]["similarity"]
                .as_f64()
                .unwrap(),
            1.0
        );
    }

    // from spiderhead/checks/semantic_similarity.py::cache_key (contract: the
    // digest binds the normalized source and the canonical identity)
    #[test]
    fn cache_key_binds_source_and_identity() {
        let identity = json!({"a": 1, "b": "x"});
        let first = cache_key(&"a".repeat(64), &identity).unwrap();
        let second = cache_key(&"a".repeat(64), &identity).unwrap();
        assert_eq!(first, second);
        let error = cache_key("short", &identity).unwrap_err();
        assert_eq!(
            error.to_string(),
            "source_sha256 must be a SHA-256 hex digest"
        );
    }

    // from spiderhead/checks/semantic_similarity.py::adapter_identity (contract:
    // declaration validation with Python error texts)
    #[test]
    fn adapter_identity_validation_messages() {
        let adapter = DeclaredEmbeddingAdapter::new(&json!({"kind": "local"}));
        let error = adapter_identity(&adapter).unwrap_err();
        assert_eq!(
            error.to_string(),
            "embedding adapter declaration is missing: model_id, model_version, settings, data_transfer"
        );
        let adapter = DeclaredEmbeddingAdapter::new(&json!({
            "kind": "hybrid", "model_id": "m", "model_version": "1",
            "settings": {}, "data_transfer": "none",
        }));
        let error = adapter_identity(&adapter).unwrap_err();
        assert_eq!(
            error.to_string(),
            "embedding adapter kind must be local or provider"
        );
        let adapter = DeclaredEmbeddingAdapter::new(&json!({
            "kind": "local", "model_id": "m", "model_version": "1",
            "settings": "x", "data_transfer": "none",
        }));
        let error = adapter_identity(&adapter).unwrap_err();
        assert_eq!(
            error.to_string(),
            "embedding adapter settings must be an object"
        );
        let adapter = DeclaredEmbeddingAdapter::new(&json!({
            "kind": "local", "model_id": "m", "model_version": "1",
            "settings": {}, "data_transfer": "external",
        }));
        let error = adapter_identity(&adapter).unwrap_err();
        assert_eq!(
            error.to_string(),
            "local adapter must declare data_transfer='none'"
        );
        let adapter = DeclaredEmbeddingAdapter::new(&json!({
            "kind": "provider", "model_id": "m", "model_version": "1",
            "settings": {}, "data_transfer": "external", "external_authorized": true,
            "model_path": "/private/model",
        }));
        let identity = adapter_identity(&adapter).unwrap();
        assert_eq!(identity["cache_schema"], json!(CACHE_SCHEMA_VERSION));
        assert!(identity.get("model_path").is_none());
        assert_eq!(identity["external_authorized"], json!(true));
    }

    // from spiderhead/checks/semantic_similarity.py::_cosine (contract:
    // dimension mismatch and zero-magnitude refusals)
    #[test]
    fn cosine_refuses_inconsistent_and_zero_vectors() {
        let error = cosine(&[1.0, 0.0], &[1.0, 0.0, 0.0]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "embedding vectors have inconsistent dimensions"
        );
        let error = cosine(&[0.0, 0.0], &[1.0, 0.0]).unwrap_err();
        assert_eq!(error.to_string(), "embedding vector has zero magnitude");
        assert_eq!(cosine(&[2.0, 0.0], &[0.0, 5.0]).unwrap(), 0.0);
        assert_eq!(cosine(&[3.0, 4.0], &[3.0, 4.0]).unwrap(), 1.0);
    }
}
