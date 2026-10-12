"""Source-backed comparison keeps complete rows without duplicating native payloads."""

from __future__ import annotations

import copy
import json
import os
import sqlite3
from contextlib import closing

import pytest

from seohead.sf.core.compare import compare
from seohead.sf.core.compare_store import iter_compare_rows
from seohead.storage.audit_v2 import AuditV2Error, AuditV2Reader, write_audit_v2
from tests.test_compare_bounded import _source
from tests.test_verify_fixes import A, B, _audit, _issue


def test_reader_pins_snapshot_before_validation_and_releases_on_close(tmp_path, monkeypatch):
    validate = AuditV2Reader._validate

    def check_snapshot(self, **kwargs):
        assert self.con.in_transaction
        return validate(self, **kwargs)

    monkeypatch.setattr(AuditV2Reader, "_validate", check_snapshot)
    source = _source(tmp_path, "pinned", _audit(issues=[_issue("a", "TEST", A)]))
    with closing(sqlite3.connect(source.path, timeout=0)) as writer:
        with pytest.raises(sqlite3.OperationalError, match="locked"):
            writer.execute("UPDATE items SET value_json='{}' WHERE pointer='/issues'")
            writer.commit()
        writer.rollback()
        assert source.get_item("/issues", 0)["check"] == "TEST"
        source.close()
        writer.execute("UPDATE items SET value_json='{}' WHERE pointer='/issues'")
        writer.commit()


def test_wal_update_cannot_change_validated_rows_or_same_reader_comparison(tmp_path):
    document = _audit(issues=[_issue("a", "TEST", A, details={"value": "original"})])
    original = _source(tmp_path, "wal", document)
    scan, path = original.scan_path, original.path
    original.close()
    with closing(sqlite3.connect(path)) as writer:
        assert writer.execute("PRAGMA journal_mode=WAL").fetchone()[0] == "wal"
        with AuditV2Reader(scan) as source:
            digest = source.sha256
            writer.execute(
                "UPDATE items SET value_json=replace(value_json,'original','modified') "
                "WHERE pointer='/issues'"
            )
            writer.commit()
            assert source.get_item("/issues", 0) == document["issues"][0]
            assert list(source.iter_collection("/issues")) == document["issues"]
            assert source.sha256 == digest
            result = compare(source, source, out_dir=tmp_path / "same", compression="gzip")
            assert list(iter_compare_rows(result["manifest"], "unchanged")) == [
                {"before": document["issues"][0], "after": document["issues"][0]}
            ]
            assert result["before"]["audit_sha256"] == digest
            assert result["after"]["audit_sha256"] == digest


@pytest.mark.skipif(os.name == "nt", reason="Windows can refuse replacing an open SQLite file")
def test_source_path_replacement_does_not_switch_reader_generation(tmp_path):
    document = _audit(issues=[_issue("a", "TEST", A)])
    with _source(tmp_path, "replace", document) as source:
        replacement = copy.deepcopy(document)
        replacement["issues"][0]["check"] = "CHANGED"
        collections = {f"/{name}": replacement.pop(name) for name in ("pages", "issues")}
        replacement.update(pages=[], issues=[])
        write_audit_v2(source.scan_path, replacement, collections, source.binding)
        assert source.get_item("/issues", 0) == document["issues"][0]
        with AuditV2Reader(source.scan_path) as fresh:
            assert fresh.get_item("/issues", 0)["check"] == "CHANGED"
            assert fresh.sha256 != source.sha256


@pytest.mark.parametrize("ordinal", [-1, 1, True, False, 0.0, "0", None])
def test_get_item_rejects_invalid_or_out_of_range_ordinal(tmp_path, ordinal):
    with (
        _source(tmp_path, "ordinal", _audit(issues=[_issue("a", "TEST", A)])) as source,
        pytest.raises(AuditV2Error, match="ordinal"),
    ):
        source.get_item("/issues", ordinal)


@pytest.mark.parametrize("pointer", ["/missing", None, []])
def test_get_item_rejects_unknown_collection(tmp_path, pointer):
    with (
        _source(tmp_path, "pointer", _audit()) as source,
        pytest.raises(AuditV2Error, match="collection"),
    ):
        source.get_item(pointer, 0)


def test_get_item_uses_exact_indexed_lookup_and_missing_rows_fail(tmp_path, monkeypatch):
    with _source(tmp_path, "lookup", _audit(issues=[_issue("a", "TEST", A)])) as source:
        monkeypatch.setattr(source, "iter_collection", lambda _: pytest.fail("collection scan"))
        assert source.get_item("/issues", 0)["check"] == "TEST"
        plan = source.con.execute(
            "EXPLAIN QUERY PLAN SELECT value_json FROM items WHERE pointer=? AND ordinal=?",
            ("/issues", 0),
        ).fetchall()
        assert any("SEARCH items USING INDEX" in row[3] for row in plan)
        source.collections["/issues"] = 2
        with pytest.raises(AuditV2Error, match="missing"):
            source.get_item("/issues", 1)


