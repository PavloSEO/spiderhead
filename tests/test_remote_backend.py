"""Offline API-to-queue-to-worker and durable isolation checks for #785."""

from __future__ import annotations

import hashlib
import json
import socket
import sqlite3
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from datetime import datetime, timedelta, timezone

import httpx
import pytest
from fastapi.testclient import TestClient

from seohead.integrations.remote_api.app import TokenAuthenticator, create_app
from seohead.integrations.remote_api.backend import RemoteProjectLimits, SQLiteJobBackend
from seohead.integrations.remote_api.contracts import (
    JobConflict,
    JobNotReady,
    Principal,
    ScanSubmission,
)
from seohead.recon import net
from seohead.storage.native_scan import _frozen_header_config, read_effective_config

TOKEN_A = "synthetic-remote-token-alpha-strong-1"
TOKEN_B = "synthetic-remote-token-beta-strong-2"
SITE = "https://public.example.test/"
SCANS_A = "/api/v1/projects/alpha/scans"


class _BodyStream(httpx.SyncByteStream):
    def __init__(self, body: bytes):
        self.body = body

    def __iter__(self):
        yield self.body


def _network(monkeypatch):
    requests = []

    def resolve(host, port, *, type):
        assert host == "public.example.test"
        return [(socket.AF_INET, socket.SOCK_STREAM, 6, "", ("93.184.216.34", port))]

    def fetch(_self, request):
        requests.append(request)
        if request.url.path == "/robots.txt":
            return httpx.Response(
                200,
                text="User-agent: *\nAllow: /\n",
                headers={"content-type": "text/plain"},
                request=request,
            )
        return httpx.Response(
            200,
            stream=_BodyStream(
                b"<html><head><title>Test</title></head><body><main>Example text</main></body></html>"
            ),
            headers={"content-type": "text/html; charset=utf-8"},
            request=request,
        )

    monkeypatch.setattr(net.socket, "getaddrinfo", resolve)
    monkeypatch.setattr(httpx.HTTPTransport, "handle_request", fetch)
    return requests


def _backend(tmp_path, **kwargs):
    return SQLiteJobBackend(
        tmp_path / "remote-state",
        {"alpha": RemoteProjectLimits(), "beta": RemoteProjectLimits()},
        producer_build="a" * 40,
        **kwargs,
    )


def _api(backend):
    all_permissions = frozenset(
        {"scan:submit", "scan:list", "scan:read", "scan:cancel", "scan:result"}
    )
    authenticator = TokenAuthenticator(
        {
            TokenAuthenticator.digest(TOKEN_A): Principal("operator-a", {"alpha": all_permissions}),
            TokenAuthenticator.digest(TOKEN_B): Principal("operator-b", {"beta": all_permissions}),
        }
    )
    return TestClient(create_app(backend, authenticator, target_policy=backend))


def _headers(token=TOKEN_A, key="synthetic-remote-key-123"):
    return {"Authorization": f"Bearer {token}", "Idempotency-Key": key}


@pytest.mark.parametrize("value", [True, 1.5, float("nan"), float("inf"), 0])
def test_backend_requires_exact_positive_global_worker_cap(tmp_path, value):
    with pytest.raises(ValueError, match="positive integer"):
        _backend(tmp_path, max_active_jobs=value)


@pytest.mark.parametrize("value", [True, "3", float("nan"), float("inf"), 2.9])
def test_backend_requires_finite_lease(tmp_path, value):
    with pytest.raises(ValueError, match="worker lease"):
        _backend(tmp_path, lease_seconds=value)


