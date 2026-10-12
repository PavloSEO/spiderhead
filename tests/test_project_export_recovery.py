"""Export recovery preserves a single published artifact and truthful receipts."""

from __future__ import annotations

import hashlib
import uuid

import pytest

from seohead.projects import exports


def _project(tmp_path, monkeypatch):
    project_uuid = str(uuid.uuid4())
    (tmp_path / "reports").mkdir(exist_ok=True)
    monkeypatch.setattr(
        exports,
        "_load",
        lambda _directory: (tmp_path, {"project_uuid": project_uuid}),
    )
    return project_uuid


def _job(project_uuid, job_id, *, state="running", result=None):
    return {
        "record_format": "seohead.project-export-job.v1",
        "job_id": job_id,
        "project_uuid": project_uuid,
        "dataset": "tasks",
        "format": "json",
        "output": "tasks.json",
        "compare_manifest": None,
        "state": state,
        "phase": "publishing" if result else "writing",
        "processed_rows": 1 if result else 0,
        "total_rows": 1,
        "history_state": "not_recorded",
        "created_at": exports._now(),
        "updated_at": exports._now(),
        "source": {"checklist_revision": 3},
        "temporary": f".{job_id}.partial",
        **({"result": result} if result else {}),
    }


def test_export_status_does_not_create_the_job_directory(tmp_path, monkeypatch):
    (tmp_path / "reports").mkdir()
    monkeypatch.setattr(
        exports,
        "_load",
        lambda _directory: (tmp_path, {"project_uuid": str(uuid.uuid4())}),
    )
    jobs = tmp_path / "reports" / ".export-jobs"

    with pytest.raises(exports.ProjectExportError, match="unavailable"):
        exports.export_status(tmp_path, str(uuid.uuid4()))

    assert not jobs.exists()


def test_published_file_remains_completed_when_history_receipt_write_fails(tmp_path, monkeypatch):
    _project(tmp_path, monkeypatch)
    monkeypatch.setattr(
        exports,
        "_rows",
        lambda *_args: ([{"task_id": "custom:one", "title": "Task"}], 1, {"checklist_revision": 3}),
    )

    def fail_history(*_args, **_kwargs):
        raise OSError("receipt storage unavailable")

    monkeypatch.setattr(exports, "record_export", fail_history)
    job_id = str(uuid.uuid4())
    result = exports.start_export(
        tmp_path,
        dataset="tasks",
        fmt="json",
        output="tasks.json",
        job_id=job_id,
        wait=True,
    )

    assert result["state"] == "completed"
    assert result["ok"] is True
    assert result["history_state"] == "failed"
    assert result["result"]["path"] == "reports/tasks.json"
    output = tmp_path / result["result"]["path"]
    assert output.is_file()
    assert hashlib.sha256(output.read_bytes()).hexdigest() == result["result"]["sha256"]
    status = exports.export_status(tmp_path, job_id)
    assert status["state"] == "completed"
    assert status["recoverable"] is True


def test_same_job_id_finishes_receipt_without_rewriting_published_output(tmp_path, monkeypatch):
    _project(tmp_path, monkeypatch)
    rows = [{"task_id": "custom:one", "title": "Task"}]
    monkeypatch.setattr(exports, "_rows", lambda *_args: (rows, 1, {"checklist_revision": 3}))
    calls = {"history": 0, "rows": 0}

    def count_rows(*_args):
        calls["rows"] += 1
        return rows, 1, {"checklist_revision": 3}

    monkeypatch.setattr(exports, "_rows", count_rows)

    def history(*args, **kwargs):
        calls["history"] += 1
        if calls["history"] == 1:
            raise OSError("receipt store unavailable")
        return {
            "id": str(uuid.uuid4()),
            "job_id": kwargs["job_id"],
            "dataset": kwargs["dataset"],
            "format": kwargs["fmt"],
            "path": kwargs["path"],
            "row_count": kwargs["row_count"],
            "byte_count": kwargs["byte_count"],
            "sha256": kwargs["sha256"],
            "source": kwargs["source"],
        }

    monkeypatch.setattr(exports, "record_export", history)
    job_id = str(uuid.uuid4())
    first = exports.start_export(
        tmp_path, dataset="tasks", fmt="json", output="tasks.json", job_id=job_id, wait=True
    )
    artifact = (tmp_path / first["result"]["path"]).read_bytes()
    assert first["state"] == "completed" and first["history_state"] == "failed"
    second = exports.start_export(
        tmp_path, dataset="tasks", fmt="json", output="tasks.json", job_id=job_id, wait=True
    )
    third = exports.start_export(
        tmp_path, dataset="tasks", fmt="json", output="tasks.json", job_id=job_id, wait=True
    )

    assert first["state"] == second["state"] == third["state"] == "completed"
    assert second["history_state"] == "recorded"
    assert calls == {"history": 2, "rows": 1}
    assert (tmp_path / first["result"]["path"]).read_bytes() == artifact


