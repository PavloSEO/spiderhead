"""Normalize an explicitly mapped third-party crawl CSV bundle.

The returned document remains foreign crawl evidence. It is not a native
``scan.v1`` artifact, an Export Analyzer audit, or a claim that missing fields
were measured cleanly.
"""

from __future__ import annotations

import csv
import hashlib
import json
import os
import stat
from collections import Counter
from pathlib import Path, PurePosixPath
from typing import Any
from urllib.parse import urlsplit

from seohead.checks.sitemap import normalize_url

MANIFEST_VERSION = "third_party_crawl_manifest.v1"
SCHEMA_VERSION = "third_party_crawl.v1"
MAX_MANIFEST_BYTES = 64 * 1024
MAX_TOTAL_CSV_BYTES = 32 * 1024 * 1024
MAX_ROWS_PER_DATASET = 100_000

# Canonical field order and types are part of third_party_crawl.v1.
DATASET_FIELDS: dict[str, tuple[tuple[str, str], ...]] = {
    "pages": (
        ("url", "url"),
        ("status_code", "integer"),
        ("content_type", "string"),
        ("title", "string"),
        ("canonical_url", "url"),
        ("indexability", "string"),
    ),
    "links": (
        ("source_url", "url"),
        ("destination_url", "url"),
        ("anchor", "string"),
        ("rel", "string"),
        ("position", "string"),
        ("status_code", "integer"),
    ),
    "statuses": (
        ("url", "url"),
        ("status_code", "integer"),
        ("status_text", "string"),
    ),
    "redirects": (
        ("source_url", "url"),
        ("destination_url", "url"),
        ("status_code", "integer"),
        ("hop", "integer"),
    ),
}

_SOURCE_FIELDS = {
    "product",
    "version",
    "format",
    "format_version",
    "exported_at",
    "crawl_state",
    "crawl_state_reason",
}
_MANIFEST_FIELDS = {"schema_version", "source", "datasets"}
_DATASET_CONFIG_FIELDS = {"file", "columns", "reason"}


class ExternalCrawlImportError(ValueError):
    """The manifest or one of its source files violates the import contract."""


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ExternalCrawlImportError(f"duplicate JSON key: {key!r}")
        result[key] = value
    return result


def _reject_constant(value: str) -> None:
    raise ExternalCrawlImportError(f"non-JSON numeric constant: {value}")


def _validate_unicode(value: Any) -> None:
    if isinstance(value, str):
        if any(0xD800 <= ord(char) <= 0xDFFF for char in value):
            raise ExternalCrawlImportError("manifest contains an unpaired Unicode surrogate")
    elif isinstance(value, dict):
        for key, child in value.items():
            _validate_unicode(key)
            _validate_unicode(child)
    elif isinstance(value, list):
        for child in value:
            _validate_unicode(child)


def _read_manifest(path: Path) -> tuple[dict[str, Any], bytes]:
    try:
        if path.is_symlink() or not path.is_file():
            raise ExternalCrawlImportError("manifest must be a regular, non-symlink JSON file")
        size = path.stat().st_size
        if size > MAX_MANIFEST_BYTES:
            raise ExternalCrawlImportError(f"manifest exceeds the {MAX_MANIFEST_BYTES}-byte limit")
        raw = path.read_bytes()
    except ExternalCrawlImportError:
        raise
    except (OSError, ValueError) as exc:
        detail = getattr(exc, "strerror", None) or str(exc)
        raise ExternalCrawlImportError(f"cannot read manifest {path.name!r}: {detail}") from exc
    if len(raw) != size:
        raise ExternalCrawlImportError("manifest changed while it was read")
    try:
        document = json.loads(
            raw.decode("utf-8-sig"),
            object_pairs_hook=_unique_object,
            parse_constant=_reject_constant,
        )
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise ExternalCrawlImportError(f"manifest is not valid UTF-8 JSON: {exc}") from exc
    if not isinstance(document, dict):
        raise ExternalCrawlImportError("manifest root must be a JSON object")
    _validate_unicode(document)
    return document, raw


