"""Adversarial persisted-state validation for the native scan writer."""

from __future__ import annotations

import hashlib
import json
import sqlite3

import pytest

from seohead.crawl.settings import fingerprint
from seohead.storage import ScanError, open_scan
from seohead.storage.native_scan import NativeScan, read_effective_config
from tests.test_scan_native import _link, _metadata, _record, _runtime


def _committed_scan(tmp_path):
    path = tmp_path / "scan.sqlite"
    scan = NativeScan.create(path, **_metadata())
    scan.enqueue([("https://example.test/", 0)])
    lease = scan.claim(1)[0]
    scan.commit_page(
        lease,
        _record(),
        links=[
            _link(lease.url, "https://example.test/a", "first"),
            _link(lease.url, "https://example.test/b", "second"),
        ],
        forms=[
            {"page": lease.url, "method": "post", "action": "/one", "has_password": False},
            {"page": lease.url, "method": "get", "action": "/two", "has_password": True},
        ],
        discovered=[("https://example.test/next", 1)],
        runtime=_runtime(),
    )
    scan.close()
    return path


def test_create_refuses_a_non_resolved_config(tmp_path):
    metadata = _metadata()
    metadata["config"] = {
        "speed": {"concurrency": 1},
        "limits": {"max_query_variants_per_path": 1},
    }
    metadata["config_fingerprint"] = None

    with pytest.raises(ScanError, match="config"):
        NativeScan.create(tmp_path / "invalid.sqlite", **metadata)

    assert not (tmp_path / "invalid.sqlite").exists()


def test_create_requires_non_result_config_fields_too(tmp_path):
    metadata = _metadata()
    del metadata["config"]["output"]
    with pytest.raises(ScanError, match="complete resolved"):
        NativeScan.create(tmp_path / "invalid.sqlite", **metadata)
    assert not (tmp_path / "invalid.sqlite").exists()


def test_new_config_fields_round_trip_outside_the_frozen_header(tmp_path):
    metadata = _metadata(
        **{
            "scope.normalize_tracking_parameters": False,
            "rendering.artifacts.screenshot_mode": "first_screen",
        }
    )
    path = tmp_path / "config-extension.sqlite"
    with NativeScan.create(path, **metadata) as scan:
        assert scan.finish_without_audit("config extension fixture")

    with open_scan(path, require_audit=False) as con:
        header = con.execute("SELECT config_json,config_fingerprint FROM scan").fetchone()
        stored = json.loads(header[0])
        effective = read_effective_config(con)
        extension = con.execute(
            "SELECT payload_json FROM context_items "
            "WHERE kind='native_config_extension' AND item_key='run'"
        ).fetchone()

    assert "normalize_tracking_parameters" not in stored["scope"]
    assert "auth" not in stored["http"]
    assert "actions" not in stored["rendering"]
    assert "screenshot_mode" not in stored["rendering"]["artifacts"]
    assert header[1] == fingerprint(stored)
    assert effective["scope"]["normalize_tracking_parameters"] is False
    assert effective["rendering"]["artifacts"]["screenshot_mode"] == "first_screen"
    payload = json.loads(extension[0])
    assert payload["schema_version"] == "native_config_extension.v1"
    # The fingerprint binds the reconstructed effective config minus the probe
    # toggle, which never enters the header or the extension vocabulary.
    from seohead.storage.native_scan import _effective_config_fingerprint

    assert payload["effective_fingerprint"] == _effective_config_fingerprint(effective)


def test_legacy_config_defaults_keep_tracking_parameters_and_refuse_semantic_resume(tmp_path):
    metadata = _metadata(**{"scope.normalize_tracking_parameters": False})
    path = tmp_path / "legacy-header.sqlite"
    with NativeScan.create(path, **metadata):
        pass
    with sqlite3.connect(path) as con:
        con.execute(
            "DELETE FROM context_items WHERE kind='native_config_extension' AND item_key='run'"
        )
    before = hashlib.sha256(path.read_bytes()).hexdigest()

    with open_scan(path, require_audit=False) as con:
        effective = read_effective_config(con)
        header_fingerprint = con.execute(
            "SELECT config_fingerprint FROM scan WHERE singleton=1"
        ).fetchone()[0]
    assert effective["scope"]["normalize_tracking_parameters"] is False
    with open_scan(path, require_audit=False) as con:
        stored_header = json.loads(
            con.execute("SELECT config_json FROM scan WHERE singleton=1").fetchone()[0]
        )
    assert header_fingerprint == fingerprint(stored_header)
    assert hashlib.sha256(path.read_bytes()).hexdigest() == before

    changed = _metadata()
    with pytest.raises(ScanError, match="configuration differs"):
        NativeScan.open(path, expected_config=changed["config"])