@pytest.mark.parametrize("failure", [None, "deadline", "render"])
def test_render_coverage_survives_worker_restart_and_authenticated_downloads(
    monkeypatch, tmp_path, failure
):
    from seohead.checks import render
    from seohead.crawl import render_escalation, sqlite_render

    _network(monkeypatch)
    clock = [0.0]
    calls = []
    escalate = render_escalation.escalate

    def controlled_escalation(*args, **kwargs):
        return escalate(*args, **kwargs, clock=lambda: clock[0])

    def document(target, *_args, **_kwargs):
        calls.append(target)
        if failure == "deadline":
            clock[0] = 61.0
        if failure == "render" and len(calls) == 2:
            return {"ok": False, "error": "synthetic private render error"}
        renderer = sqlite_render._unknown_renderer(
            target, ScanSubmission(target_url=SITE).options.effective_config()
        )
        renderer.update(engine="playwright-chromium", engine_version="synthetic")
        return {
            "ok": True,
            "url": target,
            "final_url": target,
            "renderer": renderer,
            "html": "<html><head><title>Rendered proof</title></head>"
            "<body><h1>Rendered proof</h1><main>Rendered useful content.</main></body></html>",
        }

    monkeypatch.setattr(render_escalation, "escalate", controlled_escalation)
    monkeypatch.setattr(render, "render_document", document)
    backend = _backend(tmp_path)
    api = _api(backend)
    sent = api.post(
        SCANS_A,
        headers=_headers(),
        json={
            "target_url": SITE,
            "options": {
                "max_urls": 1,
                "max_requests": 20,
                "max_crawl_seconds": 60,
                "rendering_mode": "js",
            },
        },
    )
    assert sent.status_code == 202, sent.text
    job_id = sent.json()["job_id"]
    finished = backend.run_one("render-worker")
    reason = {
        None: "",
        "deadline": "render_time_budget_exhausted",
        "render": "rendered_evidence_incomplete",
    }[failure]
    assert finished.state == ("partial" if failure else "finished")
    assert finished.finish_reason == (reason or "finished")
    result = backend.get_result("alpha", job_id)
    assert result.coverage == ("partial" if failure else "complete")
    assert result.audit_available
    assert not result.evidence.source.crawl_partial
    scan = backend.root / "projects" / "alpha" / job_id / "scan.sqlite"
    assert scan.with_suffix(".sqlite.audit-v2.sqlite").is_file()
    with closing(sqlite3.connect(scan)) as con:
        assert con.execute("SELECT representation FROM pages").fetchone()[0] == (
            "static" if failure else "rendered"
        )
        config = json.loads(con.execute("SELECT config_json FROM scan").fetchone()[0])
    assert config["rendering"]["escalation"]["max_render_seconds"] == 60

    # Old releases persisted this incomplete render as finished. Correct its
    # read projection, without rewriting historical job or capture evidence.
    with backend._db(write=True) as con:
        con.execute(
            "UPDATE jobs SET state='finished',finish_reason='finished',audit_reason='' WHERE job_id=?",
            (job_id,),
        )
    before = {
        path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in scan.parent.iterdir()
    }
    restarted = _backend(tmp_path)
    api = _api(restarted)
    response = api.get(f"{SCANS_A}/{job_id}/result", headers=_headers())
    assert response.status_code == 200
    saved = response.json()
    assert saved["job"]["state"] == "finished"
    assert saved["coverage"] == ("partial" if failure else "complete")
    assert saved["audit_reason"] == reason
    assert SITE not in response.text
    for artifact in saved["artifacts"]:
        route = f"{SCANS_A}/{job_id}/artifacts/{artifact['artifact_id']}"
        assert api.get(route).status_code == 401
        downloaded = api.get(route, headers=_headers())
        assert downloaded.status_code == 200
        path = restarted.artifact_path("alpha", job_id, artifact["artifact_id"])
        assert hashlib.sha256(downloaded.content).hexdigest() == before[path.name]
    assert before == {
        path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in scan.parent.iterdir()
    }
    with restarted._db() as con:
        assert tuple(
            con.execute(
                "SELECT state,finish_reason,audit_reason FROM jobs WHERE job_id=?", (job_id,)
            ).fetchone()
        ) == ("finished", "finished", "")
    for state, coverage in (("failed", "failed"), ("cancelled", "partial")):
        with restarted._db(write=True) as con:
            con.execute("UPDATE jobs SET state=? WHERE job_id=?", (state, job_id))
        assert restarted.get_result("alpha", job_id).coverage == coverage


@pytest.mark.parametrize(
    "changes,reason",
    [
        ({"mode": "raw"}, "render_evidence_unavailable"),
        ({"time_budget_exhausted": "false"}, "render_evidence_unavailable"),
        ({"render_budget_exhausted": True}, "render_url_budget_exhausted"),
        ({"patterns_partially_rendered": [SITE]}, "render_url_budget_exhausted"),
        ({"patterns_unprobed": [SITE]}, "render_probe_unavailable"),
        (None, "render_evidence_unavailable"),
        ({"patterns_sampled": None}, "render_evidence_unavailable"),
    ],
)
def test_render_coverage_requires_valid_requested_measurements(tmp_path, changes, reason):
    from dataclasses import asdict

    from seohead.crawl.render_escalation import EscalationResult
    from seohead.integrations.remote_api.backend import _render_coverage_reason

    summary = asdict(EscalationResult(mode="js"))
    if changes is not None:
        summary.update(changes)
    else:
        summary.pop("time_budget_exhausted")
    path = tmp_path / "audit.json"
    path.write_text(json.dumps({"run": {"render_escalation": summary}}))
    assert _render_coverage_reason(path, "js") == reason
    assert _render_coverage_reason(path, "raw") == ""
    path.write_text('{"run":{"render_escalation":null}}')
    assert _render_coverage_reason(path, "js") == "render_evidence_unavailable"


