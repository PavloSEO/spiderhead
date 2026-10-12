"""Project saved crawl and provider evidence into a typed, offline BI package.

The package is a read-only projection. It does not collect data, calculate SEO
findings, or join provider rows itself: provider URL joins use the shared
``evidence_join`` contract from issue #781. CSV partitions are deterministic,
formula-safe, and written completely or not published at all.
"""

from __future__ import annotations

import csv
import hashlib
import io
import json
import math
import os
import re
import shutil
import sqlite3
import tempfile
from collections import Counter, defaultdict
from collections.abc import Callable, Iterable, Iterator
from contextlib import ExitStack
from dataclasses import dataclass
from datetime import date, datetime
from pathlib import Path
from typing import Any

from seohead.checks.external_join import normalize_join_key
from seohead.storage import open_scan
from seohead.storage.inputs import is_sqlite_input

MANIFEST_FORMAT = "seohead.bi-manifest.v1"
BI_SCHEMA_VERSION = "seohead.bi.v1"
JOIN_FORMAT = "seohead.evidence-join.v1"
NORMALIZED_FORMAT = "seohead.normalized-evidence.v1"
MAX_AUDIT_BYTES = 64 * 1024 * 1024
MAX_SCAN_BYTES = 32 * 1024 * 1024 * 1024
DEFAULT_MAX_SCAN_BYTES = 8 * 1024 * 1024 * 1024
MAX_PROVIDER_JOIN_BYTES = 128 * 1024 * 1024
MAX_PROVIDER_JOIN_TOTAL_BYTES = 256 * 1024 * 1024
MAX_PROVIDER_SOURCES = 8
MAX_PROVIDER_ROWS_PER_SOURCE = 100_000
MAX_PAGES = 1_000_000
MAX_FINDINGS = 1_000_000
MAX_LINK_OCCURRENCES = 5_000_000
MAX_ROWS_PER_PARTITION = 250_000
MAX_BYTES_PER_PARTITION = 64 * 1024 * 1024
MAX_OUTPUT_BYTES = 16 * 1024 * 1024 * 1024
MAX_OUTPUT_ROWS = 10_000_000
MAX_OUTPUT_PARTITIONS = 10_000
MAX_MANIFEST_BYTES = 16 * 1024 * 1024
MAX_CELL_BYTES = 8 * 1024 * 1024
DEFAULT_ROWS_PER_PARTITION = 25_000
DEFAULT_BYTES_PER_PARTITION = 8 * 1024 * 1024
DEFAULT_MAX_OUTPUT_BYTES = 4 * 1024 * 1024 * 1024
MIN_FREE_DISK_BYTES = 16 * 1024 * 1024
MAX_PROJECTION_INDEX_BYTES = 2 * 1024 * 1024 * 1024


class BIExportError(ValueError):
    """A source artifact or bounded BI export violates its declared contract."""


@dataclass(frozen=True)
class Field:
    name: str
    type: str
    nullable: bool = True
    description: str = ""


PAGE_FIELDS = (
    Field("run_id", "string", False, "Stable identity of the source scan or audit."),
    Field("url_observation_id", "string", False, "Stable key for this run and raw URL."),
    Field("source_ordinal", "integer", False, "Source page order, retained for traceability."),
    Field("url", "string", False, "URL exactly as retained by the source artifact."),
    Field("url_key", "string", True, "Strict external_join.v1 key, when derivable."),
    Field("url_key_state", "string", False, "keyed, unkeyable, or not supplied."),
    Field("url_key_reason", "string", True, "Reason a URL key is unavailable."),
    Field("status_code", "integer", True),
    Field("status_code_state", "string", False),
    Field("status_code_reason", "string", True),
    Field("content_type", "string", True),
    Field("content_type_state", "string", False),
    Field("content_type_reason", "string", True),
    Field("indexability", "string", True),
    Field("indexability_state", "string", False),
    Field("indexability_reason", "string", True),
    Field("indexability_status", "string", True),
    Field("indexability_status_state", "string", False),
    Field("indexability_status_reason", "string", True),
    Field("crawl_depth", "integer", True),
    Field("crawl_depth_state", "string", False),
    Field("crawl_depth_reason", "string", True),
    Field("response_time_seconds", "number", True),
    Field("response_time_state", "string", False),
    Field("response_time_reason", "string", True),
    Field("size_bytes", "integer", True),
    Field("size_state", "string", False),
    Field("size_reason", "string", True),
    Field("title", "string", True),
    Field("title_state", "string", False),
    Field("title_reason", "string", True),
    Field("meta_description", "string", True),
    Field("meta_description_state", "string", False),
    Field("meta_description_reason", "string", True),
    Field("h1", "string", True),
    Field("h1_state", "string", False),
    Field("h1_reason", "string", True),
    Field("canonical", "string", True),
    Field("canonical_state", "string", False),
    Field("canonical_reason", "string", True),
    Field("word_count", "integer", True),
    Field("word_count_state", "string", False),
    Field("word_count_reason", "string", True),
    Field("text_ratio", "number", True),
    Field("text_ratio_state", "string", False),
    Field("text_ratio_reason", "string", True),
    Field("inlinks", "integer", True),
    Field("inlinks_state", "string", False),
    Field("inlinks_reason", "string", True),
    Field("unique_inlinks", "integer", True),
    Field("unique_inlinks_state", "string", False),
    Field("unique_inlinks_reason", "string", True),
    Field("internal_outlinks", "integer", True),
    Field("internal_outlinks_state", "string", False),
    Field("internal_outlinks_reason", "string", True),
    Field("outlinks_total", "integer", True),
    Field("outlinks_total_state", "string", False),
    Field("outlinks_total_reason", "string", True),
    Field("external_outlinks", "integer", True),
    Field("external_outlinks_state", "string", False),
    Field("external_outlinks_reason", "string", True),
    Field("link_counts_state", "string", False),
    Field("link_counts_reason", "string", True),
    Field("body_evidence_state", "string", False),
    Field("body_evidence_reason", "string", True),
    Field("representation", "string", True),
    Field("representation_state", "string", False),
    Field("representation_reason", "string", True),
    Field("source_fields_json", "json", False, "Unprojected source fields, preserved as JSON."),
)

FINDING_FIELDS = (
    Field("run_id", "string", False),
    Field("finding_id", "string", False),
    Field("source_finding_id", "string", True),
    Field("source_ordinal", "integer", False),
    Field("finding_kind", "string", False),
    Field("check_id", "string", True),
    Field("check_state", "string", False),
    Field("check_reason", "string", True),
    Field("severity", "string", True),
    Field("severity_state", "string", False),
    Field("severity_reason", "string", True),
    Field("url", "string", True),
    Field("url_state", "string", False),
    Field("url_reason", "string", True),
    Field("url_key", "string", True),
    Field("url_key_state", "string", False),
    Field("url_key_reason", "string", True),
    Field("message", "string", True),
    Field("message_state", "string", False),
    Field("message_reason", "string", True),
    Field("source", "string", True),
    Field("source_state", "string", False),
    Field("source_reason", "string", True),
    Field("status_code", "integer", True),
    Field("status_code_state", "string", False),
    Field("status_code_reason", "string", True),
    Field("occurrences_count", "integer", True),
    Field("occurrences_count_state", "string", False),
    Field("occurrences_count_reason", "string", True),
    Field("group_state", "string", False),
    Field("fix_hint", "string", True),
    Field("fix_hint_state", "string", False),
    Field("fix_hint_reason", "string", True),
    Field("group_id", "string", True),
    Field("group_value", "string", True),
    Field("group_url_count", "integer", True),
    Field("group_urls_json", "json", True),
    Field("locations_json", "json", False),
    Field("details_json", "json", False),
    Field("evidence_json", "json", False),
    Field("source_finding_json", "json", False),
)

METRIC_BASE_FIELDS = (
    Field("run_id", "string", False),
    Field("metric_observation_id", "string", False),
    Field("provider_source_id", "string", False),
    Field("provider", "string", True),
    Field("operation", "string", True),
    Field("reporting_identity", "string", True),
    Field("privacy", "string", True),
    Field("metric_name", "string", False),
    Field("metric_unit", "string", True),
    Field("metric_unit_state", "string", False),
    Field("metric_unit_reason", "string", True),
    Field("value_number", "number", True),
    Field("value_state", "string", False),
    Field("value_reason", "string", True),
    Field("raw_value_json", "json", True),
    Field("url_raw", "string", True),
    Field("url_resolved", "string", True),
    Field("url_key", "string", True),
    Field("url_state", "string", False),
    Field("url_reason", "string", True),
    Field("period_start", "date", True),
    Field("period_end", "date", True),
    Field("period_state", "string", False),
    Field("period_reason", "string", True),
    Field("timezone", "string", True),
    Field("timezone_state", "string", False),
    Field("timezone_reason", "string", True),
    Field("attribution", "string", True),
    Field("attribution_state", "string", False),
    Field("attribution_reason", "string", True),
    Field("search_engine", "string", True),
    Field("search_engine_state", "string", False),
    Field("search_engine_reason", "string", True),
    Field("search_type", "string", True),
    Field("search_type_state", "string", False),
    Field("search_type_reason", "string", True),
    Field("collection_state", "string", False),
    Field("collection_reason", "string", True),
    Field("collection_sampled", "boolean", True),
    Field("collection_thresholded", "boolean", True),
    Field("collection_truncated", "boolean", True),
    Field("source_row_index", "integer", False),
    Field("natural_key_sha256", "string", False),
    Field("ambiguous_source_row", "boolean", False),
    Field("population_state", "string", False),
    Field("matched_page_count", "integer", False),
    Field("matched_page_urls_json", "json", False),
    Field("dimensions_state_json", "json", False),
    Field("source_row_json", "json", False),
    Field("source_metadata_json", "json", False),
)

LINK_FIELDS = (
    Field("run_id", "string", False),
    Field("link_occurrence_id", "string", False),
    Field("source_url", "string", False),
    Field("source_url_key", "string", True),
    Field("source_url_key_state", "string", False),
    Field("source_url_key_reason", "string", True),
    Field("destination_url", "string", False),
    Field("destination_url_key", "string", True),
    Field("destination_url_key_state", "string", False),
    Field("destination_url_key_reason", "string", True),
    Field("link_ordinal", "integer", False),
    Field("evidence_representation", "string", False),
    Field("link_type", "string", True),
    Field("link_type_state", "string", False),
    Field("link_type_reason", "string", True),
    Field("anchor", "string", True),
    Field("anchor_state", "string", False),
    Field("anchor_reason", "string", True),
    Field("nofollow", "boolean", True),
    Field("nofollow_state", "string", False),
    Field("nofollow_reason", "string", True),
    Field("rel_json", "json", True),
    Field("rel_state", "string", False),
    Field("rel_reason", "string", True),
    Field("target", "string", True),
    Field("target_state", "string", False),
    Field("target_reason", "string", True),
    Field("placement", "string", True),
    Field("placement_state", "string", False),
    Field("placement_reason", "string", True),
    Field("dom_context_json", "json", True),
    Field("dom_context_state", "string", False),
    Field("dom_context_reason", "string", True),
    Field("source_document_id", "integer", True),
    Field("source_document_state", "string", False),
    Field("source_document_reason", "string", True),
    Field("raw_href", "string", True),
    Field("source_link_id", "integer", False),
    Field("source_link_json", "json", False),
)

COVERAGE_FIELDS = (
    Field("run_id", "string", False),
    Field("dataset", "string", False),
    Field("evidence_source_id", "string", False),
    Field("population", "string", False),
    Field("state", "string", False),
    Field("reason", "string", True),
    Field("source_rows", "integer", True),
    Field("exported_rows", "integer", True),
    Field("unavailable_rows", "integer", True),
    Field("details_json", "json", False),
)

# Cohorts deliberately remain URL observations rather than aggregate claims.  A
# report can group them, while retaining the input values, scope, and every
# reason an observation could not be classified.
COHORT_FIELDS = (
    Field("run_id", "string", False),
    Field("cohort_observation_id", "string", False),
    Field("cohort_id", "string", False),
    Field("definition_version", "string", False),
    Field("definition", "string", False),
    Field("url", "string", True),
    Field("url_key", "string", True),
    Field("url_key_state", "string", False),
    Field("url_key_reason", "string", True),
    Field("membership", "string", False),
    Field("state", "string", False),
    Field("reason", "string", True),
    Field("value_label", "string", True),
    Field("value_number", "number", True),
    Field("threshold", "number", True),
    Field("numerator", "integer", True),
    Field("denominator", "integer", True),
    Field("extraction_coverage_state", "string", False),
    Field("extraction_coverage_reason", "string", True),
    Field("search_metric", "string", True),
    Field("search_value", "number", True),
    Field("search_value_state", "string", False),
    Field("sessions_value", "number", True),
    Field("sessions_value_state", "string", False),
    Field("period_start", "date", True),
    Field("period_end", "date", True),
    Field("timezone", "string", True),
    Field("source_metric_observations_json", "json", False),
)

DATASET_SPECS = {
    "pages": (
        PAGE_FIELDS,
        "one URL observation per run and raw URL",
        ("run_id", "url_observation_id"),
    ),
    "findings": (
        FINDING_FIELDS,
        "one finding/check observation per run and source finding",
        ("run_id", "finding_id"),
    ),
    "metrics": (
        METRIC_BASE_FIELDS,
        "one source metric observation per provider row and metric at its declared dimension/period grain",
        ("run_id", "provider_source_id", "metric_observation_id"),
    ),
    "link_occurrences": (
        LINK_FIELDS,
        "one row per retained source-to-target link occurrence",
        ("run_id", "link_occurrence_id"),
    ),
    "coverage": (
        COVERAGE_FIELDS,
        "one row per dataset, provider population, or check coverage state",
        ("run_id", "dataset", "evidence_source_id", "population"),
    ),
    "cohorts": (
        COHORT_FIELDS,
        "one evidence-qualified URL cohort observation per run, URL, and cohort definition",
        ("run_id", "cohort_observation_id"),
    ),
}


def _canonical_json(value: Any) -> str:
    try:
        return json.dumps(
            value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False
        )
    except (TypeError, ValueError) as exc:
        raise BIExportError(f"value is not safe JSON evidence: {exc}") from exc


def _digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _digest_parts(*values: str) -> str:
    return hashlib.sha256("\0".join(values).encode("utf-8")).hexdigest()


def _unique_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise BIExportError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def _reject_constant(value: str) -> None:
    raise BIExportError(f"invalid JSON numeric constant {value!r}")