def test_config_extension_validates_operational_screenshot_mode(tmp_path):
    path = tmp_path / "bad-config-extension.sqlite"
    with NativeScan.create(path, **_metadata()):
        pass
    with open_scan(path, require_audit=False) as con:
        effective = read_effective_config(con)
    effective["rendering"]["artifacts"]["screenshot_mode"] = "unchecked-value"
    with sqlite3.connect(path) as con:
        payload = json.loads(
            con.execute(
                "SELECT payload_json FROM context_items WHERE kind='native_config_extension'"
            ).fetchone()[0]
        )
    payload["settings"]["rendering.artifacts.screenshot_mode"] = "unchecked-value"
    from seohead.storage.native_scan import _effective_config_fingerprint

    payload["effective_fingerprint"] = _effective_config_fingerprint(effective)
    with sqlite3.connect(path) as con:
        con.execute(
            "UPDATE context_items SET payload_json=? WHERE kind='native_config_extension'",
            (json.dumps(payload, sort_keys=True, separators=(",", ":")),),
        )

    with pytest.raises(ScanError):
        NativeScan.inspect(path)


def test_config_extension_auth_is_redacted_and_bound_to_live_credentials(tmp_path, monkeypatch):
    monkeypatch.setenv("SCAN_USER", "crawl-user")
    monkeypatch.setenv("SCAN_PASSWORD", "first-password")
    metadata = _metadata(
        **{
            "cache.mode": "off",
            "http.auth_acknowledged": True,
            "http.auth": [
                {
                    "host": "example.test",
                    "scheme": "basic",
                    "username_env": "env:SCAN_USER",
                    "password_env": "env:SCAN_PASSWORD",
                }
            ],
        }
    )
    path = tmp_path / "auth-config-extension.sqlite"
    with NativeScan.create(path, **metadata):
        pass
    with sqlite3.connect(path) as con:
        stored = (
            " ".join(row[0] for row in con.execute("SELECT config_json FROM scan"))
            + " "
            + " ".join(
                row[0]
                for row in con.execute(
                    "SELECT payload_json FROM context_items WHERE kind IN "
                    "('native_config_extension','credential_context')"
                )
            )
        )
    assert "first-password" not in stored

    monkeypatch.delenv("SCAN_USER")
    monkeypatch.delenv("SCAN_PASSWORD")
    NativeScan.inspect(path)  # Read-only validation never needs credential values.

    monkeypatch.setenv("SCAN_USER", "crawl-user")
    monkeypatch.setenv("SCAN_PASSWORD", "first-password")
    with NativeScan.open(path, expected_config=metadata["config"]):
        pass
    monkeypatch.setenv("SCAN_PASSWORD", "changed-password")
    with pytest.raises(ScanError, match="credential context differs"):
        NativeScan.open(path, expected_config=metadata["config"])


def test_unsupported_writer_platform_publishes_no_file(tmp_path, monkeypatch):
    from seohead.core import filesystem

    monkeypatch.setattr(filesystem, "fcntl", None)
    monkeypatch.setattr(filesystem, "msvcrt", None)
    with pytest.raises(ScanError, match="file-lock backend"):
        NativeScan.create(tmp_path / "invalid.sqlite", **_metadata())
    assert not (tmp_path / "invalid.sqlite").exists()


def test_native_inspector_rejects_unknown_raw_context(tmp_path):
    path = _committed_scan(tmp_path)
    with sqlite3.connect(path) as con:
        con.execute(
            "INSERT INTO context_items(kind,item_key,payload_version,payload_json,completeness,reason) "
            "VALUES(?,?,?,?,?,?)",
            (
                "native_start_page",
                "run",
                "scan_context.v1",
                '{"html":"untrusted retained body"}',
                "complete",
                "raw",
            ),
        )

    with pytest.raises(ScanError, match="context"):
        NativeScan.inspect(path)


