"""Narrow CLI adapter for Settings → Источники данных. Every core call runs outside the UI thread.

Only declared, redacted fields cross into the UI: provider ids, credential component names (never values), where a
credential is referenced (``config:…`` / ``env:…``), readiness states and spend totals. The operation allowlist has no
credential write, sync or paid call; ``verify`` is a free read the user starts explicitly. Core stderr is never forwarded.
"""

from __future__ import annotations

import json
import math
import re
import subprocess
from datetime import datetime

from PyQt5 import sip
from PyQt5.QtCore import QObject, QRunnable, QThreadPool, pyqtSignal

IDENTIFIER = re.compile(r"[a-zA-Z0-9_.:-]{1,100}\Z")
REFERENCE = re.compile(r"(?:config:[a-zA-Z0-9_./-]+|env:[A-Z0-9_]+|local-grant:[a-z0-9_]+)\Z")
DAY = re.compile(r"\d{4}-\d{2}-\d{2}\Z")
STATES = {"configured_unverified", "missing", "invalid", "not_required", "not_configured", "verified", "ready",
          "connected", "expired", "revoked", "waiting", "failed"}
VERIFY_PROVIDERS = {"gsc", "yandex_webmaster", "bing_webmaster"}
FAILED = "Ядро не вернуло данные. Повторите чтение."


def _number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def redacted_providers(providers):
    """Configuration is never relabelled as authenticated access; only declared fields survive."""
    result = {}
    for pid, entry in (providers or {}).items():
        if not isinstance(pid, str) or not IDENTIFIER.fullmatch(pid) or not isinstance(entry, dict):
            continue
        sources = {}
        for component, source in (entry.get("credential_sources") or {}).items():
            if not isinstance(source, dict) or not IDENTIFIER.fullmatch(str(component)):
                continue
            reference = source.get("source_reference")
            sources[component] = {
                "source_reference": reference if isinstance(reference, str) and REFERENCE.fullmatch(reference) else None,
                "accepted_source_references": [r for r in source.get("accepted_source_references", []) if isinstance(r, str) and REFERENCE.fullmatch(r)],
            }
        state = entry.get("readiness_state", entry.get("state"))
        result[pid] = {
            "readiness_state": state if state in STATES else "unknown",
            "verified": entry.get("verified") is True,
            "credential_components": {k: v is True for k, v in (entry.get("credential_components") or {}).items() if IDENTIFIER.fullmatch(str(k))},
            "credential_sources": sources,
            "quota_mode": str(entry.get("quota_mode") or "")[:120],
        }
    return result


def redacted_registry(registry):
    """Registry facts the sheet shows: access class, credential components, operation names, privacy class."""
    result = {}
    for pid, entry in ((registry or {}).get("providers") or {}).items():
        if not isinstance(pid, str) or not IDENTIFIER.fullmatch(pid) or not isinstance(entry, dict):
            continue
        result[pid] = {
            "credential_components": [c for c in entry.get("credential_components", []) if isinstance(c, str) and IDENTIFIER.fullmatch(c)],
            "access": str(entry.get("access") or "")[:40],
            "operations": [o for o in entry.get("operations", []) if isinstance(o, str) and IDENTIFIER.fullmatch(o)][:40],
            "quota_mode": str(entry.get("quota_mode") or "")[:120],
            "privacy_class": str(entry.get("privacy_class") or "")[:40],
        }
    return result


def redacted_spend(report):
    """Totals as the core logged them (requests, usd, limits); nothing is converted into roubles."""
    def totals(mapping):
        result = {}
        for name, values in (mapping or {}).items():
            if not isinstance(name, str) or not IDENTIFIER.fullmatch(name) or not isinstance(values, dict):
                continue
            units = {u: v for u, v in values.items() if IDENTIFIER.fullmatch(str(u)) and _number(v)}
            if units:
                result[name] = units
        return result

    calls, since = report.get("calls"), report.get("since")
    return {
        "calls": calls if isinstance(calls, int) and not isinstance(calls, bool) and calls >= 0 else None,
        "since": since if isinstance(since, str) and DAY.fullmatch(since) else None,
        "by_source": totals(report.get("by_source")),
        "by_operation": totals(report.get("by_operation")),
        "by_day": totals(report.get("by_day")),
        "uncertain_count": len(report.get("uncertain") or []),
    }