def _mapping(value: Any, label: str, allowed: set[str]) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ExternalCrawlImportError(f"{label} must be an object")
    unknown = set(value) - allowed
    if unknown:
        raise ExternalCrawlImportError(
            f"{label} has unknown field(s): {', '.join(sorted(unknown))}"
        )
    return value


def _required_text(value: Any, label: str, *, max_length: int = 200) -> str:
    if not isinstance(value, str) or not value.strip() or len(value) > max_length:
        raise ExternalCrawlImportError(
            f"{label} must be a non-empty string of at most {max_length} characters"
        )
    return value.strip()


def _source_identity(value: Any) -> dict[str, Any]:
    source = _mapping(value, "source", _SOURCE_FIELDS)
    identity = {
        "product": _required_text(source.get("product"), "source.product"),
        "version": _required_text(source.get("version"), "source.version"),
        "format": _required_text(source.get("format"), "source.format"),
        "format_version": _required_text(source.get("format_version"), "source.format_version"),
        "exported_at": source.get("exported_at"),
        "crawl_state": source.get("crawl_state", "unknown"),
        "crawl_state_reason": source.get("crawl_state_reason"),
    }
    if identity["exported_at"] is not None and not isinstance(identity["exported_at"], str):
        raise ExternalCrawlImportError("source.exported_at must be a string or null")
    if not isinstance(identity["crawl_state"], str) or identity["crawl_state"] not in {
        "complete",
        "partial",
        "unknown",
    }:
        raise ExternalCrawlImportError("source.crawl_state must be complete, partial, or unknown")
    if identity["crawl_state_reason"] is not None and not isinstance(
        identity["crawl_state_reason"], str
    ):
        raise ExternalCrawlImportError("source.crawl_state_reason must be a string or null")
    return identity


def _dataset_path(root: Path, value: Any, used: set[Path], label: str) -> Path:
    relative = _required_text(value, f"{label}.file", max_length=512)
    candidate = PurePosixPath(relative)
    if (
        candidate.is_absolute()
        or "\\" in relative
        or ":" in relative
        or any(part in {"", ".", ".."} for part in candidate.parts)
    ):
        raise ExternalCrawlImportError(
            f"{label}.file must be a relative path inside the manifest directory"
        )
    if candidate.suffix.lower() != ".csv":
        raise ExternalCrawlImportError(
            f"{label}.file must name a UTF-8 CSV file; XLSX is not supported"
        )
    path = root.joinpath(*candidate.parts)
    try:
        component = root
        for part in candidate.parts:
            component = component / part
            if component.is_symlink():
                raise ExternalCrawlImportError(
                    f"{label}.file cannot contain symlink path components"
                )
        if not path.is_file() or not path.resolve().is_relative_to(root):
            raise ExternalCrawlImportError(
                f"{label}.file must be an existing regular file inside the manifest directory"
            )
        resolved = path.resolve()
        if resolved in used:
            raise ExternalCrawlImportError(
                f"source file is assigned to more than one dataset: {relative!r}"
            )
        used.add(resolved)
        return resolved
    except OSError as exc:
        raise ExternalCrawlImportError(
            f"cannot inspect {label}.file {relative!r}: {exc.strerror}"
        ) from exc


def _parse_value(raw: str | None, kind: str, label: str) -> tuple[Any, str | None]:
    if raw is None or raw == "":
        return None, None
    if kind == "string":
        return raw, None
    if kind == "integer":
        try:
            value = int(raw, 10)
        except ValueError:
            return None, f"{label} is not an integer"
        if label.startswith("status_code") and not 100 <= value <= 599:
            return None, f"{label} is outside the HTTP status-code range 100..599"
        if label.startswith("hop") and value < 0:
            return None, f"{label} must not be negative"
        return value, None
    if kind == "url":
        try:
            parts = urlsplit(raw)
            if parts.scheme.lower() not in {"http", "https"} or not parts.hostname:
                raise ValueError
            if parts.username is not None or parts.password is not None:
                return None, f"{label} contains embedded URL credentials"
            normalize_url(raw)
            return raw, None
        except (ValueError, UnicodeError):
            return None, f"{label} is not an absolute HTTP(S) URL"
    raise AssertionError(f"unhandled field type: {kind}")


