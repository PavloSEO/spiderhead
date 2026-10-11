"""Explicit, injected delivery of retained local monitor notices.

There is deliberately no account, endpoint, timer, or transport implementation
here. A host that already has an authorized service destination injects the
small ``send`` callable. The local monitor remains quiet unless that host also
creates this adapter with ``enabled=True`` and calls it explicitly.
"""

from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import time
import uuid
from collections.abc import Callable, Iterable
from contextlib import closing, suppress
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


class DeliveryUnavailable(ValueError):
    """A request cannot safely be delivered and must not claim success."""


_SEND_LEASE_SECONDS = 300


class DeliveryReceipts:
    """Small private receipt store that survives adapter restart and retry."""

    def __init__(self, path: str | Path) -> None:
        self.path = Path(path)
        self.owner_id = f"{os.getpid()}:{uuid.uuid4().hex}"
        self.path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        if self.path.exists() and self.path.stat().st_mode & 0o077:
            raise ValueError("delivery receipt store must be private")
        # Create the file private before SQLite does, so it never exists with wider modes.
        os.close(os.open(self.path, os.O_CREAT | os.O_WRONLY, 0o600))
        with closing(sqlite3.connect(self.path)) as con, con:
            con.execute(
                """CREATE TABLE IF NOT EXISTS deliveries (
                    job_id TEXT NOT NULL, artifact_id TEXT NOT NULL, destination TEXT NOT NULL,
                    receipt TEXT NOT NULL, state TEXT NOT NULL,
                    owner_id TEXT, lease_until REAL,
                    retry_authorized INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY(job_id, artifact_id, destination)
                )"""
            )
            columns = {row[1] for row in con.execute("PRAGMA table_info(deliveries)")}
            for name, declaration in (
                ("owner_id", "TEXT"),
                ("lease_until", "REAL"),
                ("retry_authorized", "INTEGER NOT NULL DEFAULT 0"),
            ):
                if name not in columns:
                    con.execute(f"ALTER TABLE deliveries ADD COLUMN {name} {declaration}")
            # Older releases reset every send exception to pending. Those rows have no
            # verification receipt, so treat them as ambiguous rather than replaying them.
            con.execute(
                "UPDATE deliveries SET state='uncertain' WHERE state='pending' "
                "AND retry_authorized=0"
            )
            con.execute(
                "UPDATE deliveries SET owner_id='legacy:unknown' "
                "WHERE state='sending' AND owner_id IS NULL"
            )
        os.chmod(self.path, 0o600)

    def reserve(self, job_id: str, artifact_id: str, destination: str) -> tuple[str, str]:
        """Claim one delivery; expired sends become uncertain and are never replayed."""
        now = time.time()
        with closing(sqlite3.connect(self.path, isolation_level=None)) as con:
            con.execute("BEGIN IMMEDIATE")
            existing = con.execute(
                "SELECT receipt,state,owner_id,lease_until,retry_authorized FROM deliveries "
                "WHERE job_id=? AND artifact_id=? AND destination=?",
                (job_id, artifact_id, destination),
            ).fetchone()
            if existing is not None:
                if existing[1] == "delivered":
                    con.commit()
                    return existing[0], "delivered"
                if existing[1] == "sending":
                    lease_until = existing[3]
                    if isinstance(lease_until, (int, float)) and lease_until > now:
                        con.commit()
                        return existing[0], "in_progress"
                    con.execute(
                        "UPDATE deliveries SET state='uncertain',lease_until=NULL "
                        "WHERE job_id=? AND artifact_id=? AND destination=? AND state='sending'",
                        (job_id, artifact_id, destination),
                    )
                    con.commit()
                    return existing[0], "uncertain"
                if existing[1] == "uncertain":
                    con.commit()
                    return existing[0], "uncertain"
                if existing[1] == "pending" and existing[4] == 1:
                    con.execute(
                        "UPDATE deliveries SET state='sending',owner_id=?,lease_until=?,"
                        "retry_authorized=0 WHERE job_id=? AND artifact_id=? AND destination=?",
                        (
                            self.owner_id,
                            now + _SEND_LEASE_SECONDS,
                            job_id,
                            artifact_id,
                            destination,
                        ),
                    )
                    con.commit()
                    return existing[0], "claimed"
                con.execute(
                    "UPDATE deliveries SET state='uncertain',owner_id=NULL,lease_until=NULL,"
                    "retry_authorized=0 WHERE job_id=? AND artifact_id=? AND destination=?",
                    (job_id, artifact_id, destination),
                )
                con.commit()
                return existing[0], "uncertain"
            receipt = uuid.uuid4().hex
            con.execute(
                "INSERT INTO deliveries "
                "(job_id,artifact_id,destination,receipt,state,owner_id,lease_until,retry_authorized) "
                "VALUES(?,?,?,?,?,?,?,0)",
                (
                    job_id,
                    artifact_id,
                    destination,
                    receipt,
                    "sending",
                    self.owner_id,
                    now + _SEND_LEASE_SECONDS,
                ),
            )
            con.commit()
            return receipt, "claimed"

    def uncertain(self, job_id: str, artifact_id: str, destination: str, receipt: str) -> None:
        """Persist an ambiguous send outcome without releasing it for retry."""
        with closing(sqlite3.connect(self.path)) as con, con:
            con.execute(
                """UPDATE deliveries SET state='uncertain',owner_id=NULL,lease_until=NULL,
                   retry_authorized=0 WHERE job_id=? AND artifact_id=? AND destination=?
                   AND receipt=? AND state='sending' AND owner_id=?""",
                (job_id, artifact_id, destination, receipt, self.owner_id),
            )

    def reconcile(
        self,
        receipt: str,
        *,
        provider_status: str,
        sender_stopped: bool = False,
    ) -> dict[str, str]:
        """Resolve an uncertain receipt after an authoritative provider status check.

        ``not_delivered`` releases a later retry only when the caller has verified that no
        external delivery occurred. When a durable sending owner remains after lease expiry,
        the caller must also confirm that the owner process has stopped. ``unknown`` retains
        the uncertain state.
        """
        if not isinstance(provider_status, str) or provider_status not in {
            "delivered",
            "not_delivered",
            "unknown",
        }:
            raise ValueError("provider_status must be delivered, not_delivered, or unknown")
        if not isinstance(receipt, str) or not receipt:
            raise ValueError("receipt must be non-empty text")
        with closing(sqlite3.connect(self.path, isolation_level=None)) as con:
            con.execute("BEGIN IMMEDIATE")
            row = con.execute(
                "SELECT state,lease_until,owner_id FROM deliveries WHERE receipt=?", (receipt,)
            ).fetchone()
            if row is None:
                con.rollback()
                raise DeliveryUnavailable("delivery receipt was not found")
            if row[0] == "sending":
                lease_until = row[1]
                if isinstance(lease_until, (int, float)) and lease_until > time.time():
                    con.commit()
                    return {"receipt": receipt, "state": "in_progress"}
                con.execute(
                    "UPDATE deliveries SET state='uncertain',lease_until=NULL "
                    "WHERE receipt=? AND state='sending'",
                    (receipt,),
                )
                row = ("uncertain", None, row[2])
            if row[0] == "delivered":
                con.commit()
                return {"receipt": receipt, "state": "delivered"}
            if row[0] != "uncertain":
                con.rollback()
                raise DeliveryUnavailable("receipt is not awaiting reconciliation")
            if provider_status == "unknown":
                con.commit()
                return {"receipt": receipt, "state": "uncertain"}
            if (
                provider_status == "not_delivered"
                and row[2] is not None
                and sender_stopped is not True
            ):
                con.commit()
                return {
                    "receipt": receipt,
                    "state": "uncertain",
                    "reason": "the expired sending owner must be confirmed stopped before retry",
                }
            state = "delivered" if provider_status == "delivered" else "pending"
            con.execute(
                "UPDATE deliveries SET state=?,owner_id=NULL,lease_until=NULL,retry_authorized=? "
                "WHERE receipt=? AND state='uncertain'",
                (state, int(provider_status == "not_delivered"), receipt),
            )
            con.commit()
            return {"receipt": receipt, "state": state}

    def delivered(self, job_id: str, artifact_id: str, destination: str, receipt: str) -> None:
        with closing(sqlite3.connect(self.path)) as con, con:
            changed = con.execute(
                """UPDATE deliveries SET state='delivered',owner_id=NULL,lease_until=NULL
                   WHERE job_id=? AND artifact_id=? AND destination=? AND receipt=?
                   AND state='sending' AND owner_id=?""",
                (job_id, artifact_id, destination, receipt, self.owner_id),
            ).rowcount
        if changed != 1:
            with closing(sqlite3.connect(self.path)) as con:
                existing = con.execute(
                    "SELECT state FROM deliveries WHERE job_id=? AND artifact_id=? "
                    "AND destination=? AND receipt=?",
                    (job_id, artifact_id, destination, receipt),
                ).fetchone()
            if existing is None or existing[0] != "delivered":
                raise DeliveryUnavailable("delivery receipt changed while sending")


