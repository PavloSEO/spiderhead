"""A readable SQLite file must also satisfy the point-A evidence contract."""

from __future__ import annotations

import hashlib
import json
import sqlite3
import subprocess
import sys

import pytest

from seohead.storage import (
    _LATE_PAGE_FIELDS,
    _PAGE_NONNEGATIVE_INTS,
    APPLICATION_ID,
    ScanError,
    import_run,
    open_scan,
    read_audit,
)
from tests.test_scan_artifact import BUILD
from tests.test_scan_artifact import legacy_run as legacy_run


@pytest.fixture
def artifact(legacy_run, tmp_path):
    out = tmp_path / "scan.sqlite"
    import_run(legacy_run, out, producer_build=BUILD)
    return out


def test_original_bytes_fields_occurrences_and_producer_survive(legacy_run, artifact):
    con = open_scan(artifact)
    try:
        assert con.execute("PRAGMA application_id").fetchone()[0] == APPLICATION_ID
        assert con.execute("PRAGMA user_version").fetchone()[0] == 1
        scan = dict(con.execute("SELECT * FROM scan").fetchone())
        raw = (legacy_run / "audit.json").read_bytes()
        saved = con.execute("SELECT * FROM audit").fetchone()
        assert saved["document_json"].encode("utf-8") == raw
        assert saved["sha256"] == hashlib.sha256(raw).hexdigest()
        audit = json.loads(raw)
        assert scan["writer_revision"] == saved["analyzer_revision"] == BUILD
        assert scan["writer_version"] == audit["tool"]["version"]
        assert json.loads(scan["config_json"]) == audit["run"]["crawl_config"]
        capabilities = json.loads(scan["capabilities_json"])
        for key in ("html_bodies", "resource_bodies", "resume", "offline_reanalysis"):
            assert capabilities[key]["state"] == "unavailable"
            assert capabilities[key]["reason"]
        for table in ("bodies", "documents", "responses", "frontier", "resume_state"):
            assert con.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0] == 0
        expected = [
            json.loads(line) for line in (legacy_run / "pages.jsonl").read_text().splitlines()
        ]
        for page in expected:
            # This older fixture predates duplicate-id capture. The import
            # records that as an explicit unmeasured null rather than
            # pretending the document had no repeated ids.
            page.setdefault("duplicate_ids", None)
            # The additive JSONL measurement is retained in a typed context,
            # while the frozen page columns stay unchanged.
            page.pop("render_blocking", None)
        actual = []
        for record in con.execute(
            "SELECT p.*, u.url FROM pages p JOIN urls u USING(url_id) ORDER BY page_ordinal"
        ):
            page = dict(record)
            for key in ("url_id", "page_ordinal", "document_id"):
                page.pop(key)
            page["redirect_chain"] = json.loads(page.pop("redirect_chain_json"))
            page["hreflang"] = json.loads(page.pop("hreflang_json"))
            page["heading_outline"] = json.loads(page.pop("heading_outline_json"))
            stored_placement = page.pop("link_placement_json")
            page["link_placement"] = (
                None if stored_placement is None else json.loads(stored_placement)
            )
            stored_chain = page.pop("canonical_chain_json")
            page["canonical_chain"] = [] if stored_chain is None else json.loads(stored_chain)
            stored_invalid_targets = page.pop("invalid_link_targets_json")
            page["invalid_link_targets"] = (
                None if stored_invalid_targets is None else json.loads(stored_invalid_targets)
            )
            stored_signals = page.pop("trust_signals_json")
            page["trust_signals"] = None if stored_signals is None else json.loads(stored_signals)
            stored_duplicate_ids = page.pop("duplicate_ids_json")
            page["duplicate_ids"] = (
                None if stored_duplicate_ids is None else json.loads(stored_duplicate_ids)
            )
            for key in page:
                if key == "head_not_first" or key == "transport_upgraded" or key.endswith(
                    "_outside_head"
                ):
                    page[key] = None if page[key] is None else bool(page[key])
            actual.append(page)
        assert actual == expected
        from seohead.storage.exports import _page_rows

        restored = list(_page_rows(con))
        original = [
            json.loads(line) for line in (legacy_run / "pages.jsonl").read_text().splitlines()
        ]
        assert [page.get("render_blocking") for page in restored] == [
            page.get("render_blocking") for page in original
        ]
        expected_links = [
            json.loads(line) for line in (legacy_run / "links.jsonl").read_text().splitlines()
        ]
        links = []
        for record in con.execute(
            "SELECT l.*, s.url AS source, d.url AS destination FROM links l JOIN urls s ON s.url_id=l.source_url_id JOIN urls d ON d.url_id=l.destination_url_id ORDER BY link_id"
        ):
            links.append(
                {
                    k: record[k]
                    for k in (
                        "source",
                        "destination",
                        "anchor",
                        "anchor_source",
                        "position",
                        "target",
                        "raw_href",
                    )
                }
                | {"nofollow": bool(record["nofollow"]), "rel": json.loads(record["rel_json"])}
            )
        assert links == expected_links
    finally:
        con.close()