def test_synthetic_api_queue_worker_result_and_private_artifacts(monkeypatch, tmp_path):
    requests = _network(monkeypatch)
    backend = _backend(tmp_path)
    api = _api(backend)
    schema = api.get("/api/v1/openapi.json", headers=_headers()).json()
    assert "/api/v1/projects/{project_id}/scans/{job_id}/artifacts/{artifact_id}" in schema["paths"]
    sent = api.post(
        SCANS_A,
        headers=_headers(),
        json={"target_url": SITE, "options": {"max_urls": 1, "max_requests": 20}},
    )
    assert sent.status_code == 202, sent.text
    job_id = sent.json()["job_id"]
    assert sent.json()["state"] == "queued"
    assert api.get(f"{SCANS_A}/{job_id}/result", headers=_headers()).status_code == 409
    finished = backend.run_one("worker-a")
    assert finished is not None and finished.job_id == job_id
    assert finished.state == "finished"
    result_response = api.get(f"{SCANS_A}/{job_id}/result", headers=_headers())
    assert result_response.status_code == 200, result_response.text
    result = result_response.json()
    assert result["audit_available"]
    assert result["coverage"] == "complete"
    assert result["evidence"]["source"]["source_kind"] == "native"
    assert {item["kind"] for item in result["artifacts"]} >= {
        "scan",
        "audit_json",
        "audit_md",
    }
    scan_ref = next(item for item in result["artifacts"] if item["kind"] == "scan")
    path = backend.artifact_path("alpha", job_id, scan_ref["artifact_id"])
    assert path is not None and path.name == "scan.sqlite"
    with closing(sqlite3.connect(path)) as scan_db:
        scan_db.row_factory = sqlite3.Row
        stored_config = scan_db.execute(
            "SELECT config_json FROM scan WHERE singleton=1"
        ).fetchone()[0]
        effective_config = read_effective_config(scan_db)
        assert (
            effective_config
            == ScanSubmission(
                target_url=SITE, options={"max_urls": 1, "max_requests": 20}
            ).options.effective_config()
        )
    frozen_config = json.loads(stored_config)
    assert "asset_thresholds" not in frozen_config["analysis"]
    assert frozen_config == _frozen_header_config(effective_config)
    download = api.get(
        f"{SCANS_A}/{job_id}/artifacts/{scan_ref['artifact_id']}", headers=_headers()
    )
    assert download.status_code == 200 and download.content.startswith(b"SQLite format 3")
    assert (
        api.get(
            f"/api/v1/projects/beta/scans/{job_id}/artifacts/{scan_ref['artifact_id']}",
            headers=_headers(TOKEN_B),
        ).status_code
        == 404
    )
    assert backend.artifact_path("beta", job_id, scan_ref["artifact_id"]) is None
    assert all(request.url.host == "93.184.216.34" for request in requests)
    assert "public.example.test" not in json.dumps(backend.events("alpha", job_id))
    assert backend.events("beta", job_id) == []
    md_ref = next(item for item in result["artifacts"] if item["kind"] == "audit_md")
    md_path = backend.artifact_path("alpha", job_id, md_ref["artifact_id"])
    assert md_path is not None
    md_path.unlink()
    md_path.symlink_to(path)
    assert backend.artifact_path("alpha", job_id, md_ref["artifact_id"]) is None
    assert (
        api.get(
            f"{SCANS_A}/{job_id}/artifacts/{md_ref['artifact_id']}", headers=_headers()
        ).status_code
        == 404
    )
    degraded = backend.get_result("alpha", job_id)
    assert degraded is not None and degraded.coverage == "partial"
    assert degraded.audit_reason == "required report artifact unavailable"


def test_disposable_queue_restore_preserves_api_bytes_and_runs_a_queued_job_once(
    monkeypatch, tmp_path
):
    from scripts.disposable_worker_recovery import verify_queue_restore

    requests = _network(monkeypatch)
    backend = _backend(tmp_path)
    api = _api(backend)
    body = {"target_url": SITE, "options": {"max_urls": 1, "max_requests": 20}}
    assert api.post(SCANS_A, headers=_headers(), json=body).status_code == 202
    assert backend.run_one("before-backup").state == "finished"
    queued = api.post(SCANS_A, headers=_headers(key="queued-before-backup"), json=body).json()
    authenticator = TokenAuthenticator(
        {
            TokenAuthenticator.digest(TOKEN_A): Principal(
                "operator-a", {"alpha": frozenset({"scan:submit", "scan:read", "scan:result"})}
            )
        }
    )
    before_requests = len(requests)
    restored = tmp_path / "restored"
    proof = verify_queue_restore(backend, tmp_path / "backup", restored, authenticator, TOKEN_A)
    assert proof == {
        "jobs": 2,
        "artifacts": 3,
        "queued_jobs": 1,
        "authenticated_bytes_unchanged": True,
        "project_authorization": True,
        "idempotency_preserved": True,
    }
    assert len(requests) == before_requests
    recovered = SQLiteJobBackend(restored, backend.projects, producer_build="a" * 40)
    completed = recovered.run_one("after-restore")
    assert completed.job_id == queued["job_id"] and completed.state == "finished"
    assert recovered.run_one("after-restore-again") is None
    assert backend.get_job("alpha", queued["job_id"]).state == "queued"
    replay = _api(recovered).post(SCANS_A, headers=_headers(key="queued-before-backup"), json=body)
    assert replay.status_code == 200 and replay.json()["job_id"] == queued["job_id"]
    assert TOKEN_A not in json.dumps(proof) and SITE not in json.dumps(proof)


