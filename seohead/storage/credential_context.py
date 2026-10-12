"""Redacted credential resume context and a local equality verifier."""

from __future__ import annotations

import copy
import hashlib
import json
import os
import re
import uuid
from typing import Any

from seohead.crawl.settings import redact_sensitive_headers

from . import ScanError

_ENV = re.compile(r"env:([A-Za-z_][A-Za-z0-9_]*)\Z")
_REDACTED = "REDACTED"


def redact_config(settings: dict[str, Any]) -> dict[str, Any]:
    """Copy effective settings while removing credential names, values, and profile paths."""
    if not isinstance(settings, dict):
        raise ScanError("settings must be a mapping")
    recorded = copy.deepcopy(settings)
    http = recorded.get("http")
    browser = recorded.get("rendering", {}).get("browser")
    if not isinstance(http, dict) or not isinstance(browser, dict):
        raise ScanError("settings lack the effective HTTP or browser configuration")
    entries = http.get("credential_headers")
    if not isinstance(entries, list):
        raise ScanError("credential headers must be a list")
    http["headers"] = redact_sensitive_headers(http.get("headers"))
    for entry in entries:
        if not isinstance(entry, dict) or not isinstance(entry.get("headers"), dict):
            raise ScanError("credential header entry is malformed")
        entry["headers"] = {str(name): _REDACTED for name in entry["headers"]}
    if browser.get("persistent_profile_dir"):
        browser["persistent_profile_dir"] = _REDACTED
    return recorded


def validate_recorded_credentials(snapshot: dict[str, Any]) -> dict[str, Any]:
    """Validate recorded redaction without reading the environment, return a safe validation copy."""
    if not isinstance(snapshot, dict):
        raise ScanError("recorded settings must be a mapping")
    checked = copy.deepcopy(snapshot)
    http = checked.get("http")
    browser = checked.get("rendering", {}).get("browser")
    if not isinstance(http, dict) or not isinstance(browser, dict):
        raise ScanError("recorded settings lack HTTP or browser configuration")
    entries = http.get("credential_headers")
    if not isinstance(entries, list):
        raise ScanError("recorded credential headers must be a list")
    for entry in entries:
        headers = entry.get("headers") if isinstance(entry, dict) else None
        if (
            not isinstance(entry, dict)
            or type(entry.get("host")) is not str
            or not entry["host"]
            or not isinstance(headers, dict)
            or not headers
            or any(
                type(name) is not str or not name or value != _REDACTED
                for name, value in headers.items()
            )
        ):
            raise ScanError("recorded credential headers are not exactly redacted")
    profile_dir = browser.get("persistent_profile_dir")
    if profile_dir not in {"", _REDACTED}:
        raise ScanError("recorded persistent profile path is not redacted")
    auth_entries = http.get("auth", [])
    if not isinstance(auth_entries, list):
        raise ScanError("recorded HTTP auth must be a list")
    auth_acknowledged = http.get("auth_acknowledged", False)
    if type(auth_acknowledged) is not bool:
        raise ScanError("recorded HTTP auth acknowledgement must be boolean")
    if auth_entries and not auth_acknowledged:
        raise ScanError("recorded HTTP auth lacks its required acknowledgement")
    seen_auth_hosts: set[str] = set()
    credential_hosts = {str(entry.get("host", "")).lower() for entry in entries}
    for entry in auth_entries:
        if (
            not isinstance(entry, dict)
            or set(entry) != {"host", "scheme", "username_env", "password_env"}
            or type(entry["host"]) is not str
            or not entry["host"]
            or entry["host"] != entry["host"].lower()
            or "/" in entry["host"]
            or entry["host"] in seen_auth_hosts
            or entry["host"] in credential_hosts
            or type(entry["scheme"]) is not str
            or entry["scheme"] not in {"basic", "digest"}
            or any(
                not isinstance(entry[name], str) or _ENV.fullmatch(entry[name]) is None
                for name in ("username_env", "password_env")
            )
        ):
            raise ScanError("recorded HTTP auth environment references are invalid")
        seen_auth_hosts.add(entry["host"])
    # This copy is only for structural settings validation; it never represents
    # an authenticated or persistent-profile crawl at runtime.
    http["credential_headers"] = []
    http["credentials_acknowledged"] = False
    http["auth"] = []
    http["auth_acknowledged"] = False
    browser["persistent_profile"] = False
    browser["persistent_profile_dir"] = ""
    return checked


def credential_verifier(live_settings: dict[str, Any], scan_uuid: str) -> str | None:
    """Return a salted local equality verifier for explicitly configured headers."""
    if not isinstance(live_settings, dict):
        raise ScanError("live settings must be a mapping")
    try:
        salt = uuid.UUID(scan_uuid).bytes
    except (TypeError, ValueError, AttributeError) as exc:
        raise ScanError("scan UUID is invalid for credential verification") from exc
    http = live_settings.get("http")
    entries = http.get("credential_headers") if isinstance(http, dict) else None
    if not isinstance(entries, list):
        raise ScanError("live credential headers must be a list")
    auth_entries = http.get("auth", []) if isinstance(http, dict) else None
    if not isinstance(auth_entries, list):
        raise ScanError("live HTTP auth must be a list")
    if not entries and not auth_entries:
        return None
    canonical: list[tuple[str, tuple[tuple[str, str], ...]]] = []
    for entry in entries:
        headers = entry.get("headers") if isinstance(entry, dict) else None
        if (
            not isinstance(entry, dict)
            or type(entry.get("host")) is not str
            or not isinstance(headers, dict)
        ):
            raise ScanError("live credential header entry is malformed")
        values: list[tuple[str, str]] = []
        for name, reference in headers.items():
            if type(name) is not str or type(reference) is not str:
                raise ScanError("credential header names and references must be strings")
            match = _ENV.fullmatch(reference)
            if match is None or match.group(1) not in os.environ:
                raise ScanError("credential verifier requires each configured environment value")
            values.append((name.lower(), os.environ[match.group(1)]))
        canonical.append((entry["host"].lower(), tuple(sorted(values))))
    if auth_entries:
        auth_canonical: list[tuple[str, ...]] = []
        for entry in auth_entries:
            if (
                not isinstance(entry, dict)
                or set(entry) != {"host", "scheme", "username_env", "password_env"}
                or any(type(entry[name]) is not str for name in entry)
            ):
                raise ScanError("live HTTP auth entry is malformed")
            values = []
            for name in ("username_env", "password_env"):
                match = _ENV.fullmatch(entry[name])
                if match is None or match.group(1) not in os.environ:
                    raise ScanError("HTTP auth verifier requires configured environment values")
                values.append(os.environ[match.group(1)])
            auth_canonical.append(
                (
                    entry["host"].lower(),
                    entry["scheme"],
                    entry["username_env"],
                    entry["password_env"],
                    values[0],
                    values[1],
                )
            )
        value = {
            "credential_headers": sorted(canonical),
            "http_auth": sorted(auth_canonical),
        }
    else:
        # Preserve the verifier format for existing credential-header artifacts.
        value = sorted(canonical)
    payload = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return hashlib.pbkdf2_hmac("sha256", payload, salt, 100_000).hex()