def _read_dataset(
    dataset: str,
    config: dict[str, Any],
    root: Path,
    used: set[Path],
    remaining_bytes: int,
) -> tuple[dict[str, Any], int]:
    config = _mapping(config, f"datasets.{dataset}", _DATASET_CONFIG_FIELDS)
    columns = _mapping(
        config.get("columns", {}),
        f"datasets.{dataset}.columns",
        {field for field, _ in DATASET_FIELDS[dataset]},
    )
    if any(not isinstance(column, str) or not column for column in columns.values()):
        raise ExternalCrawlImportError(
            f"datasets.{dataset}.columns values must be non-empty header names"
        )
    if len(set(columns.values())) != len(columns):
        raise ExternalCrawlImportError(
            f"datasets.{dataset}.columns cannot map two fields to one source column"
        )

    filename = config.get("file")
    if filename is None:
        reason = config.get("reason")
        if not isinstance(reason, str) or not reason.strip():
            raise ExternalCrawlImportError(f"datasets.{dataset} without a file requires a reason")
        fields = {
            field: {
                "state": "not_exported",
                "source_column": None,
                "rows": 0,
                "missing": 0,
                "invalid": 0,
                "reason": reason.strip(),
            }
            for field, _ in DATASET_FIELDS[dataset]
        }
        return (
            {
                "state": "unavailable",
                "reason": reason.strip(),
                "row_count": 0,
                "records": [],
                "field_coverage": fields,
                "duplicate_summary": {"duplicate_groups": 0, "duplicate_rows": 0},
            },
            0,
        )

    path = _dataset_path(root, filename, used, f"datasets.{dataset}")
    try:
        before = path.stat()
    except OSError as exc:
        raise ExternalCrawlImportError(
            f"cannot stat source CSV {path.name!r}: {exc.strerror}"
        ) from exc
    if not stat.S_ISREG(before.st_mode):
        raise ExternalCrawlImportError(f"source CSV {path.name!r} is not a regular file")
    if before.st_size > min(MAX_TOTAL_CSV_BYTES, remaining_bytes):
        raise ExternalCrawlImportError(
            f"source CSV {path.name!r} exceeds the remaining {min(MAX_TOTAL_CSV_BYTES, remaining_bytes)}-byte import budget"
        )
    digest = hashlib.sha256()
    try:
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        with path.open("r", encoding="utf-8-sig", newline="") as stream:
            reader = csv.DictReader(stream, strict=True)
            headers = reader.fieldnames or []
            if not headers or any(not header for header in headers):
                raise ExternalCrawlImportError(
                    f"source CSV {path.name!r} requires non-empty headers"
                )
            if len(set(headers)) != len(headers):
                raise ExternalCrawlImportError(f"source CSV {path.name!r} has duplicate headers")
            records: list[dict[str, Any]] = []
            field_stats = {
                field: {"rows": 0, "present": 0, "missing": 0, "invalid": 0}
                for field, _ in DATASET_FIELDS[dataset]
            }
            row_errors: list[dict[str, Any]] = []
            for row_number, raw_row in enumerate(reader, start=2):
                if row_number - 1 > MAX_ROWS_PER_DATASET:
                    raise ExternalCrawlImportError(
                        f"source CSV {path.name!r} exceeds the {MAX_ROWS_PER_DATASET}-row dataset limit"
                    )
                if None in raw_row:
                    raise ExternalCrawlImportError(
                        f"source CSV {path.name!r} row {row_number} has more cells than its header"
                    )
                raw = {key: value for key, value in raw_row.items() if key is not None}
                record: dict[str, Any] = {"source_row": row_number}
                invalid_fields: list[dict[str, str]] = []
                for field, kind in DATASET_FIELDS[dataset]:
                    stats = field_stats[field]
                    stats["rows"] += 1
                    source_column = columns.get(field)
                    if source_column is None or source_column not in headers:
                        record[field] = None
                        stats["missing"] += 1
                        continue
                    raw_value = raw.get(source_column)
                    parsed, error = _parse_value(raw_value, kind, f"{field} at row {row_number}")
                    if error:
                        credential_url = "embedded URL credentials" in error
                        record[field] = (
                            raw_value
                            if kind == "url" and raw_value is not None and not credential_url
                            else None
                        )
                        if kind == "url":
                            key_name = {
                                "url": "url_key",
                                "source_url": "source_url_key",
                                "destination_url": "destination_url_key",
                                "canonical_url": "canonical_url_key",
                            }[field]
                            record[key_name] = None
                        elif raw_value is not None:
                            record.setdefault("unparsed_values", {})[field] = raw_value
                        stats["invalid"] += 1
                        if raw_value not in (None, ""):
                            stats["present"] += 1
                        invalid_fields.append({"field": field, "reason": error})
                        continue
                    record[field] = parsed
                    if kind == "url" and parsed is not None:
                        key_name = {
                            "url": "url_key",
                            "source_url": "source_url_key",
                            "destination_url": "destination_url_key",
                            "canonical_url": "canonical_url_key",
                        }[field]
                        record[key_name] = normalize_url(parsed)
                    if parsed is None:
                        stats["missing"] += 1
                    else:
                        stats["present"] += 1
                if invalid_fields:
                    record["field_errors"] = invalid_fields
                    row_errors.append({"source_row": row_number, "fields": invalid_fields})
                records.append(record)
    except ExternalCrawlImportError:
        raise
    except (OSError, UnicodeError, csv.Error) as exc:
        raise ExternalCrawlImportError(f"cannot parse source CSV {path.name!r}: {exc}") from exc
    try:
        after = path.stat()
    except OSError as exc:
        raise ExternalCrawlImportError(
            f"cannot restat source CSV {path.name!r}: {exc.strerror}"
        ) from exc
    if (before.st_size, before.st_mtime_ns, before.st_ino) != (
        after.st_size,
        after.st_mtime_ns,
        after.st_ino,
    ):
        raise ExternalCrawlImportError(f"source CSV {path.name!r} changed during import")

    # Keep every row. A normalized URL collision is described; it never silently
    # collapses duplicate observations into one record.
    identity_fields = {
        "pages": ("url_key",),
        "links": ("source_url_key", "destination_url_key", "anchor"),
        "statuses": ("url_key", "status_code"),
        "redirects": ("source_url_key", "hop"),
    }[dataset]
    seen: Counter[tuple[Any, ...]] = Counter()
    for record in records:
        values: list[Any] = []
        for field in identity_fields:
            value = record.get(field)
            values.append(value)
        if all(value is not None for value in values):
            seen[tuple(values)] += 1
    duplicate_groups = sum(count > 1 for count in seen.values())
    duplicate_rows = sum(count - 1 for count in seen.values() if count > 1)

    field_coverage: dict[str, Any] = {}
    for field, _ in DATASET_FIELDS[dataset]:
        stats = field_stats[field]
        source_column = columns.get(field)
        if source_column is None:
            state, reason = "not_exported", "manifest does not map this field"
        elif source_column not in headers:
            state, reason = "unavailable", "mapped source column is absent from the CSV header"
        elif not records:
            state, reason = "unavailable", "source CSV contains no data rows"
        elif stats["invalid"] or stats["missing"]:
            state, reason = "partial", "one or more source values are blank or invalid"
        else:
            state, reason = "complete", "all rows contain a valid mapped value"
        field_coverage[field] = {
            "state": state,
            "scope": "values in the supplied CSV rows only; does not describe whole-crawl coverage",
            "source_column": source_column,
            "rows": stats["rows"],
            "present": stats["present"],
            "missing": stats["missing"],
            "invalid": stats["invalid"],
            "reason": reason,
        }
    relative_name = path.relative_to(root).as_posix()
    dataset_result = {
        "state": "available" if records else "unavailable",
        "reason": None if records else "source CSV contains no data rows",
        "row_count": len(records),
        "records": records,
        "field_coverage": field_coverage,
        "duplicate_summary": {
            "duplicate_groups": duplicate_groups,
            "duplicate_rows": duplicate_rows,
        },
        "row_errors": row_errors,
    }
    dataset_result["provenance"] = {
        "file": relative_name,
        "bytes": before.st_size,
        "sha256": digest.hexdigest(),
        "headers": headers,
        "unmapped_headers": [header for header in headers if header not in columns.values()],
    }
    return dataset_result, before.st_size