def test_reads_are_read_only_and_do_not_resave(artifact):
    before = artifact.read_bytes()
    con = open_scan(artifact)
    try:
        with pytest.raises(sqlite3.OperationalError, match="readonly"):
            con.execute("UPDATE pages SET title = 'changed'")
    finally:
        con.close()
    assert read_audit(artifact)["pages"]
    assert artifact.read_bytes() == before


@pytest.mark.parametrize(
    "mutation, message",
    [
        ("PRAGMA user_version=2", "user_version"),
        ("PRAGMA application_id=1", "application_id"),
        ("DROP TABLE pages", "schema differs"),
        ("CREATE VIEW surprise AS SELECT load_extension('evil')", "schema differs"),
        ("CREATE VIEW sqliteXsurprise AS SELECT 1", "schema differs"),
        ("CREATE TRIGGER surprise AFTER INSERT ON pages BEGIN SELECT 1; END", "schema differs"),
        ("UPDATE audit SET sha256=printf('%064d', 0)", "hash"),
        ("UPDATE scan SET evidence_revision=2", "revision"),
        ("UPDATE scan SET config_fingerprint='wrong'", "fingerprint"),
        ("UPDATE links SET destination_url_id=999999", "foreign-key"),
        ("DELETE FROM pages WHERE page_ordinal=1", "foreign-key|populations"),
        ("UPDATE links SET rel_json='{}'", "string tokens"),
        ("INSERT INTO bodies VALUES(printf('%064d',0),'identity',1,1,x'61')", "point-A"),
    ],
)
def test_inconsistent_or_unsupported_artifact_is_refused(artifact, mutation, message):
    with sqlite3.connect(artifact) as con:
        con.execute(mutation)
    before = artifact.read_bytes()
    with pytest.raises(ScanError, match=message):
        open_scan(artifact)
    assert artifact.read_bytes() == before


@pytest.mark.parametrize("name", ["pages.jsonl", "links.jsonl", "audit.json"])
def test_missing_input_does_not_publish_partial_file(legacy_run, tmp_path, name):
    (legacy_run / name).unlink()
    target = tmp_path / "failed.sqlite"
    with pytest.raises(ScanError):
        import_run(legacy_run, target, producer_build=BUILD)
    assert not target.exists()
    assert not list(tmp_path.glob(".scan-import-*"))


def test_append_truncated_tail_records_partial_without_altering_audit(legacy_run, tmp_path):
    with (legacy_run / "links.jsonl").open("ab") as stream:
        stream.write(b'{"source":')
    target = tmp_path / "partial.sqlite"
    import_run(legacy_run, target, producer_build=BUILD)
    con = open_scan(target)
    try:
        scan = con.execute("SELECT * FROM scan").fetchone()
        assert scan["crawl_partial"] == 1
        assert "truncated final" in scan["limitations_json"]
        assert (
            con.execute("SELECT document_json FROM audit").fetchone()[0].encode()
            == (legacy_run / "audit.json").read_bytes()
        )
    finally:
        con.close()