def redacted_status(status):
    keys = ("source", "resource", "rows", "first_date", "last_date", "last_fetched_at", "synced_days", "requested_days")
    entries = []
    for entry in (status or {}).get("sources", [])[:200]:
        if isinstance(entry, dict):
            row = {k: entry.get(k) for k in keys}
            coverage = entry.get("coverage") if isinstance(entry.get("coverage"), dict) else {}
            row["failed_days"] = len(coverage.get("failed") or [])
            entries.append(row)
    return entries


def read_cli(executable, arguments):
    """Never forward stderr, exceptions or arbitrary JSON (may contain secrets)."""
    completed = subprocess.run([executable, *arguments], capture_output=True, text=True, timeout=45, check=False)
    if completed.returncode != 0 or len(completed.stdout) > 4 * 1024 * 1024:
        raise ValueError("Core command unavailable")
    value = json.loads(completed.stdout)
    if not isinstance(value, dict) or value.get("ok") is False:
        raise ValueError("Core command refused")
    return value


def load_sources(executable, operation, project=None, provider=None, since=None, *, call=read_cli):
    """Explicit allowlist; this adapter cannot store a key, sync, or launch a paid call."""
    if operation == "snapshot":
        result = {"errors": []}
        commands = {"registry": ["provider-registry"], "readiness": ["provider-readiness"],
                    "spend": ["spend-report", "--since", datetime.now().strftime("%Y-%m-01")]}
        for name, args in commands.items():
            try:
                value = call(executable, args)
                result[name] = {"registry": redacted_registry, "readiness": lambda v: redacted_providers(v.get("providers")),
                                "spend": redacted_spend}[name](value)
            except (OSError, ValueError, TypeError, AttributeError, subprocess.SubprocessError):
                result["errors"].append(name)
        return result
    if operation == "spend":
        return {"spend": redacted_spend(call(executable, ["spend-report", *(["--since", since] if since and DAY.fullmatch(since) else [])]))}
    if operation == "doctor":
        return {"providers": redacted_providers(call(executable, ["sources-doctor"]).get("provider_status"))}
    if operation == "status":
        if not project:
            raise ValueError("no project")
        return {"sources": redacted_status(call(executable, ["sources-status", "--project", project]))}
    if operation == "auth-status" and provider == "gsc":
        value = call(executable, ["provider-auth", "--provider", provider, "--action", "status"])
        return {"configured": value.get("configured") is True, "access_verified": value.get("access_verified") is True}
    if operation == "verify" and provider in VERIFY_PROVIDERS:
        value = call(executable, ["provider-verify", "--provider", provider])
        return {"verified": value.get("verified") is True, "state": value.get("state") if value.get("state") in STATES else "unknown"}
    raise ValueError("Unsupported settings operation")


class _Signals(QObject):
    done = pyqtSignal(object)
    failed = pyqtSignal(str)


class _Read(QRunnable):
    def __init__(self, executable, operation, project, provider, since):
        super().__init__()
        self.signals = _Signals()
        self.args = executable, operation, project, provider, since

    def run(self):
        try:
            self.signals.done.emit(load_sources(*self.args))
        except (OSError, ValueError, TypeError, AttributeError, subprocess.SubprocessError):
            self.signals.failed.emit(FAILED)


class SourceService(QObject):
    """Owns the workers and drops answers addressed to a page that is already gone."""

    def __init__(self, executable, parent=None):
        super().__init__(parent)
        self.executable = executable
        self._pending = {}
        self._serial = 0

    def request(self, operation, callback, on_error, owner, project=None, provider=None, since=None):
        if not self.executable:
            on_error("CLI ядра seohead не найден")
            return
        self._serial += 1
        serial = self._serial
        worker = self._pending[serial] = _Read(self.executable, operation, project, provider, since)

        def deliver(handler, value):
            self._pending.pop(serial, None)
            if not sip.isdeleted(owner):
                handler(value)

        worker.signals.done.connect(lambda value: deliver(callback, value))
        worker.signals.failed.connect(lambda value: deliver(on_error, value))
        QThreadPool.globalInstance().start(worker)