def import_third_party_crawl(manifest_path: str | os.PathLike[str]) -> dict[str, Any]:
    """Read a mapped CSV bundle into the versioned third_party_crawl.v1 envelope."""
    if not isinstance(manifest_path, (str, os.PathLike)) or not str(manifest_path).strip():
        raise ExternalCrawlImportError("manifest_path must be a non-empty local path")
    try:
        manifest = Path(manifest_path)
    except (TypeError, ValueError) as exc:
        raise ExternalCrawlImportError("manifest_path must be a valid local path") from exc
    document, raw_manifest = _read_manifest(manifest)
    if set(document) != _MANIFEST_FIELDS:
        missing = _MANIFEST_FIELDS - set(document)
        unknown = set(document) - _MANIFEST_FIELDS
        parts = []
        if missing:
            parts.append("missing " + ", ".join(sorted(missing)))
        if unknown:
            parts.append("unknown " + ", ".join(sorted(unknown)))
        raise ExternalCrawlImportError("manifest fields: " + "; ".join(parts))
    if document.get("schema_version") != MANIFEST_VERSION:
        raise ExternalCrawlImportError(
            f"unsupported manifest schema_version; expected {MANIFEST_VERSION!r}"
        )
    source = _source_identity(document.get("source"))
    datasets_value = document.get("datasets")
    if not isinstance(datasets_value, dict):
        raise ExternalCrawlImportError(
            "datasets must be an object keyed by pages, links, statuses, or redirects"
        )
    unknown_datasets = set(datasets_value) - set(DATASET_FIELDS)
    if unknown_datasets:
        raise ExternalCrawlImportError(f"unknown dataset(s): {', '.join(sorted(unknown_datasets))}")

    root = manifest.resolve().parent
    used: set[Path] = set()
    datasets: dict[str, Any] = {}
    total_bytes = 0
    for name in DATASET_FIELDS:
        if name not in datasets_value:
            datasets[name] = {
                "state": "unavailable",
                "reason": "dataset was not declared in the manifest",
                "row_count": 0,
                "records": [],
                "field_coverage": {
                    field: {
                        "state": "not_exported",
                        "source_column": None,
                        "rows": 0,
                        "present": 0,
                        "missing": 0,
                        "invalid": 0,
                        "reason": "dataset was not declared in the manifest",
                    }
                    for field, _ in DATASET_FIELDS[name]
                },
                "duplicate_summary": {"duplicate_groups": 0, "duplicate_rows": 0},
            }
            continue
        config = datasets_value[name]
        result, size = _read_dataset(
            name,
            config,
            root,
            used,
            MAX_TOTAL_CSV_BYTES - total_bytes,
        )
        total_bytes += size
        datasets[name] = result

    rows = {name: dataset["row_count"] for name, dataset in datasets.items()}
    field_states = [
        field["state"]
        for dataset in datasets.values()
        for field in dataset["field_coverage"].values()
    ]
    import_state = (
        "unavailable"
        if all(state == "not_exported" for state in field_states)
        else "complete"
        if all(state == "complete" for state in field_states)
        else "partial"
    )
    return {
        "schema_version": SCHEMA_VERSION,
        "evidence_kind": "third_party_crawl_export",
        "compatibility": {
            "scan_v1": False,
            "sf_audit_json": False,
            "reason": "external observations are not native scan or Screaming Frog analyzer evidence",
        },
        "source": source,
        "crawl_completeness": {
            "state": (
                f"source_claimed_{source['crawl_state']}"
                if source["crawl_state"] != "unknown"
                else "unknown"
            ),
            "verified_by_importer": False,
            "reason": source["crawl_state_reason"]
            or "crawl completion is a source claim and was not independently verified",
        },
        "import_coverage": {
            "state": import_state,
            "reason": "coverage describes mapped values and declared datasets in the supplied export only",
        },
        "provenance": {
            "manifest_schema_version": MANIFEST_VERSION,
            "manifest_file": manifest.name,
            "manifest_sha256": hashlib.sha256(raw_manifest).hexdigest(),
            "input_bytes": total_bytes,
        },
        "datasets": datasets,
        "summary": {"rows_by_dataset": rows, "input_bytes": total_bytes},
    }