def _canonical(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


@dataclass
class MonitorServiceDelivery:
    """Send retained actionable/recovery notices once through an authorized adapter."""

    project_uuid: str
    allowed_destinations: Iterable[str]
    receipts: DeliveryReceipts
    send: Callable[[str, dict[str, Any], str], None]
    enabled: bool = False
    include_recoveries: bool = True
    _destinations: frozenset[str] = field(init=False, repr=False)

    def __post_init__(self) -> None:
        self._destinations = frozenset(self.allowed_destinations)
        if (
            not isinstance(self.project_uuid, str)
            or not self.project_uuid
            or len(self.project_uuid) > 256
        ):
            raise ValueError("monitor delivery requires a project identity")
        if not self._destinations or any(
            not isinstance(value, str) or not value for value in self._destinations
        ):
            raise ValueError("monitor delivery requires explicit destinations")

    @staticmethod
    def _events(run: dict[str, Any], include_recoveries: bool) -> list[dict[str, Any]]:
        events = [
            {"kind": "alert", "url": alert["url"], "changes": alert["changes"]}
            for alert in run.get("alerts", [])
        ]
        if include_recoveries:
            events.extend(
                {"kind": "recovery", "url": recovery["url"], "change": recovery["change"]}
                for recovery in run.get("recoveries", [])
            )
        return events

    def deliver(self, run: dict[str, Any], destination: str) -> dict[str, Any]:
        """Deliver only retained, complete alert events and return durable receipts."""
        if not self.enabled:
            raise DeliveryUnavailable("monitor service delivery is disabled")
        if destination not in self._destinations:
            raise PermissionError("destination is not authorized for monitor delivery")
        coverage = run.get("coverage")
        if run.get("state") in {"partial", "failed"} or (
            isinstance(coverage, dict) and not coverage.get("complete", False)
        ):
            raise DeliveryUnavailable("partial monitor evidence is retained but is not deliverable")
        scan_id = run.get("scan_id")
        if not isinstance(scan_id, str) or not scan_id:
            raise DeliveryUnavailable("monitor run lacks retained scan identity")
        receipts = []
        for event in self._events(run, self.include_recoveries):
            payload = {"project_uuid": self.project_uuid, "scan_id": scan_id, "event": event}
            artifact_id = hashlib.sha256(_canonical(payload).encode()).hexdigest()
            receipt, state = self.receipts.reserve(self.project_uuid, artifact_id, destination)
            if state == "delivered":
                receipts.append({"receipt": receipt, "state": "delivered", "kind": event["kind"]})
                continue
            if state in {"in_progress", "uncertain"}:
                receipts.append({"receipt": receipt, "state": state, "kind": event["kind"]})
                return {
                    "ok": False,
                    "delivery": state,
                    "receipts": receipts,
                    "action_required": (
                        "wait for the active sender"
                        if state == "in_progress"
                        else "verify the destination by receipt; reconcile as delivered or "
                        "not_delivered before retry; an expired owner lease also requires "
                        "sender_stopped=True"
                    ),
                }
            try:
                self.send(destination, payload, receipt)
                self.receipts.delivered(self.project_uuid, artifact_id, destination, receipt)
            except Exception:
                with suppress(Exception):
                    self.receipts.uncertain(self.project_uuid, artifact_id, destination, receipt)
                receipts.append({"receipt": receipt, "state": "uncertain", "kind": event["kind"]})
                return {
                    "ok": False,
                    "delivery": "uncertain",
                    "receipts": receipts,
                    "action_required": (
                        "verify the destination by receipt; reconcile as delivered or "
                        "not_delivered before retry; an expired owner lease also requires "
                        "sender_stopped=True"
                    ),
                }
            else:
                receipts.append({"receipt": receipt, "state": "sent", "kind": event["kind"]})
        return {"ok": True, "delivery": "sent" if receipts else "quiet", "receipts": receipts}
