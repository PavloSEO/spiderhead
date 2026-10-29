//! Export and re-read of the comparison streams: one validated NDJSON file per
//! bucket (optionally gzip), written so identical rows produce identical bytes
//! in separate CLI and MCP output directories, and a validating reader that
//! refuses tampered or truncated files.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use flate2::{Compression, GzBuilder};
use serde_json::{Map, Value};
use sha2::Digest as _;
use spiderhead_store::evidence_contract::Obj;

use crate::compare::base::{os_error_text, CompareError};

use super::shared::{check_cancel, file_sha256_with_size, hex, py_json};

/// Rows between progress notifications and checkpoint commits.
pub(crate) const INDEX_BATCH: i64 = 10_000;

/// Write one comparison stream to `root/<name>.ndjson[.gz]` and return the
/// manifest entry: path, format, row and byte counts and the stored digest.
/// `rows` is drained lazily, so multi-gigabyte buckets never sit in memory.
pub(crate) fn export_one(
    root: &Path,
    name: &str,
    rows: &mut dyn Iterator<Item = Result<Value, CompareError>>,
    compression: &str,
    on_progress: Option<&mut dyn FnMut(i64)>,
    cancel_event: Option<&dyn Fn() -> bool>,
) -> Result<Obj, CompareError> {
    let compressed = compression == "gzip";
    let file_name = format!("{name}.ndjson{}", if compressed { ".gz" } else { "" });
    let path = root.join(&file_name);
    let mut raw = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| CompareError(os_error_text(&error, &path)))?;
    let mut owned_progress = on_progress;
    let mut count: i64 = 0;
    // Uncompressed payload bytes; gzip entries also report the stored size.
    let mut size: i64 = 0;
    let mut stored_size: u64 = 0;
    // Exclude clock and destination path from gzip headers: the same rows must
    // produce the same bytes in separate CLI and MCP output directories.
    let digest_hex = if compressed {
        let gzip = GzBuilder::new().mtime(0);
        let mut stream = gzip.write(&mut raw, Compression::new(6));
        for row in rows {
            let row = row?;
            check_cancel(cancel_event)?;
            let data = format!("{}\n", py_json(&row)).into_bytes();
            stream
                .write_all(&data)
                .map_err(|e| CompareError(e.to_string()))?;
            count += 1;
            size += data.len() as i64;
            if count % INDEX_BATCH == 0 {
                if let Some(progress) = &mut owned_progress {
                    progress(count);
                }
            }
        }
        stream.finish().map_err(|e| CompareError(e.to_string()))?;
        raw.flush().map_err(|e| CompareError(e.to_string()))?;
        raw.sync_all().map_err(|e| CompareError(e.to_string()))?;
        // The stored digest covers the gzip bytes on disk, not the payload.
        let (hash, stored) = file_sha256_with_size(&path)?;
        stored_size = stored;
        hash
    } else {
        let mut digest = sha2::Sha256::new();
        for row in rows {
            let row = row?;
            check_cancel(cancel_event)?;
            let data = format!("{}\n", py_json(&row)).into_bytes();
            digest.update(&data);
            raw.write_all(&data)
                .map_err(|e| CompareError(e.to_string()))?;
            count += 1;
            size += data.len() as i64;
            if count % INDEX_BATCH == 0 {
                if let Some(progress) = &mut owned_progress {
                    progress(count);
                }
            }
        }
        raw.flush().map_err(|e| CompareError(e.to_string()))?;
        raw.sync_all().map_err(|e| CompareError(e.to_string()))?;
        hex(&digest.finalize())
    };
    drop(raw);
    if count % INDEX_BATCH != 0 {
        if let Some(progress) = &mut owned_progress {
            progress(count);
        }
    }
    if !compressed {
        stored_size = size as u64;
    }
    let mut entry = Map::new();
    entry.insert("path".into(), Value::String(file_name));
    entry.insert(
        "format".into(),
        Value::String(if compressed {
            "ndjson.gz".into()
        } else {
            "ndjson".into()
        }),
    );
    entry.insert("rows".into(), Value::from(count));
    entry.insert("bytes".into(), Value::from(stored_size));
    entry.insert("sha256".into(), Value::String(digest_hex));
    if compressed {
        entry.insert("compression".into(), Value::String("gzip".into()));
        entry.insert("uncompressed_bytes".into(), Value::from(size));
    }
    Ok(entry)
}

/// A validated, lazily decoded comparison stream. Manifest validation errors
/// surface on the first pull; framing, row-count and byte-count checks surface
/// while streaming or at exhaustion — the caller must drain the iterator to
/// prove integrity. Any error closes the stream after one report, like a
/// reference generator that raised.
pub struct CompareRows {
    manifest_path: PathBuf,
    name: String,
    started: bool,
    done: bool,
    state: Option<DecodeState>,
}

