"""Crash recovery for ambiguous retained-monitor delivery outcomes."""

from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import time

from seohead.projects.service_delivery import DeliveryReceipts, MonitorServiceDelivery


def _run():
    return {
        "state": "complete",
        "coverage": {"complete": True},
        "scan_id": "scan:recovery",
        "alerts": [
            {
                "url": "https://example.test/a",
                "changes": [{"kind": "status_changed", "severity": "warning"}],
            }
        ],
        "recoveries": [],
    }


def _service(path, send, *, owner="project:one"):
    return MonitorServiceDelivery(
        project_uuid=owner,
        allowed_destinations={"service:test"},
        receipts=DeliveryReceipts(path),
        send=send,
        enabled=True,
    )


def test_ambiguous_send_failure_stays_uncertain_after_restart(tmp_path):
    path = tmp_path / "receipts.sqlite"
    attempted = []

    def send(destination, payload, receipt):
        attempted.append((destination, payload, receipt))
        raise TimeoutError("the remote side may have accepted the message")

    first = _service(path, send).deliver(_run(), "service:test")
    receipt = first["receipts"][0]["receipt"]
    assert first["ok"] is False
    assert first["delivery"] == "uncertain"
    assert first["action_required"]
    assert len(attempted) == 1

    replayed = []
    restarted = _service(
        path, lambda destination, payload, receipt: replayed.append(receipt)
    ).deliver(_run(), "service:test")
    assert restarted["delivery"] == "uncertain"
    assert restarted["receipts"][0]["receipt"] == receipt
    assert replayed == []


def test_expired_sending_lease_becomes_uncertain_instead_of_resend(tmp_path):
    path = tmp_path / "receipts.sqlite"
    owner = "project:one"
    run = _run()
    event = MonitorServiceDelivery._events(run, True)[0]
    payload = {"project_uuid": owner, "scan_id": run["scan_id"], "event": event}
    artifact_id = hashlib.sha256(
        json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
    ).hexdigest()
    receipts = DeliveryReceipts(path)
    receipt, state = receipts.reserve(owner, artifact_id, "service:test")
    assert state == "claimed"
    with sqlite3.connect(path) as con:
        con.execute(
            "UPDATE deliveries SET lease_until=? WHERE receipt=?",
            (time.time() - 1, receipt),
        )

    attempted = []
    recovered = _service(
        path, lambda destination, payload, key: attempted.append(key), owner=owner
    ).deliver(run, "service:test")
    assert recovered["delivery"] == "uncertain"
    assert recovered["receipts"][0]["receipt"] == receipt
    assert attempted == []
    recovered_receipts = DeliveryReceipts(path)
    assert (
        recovered_receipts.reconcile(receipt, provider_status="not_delivered")["state"]
        == "uncertain"
    )
    assert (
        recovered_receipts.reconcile(
            receipt, provider_status="not_delivered", sender_stopped=True
        )["state"]
        == "pending"
    )
    retried = _service(
        path, lambda _destination, _payload, key: attempted.append(key), owner=owner
    ).deliver(run, "service:test")
    assert retried["delivery"] == "sent"
    assert attempted == [receipt]


def test_legacy_sending_row_requires_stopped_owner_before_retry(tmp_path):
    path = tmp_path / "receipts.sqlite"
    receipt = "legacy-receipt"
    with sqlite3.connect(path) as con:
        con.execute(
            """CREATE TABLE deliveries (
                job_id TEXT NOT NULL, artifact_id TEXT NOT NULL, destination TEXT NOT NULL,
                receipt TEXT NOT NULL, state TEXT NOT NULL,
                PRIMARY KEY(job_id, artifact_id, destination)
            )"""
        )
        con.execute(
            "INSERT INTO deliveries VALUES(?,?,?,?,?)",
            ("project:one", "artifact:legacy", "service:test", receipt, "sending"),
        )
    os.chmod(path, 0o600)

    receipts = DeliveryReceipts(path)
    assert receipts.reserve("project:one", "artifact:legacy", "service:test") == (
        receipt,
        "uncertain",
    )
    assert receipts.reconcile(receipt, provider_status="not_delivered")["state"] == "uncertain"
    assert (
        receipts.reconcile(
            receipt, provider_status="not_delivered", sender_stopped=True
        )["state"]
        == "pending"
    )


def test_live_sending_lease_is_owned_and_not_replayed(tmp_path):
    path = tmp_path / "receipts.sqlite"
    owner = "project:one"
    run = _run()
    event = MonitorServiceDelivery._events(run, True)[0]
    payload = {"project_uuid": owner, "scan_id": run["scan_id"], "event": event}
    artifact_id = hashlib.sha256(
        json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
    ).hexdigest()
    receipt, state = DeliveryReceipts(path).reserve(owner, artifact_id, "service:test")
    assert state == "claimed"

    attempted = []
    result = _service(
        path, lambda _destination, _payload, key: attempted.append(key), owner=owner
    ).deliver(run, "service:test")
    assert result["delivery"] == "in_progress"
    assert result["receipts"][0]["receipt"] == receipt
    assert attempted == []


def test_only_verified_non_delivery_releases_an_uncertain_receipt(tmp_path):
    path = tmp_path / "receipts.sqlite"

    def fail_after_attempt(_destination, _payload, _receipt):
        raise TimeoutError("ambiguous result")

    first = _service(path, fail_after_attempt).deliver(_run(), "service:test")
    receipt = first["receipts"][0]["receipt"]
    receipts = DeliveryReceipts(path)
    assert receipts.reconcile(receipt, provider_status="unknown")["state"] == "uncertain"

    replayed = []
    still_uncertain = _service(
        path, lambda _destination, _payload, key: replayed.append(key)
    ).deliver(_run(), "service:test")
    assert still_uncertain["delivery"] == "uncertain"
    assert replayed == []

    assert receipts.reconcile(receipt, provider_status="not_delivered")["state"] == "pending"
    retried = _service(
        path, lambda _destination, _payload, key: replayed.append(key)
    ).deliver(_run(), "service:test")
    assert retried["delivery"] == "sent"
    assert replayed == [receipt]


def test_receipt_store_failure_after_send_is_uncertain_and_not_replayed(tmp_path, monkeypatch):
    path = tmp_path / "receipts.sqlite"
    receipts = DeliveryReceipts(path)
    sent = []

    def receipt_write_failure(*_args):
        raise OSError("receipt storage unavailable")

    monkeypatch.setattr(receipts, "delivered", receipt_write_failure)
    result = MonitorServiceDelivery(
        project_uuid="project:one",
        allowed_destinations={"service:test"},
        receipts=receipts,
        send=lambda _destination, _payload, key: sent.append(key),
        enabled=True,
    ).deliver(_run(), "service:test")
    assert result["delivery"] == "uncertain"
    assert len(sent) == 1

    replayed = []
    recovered = _service(
        path, lambda _destination, _payload, key: replayed.append(key)
    ).deliver(_run(), "service:test")
    assert recovered["delivery"] == "uncertain"
    assert replayed == []
