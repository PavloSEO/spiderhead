"""Producer metadata must identify a packaged build without a checkout."""

from __future__ import annotations

import os
import shutil
from pathlib import Path
from types import SimpleNamespace

import pytest

from seohead import __version__
from seohead._build import provenance as build_provenance
from seohead._build.provenance import BuildProvenanceError, PackagedProvenance
from seohead.mcp import scan_handlers

ROOT = Path(__file__).resolve().parent.parent
REVISION = "a" * 40


def _staged_package(tmp_path: Path) -> Path:
    staged = tmp_path / "outside-checkout"
    shutil.copytree(
        ROOT / "seohead", staged / "seohead", ignore=shutil.ignore_patterns("__pycache__")
    )
    return staged


def _write_clean_manifest(root: Path) -> dict:
    manifest = build_provenance.build_manifest(root, version=__version__, revision=REVISION)
    build_provenance.write_manifest(root, manifest)
    return manifest


def _completed(returncode: int, stdout: str = "") -> SimpleNamespace:
    return SimpleNamespace(returncode=returncode, stdout=stdout, stderr="")


def test_staged_package_manifest_covers_python_sql_and_json_without_absolute_paths(tmp_path):
    staged = _staged_package(tmp_path)
    source_before = (staged / "seohead" / "storage" / "scan_v1.sql").read_bytes()
    manifest = _write_clean_manifest(staged)

    assert build_provenance.validate_manifest(staged).revision == REVISION
    assert "seohead/storage/scan_v1.sql" in manifest["files"]
    assert "seohead/data/schemaorg.json" in manifest["files"]
    assert "seohead/_build_provenance.json" not in manifest["files"]
    assert all(
        not Path(name).is_absolute() and ".." not in Path(name).parts for name in manifest["files"]
    )
    assert (staged / "seohead" / "storage" / "scan_v1.sql").read_bytes() == source_before


def test_build_manifest_records_a_reproducible_utc_timestamp(tmp_path, monkeypatch):
    staged = _staged_package(tmp_path)
    monkeypatch.setenv("SOURCE_DATE_EPOCH", "1791806400")

    manifest = build_provenance.build_manifest(staged, version=__version__, revision=REVISION)
    result = build_provenance.validate_manifest(staged, manifest)

    assert result.built_at == "2026-10-12T12:00:00Z"
    legacy = {key: value for key, value in manifest.items() if key != "built_at"}
    assert build_provenance.validate_manifest(staged, legacy).built_at is None


def test_build_manifest_rejects_invalid_reproducible_timestamp(tmp_path, monkeypatch):
    staged = _staged_package(tmp_path)
    monkeypatch.setenv("SOURCE_DATE_EPOCH", "not-an-integer")

    with pytest.raises(BuildProvenanceError, match="SOURCE_DATE_EPOCH"):
        build_provenance.build_manifest(staged, version=__version__, revision=REVISION)


@pytest.mark.parametrize("manifest_version", (99, True))
def test_staged_package_refuses_changed_sql_or_malformed_metadata(tmp_path, manifest_version):
    staged = _staged_package(tmp_path)
    _write_clean_manifest(staged)
    (staged / "seohead" / "storage" / "scan_v1.sql").write_text("changed", encoding="utf-8")

    with pytest.raises(BuildProvenanceError, match="hash does not match"):
        build_provenance.packaged_provenance(staged)

    malformed = build_provenance.build_manifest(staged, version=__version__, revision=REVISION)
    malformed["manifest_version"] = manifest_version
    build_provenance.write_manifest(staged, malformed)
    with pytest.raises(BuildProvenanceError, match="version is unsupported"):
        build_provenance.packaged_provenance(staged)


def test_staging_rejects_symlinked_directories_and_removes_stale_metadata(tmp_path):
    staged = _staged_package(tmp_path)
    _write_clean_manifest(staged)
    build_provenance.remove_manifest(staged)
    assert not (staged / "seohead" / build_provenance.MANIFEST_FILENAME).exists()

    (staged / "seohead" / "escaped").symlink_to(
        staged / "seohead" / "storage", target_is_directory=True
    )
    with pytest.raises(BuildProvenanceError, match="symlinked"):
        build_provenance.build_manifest(staged, version=__version__, revision=REVISION)


def test_staged_output_validation_refuses_a_stale_runtime_file(tmp_path):
    staged = _staged_package(tmp_path)
    expected_files = build_provenance.package_source_files(staged)
    (staged / "seohead" / "stale_extra.py").write_text("stale = True\n", encoding="utf-8")

    with pytest.raises(
        BuildProvenanceError, match=r"unexpected staged files: seohead/stale_extra\.py"
    ):
        build_provenance.validate_staged_files(staged, expected_files)


