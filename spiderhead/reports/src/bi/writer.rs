//! Bounded CSV partition writing and atomic publication of the BI package
//! (partitions plus manifest, written completely or not published at all).

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::Digest;

use crate::bi::cohorts::{cohort_rows, coverage_rows};
use crate::bi::inputs::{join_from_provider, validate_join};
use crate::bi::project::{
    finding_source, link_row, metric_rows, page_row, provider_collection, provider_observations,
    Observation,
};
use crate::bi::schema::{
    format_cell, manifest_dataset, max_output_partitions, metric_fields, safe_dimension_fields,
    BIExportError, Field, ProviderInput, RunInput, BI_SCHEMA_VERSION, COHORT_FIELDS,
    COVERAGE_FIELDS, FINDING_FIELDS, LINK_FIELDS, MANIFEST_FORMAT, MAX_CELL_BYTES, MAX_FINDINGS,
    MAX_LINK_OCCURRENCES, MAX_MANIFEST_BYTES, MAX_OUTPUT_ROWS, MAX_PAGES,
    MAX_PROJECTION_INDEX_BYTES, MAX_PROVIDER_ROWS_PER_SOURCE, MAX_PROVIDER_SOURCES,
    MIN_FREE_DISK_BYTES, PAGE_FIELDS,
};
use crate::bi_index::{projection_index, GroupIndex, InlinkIndex};
use crate::py;

fn err(msg: impl Into<String>) -> BIExportError {
    BIExportError::new(msg)
}

fn free_bytes(path: &Path) -> Option<u64> {
    spiderhead_core::filesystem::free_space(path).ok()
}

/// Shared byte and partition budget across all writers; `Cell`
/// fields allow updates behind a shared `&self` reference.
pub struct OutputBudget {
    max_bytes: u64,
    bytes: Cell<u64>,
    partitions: Cell<usize>,
}

impl OutputBudget {
    /// Create a budget with a byte limit.
    pub fn new(max_bytes: u64) -> Self {
        OutputBudget {
            max_bytes,
            bytes: Cell::new(0),
            partitions: Cell::new(0),
        }
    }

    /// Reserve one partition slot.
    pub fn reserve_partition(&self) -> Result<(), BIExportError> {
        if self.partitions.get() >= max_output_partitions() {
            return Err(err(
                "too many CSV partitions; increase max_rows_per_file or max_bytes_per_file",
            ));
        }
        self.partitions.set(self.partitions.get() + 1);
        Ok(())
    }

    /// Reserve bytes against the budget.
    pub fn reserve_bytes(&self, amount: u64) -> Result<(), BIExportError> {
        if self.bytes.get() + amount > self.max_bytes {
            return Err(err(format!(
                "total output exceeds the {}-byte bound; no package was published",
                self.max_bytes
            )));
        }
        self.bytes.set(self.bytes.get() + amount);
        Ok(())
    }
}

/// Deterministic, formula-safe CSV partitions for one
/// dataset with SHA-256 digests and shared budget accounting.
pub struct PartitionWriter<'a> {
    directory: PathBuf,
    dataset: String,
    fields: Vec<Field>,
    max_rows_per_file: usize,
    max_bytes_per_file: u64,
    budget: &'a OutputBudget,
    total_bytes: u64,
    total_rows: usize,
    disk_check_at: u64,
    /// Cells prefixed with `'` to neutralize spreadsheet formulas.
    pub neutralized_cells: usize,
    /// Closed partition descriptors in write order.
    pub partitions: Vec<Value>,
    stream: Option<fs::File>,
    digest: sha2::Sha256,
    path: Option<PathBuf>,
    partition_rows: usize,
    partition_bytes: u64,
    header: Vec<u8>,
}

impl<'a> PartitionWriter<'a> {
    /// Create a writer for one dataset in `directory` with its fields.
    pub fn new(
        directory: &Path,
        dataset: &str,
        fields: &[Field],
        max_rows_per_file: usize,
        max_bytes_per_file: u64,
        budget: &'a OutputBudget,
    ) -> Self {
        PartitionWriter {
            directory: directory.to_path_buf(),
            dataset: dataset.to_owned(),
            fields: fields.to_vec(),
            max_rows_per_file,
            max_bytes_per_file,
            budget,
            total_bytes: 0,
            total_rows: 0,
            disk_check_at: 0,
            neutralized_cells: 0,
            partitions: Vec::new(),
            stream: None,
            digest: sha2::Sha256::new(),
            path: None,
            partition_rows: 0,
            partition_bytes: 0,
            header: Self::encode(
                &fields
                    .iter()
                    .map(|f| f.name.to_string())
                    .collect::<Vec<_>>(),
            ),
        }
    }