def test_malformed_middle_row_is_not_silently_lost(legacy_run, tmp_path):
    path = legacy_run / "links.jsonl"
    path.write_bytes(b'{"source":\n' + path.read_bytes())
    with pytest.raises(ScanError, match="invalid JSON"):
        import_run(legacy_run, tmp_path / "out.sqlite", producer_build=BUILD)


def test_existing_destination_and_symlink_are_never_overwritten(legacy_run, tmp_path):
    existing = tmp_path / "existing.sqlite"
    existing.write_bytes(b"keep me")
    link = tmp_path / "link.sqlite"
    link.symlink_to(existing)
    for target in (existing, link):
        with pytest.raises(ScanError, match="already exists"):
            import_run(legacy_run, target, producer_build=BUILD)
    assert existing.read_bytes() == b"keep me"
    assert link.is_symlink()


def test_missing_producer_or_configuration_cannot_be_inferred_from_current_checkout(
    legacy_run, tmp_path
):
    with pytest.raises(ScanError, match="original crawl"):
        import_run(legacy_run, tmp_path / "out.sqlite", producer_build="")
    path = legacy_run / "audit.json"
    data = json.loads(path.read_text())
    effective = data["run"].pop("crawl_config")
    path.write_text(json.dumps(data))
    with pytest.raises(ScanError, match="effective configuration"):
        import_run(legacy_run, tmp_path / "out.sqlite", producer_build=BUILD)
    import_run(
        legacy_run, tmp_path / "out.sqlite", producer_build=BUILD, effective_config=effective
    )
    assert "crawl_config" not in read_audit(tmp_path / "out.sqlite")["run"]


def test_explicit_configuration_must_agree(legacy_run, tmp_path):
    with pytest.raises(ScanError, match="differs"):
        import_run(
            legacy_run,
            tmp_path / "out.sqlite",
            producer_build=BUILD,
            effective_config={"different": True},
        )


def test_cli_import_inspect_and_report_are_additive(legacy_run, tmp_path):
    target = tmp_path / "cli.sqlite"
    command = [sys.executable, "-m", "seohead.storage"]
    subprocess.run(
        [*command, "import-run", str(legacy_run), "--out", str(target), "--producer-build", BUILD],
        capture_output=True,
        text=True,
        check=True,
    )
    inspected = subprocess.run(
        [*command, "inspect", str(target)], capture_output=True, text=True, check=True
    )
    assert json.loads(inspected.stdout)["scan"]["writer_revision"] == BUILD
    for source, name in ((legacy_run, "directory.md"), (target, "sqlite.md")):
        subprocess.run(
            [*command, "report", str(source), "--out", str(tmp_path / name)],
            capture_output=True,
            text=True,
            check=True,
        )
    assert (tmp_path / "directory.md").read_bytes() == (tmp_path / "sqlite.md").read_bytes()
    failed = subprocess.run(
        [*command, "inspect", str(tmp_path / "missing.sqlite")], capture_output=True, text=True
    )
    assert failed.returncode == 1
    assert "cannot read scan" in failed.stderr
    assert "Traceback" not in failed.stderr


def test_format_is_readable_without_seohead(artifact, tmp_path):
    script = "import sqlite3,sys; c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True); assert c.execute('PRAGMA user_version').fetchone()[0]==1; assert c.execute('SELECT COUNT(*) FROM pages').fetchone()[0]==2; c.close()"
    subprocess.run([sys.executable, "-I", "-c", script, str(artifact)], cwd=tmp_path, check=True)


@pytest.mark.parametrize(
    "mutation, message",
    [
        ("UPDATE scan SET lifecycle='running'", "lifecycle"),
        ("UPDATE scan SET evidence_version='crawl.v99'", "evidence version"),
        ("UPDATE scan SET corpus_partial=0", "corpus"),
        ("UPDATE pages SET page_ordinal=100 WHERE page_ordinal=0", "ordinals"),
        ("UPDATE links SET ordinal=100 WHERE link_id=1", "ordinals"),
        ("UPDATE pages SET size_bytes=-1", "negative"),
        ("UPDATE pages SET status_code='hello'", "scalar type"),
        ("UPDATE pages SET response_time=1e999", "non-finite"),
        ("UPDATE scan SET limitations_json='{}'", "string list"),
        (
            'UPDATE context_items SET payload_json=\'{"source_format":null,"inputs":"wrong","recovered_truncated_final_line":"yes","resume_eligible":false}\'',
            "provenance",
        ),
    ],
)
def test_false_metadata_types_and_order_are_refused(artifact, mutation, message):
    with sqlite3.connect(artifact) as con:
        con.execute(mutation)
    with pytest.raises(ScanError, match=message):
        open_scan(artifact)