def test_trusted_remote_browser_and_credential_references_are_not_submission_fields(
    monkeypatch, tmp_path
):
    _network(monkeypatch)
    secret = "Bearer synthetic-value-that-must-not-be-stored"
    monkeypatch.setenv("SEOHEAD_REMOTE_TEST_TOKEN", secret)
    seen = []

    def runner(_url, **kwargs):
        seen.append(kwargs["settings"])
        return {"audit_available": False, "partial": True}

    limits = RemoteProjectLimits(
        browser_transport={
            "transport": "remote",
            "remote_protocol": "playwright",
            "remote_endpoint_env": "SEOHEAD_REMOTE_BROWSER_ENDPOINT",
            "remote_playwright_version": "1.57.0",
        },
        credential_headers=(
            {
                "host": "public.example.test",
                "headers": {"Authorization": "env:SEOHEAD_REMOTE_TEST_TOKEN"},
            },
        ),
    )
    backend = SQLiteJobBackend(
        tmp_path / "remote-state",
        {"alpha": limits},
        producer_build="a" * 40,
        runner=runner,
    )
    request = ScanSubmission(
        target_url=SITE,
        options={"max_urls": 1, "max_requests": 20, "rendering_mode": "js"},
    )
    outcome = backend.submit(
        "alpha",
        "operator-a",
        "synthetic-remote-settings",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    )
    with closing(sqlite3.connect(backend.db_path)) as con:
        stored = con.execute(
            "SELECT config_json FROM jobs WHERE job_id=?", (outcome.job.job_id,)
        ).fetchone()[0]
    assert secret not in stored
    assert json.loads(stored)["rendering"]["browser"]["transport"] == "remote"
    assert json.loads(stored)["http"]["credential_headers"][0]["headers"] == {
        "Authorization": "env:SEOHEAD_REMOTE_TEST_TOKEN"
    }
    assert backend.run_one("worker-a").state == "failed"
    assert seen and seen[0]["rendering"]["browser"]["transport"] == "remote"
    assert secret not in json.dumps(backend.events("alpha", outcome.job.job_id))


def test_missing_trusted_credential_reference_refuses_before_queueing(monkeypatch, tmp_path):
    monkeypatch.delenv("SEOHEAD_MISSING_REMOTE_TOKEN", raising=False)
    backend = SQLiteJobBackend(
        tmp_path / "remote-state",
        {
            "alpha": RemoteProjectLimits(
                credential_headers=(
                    {
                        "host": "public.example.test",
                        "headers": {"Authorization": "env:SEOHEAD_MISSING_REMOTE_TOKEN"},
                    },
                )
            )
        },
        producer_build="a" * 40,
    )
    request = ScanSubmission(target_url=SITE, options={"max_urls": 1, "max_requests": 20})
    with pytest.raises(ValueError, match="SEOHEAD_MISSING_REMOTE_TOKEN"):
        backend.submit(
            "alpha",
            "operator-a",
            "synthetic-missing-reference",
            request.fingerprint(),
            request,
            request.options.effective_config(),
        )


def test_trusted_credential_rotation_and_revocation_are_rechecked_before_worker(
    monkeypatch, tmp_path
):
    _network(monkeypatch)
    reference = "SEOHEAD_REMOTE_ROTATION_TOKEN"
    monkeypatch.setenv(reference, "Bearer before-rotation")
    seen = []

    def runner(_url, *, settings, **_kwargs):
        from seohead.crawl.settings import resolve_credential_headers

        seen.append(
            resolve_credential_headers(
                settings["http"]["credential_headers"], "public.example.test"
            )
        )
        return {"audit_available": False, "partial": True}

    limits = RemoteProjectLimits(
        credential_headers=(
            {
                "host": "public.example.test",
                "headers": {"Authorization": f"env:{reference}"},
            },
        )
    )
    backend = SQLiteJobBackend(
        tmp_path / "remote-state", {"alpha": limits}, producer_build="a" * 40, runner=runner
    )
    request = ScanSubmission(target_url=SITE, options={"max_urls": 1, "max_requests": 20})
    first = backend.submit(
        "alpha",
        "operator-a",
        "rotated",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    ).job
    monkeypatch.setenv(reference, "Bearer after-rotation")
    assert backend.run_one("worker-a").state == "failed"  # fake runner writes no scan
    assert seen == [{"Authorization": "Bearer after-rotation"}]

    second = backend.submit(
        "alpha",
        "operator-a",
        "revoked",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    ).job
    monkeypatch.delenv(reference)
    assert backend.run_one("worker-b").state == "failed"
    assert seen == [{"Authorization": "Bearer after-rotation"}]
    assert backend.get_job("alpha", first.job_id).state == "failed"
    assert backend.get_job("alpha", second.job_id).finish_reason == "worker_failure"


def test_trusted_credential_reference_reaches_runtime_without_leaking_provenance(
    monkeypatch, tmp_path
):
    requests = _network(monkeypatch)
    secret = "Bearer synthetic-runtime-secret"
    monkeypatch.setenv("SEOHEAD_REMOTE_RUNTIME_TOKEN", secret)
    backend = SQLiteJobBackend(
        tmp_path / "remote-state",
        {
            "alpha": RemoteProjectLimits(
                credential_headers=(
                    {
                        "host": "public.example.test",
                        "headers": {"Authorization": "env:SEOHEAD_REMOTE_RUNTIME_TOKEN"},
                    },
                )
            )
        },
        producer_build="a" * 40,
    )
    request = ScanSubmission(target_url=SITE, options={"max_urls": 1, "max_requests": 20})
    job = backend.submit(
        "alpha",
        "operator-a",
        "synthetic-runtime-reference",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    ).job
    assert backend.run_one("worker-a").state == "finished"
    assert any(outbound.headers.get("authorization") == secret for outbound in requests)
    scan_ref = next(
        item for item in backend.get_result("alpha", job.job_id).artifacts if item.kind == "scan"
    )
    scan_path = backend.artifact_path("alpha", job.job_id, scan_ref.artifact_id)
    with closing(sqlite3.connect(scan_path)) as con:
        stored = con.execute("SELECT config_json FROM scan WHERE singleton=1").fetchone()[0]
    assert secret not in stored
    assert "REDACTED" in stored
    assert secret not in json.dumps(backend.events("alpha", job.job_id))