def test_interrupted_prepared_export_resumes_from_verified_temporary_file(tmp_path, monkeypatch):
    project_uuid = _project(tmp_path, monkeypatch)
    job_id = str(uuid.uuid4())
    content = b'{"rows":[{"task_id":"custom:one"}]}\n'
    result = {
        "path": "reports/tasks.json",
        "rows": 1,
        "bytes": len(content),
        "sha256": hashlib.sha256(content).hexdigest(),
    }
    job = _job(project_uuid, job_id, result=result)
    _root, temporary = exports._artifact_paths(tmp_path, job)
    temporary.write_bytes(content)
    exports._write_job(tmp_path, job)
    monkeypatch.setattr(
        exports,
        "_rows",
        lambda *_args: (_ for _ in ()).throw(AssertionError("prepared export must be resumed")),
    )
    monkeypatch.setattr(
        exports,
        "record_export",
        lambda *_args, **kwargs: {
            "id": str(uuid.uuid4()),
            "job_id": kwargs["job_id"],
            "dataset": kwargs["dataset"],
            "format": kwargs["fmt"],
            "path": kwargs["path"],
            "row_count": kwargs["row_count"],
            "byte_count": kwargs["byte_count"],
            "sha256": kwargs["sha256"],
            "source": kwargs["source"],
        },
    )

    status = exports.export_status(tmp_path, job_id)
    resumed = exports.start_export(
        tmp_path, dataset="tasks", fmt="json", output="tasks.json", job_id=job_id, wait=True
    )

    assert status["state"] == "interrupted" and status["recoverable"] is True
    assert resumed["state"] == "completed" and resumed["history_state"] == "recorded"
    assert (tmp_path / result["path"]).read_bytes() == content
    assert not temporary.exists()


def test_interrupted_unprepared_partial_file_is_cleaned_before_retry(tmp_path, monkeypatch):
    project_uuid = _project(tmp_path, monkeypatch)
    job_id = str(uuid.uuid4())
    job = _job(project_uuid, job_id, result=None)
    _root, temporary = exports._artifact_paths(tmp_path, job)
    temporary.write_bytes(b"partial")
    exports._write_job(tmp_path, job)
    monkeypatch.setattr(
        exports,
        "_rows",
        lambda *_args: ([{"task_id": "custom:one", "title": "Task"}], 1, {"checklist_revision": 4}),
    )
    monkeypatch.setattr(
        exports,
        "record_export",
        lambda *_args, **kwargs: {
            "id": str(uuid.uuid4()),
            "job_id": kwargs["job_id"],
            "dataset": kwargs["dataset"],
            "format": kwargs["fmt"],
            "path": kwargs["path"],
            "row_count": kwargs["row_count"],
            "byte_count": kwargs["byte_count"],
            "sha256": kwargs["sha256"],
            "source": kwargs["source"],
        },
    )

    result = exports.start_export(
        tmp_path, dataset="tasks", fmt="json", output="tasks.json", job_id=job_id, wait=True
    )

    assert result["state"] == "completed"
    assert not temporary.exists()
    assert (tmp_path / "reports" / "tasks.json").is_file()