struct DecodeState {
    reader: Box<dyn BufRead>,
    expected_size: i64,
    expected_rows: i64,
    count: i64,
    size: i64,
}

/// Validate nothing yet; the manifest is read and verified on first pull.
pub(crate) fn iter_compare_rows(manifest_path: &Path, name: &str) -> CompareRows {
    CompareRows {
        manifest_path: manifest_path.to_path_buf(),
        name: name.to_owned(),
        started: false,
        done: false,
        state: None,
    }
}

impl CompareRows {
    fn wrap(&self, error: impl std::fmt::Display) -> CompareError {
        CompareError(format!(
            "invalid comparison file '{}': {}",
            self.name, error
        ))
    }

    fn open(&mut self) -> Result<DecodeState, CompareError> {
        let manifest_text = std::fs::read_to_string(&self.manifest_path)
            .map_err(|error| self.wrap(os_error_text(&error, &self.manifest_path)))?;
        let manifest: Value =
            serde_json::from_str(&manifest_text).map_err(|error| self.wrap(error))?;
        let manifest = manifest
            .as_object()
            .ok_or_else(|| self.wrap("unsupported comparison manifest schema"))?;
        if manifest.get("schema_version").and_then(Value::as_str) != Some(super::COMPARE_V2) {
            return Err(CompareError(
                "unsupported comparison manifest schema".into(),
            ));
        }
        let files = manifest
            .get("files")
            .and_then(Value::as_object)
            .ok_or_else(|| self.wrap("'files'"))?;
        let entry = files
            .get(&self.name)
            .and_then(Value::as_object)
            .ok_or_else(|| self.wrap(format!("'{}'", self.name)))?;
        let relative = entry
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| self.wrap("'path'"))?;
        if Path::new(relative).file_name() != Some(std::ffi::OsStr::new(relative)) {
            return Err(CompareError(
                "comparison file path must be a local filename".into(),
            ));
        }
        let path = self
            .manifest_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(relative);
        if path.is_symlink() {
            return Err(CompareError(
                "comparison file must not be a symbolic link".into(),
            ));
        }
        let format = entry
            .get("format")
            .and_then(Value::as_str)
            .ok_or_else(|| self.wrap("'format'"))?;
        if format != "ndjson" && format != "ndjson.gz" {
            return Err(CompareError("unsupported comparison file format".into()));
        }
        let compressed = format == "ndjson.gz";
        if compressed && entry.get("compression").and_then(Value::as_str) != Some("gzip") {
            return Err(CompareError(
                "comparison compression metadata mismatch".into(),
            ));
        }
        let stored_bytes = entry
            .get("bytes")
            .and_then(Value::as_i64)
            .ok_or_else(|| self.wrap("'bytes'"))?;
        let expected_size = if compressed {
            entry
                .get("uncompressed_bytes")
                .and_then(Value::as_i64)
                .ok_or_else(|| self.wrap("'uncompressed_bytes'"))?
        } else {
            stored_bytes
        };
        let expected_rows = entry
            .get("rows")
            .and_then(Value::as_i64)
            .ok_or_else(|| self.wrap("'rows'"))?;
        for value in [stored_bytes, expected_size, expected_rows] {
            if value < 0 {
                return Err(CompareError(
                    "invalid comparison file count metadata".into(),
                ));
            }
        }
        // The stored-byte hash is verified before the first row is yielded.
        let (stored_digest, read_size) =
            file_sha256_with_size(&path).map_err(|error| self.wrap(error.0))?;
        if read_size as i64 != stored_bytes
            || stored_digest
                != entry
                    .get("sha256")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
        {
            return Err(CompareError(
                "comparison file checksum or byte count mismatch".into(),
            ));
        }
        let file = File::open(&path).map_err(|error| self.wrap(os_error_text(&error, &path)))?;
        let reader: Box<dyn BufRead> = if compressed {
            Box::new(BufReader::new(flate2::read::GzDecoder::new(file)))
        } else {
            Box::new(BufReader::new(file))
        };
        Ok(DecodeState {
            reader,
            expected_size,
            expected_rows,
            count: 0,
            size: 0,
        })
    }
}