@pytest.mark.parametrize("kind", ["scan", "audit_json", "audit_md"])
def test_same_size_artifact_tampering_denies_download_and_complete_coverage(
    monkeypatch, tmp_path, kind
):
    _network(monkeypatch)
    backend = _backend(tmp_path)
    api = _api(backend)
    sent = api.post(
        SCANS_A,
        headers=_headers(),
        json={"target_url": SITE, "options": {"max_urls": 1, "max_requests": 20}},
    )
    assert sent.status_code == 202
    job_id = sent.json()["job_id"]
    assert backend.run_one("worker-a").state == "finished"
    result = backend.get_result("alpha", job_id)
    ref = next(item for item in result.artifacts if item.kind == kind)
    path = backend.artifact_path("alpha", job_id, ref.artifact_id)
    assert path is not None
    original = path.read_bytes()
    path.write_bytes(bytes([original[0] ^ 1]) + original[1:])
    assert path.stat().st_size == len(original)
    assert backend.artifact_path("alpha", job_id, ref.artifact_id) is None
    assert (
        api.get(f"{SCANS_A}/{job_id}/artifacts/{ref.artifact_id}", headers=_headers()).status_code
        == 404
    )
    assert backend.get_result("alpha", job_id).coverage != "complete"


def test_open_artifact_stream_survives_concurrent_retention_unlink(monkeypatch, tmp_path):
    _network(monkeypatch)
    backend = _backend(tmp_path)
    api = _api(backend)
    sent = api.post(
        SCANS_A,
        headers=_headers(),
        json={"target_url": SITE, "options": {"max_urls": 1, "max_requests": 20}},
    )
    job_id = sent.json()["job_id"]
    assert backend.run_one("worker-a").state == "finished"
    scan_ref = next(
        item for item in backend.get_result("alpha", job_id).artifacts if item.kind == "scan"
    )
    original_open = backend.open_artifact

    def open_then_prune(project_id, requested_job_id, artifact_id):
        opened = original_open(project_id, requested_job_id, artifact_id)
        backend.prune_terminal(
            project_id,
            before=datetime.now(timezone.utc) + timedelta(days=1),
            confirm=True,
        )
        return opened

    monkeypatch.setattr(backend, "open_artifact", open_then_prune)
    delivered = api.get(f"{SCANS_A}/{job_id}/artifacts/{scan_ref.artifact_id}", headers=_headers())
    assert delivered.status_code == 200
    assert delivered.content.startswith(b"SQLite format 3")
    assert backend.get_job("alpha", job_id) is None


def test_idempotency_persists_across_backend_restart_and_is_project_scoped(monkeypatch, tmp_path):
    _network(monkeypatch)
    backend = _backend(tmp_path)
    api = _api(backend)
    payload = {"target_url": SITE, "options": {"max_requests": 20}}
    first = api.post(SCANS_A, headers=_headers(), json=payload)
    assert first.status_code == 202
    restarted = _backend(tmp_path)
    api2 = _api(restarted)
    replay = api2.post(SCANS_A, headers=_headers(), json=payload)
    assert replay.status_code == 200
    assert replay.json()["job_id"] == first.json()["job_id"]
    conflict = api2.post(
        SCANS_A,
        headers=_headers(),
        json={"target_url": SITE, "options": {"max_urls": 1, "max_requests": 21}},
    )
    assert conflict.status_code == 409
    assert (
        api2.get(f"{SCANS_A}/{first.json()['job_id']}", headers=_headers(TOKEN_B)).status_code
        == 404
    )
    assert (
        api2.get(
            f"/api/v1/projects/beta/scans/{first.json()['job_id']}", headers=_headers(TOKEN_B)
        ).status_code
        == 404
    )
    assert restarted.list_jobs("beta", 0, 10) == []


def test_remote_api_rejects_private_target_despite_local_opt_in(monkeypatch, tmp_path):
    monkeypatch.setenv(net.PRIVATE_NETWORK_ENV, "1")
    backend = _backend(tmp_path)
    api = _api(backend)
    denied = api.post(
        SCANS_A,
        headers=_headers(),
        json={"target_url": "http://127.0.0.1/", "options": {"max_requests": 20}},
    )
    assert denied.status_code == 403
    assert denied.json()["error"]["code"] == "target_denied"
    assert backend.list_jobs("alpha", 0, 10) == []