def test_provenance_hashes_the_consumed_stream_not_a_later_replacement(
    legacy_run, tmp_path, monkeypatch
):
    import seohead.storage as storage

    source = legacy_run / "pages.jsonl"
    consumed = source.read_bytes()
    original = storage._import_links

    def replace_source_after_pages(*args, **kwargs):
        source.write_bytes(b"a later generation, not the imported records")
        return original(*args, **kwargs)

    monkeypatch.setattr(storage, "_import_links", replace_source_after_pages)
    out = tmp_path / "out.sqlite"
    import_run(legacy_run, out, producer_build=BUILD)
    con = open_scan(out)
    try:
        data = json.loads(
            con.execute(
                "SELECT payload_json FROM context_items WHERE kind='legacy_import_provenance'"
            ).fetchone()[0]
        )
        assert data["inputs"][0]["sha256"] == hashlib.sha256(consumed).hexdigest()
        assert data["inputs"][0]["bytes"] == len(consumed)
    finally:
        con.close()


def test_second_truncated_record_is_refused(legacy_run, tmp_path):
    for name in ("pages.jsonl", "links.jsonl"):
        with (legacy_run / name).open("ab") as stream:
            stream.write(b'{"url":')
    with pytest.raises(ScanError, match="only one truncated"):
        import_run(legacy_run, tmp_path / "out.sqlite", producer_build=BUILD)


def test_duplicate_json_keys_are_not_mistaken_for_truncation(legacy_run, tmp_path):
    with (legacy_run / "links.jsonl").open("ab") as stream:
        stream.write(b'{"source":"a","source":"b"}')
    with pytest.raises(ScanError, match="duplicate JSON key"):
        import_run(legacy_run, tmp_path / "out.sqlite", producer_build=BUILD)


def test_huge_integer_is_an_actionable_input_error(legacy_run, tmp_path):
    path = legacy_run / "pages.jsonl"
    pages = [json.loads(line) for line in path.read_text().splitlines()]
    pages[0]["size_bytes"] = 10**400
    path.write_text("".join(json.dumps(row) + "\n" for row in pages))
    with pytest.raises(ScanError, match="expected INTEGER"):
        import_run(legacy_run, tmp_path / "out.sqlite", producer_build=BUILD)


def test_sqlite_runtime_and_missing_module_are_named(artifact, monkeypatch):
    import seohead.storage as storage

    monkeypatch.setattr(storage.sqlite3, "sqlite_version_info", (3, 30, 0))
    with pytest.raises(ScanError, match=r"SQLite >= 3\.31"):
        open_scan(artifact)
    monkeypatch.setattr(storage, "sqlite3", None)
    with pytest.raises(ScanError, match="standard-library module"):
        open_scan(artifact)


def test_documented_stdlib_queries_and_bounded_decoder_execute(artifact, tmp_path, monkeypatch):
    import re
    import zlib
    from pathlib import Path

    documentation = (Path(__file__).resolve().parents[1] / "docs" / "STORAGE.md").read_text()
    code_blocks = re.findall(r"```python\n(.*?)```", documentation, re.S)
    monkeypatch.chdir(tmp_path)
    namespace = {}
    exec(code_blocks[0], namespace)
    sql = re.findall(r"```sql\n(.*?)```", documentation, re.S)[0]
    con = sqlite3.connect(artifact.as_uri() + "?mode=ro", uri=True)
    try:
        statement = ""
        for line in sql.splitlines():
            statement += line + "\n"
            if sqlite3.complete_statement(statement):
                assert con.execute(statement).fetchall()
                statement = ""
    finally:
        con.close()
    exec(code_blocks[1], namespace)
    body = b"future body fixture"
    digest = hashlib.sha256(body).hexdigest()
    decoder = namespace["decode_body"]
    assert decoder("zlib", zlib.compress(body), len(body), digest) == body
    assert decoder("identity", body, len(body), digest) == body
    for broken in (zlib.compress(body)[:-1], zlib.compress(body) + b"garbage"):
        with pytest.raises(ValueError):
            decoder("zlib", broken, len(body), digest)
    with pytest.raises(ValueError, match="digest"):
        decoder("identity", body, len(body), "0" * 64)