    /// One CSV row with minimal quoting and LF line endings.
    pub fn encode(values: &[String]) -> Vec<u8> {
        let mut writer = csv::WriterBuilder::new()
            .terminator(csv::Terminator::Any(b'\n'))
            .from_writer(Vec::new());
        let _ = writer.write_record(values);
        writer.into_inner().unwrap_or_default()
    }

    fn open_partition(&mut self) -> Result<(), BIExportError> {
        self.budget.reserve_partition()?;
        let index = self.partitions.len() + 1;
        self.path = Some(
            self.directory
                .join(format!("{}-{:04}.csv", self.dataset, index)),
        );
        let path = self.path.clone().unwrap_or_default();
        let file =
            fs::File::create(&path).map_err(|e| err(format!("cannot create BI partition: {e}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        }
        self.stream = Some(file);
        self.digest = sha2::Sha256::new();
        self.partition_rows = 0;
        self.partition_bytes = 0;
        if self.header.len() as u64 > self.max_bytes_per_file {
            return Err(err(format!(
                "{} CSV header exceeds the partition byte limit",
                self.dataset
            )));
        }
        let header = std::mem::take(&mut self.header);
        let write = self.write_bytes(&header);
        self.header = header;
        write
    }

    fn write_bytes(&mut self, content: &[u8]) -> Result<(), BIExportError> {
        if self.total_bytes >= self.disk_check_at || content.len() >= 1024 * 1024 {
            let free = free_bytes(&self.directory).unwrap_or(u64::MAX);
            if free < MIN_FREE_DISK_BYTES + content.len() as u64 {
                return Err(err(
                    "insufficient free disk for BI output; no package was published",
                ));
            }
            self.disk_check_at = self.total_bytes + 1024 * 1024;
        }
        self.budget.reserve_bytes(content.len() as u64)?;
        if let Some(stream) = &mut self.stream {
            stream
                .write_all(content)
                .map_err(|e| err(format!("BI partition write failed: {e}")))?;
        }
        use sha2::Digest;
        self.digest.update(content);
        self.partition_bytes += content.len() as u64;
        self.total_bytes += content.len() as u64;
        Ok(())
    }

    /// Write one typed dataset row.
    pub fn write(&mut self, row: &Value) -> Result<(), BIExportError> {
        if self.total_rows >= MAX_OUTPUT_ROWS {
            return Err(err(format!(
                "{} exceeds the {}-row hard bound; no package was published",
                self.dataset, MAX_OUTPUT_ROWS
            )));
        }
        let declared: BTreeSet<&str> = self.fields.iter().map(|f| f.name.as_ref()).collect();
        if let Some(extra) = py::map(row)
            .map(|m| m.keys().find(|k| !declared.contains(k.as_str())).is_some())
            .filter(|has| *has)
        {
            let _ = extra;
            return Err(err(format!(
                "{} row contains undeclared fields",
                self.dataset
            )));
        }
        let mut values = Vec::with_capacity(self.fields.len());
        for field in &self.fields {
            let value = row.get(field.name.as_ref()).cloned().unwrap_or(Value::Null);
            if value.is_null() && !field.nullable {
                return Err(err(format!(
                    "{}.{} is not nullable",
                    self.dataset, field.name
                )));
            }
            let (cell, neutralized) = format_cell(
                &value,
                field.type_,
                &format!("{}.{}", self.dataset, field.name),
            )?;
            self.neutralized_cells += usize::from(neutralized);
            if cell.len() as u64 > MAX_CELL_BYTES {
                return Err(err(format!(
                    "{}.{} exceeds the {}-byte cell bound",
                    self.dataset, field.name, MAX_CELL_BYTES
                )));
            }
            values.push(cell);
        }
        self.write_cells(&values)
    }

    /// Write already-serialized CSV values.
    pub fn write_cells(&mut self, values: &[String]) -> Result<(), BIExportError> {
        if values.len() != self.fields.len() {
            return Err(err("CSV row width differs from the selected field schema"));
        }
        if values.iter().any(|v| v.len() as u64 > MAX_CELL_BYTES) {
            return Err(err(format!(
                "CSV value exceeds the {MAX_CELL_BYTES}-byte cell bound"
            )));
        }
        if self.total_rows >= MAX_OUTPUT_ROWS {
            return Err(err("selected output exceeds the row bound"));
        }
        let encoded = Self::encode(values);
        if encoded.len() as u64 + self.header.len() as u64 > self.max_bytes_per_file {
            return Err(err(format!(
                "one {} row exceeds the configured partition byte limit",
                self.dataset
            )));
        }
        if self.stream.is_none()
            || self.partition_rows >= self.max_rows_per_file
            || self.partition_bytes + encoded.len() as u64 > self.max_bytes_per_file
        {
            self.close_partition();
            self.open_partition()?;
        }
        self.write_bytes(&encoded)?;
        self.partition_rows += 1;
        self.total_rows += 1;
        Ok(())
    }

    fn close_partition(&mut self) {
        let (Some(mut stream), Some(path)) = (self.stream.take(), self.path.take()) else {
            return;
        };
        let _ = stream.flush();
        let _ = stream.sync_all();
        self.partitions.push(json!({
            "path": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            "rows": self.partition_rows,
            "bytes": self.partition_bytes,
            "sha256": format!("{:x}", self.digest.finalize_reset()),
        }));
    }

    /// Release an interrupted writer without publishing metadata.
    pub fn close(&mut self) {
        self.stream = None;
    }

    /// Finish the partition and record its descriptor.
    pub fn finish(&mut self) -> Result<Value, BIExportError> {
        if self.stream.is_none() {
            self.open_partition()?;
        }
        self.close_partition();
        Ok(json!({
            "row_count": self.total_rows,
            "partition_count": self.partitions.len(),
            "partitions": self.partitions,
            "bytes": self.total_bytes,
            "formula_safe_cells_prefixed": self.neutralized_cells,
        }))
    }
}

/// Scratch or stage directory, removed on drop unless it was published.
struct TempDir(PathBuf);

impl TempDir {
    fn new(parent: &Path) -> Result<Self, BIExportError> {
        for attempt in 0..100u32 {
            let dir = parent.join(format!(
                ".seohead-bi-{}-{attempt}-{:x}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0)
            ));
            match fs::create_dir(&dir) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
                    }
                    return Ok(TempDir(dir));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(err(format!("cannot create BI staging directory: {e}"))),
            }
        }
        Err(err("cannot create BI staging directory"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Manifest JSON: UTF-8, sorted keys, two-space indent, trailing newline, no NaN.
pub(crate) fn dump_manifest(manifest: &Value) -> Result<Vec<u8>, BIExportError> {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut out = Map::new();
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
                for key in keys {
                    out.insert(key.clone(), sorted(&map[key]));
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    if crate::bi::schema::canonical_json(manifest).is_err() {
        return Err(err("value is not safe JSON evidence"));
    }
    serde_json::to_string_pretty(&sorted(manifest))
        .map(|s| (s + "\n").into_bytes())
        .map_err(|e| err(format!("BI manifest could not be serialized: {e}")))
}

/// Project one run into CSV partitions plus a manifest, then publish them atomically.
#[allow(clippy::too_many_arguments)]
pub fn write_package(
    run: &mut RunInput,
    has_scan_connection: bool,
    destination: &Path,
    providers: &mut [ProviderInput],
    max_rows_per_file: usize,
    max_bytes_per_file: u64,
    max_output_bytes: u64,
    search_metric: Option<&str>,
) -> Result<Value, BIExportError> {
    let mut provider_joins: Vec<Option<Value>> = Vec::new();
    let mut dimension_names: BTreeSet<String> = BTreeSet::new();
    let mut observed: Vec<(usize, Vec<Observation>, Value)> = Vec::new();
    for (index, provider) in providers.iter_mut().enumerate() {
        let join = join_from_provider(provider, run)?;
        provider.join = join.clone();
        provider_joins.push(join.clone());
        if let Some(join) = &join {
            validate_join(join)?;
            let (observations, info) = provider_observations(provider, join)?;
            if let Some(dims) = info.get("dimensions").and_then(Value::as_array) {
                for name in dims {
                    if let Some(name) = name.as_str() {
                        dimension_names.insert(name.to_owned());
                    }
                }
            }
            observed.push((index, observations, info));
        }
    }
    let dimension_fields = safe_dimension_fields(dimension_names.iter().map(String::as_str));
    let metric_fields = metric_fields(&dimension_fields);

    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let index = projection_index(parent, MAX_PROJECTION_INDEX_BYTES)?;
    let mut group_map = GroupIndex::new(&index.con, &mut run.groups)?;
    let inlink_index = if run.source_kind == "scan" && run.links_source_state != "unavailable" {
        Some(InlinkIndex::new(&index.con, run)?)
    } else {
        None
    };
    let stage = TempDir::new(parent)?;
    let budget = OutputBudget::new(max_output_bytes);
    let group_companion = if group_map.source_count > 0 {
        Some(crate::bi_index::write_group_members_companion(
            &mut group_map,
            &stage.0,
            &run.run_id,
            run.audit_sha256.as_deref(),
            max_rows_per_file,
            max_bytes_per_file,
            &budget,
        )?)
    } else {
        None
    };
    let mut dataset_outputs: HashMap<String, Value> = HashMap::new();
    let dataset_states: Vec<(&str, String, Value, &str, Value)> = vec![
        (
            "pages",
            "available".into(),
            Value::Null,
            "retained source page rows",
            json!({"source_rows": run.page_count}),
        ),
        (
            "findings",
            if py::truthy(
                run.run_metadata
                    .get("audit_available")
                    .or(Some(&json!(true))),
            ) {
                "available".into()
            } else {
                "unavailable".into()
            },
            if py::truthy(
                run.run_metadata
                    .get("audit_available")
                    .or(Some(&json!(true))),
            ) {
                Value::Null
            } else {
                json!("scan has no current saved audit; findings were not produced")
            },
            "retained source findings",
            json!({"source_rows": run.finding_count}),
        ),
        (
            "metrics",
            if providers.is_empty() {
                "not_configured".into()
            } else if !provider_joins.iter().any(|j| j.is_some()) {
                "unavailable".into()
            } else if providers.iter().any(|p| p.error_state.is_some()) {
                "partial".into()
            } else {
                "available".into()
            },
            if providers.is_empty() {
                json!("no provider join artifact was supplied")
            } else if !provider_joins.iter().any(|j| j.is_some()) {
                json!("no supplied provider artifact exposes typed metric observations")
            } else if providers.iter().any(|p| p.error_state.is_some()) {
                json!("one or more provider artifacts expose only partial/untyped evidence")
            } else {
                Value::Null
            },
            "provider evidence rows; unmatched populations remain identifiable",
            json!({"provider_sources": providers.len()}),
        ),
        (
            "link_occurrences",
            run.links_source_state.clone(),
            run.links_source_reason
                .as_deref()
                .map_or(Value::Null, |r| json!(r)),
            if has_scan_connection {
                "retained scan links"
            } else {
                "audit does not retain link rows"
            },
            json!({"source_rows": run.link_count}),
        ),
        (
            "cohorts",
            "available".into(),
            Value::Null,
            "derived retained URL observations; unclassified rows retain their reason",
            json!({"search_metric": search_metric, "page_rows": run.page_count}),
        ),
    ];
    for (name, state, reason, population, coverage) in &dataset_states {
        let fields: Vec<Field> = match *name {
            "pages" => PAGE_FIELDS.to_vec(),
            "findings" => FINDING_FIELDS.to_vec(),
            "metrics" => metric_fields.clone(),
            "link_occurrences" => LINK_FIELDS.to_vec(),
            _ => COHORT_FIELDS.to_vec(),
        };
        let mut writer = PartitionWriter::new(
            &stage.0,
            name,
            &fields,
            max_rows_per_file,
            max_bytes_per_file,
            &budget,
        );
        let write_result = (|| -> Result<Value, BIExportError> {
            match *name {
                "pages" => {
                    for (ordinal, page) in (run.pages_factory)().enumerate() {
                        writer.write(&page_row(run, &page, ordinal)?)?;
                    }
                }
                "findings" => {
                    for (ordinal, finding) in (run.findings_factory)().enumerate() {
                        writer.write(&finding_source(run, &finding, ordinal, &group_map)?)?;
                    }
                }
                "metrics" => {
                    let sources: Vec<(&ProviderInput, &Vec<Observation>, &Value)> = observed
                        .iter()
                        .map(|(i, o, inf)| (&providers[*i], o, inf))
                        .collect();
                    for row in metric_rows(run, &sources, &dimension_fields) {
                        writer.write(&row?)?;
                    }
                }
                "link_occurrences" => {
                    if state != "unavailable" {
                        for record in (run.links_factory)() {
                            writer.write(&link_row(run, &record)?)?;
                        }
                    }
                }
                _ => {
                    let sources: Vec<(&ProviderInput, &Vec<Observation>, &Value)> = observed
                        .iter()
                        .map(|(i, o, inf)| (&providers[*i], o, inf))
                        .collect();
                    for row in cohort_rows(
                        run,
                        &sources,
                        search_metric,
                        Some(&index.con),
                        inlink_index.as_ref(),
                    ) {
                        writer.write(&row?)?;
                    }
                }
            }
            writer.finish()
        })();
        writer.close();
        let result = write_result?;
        let expected = coverage.get("source_rows").and_then(Value::as_u64);
        if let Some(expected) = expected {
            if result.get("row_count").and_then(Value::as_u64) != Some(expected) {
                return Err(err(format!(
                    "{name} row conservation failed: source has {expected}, output has {}",
                    result.get("row_count").and_then(Value::as_u64).unwrap_or(0)
                )));
            }
        }
        if name == &"metrics" && !providers.is_empty() {
            let provider_observation_count: usize = observed.iter().map(|(_, o, _)| o.len()).sum();
            if result.get("row_count").and_then(Value::as_u64)
                != Some(provider_observation_count as u64)
            {
                return Err(err("provider metric row conservation failed"));
            }
        }
        let mut output = result.as_object().cloned().unwrap_or_default();
        output.insert("state".to_owned(), json!(state));
        output.insert("reason".to_owned(), reason.clone());
        output.insert("source_population".to_owned(), json!(population));
        output.insert("coverage".to_owned(), coverage.clone());
        dataset_outputs.insert((*name).to_owned(), Value::Object(output));
    }

    let mut coverage_writer = PartitionWriter::new(
        &stage.0,
        "coverage",
        COVERAGE_FIELDS,
        max_rows_per_file,
        max_bytes_per_file,
        &budget,
    );
    let coverage_result = (|| -> Result<Value, BIExportError> {
        for row in coverage_rows(run, providers, &dataset_outputs, &group_map)? {
            coverage_writer.write(&row)?;
        }
        coverage_writer.finish()
    })();
    coverage_writer.close();
    let coverage_result = coverage_result?;
    let mut output = coverage_result.as_object().cloned().unwrap_or_default();
    output.insert("state".to_owned(), json!("available"));
    output.insert("reason".to_owned(), Value::Null);
    output.insert(
        "source_population".to_owned(),
        json!("input artifact and declared check coverage"),
    );
    output.insert("coverage".to_owned(), json!({}));
    dataset_outputs.insert("coverage".to_owned(), Value::Object(output));

    let mut datasets = Map::new();
    for (name, fields) in [
        ("pages", PAGE_FIELDS.to_vec()),
        ("findings", FINDING_FIELDS.to_vec()),
        ("metrics", metric_fields.clone()),
        ("link_occurrences", LINK_FIELDS.to_vec()),
        ("cohorts", COHORT_FIELDS.to_vec()),
        ("coverage", COVERAGE_FIELDS.to_vec()),
    ] {
        let out = dataset_outputs
            .get(name)
            .cloned()
            .unwrap_or_else(|| json!({}));
        datasets.insert(
            name.to_owned(),
            manifest_dataset(
                name,
                &fields,
                &out,
                out.get("state").and_then(Value::as_str).unwrap_or(""),
                out.get("reason"),
                out.get("source_population")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                out.get("row_count").and_then(Value::as_u64).unwrap_or(0) as usize,
                out.get("coverage"),
            ),
        );
    }
    let mut provider_manifest = Vec::new();
    for (provider, join) in providers.iter().zip(provider_joins.iter()) {
        if provider.error_state.is_some() {
            provider_manifest.push(json!({
                "provider_source_id": provider.source_id,
                "file": provider.name,
                "sha256": provider.sha256,
                "bytes": provider.byte_count,
                "state": provider.error_state,
                "reason": provider.error_reason,
                "source_rows": provider.reported_rows,
            }));
            continue;
        }
        let Some(join) = join else {
            provider_manifest.push(json!({
                "provider_source_id": provider.source_id,
                "file": provider.name,
                "sha256": provider.sha256,
                "bytes": provider.byte_count,
                "state": "unsupported",
                "reason": provider.error_reason,
            }));
            continue;
        };
        let collection = provider_collection(join.get("evidence").unwrap_or(&json!({})))?;
        provider_manifest.push(json!({
            "provider_source_id": provider.source_id,
            "file": provider.name,
            "sha256": provider.sha256,
            "bytes": provider.byte_count,
            "state": collection.get("state"),
            "reason": collection.get("reason"),
            "format": join.get("format"),
            "evidence": join.get("evidence"),
            "summary": join.get("summary"),
            "compatibility": provider.compatibility,
        }));
    }
    let mut manifest = json!({
        "format": MANIFEST_FORMAT,
        "schema_version": BI_SCHEMA_VERSION,
        "compatibility": {
            "policy": "v1 field and key changes require a new dataset schema version",
            "dynamic_dimensions": "metrics dataset may append source-declared string dimension columns; mapping is recorded here",
            "null_encoding": "nullable CSV cells are empty only when the companion state/reason columns identify unavailability; numeric zero is emitted as 0",
            "csv_safety": "formula-leading strings are prefixed with an apostrophe; the manifest counts transformed cells",
        },
        "run": Value::Object(run.run_metadata.clone()),
        "input": {
            "kind": run.source_kind,
            "schema": run.source_schema,
            "name": run.source_name,
            "sha256": run.source_sha256,
            "bytes": run.source_bytes,
        },
        "crawl_completeness": {
            "state": run.run_metadata.get("crawl_state").cloned().unwrap_or(json!("unknown")),
            "reason": run.run_metadata.get("crawl_reason").cloned().unwrap_or(Value::Null),
            "verified_by_projection": false,
        },
        "url_key_policy": {
            "version": "external_join.v1",
            "ignore_query": false,
            "ignore_scheme": false,
            "casefold_path": false,
            "source": "spiderhead.checks.external_join.normalize_join_key",
        },
        "provider_sources": provider_manifest,
        "metrics_dimension_columns": Value::Object(dimension_fields.clone()),
        "resource_bounds": {
            "max_pages": MAX_PAGES,
            "max_findings": MAX_FINDINGS,
            "max_link_occurrences": MAX_LINK_OCCURRENCES,
            "max_provider_sources": MAX_PROVIDER_SOURCES,
            "max_provider_rows_per_source": MAX_PROVIDER_ROWS_PER_SOURCE,
            "max_rows_per_partition": max_rows_per_file,
            "max_bytes_per_partition": max_bytes_per_file,
            "max_output_bytes": max_output_bytes,
            "max_output_partitions": max_output_partitions(),
            "max_manifest_bytes": MAX_MANIFEST_BYTES,
            "max_cell_bytes": MAX_CELL_BYTES,
            "max_projection_index_bytes": MAX_PROJECTION_INDEX_BYTES,
            "projection_index_cache_bytes": 2 * 1024 * 1024,
            "min_free_disk_bytes": MIN_FREE_DISK_BYTES,
            "overflow": "fail without publishing any complete package; no truncation or sampling",
        },
        "datasets": Value::Object(datasets),
    });
    if let Some(companion) = &group_companion {
        manifest["group_members"] = companion.clone();
        crate::bi_index::verify_group_member_references(&stage.0, &manifest)?;
    }
    let manifest_bytes = dump_manifest(&manifest)?;
    if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(err("BI manifest exceeds its byte bound"));
    }
    budget.reserve_bytes(manifest_bytes.len() as u64)?;
    let manifest_path = stage.0.join("manifest.json");
    fs::write(&manifest_path, &manifest_bytes)
        .map_err(|e| err(format!("BI manifest write failed: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&stage.0, destination)
        .map_err(|e| err(format!("BI package publication failed: {e}")))?;
    std::mem::forget(stage);
    Ok(json!({
        "format": "spiderhead.bi-export-result.v1",
        "schema_version": BI_SCHEMA_VERSION,
        "manifest": destination.join("manifest.json").to_string_lossy(),
        "output_directory": destination.to_string_lossy(),
        "run_id": run.run_id,
        "crawl_state": run.run_metadata.get("crawl_state").cloned().unwrap_or(Value::Null),
        "datasets": Value::Object(
            dataset_outputs
                .iter()
                .map(|(name, item)| {
                    (
                        name.clone(),
                        json!({
                            "state": item.get("state"),
                            "rows": item.get("row_count"),
                            "partitions": item.get("partition_count"),
                        }),
                    )
                })
                .collect(),
        ),
    }))
}