def test_api_reports_project_budget_and_queue_capacity_without_500(monkeypatch, tmp_path):
    _network(monkeypatch)
    backend = SQLiteJobBackend(
        tmp_path / "remote-state",
        {"alpha": RemoteProjectLimits(max_queued_jobs=1, max_requests=20)},
        producer_build="a" * 40,
    )
    api = _api(backend)
    budget = api.post(
        SCANS_A,
        headers=_headers(),
        json={"target_url": SITE, "options": {"max_requests": 21}},
    )
    assert budget.status_code == 422
    assert budget.json()["error"]["code"] == "budget_exceeded"
    first = api.post(
        SCANS_A,
        headers=_headers(),
        json={"target_url": SITE, "options": {"max_urls": 1, "max_requests": 20}},
    )
    assert first.status_code == 202
    full = api.post(
        SCANS_A,
        headers=_headers(key="another-queue-key"),
        json={"target_url": SITE, "options": {"max_urls": 1, "max_requests": 20}},
    )
    assert full.status_code == 503
    assert full.json()["error"]["code"] == "queue_full"
    assert len(backend.list_jobs("alpha", 0, 10)) == 1


def test_secret_bearing_target_query_stays_out_of_events_and_api_status(monkeypatch, tmp_path):
    _network(monkeypatch)
    backend = _backend(tmp_path)
    api = _api(backend)
    secret_url = SITE + "?token=synthetic-secret"
    sent = api.post(
        SCANS_A,
        headers=_headers(),
        json={"target_url": secret_url, "options": {"max_urls": 1, "max_requests": 20}},
    )
    assert sent.status_code == 202
    job_id = sent.json()["job_id"]
    assert "synthetic-secret" not in sent.text
    backend.run_one("worker-a")
    status = api.get(f"{SCANS_A}/{job_id}", headers=_headers())
    result = api.get(f"{SCANS_A}/{job_id}/result", headers=_headers())
    assert status.status_code == result.status_code == 200
    assert "synthetic-secret" not in status.text + result.text
    assert "synthetic-secret" not in json.dumps(backend.events("alpha", job_id))


def test_queued_cancellation_and_result_do_not_start_worker(monkeypatch, tmp_path):
    requests = _network(monkeypatch)
    backend = _backend(tmp_path)
    request = ScanSubmission(target_url=SITE)
    job = backend.submit(
        "alpha",
        "operator",
        "cancel-key",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    ).job
    cancelled = backend.cancel_job("alpha", job.job_id)
    assert cancelled is not None and cancelled.state == "cancelled"
    assert backend.run_one("worker-a") is None
    result = backend.get_result("alpha", job.job_id)
    assert result is not None and result.coverage == "skipped" and not result.audit_available
    assert requests == []


def test_running_cancellation_is_observed_at_progress_boundary(monkeypatch, tmp_path):
    _network(monkeypatch)
    holder = {}

    def runner(_target, *, scan_out, settings, producer_build, progress):
        del scan_out, settings, producer_build
        job = holder["job"]
        holder["backend"].cancel_job("alpha", job.job_id)
        progress(1, 0)
        pytest.fail("cancelled worker continued")

    backend = _backend(tmp_path, runner=runner)
    request = ScanSubmission(target_url=SITE)
    job = backend.submit(
        "alpha",
        "operator",
        "running-cancel",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    ).job
    holder.update(backend=backend, job=job)
    ended = backend.run_one("worker-a")
    assert ended is not None and ended.state == "cancelled"
    assert ended.finish_reason == "cancelled_by_operator"


def test_crash_lease_recovery_records_failure_and_preserves_next_queued_job(monkeypatch, tmp_path):
    _network(monkeypatch)
    clock = [1_700_000_000.0]

    def crash(_target, *, scan_out, settings, producer_build, progress):
        del scan_out, settings, producer_build, progress
        raise SystemExit("synthetic worker crash")

    backend = _backend(tmp_path, now=lambda: clock[0], lease_seconds=3, runner=crash)
    request = ScanSubmission(target_url=SITE)
    first = backend.submit(
        "alpha",
        "operator",
        "first-crash",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    ).job
    second = backend.submit(
        "alpha",
        "operator",
        "second-queue",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    ).job
    with pytest.raises(SystemExit):
        backend.run_one("worker-a")
    assert backend.get_job("alpha", first.job_id).state == "running"
    clock[0] += 4
    restarted = _backend(tmp_path, now=lambda: clock[0], lease_seconds=3, runner=crash)
    assert restarted.recover_expired() == 0  # startup already recovered the expired lease
    failed = restarted.get_job("alpha", first.job_id)
    assert failed is not None and failed.state == "failed"
    assert failed.finish_reason == "worker_lease_expired"
    assert restarted.get_job("alpha", second.job_id).state == "queued"
    assert "public.example.test" not in json.dumps(restarted.events("alpha", first.job_id))


def test_explicit_retention_tombstones_and_removes_only_one_project(monkeypatch, tmp_path):
    _network(monkeypatch)
    clock = [1_700_000_000.0]
    backend = _backend(tmp_path, now=lambda: clock[0])
    request = ScanSubmission(target_url=SITE)
    alpha = backend.submit(
        "alpha",
        "operator",
        "alpha-key",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    ).job
    beta = backend.submit(
        "beta",
        "operator",
        "beta-key",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    ).job
    backend.cancel_job("alpha", alpha.job_id)
    backend.cancel_job("beta", beta.job_id)
    clock[0] += 10
    cutoff = datetime.fromtimestamp(clock[0], tz=timezone.utc)
    assert backend.prune_terminal("alpha", before=cutoff) == [alpha.job_id]
    assert backend.get_job("alpha", alpha.job_id) is not None
    assert backend.prune_terminal("alpha", before=cutoff, confirm=True) == [alpha.job_id]
    assert backend.get_job("alpha", alpha.job_id) is None
    assert backend.get_job("beta", beta.job_id) is not None