def test_schema_maps_all_current_page_and_link_fields():
    from dataclasses import fields
    from typing import get_args, get_type_hints

    from seohead.crawl.collect import PageRecord
    from seohead.crawl.spider import LinkEdge
    from seohead.storage import _expected

    page_columns = {column[1] for column in _expected()[1]["pages"]}
    mapped = {
        "url": "url_id",
        "redirect_chain": "redirect_chain_json",
        "hreflang": "hreflang_json",
        "heading_outline": "heading_outline_json",
        "invalid_link_targets": "invalid_link_targets_json",
        "link_placement": "link_placement_json",
        "canonical_chain": "canonical_chain_json",
        "trust_signals": "trust_signals_json",
        "duplicate_ids": "duplicate_ids_json",
    }
    # Transient measurements use a validated context adjunct, not frozen page columns.
    transient = {"render_blocking"}
    assert transient <= {field.name for field in fields(PageRecord)}
    assert transient.isdisjoint(page_columns)
    assert {
        mapped.get(field.name, field.name)
        for field in fields(PageRecord)
        if field.name not in transient
    } == page_columns - {
        "page_ordinal",
        "document_id",
    }
    page_record_fields = {field.name for field in fields(PageRecord)}
    assert set(_LATE_PAGE_FIELDS) <= page_record_fields
    assert len(page_record_fields - set(_LATE_PAGE_FIELDS) - transient) == 47
    assert {
        name
        for name, annotation in get_type_hints(PageRecord).items()
        if annotation is int or set(get_args(annotation)) == {int, type(None)}
    } - {"status_code"} == _PAGE_NONNEGATIVE_INTS
    assert {field.name for field in fields(LinkEdge)} == {
        "source",
        "destination",
        "anchor",
        "anchor_source",
        "nofollow",
        "position",
        "rel",
        "target",
        "raw_href",
    }


@pytest.mark.parametrize(
    "mutation",
    [
        "UPDATE scan SET start_url='https://wrong.example/'",
        "UPDATE scan SET parent_scan_uuid='00000000-0000-0000-0000-000000000000'",
        "UPDATE context_items SET reason='resume complete'",
    ],
)
def test_legacy_lineage_and_unavailable_resume_cannot_disagree(artifact, mutation):
    with sqlite3.connect(artifact) as con:
        con.execute(mutation)
    with pytest.raises(ScanError, match=r"lineage|resume unavailable"):
        open_scan(artifact)


def test_report_cli_cannot_overwrite_its_source_via_an_alias(artifact, tmp_path):
    from seohead.storage.__main__ import main

    original = artifact.read_bytes()
    alias = tmp_path / "output.md"
    alias.symlink_to(artifact)
    assert main(["report", str(artifact), "--out", str(alias)]) == 1
    assert artifact.read_bytes() == original


def test_report_cli_cannot_overwrite_source_audit(legacy_run):
    from seohead.storage.__main__ import main

    source = legacy_run / "audit.json"
    original = source.read_bytes()
    assert main(["report", str(legacy_run), "--out", str(source)]) == 1
    assert source.read_bytes() == original


def test_explicit_null_config_is_not_treated_as_an_omitted_option(legacy_run, tmp_path):
    from seohead.storage.__main__ import main

    config = tmp_path / "null.json"
    config.write_text("null")
    out = tmp_path / "out.sqlite"
    assert (
        main(
            [
                "import-run",
                str(legacy_run),
                "--out",
                str(out),
                "--producer-build",
                BUILD,
                "--config",
                str(config),
            ]
        )
        == 1
    )
    assert not out.exists()