def _load_json_bytes(raw: bytes, label: str) -> dict[str, Any]:
    try:
        value = json.loads(
            raw.decode("utf-8-sig"),
            object_pairs_hook=_unique_pairs,
            parse_constant=_reject_constant,
        )
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise BIExportError(f"{label} is not valid UTF-8 JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise BIExportError(f"{label} must be a JSON object")
    return value


def _read_json_path(
    path_value: str | os.PathLike[str], label: str, max_bytes: int
) -> tuple[dict[str, Any], bytes]:
    if not isinstance(path_value, (str, os.PathLike)) or not str(path_value).strip():
        raise BIExportError(f"{label} must be a non-empty local file path")
    path = Path(path_value)
    try:
        if path.is_symlink() or not path.is_file():
            raise BIExportError(f"{label} must be a regular non-symlink file")
        before = path.stat()
        if before.st_size > max_bytes:
            raise BIExportError(f"{label} exceeds the {max_bytes}-byte input bound")
        with path.open("rb") as stream:
            raw = stream.read(max_bytes + 1)
        after = path.stat()
    except BIExportError:
        raise
    except OSError as exc:
        raise BIExportError(f"cannot read {label}: {exc.strerror or exc}") from exc
    if len(raw) > max_bytes:
        raise BIExportError(f"{label} exceeds the {max_bytes}-byte input bound")
    if (before.st_size, before.st_mtime_ns, before.st_ino) != (
        after.st_size,
        after.st_mtime_ns,
        after.st_ino,
    ) or len(raw) != after.st_size:
        raise BIExportError(f"{label} changed while it was read")
    return _load_json_bytes(raw, label), raw


def _sha256_path(path: Path, max_bytes: int, label: str) -> tuple[str, int]:
    try:
        if path.is_symlink() or not path.is_file():
            raise BIExportError(f"{label} must be a regular non-symlink file")
        before = path.stat()
        if before.st_size > max_bytes:
            raise BIExportError(f"{label} exceeds the {max_bytes}-byte input bound")
        digest = hashlib.sha256()
        read = 0
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                read += len(chunk)
                if read > max_bytes:
                    raise BIExportError(f"{label} exceeds the {max_bytes}-byte input bound")
                digest.update(chunk)
        after = path.stat()
    except BIExportError:
        raise
    except OSError as exc:
        raise BIExportError(f"cannot hash {label}: {exc.strerror or exc}") from exc
    if (before.st_size, before.st_mtime_ns, before.st_ino) != (
        after.st_size,
        after.st_mtime_ns,
        after.st_ino,
    ) or read != after.st_size:
        raise BIExportError(f"{label} changed while it was read")
    return digest.hexdigest(), read


def _json_value(value: Any) -> Any:
    if isinstance(value, (dict, list, str, int, bool)) or value is None:
        return value
    if isinstance(value, float) and math.isfinite(value):
        return value
    if isinstance(value, float):
        return None
    if isinstance(value, (date, datetime)):
        return value.isoformat()
    return str(value)


_SENSITIVE_SETTING_TOKENS = (
    "authorization",
    "cookie",
    "password",
    "secret",
    "token",
    "api_key",
    "private_key",
)


def _redact_setting(value: Any, key: str = "") -> Any:
    if any(token in key.casefold() for token in _SENSITIVE_SETTING_TOKENS):
        return "REDACTED"
    if isinstance(value, dict):
        return {str(name): _redact_setting(child, str(name)) for name, child in value.items()}
    if isinstance(value, list):
        return [_redact_setting(child, key) for child in value]
    return _json_value(value)


def _cell_text(value: Any) -> str:
    from seohead.reports import neutralize_formula

    if value is None:
        return ""
    if isinstance(value, bool):
        return "true" if value else "false"
    return str(neutralize_formula(value))


def _format_cell(value: Any, field_type: str, field_name: str) -> tuple[str, bool]:
    if value is None:
        return "", False
    if field_type == "boolean":
        if type(value) is not bool:
            raise BIExportError(f"{field_name} must be boolean or null")
        return ("true" if value else "false"), False
    if field_type == "integer":
        if type(value) is not int:
            raise BIExportError(f"{field_name} must be integer or null")
        return str(value), False
    if field_type == "number":
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise BIExportError(f"{field_name} must be numeric or null")
        if isinstance(value, float) and not math.isfinite(value):
            raise BIExportError(f"{field_name} must be finite")
        return repr(value) if isinstance(value, float) else str(value), False
    if field_type == "date":
        if not isinstance(value, str):
            raise BIExportError(f"{field_name} must be an ISO date or null")
        try:
            return date.fromisoformat(value).isoformat(), False
        except ValueError as exc:
            raise BIExportError(f"{field_name} must be an ISO date or null") from exc
    if field_type == "json":
        return _canonical_json(value), False
    if field_type == "string":
        if not isinstance(value, str):
            raise BIExportError(f"{field_name} must be a string or null")
        safe = _cell_text(value)
        return safe, safe != value
    raise BIExportError(f"unsupported BI field type {field_type!r}")


class _PartitionWriter:
    def __init__(
        self,
        directory: Path,
        dataset: str,
        fields: tuple[Field, ...],
        *,
        max_rows_per_file: int,
        max_bytes_per_file: int,
        budget: _OutputBudget,
    ) -> None:
        self.directory = directory
        self.dataset = dataset
        self.fields = fields
        self.max_rows_per_file = max_rows_per_file
        self.max_bytes_per_file = max_bytes_per_file
        self.budget = budget
        self.total_bytes = 0
        self.total_rows = 0
        self._disk_check_at = 0
        self.neutralized_cells = 0
        self.partitions: list[dict[str, Any]] = []
        self._stream = None
        self._digest = None
        self._path: Path | None = None
        self._partition_rows = 0
        self._partition_bytes = 0
        self._header = self._encode([field.name for field in fields], header=True)

    @staticmethod
    def _encode(values: list[str], *, header: bool = False) -> bytes:
        buffer = io.StringIO(newline="")
        writer = csv.writer(buffer, quoting=csv.QUOTE_MINIMAL, lineterminator="\n")
        writer.writerow(values)
        return buffer.getvalue().encode("utf-8")

    def _open_partition(self) -> None:
        self.budget.reserve_partition()
        index = len(self.partitions) + 1
        self._path = self.directory / f"{self.dataset}-{index:04d}.csv"
        self._stream = self._path.open("wb")
        os.chmod(self._path, 0o600)
        self._digest = hashlib.sha256()
        self._partition_rows = 0
        self._partition_bytes = 0
        if len(self._header) > self.max_bytes_per_file:
            raise BIExportError(f"{self.dataset} CSV header exceeds the partition byte limit")
        self._write_bytes(self._header)

    def _write_bytes(self, content: bytes) -> None:
        if self.total_bytes >= self._disk_check_at or len(content) >= 1024 * 1024:
            if shutil.disk_usage(self.directory).free < MIN_FREE_DISK_BYTES + len(content):
                raise BIExportError(
                    "insufficient free disk for BI output; no package was published"
                )
            self._disk_check_at = self.total_bytes + 1024 * 1024
        self.budget.reserve_bytes(len(content))
        self._stream.write(content)
        self._digest.update(content)
        self._partition_bytes += len(content)
        self.total_bytes += len(content)

    def write(self, row: dict[str, Any]) -> None:
        if self.total_rows >= MAX_OUTPUT_ROWS:
            raise BIExportError(
                f"{self.dataset} exceeds the {MAX_OUTPUT_ROWS}-row hard bound; no package was published"
            )
        if set(row) - {field.name for field in self.fields}:
            raise BIExportError(f"{self.dataset} row contains undeclared fields")
        values: list[str] = []
        for field in self.fields:
            value = row.get(field.name)
            if value is None and not field.nullable:
                raise BIExportError(f"{self.dataset}.{field.name} is not nullable")
            cell, neutralized = _format_cell(value, field.type, f"{self.dataset}.{field.name}")
            self.neutralized_cells += int(neutralized)
            if len(cell.encode("utf-8")) > MAX_CELL_BYTES:
                raise BIExportError(
                    f"{self.dataset}.{field.name} exceeds the {MAX_CELL_BYTES}-byte cell bound"
                )
            values.append(cell)
        self.write_cells(values)

    def write_cells(self, values: list[str]) -> None:
        """Write already serialized CSV values without reinterpreting their typed grain."""
        if len(values) != len(self.fields):
            raise BIExportError("CSV row width differs from the selected field schema")
        if any(len(value.encode("utf-8")) > MAX_CELL_BYTES for value in values):
            raise BIExportError(f"CSV value exceeds the {MAX_CELL_BYTES}-byte cell bound")
        if self.total_rows >= MAX_OUTPUT_ROWS:
            raise BIExportError("selected output exceeds the row bound")
        encoded = self._encode(values)
        if len(encoded) + len(self._header) > self.max_bytes_per_file:
            raise BIExportError(
                f"one {self.dataset} row exceeds the configured partition byte limit"
            )
        if self._stream is None or (
            self._partition_rows >= self.max_rows_per_file
            or self._partition_bytes + len(encoded) > self.max_bytes_per_file
        ):
            self._close_partition()
            self._open_partition()
        self._write_bytes(encoded)
        self._partition_rows += 1
        self.total_rows += 1

    def _close_partition(self) -> None:
        if self._stream is None or self._path is None or self._digest is None:
            return
        self._stream.flush()
        os.fsync(self._stream.fileno())
        self._stream.close()
        self.partitions.append(
            {
                "path": self._path.name,
                "rows": self._partition_rows,
                "bytes": self._partition_bytes,
                "sha256": self._digest.hexdigest(),
            }
        )
        self._stream = None
        self._digest = None
        self._path = None

    def close(self) -> None:
        """Release an interrupted writer without publishing metadata."""
        if self._stream is not None:
            self._stream.close()
            self._stream = None

    def finish(self) -> dict[str, Any]:
        if self._stream is None:
            self._open_partition()
        self._close_partition()
        return {
            "row_count": self.total_rows,
            "partition_count": len(self.partitions),
            "partitions": self.partitions,
            "bytes": self.total_bytes,
            "formula_safe_cells_prefixed": self.neutralized_cells,
        }


class _OutputBudget:
    def __init__(self, max_bytes: int) -> None:
        self.max_bytes = max_bytes
        self.bytes = 0
        self.partitions = 0

    def reserve_partition(self) -> None:
        if self.partitions >= MAX_OUTPUT_PARTITIONS:
            raise BIExportError(
                "too many CSV partitions; increase max_rows_per_file or max_bytes_per_file"
            )
        self.partitions += 1

    def reserve_bytes(self, amount: int) -> None:
        if self.bytes + amount > self.max_bytes:
            raise BIExportError(
                f"total output exceeds the {self.max_bytes}-byte bound; no package was published"
            )
        self.bytes += amount


@dataclass
class _RunInput:
    run_id: str
    source_kind: str
    source_schema: str
    source_name: str | None
    source_sha256: str
    source_bytes: int
    run_metadata: dict[str, Any]
    pages_factory: Any
    page_count: int
    findings_factory: Any
    finding_count: int
    groups: Iterable[dict[str, Any]]
    links_factory: Any
    links_source_state: str
    links_source_reason: str | None
    coverage_rows: list[dict[str, Any]]
    link_count: int | None
    audit_sha256: str | None = None
    close: Any = None


@dataclass
class _ProviderInput:
    source_id: str
    name: str | None
    sha256: str
    byte_count: int
    join: dict[str, Any] | None
    compatibility: dict[str, Any] | None
    normalized: dict[str, Any] | None
    store: Any = None
    error_state: str | None = None
    error_reason: str | None = None
    reported_rows: int | None = None


@dataclass(frozen=True)
class _ObservationStream:
    """Re-iterable, counted observations without a provider-row list."""

    factory: Callable[[], Iterator[dict[str, Any]]]
    count: int

    def __iter__(self) -> Iterator[dict[str, Any]]:
        return self.factory()

    def __len__(self) -> int:
        return self.count


def _state_value(
    value: Any, *, reason: str = "source field is null"
) -> tuple[Any, str, str | None]:
    if value is None:
        return None, "unavailable", reason
    return value, "measured", None


def _audit_kind(document: dict[str, Any]) -> str:
    from seohead.reports import _detect_kind

    kind, error = _detect_kind(document)
    if error:
        raise BIExportError(error)
    if kind is None:
        raise BIExportError(
            "audit does not match a supported site-audit/1 or SF audit.json contract"
        )
    if kind == "sf-audit":
        from seohead.storage import _audit

        try:
            _audit(_canonical_json(document))
        except Exception as exc:
            raise BIExportError(f"audit failed the saved audit schema validation: {exc}") from exc
    else:
        if (
            document.get("schema") != "seohead.site-audit/1"
            or not isinstance(document.get("pages"), list)
            or not isinstance(document.get("findings"), list)
            or not isinstance(document.get("summary"), dict)
        ):
            raise BIExportError("site audit has invalid or missing pages/findings/summary")
    return kind


def _audit_source(document: dict[str, Any], raw: bytes, source_name: str | None) -> _RunInput:
    kind = _audit_kind(document)
    run = document.get("run") if kind == "sf-audit" else {}
    run = run if isinstance(run, dict) else {}
    pages = document.get("pages")
    findings = document.get("issues") if kind == "sf-audit" else document.get("findings")
    groups = document.get("groups") if kind == "sf-audit" else []
    if not isinstance(pages, list) or not isinstance(findings, list):
        raise BIExportError("validated audit pages and findings must be lists")
    if len(pages) > MAX_PAGES:
        raise BIExportError(f"audit page rows exceed the {MAX_PAGES}-row source bound")
    if len(findings) > MAX_FINDINGS:
        raise BIExportError(f"audit findings exceed the {MAX_FINDINGS}-row source bound")
    if any(not isinstance(page, dict) for page in pages):
        raise BIExportError("audit contains a non-object page row")
    page_urls = [_page_url(page) for page in pages]
    if len(set(page_urls)) != len(page_urls):
        raise BIExportError(
            "source audit has duplicate raw page URLs; page rows cannot be keyed uniquely"
        )
    if any(not isinstance(finding, dict) for finding in findings):
        raise BIExportError("audit contains a non-object finding row")
    raw_sha = _digest(raw)
    source_schema = document.get("schema_version") or document.get("schema")
    run_id = run.get("scan_uuid")
    if not isinstance(run_id, str) or not run_id:
        run_id = f"audit:{raw_sha[:24]}"
    partial = run.get("crawl_partial")
    crawl_valid = run.get("crawl_valid")
    if crawl_valid is False:
        crawl_state, crawl_reason = (
            "failed",
            run.get("crawl_invalid_reason") or "audit marks crawl invalid",
        )
    elif partial is True:
        crawl_state, crawl_reason = (
            "partial",
            run.get("crawl_finish_reason") or "source audit is partial",
        )
    elif run.get("input_mode") == "crawl":
        crawl_state, crawl_reason = "complete", None
    else:
        crawl_state, crawl_reason = (
            "unknown",
            "audit source does not establish a completed native crawl",
        )
    audit_summary = document.get("summary") if isinstance(document.get("summary"), dict) else {}
    check_coverage = audit_summary.get("check_coverage") or {}
    coverage_rows = _check_coverage_rows(run_id, run, check_coverage)
    check_skips = {
        str(item.get("id")): str(item.get("reason") or "source check was skipped")
        for item in (run.get("checks_skipped") or [])
        if isinstance(item, dict) and item.get("id")
    }
    if kind == "site-audit":
        for tool in audit_summary.get("tools_run") or []:
            if isinstance(tool, str):
                coverage_rows.append(
                    _coverage_row(
                        run_id,
                        "tools",
                        tool,
                        "tool",
                        "completed",
                        None,
                        1,
                        1,
                        0,
                        {"tool": tool},
                    )
                )
        for failure in audit_summary.get("tools_failed") or []:
            if isinstance(failure, dict):
                tool = str(failure.get("tool") or "unknown")
                coverage_rows.append(
                    _coverage_row(
                        run_id,
                        "tools",
                        tool,
                        "tool",
                        "failed",
                        str(failure.get("error") or "site audit tool failed"),
                        1,
                        0,
                        1,
                        failure,
                    )
                )
        for failure in audit_summary.get("page_tools_failed") or []:
            if isinstance(failure, dict):
                tool = str(failure.get("tool") or "unknown")
                failed_pages = failure.get("failed_pages")
                pages_checked = failure.get("pages_checked")
                if type(failed_pages) is not int or failed_pages < 0:
                    failed_pages = None
                if type(pages_checked) is not int or pages_checked < 0:
                    pages_checked = None
                succeeded = (
                    pages_checked - failed_pages
                    if pages_checked is not None and failed_pages is not None
                    else None
                )
                coverage_rows.append(
                    _coverage_row(
                        run_id,
                        "tools",
                        tool,
                        "page_tool",
                        "failed"
                        if failed_pages == pages_checked and pages_checked is not None
                        else "partial",
                        "page tool failed for one or more URLs",
                        pages_checked,
                        succeeded,
                        failed_pages,
                        failure,
                    )
                )
    metadata = {
        "run_id": run_id,
        "source_kind": kind,
        "source_schema": source_schema,
        "source_name": source_name,
        "source_sha256": raw_sha,
        "source_bytes": len(raw),
        "started_at": run.get("started_at"),
        "finished_at": run.get("finished_at")
        or run.get("generated_at")
        or document.get("generated_at"),
        "target_scope": run.get("source")
        or run.get("project")
        or document.get("url")
        or document.get("domain"),
        "crawl_state": crawl_state,
        "crawl_reason": crawl_reason,
        "crawl_settings": _redact_setting(run.get("crawl_config"))
        if run.get("crawl_config") is not None
        else None,
        "input_mode": run.get("input_mode"),
        "summary": audit_summary,
        "check_skips": check_skips,
    }
    unavailable_links = "audit document does not retain a complete link-occurrence inventory"
    if groups is None:
        groups = []
    if not isinstance(groups, list) or any(not isinstance(group, dict) for group in groups):
        raise BIExportError("audit groups must be a list of objects")
    metadata["audit_sha256"] = raw_sha
    return _RunInput(
        run_id=run_id,
        source_kind=kind,
        source_schema=str(source_schema or "unknown"),
        source_name=source_name,
        source_sha256=raw_sha,
        source_bytes=len(raw),
        run_metadata=metadata,
        pages_factory=lambda: iter(pages),
        page_count=len(pages),
        findings_factory=lambda: iter(findings),
        finding_count=len(findings),
        groups=groups,
        links_factory=lambda: iter(()),
        links_source_state="unavailable",
        links_source_reason=unavailable_links,
        coverage_rows=coverage_rows,
        link_count=None,
        audit_sha256=raw_sha,
    )


def _read_audit_input(value: Any) -> tuple[dict[str, Any], bytes, str | None]:
    if isinstance(value, dict):
        raw = _canonical_json(value).encode("utf-8")
        if len(raw) > MAX_AUDIT_BYTES:
            raise BIExportError(f"inline audit exceeds the {MAX_AUDIT_BYTES}-byte bound")
        return value, raw, None
    raw_value = str(value) if isinstance(value, os.PathLike) else value
    if not isinstance(raw_value, str) or not raw_value.strip():
        raise BIExportError("audit must be an audit document or local path")
    path = Path(raw_value)
    if is_sqlite_input(path):
        raise BIExportError("SQLite scan paths must be supplied with --scan, not --audit")
    document, raw = _read_json_path(path, "audit", MAX_AUDIT_BYTES)
    return document, raw, path.name


def _check_coverage_rows(
    run_id: str, run: dict[str, Any], coverage: dict[str, Any]
) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for key, label in (("checks_skipped", "skipped"), ("checks_disabled", "disabled")):
        entries = run.get(key)
        if not isinstance(entries, list):
            continue
        for index, item in enumerate(entries):
            if not isinstance(item, dict):
                continue
            check_id = item.get("id") or item.get("check") or f"{label}:{index}"
            rows.append(
                {
                    "run_id": run_id,
                    "dataset": "checks",
                    "evidence_source_id": str(check_id),
                    "population": "check",
                    "state": label,
                    "reason": item.get("reason"),
                    "source_rows": 1,
                    "exported_rows": 0,
                    "unavailable_rows": 1 if label == "skipped" else 0,
                    "details_json": item,
                }
            )
    silent = coverage.get("checks_silent_ids") if isinstance(coverage, dict) else None
    if isinstance(silent, list):
        for check_id in silent:
            if isinstance(check_id, str):
                rows.append(
                    {
                        "run_id": run_id,
                        "dataset": "checks",
                        "evidence_source_id": check_id,
                        "population": "check",
                        "state": "silent",
                        "reason": "source audit recorded a silent completed check",
                        "source_rows": 1,
                        "exported_rows": 0,
                        "unavailable_rows": 0,
                        "details_json": {"check_id": check_id},
                    }
                )
    return rows


def _scan_source(
    path_value: str | os.PathLike[str],
    con: sqlite3.Connection,
    *,
    max_scan_bytes: int,
    index_parent: Path,
) -> _RunInput:
    path = Path(path_value)
    scan_sha, scan_bytes = _sha256_path(path, max_scan_bytes, "scan")
    row = con.execute("SELECT * FROM scan WHERE singleton=1").fetchone()
    if row is None:
        raise BIExportError("validated scan is missing its run manifest")
    scan = dict(row)
    try:
        audit_row = con.execute(
            "SELECT document_json,sha256 FROM audit WHERE singleton=1"
        ).fetchone()
        from seohead.storage.audit_v2 import AuditV2Reader, audit_v2_path

        audit_reader = AuditV2Reader(path) if audit_v2_path(path).exists() else None
        if audit_reader is not None:
            # Keep issue rows in their companion and reopen the ordered cursor
            # for each consumer.  Large audit.v2 findings never pass through
            # legacy JSON materialization.
            audit = audit_reader.header
        else:
            audit = json.loads(audit_row[0]) if audit_row else None
        capabilities = json.loads(scan["capabilities_json"])
        limitations = json.loads(scan["limitations_json"])
        if scan["source_kind"] in {"native", "reanalysis"}:
            from seohead.storage.native_scan import read_effective_config

            settings_json = read_effective_config(con)
        else:
            settings_json = json.loads(scan["config_json"])
        retention = json.loads(scan["retention_json"])
    except (TypeError, ValueError, json.JSONDecodeError) as exc:
        raise BIExportError(f"validated scan metadata could not be decoded: {exc}") from exc
    if not isinstance(capabilities, dict) or not isinstance(settings_json, dict):
        raise BIExportError("validated scan capabilities or settings are malformed")
    page_count = int(con.execute("SELECT COUNT(*) FROM pages").fetchone()[0])
    if page_count > MAX_PAGES:
        raise BIExportError(f"scan pages exceed the {MAX_PAGES}-row source bound")
    if con.execute(
        "SELECT 1 FROM pages p JOIN urls u USING(url_id) GROUP BY u.url HAVING COUNT(*) > 1 LIMIT 1"
    ).fetchone():
        raise BIExportError(
            "retained scan has duplicate raw page URLs; page rows cannot be keyed uniquely"
        )
    findings = audit.get("issues") if isinstance(audit, dict) else []
    groups = audit.get("groups") if isinstance(audit, dict) else []
    findings = findings if isinstance(findings, list) else []
    groups = groups if isinstance(groups, list) else []
    if audit_reader is not None:

        def findings_factory():
            return audit_reader.iter_collection("/issues")

        finding_count = audit_reader.count("/issues")
        groups = (
            _ObservationStream(
                lambda: audit_reader.iter_collection("/groups"), audit_reader.count("/groups")
            )
            if "/groups" in audit_reader.collections
            else []
        )
        audit_page_rows = (
            audit_reader.iter_collection("/pages") if "/pages" in audit_reader.collections else ()
        )
    else:

        def findings_factory():
            return iter(findings)

        finding_count = len(findings)
        audit_page_rows = (audit.get("pages") or []) if isinstance(audit, dict) else []
    audit_pages = (
        {
            page.get("url"): {
                "indexability": page.get("indexability"),
                "indexability_status": page.get("indexability_status"),
            }
            for page in audit_page_rows
            if isinstance(page, dict) and isinstance(page.get("url"), str)
        }
        if audit_reader is None
        else None
    )
    audit_overlay_path: Path | None = None
    audit_overlay_temp: tempfile.TemporaryDirectory[str] | None = None
    if audit_reader is not None and "/pages" in audit_reader.collections:
        # audit.v2 preserves its own collection order, which is not a contract
        # with the retained crawl's page ordinal.  A disk-backed URL index keeps
        # the projection re-iterable at one million pages without either a
        # positional zip or a million-entry Python dictionary.
        audit_overlay_temp = tempfile.TemporaryDirectory(
            prefix=".seohead-bi-audit-pages-", dir=index_parent
        )
        audit_overlay_path = Path(audit_overlay_temp.name) / "pages.sqlite"
        overlay_con = sqlite3.connect(audit_overlay_path)
        try:
            overlay_con.execute("PRAGMA cache_size=-2048")
            overlay_con.execute("PRAGMA journal_mode=OFF")
            overlay_con.execute(f"PRAGMA max_page_count={MAX_PROJECTION_INDEX_BYTES // 4096}")
            overlay_con.execute(
                "CREATE TABLE overlays (url TEXT PRIMARY KEY, indexability_json TEXT, "
                "indexability_status_json TEXT)"
            )
            for overlay in audit_reader.iter_collection("/pages"):
                if not isinstance(overlay, dict) or not isinstance(overlay.get("url"), str):
                    raise BIExportError("saved audit page lacks a URL for streaming projection")
                values = (
                    overlay["url"],
                    _canonical_json(overlay.get("indexability")),
                    _canonical_json(overlay.get("indexability_status")),
                )
                if any(len(value.encode("utf-8")) > MAX_CELL_BYTES for value in values):
                    raise BIExportError("one audit page overlay exceeds the BI cell bound")
                overlay_con.execute("INSERT INTO overlays VALUES (?,?,?)", values)
            overlay_con.commit()
        except sqlite3.IntegrityError as exc:
            raise BIExportError("saved audit has duplicate URL overlays") from exc
        finally:
            overlay_con.close()
    if finding_count > MAX_FINDINGS:
        raise BIExportError(f"scan findings exceed the {MAX_FINDINGS}-row source bound")
    link_count = int(con.execute("SELECT COUNT(*) FROM links").fetchone()[0])
    if link_count > MAX_LINK_OCCURRENCES:
        raise BIExportError(
            f"scan link occurrences exceed the {MAX_LINK_OCCURRENCES}-row source bound"
        )
    link_capability = capabilities.get("links")
    if isinstance(link_capability, dict):
        links_state = link_capability.get("state") or "unknown"
        links_reason = link_capability.get("reason")
    else:
        links_state, links_reason = "unknown", "scan does not declare link evidence capability"
    if links_state not in {"complete", "partial"}:
        links_state = "unavailable"
        links_reason = links_reason or "scan link occurrence capability is unavailable"
    elif bool(scan.get("crawl_partial")) and links_state == "complete":
        links_state = "partial"
        links_reason = links_reason or "the source crawl is partial"
    if scan.get("lifecycle") != "finished":
        crawl_state = "failed" if scan.get("lifecycle") == "failed" else "partial"
        crawl_reason = scan.get("finish_reason") or scan.get("lifecycle")
    elif scan.get("crawl_partial"):
        crawl_state = "partial"
        crawl_reason = scan.get("finish_reason") or "scan is marked partial"
    else:
        crawl_state, crawl_reason = "complete", None
    try:
        from seohead.crawl.settings import manifest as settings_manifest

        safe_settings = settings_manifest(settings_json)
    except Exception as exc:
        safe_settings = {"state": "unavailable", "reason": f"settings projection failed: {exc}"}
    source_check_coverage = (
        (audit.get("summary") or {}).get("check_coverage", {}) if isinstance(audit, dict) else {}
    )
    coverage_rows = (
        _check_coverage_rows(str(scan["scan_uuid"]), audit.get("run") or {}, source_check_coverage)
        if isinstance(audit, dict)
        else []
    )
    if not isinstance(audit, dict):
        coverage_rows.append(
            _coverage_row(
                str(scan["scan_uuid"]),
                "checks",
                "audit",
                "check_coverage",
                "unavailable",
                "scan has no current saved audit; findings and check coverage were not produced",
                None,
                0,
                None,
                {},
            )
        )
    metadata = {
        "run_id": scan["scan_uuid"],
        "source_kind": "scan",
        "source_schema": scan.get("format_version"),
        "source_name": path.name,
        "source_sha256": scan_sha,
        "source_bytes": scan_bytes,
        "scan_uuid": scan["scan_uuid"],
        "scan_source_kind": scan.get("source_kind"),
        "parent_scan_uuid": scan.get("parent_scan_uuid"),
        "started_at": scan.get("created_at"),
        "finished_at": scan.get("finished_at"),
        "target_scope": scan.get("start_url"),
        "crawl_state": crawl_state,
        "crawl_reason": crawl_reason,
        "lifecycle": scan.get("lifecycle"),
        "finish_reason": scan.get("finish_reason"),
        "crawl_settings": safe_settings,
        "limitations": limitations,
        "capabilities": capabilities,
        "retention": retention,
        "evidence_revision": scan.get("evidence_revision"),
        "audit_available": isinstance(audit, dict),
        "audit_sha256": audit_reader.sha256
        if audit_reader is not None
        else audit_row[1]
        if audit_row
        else None,
    }

    def links_factory() -> Iterator[dict[str, Any]]:
        cursor = con.execute(
            "SELECT l.*,source.url AS source_url,destination.url AS destination_url "
            "FROM links l "
            "JOIN urls source ON source.url_id=l.source_url_id "
            "JOIN urls destination ON destination.url_id=l.destination_url_id "
            "ORDER BY l.link_id"
        )
        for record in cursor:
            yield dict(record)

    def pages_factory() -> Iterator[dict[str, Any]]:
        overlay_con = (
            sqlite3.connect(audit_overlay_path) if audit_overlay_path is not None else None
        )
        try:
            for record in con.execute(
                "SELECT p.*,u.url FROM pages p JOIN urls u USING(url_id) ORDER BY p.page_ordinal"
            ):
                page = dict(record)
                if overlay_con is not None:
                    overlay = overlay_con.execute(
                        "SELECT indexability_json,indexability_status_json FROM overlays WHERE url=?",
                        (page["url"],),
                    ).fetchone()
                    if overlay is not None:
                        page["_bi_audit_page"] = {
                            "indexability": json.loads(overlay[0]),
                            "indexability_status": json.loads(overlay[1]),
                        }
                elif audit_pages is not None:
                    overlay = audit_pages.get(page["url"])
                    if overlay is not None:
                        page["_bi_audit_page"] = {
                            "indexability": overlay.get("indexability"),
                            "indexability_status": overlay.get("indexability_status"),
                        }
                yield page
        finally:
            if overlay_con is not None:
                overlay_con.close()

    def close() -> None:
        if audit_reader is not None:
            audit_reader.close()
        if audit_overlay_temp is not None:
            audit_overlay_temp.cleanup()

    return _RunInput(
        run_id=str(scan["scan_uuid"]),
        source_kind="scan",
        source_schema=str(scan.get("format_version") or "scan.v1"),
        source_name=path.name,
        source_sha256=scan_sha,
        source_bytes=scan_bytes,
        run_metadata=metadata,
        pages_factory=pages_factory,
        page_count=page_count,
        findings_factory=findings_factory,
        finding_count=finding_count,
        groups=groups,
        links_factory=links_factory,
        links_source_state=str(links_state),
        links_source_reason=links_reason,
        coverage_rows=coverage_rows,
        link_count=link_count if links_state in {"complete", "partial"} else None,
        audit_sha256=audit_reader.sha256
        if audit_reader is not None
        else audit_row[1]
        if audit_row
        else None,
        close=close if audit_reader is not None or audit_overlay_temp is not None else None,
    )


def _page_url(page: dict[str, Any]) -> str | None:
    url = page.get("url")
    return url if isinstance(url, str) and url else None


def _page_key(url: str | None) -> tuple[str | None, str, str | None]:
    key = normalize_join_key(url)
    if key is None:
        return None, "unkeyable", "URL is not a valid absolute HTTP(S) join key"
    return key, "keyed", None


def _measure(value: Any, state: str, reason: str | None = None) -> tuple[Any, str, str | None]:
    if state == "unavailable":
        return None, state, reason or "source value unavailable"
    if value is None:
        return None, "unavailable", reason or "source value is null"
    return value, state, reason


def _audit_metric(page: dict[str, Any], name: str, kind: str) -> Any:
    if kind == "site-audit":
        if name == "meta_description" and "description" in page:
            return page["description"]
        if name in page:
            return page[name]
        return None
    metrics = page.get("metrics")
    if not isinstance(metrics, dict):
        return None
    aliases = {
        "meta_description": "meta_description",
        "crawl_depth": "crawl_depth",
        "response_time": "response_time",
        "word_count": "word_count",
        "text_ratio": "text_ratio",
        "title": "title",
        "h1": "h1",
        "canonical": "canonical",
        "inlinks": "inlinks",
        "outlinks": "outlinks",
        "external_outlinks": "external_outlinks",
    }
    value = metrics.get(aliases.get(name, name))
    if name == "h1" and isinstance(value, list):
        return value[0] if value else ""
    if name == "meta_description":
        # SF audits carry its text in metrics.meta_description.
        return value
    return value


def _audit_metric_present(page: dict[str, Any], name: str, kind: str) -> bool:
    if kind == "site-audit":
        return name in page
    metrics = page.get("metrics")
    if not isinstance(metrics, dict):
        return False
    aliases = {
        "meta_description": "meta_description",
        "crawl_depth": "crawl_depth",
        "response_time_seconds": "response_time",
        "word_count": "word_count",
        "text_ratio": "text_ratio",
        "title": "title",
        "h1": "h1",
        "canonical": "canonical",
        "inlinks": "inlinks",
        "unique_inlinks": "unique_inlinks",
        "internal_outlinks": "outlinks",
        "external_outlinks": "external_outlinks",
        "size_bytes": "size_bytes",
    }
    return aliases.get(name, name) in metrics


def _audit_body_value(
    value: Any, *, present: bool, skip_reason: str | None, field_name: str
) -> tuple[Any, str, str | None]:
    if skip_reason and (not present or value is None or value == ""):
        return None, "unavailable", skip_reason
    if not present:
        return None, "unknown", f"audit does not include {field_name} for this page"
    if value is None:
        return None, "unknown", f"audit field {field_name} is null without a measured-state marker"
    return value, "measured", None


def _page_row(run: _RunInput, page: dict[str, Any], ordinal: int) -> dict[str, Any]:
    url = _page_url(page)
    if url is None:
        raise BIExportError(f"page source row {ordinal} has no non-empty URL")
    url_key, key_state, key_reason = _page_key(url)
    body_marker = page.get("body_unavailable")
    if body_marker is None and run.source_kind == "scan":
        body_state, body_reason = "unknown", "scan does not retain body availability for this row"
    elif body_marker:
        body_state, body_reason = "unavailable", str(body_marker)
    elif run.source_kind == "scan":
        content_type = str(page.get("content_type") or "").split(";", 1)[0].strip().casefold()
        if content_type not in {"text/html", "application/xhtml+xml"}:
            body_state, body_reason = (
                "not_applicable",
                "response is not a supported HTML media type",
            )
        elif page.get("document_id") is None:
            body_state, body_reason = "unavailable", "scan page has no parsed document evidence"
        else:
            body_state, body_reason = "measured", None
    else:
        body_state, body_reason = (
            "unknown",
            "audit document does not declare per-page body retention",
        )

    def body_value(name: str, value: Any) -> tuple[Any, str, str | None]:
        if body_state == "not_applicable":
            return None, "not_applicable", body_reason
        if body_state != "measured":
            return None, body_state, body_reason
        if value is None:
            return None, "unavailable", "source audit does not include this page field"
        return value, "measured", None

    def source_value(name: str, value: Any) -> tuple[Any, str, str | None]:
        if value is None:
            return None, "unavailable", f"source does not include {name} for this page"
        return value, "measured", None

    link_state = "measured" if run.source_kind != "scan" else run.links_source_state
    link_reason = run.links_source_reason

    if run.source_kind == "scan":
        audit_page = page.get("_bi_audit_page") or {}
        if not isinstance(audit_page, dict):
            audit_page = {}
        values = {
            "status_code": page.get("status_code"),
            "content_type": page.get("content_type"),
            "indexability": audit_page.get("indexability"),
            "indexability_status": audit_page.get("indexability_status"),
            "crawl_depth": page.get("crawl_depth"),
            "response_time_seconds": page.get("response_time"),
            "size_bytes": page.get("size_bytes"),
            "title": page.get("title"),
            "meta_description": page.get("meta_description"),
            "h1": page.get("h1"),
            "canonical": page.get("canonical"),
            "word_count": page.get("word_count"),
            "text_ratio": page.get("text_ratio"),
            "inlinks": page.get("inlinks"),
            "unique_inlinks": page.get("unique_inlinks"),
            "internal_outlinks": None,
            "outlinks_total": page.get("outlinks"),
            "external_outlinks": page.get("external_outlinks"),
        }
        body_names = {"title", "meta_description", "h1", "canonical", "word_count", "text_ratio"}
        outlinks_total = page.get("outlinks")
        external_outlinks = page.get("external_outlinks")
        internal_outlinks = (
            max(outlinks_total - external_outlinks, 0)
            if type(outlinks_total) is int and type(external_outlinks) is int
            else None
        )
        values["internal_outlinks"] = internal_outlinks
        link_cap = (run.run_metadata.get("capabilities") or {}).get("links") or {}
        link_state = link_cap.get("state", "unknown") if isinstance(link_cap, dict) else "unknown"
        if run.links_source_state == "unavailable":
            link_state = "unavailable"
        link_reason = run.links_source_reason
        states: dict[str, tuple[Any, str, str | None]] = {}
        for name, value in values.items():
            if name in body_names:
                states[name] = body_value(name, value)
            elif name in {
                "inlinks",
                "unique_inlinks",
                "internal_outlinks",
                "outlinks_total",
                "external_outlinks",
            }:
                states[name] = (
                    (None, "unavailable", link_reason or "link evidence unavailable")
                    if link_state not in {"complete", "partial"}
                    else source_value(name, value)
                )
            else:
                states[name] = source_value(name, value)
        states["internal_outlinks"] = (
            (None, "unavailable", link_reason or "outlink counts were not retained")
            if link_state not in {"complete", "partial"} or internal_outlinks is None
            else (
                internal_outlinks,
                "derived",
                "total outlinks minus external outlinks, matching the crawl evidence projection",
            )
        )
        states["indexability"] = source_value("indexability", values["indexability"])
        representation = page.get("representation")
        source_ordinal = page.get("page_ordinal", ordinal)
        source_fields = {
            key: _json_value(value)
            for key, value in page.items()
            if key not in {"url", "url_id", "page_ordinal", "document_id", "_bi_audit_page"}
        }
    else:
        source_ordinal = ordinal
        values = {
            "status_code": page.get("status_code", page.get("status")),
            "content_type": page.get("content_type"),
            "indexability": page.get("indexability"),
            "indexability_status": page.get("indexability_status"),
            "crawl_depth": _audit_metric(page, "crawl_depth", run.source_kind),
            "response_time_seconds": _audit_metric(page, "response_time", run.source_kind),
            "size_bytes": _audit_metric(page, "size_bytes", run.source_kind),
            "title": _audit_metric(page, "title", run.source_kind),
            "meta_description": _audit_metric(page, "meta_description", run.source_kind),
            "h1": _audit_metric(page, "h1", run.source_kind),
            "canonical": _audit_metric(page, "canonical", run.source_kind),
            "word_count": _audit_metric(page, "word_count", run.source_kind),
            "text_ratio": _audit_metric(page, "text_ratio", run.source_kind),
            "inlinks": _audit_metric(page, "inlinks", run.source_kind),
            "unique_inlinks": _audit_metric(page, "unique_inlinks", run.source_kind),
            "internal_outlinks": _audit_metric(page, "outlinks", run.source_kind),
            "outlinks_total": None,
            "external_outlinks": _audit_metric(page, "external_outlinks", run.source_kind),
        }
        body_checks = {
            "title": "TITLE_MISSING",
            "meta_description": "DESC_MISSING",
            "h1": "H1_MISSING",
            "canonical": "CANONICAL_MISSING",
            "word_count": "THIN_CONTENT",
            "text_ratio": "LOW_TEXT_RATIO",
        }
        states = {}
        for name, value in values.items():
            if name in body_checks:
                source_name = (
                    "description"
                    if run.source_kind == "site-audit" and name == "meta_description"
                    else name
                )
                states[name] = _audit_body_value(
                    value,
                    present=_audit_metric_present(page, source_name, run.source_kind),
                    skip_reason=(run.run_metadata.get("check_skips") or {}).get(body_checks[name]),
                    field_name=name,
                )
            else:
                states[name] = source_value(name, value)
        if run.source_kind == "sf-audit" and values["status_code"] == 0:
            states["status_code"] = (
                None,
                "unavailable",
                "SF audit uses status_code 0 as a no-response sentinel, not an HTTP status",
            )
        internal_value, internal_state, internal_reason = states["internal_outlinks"]
        external_value, external_state, external_reason = states["external_outlinks"]
        if internal_state == external_state == "measured":
            states["outlinks_total"] = (
                internal_value + external_value,
                "derived",
                "source internal plus external outlink counts",
            )
        else:
            states["outlinks_total"] = (
                None,
                "unavailable",
                internal_reason or external_reason or "source does not expose both outlink counts",
            )
        representation = _audit_metric(page, "representation", run.source_kind)
        source_fields = _json_value(page)

    if type(source_ordinal) is not int:
        raise BIExportError(f"page source row {ordinal} has a non-integer source ordinal")
    if type(states["status_code"][0]) not in {int, type(None)}:
        raise BIExportError(f"page {url!r} has a non-integer status code")
    if type(states["crawl_depth"][0]) not in {int, type(None)}:
        raise BIExportError(f"page {url!r} has a non-integer crawl depth")
    if type(states["word_count"][0]) not in {int, type(None)}:
        raise BIExportError(f"page {url!r} has a non-integer word count")
    if type(states["size_bytes"][0]) not in {int, type(None)}:
        raise BIExportError(f"page {url!r} has a non-integer byte size")

    row: dict[str, Any] = {
        "run_id": run.run_id,
        "url_observation_id": _digest_parts(run.run_id, url)[:32],
        "source_ordinal": source_ordinal,
        "url": url,
        "url_key": url_key,
        "url_key_state": key_state,
        "url_key_reason": key_reason,
        "body_evidence_state": body_state,
        "body_evidence_reason": body_reason,
        "representation": representation,
        "representation_state": "measured"
        if isinstance(representation, str) and representation
        else "unavailable",
        "representation_reason": None
        if isinstance(representation, str) and representation
        else "source does not include page representation",
        "source_fields_json": source_fields,
    }
    page_field_names = (
        "status_code",
        "content_type",
        "indexability",
        "indexability_status",
        "crawl_depth",
        "response_time_seconds",
        "size_bytes",
        "title",
        "meta_description",
        "h1",
        "canonical",
        "word_count",
        "text_ratio",
    )
    for output_name in page_field_names:
        value, state, reason = states[output_name]
        row[output_name] = value
        state_name = {
            "response_time_seconds": "response_time",
            "size_bytes": "size",
        }.get(output_name, output_name)
        row[f"{state_name}_state"] = state
        row[f"{state_name}_reason"] = reason
    for output_name in (
        "inlinks",
        "unique_inlinks",
        "internal_outlinks",
        "outlinks_total",
        "external_outlinks",
    ):
        value, state, reason = states[output_name]
        row[output_name] = value
        row[f"{output_name}_state"] = state
        row[f"{output_name}_reason"] = reason
    link_states = [
        states[name][1]
        for name in (
            "inlinks",
            "unique_inlinks",
            "internal_outlinks",
            "outlinks_total",
            "external_outlinks",
        )
    ]
    unavailable_link_counts = link_states.count("unavailable")
    if unavailable_link_counts == len(link_states):
        row["link_counts_state"] = "unavailable"
        row["link_counts_reason"] = link_reason or "one or more link counts are unavailable"
    elif link_state == "partial" or unavailable_link_counts:
        row["link_counts_state"] = "partial"
        if unavailable_link_counts:
            row["link_counts_reason"] = (
                link_reason or "some link counts are unavailable in the retained page projection"
            )
        else:
            row["link_counts_reason"] = link_reason or "link counts cover a partial crawl"
    else:
        row["link_counts_state"] = "measured"
        row["link_counts_reason"] = None
    return row


def _finding_source(
    run: _RunInput, finding: dict[str, Any], ordinal: int, group_map: Any
) -> dict[str, Any]:
    if run.source_kind in {"sf-audit", "scan"}:
        check_id = finding.get("check")
        url = finding.get("target_url")
        message = finding.get("message")
        details = finding.get("details")
        locations = finding.get("locations")
        evidence = finding.get("evidence")
        source = finding.get("source")
        status = finding.get("status_code")
        occurrences = finding.get("occurrences_count")
        fix_hint = finding.get("fix_hint")
        group_id = finding.get("group_id")
        severity = finding.get("severity")
        finding_id = finding.get("id")
        kind = "audit_finding"
        status_present = "status_code" in finding
        occurrences_present = "occurrences_count" in finding
    else:
        check_id = finding.get("check")
        url = finding.get("url")
        message = finding.get("text")
        details = finding.get("details")
        locations = finding.get("locations")
        evidence = finding.get("evidence")
        source = finding.get("source")
        status = finding.get("status_code")
        occurrences = finding.get("occurrences_count")
        fix_hint = finding.get("fix_hint")
        group_id = finding.get("group_id")
        severity = finding.get("severity")
        finding_id = finding.get("id")
        kind = "site_audit_finding"
        status_present = "status_code" in finding
        occurrences_present = "occurrences_count" in finding
    group = group_map.get(group_id) if isinstance(group_id, str) else None
    source_finding_id = finding_id if isinstance(finding_id, str) and finding_id else None
    finding_id = _digest_parts(
        run.run_id,
        str(ordinal),
        source_finding_id or "",
        _canonical_json(finding),
    )[:32]
    url_key, key_state, key_reason = _page_key(url if isinstance(url, str) else None)
    if not isinstance(check_id, str) or not check_id:
        check_state, check_reason = "unavailable", "source finding has no check identifier"
        check_id = None
    else:
        check_state, check_reason = "measured", None
    if status is not None and type(status) is not int:
        raise BIExportError(f"finding {finding_id!r} status_code must be an integer or null")
    if occurrences is not None and type(occurrences) is not int:
        raise BIExportError(f"finding {finding_id!r} occurrences_count must be an integer or null")
    url_state = "measured" if isinstance(url, str) and url else "not_applicable"
    url_reason = None if url_state == "measured" else "source finding is not attached to one URL"
    status_state = (
        "measured" if status is not None else "unavailable" if status_present else "not_applicable"
    )
    status_reason = (
        "source finding status code is null"
        if status_present and status is None
        else None
        if status is not None
        else "finding contract does not attach a status code"
    )
    occurrence_state = (
        "measured"
        if occurrences is not None
        else "unavailable"
        if occurrences_present
        else "not_applicable"
    )
    occurrence_reason = (
        "source finding occurrence count is null"
        if occurrences_present and occurrences is None
        else None
        if occurrences is not None
        else "finding contract does not declare an occurrence count"
    )
    message_state = "measured" if isinstance(message, str) and message else "unavailable"
    message_reason = None if message_state == "measured" else "source finding has no message text"
    severity_state = "measured" if isinstance(severity, str) and severity else "unknown"
    severity_reason = None if severity_state == "measured" else "source severity is not declared"
    source_state = "measured" if isinstance(source, str) and source else "unknown"
    source_reason = None if source_state == "measured" else "source finding origin is not declared"
    fix_state = "measured" if isinstance(fix_hint, str) and fix_hint else "not_supplied"
    return {
        "run_id": run.run_id,
        "finding_id": finding_id,
        "source_finding_id": source_finding_id,
        "source_ordinal": ordinal,
        "finding_kind": kind,
        "check_id": check_id,
        "check_state": check_state,
        "check_reason": check_reason,
        "severity": severity,
        "severity_state": severity_state,
        "severity_reason": severity_reason,
        "url": url,
        "url_state": url_state,
        "url_reason": url_reason,
        "url_key": url_key,
        "url_key_state": key_state,
        "url_key_reason": key_reason,
        "message": message,
        "message_state": message_state,
        "message_reason": message_reason,
        "source": source,
        "source_state": source_state,
        "source_reason": source_reason,
        "status_code": status,
        "status_code_state": status_state,
        "status_code_reason": status_reason,
        "occurrences_count": occurrences,
        "occurrences_count_state": occurrence_state,
        "occurrences_count_reason": occurrence_reason,
        "fix_hint": fix_hint,
        "fix_hint_state": fix_state,
        "fix_hint_reason": None if fix_state == "measured" else "source finding has no fix hint",
        "group_id": group_id,
        "group_state": "grouped" if group else "not_grouped",
        "group_value": group.get("value") if group else None,
        "group_url_count": group.get("member_count", len(group.get("urls") or []))
        if group
        else None,
        "group_urls_json": group.get("urls") if group else None,
        "locations_json": locations if locations is not None else [],
        "details_json": details if details is not None else {},
        "evidence_json": evidence if evidence is not None else {},
        "source_finding_json": finding,
    }


def _provider_file(path_value: str | os.PathLike[str]) -> _ProviderInput:
    path = Path(path_value)
    from seohead.data_sources.evidence_join_store import is_store, open_store

    if is_store(path):
        store = open_store(path)
        metadata = store.metadata
        source_id = metadata.get("content_sha256")
        if not isinstance(source_id, str) or not re.fullmatch(r"[0-9a-f]{64}", source_id):
            raise BIExportError("evidence join store has no content hash")
        sha256, byte_count = _sha256_path(path, MAX_SCAN_BYTES, "provider join store")
        return _ProviderInput(
            source_id=source_id,
            name=path.name,
            sha256=sha256,
            byte_count=byte_count,
            join=metadata,
            compatibility=None,
            normalized=None,
            store=store,
        )
    document, raw = _read_json_path(path_value, "provider join", MAX_PROVIDER_JOIN_BYTES)
    return _provider_document(document, raw, Path(path_value).name)


def _provider_document(document: dict[str, Any], raw: bytes, name: str | None) -> _ProviderInput:
    source_id = _digest(raw)
    compatibility = None
    if document.get("format") == NORMALIZED_FORMAT:
        normalized = document
        join = None
    elif document.get("format") == JOIN_FORMAT:
        join = document
        normalized = None
    elif isinstance(document.get("join"), dict) and document["join"].get("format") == JOIN_FORMAT:
        join = document["join"]
        normalized = None
        compatibility = document.get("compatibility")
    elif document.get("format") == "seohead.provider-join.v1":
        summary = (document.get("join") or {}).get("summary") or {}
        count = summary.get("rows")
        if type(count) is not int or count < 0:
            raise BIExportError("legacy provider-join artifact has invalid source row counts")
        return _ProviderInput(
            source_id=source_id,
            name=name,
            sha256=_digest(raw),
            byte_count=len(raw),
            join=None,
            compatibility=None,
            normalized=None,
            error_state="unsupported",
            error_reason=(
                "legacy provider-join.v1 does not declare typed metric names, units, periods, "
                "or per-value availability; no metric rows were inferred"
            ),
            reported_rows=count,
        )
    else:
        raise BIExportError(
            "provider input must be seohead.normalized-evidence.v1, "
            "seohead.evidence-join.v1, or a saved evidence-join artifact"
        )
    if normalized is not None:
        rows = normalized.get("rows")
        if not isinstance(rows, list) or len(rows) > MAX_PROVIDER_ROWS_PER_SOURCE:
            raise BIExportError(
                f"normalized provider rows exceed the {MAX_PROVIDER_ROWS_PER_SOURCE}-row source bound"
            )
        mapping = normalized.get("mapping")
        if not isinstance(mapping, dict):
            raise BIExportError("normalized provider evidence lacks its mapping/provenance")
        summary = normalized.get("summary")
        if not isinstance(summary, dict) or summary.get("rows") != len(rows):
            raise BIExportError(
                "normalized provider summary does not conserve its source row count"
            )
        if not isinstance(mapping.get("metrics"), list) or not mapping.get("metrics"):
            raise BIExportError("normalized provider evidence has no declared numeric metrics")
    if join is not None:
        _validate_join(join)
    return _ProviderInput(
        source_id=source_id,
        name=name,
        sha256=_digest(raw),
        byte_count=len(raw),
        join=join,
        compatibility=compatibility,
        normalized=normalized,
    )


def _validate_join(join: dict[str, Any]) -> None:
    if join.get("format") != JOIN_FORMAT:
        raise BIExportError(f"provider join format must be {JOIN_FORMAT}")
    for name in ("matched", "crawl_only", "external_only", "unkeyable_pages", "unkeyable_rows"):
        if not isinstance(join.get(name), list):
            raise BIExportError(f"provider join {name} population must be a list")
    evidence = join.get("evidence")
    summary = join.get("summary")
    if not isinstance(evidence, dict) or not isinstance(summary, dict):
        raise BIExportError("provider join lacks evidence provenance or summary")
    metrics = evidence.get("metrics")
    dimensions = evidence.get("dimensions")
    if (
        not isinstance(metrics, list)
        or not metrics
        or any(not isinstance(item, dict) for item in metrics)
    ):
        raise BIExportError("provider join does not declare typed numeric metrics")
    if not isinstance(dimensions, list) or any(
        not isinstance(item, str) or not item for item in dimensions
    ):
        raise BIExportError("provider join dimensions must be declared string names")
    if len(metrics) > 64 or len(dimensions) > 64:
        raise BIExportError("provider join exceeds metric/dimension field bounds")
    if len(set(dimensions)) != len(dimensions):
        raise BIExportError("provider join dimension names must be unique")
    names = [item.get("name") for item in metrics]
    if len(set(names)) != len(names) or any(
        not isinstance(name, str) or not name for name in names
    ):
        raise BIExportError("provider join metric names must be unique non-empty strings")
    for item in metrics:
        if item.get("type", "number") != "number":
            raise BIExportError(f"provider metric {item.get('name')!r} is not numeric")
    row_count = summary.get("rows")
    if type(row_count) is not int or not 0 <= row_count <= MAX_PROVIDER_ROWS_PER_SOURCE:
        raise BIExportError("provider join row count exceeds its declared bound")
    source_counts = {
        "pages": summary.get("pages"),
        "matched_pages": summary.get("matched_pages"),
        "matched_rows": summary.get("matched_rows"),
        "external_only": summary.get("external_only"),
        "unkeyable_rows": summary.get("unkeyable_rows"),
        "crawl_only": summary.get("crawl_only"),
        "unkeyable_pages": summary.get("unkeyable_pages"),
    }
    if any(type(value) is not int or value < 0 for value in source_counts.values()):
        raise BIExportError("provider join contains invalid population counts")
    for population, summary_key in (
        ("matched", "matched_pages"),
        ("crawl_only", "crawl_only"),
        ("external_only", "external_only"),
        ("unkeyable_pages", "unkeyable_pages"),
        ("unkeyable_rows", "unkeyable_rows"),
    ):
        if len(join[population]) != source_counts[summary_key]:
            raise BIExportError(f"provider join {population} rows disagree with its summary")
    if (
        source_counts["matched_rows"]
        + source_counts["external_only"]
        + source_counts["unkeyable_rows"]
        != row_count
    ):
        raise BIExportError("provider join evidence populations do not conserve source rows")
    if (
        source_counts["matched_pages"]
        + source_counts["crawl_only"]
        + source_counts["unkeyable_pages"]
        != source_counts["pages"]
    ):
        raise BIExportError("provider join page populations do not conserve crawl pages")


def _validate_store_join(join: dict[str, Any]) -> None:
    """Validate the typed header of a cursor-backed evidence join."""
    if join.get("format") != "seohead.evidence-join-sqlite.v1":
        raise BIExportError("unsupported cursor-backed evidence join format")
    evidence = join.get("evidence")
    summary = join.get("summary")
    if not isinstance(evidence, dict) or not isinstance(summary, dict):
        raise BIExportError("evidence join store lacks provenance or summary")
    metrics, dimensions = evidence.get("metrics"), join.get("dimension_names")
    if (
        not isinstance(metrics, list)
        or not metrics
        or any(not isinstance(item, dict) for item in metrics)
    ):
        raise BIExportError("evidence join store does not declare typed numeric metrics")
    if not isinstance(dimensions, list) or any(
        not isinstance(name, str) or not name for name in dimensions
    ):
        raise BIExportError("evidence join store dimensions are invalid")
    if len(metrics) > 64 or len(dimensions) > 64:
        raise BIExportError("evidence join store exceeds metric/dimension field bounds")
    names = [item.get("name") for item in metrics]
    if len(set(names)) != len(names) or any(
        not isinstance(name, str) or not name for name in names
    ):
        raise BIExportError("evidence join store metric names are invalid")
    for item in metrics:
        if item.get("type", "number") != "number":
            raise BIExportError(f"provider metric {item.get('name')!r} is not numeric")


def _join_from_provider(provider: _ProviderInput, run: _RunInput) -> dict[str, Any] | None:
    if provider.join is not None:
        join = provider.join
        crawl = join.get("crawl") or {}
        source_scan_id = crawl.get("scan_uuid")
        if source_scan_id and run.source_kind == "scan" and source_scan_id != run.run_id:
            raise BIExportError(
                f"provider join {provider.name or provider.source_id} belongs to scan {source_scan_id}, "
                f"not {run.run_id}"
            )
        return join
    if provider.normalized is None:
        return None
    if run.page_count > 50_000:
        raise BIExportError(
            "normalized provider evidence must be joined before a large scan BI export; "
            "supply the saved evidence-join artifact to avoid materializing crawl pages"
        )
    document = provider.normalized
    rows = document.get("rows") or []
    page_counts: Counter[str] = Counter()
    evidence_counts: Counter[str] = Counter()
    pages = list(run.pages_factory())
    for page in pages:
        key = normalize_join_key(_page_url(page))
        if key is not None:
            page_counts[key] += 1
    for row in rows:
        url = row.get("url") or {}
        key = (
            None
            if url.get("state") == "unkeyable"
            else normalize_join_key(url.get("resolved") or url.get("raw"))
        )
        if key is not None:
            evidence_counts[key] += 1
    candidate_pairs = sum(
        page_counts[key] * evidence_counts[key]
        for key in page_counts.keys() & evidence_counts.keys()
    )
    if candidate_pairs > 500_000:
        raise BIExportError(
            "provider join would create over 500000 page/evidence references; the source has a "
            "high normalized-key collision and no BI package was published"
        )
    from seohead.data_sources.evidence_join import join_evidence

    return join_evidence(
        pages, document, crawl={"source": run.source_kind, "scan_uuid": run.run_id}
    )


def _row_id(row: dict[str, Any]) -> tuple[int, str]:
    index = row.get("row_index")
    digest = row.get("natural_key_sha256")
    if (
        type(index) is not int
        or index < 0
        or not isinstance(digest, str)
        or not re.fullmatch(r"[0-9a-f]{64}", digest)
    ):
        raise BIExportError(
            "normalized provider row lacks a valid row_index/natural_key_sha256 identity"
        )
    return index, digest


def _provider_observations(
    provider: _ProviderInput, join: dict[str, Any]
) -> tuple[Iterable[dict[str, Any]], dict[str, Any]]:
    if provider.store is not None:
        _validate_store_join(join)
        evidence = join["evidence"]
        metrics = evidence["metrics"]
        dimensions = join["dimension_names"]
        summary = join["summary"]
        expected_rows = summary["rows"]

        def stream() -> Iterator[dict[str, Any]]:
            rows_seen = 0
            for item in provider.store.iter_observations():
                row = item["row"]
                if not isinstance(row, dict):
                    raise BIExportError("evidence join store has a non-object normalized row")
                identity = _row_id(row)
                if identity != item["identity"]:
                    raise BIExportError("evidence join store row identity disagrees with its key")
                dimensions_row = row.get("dimensions") or {}
                if not isinstance(dimensions_row, dict):
                    raise BIExportError("normalized provider dimensions must be an object")
                if set(dimensions_row) - set(dimensions):
                    raise BIExportError("evidence join store row has an undeclared dimension")
                row_metrics = row.get("metrics")
                if not isinstance(row_metrics, dict) or set(row_metrics) != {
                    metric["name"] for metric in metrics
                }:
                    raise BIExportError(
                        "provider row metric names differ from the declared source metrics"
                    )
                url = row.get("url") or {}
                if not isinstance(url, dict):
                    raise BIExportError("normalized provider URL provenance must be an object")
                population = item["population"]
                if population not in {"matched", "external_only", "unkeyable_rows"}:
                    raise BIExportError("evidence join store has an invalid evidence population")
                matched_count = item["matched_page_count"]
                if type(matched_count) is not int or matched_count < 0:
                    raise BIExportError("evidence join store has an invalid matched-page count")
                if (population == "matched") != bool(matched_count):
                    raise BIExportError("evidence join store population disagrees with match edges")
                rows_seen += 1
                for metric in metrics:
                    name = metric["name"]
                    entry = row_metrics[name]
                    if not isinstance(entry, dict) or entry.get("state") not in {
                        "measured",
                        "unavailable",
                    }:
                        raise BIExportError(
                            f"provider metric {name!r} has an invalid availability state"
                        )
                    value = entry.get("value")
                    if entry["state"] == "measured":
                        if (
                            isinstance(value, bool)
                            or not isinstance(value, (int, float))
                            or not math.isfinite(value)
                        ):
                            raise BIExportError(
                                f"measured provider metric {name!r} must be a finite number"
                            )
                    elif value is not None:
                        raise BIExportError(
                            f"unavailable provider metric {name!r} must have a null value"
                        )
                    yield {
                        "identity": identity,
                        "row": row,
                        "metric": metric,
                        "entry": entry,
                        "dimensions": dimensions_row,
                        "population": "unkeyable" if population == "unkeyable_rows" else population,
                        "matched_urls": (),
                        "matched_page_count": matched_count,
                        "matched_url_provenance": {
                            "state": "cursor_backed",
                            "format": join["format"],
                            "row_index": identity[0],
                            "natural_key_sha256": identity[1],
                        },
                        "url": url,
                    }
            if rows_seen != expected_rows:
                raise BIExportError("evidence join store rows disagree with its summary")

        info = {
            "dimensions": dimensions,
            "metric_specs": metrics,
            "summary": summary,
            "crawl": join.get("crawl") or {},
            "evidence": evidence,
            "compatibility": provider.compatibility,
        }
        return _ObservationStream(stream, expected_rows * len(metrics)), info
    evidence = join["evidence"]
    metrics = evidence["metrics"]
    declared_dimensions = evidence["dimensions"]
    row_map: dict[tuple[int, str], dict[str, Any]] = {}
    matches: dict[tuple[int, str], list[str]] = defaultdict(list)
    populations: dict[tuple[int, str], str] = {}
    for item in join["matched"]:
        if not isinstance(item, dict) or not isinstance(item.get("rows"), list):
            raise BIExportError("provider join matched entry has malformed rows")
        page_url = item.get("url") or (item.get("page") or {}).get("url")
        for row in item["rows"]:
            if not isinstance(row, dict):
                raise BIExportError("provider join contains a non-object normalized row")
            identity = _row_id(row)
            previous = row_map.get(identity)
            if previous is not None and _canonical_json(previous) != _canonical_json(row):
                raise BIExportError(
                    "provider join repeats a source row identity with different content"
                )
            row_map[identity] = row
            if isinstance(page_url, str) and page_url:
                matches[identity].append(page_url)
            populations[identity] = "matched"
    for item in join["external_only"]:
        row = item.get("row") if isinstance(item, dict) else None
        if not isinstance(row, dict):
            raise BIExportError("provider join external_only entry lacks a row")
        identity = _row_id(row)
        if identity in row_map and _canonical_json(row_map[identity]) != _canonical_json(row):
            raise BIExportError(
                "provider join repeats a source row identity with different content"
            )
        row_map[identity] = row
        populations.setdefault(identity, "external_only")
    for row in join["unkeyable_rows"]:
        if not isinstance(row, dict):
            raise BIExportError("provider join unkeyable_rows contains a non-object")
        identity = _row_id(row)
        if identity in row_map and _canonical_json(row_map[identity]) != _canonical_json(row):
            raise BIExportError(
                "provider join repeats a source row identity with different content"
            )
        row_map[identity] = row
        populations.setdefault(identity, "unkeyable")
    if len(row_map) != join["summary"].get("rows"):
        raise BIExportError(
            "provider join populations do not conserve the normalized source row count"
        )
    matched_rows = sum(1 for identity in row_map if populations.get(identity) == "matched")
    if matched_rows != join["summary"].get("matched_rows"):
        raise BIExportError("provider join matched evidence rows disagree with its summary")
    observations: list[dict[str, Any]] = []
    dimensions_seen = set(declared_dimensions)
    for identity in sorted(row_map):
        row = row_map[identity]
        dimensions_row = row.get("dimensions") or {}
        if not isinstance(dimensions_row, dict):
            raise BIExportError("normalized provider dimensions must be an object")
        dimensions_seen.update(str(name) for name in dimensions_row)
        row_metrics = row.get("metrics")
        if not isinstance(row_metrics, dict):
            raise BIExportError("normalized provider row metrics must be an object")
        if set(row_metrics) != {metric["name"] for metric in metrics}:
            raise BIExportError("provider row metric names differ from the declared source metrics")
        for metric in metrics:
            name = metric["name"]
            entry = row_metrics[name]
            if not isinstance(entry, dict) or entry.get("state") not in {"measured", "unavailable"}:
                raise BIExportError(f"provider metric {name!r} has an invalid availability state")
            value = entry.get("value")
            if entry["state"] == "measured":
                if (
                    isinstance(value, bool)
                    or not isinstance(value, (int, float))
                    or not math.isfinite(value)
                ):
                    raise BIExportError(
                        f"measured provider metric {name!r} must be a finite number"
                    )
            elif value is not None:
                raise BIExportError(f"unavailable provider metric {name!r} must have a null value")
            url = row.get("url") or {}
            if not isinstance(url, dict):
                raise BIExportError("normalized provider URL provenance must be an object")
            observations.append(
                {
                    "identity": identity,
                    "row": row,
                    "metric": metric,
                    "entry": entry,
                    "dimensions": dimensions_row,
                    "population": populations.get(identity, "unkeyable"),
                    "matched_urls": sorted(matches.get(identity, [])),
                    "matched_page_count": len(matches.get(identity, [])),
                    "url": url,
                }
            )
    info = {
        "dimensions": sorted(dimensions_seen),
        "metric_specs": metrics,
        "summary": join["summary"],
        "crawl": join.get("crawl") or {},
        "evidence": evidence,
        "compatibility": provider.compatibility,
    }
    return observations, info


def _safe_dimension_fields(names: Iterable[str]) -> dict[str, str]:
    result: dict[str, str] = {}
    used: dict[str, str] = {}
    for name in sorted(set(names)):
        slug = re.sub(r"[^a-z0-9_]+", "_", name.casefold()).strip("_") or "value"
        field = f"dimension_{slug}"
        if field in used and used[field] != name:
            field += "_" + hashlib.sha256(name.encode("utf-8")).hexdigest()[:8]
        used[field] = name
        result[name] = field
    return result


def _provider_collection(header: dict[str, Any]) -> dict[str, Any]:
    """Interpret collection provenance without upgrading an import default to evidence."""
    collection = header.get("collection") or {}
    origins = header.get("field_origins") or {}
    if not isinstance(collection, dict) or not isinstance(origins, dict):
        raise BIExportError("provider source has malformed collection metadata or field origins")
    state = collection.get("state")
    reason = None
    if not state or origins.get("collection_state") not in (
        "declared",
        "envelope",
    ):
        state, reason = "unknown", "source did not establish collection_state"
    elif state == "complete":
        flags = ("sampled", "thresholded", "truncated")
        unknown = [
            flag
            for flag in flags
            if type(collection.get(flag)) is not bool
            or origins.get(flag) not in ("declared", "envelope")
        ]
        degraded = [flag for flag in flags if collection.get(flag) is True and flag not in unknown]
        if degraded:
            state, reason = "partial", "provider collection is degraded: " + ", ".join(degraded)
        if unknown:
            state = "partial" if degraded else "unknown"
            reason = "; ".join(
                filter(
                    None,
                    (
                        reason,
                        "source did not explicitly establish collection flags: "
                        + ", ".join(unknown),
                    ),
                )
            )
    if reason:
        return {
            **collection,
            "state": state,
            "reason": "; ".join(filter(None, (reason, collection.get("reason")))),
        }
    return collection


def _metric_rows(
    run: _RunInput,
    sources: list[tuple[_ProviderInput, list[dict[str, Any]], dict[str, Any]]],
    dimensions: dict[str, str],
) -> Iterator[dict[str, Any]]:
    for provider, observations, info in sources:
        if provider.error_state:
            continue
        header = info["evidence"]
        period = header.get("period") or {}
        if period and not isinstance(period, dict):
            raise BIExportError(
                f"provider source {provider.source_id} has malformed period metadata"
            )
        start_date, end_date = period.get("start_date"), period.get("end_date")
        if start_date is not None:
            try:
                date.fromisoformat(start_date)
                date.fromisoformat(end_date)
            except (TypeError, ValueError) as exc:
                raise BIExportError(
                    f"provider source {provider.source_id} has invalid period dates"
                ) from exc
        collection = _provider_collection(header)
        field_origins = header.get("field_origins") or {}
        if not isinstance(field_origins, dict):
            raise BIExportError(f"provider source {provider.source_id} has malformed field origins")

        def fact(
            name: str,
            value: Any,
            origins: dict[str, Any] = field_origins,
        ) -> tuple[str, str | None]:
            if value is None:
                return "unknown", f"source did not establish {name}"
            origin = origins.get(name)
            if origin == "unknown":
                return "unknown", f"{name} has an unknown provenance origin"
            return "measured", None

        period_state = "measured" if start_date is not None and end_date is not None else "unknown"
        period_reason = (
            None if period_state == "measured" else "inclusive period boundaries are unknown"
        )
        for obs in observations:
            row = obs["row"]
            entry = obs["entry"]
            url = obs["url"]
            index, natural_hash = obs["identity"]
            metric_name = obs["metric"]["name"]
            dims_row = obs["dimensions"]
            key = url.get("normalized") if url.get("state") == "keyed" else None
            observation_id = _digest_parts(
                provider.source_id, str(index), natural_hash, metric_name
            )[:32]
            result = {
                "run_id": run.run_id,
                "metric_observation_id": observation_id,
                "provider_source_id": provider.source_id,
                "provider": header.get("provider"),
                "operation": header.get("operation"),
                "reporting_identity": header.get("reporting_identity"),
                "privacy": header.get("privacy"),
                "metric_name": metric_name,
                "metric_unit": obs["metric"].get("unit") or entry.get("unit"),
                "metric_unit_state": "measured"
                if obs["metric"].get("unit") or entry.get("unit")
                else "unknown",
                "metric_unit_reason": None
                if obs["metric"].get("unit") or entry.get("unit")
                else "unit is not declared",
                "value_number": entry.get("value") if entry.get("state") == "measured" else None,
                "value_state": entry["state"],
                "value_reason": entry.get("reason"),
                "raw_value_json": entry.get("raw"),
                "url_raw": url.get("raw"),
                "url_resolved": url.get("resolved"),
                "url_key": key,
                "url_state": url.get("state") or "unknown",
                "url_reason": url.get("reason"),
                "period_start": start_date,
                "period_end": end_date,
                "period_state": period_state,
                "period_reason": period_reason,
                "timezone": header.get("timezone"),
                "timezone_state": fact("timezone", header.get("timezone"))[0],
                "timezone_reason": fact("timezone", header.get("timezone"))[1],
                "attribution": header.get("attribution"),
                "attribution_state": fact("attribution", header.get("attribution"))[0],
                "attribution_reason": fact("attribution", header.get("attribution"))[1],
                "search_engine": header.get("search_engine"),
                "search_engine_state": fact("search_engine", header.get("search_engine"))[0],
                "search_engine_reason": fact("search_engine", header.get("search_engine"))[1],
                "search_type": header.get("search_type"),
                "search_type_state": fact("search_type", header.get("search_type"))[0],
                "search_type_reason": fact("search_type", header.get("search_type"))[1],
                "collection_state": collection["state"],
                "collection_reason": collection.get("reason"),
                "collection_sampled": collection.get("sampled"),
                "collection_thresholded": collection.get("thresholded"),
                "collection_truncated": collection.get("truncated"),
                "source_row_index": index,
                "natural_key_sha256": natural_hash,
                "ambiguous_source_row": bool(row.get("ambiguous")),
                "population_state": obs["population"],
                "matched_page_count": obs.get("matched_page_count", len(obs["matched_urls"])),
                "matched_page_urls_json": obs.get("matched_url_provenance", obs["matched_urls"]),
                "dimensions_state_json": {
                    name: {
                        "state": "unavailable" if value is None else "measured",
                        "reason": "source dimension is null" if value is None else None,
                    }
                    for name, value in dims_row.items()
                },
                "source_row_json": row,
                "source_metadata_json": header,
            }
            for dimension, field_name in dimensions.items():
                value = dims_row.get(dimension)
                result[field_name] = None if value is None else str(value)
            yield result


def _cohort_row(
    run: _RunInput,
    page: dict[str, Any],
    ordinal: int,
    cohort_id: str,
    definition: str,
    *,
    membership: str,
    state: str,
    reason: str | None = None,
    value_label: str | None = None,
    value_number: int | float | None = None,
    threshold: int | float | None = None,
    numerator: int | None = None,
    denominator: int | None = None,
    extraction_state: str = "not_applicable",
    extraction_reason: str | None = None,
    search_metric: str | None = None,
    search_value: int | float | None = None,
    search_state: str = "not_configured",
    sessions_value: int | float | None = None,
    sessions_state: str = "not_configured",
    period_start: str | None = None,
    period_end: str | None = None,
    timezone: str | None = None,
    source_observations: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    url = _page_url(page)
    key, key_state, key_reason = _page_key(url)
    return {
        "run_id": run.run_id,
        "cohort_observation_id": _digest_parts(run.run_id, str(ordinal), cohort_id)[:32],
        "cohort_id": cohort_id,
        "definition_version": "seohead.bi.cohort.v1",
        "definition": definition,
        "url": url,
        "url_key": key,
        "url_key_state": key_state,
        "url_key_reason": key_reason,
        "membership": membership,
        "state": state,
        "reason": reason,
        "value_label": value_label,
        "value_number": value_number,
        "threshold": threshold,
        "numerator": numerator,
        "denominator": denominator,
        "extraction_coverage_state": extraction_state,
        "extraction_coverage_reason": extraction_reason,
        "search_metric": search_metric,
        "search_value": search_value,
        "search_value_state": search_state,
        "sessions_value": sessions_value,
        "sessions_value_state": sessions_state,
        "period_start": period_start,
        "period_end": period_end,
        "timezone": timezone,
        "source_metric_observations_json": source_observations or [],
    }


def _quadrant_candidates(
    sources: list[tuple[_ProviderInput, list[dict[str, Any]], dict[str, Any]]],
    search_metric: str | None,
    *,
    con: sqlite3.Connection,
) -> tuple[Any, str | None, Any]:
    """Return only one-to-one, complete, same-window search/session pairs.

    The normalized provider grain may include query/device dimensions.  Those
    rows cannot be summed into a URL metric here, so they remain explicitly
    unclassified rather than becoming a dashboard-friendly fiction.
    """
    if search_metric not in {"clicks", "impressions"}:
        return {}, "choose search_metric 'clicks' or 'impressions' to enable quadrants", {}
    from seohead.data_sources.evidence_join import evidence_compatibility
    from seohead.reports.bi_index import QuadrantLookup

    con.execute(
        "CREATE TABLE candidates (url TEXT, start TEXT, end TEXT, timezone TEXT, "
        "search_count INTEGER DEFAULT 0, sessions_count INTEGER DEFAULT 0, "
        "search_json TEXT, sessions_json TEXT, PRIMARY KEY(url,start,end,timezone))"
    )
    con.execute("CREATE TABLE blocked (url TEXT PRIMARY KEY, reason TEXT)")

    def block(key: str, reason: str) -> None:
        con.execute("INSERT OR IGNORE INTO blocked VALUES (?,?)", (key, reason))

    source_contexts = {}
    for provider, observations, info in sources:
        header = info["evidence"]
        provider_name = str(header.get("provider") or "").casefold()
        collection = _provider_collection(header)
        period = header.get("period") or {}
        timezone = header.get("timezone")
        if provider_name not in {
            "gsc",
            "google_search_console",
            "search_console",
            "ga4",
            "google_analytics_4",
            "google_analytics",
        }:
            continue
        origins = header.get("field_origins") or {}
        # Only at most MAX_PROVIDER_SOURCES headers reach the compatibility engine;
        # URL populations continue to stream through the existing SQLite index.
        source_contexts[provider.source_id] = {
            "format": NORMALIZED_FORMAT,
            "mapping": {
                "source": header,
                "period": period,
                "collection": collection,
                "dimensions": header.get("dimensions") or [],
                "metrics": header.get("metrics") or [],
            },
            "provenance": {
                "fields": {
                    name: {
                        "value": value if origins.get(name) in ("declared", "envelope") else None
                    }
                    for name, value in header.items()
                }
            },
            "rows": [],
        }
        collection_reason = None
        if collection.get("state") != "complete":
            collection_reason = collection.get("reason") or "provider collection is not complete"
        for observation in observations:
            row = observation["row"]
            url = observation["url"]
            entry = observation["entry"]
            metric = observation["metric"]
            if (
                observation["population"] != "matched"
                or url.get("state") != "keyed"
                or row.get("dimensions")
            ):
                continue
            key = url.get("normalized")
            if not isinstance(key, str):
                continue
            matched_page_count = observation.get(
                "matched_page_count", len(observation.get("matched_urls") or ())
            )
            if type(matched_page_count) is not int or matched_page_count < 1:
                raise BIExportError("matched provider observation has an invalid page-match count")
            if matched_page_count != 1:
                block(
                    key,
                    "normalized URL key matches multiple retained crawl URLs; "
                    "provider traffic cannot be attributed to one URL observation",
                )
                continue
            if row.get("ambiguous"):
                block(
                    key,
                    "provider source marks this normalized URL row as ambiguous",
                )
                continue
            if (
                provider_name in {"gsc", "google_search_console", "search_console"}
                and metric.get("name") == search_metric
            ):
                axis = "search"
            elif (
                provider_name in {"ga4", "google_analytics_4", "google_analytics"}
                and metric.get("name") == "sessions"
            ):
                axis = "sessions"
            else:
                continue
            if collection_reason:
                block(key, collection_reason)
                continue
            if entry.get("state") != "measured":
                block(key, "provider did not measure the selected metric")
                continue
            if (metric.get("unit") or entry.get("unit")) != "count":
                block(key, "search clicks/impressions and sessions require declared count units")
                continue
            if entry["value"] < 0:
                block(key, "search clicks/impressions and sessions must be nonnegative counts")
                continue
            if (
                not isinstance(timezone, str)
                or not isinstance(period.get("start_date"), str)
                or not isinstance(period.get("end_date"), str)
            ):
                block(key, "provider period and timezone must be established")
                continue
            source = {
                "provider_source_id": provider.source_id,
                "provider": header.get("provider"),
                "operation": header.get("operation"),
                "reporting_identity": header.get("reporting_identity"),
                "metric": metric.get("name"),
                "metric_unit": metric.get("unit") or entry.get("unit"),
                "source_row_index": row.get("row_index"),
                "natural_key_sha256": row.get("natural_key_sha256"),
                "value": entry.get("value"),
                "period_start": period["start_date"],
                "period_end": period["end_date"],
                "timezone": timezone,
                "attribution": header.get("attribution"),
                "search_engine": header.get("search_engine"),
                "search_type": header.get("search_type"),
                "collection": collection,
                "field_origins": origins,
                "boundary_policy": "strict",
                "cross_source_policy": "juxtapose",
            }
            window = (key, period["start_date"], period["end_date"], timezone)
            encoded = _canonical_json(source)
            if len(encoded.encode("utf-8")) > MAX_CELL_BYTES:
                raise BIExportError("one quadrant observation exceeds the BI cell bound")
            # Only two fixed axis names reach this SQL; no user identifier is interpolated.
            con.execute(
                f"INSERT INTO candidates(url,start,end,timezone,{axis}_count,{axis}_json) "
                f"VALUES (?,?,?,?,1,?) ON CONFLICT(url,start,end,timezone) DO UPDATE SET "
                f"{axis}_count={axis}_count+1, {axis}_json=COALESCE({axis}_json,excluded.{axis}_json)",
                (*window, encoded),
            )
    con.execute(
        "INSERT OR IGNORE INTO blocked SELECT url,? FROM candidates "
        "WHERE search_count>1 OR sessions_count>1",
        ("more than one complete provider observation shares this normalized URL key and period",),
    )
    con.execute(
        "INSERT OR IGNORE INTO blocked SELECT url,? FROM candidates "
        "WHERE search_count=1 AND sessions_count=1 GROUP BY url HAVING COUNT(*)>1",
        ("more than one compatible provider period is retained for this normalized URL key",),
    )
    compatibility_cache = {}
    for key, search_json, sessions_json in con.execute(
        "SELECT url,search_json,sessions_json FROM candidates WHERE search_count=1 AND sessions_count=1"
    ):
        pair_ids = tuple(
            json.loads(value)["provider_source_id"] for value in (search_json, sessions_json)
        )
        if pair_ids not in compatibility_cache:
            decision = evidence_compatibility(
                *(source_contexts[source_id] for source_id in pair_ids),
                policy={
                    "boundary_policy": "strict",
                    "cross_source": "juxtapose",
                    "quadrant": {"left_metric": search_metric, "right_metric": "sessions"},
                },
            )
            compatibility_cache[pair_ids] = decision
        decision = compatibility_cache[pair_ids]
        if not decision["quadrant"]["eligible"]:
            reasons = decision["quadrant"]["reasons"] + [
                aspect["reason"]
                for aspect in decision["reasons"]
                if aspect.get("unverified") or aspect["verdict"] != "compatible"
            ]
            block(key, "provider scope is not qualified: " + ", ".join(reasons))
    return QuadrantLookup(con), None, QuadrantLookup(con, blocked=True)


def _cohort_rows(
    run: _RunInput,
    sources: list[tuple[_ProviderInput, list[dict[str, Any]], dict[str, Any]]],
    search_metric: str | None,
    *,
    index_con: sqlite3.Connection | None = None,
    inlink_index: Any = None,
) -> Iterator[dict[str, Any]]:
    """Yield transparent technical cohorts and optional provider quadrants."""
    definition_status = (
        "Observed HTTP status group for this retained crawl URL; no Google-crawl claim."
    )
    definition_indexability = (
        "Captured indexability/directive state from the selected retained run."
    )
    definition_depth = "Crawl-relative depth band; deep means retained crawl depth >= 3."
    definition_inlinks = (
        "Observed eligible-page share: numerator is unique retained in-scope sources linking "
        "to this target; denominator is in-scope pages with completed HTML link extraction. "
        "Repeated occurrences count once per source. Partial scope is not the whole site."
    )
    definition_quadrant = (
        "Search Console {metric} and GA sessions are separate axes. A quadrant needs one measured "
        "dimensionless URL value from each complete source for the same inclusive local-date window "
        "and timezone, with explicit unsampled/unthresholded/untruncated collection and known "
        "attribution. Nonnegative counts and source scopes remain separately labelled under "
        "strict calendar and cross-source juxtaposition policies; values are never summed."
    )
    if search_metric is None:
        pairs, pair_reason, blocked_keys = (
            {},
            "choose search_metric 'clicks' or 'impressions' to enable quadrants",
            {},
        )
    else:
        if index_con is None:
            raise BIExportError("provider quadrant projection requires a bounded disk index")
        pairs, pair_reason, blocked_keys = _quadrant_candidates(
            sources, search_metric, con=index_con
        )
    denominator = inlink_index.denominator if inlink_index is not None else None
    for ordinal, page in enumerate(run.pages_factory()):
        projected = _page_row(run, page, ordinal)
        status = projected["status_code"]
        if projected["status_code_state"] == "measured":
            label = (
                "successful_html"
                if status is not None and 200 <= status < 300
                else "redirect"
                if status is not None and 300 <= status < 400
                else "client_error"
                if status is not None and 400 <= status < 500
                else "server_error"
                if status is not None and 500 <= status < 600
                else "other_status"
            )
            yield _cohort_row(
                run,
                page,
                ordinal,
                "observed_status",
                definition_status,
                membership="member",
                state="available",
                value_label=label,
                value_number=status,
            )
        else:
            yield _cohort_row(
                run,
                page,
                ordinal,
                "observed_status",
                definition_status,
                membership="unclassified",
                state="unavailable",
                reason=projected["status_code_reason"],
            )
        indexability = projected["indexability"]
        index_state = projected["indexability_state"]
        yield _cohort_row(
            run,
            page,
            ordinal,
            "captured_indexability",
            definition_indexability,
            membership="member" if index_state == "measured" else "unclassified",
            state="available" if index_state == "measured" else index_state,
            reason=projected["indexability_reason"],
            value_label=str(indexability) if indexability is not None else None,
        )
        depth = projected["crawl_depth"]
        if projected["crawl_depth_state"] == "measured":
            yield _cohort_row(
                run,
                page,
                ordinal,
                "crawl_relative_depth",
                definition_depth,
                membership="member",
                state="available",
                value_label="deep" if depth >= 3 else "shallow",
                value_number=depth,
                threshold=3,
            )
        else:
            yield _cohort_row(
                run,
                page,
                ordinal,
                "crawl_relative_depth",
                definition_depth,
                membership="unclassified",
                state="unavailable",
                reason=projected["crawl_depth_reason"],
                threshold=3,
            )
        if inlink_index is not None and denominator:
            numerator = inlink_index.numerator(projected["url"])
            complete = (
                denominator == run.page_count and run.run_metadata.get("crawl_state") == "complete"
            )
            yield _cohort_row(
                run,
                page,
                ordinal,
                "observed_unique_inlink_share",
                definition_inlinks,
                membership="member",
                state="available" if complete else "partial",
                reason=None
                if complete
                else "share of observed eligible pages; crawl or extraction coverage is partial",
                value_number=numerator / denominator,
                numerator=numerator,
                denominator=denominator,
                extraction_state="complete" if denominator == run.page_count else "partial",
                extraction_reason=None
                if denominator == run.page_count
                else "failed, skipped or non-HTML sources excluded",
            )
        else:
            yield _cohort_row(
                run,
                page,
                ordinal,
                "observed_unique_inlink_share",
                definition_inlinks,
                membership="unclassified",
                state="unavailable",
                reason="retained occurrence evidence and completed source extraction are required",
                extraction_state="unavailable",
            )
        key = projected["url_key"]
        pair = pairs.get(key) if isinstance(key, str) else None
        blocked_reason = blocked_keys.get(key) if isinstance(key, str) else None
        if pair is not None:
            search, sessions = pair
            search_value = search["value"]
            sessions_value = sessions["value"]
            yield _cohort_row(
                run,
                page,
                ordinal,
                "search_visibility_vs_sessions",
                definition_quadrant.format(metric=search_metric),
                membership="member",
                state="available",
                value_label=(
                    "positive_search_positive_sessions"
                    if search_value > 0 and sessions_value > 0
                    else "positive_search_zero_sessions"
                    if search_value > 0
                    else "zero_search_positive_sessions"
                    if sessions_value > 0
                    else "zero_search_zero_sessions"
                ),
                search_metric=search_metric,
                search_value=search_value,
                search_state="measured",
                sessions_value=sessions_value,
                sessions_state="measured",
                period_start=search["period_start"],
                period_end=search["period_end"],
                timezone=search["timezone"],
                source_observations=[search, sessions],
            )
        else:
            yield _cohort_row(
                run,
                page,
                ordinal,
                "search_visibility_vs_sessions",
                definition_quadrant.format(
                    metric=search_metric or "selected Search Console metric"
                ),
                membership="unclassified",
                state="not_configured" if pair_reason else "incomplete",
                reason=pair_reason
                or blocked_reason
                or "no one-to-one complete compatible URL metric pair is retained",
                search_metric=search_metric,
                search_state="not_configured" if pair_reason else "unavailable",
                sessions_state="not_configured" if pair_reason else "unavailable",
            )


def _coverage_row(
    run_id: str,
    dataset: str,
    source_id: str,
    population: str,
    state: str,
    reason: str | None,
    source_rows: int | None,
    exported_rows: int | None,
    unavailable_rows: int | None,
    details: Any = None,
) -> dict[str, Any]:
    return {
        "run_id": run_id,
        "dataset": dataset,
        "evidence_source_id": source_id,
        "population": population,
        "state": state,
        "reason": reason,
        "source_rows": source_rows,
        "exported_rows": exported_rows,
        "unavailable_rows": unavailable_rows,
        "details_json": details if details is not None else {},
    }


def _coverage_rows(
    run: _RunInput,
    providers: list[_ProviderInput],
    dataset_results: dict[str, dict[str, Any]],
    group_index: Any,
) -> Iterator[dict[str, Any]]:
    pages_result = dataset_results["pages"]
    findings_result = dataset_results["findings"]
    links_result = dataset_results["link_occurrences"]
    metrics_result = dataset_results["metrics"]
    cohorts_result = dataset_results["cohorts"]
    yield _coverage_row(
        run.run_id,
        "run",
        run.run_id,
        "crawl",
        run.run_metadata["crawl_state"],
        run.run_metadata.get("crawl_reason"),
        run.page_count,
        run.page_count,
        0 if run.run_metadata["crawl_state"] == "complete" else run.page_count,
        {"source_kind": run.source_kind, "source_schema": run.source_schema},
    )
    for dataset, result, source_count in (
        ("pages", pages_result, run.page_count),
        ("findings", findings_result, run.finding_count),
        ("link_occurrences", links_result, run.link_count),
        ("metrics", metrics_result, None),
        ("cohorts", cohorts_result, run.page_count * 5),
    ):
        yield _coverage_row(
            run.run_id,
            dataset,
            run.run_id,
            "dataset",
            result["state"],
            result.get("reason"),
            source_count,
            result["row_count"],
            result.get("unavailable_rows"),
            result.get("coverage") or {},
        )
    yield from run.coverage_rows
    if group_index.source_count:
        total, linked = group_index.coverage()
        yield _coverage_row(
            run.run_id,
            "findings",
            "groups",
            "finding_groups",
            "partial" if total != linked else "represented",
            "some source groups are not referenced by a finding row" if total != linked else None,
            group_index.source_count,
            linked,
            total - linked,
            {"unlinked_ids": "one exact finding_group row per unlinked group follows"},
        )
        for group_id in group_index.unlinked():
            yield _coverage_row(
                run.run_id,
                "findings",
                group_id,
                "finding_group",
                "unrepresented",
                "source group is not referenced by a finding row",
                1,
                0,
                1,
                {"group_id": group_id},
            )
    if not providers:
        yield _coverage_row(
            run.run_id,
            "metrics",
            "provider_sources",
            "provider_source",
            "not_configured",
            "no provider join artifact was supplied",
            0,
            0,
            None,
        )
        return
    for provider in providers:
        if provider.error_state:
            yield _coverage_row(
                run.run_id,
                "metrics",
                provider.source_id,
                "provider_source",
                provider.error_state,
                provider.error_reason,
                provider.reported_rows,
                0,
                provider.reported_rows,
                {"file": provider.name, "sha256": provider.sha256, "bytes": provider.byte_count},
            )
            continue
        join = provider.join
        if join is None:
            yield _coverage_row(
                run.run_id,
                "metrics",
                provider.source_id,
                "provider_source",
                "not_joined",
                "normalized evidence was accepted but no crawl join was produced",
                None,
                0,
                None,
                {"file": provider.name, "sha256": provider.sha256},
            )
            continue
        summary = join.get("summary") or {}
        evidence = join.get("evidence") or {}
        collection = _provider_collection(evidence)
        collection_state = collection["state"]
        for population, key, state, reason in (
            ("matched", "matched_rows", "measured", None),
            ("crawl_only", "crawl_only", "unavailable", "no provider row matched this crawl URL"),
            ("external_only", "external_only", "measured", None),
            (
                "unkeyable_rows",
                "unkeyable_rows",
                "unavailable",
                "provider row has no usable URL key",
            ),
            (
                "unkeyable_pages",
                "unkeyable_pages",
                "unavailable",
                "crawl URL has no usable join key",
            ),
        ):
            count = summary.get(key)
            if type(count) is not int:
                count = 0
            yield _coverage_row(
                run.run_id,
                "metrics",
                provider.source_id,
                population,
                collection_state if collection_state != "complete" else state,
                collection.get("reason") or reason,
                count,
                count,
                count if state == "unavailable" else 0,
                {"provider": evidence.get("provider"), "operation": evidence.get("operation")},
            )
        observations, _info = _provider_observations(provider, join)
        unavailable_metrics = sum(
            1 for observation in observations if observation["entry"].get("state") != "measured"
        )
        yield _coverage_row(
            run.run_id,
            "metrics",
            provider.source_id,
            "metric_observations",
            collection_state,
            collection.get("reason"),
            len(observations),
            len(observations),
            unavailable_metrics,
            {
                "provider": evidence.get("provider"),
                "operation": evidence.get("operation"),
                "metrics": [metric.get("name") for metric in evidence.get("metrics") or []],
                "zero_is_measured": True,
            },
        )


def _schema_fields(fields: tuple[Field, ...]) -> list[dict[str, Any]]:
    return [
        {
            "name": field.name,
            "type": field.type,
            "nullable": field.nullable,
            "description": field.description,
        }
        for field in fields
    ]


def _manifest_dataset(
    name: str,
    fields: tuple[Field, ...],
    result: dict[str, Any],
    *,
    state: str,
    reason: str | None,
    source_population: str,
    row_count: int,
    coverage: dict[str, Any] | None = None,
) -> dict[str, Any]:
    _base_fields, grain, key = DATASET_SPECS[name]
    return {
        "schema_version": f"seohead.bi.{name}.v1",
        "grain": grain,
        "primary_key": list(key),
        "fields": _schema_fields(fields),
        "state": state,
        "reason": reason,
        "source_population": source_population,
        "row_count": row_count,
        "partition_count": result["partition_count"],
        "bytes": result["bytes"],
        "formula_safe_cells_prefixed": result["formula_safe_cells_prefixed"],
        "partitions": result["partitions"],
        "coverage": coverage or {},
    }


def _scan_run(
    path: str | os.PathLike[str], out_directory: Path, providers: list[_ProviderInput], **limits
) -> dict[str, Any]:
    from contextlib import closing

    try:
        con = open_scan(path, require_audit=False)
    except Exception as exc:
        raise BIExportError(f"scan input failed validation: {exc}") from exc
    with closing(con):
        run = _scan_source(
            path,
            con,
            max_scan_bytes=limits.pop("max_scan_bytes"),
            index_parent=out_directory.parent,
        )
        try:
            return _write_package(run, con, out_directory, providers, **limits)
        finally:
            if run.close is not None:
                run.close()


def _audit_run(
    value: Any, out_directory: Path, providers: list[_ProviderInput], **limits
) -> dict[str, Any]:
    document, raw, name = _read_audit_input(value)
    run = _audit_source(document, raw, name)
    limits.pop("max_scan_bytes", None)
    return _write_package(run, None, out_directory, providers, **limits)


def export_bi(
    *,
    scan: str | os.PathLike[str] | None = None,
    audit: Any = None,
    provider_joins: list[str | os.PathLike[str]] | None = None,
    out_dir: str | os.PathLike[str] | None = None,
    max_rows_per_file: int = DEFAULT_ROWS_PER_PARTITION,
    max_bytes_per_file: int = DEFAULT_BYTES_PER_PARTITION,
    max_output_bytes: int = DEFAULT_MAX_OUTPUT_BYTES,
    max_scan_bytes: int = DEFAULT_MAX_SCAN_BYTES,
    search_metric: str | None = None,
) -> dict[str, Any]:
    """Write a complete local BI package from one saved run and optional joins."""
    if (scan is None) == (audit is None):
        raise BIExportError("supply exactly one of scan or audit")
    if out_dir is None or not str(out_dir).strip():
        raise BIExportError("out_dir is required; BI packages are written only to an explicit path")
    if type(max_rows_per_file) is not int or not 1 <= max_rows_per_file <= MAX_ROWS_PER_PARTITION:
        raise BIExportError(f"max_rows_per_file must be 1..{MAX_ROWS_PER_PARTITION}")
    if (
        type(max_bytes_per_file) is not int
        or not 1024 <= max_bytes_per_file <= MAX_BYTES_PER_PARTITION
    ):
        raise BIExportError(f"max_bytes_per_file must be 1024..{MAX_BYTES_PER_PARTITION}")
    if (
        type(max_output_bytes) is not int
        or max_output_bytes < max_bytes_per_file
        or max_output_bytes > MAX_OUTPUT_BYTES
    ):
        raise BIExportError(
            f"max_output_bytes must be at least max_bytes_per_file and at most {MAX_OUTPUT_BYTES}"
        )
    if type(max_scan_bytes) is not int or not 1 <= max_scan_bytes <= MAX_SCAN_BYTES:
        raise BIExportError(f"max_scan_bytes must be 1..{MAX_SCAN_BYTES}")
    if search_metric is not None and search_metric not in {"clicks", "impressions"}:
        raise BIExportError("search_metric must be 'clicks', 'impressions', or null")
    provider_paths = list(provider_joins or [])
    if len(provider_paths) > MAX_PROVIDER_SOURCES:
        raise BIExportError(f"provider_joins is limited to {MAX_PROVIDER_SOURCES} sources")
    providers: list[_ProviderInput] = []
    provider_total_bytes = 0
    provider_hashes: set[str] = set()
    for path in provider_paths:
        provider = _provider_file(path)
        if provider.sha256 in provider_hashes:
            raise BIExportError("the same provider join artifact was supplied more than once")
        provider_hashes.add(provider.sha256)
        provider_total_bytes += provider.byte_count
        if provider_total_bytes > MAX_PROVIDER_JOIN_TOTAL_BYTES:
            raise BIExportError(
                f"provider join inputs exceed the {MAX_PROVIDER_JOIN_TOTAL_BYTES}-byte total bound"
            )
        providers.append(provider)
    destination = Path(out_dir).absolute()
    if destination.is_symlink() or os.path.lexists(destination):
        raise BIExportError(f"output directory already exists or is a symlink: {destination.name}")
    parent = destination.parent
    if not parent.is_dir():
        raise BIExportError("out_dir parent directory must exist")
    if parent.is_symlink():
        raise BIExportError("out_dir parent directory must not be a symlink")
    limits = {
        "max_rows_per_file": max_rows_per_file,
        "max_bytes_per_file": max_bytes_per_file,
        "max_output_bytes": max_output_bytes,
        "max_scan_bytes": max_scan_bytes,
        "search_metric": search_metric,
    }
    if scan is not None:
        return _scan_run(scan, destination, providers, **limits)
    return _audit_run(audit, destination, providers, **limits)


def _write_package(
    run: _RunInput,
    con: sqlite3.Connection | None,
    destination: Path,
    providers: list[_ProviderInput],
    *,
    max_rows_per_file: int,
    max_bytes_per_file: int,
    max_output_bytes: int,
    search_metric: str | None,
) -> dict[str, Any]:
    provider_joins: list[dict[str, Any] | None] = []
    dimension_names: set[str] = set()
    provider_observations: list[
        tuple[_ProviderInput, Iterable[dict[str, Any]], dict[str, Any]]
    ] = []
    for provider in providers:
        join = _join_from_provider(provider, run)
        provider.join = join
        provider_joins.append(join)
        if join is not None:
            if provider.store is not None:
                _validate_store_join(join)
            else:
                _validate_join(join)
            observations, info = _provider_observations(provider, join)
            dimension_names.update(info["dimensions"])
            provider_observations.append((provider, observations, info))
    dimension_fields = _safe_dimension_fields(dimension_names)
    metric_fields = METRIC_BASE_FIELDS + tuple(
        Field(field_name, "string", True, f"Provider dimension {dimension_name}.")
        for dimension_name, field_name in dimension_fields.items()
    )

    def page_rows() -> Iterator[dict[str, Any]]:
        for ordinal, page in enumerate(run.pages_factory()):
            yield _page_row(run, page, ordinal)

    def finding_rows() -> Iterator[dict[str, Any]]:
        for ordinal, finding in enumerate(run.findings_factory()):
            yield _finding_source(run, finding, ordinal, group_map)

    def link_rows() -> Iterator[dict[str, Any]]:
        if run.links_source_state == "unavailable":
            return
        for record in run.links_factory():
            source = record.get("source_url")
            destination_url = record.get("destination_url")
            if (
                not isinstance(source, str)
                or not source
                or not isinstance(destination_url, str)
                or not destination_url
            ):
                raise BIExportError("retained link occurrence lacks source or destination URL")
            rel_value = record.get("rel_json")
            try:
                rel = json.loads(rel_value) if isinstance(rel_value, str) else rel_value
            except json.JSONDecodeError as exc:
                raise BIExportError("retained link occurrence has invalid rel_json") from exc
            if rel is not None and not isinstance(rel, list):
                raise BIExportError("retained link rel evidence must be a JSON list")
            link_id = record.get("link_id")
            ordinal = record.get("ordinal")
            if type(link_id) is not int or type(ordinal) is not int:
                raise BIExportError("retained link occurrence lacks its numeric identity")
            source_key, source_key_state, source_key_reason = _page_key(source)
            destination_key, destination_key_state, destination_key_reason = _page_key(
                destination_url
            )
            evidence_representation = record.get("evidence_representation")
            if not isinstance(evidence_representation, str):
                raise BIExportError("retained link occurrence has no representation")
            occurrence_id = _digest_parts(run.run_id, str(link_id))[:32]
            payload = {key: _json_value(value) for key, value in record.items()}
            yield {
                "run_id": run.run_id,
                "link_occurrence_id": occurrence_id,
                "source_url": source,
                "source_url_key": source_key,
                "source_url_key_state": source_key_state,
                "source_url_key_reason": source_key_reason,
                "destination_url": destination_url,
                "destination_url_key": destination_key,
                "destination_url_key_state": destination_key_state,
                "destination_url_key_reason": destination_key_reason,
                "link_ordinal": ordinal,
                "evidence_representation": evidence_representation,
                "link_type": "hyperlink" if evidence_representation != "legacy_unknown" else None,
                "link_type_state": "measured"
                if evidence_representation != "legacy_unknown"
                else "unknown",
                "link_type_reason": None
                if evidence_representation != "legacy_unknown"
                else "legacy source does not retain link element type",
                "anchor": record.get("anchor"),
                "anchor_state": "measured" if record.get("anchor") is not None else "unknown",
                "anchor_reason": None
                if record.get("anchor") is not None
                else "anchor value was not retained",
                "nofollow": bool(record["nofollow"])
                if record.get("nofollow") is not None
                else None,
                "nofollow_state": "measured" if record.get("nofollow") is not None else "unknown",
                "nofollow_reason": None
                if record.get("nofollow") is not None
                else "nofollow state was not retained",
                "rel_json": rel,
                "rel_state": "measured" if rel is not None else "unknown",
                "rel_reason": None if rel is not None else "link relation values were not retained",
                "target": record.get("target"),
                "target_state": "measured" if record.get("target") is not None else "unknown",
                "target_reason": None
                if record.get("target") is not None
                else "target attribute was not retained",
                "placement": record.get("position") or None,
                "placement_state": "measured" if record.get("position") else "unknown",
                "placement_reason": None
                if record.get("position")
                else "link placement was not captured",
                "dom_context_json": None,
                "dom_context_state": "not_captured",
                "dom_context_reason": "scan.v1 does not retain DOM ancestor context for link occurrences",
                "source_document_id": record.get("source_document_id"),
                "source_document_state": "measured"
                if record.get("source_document_id") is not None
                else "not_captured",
                "source_document_reason": None
                if record.get("source_document_id") is not None
                else "source document identity was not retained",
                "raw_href": record.get("raw_href"),
                "source_link_id": link_id,
                "source_link_json": payload,
            }

    from seohead.reports.bi_index import GroupIndex, InlinkIndex, projection_index

    with ExitStack() as stack:
        index_con = stack.enter_context(
            projection_index(destination.parent, MAX_PROJECTION_INDEX_BYTES)
        )
        group_map = GroupIndex(index_con, run.groups)
        inlink_index = (
            InlinkIndex(index_con, run)
            if run.source_kind == "scan" and run.links_source_state != "unavailable"
            else None
        )
        temp = stack.enter_context(
            tempfile.TemporaryDirectory(prefix=".seohead-bi-", dir=destination.parent)
        )
        stage = Path(temp)
        os.chmod(stage, 0o700)
        budget = _OutputBudget(max_output_bytes)
        from seohead.reports.bi_index import write_group_members_companion

        group_companion = (
            write_group_members_companion(
                group_map,
                stage,
                run_id=run.run_id,
                source_audit_sha256=run.audit_sha256,
                max_rows_per_file=max_rows_per_file,
                max_bytes_per_file=max_bytes_per_file,
                budget=budget,
            )
            if group_map.source_count
            else None
        )
        dataset_outputs: dict[str, dict[str, Any]] = {}
        row_sources = {
            "pages": page_rows(),
            "findings": finding_rows(),
            "metrics": (row for row in _metric_rows(run, provider_observations, dimension_fields)),
            "link_occurrences": link_rows(),
            "cohorts": _cohort_rows(
                run,
                provider_observations,
                search_metric,
                index_con=index_con,
                inlink_index=inlink_index,
            ),
        }
        schemas = {
            "pages": PAGE_FIELDS,
            "findings": FINDING_FIELDS,
            "metrics": metric_fields,
            "link_occurrences": LINK_FIELDS,
            "cohorts": COHORT_FIELDS,
        }
        dataset_states = {
            "pages": (
                "available",
                None,
                "retained source page rows",
                {"source_rows": run.page_count},
            ),
            "findings": (
                "available" if run.run_metadata.get("audit_available", True) else "unavailable",
                None
                if run.run_metadata.get("audit_available", True)
                else "scan has no current saved audit; findings were not produced",
                "retained source findings",
                {"source_rows": run.finding_count},
            ),
            "metrics": (
                "not_configured"
                if not providers
                else "unavailable"
                if not any(join is not None for join in provider_joins)
                else "partial"
                if any(provider.error_state for provider in providers)
                else "available",
                "no provider join artifact was supplied"
                if not providers
                else "no supplied provider artifact exposes typed metric observations"
                if not any(join is not None for join in provider_joins)
                else "one or more provider artifacts expose only partial/untyped evidence"
                if any(provider.error_state for provider in providers)
                else None,
                "provider evidence rows; unmatched populations remain identifiable",
                {"provider_sources": len(providers)},
            ),
            "link_occurrences": (
                run.links_source_state,
                run.links_source_reason,
                "retained scan links" if con is not None else "audit does not retain link rows",
                {"source_rows": run.link_count},
            ),
            "cohorts": (
                "available",
                None,
                "derived retained URL observations; unclassified rows retain their reason",
                {"search_metric": search_metric, "page_rows": run.page_count},
            ),
        }
        for name in ("pages", "findings", "metrics", "link_occurrences", "cohorts"):
            state, reason, population, coverage = dataset_states[name]
            writer = _PartitionWriter(
                stage,
                name,
                schemas[name],
                max_rows_per_file=max_rows_per_file,
                max_bytes_per_file=max_bytes_per_file,
                budget=budget,
            )
            if name == "link_occurrences" and state == "unavailable":
                row_sources[name] = iter(())
            try:
                for row in row_sources[name]:
                    writer.write(row)
                result = writer.finish()
            finally:
                writer.close()
            expected = coverage.get("source_rows")
            if expected is not None and result["row_count"] != expected:
                raise BIExportError(
                    f"{name} row conservation failed: source has {expected}, output has {result['row_count']}"
                )
            if name == "metrics" and providers:
                provider_observation_count = sum(
                    len(observations) for _, observations, _ in provider_observations
                )
                if result["row_count"] != provider_observation_count:
                    raise BIExportError("provider metric row conservation failed")
            dataset_outputs[name] = {
                **result,
                "state": state,
                "reason": reason,
                "source_population": population,
                "coverage": coverage,
            }

        coverage_writer = _PartitionWriter(
            stage,
            "coverage",
            COVERAGE_FIELDS,
            max_rows_per_file=max_rows_per_file,
            max_bytes_per_file=max_bytes_per_file,
            budget=budget,
        )
        try:
            for row in _coverage_rows(run, providers, dataset_outputs, group_map):
                coverage_writer.write(row)
            coverage_result = coverage_writer.finish()
        finally:
            coverage_writer.close()
        dataset_outputs["coverage"] = {
            **coverage_result,
            "state": "available",
            "reason": None,
            "source_population": "input artifact and declared check coverage",
            "coverage": {},
        }

        fields_by_dataset = {
            "pages": PAGE_FIELDS,
            "findings": FINDING_FIELDS,
            "metrics": metric_fields,
            "link_occurrences": LINK_FIELDS,
            "cohorts": COHORT_FIELDS,
            "coverage": COVERAGE_FIELDS,
        }
        datasets = {
            name: _manifest_dataset(
                name,
                fields_by_dataset[name],
                dataset_outputs[name],
                state=dataset_outputs[name]["state"],
                reason=dataset_outputs[name]["reason"],
                source_population=dataset_outputs[name]["source_population"],
                row_count=dataset_outputs[name]["row_count"],
                coverage=dataset_outputs[name]["coverage"],
            )
            for name in fields_by_dataset
        }
        provider_manifest = []
        for provider, join in zip(providers, provider_joins, strict=True):
            if provider.error_state:
                provider_manifest.append(
                    {
                        "provider_source_id": provider.source_id,
                        "file": provider.name,
                        "sha256": provider.sha256,
                        "bytes": provider.byte_count,
                        "state": provider.error_state,
                        "reason": provider.error_reason,
                        "source_rows": provider.reported_rows,
                    }
                )
                continue
            if join is None:
                provider_manifest.append(
                    {
                        "provider_source_id": provider.source_id,
                        "file": provider.name,
                        "sha256": provider.sha256,
                        "bytes": provider.byte_count,
                        "state": "unsupported",
                        "reason": provider.error_reason,
                    }
                )
                continue
            provider_manifest.append(
                {
                    "provider_source_id": provider.source_id,
                    "file": provider.name,
                    "sha256": provider.sha256,
                    "bytes": provider.byte_count,
                    "state": _provider_collection(join.get("evidence") or {})["state"],
                    "reason": _provider_collection(join.get("evidence") or {}).get("reason"),
                    "format": join.get("format"),
                    "evidence": join.get("evidence"),
                    "summary": join.get("summary"),
                    "compatibility": provider.compatibility,
                }
            )
        manifest = {
            "format": MANIFEST_FORMAT,
            "schema_version": BI_SCHEMA_VERSION,
            "compatibility": {
                "policy": "v1 field and key changes require a new dataset schema version",
                "dynamic_dimensions": "metrics dataset may append source-declared string dimension columns; mapping is recorded here",
                "null_encoding": "nullable CSV cells are empty only when the companion state/reason columns identify unavailability; numeric zero is emitted as 0",
                "csv_safety": "formula-leading strings are prefixed with an apostrophe; the manifest counts transformed cells",
            },
            "run": run.run_metadata,
            "input": {
                "kind": run.source_kind,
                "schema": run.source_schema,
                "name": run.source_name,
                "sha256": run.source_sha256,
                "bytes": run.source_bytes,
            },
            "crawl_completeness": {
                "state": run.run_metadata.get("crawl_state", "unknown"),
                "reason": run.run_metadata.get("crawl_reason"),
                "verified_by_projection": False,
            },
            "url_key_policy": {
                "version": "external_join.v1",
                "ignore_query": False,
                "ignore_scheme": False,
                "casefold_path": False,
                "source": "seohead.checks.external_join.normalize_join_key",
            },
            "provider_sources": provider_manifest,
            "metrics_dimension_columns": dimension_fields,
            "resource_bounds": {
                "max_pages": MAX_PAGES,
                "max_findings": MAX_FINDINGS,
                "max_link_occurrences": MAX_LINK_OCCURRENCES,
                "max_provider_sources": MAX_PROVIDER_SOURCES,
                "max_provider_rows_per_source": MAX_PROVIDER_ROWS_PER_SOURCE,
                "max_rows_per_partition": max_rows_per_file,
                "max_bytes_per_partition": max_bytes_per_file,
                "max_output_bytes": max_output_bytes,
                "max_output_partitions": MAX_OUTPUT_PARTITIONS,
                "max_manifest_bytes": MAX_MANIFEST_BYTES,
                "max_cell_bytes": MAX_CELL_BYTES,
                "max_projection_index_bytes": MAX_PROJECTION_INDEX_BYTES,
                "projection_index_cache_bytes": 2 * 1024 * 1024,
                "min_free_disk_bytes": MIN_FREE_DISK_BYTES,
                "overflow": "fail without publishing any complete package; no truncation or sampling",
            },
            "datasets": datasets,
        }
        if group_companion is not None:
            manifest["group_members"] = group_companion
            from seohead.reports.bi_index import verify_group_member_references

            verify_group_member_references(stage, manifest)
        manifest_path = stage / "manifest.json"
        manifest_bytes = (
            json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2, allow_nan=False)
            + "\n"
        ).encode("utf-8")
        if len(manifest_bytes) > MAX_MANIFEST_BYTES:
            raise BIExportError("BI manifest exceeds its byte bound")
        budget.reserve_bytes(len(manifest_bytes))
        manifest_path.write_bytes(manifest_bytes)
        os.chmod(manifest_path, 0o600)
        os.replace(stage, destination)
    return {
        "format": "seohead.bi-export-result.v1",
        "schema_version": BI_SCHEMA_VERSION,
        "manifest": str(destination / "manifest.json"),
        "output_directory": str(destination),
        "run_id": run.run_id,
        "crawl_state": run.run_metadata.get("crawl_state"),
        "datasets": {
            name: {
                "state": item["state"],
                "rows": item["row_count"],
                "partitions": item["partition_count"],
            }
            for name, item in dataset_outputs.items()
        },
    }
