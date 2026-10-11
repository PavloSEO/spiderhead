"""Portable project archive contracts for #996 (first slice: local stores only, synthetic data)."""

from __future__ import annotations

import hashlib
import json
import sqlite3
import zipfile

import pytest

from seohead.projects import archive
from seohead.projects.archive import archive_project, restore_project
from seohead.projects.workspace import create_project


def _project(tmp_path):
    project = tmp_path / "src"
    create_project(project, "https://example.test/", label="Synthetic")
    with sqlite3.connect(project / "scans" / "scan.sqlite") as db:
        db.execute("create table pages(url text, status integer)")
        db.executemany(
            "insert into pages values (?, ?)",
            [("https://example.test/a", 200), ("https://example.test/b", 404)],
        )
    (project / "scans" / "api_token.txt").write_text("synthetic-value", encoding="utf-8")
    return project


def _rows(path):
    with sqlite3.connect(path) as db:
        return db.execute("select url, status from pages order by url").fetchall()


def _zip_with(tmp_path, members: dict[str, bytes], manifest_files=None):
    """Build an archive by hand so tests can break one property at a time."""
    listed = (
        manifest_files
        if manifest_files is not None
        else [
            {"path": name, "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
            for name, data in members.items()
        ]
    )
    manifest = {
        "format": archive.ARCHIVE_FORMAT,
        "archive_version": archive.ARCHIVE_VERSION,
        "project_format": "seohead.project.v1",
        "directories": ["scans", "reports"],
        "files": listed,
    }
    path = tmp_path / "crafted.zip"
    with zipfile.ZipFile(path, "w") as zf:
        zf.writestr("manifest.json", json.dumps(manifest))
        for name, data in members.items():
            zf.writestr(name, data)
    return path


def test_round_trip_keeps_scan_query_results_and_drops_credential_names(tmp_path):
    project = _project(tmp_path)
    out = tmp_path / "project.zip"
    result = archive_project(project, out)
    assert result["ok"] and out.is_file()
    assert "scans/api_token.txt" not in result["files"]
    assert {item["path"] for item in result["skipped"]} >= {"scans/api_token.txt"}

    with zipfile.ZipFile(out) as zf:
        names = zf.namelist()
    assert not [name for name in names if archive._credential_name(name)]

    restored = restore_project(out, tmp_path / "restored")
    assert restored["ok"]
    assert _rows(tmp_path / "restored" / "scans" / "scan.sqlite") == _rows(
        project / "scans" / "scan.sqlite"
    )
    assert not (tmp_path / "restored" / "scans" / "api_token.txt").exists()


def test_archive_can_omit_raw_html_bodies_and_preserve_evidence_tables(tmp_path):
    project = _project(tmp_path)
    scan = project / "scans" / "body-scan.sqlite"
    digest = hashlib.sha256(b"<html>private body</html>").hexdigest()
    with sqlite3.connect(scan) as db:
        db.executescript(
            "CREATE TABLE scan(singleton INTEGER,format_version TEXT);"
            "INSERT INTO scan VALUES(1,'scan.v1');"
            "CREATE TABLE bodies(sha256 TEXT PRIMARY KEY,data BLOB);"
            "CREATE TABLE documents(document_id INTEGER PRIMARY KEY,body_sha256 TEXT,"
            "body_state TEXT,body_reason TEXT,fidelity TEXT,source_response_id INTEGER);"
            "CREATE TABLE responses(response_id INTEGER PRIMARY KEY,purpose TEXT,"
            "body_sha256 TEXT,body_state TEXT,body_reason TEXT,content_type TEXT);"
        )
        db.execute("INSERT INTO bodies VALUES (?,?)", (digest, b"<html>private body</html>"))
        db.execute(
            "INSERT INTO documents VALUES (1,?,'complete','none','entity_bytes',1)", (digest,)
        )
        db.execute(
            "INSERT INTO responses VALUES (1,'page',?,'complete','none','text/html')", (digest,)
        )

    out = tmp_path / "without-bodies.zip"
    result = archive_project(project, out, exclude_bodies=True)
    assert result["exclude_bodies"] is True
    assert result["excluded_body_blobs"] == 1
    with zipfile.ZipFile(out) as zf:
        archived = tmp_path / "archived-scan.sqlite"
        archived.write_bytes(zf.read("scans/body-scan.sqlite"))
    with sqlite3.connect(archived) as db:
        assert db.execute("SELECT COUNT(*) FROM bodies").fetchone()[0] == 0
        assert db.execute("SELECT body_sha256,body_state,body_reason FROM documents").fetchone() == (
            None,
            "unavailable",
            "not_in_corpus",
        )
        assert db.execute("SELECT COUNT(*) FROM responses").fetchone()[0] == 1


def test_archive_can_keep_only_the_newest_n_scan_files(tmp_path):
    project = _project(tmp_path)
    for name, created in (
        ("old.sqlite", "2026-01-01T00:00:00Z"),
        ("new.sqlite", "2026-10-01T00:00:00Z"),
    ):
        with sqlite3.connect(project / "scans" / name) as db:
            db.execute("CREATE TABLE scan(singleton INTEGER,scan_uuid TEXT,created_at TEXT,finished_at TEXT)")
            db.execute("INSERT INTO scan VALUES(1,?,?,NULL)", (name, created))

    out = tmp_path / "latest.zip"
    result = archive_project(project, out, keep_newest_scans=1)

    assert "scans/old.sqlite" not in result["files"]
    assert "scans/new.sqlite" in result["files"]
    assert {item["path"] for item in result["skipped"]} >= {"scans/old.sqlite"}


def test_archive_can_omit_html_bodies_older_than_a_named_retention_window(tmp_path):
    project = _project(tmp_path)
    scan = project / "scans" / "aged.sqlite"
    digest = hashlib.sha256(b"<html>old</html>").hexdigest()
    with sqlite3.connect(scan) as db:
        db.executescript(
            "CREATE TABLE scan(singleton INTEGER,format_version TEXT);"
            "INSERT INTO scan VALUES(1,'scan.v1');"
            "CREATE TABLE bodies(sha256 TEXT PRIMARY KEY,data BLOB);"
            "CREATE TABLE documents(document_id INTEGER PRIMARY KEY,body_sha256 TEXT,"
            "body_state TEXT,body_reason TEXT,captured_at TEXT,fidelity TEXT,"
            "source_response_id INTEGER);"
            "CREATE TABLE responses(response_id INTEGER PRIMARY KEY,purpose TEXT,"
            "body_sha256 TEXT,body_state TEXT,body_reason TEXT,content_type TEXT,received_at TEXT);"
        )
        db.execute("INSERT INTO bodies VALUES (?,?)", (digest, b"<html>old</html>"))
        db.execute(
            "INSERT INTO documents VALUES (1,?,'complete','none','2000-01-01T00:00:00Z','entity_bytes',1)",
            (digest,),
        )
        db.execute(
            "INSERT INTO responses VALUES (1,'page',?,'complete','none','text/html','2000-01-01T00:00:00Z')",
            (digest,),
        )

    out = tmp_path / "aged.zip"
    result = archive_project(project, out, exclude_bodies_older_than_days=30)
    assert result["exclude_bodies_older_than_days"] == 30
    assert result["excluded_body_blobs"] == 1
    with zipfile.ZipFile(out) as zf:
        archived = tmp_path / "aged-archive.sqlite"
        archived.write_bytes(zf.read("scans/aged.sqlite"))
    with sqlite3.connect(archived) as db:
        assert db.execute("SELECT COUNT(*) FROM bodies").fetchone()[0] == 0
        assert db.execute("SELECT COUNT(*) FROM documents").fetchone()[0] == 1
        assert db.execute("SELECT body_sha256 FROM responses").fetchone()[0] is None


def test_dry_run_reports_plan_and_writes_nothing(tmp_path):
    project = _project(tmp_path)
    out = tmp_path / "planned.zip"
    plan = archive_project(project, out, dry_run=True)
    assert plan["dry_run"] is True
    assert plan["estimated_bytes"] > 0
    assert not out.exists()


def test_archive_refuses_to_overwrite_existing_output(tmp_path):
    project = _project(tmp_path)
    out = tmp_path / "exists.zip"
    out.write_bytes(b"prior")
    with pytest.raises(ValueError, match="already exists"):
        archive_project(project, out)
    assert out.read_bytes() == b"prior"


def test_restore_refuses_existing_target_and_leaves_it_untouched(tmp_path):
    project = _project(tmp_path)
    out = tmp_path / "project.zip"
    archive_project(project, out)
    target = tmp_path / "occupied"
    target.mkdir()
    (target / "keep.txt").write_text("mine", encoding="utf-8")
    with pytest.raises(ValueError, match="new path"):
        restore_project(out, target)
    assert (target / "keep.txt").read_text(encoding="utf-8") == "mine"


def test_tampered_member_fails_verification_and_publishes_nothing(tmp_path):
    project = _project(tmp_path)
    out = tmp_path / "project.zip"
    archive_project(project, out)
    with zipfile.ZipFile(out) as zf:
        members = {name: zf.read(name) for name in zf.namelist()}
    members["scans/scan.sqlite"] = members["scans/scan.sqlite"][:-1] + b"X"
    # Keep the original manifest so the tampered member no longer matches its recorded hash.
    tampered = tmp_path / "tampered.zip"
    with zipfile.ZipFile(tampered, "w") as zf:
        for name, data in members.items():
            zf.writestr(name, data)
    target = tmp_path / "bad-restore"
    with pytest.raises(ValueError, match="failed verification"):
        restore_project(tampered, target)
    assert not target.exists()
    assert not [p for p in tmp_path.iterdir() if p.name.startswith(".seohead-restore-")]


@pytest.mark.parametrize("member", ["../evil.txt", "/abs.txt", "scans/../../escape.txt"])
def test_zip_slip_members_are_refused(tmp_path, member):
    data = b"x"
    crafted = _zip_with(tmp_path, {member: data})
    with pytest.raises(ValueError, match="unsafe"):
        restore_project(crafted, tmp_path / "slip")
    assert not (tmp_path / "evil.txt").exists()


def test_symlink_member_is_refused(tmp_path):
    path = tmp_path / "link.zip"
    manifest = json.dumps(
        {
            "format": archive.ARCHIVE_FORMAT,
            "archive_version": archive.ARCHIVE_VERSION,
            "project_format": "seohead.project.v1",
            "directories": ["scans", "reports"],
            "files": [],
        }
    )
    with zipfile.ZipFile(path, "w") as zf:
        zf.writestr("manifest.json", manifest)
        info = zipfile.ZipInfo("scans/link")
        info.external_attr = (0o120777) << 16
        zf.writestr(info, "/etc/passwd")
    with pytest.raises(ValueError, match=r"symlink|unsafe|do not match"):
        restore_project(path, tmp_path / "linked")


def test_members_missing_from_manifest_are_refused(tmp_path):
    path = _zip_with(tmp_path, {"log.md": b"# log\n", "extra.txt": b"x"}, manifest_files=[])
    with pytest.raises(ValueError, match="do not match the manifest"):
        restore_project(path, tmp_path / "extra")


def test_cli_archive_and_restore_round_trip(tmp_path, capsys):
    from seohead import cli

    project = _project(tmp_path)
    out = tmp_path / "cli.zip"
    assert cli.main(["project", "archive", "--project", str(project), "--out", str(out)]) == 0
    capsys.readouterr()
    assert cli.main(["project", "restore", str(out), "--to", str(tmp_path / "cli-restored")]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["ok"] is True
    assert cli.main(["project", "restore", str(out), "--to", str(tmp_path / "cli-restored")]) == 1
    assert json.loads(capsys.readouterr().out)["ok"] is False


def test_project_archive_mcp_and_flat_cli_share_plan(tmp_path, capsys):
    from seohead.mcp import handlers
    from seohead.mcp.mcp_server import build_server

    project = _project(tmp_path)
    out = tmp_path / "planned.zip"
    assert cli.main(
        ["project-archive", "--project", str(project), "--out", str(out), "--dry-run"]
    ) == 0
    command_result = json.loads(capsys.readouterr().out)
    direct_result = handlers.project_archive(str(project), str(out), dry_run=True)
    tool = build_server()._tool_manager.get_tool("seo_project_archive")
    mcp_result = tool.fn(project=str(project), out=str(out), dry_run=True)
    assert command_result == direct_result == mcp_result