@pytest.mark.parametrize("compression", ["none", "gzip"])
@pytest.mark.parametrize("native_sides", [(True, True), (True, False), (False, True)])
@pytest.mark.parametrize("mapped", [False, True])
def test_native_and_mixed_sources_keep_exact_output_bytes(
    tmp_path, compression, native_sides, mapped
):
    import contextlib

    gone, new = "https://example.test/gone", "https://example.test/new"
    before = _audit(
        urls=(B, gone, A),
        issues=[
            _issue("left", "OLD", B),
            _issue(
                "same",
                "TEST",
                A,
                suppressed=True,
                details={"unicode": "日本語", "nested": [1, None]},
            ),
            _issue("gone", "TEST", gone),
            _issue("wide", "WIDE", None),
        ],
    )
    after = _audit(
        urls=(new, A, B),
        issues=[
            _issue("new", "TEST", new),
            _issue("same2", "TEST", A, details={"nested": [False, "new"]}),
            _issue("entered", "ENTERED", B),
        ],
    )
    before["pages"][2]["metrics"].update(title="Original", h1=["One", "Two"])
    after["pages"][1]["metrics"].update(title="New", canonical=A)
    declaration = None
    if mapped:
        declaration = {
            "schema_version": "url-correspondence.v1",
            "origin_map": {},
            "pairs": [{"before": A, "after": A}, {"before": gone, "after": new}],
        }
    expected = compare(
        before,
        after,
        out_dir=tmp_path / "legacy",
        compression=compression,
        correspondence=declaration,
    )
    with contextlib.ExitStack() as stack:
        sources = [
            stack.enter_context(_source(tmp_path, name, document)) if native else document
            for name, document, native in zip(
                ("before", "after"), (before, after), native_sides, strict=True
            )
        ]
        actual = compare(
            *sources,
            out_dir=tmp_path / "actual",
            compression=compression,
            correspondence=declaration,
        )
    assert actual["files"] == expected["files"]
    for field in ("summary", "conservation", "warnings", "compatibility", "measurement_gaps"):
        assert actual.get(field) == expected.get(field)
    for name in expected["files"]:
        assert list(iter_compare_rows(actual["manifest"], name)) == list(
            iter_compare_rows(expected["manifest"], name)
        )


def test_native_index_size_is_independent_of_payload_size(tmp_path, monkeypatch):
    import seohead.sf.core.compare_store as store

    export = store._export
    measurements = []

    def inspect_index(root, *args, **kwargs):
        if not measurements or measurements[-1][0] != root:
            index_path = root.parent / "index.sqlite"
            with closing(sqlite3.connect(index_path)) as con:
                for table in ("pages", "issues"):
                    assert (
                        con.execute(
                            f"SELECT count(*) FROM {table} WHERE document IS NOT NULL"
                        ).fetchone()[0]
                        == 0
                    )
                    assert con.execute(f"SELECT count(*) FROM {table}").fetchone()[0] == 48
                assert [row[1] for row in con.execute("PRAGMA table_info(delta)")] == [
                    "bucket",
                    "check_key",
                    "url",
                    "issue_rowid",
                    "segment",
                ]
                measurements.append((root, index_path.stat().st_size))
        return export(root, *args, **kwargs)

    monkeypatch.setattr(store, "_export", inspect_index)
    for size in (32, 65536):
        urls = [f"https://example.test/{i}" for i in range(24)]
        document = _audit(
            urls=urls,
            issues=[_issue(str(i), "TEST", url, details="x" * size) for i, url in enumerate(urls)],
        )
        for page in document["pages"]:
            page["opaque"] = "y" * size
        with _source(tmp_path, f"payload-{size}", document) as source:
            result = compare(
                source, source, out_dir=tmp_path / f"output-{size}", compression="gzip"
            )
        rows = list(iter_compare_rows(result["manifest"], "unchanged"))
        assert len(rows) == 24 and all(row["before"]["details"] == "x" * size for row in rows)
    assert len(measurements) == 2
    assert measurements[0][1] == measurements[1][1]


def test_native_lookup_failure_never_publishes(tmp_path, monkeypatch):
    with _source(tmp_path, "failure", _audit(issues=[_issue("a", "TEST", A)])) as source:

        def fail(*args):
            raise AuditV2Error("missing source row")

        monkeypatch.setattr(source, "get_item", fail)
        with pytest.raises(AuditV2Error, match="missing source row"):
            compare(source, source, out_dir=tmp_path / "failed")
    assert not (tmp_path / "failed").exists()
    assert not list(tmp_path.glob(".compare-*"))