def test_related_task_datasets_stream_comments_and_append_only_history(tmp_path, monkeypatch):
    from seohead.projects import coverage

    project_uuid = _project(tmp_path, monkeypatch)
    document = {
        "revision": 9,
        "items": {
            "custom:one": {
                "definitions": [
                    {"observed_at": "2026-10-01T10:00:00Z", "definition": {"title": "v1"}},
                    {"observed_at": "2026-10-02T10:00:00Z", "definition": {"title": "v2"}},
                ],
                "records": [
                    {
                        "recorded_at": "2026-10-01T11:00:00Z",
                        "revision": 4,
                        "actor": "owner",
                        "status": "succeeded",
                    }
                ],
                "comments": [
                    {
                        "created_at": "2026-10-02T11:00:00Z",
                        "actor": "reviewer",
                        "text": "Reviewed",
                        "reference": {"type": "finding", "id": "F-1"},
                    }
                ],
            }
        },
    }
    monkeypatch.setattr(
        exports, "_load", lambda _directory: (tmp_path, {"project_uuid": project_uuid})
    )
    monkeypatch.setattr(coverage, "_read", lambda _root, _project: document)

    comments, comment_count, comment_source = exports._task_related_rows(tmp_path, "task_comments")
    history, history_count, history_source = exports._task_related_rows(tmp_path, "task_history")
    comment_rows = list(comments)
    history_rows = list(history)

    assert comment_count == len(comment_rows) == 1
    assert comment_rows[0]["reference"] == {"type": "finding", "id": "F-1"}
    assert history_count == len(history_rows) == 3
    assert [row["event_type"] for row in history_rows] == ["definition", "attempt", "definition"]
    assert [row["event_ordinal"] for row in history_rows] == [1, 2, 3]
    assert history_rows[1]["entry_json"]["revision"] == 4
    assert comment_source == history_source == {"checklist_revision": 9}


def test_default_tasks_export_uses_only_stored_items_and_includes_kind_source(
    tmp_path, monkeypatch
):
    from seohead.projects import catalogue, coverage

    project_uuid = _project(tmp_path, monkeypatch)
    document = {
        "revision": 5,
        "items": {
            "check:TITLE": {
                "definition": {
                    "kind": "check",
                    "source_hash": "a" * 64,
                    "operation": "check:TITLE",
                }
            },
            "custom:owner-task": {
                "definition": {
                    "kind": "custom",
                    "source_hash": None,
                    "operation": None,
                }
            },
        },
    }
    monkeypatch.setattr(
        exports, "_load", lambda _directory: (tmp_path, {"project_uuid": project_uuid})
    )
    monkeypatch.setattr(coverage, "_read", lambda _root, _project: document)
    monkeypatch.setattr(catalogue, "load_catalogue", lambda: {})
    monkeypatch.setattr(
        coverage,
        "_status",
        lambda *_args, **_kwargs: {
            "state": "initialized",
            "revision": 5,
            "items": [
                {
                    "id": "check:TITLE",
                    "title": "Title",
                    "attempt_status": "not_run",
                    "task": {},
                    "affected_urls": {},
                },
                {
                    "id": "custom:owner-task",
                    "title": "Owner task",
                    "attempt_status": "not_run",
                    "task": {},
                    "affected_urls": {},
                },
                {
                    "id": "check:GLOBAL_ONLY",
                    "title": "Catalogue only",
                    "attempt_status": "not_run",
                    "task": {},
                    "affected_urls": {},
                },
            ],
        },
    )

    rows, revision = exports._task_rows(tmp_path)

    assert revision == 5
    assert [row["task_id"] for row in rows] == ["check:TITLE", "custom:owner-task"]
    assert rows[0]["kind"] == "check"
    assert rows[0]["source_hash"] == "a" * 64
    assert rows[0]["operation"] == "check:TITLE"
    assert rows[1]["kind"] == "custom"