def test_import_run_merges_partial_crawl_reason_into_pages_capability(legacy_run, tmp_path):
    """capabilities.pages.reason must mention both a partial crawl and missing legacy fields.

    Positive control: crawl_partial=true plus missing late page fields must merge both causes.
    Negative control: crawl_partial=false with the same missing fields must stay unchanged.
    """
    audit_path = legacy_run / "audit.json"
    audit = json.loads(audit_path.read_text())
    original_audit_text = audit_path.read_text()

    pages_path = legacy_run / "pages.jsonl"
    late_fields = ("content_frames", "content_frames_same_origin", "hreflang", "body_unavailable")
    stripped_pages = "\n".join(
        json.dumps({k: v for k, v in json.loads(line).items() if k not in late_fields})
        for line in pages_path.read_text().splitlines()
    )
    pages_path.write_text(stripped_pages)

    audit["run"]["crawl_partial"] = True
    audit_path.write_text(json.dumps(audit))
    partial_out = tmp_path / "partial.sqlite"
    import_run(legacy_run, partial_out, producer_build=BUILD)
    con = open_scan(partial_out)
    capabilities = json.loads(con.execute("SELECT * FROM scan").fetchone()["capabilities_json"])
    con.close()
    assert capabilities["pages"]["state"] == "partial"
    assert "legacy source is partial" in capabilities["pages"]["reason"]
    assert "legacy page fields unavailable" in capabilities["pages"]["reason"]
    assert capabilities["links"]["reason"] == "legacy source is partial"

    audit_path.write_text(original_audit_text)
    complete_out = tmp_path / "complete.sqlite"
    import_run(legacy_run, complete_out, producer_build=BUILD)
    con = open_scan(complete_out)
    capabilities = json.loads(con.execute("SELECT * FROM scan").fetchone()["capabilities_json"])
    con.close()
    assert capabilities["pages"]["state"] == "partial"
    assert capabilities["pages"]["reason"].startswith("legacy page fields unavailable:")
    assert "legacy source is partial" not in capabilities["pages"]["reason"]


def test_import_preserves_all_missing_late_page_fields_as_unavailable(legacy_run, tmp_path):
    audit_bytes = (legacy_run / "audit.json").read_bytes()
    pages_path = legacy_run / "pages.jsonl"
    pages_path.write_text(
        "".join(
            json.dumps(
                {
                    key: value
                    for key, value in json.loads(line).items()
                    if key not in _LATE_PAGE_FIELDS
                }
            )
            + "\n"
            for line in pages_path.read_text().splitlines()
        )
    )

    out = import_run(legacy_run, tmp_path / "historical.sqlite", producer_build=BUILD)
    con = open_scan(out)
    try:
        assert all(
            value is None
            for row in con.execute("SELECT " + ",".join(_LATE_PAGE_FIELDS.values()) + " FROM pages")
            for value in row
        )
        capabilities = json.loads(con.execute("SELECT capabilities_json FROM scan").fetchone()[0])
        assert capabilities["pages"]["state"] == "partial"
        for name in _LATE_PAGE_FIELDS:
            assert name in capabilities["pages"]["reason"]
        assert con.execute("SELECT document_json FROM audit").fetchone()[0].encode() == audit_bytes
    finally:
        con.close()


@pytest.mark.parametrize("field", sorted(_PAGE_NONNEGATIVE_INTS))
def test_import_rejects_negative_page_counts(legacy_run, tmp_path, field):
    pages_path = legacy_run / "pages.jsonl"
    pages = [json.loads(line) for line in pages_path.read_text().splitlines()]
    pages[0][field] = -1
    pages_path.write_text("".join(json.dumps(page) + "\n" for page in pages))

    with pytest.raises(ScanError, match=rf"pages\.{field}: expected a nonnegative integer"):
        import_run(legacy_run, tmp_path / f"negative-{field}.sqlite", producer_build=BUILD)