impl Iterator for CompareRows {
    type Item = Result<Value, CompareError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if !self.started {
            self.started = true;
            match self.open() {
                Ok(state) => self.state = Some(state),
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
        // Take the state out so no borrow of `self` survives the match arms.
        let mut state = self.state.take()?;
        let mut line = Vec::new();
        match state.reader.read_until(b'\n', &mut line) {
            Ok(0) => {
                self.done = true;
                if state.count != state.expected_rows || state.size != state.expected_size {
                    return Some(Err(CompareError(
                        "comparison file row or decoded byte count mismatch".into(),
                    )));
                }
                None
            }
            Ok(read) => {
                state.size += read as i64;
                state.count += 1;
                if state.size > state.expected_size
                    || state.count > state.expected_rows
                    || !line.ends_with(b"\n")
                {
                    self.done = true;
                    return Some(Err(CompareError(
                        "comparison file row framing or count mismatch".into(),
                    )));
                }
                match serde_json::from_slice::<Value>(&line) {
                    Ok(row) if row.is_object() => {
                        self.state = Some(state);
                        Some(Ok(row))
                    }
                    Ok(_) => {
                        self.done = true;
                        Some(Err(CompareError(
                            "comparison file contains a non-object row".into(),
                        )))
                    }
                    Err(error) => {
                        self.done = true;
                        Some(Err(self.wrap(error)))
                    }
                }
            }
            Err(error) => {
                self.done = true;
                Some(Err(self.wrap(error)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_manifest(dir: &Path, files: Value) -> PathBuf {
        let manifest = dir.join("compare.json");
        std::fs::write(
            &manifest,
            serde_json::to_string(&json!({
                "schema_version": super::super::COMPARE_V2,
                "files": files,
            }))
            .expect("json"),
        )
        .expect("write");
        manifest
    }

    fn export_test_rows(dir: &Path) -> PathBuf {
        let rows = vec![json!({"check": "a"}), json!({"check": "b"})];
        let entry = export_one(
            dir,
            "entered",
            &mut rows.into_iter().map(Ok),
            "none",
            None,
            None,
        )
        .expect("export");
        write_manifest(dir, json!({"entered": entry}))
    }

    #[test]
    fn exported_rows_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = export_test_rows(dir.path());
        let rows: Vec<Value> = iter_compare_rows(&manifest, "entered")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["check"], json!("a"));
    }

    #[test]
    fn tampered_digest_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = export_test_rows(dir.path());
        let data_path = dir.path().join("entered.ndjson");
        let mut text = std::fs::read_to_string(&data_path).expect("read");
        text = text.replace("\"b\"", "\"c\"");
        std::fs::write(&data_path, text).expect("rewrite");
        let error = iter_compare_rows(&manifest, "entered")
            .next()
            .expect("first poll")
            .err()
            .map(|e| e.0);
        assert_eq!(
            error,
            Some("comparison file checksum or byte count mismatch".into())
        );
    }

    #[test]
    fn plain_truncation_is_caught_by_the_digest_gate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = export_test_rows(dir.path());
        let data_path = dir.path().join("entered.ndjson");
        let text = std::fs::read_to_string(&data_path).expect("read");
        let truncated: String = text.lines().take(1).map(|l| format!("{l}\n")).collect();
        std::fs::write(&data_path, truncated).expect("rewrite");
        // The stored-byte hash is verified before yielding, so no row escapes.
        let error = iter_compare_rows(&manifest, "entered")
            .next()
            .expect("first poll")
            .err()
            .map(|e| e.0);
        assert_eq!(
            error,
            Some("comparison file checksum or byte count mismatch".into())
        );
    }

    #[test]
    fn truncation_surfaces_on_drain() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = export_test_rows(dir.path());
        let data_path = dir.path().join("entered.ndjson");
        let text = std::fs::read_to_string(&data_path).expect("read");
        let truncated: String = text.lines().take(1).map(|l| format!("{l}\n")).collect();
        std::fs::write(&data_path, &truncated).expect("rewrite");
        // Re-hash the manifest so only the trailing row count can catch it.
        let mut document: Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).expect("read")).expect("json");
        let entry = document["files"]["entered"]
            .as_object_mut()
            .expect("manifest entry");
        entry.insert("bytes".into(), Value::from(truncated.len() as i64));
        entry.insert(
            "sha256".into(),
            Value::String(hex(&sha2::Sha256::digest(truncated.as_bytes()))),
        );
        std::fs::write(&manifest, py_json(&document)).expect("rewrite manifest");
        let mut iterator = iter_compare_rows(&manifest, "entered");
        assert!(iterator.next().expect("first row").is_ok());
        let second = iterator.next().expect("second poll");
        assert_eq!(
            second.err().map(|e| e.0),
            Some("comparison file row or decoded byte count mismatch".into())
        );
        assert!(iterator.next().is_none(), "an error closes the stream");
    }

    #[test]
    fn unknown_name_is_wrapped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = export_test_rows(dir.path());
        let error = iter_compare_rows(&manifest, "nope")
            .next()
            .expect("first poll")
            .err()
            .map(|e| e.0);
        assert_eq!(error, Some("invalid comparison file 'nope': 'nope'".into()));
    }
}
