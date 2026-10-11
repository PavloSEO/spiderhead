"""Verified producer metadata embedded by clean distribution builds.

This is provenance for an installed package, not an attestation system.  A
manifest names the Git revision that built it and hashes the Python sources it
ships, so a runtime without a source checkout can reject altered metadata or
code instead of guessing a revision from an unrelated repository.
"""

from __future__ import annotations

import contextlib
import hashlib
import importlib.metadata
import json
import os
import re
import tempfile
from base64 import urlsafe_b64encode
from collections.abc import Collection
from csv import reader
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

MANIFEST_FILENAME = "_build_provenance.json"
MANIFEST_VERSION = 1
_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
_REVISION = re.compile(r"[0-9a-f]{40}\Z")


class BuildProvenanceError(ValueError):
    """Raised when a packaged producer identity is absent or cannot be verified."""


@dataclass(frozen=True)
class PackagedProvenance:
    """Validated package identity from the embedded manifest."""

    version: str
    revision: str
    built_at: str | None = None


def package_source_files(source_root: Path) -> tuple[str, ...]:
    """Return every packaged runtime file, relative to ``source_root``.

    The manifest intentionally covers Python, SQL, JSON, skills, and any other
    shipped package data.  It excludes only itself and bytecode caches, so a
    changed ``scan_v1.sql`` cannot validate under the producer revision that
    built a different schema.
    """
    package = source_root / "seohead"
    if not package.is_dir():
        raise BuildProvenanceError("packaged provenance has no seohead package directory")
    package_root = package.resolve()
    files: list[str] = []
    for path in sorted(package.rglob("*")):
        if path.is_symlink():
            raise BuildProvenanceError("packaged provenance refuses symlinked source paths")
        if path == _manifest_path(source_root) or "__pycache__" in path.parts:
            continue
        if not path.is_file():
            continue
        if path.suffix in {".pyc", ".pyo"}:
            continue
        if package_root not in path.resolve().parents:
            raise BuildProvenanceError(
                "packaged provenance refuses symlinked or escaping source files"
            )
        files.append(path.relative_to(source_root).as_posix())
    return tuple(files)