def test_manifest_write_replaces_a_staged_hardlink_without_mutating_source(tmp_path):
    source = _staged_package(tmp_path / "source")
    staged = _staged_package(tmp_path / "staged")
    source_manifest = source / "seohead" / build_provenance.MANIFEST_FILENAME
    _write_clean_manifest(source)
    source_bytes = source_manifest.read_bytes()
    staged_manifest = staged / "seohead" / build_provenance.MANIFEST_FILENAME
    os.link(source_manifest, staged_manifest)

    build_provenance.write_manifest(
        staged,
        build_provenance.build_manifest(staged, version=__version__, revision="b" * 40),
    )

    assert source_manifest.read_bytes() == source_bytes
    assert source_manifest.stat().st_ino != staged_manifest.stat().st_ino


def test_wheel_record_detects_changed_manifest_bytes(monkeypatch, tmp_path):
    staged = _staged_package(tmp_path)
    _write_clean_manifest(staged)
    manifest_path = staged / "seohead" / build_provenance.MANIFEST_FILENAME
    member = f"seohead/{build_provenance.MANIFEST_FILENAME}"
    import base64
    import hashlib

    digest = base64.urlsafe_b64encode(hashlib.sha256(manifest_path.read_bytes()).digest())
    record = f"{member},sha256={digest.decode().rstrip('=')},1\n"

    class Distribution:
        files = (member,)

        @staticmethod
        def locate_file(item):
            assert item == member
            return manifest_path

        @staticmethod
        def read_text(name):
            assert name == "RECORD"
            return record

    monkeypatch.setattr(
        build_provenance.importlib.metadata, "distribution", lambda _name: Distribution()
    )
    build_provenance._verify_installed_manifest_record(manifest_path)

    manifest_path.write_text("{}", encoding="utf-8")
    with pytest.raises(BuildProvenanceError, match="metadata hash"):
        build_provenance._verify_installed_manifest_record(manifest_path)


def test_producer_uses_packaged_metadata_only_without_a_source_checkout(monkeypatch):
    monkeypatch.setattr(scan_handlers.subprocess, "run", lambda *_args, **_kwargs: _completed(1))
    monkeypatch.setattr(
        scan_handlers,
        "packaged_provenance",
        lambda: PackagedProvenance(version=__version__, revision="b" * 40),
        raising=False,
    )

    _version, revision, _runtime = scan_handlers._producer_provenance(None)

    assert revision == "b" * 40


def test_clean_source_checkout_keeps_its_git_identity(monkeypatch):
    root = Path(scan_handlers.__file__).resolve().parents[2]
    calls = iter((_completed(0, str(root)), _completed(0), _completed(0, REVISION)))
    monkeypatch.setattr(scan_handlers.subprocess, "run", lambda *_args, **_kwargs: next(calls))
    monkeypatch.setattr(
        scan_handlers,
        "packaged_provenance",
        lambda: pytest.fail("clean checkout must keep its Git identity"),
    )

    _version, revision, _runtime = scan_handlers._producer_provenance(None)

    assert revision == REVISION


def test_packaged_metadata_version_must_match_runtime(monkeypatch):
    monkeypatch.setattr(scan_handlers.subprocess, "run", lambda *_args, **_kwargs: _completed(1))
    monkeypatch.setattr(
        scan_handlers,
        "packaged_provenance",
        lambda: PackagedProvenance(version="other", revision="b" * 40),
    )

    with pytest.raises(ValueError, match="version disagrees"):
        scan_handlers._producer_provenance(None)


def test_dirty_source_checkout_refuses_instead_of_falling_back_to_packaged_metadata(monkeypatch):
    root = Path(scan_handlers.__file__).resolve().parents[2]
    calls = iter(
        (
            _completed(0, str(root)),
            _completed(0, " M seohead/changed.py\n"),
            _completed(0, REVISION),
        )
    )
    monkeypatch.setattr(scan_handlers.subprocess, "run", lambda *_args, **_kwargs: next(calls))
    monkeypatch.setattr(
        scan_handlers,
        "packaged_provenance",
        lambda: pytest.fail("dirty checkout must not use packaged metadata"),
    )

    with pytest.raises(ValueError, match="clean source checkout"):
        scan_handlers._producer_provenance(None)


def test_unrelated_outer_git_is_never_used_as_a_producer_revision(monkeypatch, tmp_path):
    monkeypatch.setattr(
        scan_handlers.subprocess,
        "run",
        lambda *_args, **_kwargs: _completed(0, str(tmp_path / "outer-repository")),
    )
    monkeypatch.setattr(
        scan_handlers,
        "packaged_provenance",
        lambda: (_ for _ in ()).throw(BuildProvenanceError("metadata unavailable")),
    )

    with pytest.raises(ValueError, match="no verified source checkout"):
        scan_handlers._producer_provenance(None)
