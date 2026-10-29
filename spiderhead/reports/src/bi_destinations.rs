//! Offline preflight contracts for optional BI destinations (facade).
//!
//! The destination layer lives in [`crate::bi`]: [`state`] for checkpoints and
//! error types, [`google_client`] for the host-allowlisted Sheets and BigQuery
//! clients, and [`planners`] for plans, apply, filter and xlsx.  This module
//! re-exports the stable names so hosts keep one import surface, including
//! the `SHEETS_MAX_CELLS` tuning knob that Google capacity checks read through.

use std::sync::atomic::{AtomicU64, Ordering};

pub use crate::bi::google_client::{
    a1_column, bq_type, bq_value, google_backoff, host_target_config, register_host_client,
    require_dataset_mapping, resolve_host_client, resolved_target_mapping, sheets_mapping,
    sheets_readback_matches, sheets_values, BeginArgs, BeginOutcome, CommitOutcome,
    DestinationClient, FetchError, Fetcher, GoogleBigQueryClient, GoogleRestClient,
    GoogleSheetsAppendClient, GoogleSheetsClient, LiveFetcher, PendingOutcome, BIGQUERY_SCOPE,
    GOOGLE_CHUNK_ROWS, GOOGLE_CHUNK_SOURCE_BYTES, GOOGLE_REQUEST_BYTES, GOOGLE_RETRIES,
    HOST_CLIENTS, HOST_CONFIG_ENV, SHEETS_SCOPE,
};
pub use crate::bi::planners::{
    advance_pending as _advance_pending, apply_with_client,
    begin_transaction as _begin_transaction, bigquery_plan,
    checksum_and_rows as _checksum_and_rows, checksum_file as _checksum_file, csv_chunks,
    csv_chunks as _csv_chunks, dataset_hashes as _dataset_hashes, destination_preview,
    export_bi_xlsx, external_companion_refs as _external_companion_refs,
    fields_for_manifest as _fields_for_manifest, filter_package, manifest as _manifest,
    pending_write as _pending_write, reconcile_begin as _reconcile_begin,
    reconcile_pending as _reconcile_pending, rows_for_pending as _rows_for_pending,
    segment_page_records as _segment_page_records, sheets_plan,
    validated_fields as _validated_fields, verify_partitions as _verify_partitions,
    EXCEL_MAX_CELL_CHARS, EXCEL_MAX_ROWS, MAX_XLSX_SHEETS,
};
pub use crate::bi::state::{
    accounting as _accounting, bi_err, load_state as _load_state, new_state as _new_state,
    result_from_state as _result_from_state, save_state as _save_state,
    state_identity as _state_identity, state_path as _state_path, uncertain,
    BIDestinationCommitUncertain, BIDestinationError, DestinationError,
    DESTINATION_STATE_DIRECTORY, DESTINATION_STATE_FORMAT,
};
pub use crate::bi::{
    schema::{
        cell_text as _cell_text, dataset_specs as DATASET_SPECS, dyn_field, max_output_partitions,
        safe_dimension_fields as _safe_dimension_fields, BIExportError, Field, BI_SCHEMA_VERSION,
        DEFAULT_BYTES_PER_PARTITION, DEFAULT_MAX_OUTPUT_BYTES, MANIFEST_FORMAT,
        MAX_BYTES_PER_PARTITION, MAX_CELL_BYTES, MAX_MANIFEST_BYTES, MAX_OUTPUT_BYTES,
        MAX_OUTPUT_PARTITIONS, MAX_PROJECTION_INDEX_BYTES, MAX_ROWS_PER_PARTITION,
        MIN_FREE_DISK_BYTES,
    },
    writer::{OutputBudget as _OutputBudget, PartitionWriter as _PartitionWriter},
};

/// `SHEETS_MAX_CELLS`: the tuning knob hosts and tests patch.
pub static SHEETS_MAX_CELLS: AtomicU64 = AtomicU64::new(10_000_000);

/// Cell capacity is read through this facade, so a patched value is the one every caller sees.
pub fn sheets_max_cells() -> i64 {
    SHEETS_MAX_CELLS.load(Ordering::Relaxed) as i64
}