@pytest.mark.parametrize(
    ("statement", "message"),
    [
        ("UPDATE pages SET page_ordinal=99", "page ordinals"),
        ("UPDATE frontier SET depth=-1 WHERE state='queued'", "frontier depth"),
    ],
)
def test_native_inspector_rejects_invalid_page_and_frontier_order(tmp_path, statement, message):
    path = _committed_scan(tmp_path)
    with sqlite3.connect(path) as con:
        con.execute(statement)

    with pytest.raises(ScanError, match=message):
        NativeScan.inspect(path)


@pytest.mark.parametrize(
    ("statement", "message"),
    [
        ("UPDATE scan SET writer_revision='not-a-full-sha'", "writer revision"),
        ("UPDATE scan SET runtime_versions_json='{}'", "runtime provenance"),
        (
            "UPDATE scan SET retention_json="
            '\'{"policy_version":"scan_retention.v1","body_mode":"off",'
            '"max_body_bytes":0,"max_body_store_bytes":0,"min_free_bytes":0,'
            '"history_warning_bytes":0,"automatic_delete":true}\'',
            "retention",
        ),
    ],
)
def test_native_inspector_rejects_forged_provenance_and_retention(tmp_path, statement, message):
    path = _committed_scan(tmp_path)
    with sqlite3.connect(path) as con:
        con.execute(statement)

    with pytest.raises(ScanError, match=message):
        NativeScan.inspect(path)


@pytest.mark.parametrize(
    ("statement", "message"),
    [
        ("UPDATE links SET ordinal=7 WHERE ordinal=1", "link ordinals"),
        ("UPDATE forms SET ordinal=7 WHERE ordinal=1", "form ordinals"),
    ],
)
def test_native_inspector_rejects_link_and_form_ordinal_gaps(tmp_path, statement, message):
    path = _committed_scan(tmp_path)
    with sqlite3.connect(path) as con:
        con.execute(statement)

    with pytest.raises(ScanError, match=message):
        NativeScan.inspect(path)


@pytest.mark.parametrize(
    "statement",
    [
        "UPDATE scan SET corpus_partial=0",
        "UPDATE scan SET evidence_version='other.v9'",
        "UPDATE scan SET parent_scan_uuid='parent'",
    ],
)
def test_native_inspector_refuses_unsupported_capture_metadata(tmp_path, statement):
    path = _committed_scan(tmp_path)
    with sqlite3.connect(path) as con:
        con.execute(statement)
    with pytest.raises(ScanError, match="metadata"):
        NativeScan.inspect(path)


@pytest.mark.parametrize("value", ["2", "-1", "'invalid'"])
def test_native_inspector_refuses_non_boolean_pin_metadata(tmp_path, value):
    path = _committed_scan(tmp_path)
    with sqlite3.connect(path) as con:
        con.execute("PRAGMA ignore_check_constraints=ON")
        con.execute("UPDATE scan SET pinned=" + value)
    with pytest.raises(ScanError, match=r"quick_check|pin metadata|invalid scalar"):
        NativeScan.inspect(path)


def test_page_budget_refuses_oversized_record_before_serialization(tmp_path, monkeypatch):
    import seohead.storage.native_scan as native

    path = tmp_path / "scan.sqlite"
    with NativeScan.create(path, **_metadata()) as scan:
        scan.enqueue([("https://example.test/", 0)])
        lease = scan.claim(1)[0]
        record = _record()
        record["title"] = "x" * (8 * 1024 * 1024 + 1)

        def unexpected_serialization(*args, **kwargs):
            pytest.fail("oversized scalar reached JSON serialization")

        monkeypatch.setattr(native.json.JSONEncoder, "iterencode", unexpected_serialization)
        with pytest.raises(ScanError, match="byte limit"):
            scan.commit_page(lease, record, runtime=_runtime())
        assert scan.con.execute("SELECT COUNT(*) FROM pages").fetchone()[0] == 0


def test_config_extension_vocabulary_is_declared_in_one_place():
    """The writer's defaults and the reader's validator must name the same keys."""
    from seohead.storage.native_context import CONFIG_EXTENSION_PATHS
    from seohead.storage.native_scan import _CONFIG_EXTENSION_DEFAULTS

    assert set(CONFIG_EXTENSION_PATHS) == set(_CONFIG_EXTENSION_DEFAULTS)