@pytest.mark.parametrize("field", sorted(_PAGE_NONNEGATIVE_INTS))
def test_reader_rejects_mutated_negative_page_counts(artifact, field):
    with sqlite3.connect(artifact) as con:
        con.execute(f"UPDATE pages SET {field}=-1 WHERE page_ordinal=0")
    with pytest.raises(ScanError, match="invalid negative PageRecord count"):
        open_scan(artifact)


@pytest.mark.parametrize(
    "value", [None, "[]", '["https://example.com/app.js"]', "", "https://example.com/app.js\n"]
)
def test_legacy_render_blocking_states_survive_import_and_export(legacy_run, tmp_path, value):
    from seohead.storage.exports import _page_rows, export_run

    source = legacy_run / "pages.jsonl"
    records = [json.loads(line) for line in source.read_text().splitlines()]
    for record in records:
        record.pop("render_blocking", None)
    records[0]["render_blocking"] = value
    source.write_text("".join(json.dumps(record) + "\n" for record in records))
    audit_bytes = (legacy_run / "audit.json").read_bytes()
    path = tmp_path / "measurements.sqlite"
    import_run(legacy_run, path, producer_build=BUILD)
    con = open_scan(path)
    try:
        assert "render_blocking" not in {row[1] for row in con.execute("PRAGMA table_info(pages)")}
        rows = list(_page_rows(con))
        expected = (
            None
            if value is None
            else json.loads(value)
            if value.startswith("[")
            else value.splitlines()
        )
        if expected is None:
            assert rows[0]["render_blocking"] is None
        else:
            assert json.loads(rows[0]["render_blocking"]) == expected
        assert "render_blocking" not in rows[1]
        context = con.execute(
            "SELECT * FROM context_items WHERE kind='legacy_render_blocking'"
        ).fetchone()
        assert context["completeness"] == ("unavailable" if value is None else "complete")
    finally:
        con.close()
    directory = tmp_path / "exported"
    export_run(str(path), str(directory))
    restored = [json.loads(line) for line in (directory / "pages.jsonl").read_text().splitlines()]
    assert restored[0]["render_blocking"] == rows[0]["render_blocking"]
    assert "render_blocking" not in restored[1]
    assert (directory / "audit.json").read_bytes() == audit_bytes


@pytest.mark.parametrize("value", [False, 0, [], "[1]", '{"resources":[]}'])
def test_legacy_import_rejects_invalid_render_blocking_measurement(legacy_run, tmp_path, value):
    source = legacy_run / "pages.jsonl"
    records = [json.loads(line) for line in source.read_text().splitlines()]
    records[0]["render_blocking"] = value
    source.write_text("".join(json.dumps(record) + "\n" for record in records))
    with pytest.raises(ScanError, match="render_blocking"):
        import_run(legacy_run, tmp_path / "invalid.sqlite", producer_build=BUILD)


def test_legacy_render_tolerance_keeps_unknown_field_rejection(legacy_run, tmp_path):
    source = legacy_run / "pages.jsonl"
    records = [json.loads(line) for line in source.read_text().splitlines()]
    records[0].update(render_blocking=None, unknown_measurement=None)
    source.write_text("".join(json.dumps(record) + "\n" for record in records))
    with pytest.raises(ScanError, match="fields differ"):
        import_run(legacy_run, tmp_path / "unknown.sqlite", producer_build=BUILD)


@pytest.mark.parametrize(
    "mutation",
    [
        "UPDATE context_items SET item_key='page:999999999' WHERE kind='legacy_render_blocking' "
        "AND item_key=(SELECT MIN(item_key) FROM context_items WHERE kind='legacy_render_blocking')",
        "UPDATE context_items SET payload_version='scan_context.v0' WHERE kind='legacy_render_blocking'",
        "UPDATE context_items SET kind='unknown_measurement' WHERE kind='legacy_render_blocking'",
    ],
)
def test_legacy_render_context_keeps_page_version_and_kind_guards(artifact, mutation):
    with sqlite3.connect(artifact) as con:
        con.execute(mutation)
    with pytest.raises(ScanError, match=r"render-blocking|legacy_import_provenance"):
        open_scan(artifact)