def _digest(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def build_manifest(source_root: Path, *, version: str, revision: str) -> dict[str, Any]:
    """Build validated metadata for a clean source tree or sdist staging tree."""
    if not isinstance(version, str) or not version:
        raise BuildProvenanceError("packaged provenance version must be a nonempty string")
    if not isinstance(revision, str) or not _REVISION.fullmatch(revision):
        raise BuildProvenanceError("packaged provenance revision must be a full lowercase Git SHA")
    files = package_source_files(source_root)
    if not files:
        raise BuildProvenanceError("packaged provenance has no runtime source files")
    source_date_epoch = os.environ.get("SOURCE_DATE_EPOCH")
    try:
        built_at = (
            datetime.fromtimestamp(int(source_date_epoch), timezone.utc)
            if source_date_epoch is not None
            else datetime.now(timezone.utc)
        )
    except (OverflowError, OSError, ValueError) as exc:
        raise BuildProvenanceError("SOURCE_DATE_EPOCH must be a valid Unix timestamp") from exc
    return {
        "manifest_version": MANIFEST_VERSION,
        "package_version": version,
        "revision": revision,
        "built_at": built_at.isoformat(timespec="seconds").replace("+00:00", "Z"),
        "files": {relative: _digest(source_root / relative) for relative in files},
    }


def _manifest_path(source_root: Path) -> Path:
    return source_root / "seohead" / MANIFEST_FILENAME


def write_manifest(source_root: Path, manifest: dict[str, Any]) -> None:
    """Write metadata only into a build or sdist staging tree."""
    target = _manifest_path(source_root)
    descriptor, temporary = tempfile.mkstemp(prefix=".build-provenance-", dir=target.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            output.write(json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n")
        os.replace(temporary, target)
    except BaseException:
        with contextlib.suppress(FileNotFoundError):
            Path(temporary).unlink()
        raise


def remove_manifest(source_root: Path) -> None:
    """Remove stale metadata from a managed build or release tree only."""
    _manifest_path(source_root).unlink(missing_ok=True)


def validate_staged_files(source_root: Path, expected_files: Collection[str]) -> None:
    """Refuse staged package files that differ from current setuptools outputs."""
    actual = set(package_source_files(source_root))
    expected = set(expected_files)
    unexpected = sorted(actual - expected)
    missing = sorted(expected - actual)
    if unexpected or missing:
        details = []
        if unexpected:
            details.append(f"unexpected staged files: {', '.join(unexpected)}")
        if missing:
            details.append(f"missing staged files: {', '.join(missing)}")
        raise BuildProvenanceError(
            "staged package outputs disagree with current build: " + "; ".join(details)
        )


def _read_manifest(source_root: Path) -> dict[str, Any]:
    path = _manifest_path(source_root)
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as exc:
        raise BuildProvenanceError("packaged producer metadata is unavailable") from exc
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise BuildProvenanceError("packaged producer metadata is unreadable") from exc
    if not isinstance(value, dict):
        raise BuildProvenanceError("packaged producer metadata must be a JSON object")
    return value


def validate_manifest(
    source_root: Path, manifest: dict[str, Any] | None = None
) -> PackagedProvenance:
    """Validate schema and every packaged Python hash without mutating runtime files."""
    value = _read_manifest(source_root) if manifest is None else manifest
    if set(value) not in (
        {"manifest_version", "package_version", "revision", "files"},
        {"manifest_version", "package_version", "revision", "built_at", "files"},
    ):
        raise BuildProvenanceError("packaged producer metadata has an unsupported schema")
    if type(value["manifest_version"]) is not int or value["manifest_version"] != MANIFEST_VERSION:
        raise BuildProvenanceError("packaged producer metadata version is unsupported")
    if not isinstance(value["package_version"], str) or not value["package_version"]:
        raise BuildProvenanceError("packaged producer metadata has an invalid package version")
    if not isinstance(value["revision"], str) or not _REVISION.fullmatch(value["revision"]):
        raise BuildProvenanceError("packaged producer metadata has an invalid Git revision")
    built_at = value.get("built_at")
    if "built_at" in value:
        if not isinstance(built_at, str):
            raise BuildProvenanceError("packaged producer metadata has an invalid build time")
        try:
            parsed_built_at = datetime.fromisoformat(built_at.replace("Z", "+00:00"))
        except ValueError as exc:
            raise BuildProvenanceError("packaged producer metadata has an invalid build time") from exc
        if (
            parsed_built_at.tzinfo is None
            or parsed_built_at.utcoffset() != timezone.utc.utcoffset(parsed_built_at)
            or not built_at.endswith("Z")
        ):
            raise BuildProvenanceError("packaged producer metadata has an invalid build time")
    files = value["files"]
    if not isinstance(files, dict) or not files:
        raise BuildProvenanceError("packaged producer metadata has no source hashes")
    expected_files = set(package_source_files(source_root))
    if set(files) != expected_files:
        raise BuildProvenanceError("packaged producer metadata source file list disagrees")
    for relative, expected_hash in files.items():
        if not isinstance(relative, str) or not isinstance(expected_hash, str):
            raise BuildProvenanceError("packaged producer metadata has an invalid source hash")
        if not _SHA256.fullmatch(expected_hash) or _digest(source_root / relative) != expected_hash:
            raise BuildProvenanceError(f"packaged producer source hash does not match: {relative}")
    return PackagedProvenance(
        version=value["package_version"], revision=value["revision"], built_at=built_at
    )


def _verify_installed_manifest_record(path: Path) -> None:
    """Require wheel RECORD to protect the manifest bytes without self-hashing it."""
    try:
        distribution = importlib.metadata.distribution("seohead-seotools")
    except importlib.metadata.PackageNotFoundError as exc:
        raise BuildProvenanceError(
            "installed packaged producer metadata has no wheel RECORD"
        ) from exc
    member = next(
        (
            str(item)
            for item in distribution.files or ()
            if distribution.locate_file(item).resolve() == path.resolve()
        ),
        None,
    )
    record = distribution.read_text("RECORD")
    if member is None or record is None:
        raise BuildProvenanceError(
            "installed packaged producer metadata is absent from wheel RECORD"
        )
    rows = {row[0]: row[1] for row in reader(record.splitlines()) if len(row) >= 2}
    expected = rows.get(member)
    actual = urlsafe_b64encode(hashlib.sha256(path.read_bytes()).digest()).decode().rstrip("=")
    if expected != f"sha256={actual}":
        raise BuildProvenanceError(
            "installed packaged producer metadata hash does not match wheel RECORD"
        )


def packaged_provenance(source_root: Path | None = None) -> PackagedProvenance:
    """Return verified installed-package provenance, never consulting Git.

    An explicit root is for build staging and tests, where a wheel ``RECORD``
    does not exist yet. Runtime uses the default and therefore verifies both the
    manifest's source hashes and the wheel's independent metadata-file hash.
    """
    root = source_root or Path(__file__).resolve().parents[2]
    provenance = validate_manifest(root)
    if source_root is None:
        _verify_installed_manifest_record(_manifest_path(root))
    return provenance