def test_legacy_fallback_freezes_payload_before_export(tmp_path, monkeypatch):
    import seohead.sf.core.compare_store as store

    before = _audit(issues=[_issue("a", "TEST", A, details={"value": "original"})])
    export = store._export

    def mutate(*args, **kwargs):
        before["issues"][0]["details"]["value"] = "changed"
        return export(*args, **kwargs)

    monkeypatch.setattr(store, "_export", mutate)
    result = compare(before, before, out_dir=tmp_path / "legacy")
    row = next(iter_compare_rows(result["manifest"], "unchanged"))
    assert row["before"]["details"]["value"] == "original"
    assert row["after"]["details"]["value"] == "original"


def test_atomic_publication_failure_keeps_checkpoint_resumable(tmp_path, monkeypatch):
    import seohead.sf.core.compare_store as store

    rename = store._rename_directory_noreplace

    def fail_before_rename(_source, _destination):
        raise OSError("synthetic publish failure")

    monkeypatch.setattr(store, "_rename_directory_noreplace", fail_before_rename)
    destination = tmp_path / "failed"
    with pytest.raises(OSError, match="publish failure"):
        compare(_audit(), _audit(), out_dir=destination, compression="gzip")
    assert not destination.exists()
    assert not list(tmp_path.glob(".*.compare-publish-*"))
    from seohead.mcp.handlers import compare_status

    assert compare_status(str(destination))["resumable"] is True
    monkeypatch.setattr(store, "_rename_directory_noreplace", rename)
    resumed = compare(_audit(), _audit(), out_dir=destination, compression="gzip", resume=True)
    assert resumed["job"]["state"] == "finished"


@pytest.mark.parametrize("failure_at", [1, 2])
def test_directory_fsync_failure_rolls_back_manifest_and_members(tmp_path, monkeypatch, failure_at):
    import seohead.sf.core.compare_store as store

    fsync = store.fsync_directory
    calls = 0

    def fail(path):
        nonlocal calls
        calls += 1
        if calls == failure_at:
            raise OSError("synthetic fsync failure")
        fsync(path)

    monkeypatch.setattr(store, "fsync_directory", fail)
    with pytest.raises(OSError, match="fsync failure"):
        compare(_audit(), _audit(), out_dir=tmp_path / "failed")
    assert not (tmp_path / "failed").exists()


def test_post_rename_interruption_leaves_complete_atomic_package(tmp_path, monkeypatch):
    import seohead.sf.core.compare_store as store

    rename = store._rename_directory_noreplace
    destination = tmp_path / "published"

    def interrupt_after_rename(source, target):
        rename(source, target)
        raise KeyboardInterrupt

    monkeypatch.setattr(store, "_rename_directory_noreplace", interrupt_after_rename)
    with pytest.raises(KeyboardInterrupt):
        compare(_audit(), _audit(), out_dir=destination)
    manifest = destination / "compare.json"
    assert manifest.is_file()
    file_names = set(json.loads(manifest.read_text(encoding="utf-8"))["files"])
    assert file_names >= {"entered", "left", "appeared", "disappeared", "unchanged"}
    for name in file_names:
        list(iter_compare_rows(manifest, name))


def test_native_unexported_page_still_requires_strict_json(tmp_path, monkeypatch):
    with _source(tmp_path, "invalid", _audit()) as source:
        get_item = source.get_item

        def invalid_page(pointer, ordinal):
            row = get_item(pointer, ordinal)
            if pointer == "/pages":
                row["unexported"] = float("nan")
            return row

        monkeypatch.setattr(source, "get_item", invalid_page)
        with pytest.raises(ValueError, match="JSON"):
            compare(source, source, out_dir=tmp_path / "invalid-output")
    assert not (tmp_path / "invalid-output").exists()


def test_publication_refuses_raced_in_member_without_clobbering(tmp_path, monkeypatch):
    import seohead.sf.core.compare_store as store

    destination = tmp_path / "raced"
    link = store.os.link
    raced = []

    def race(source, target, **kwargs):
        target.write_text("concurrent file")
        raced.append(target)
        link(source, target, **kwargs)

    monkeypatch.setattr(store.os, "link", race)
    with pytest.raises(FileExistsError):
        compare(_audit(), _audit(), out_dir=destination)
    assert len(raced) == 1
    assert raced[0].read_text() == "concurrent file"
    assert not (destination / "compare.json").exists()


def test_atomic_publish_refuses_destination_raced_in_at_rename(tmp_path, monkeypatch):
    import seohead.sf.core.compare_store as store

    destination = tmp_path / "replaced"
    rename = store._rename_directory_noreplace

    def race(source, target):
        target.mkdir()
        (target / "foreign.txt").write_text("concurrent directory")
        rename(source, target)

    monkeypatch.setattr(store, "_rename_directory_noreplace", race)
    with pytest.raises(FileExistsError):
        compare(_audit(), _audit(), out_dir=destination)
    assert (destination / "foreign.txt").read_text() == "concurrent directory"
    assert not (destination / "compare.json").exists()