def test_backend_rejects_config_drift_and_foreign_artifact_lookup(monkeypatch, tmp_path):
    requests = _network(monkeypatch)
    backend = _backend(tmp_path)
    request = ScanSubmission(target_url=SITE)
    config = request.options.effective_config()
    config["limits"]["max_urls"] = 3
    with pytest.raises(ValueError, match="do not match"):
        backend.submit("alpha", "operator", "changed", request.fingerprint(), request, config)
    with pytest.raises(JobNotReady):
        pending = backend.submit(
            "alpha",
            "operator",
            "pending",
            request.fingerprint(),
            request,
            request.options.effective_config(),
        ).job
        backend.get_result("alpha", pending.job_id)
    assert backend.artifact_path("beta", pending.job_id, "synthetic-id") is None
    assert backend.artifact_path("alpha", pending.job_id, "../scan.sqlite") is None
    changed = request.options.effective_config()
    changed["limits"]["max_urls"] = 2
    with backend._db(write=True) as con:
        con.execute(
            "UPDATE jobs SET config_json=? WHERE job_id=?",
            (json.dumps(changed), pending.job_id),
        )
    failed = backend.run_one("worker-a")
    assert failed is not None and failed.state == "failed"
    assert failed.finish_reason == "worker_failure"
    assert requests == []


def test_changed_idempotency_body_raises_backend_conflict(monkeypatch, tmp_path):
    _network(monkeypatch)
    backend = _backend(tmp_path)
    first = ScanSubmission(target_url=SITE)
    second = ScanSubmission(target_url=SITE, options={"max_urls": 1})
    backend.submit(
        "alpha",
        "operator",
        "same-key",
        first.fingerprint(),
        first,
        first.options.effective_config(),
    )
    with pytest.raises(JobConflict):
        backend.submit(
            "alpha",
            "operator",
            "same-key",
            second.fingerprint(),
            second,
            second.options.effective_config(),
        )


def test_concurrent_same_key_submits_are_atomic(monkeypatch, tmp_path):
    _network(monkeypatch)
    backend = _backend(tmp_path)
    request = ScanSubmission(target_url=SITE)

    def submit():
        return backend.submit(
            "alpha",
            "operator",
            "concurrent-key",
            request.fingerprint(),
            request,
            request.options.effective_config(),
        )

    with ThreadPoolExecutor(max_workers=2) as pool:
        outcomes = list(pool.map(lambda _index: submit(), range(2)))
    assert outcomes[0].job.job_id == outcomes[1].job.job_id
    assert {outcome.created for outcome in outcomes} == {True, False}
    assert len(backend.list_jobs("alpha", 0, 10)) == 1


def test_claims_enforce_global_and_project_slots(monkeypatch, tmp_path):
    _network(monkeypatch)
    clock = [1_700_000_000.0]
    backend = _backend(tmp_path, now=lambda: clock[0], lease_seconds=3)
    request = ScanSubmission(target_url=SITE)
    jobs = [
        backend.submit(
            project,
            "operator",
            key,
            request.fingerprint(),
            request,
            request.options.effective_config(),
        ).job
        for project, key in (("alpha", "one"), ("alpha", "two"), ("beta", "three"))
    ]
    first = backend._claim("worker-one")
    second = backend._claim("worker-two")
    assert first["job_id"] == jobs[0].job_id
    assert second["job_id"] == jobs[2].job_id
    assert backend._claim("worker-three") is None
    clock[0] += 4
    assert backend.recover_expired() == 2
    third = backend._claim("worker-three")
    assert third["job_id"] == jobs[1].job_id


def test_remote_browser_jobs_respect_slots_and_are_not_resumed_after_worker_loss(
    monkeypatch, tmp_path
):
    _network(monkeypatch)
    clock = [1_700_000_000.0]
    limits = RemoteProjectLimits(
        max_active_jobs=1,
        browser_transport={
            "transport": "remote",
            "remote_protocol": "playwright",
            "remote_endpoint_env": "SEOHEAD_REMOTE_BROWSER_ENDPOINT",
            "remote_playwright_version": "1.57.0",
        },
    )
    backend = SQLiteJobBackend(
        tmp_path / "remote-state",
        {"alpha": limits},
        producer_build="a" * 40,
        now=lambda: clock[0],
        lease_seconds=3,
    )
    request = ScanSubmission(target_url=SITE, options={"max_urls": 1, "max_requests": 20})
    first = backend.submit(
        "alpha",
        "operator",
        "remote-first",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    ).job
    second = backend.submit(
        "alpha",
        "operator",
        "remote-second",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    ).job

    assert backend._claim("worker-one")["job_id"] == first.job_id
    assert backend._claim("worker-two") is None
    clock[0] += 4
    assert backend.recover_expired() == 1
    assert backend.get_job("alpha", first.job_id).state == "failed"
    assert backend._claim("worker-three")["job_id"] == second.job_id


def test_heartbeat_renews_lease_and_expiry_fences_stale_worker(monkeypatch, tmp_path):
    _network(monkeypatch)
    clock = [1_700_000_000.0]
    backend = _backend(tmp_path, now=lambda: clock[0], lease_seconds=3)
    request = ScanSubmission(target_url=SITE)
    job = backend.submit(
        "alpha",
        "operator",
        "lease-key",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    ).job
    claimed = backend._claim("worker-a")
    assert claimed["job_id"] == job.job_id
    clock[0] += 2
    assert backend._heartbeat(job.job_id, "worker-a")
    clock[0] += 2
    assert backend.recover_expired() == 0
    clock[0] += 2
    assert backend.recover_expired() == 1
    assert backend.get_job("alpha", job.job_id).finish_reason == "worker_lease_expired"
    assert (
        backend._finalize(
            claimed,
            "worker-a",
            state="finished",
            reason="finished",
            audit_available=True,
            audit_reason="",
            artifacts={},
        )
        is None
    )
    assert backend.get_job("alpha", job.job_id).state == "failed"


def test_actual_collector_honors_cancellation_before_result_publication(monkeypatch, tmp_path):
    _network(monkeypatch)
    backend = _backend(tmp_path)
    request = ScanSubmission(target_url=SITE, options={"max_urls": 1, "max_requests": 20})
    job = backend.submit(
        "alpha",
        "operator",
        "cancel-real",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    ).job
    original = httpx.HTTPTransport.handle_request
    requested = []

    def cancel_after_page(self, outbound):
        response = original(self, outbound)
        requested.append(outbound.url.path)
        if outbound.url.path == "/":
            backend.cancel_job("alpha", job.job_id)
        return response

    monkeypatch.setattr(httpx.HTTPTransport, "handle_request", cancel_after_page)
    ended = backend.run_one("worker-a")
    assert ended is not None and ended.state == "cancelled"
    assert backend.get_result("alpha", job.job_id).coverage in {"skipped", "partial"}
    assert "/" in requested
    assert "public.example.test" not in json.dumps(backend.events("alpha", job.job_id))


def test_project_disk_limit_stops_worker_at_progress_boundary(monkeypatch, tmp_path):
    _network(monkeypatch)
    calls = [0]

    def runner(_target, *, scan_out, settings, producer_build, progress):
        del scan_out, settings, producer_build
        progress(1, 0)
        pytest.fail("resource-limited worker continued")

    backend = _backend(tmp_path, runner=runner)
    original_usage = backend._project_usage

    def usage(project_id):
        calls[0] += 1
        return 0 if calls[0] == 1 else backend.projects[project_id].max_disk_bytes + 1

    backend._project_usage = usage
    request = ScanSubmission(target_url=SITE)
    backend.submit(
        "alpha",
        "operator",
        "disk-key",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    )
    ended = backend.run_one("worker-a")
    backend._project_usage = original_usage
    assert ended is not None and ended.state == "failed"
    assert ended.finish_reason == "project_resource_limit"


def test_storage_creation_failure_is_terminal_and_safe(monkeypatch, tmp_path):
    _network(monkeypatch)
    backend = _backend(tmp_path)
    request = ScanSubmission(target_url=SITE)
    job = backend.submit(
        "alpha",
        "operator",
        "storage-key",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    ).job
    original = backend._job_dir

    def no_space(project_id, job_id, *, create=False):
        if create:
            raise OSError("synthetic disk full at /private/path?token=secret")
        return original(project_id, job_id, create=create)

    monkeypatch.setattr(backend, "_job_dir", no_space)
    ended = backend.run_one("worker-a")
    assert ended is not None and ended.state == "failed"
    assert ended.finish_reason == "storage_failure"
    assert "secret" not in json.dumps(backend.events("alpha", job.job_id))


def test_retention_removes_finished_artifacts_and_tombstone_recovery(monkeypatch, tmp_path):
    _network(monkeypatch)
    clock = [1_700_000_000.0]
    backend = _backend(tmp_path, now=lambda: clock[0])
    request = ScanSubmission(target_url=SITE, options={"max_urls": 1, "max_requests": 20})
    job = backend.submit(
        "alpha",
        "operator",
        "finished-key",
        request.fingerprint(),
        request,
        request.options.effective_config(),
    ).job
    assert backend.run_one("worker-a").state == "finished"
    result = backend.get_result("alpha", job.job_id)
    assert result is not None and len(result.artifacts) >= 3
    directory = backend._job_dir("alpha", job.job_id)
    assert directory.exists()
    clock[0] += 10
    cutoff = datetime.fromtimestamp(clock[0], tz=timezone.utc)
    with backend._db(write=True) as con:
        con.execute("UPDATE jobs SET retention_state='deleting' WHERE job_id=?", (job.job_id,))
    assert backend.get_job("alpha", job.job_id) is None
    assert backend.prune_terminal("alpha", before=cutoff, confirm=True) == [job.job_id]
    assert not directory.exists()
    assert backend.get_result("alpha", job.job_id) is None
