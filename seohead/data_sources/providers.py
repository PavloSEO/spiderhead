"""Offline-first provider registry, verification, collection evidence, and URL joins.

The registry is descriptive.  A present credential is only configuration state; authenticated
access becomes ``verified`` only after :func:`provider_verify` completes its declared live read.
No registry lookup performs a network request or a paid call.
"""

from __future__ import annotations

import hashlib
import json
import os
import stat
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from seohead.checks.external_join import join_external_data, orphan_urls
from seohead.core.filesystem import atomic_write_bytes
from seohead.data_sources import credentials
from seohead.data_sources.yandex_webmaster import OPERATIONS as _WEBMASTER_OPERATIONS

EVIDENCE_FORMAT = "seohead.provider-evidence.v1"
_STATES = {"complete", "partial", "failed", "skipped", "no_field_data", "not_configured"}
_REGISTRY: dict[str, dict[str, Any]] = {
    "arsenkin": {
        "credential_components": ["api_token"],
        "access": "read_only_paid",
        "operations": ["keyword_frequency", "serp_clustering"],
        "quota_mode": "paid limit credits",
        "privacy_class": "aggregate",
    },
    "yandex_cloud": {
        "credential_components": ["api_key", "folder_id"],
        "access": "read_only_paid",
        "operations": ["wordstat", "web_search"],
        "quota_mode": "provider quota and recorded spend",
        "privacy_class": "aggregate",
    },
    "gsc": {
        "credential_components": ["oauth_bearer", "service_account"],
        "access": "read_only",
        "operations": ["verify", "properties", "search_analytics", "inspection", "sitemaps"],
        "quota_mode": "Google Search Console row and request limits",
        "privacy_class": "restricted",
    },
    "crux": {
        "credential_components": ["api_key"],
        "access": "read_only",
        "operations": ["current", "history"],
        "cost_mode": "free_within_quota",
        "quota_mode": "Google Cloud API quota",
        "privacy_class": "aggregate",
    },
    "pagespeed": {
        "credential_components": ["api_key"],
        "access": "read_only",
        "operations": ["mobile_samples", "desktop_samples"],
        "quota_mode": "Google API quota",
        "privacy_class": "aggregate",
    },
    "ga4": {
        "credential_components": ["oauth_bearer", "service_account"],
        "access": "read_only",
        "operations": ["landing_pages", "page_views"],
        "quota_mode": "GA4 Data API quota",
        "privacy_class": "restricted",
    },
    "metrika": {
        "credential_components": ["oauth_bearer"],
        "access": "read_only",
        "operations": ["counters", "aggregate_report"],
        "quota_mode": "Yandex Metrika API quota",
        "privacy_class": "restricted",
        "excluded_operations": ["raw_logs"],
    },
    "yandex_webmaster": {
        "credential_components": ["oauth_bearer"],
        "access": "read_only",
        "operations": list(_WEBMASTER_OPERATIONS),
        "quota_mode": "Yandex Webmaster application quota",
        "privacy_class": "restricted",
    },
    "miratext": {
        "credential_components": ["api_key"],
        "access": "read_only_optional_paid",
        "operations": ["seo_analysis"],
        "quota_mode": "free queue or explicitly confirmed paid provider task",
        "privacy_class": "restricted",
    },
    "bing_webmaster": {
        "credential_components": ["api_key"],
        "access": "read_only",
        "operations": ["sites", "crawl", "links", "url_links", "keywords", "search_performance"],
        "quota_mode": "Bing Webmaster API quota",
        "privacy_class": "restricted",
    },
    "dataforseo_backlinks": {
        "credential_components": ["login", "password"],
        "access": "read_only_optional_paid",
        "operations": [
            "backlinks_summary",
            "backlinks",
            "referring_domains",
            "backlinks_history",
            "domain_pages_summary",
        ],
        "quota_mode": "paid per provider response",
        "privacy_class": "restricted",
        "default_enabled": False,
    },
    "moz": {
        "credential_components": ["api_token"],
        "access": "read_only_paid",
        "operations": ["backlinks_summary"],
        "quota_mode": "Moz API plan limits",
        "privacy_class": "restricted",
        "default_enabled": False,
    },
    "cloudflare": {
        "credential_components": ["api_token"],
        "access": "read_only",
        "operations": ["token_verify", "traffic", "logpull", "history"],
        "quota_mode": "Cloudflare account and zone plan limits",
        "privacy_class": "restricted",
    },
    "indexnow": {
        "credential_components": ["submission_key"],
        "access": "confirmed_write",
        "operations": ["submit"],
        "quota_mode": "provider submission quota",
        "privacy_class": "public_url_list",
        "default_enabled": False,
    },
    "wayback": {
        "credential_components": [],
        "access": "read_only",
        "operations": ["history"],
        "quota_mode": "public service pacing",
        "privacy_class": "public",
    },
    "crtsh": {
        "credential_components": [],
        "access": "read_only",
        "operations": ["subdomains"],
        "quota_mode": "public service availability",
        "privacy_class": "public",
    },
}

# Credential references are relative to the shared config root or name an
# environment variable. They are displayable; values and absolute paths are not.
_CREDENTIAL_SOURCES: dict[str, dict[str, tuple[str, str]]] = {
    "arsenkin": {"api_token": ("arsenkin/token", "ARSENKIN_TOKEN")},
    "yandex_cloud": {
        "api_key": ("yandex-wordstat/api_key", "YANDEX_CLOUD_API_KEY"),
        "folder_id": ("yandex-wordstat/folder_id", "YANDEX_CLOUD_FOLDER_ID"),
    },
    "gsc": {"oauth_bearer": ("gsc/access_token", "GSC_ACCESS_TOKEN")},
    "crux": {"api_key": ("crux/api_key", "CRUX_API_KEY")},
    "pagespeed": {"api_key": ("pagespeed/api_key", "PAGESPEED_API_KEY")},
    "ga4": {"oauth_bearer": ("ga4/access_token", "GA4_ACCESS_TOKEN")},
    "metrika": {"oauth_bearer": ("yandex-metrika/token", "YANDEX_METRIKA_TOKEN")},
    "yandex_webmaster": {
        "oauth_bearer": ("yandex-webmaster/access_token", "YANDEX_WEBMASTER_TOKEN")
    },
    "miratext": {"api_key": ("miratext/api_key", "MIRATEXT_API_KEY")},
    "bing_webmaster": {"api_key": ("bing-webmaster/api_key", "BING_WEBMASTER_API_KEY")},
    "dataforseo_backlinks": {
        "login": ("dataforseo/login", "DATAFORSEO_LOGIN"),
        "password": ("dataforseo/password", "DATAFORSEO_PASSWORD"),
    },
    "moz": {"api_token": ("moz/api_token", "MOZ_API_TOKEN")},
    "cloudflare": {"api_token": ("cloudflare/api-token", "CLOUDFLARE_API_TOKEN")},
    "indexnow": {"submission_key": ("indexnow/key", "INDEXNOW_KEY")},
}

# This map describes only the generic provider-collect dispatch. Dedicated
# routes and declared-but-unshipped operations stay explicit in discovery.
_COLLECTABLE_OPERATIONS: dict[str, frozenset[str]] = {
    "gsc": frozenset({"verify", "properties", "search_analytics", "inspection", "sitemaps"}),
    "crux": frozenset({"current", "history"}),
    "pagespeed": frozenset({"mobile_samples", "desktop_samples"}),
    "ga4": frozenset({"landing_pages", "page_views"}),
    "metrika": frozenset({"counters", "aggregate_report"}),
    "yandex_webmaster": frozenset(_WEBMASTER_OPERATIONS),
    "bing_webmaster": frozenset(
        {"sites", "crawl", "links", "url_links", "keywords", "search_performance"}
    ),
    "cloudflare": frozenset({"traffic", "logpull", "history"}),
    "dataforseo_backlinks": frozenset(
        {
            "backlinks_summary",
            "backlinks",
            "referring_domains",
            "backlinks_history",
            "domain_pages_summary",
        }
    ),
    "wayback": frozenset({"history"}),
    "crtsh": frozenset({"subdomains"}),
}
_DEDICATED_OPERATIONS = {
    "arsenkin": {"keyword_frequency": "keywords-exact"},
    "yandex_cloud": {
        "wordstat": "keywords-expand / keywords-seasonality",
        "web_search": "serp-fetch",
    },
    "indexnow": {"submit": "indexnow-submit"},
    "miratext": {"seo_analysis": "miratext-analyze"},
    "cloudflare": {"token_verify": "provider-verify"},
}


def provider_registry() -> dict[str, Any]:
    """Return immutable-by-convention metadata; callers receive a JSON-safe copy."""
    return {
        "format": "seohead.provider-registry.v1",
        "providers": json.loads(json.dumps(_REGISTRY)),
    }


def _credential_source_status(provider: str, component: str) -> dict[str, Any]:
    path, env_var = _CREDENTIAL_SOURCES[provider][component]
    file_env_var = (
        "SEOHEAD_CLOUDFLARE_TOKEN_FILE"
        if provider == "cloudflare" and component == "api_token"
        else None
    )
    return credentials.source_status(path, env_var, file_env_var=file_env_var)


def credential_slot(provider: str, component: str) -> dict[str, Any]:
    """Resolve a provider/component pair to its secret slot without reading the secret.

    Raises ``ValueError`` for an unknown provider or a component that is not a stored secret
    of that provider. ``env_shadowed`` reports whether the environment variable would take
    precedence over the file. The secret value is never read or returned.
    """
    if provider not in _REGISTRY:
        raise ValueError("unknown provider")
    sources = _CREDENTIAL_SOURCES.get(provider, {})
    if component not in sources:
        raise ValueError(f"{component!r} is not a stored credential component of {provider!r}")
    path, env_var = sources[component]
    slot: dict[str, Any] = {
        "provider": provider,
        "component": component,
        "source_reference": f"config:{path}",
        "env_reference": f"env:{env_var}",
        "env_shadowed": bool((os.environ.get(env_var) or "").strip()),
    }
    if provider == "cloudflare" and component == "api_token":
        slot["accepted_source_references"] = [
            f"env:{env_var}",
            f"config:{path}",
            "env-path:SEOHEAD_CLOUDFLARE_TOKEN_FILE",
        ]
        slot["env_shadowed"] = slot["env_shadowed"] or bool(
            (os.environ.get("SEOHEAD_CLOUDFLARE_TOKEN_FILE") or "").strip()
        )
    return slot


def manage_credential(
    provider: str,
    component: str,
    action: str = "status",
    *,
    value: str | None = None,
    replace: bool = False,
    confirm: bool = False,
) -> dict[str, Any]:
    """Inspect, write or revoke one registry-owned credential without returning its value."""
    slot = credential_slot(provider, component)
    path, _env_var = _CREDENTIAL_SOURCES[provider][component]
    if action == "status":
        state = _credential_source_status(provider, component)
        result = {
            "ok": True,
            "provider": provider,
            "component": component,
            **slot,
            "source_reference": state.get("source_reference") or slot["source_reference"],
            "accepted_source_references": state.get(
                "accepted_source_references", slot.get("accepted_source_references", [])
            ),
            "state": state["state"],
            "updated_at": None,
        }
        if (
            state["state"] == "configured_unverified"
            and state["source_reference"] == slot["source_reference"]
        ):
            try:
                result["updated_at"] = datetime.fromtimestamp(
                    (credentials.CONFIG_ROOT / path).stat().st_mtime, timezone.utc
                ).isoformat()
            except OSError:
                result["state"] = "unavailable"
        return result
    if action == "set":
        if not isinstance(value, str):
            raise ValueError("credential value must be non-empty text no larger than 65536 bytes")
        stored = value.removesuffix("\n").removesuffix("\r")
        if not stored.strip() or len(stored.encode("utf-8")) > 65536:
            raise ValueError("credential value must be non-empty text no larger than 65536 bytes")
        if any(ord(char) < 32 or ord(char) == 127 for char in stored):
            raise ValueError("credential value contains an unsupported character")
        credentials.write_secret(path, stored, replace=replace)
        return {
            "ok": True,
            "provider": provider,
            "component": component,
            "source_reference": slot["source_reference"],
            "env_shadowed": slot["env_shadowed"],
            "updated_at": datetime.fromtimestamp(
                (credentials.CONFIG_ROOT / path).stat().st_mtime, timezone.utc
            ).isoformat(),
            "replaced": replace,
        }
    if action == "revoke":
        if confirm is not True:
            raise ValueError("credential revocation requires confirm=true")
        removed = credentials.revoke_secret(path)
        return {
            "ok": True,
            "provider": provider,
            "component": component,
            "source_reference": slot["source_reference"],
            "env_shadowed": slot["env_shadowed"],
            "revoked": removed,
        }
    raise ValueError("action must be status, set, or revoke")


def provider_discover(
    provider: str, *, domain: str | None = None, transport: Any = None
) -> dict[str, Any]:
    """List resources visible to an authenticated account; never select or bind them."""
    if provider not in {"google", "yandex", "topvisor"}:
        raise ValueError("provider must be google, yandex, or topvisor")
    if domain is not None and (
        not isinstance(domain, str) or not domain.strip() or len(domain) > 253
    ):
        raise ValueError("domain must be a bounded hostname")
    if provider == "google":
        from seohead.data_sources.gsc import discover_properties

        result = discover_properties(transport=transport)
        rows = result.get("properties", []) if result.get("ok") else []
        resources = [
            {"service": "gsc", "resource": row.get("site_url"), "label": row.get("site_url")}
            for row in rows
            if isinstance(row, dict) and isinstance(row.get("site_url"), str)
        ]
    elif provider == "yandex":
        from seohead.data_sources.yandex_webmaster import collect

        result = collect("hosts", transport=transport)
        data = result.get("data", {}) if result.get("ok") else {}
        hosts = data.get("hosts", []) if isinstance(data, dict) else []
        resources = [
            {
                "service": "webmaster",
                "resource": row.get("host_id"),
                "label": row.get("ascii_host_url"),
            }
            for row in hosts
            if isinstance(row, dict) and isinstance(row.get("host_id"), str)
        ]
    else:
        from seohead.data_sources.topvisor import fetch

        result = fetch("projects", {"limit": 100, "offset": 0})
        rows = result.get("result", []) if result.get("ok") else []
        resources = [
            {
                "service": "topvisor",
                "resource": str(row.get("id")),
                "label": row.get("name"),
            }
            for row in rows
            if isinstance(row, dict) and row.get("id") is not None
        ]
    if domain:
        from urllib.parse import urlsplit

        needle = domain.strip().lower()
        if "://" in needle:
            needle = urlsplit(needle).hostname or needle
        needle = needle.removeprefix("www.")
        for item in resources:
            raw = str(item.get("label") or item.get("resource") or "")
            host = (
                raw.removeprefix("sc-domain:").lower()
                if raw.startswith("sc-domain:")
                else (urlsplit(raw).hostname or "").lower()
            )
            item["domain_match"] = host.removeprefix("www.") == needle if host else None
    return {
        "ok": bool(result.get("ok")),
        "provider": provider,
        "resources": resources,
        "count": len(resources),
        "truncated": bool(result.get("nextOffset")),
        "selected": False,
        "error": result.get("error"),
        "note": "discovery lists visible resources only; it does not bind or select one",
    }


def _credential_details(provider: str) -> tuple[dict[str, bool], dict[str, dict[str, Any]]]:
    sources = _CREDENTIAL_SOURCES.get(provider)
    if provider not in _REGISTRY:
        raise ValueError("unknown provider")
    components: dict[str, bool] = {}
    details: dict[str, dict[str, Any]] = {}
    for name in sources or {}:
        state = _credential_source_status(provider, name)
        details[name] = state
        components[name] = state["state"] == "configured_unverified"
    if provider in {"gsc", "ga4"}:
        components["service_account"] = credentials.gsc_service_account_available()
        service_status = credentials.gsc_service_account_status()
        service_reference = (
            "env:GSC_SERVICE_ACCOUNT_FILE"
            if "GSC_SERVICE_ACCOUNT_FILE" in os.environ
            else "config:gsc/service-account.json"
        )
        details["service_account"] = {
            "state": (
                "configured_unverified"
                if service_status == "configured_unverified"
                else "missing"
                if service_status == "missing"
                else "invalid"
            ),
            "source_reference": service_reference if service_status != "missing" else None,
            "accepted_source_references": [
                "env:GSC_SERVICE_ACCOUNT_FILE",
                "config:gsc/service-account.json",
            ],
            **(
                {"reason": service_status}
                if service_status not in {"missing", "configured_unverified"}
                else {}
            ),
        }
    if provider == "gsc":
        from seohead.data_sources.oauth import grant_available

        grant = grant_available("gsc")
        components["durable_oauth"] = grant
        details["durable_oauth"] = {
            "state": "configured_unverified" if grant else "missing",
            "source_reference": "local-grant:gsc" if grant else None,
            "accepted_source_references": ["local-grant:gsc"],
        }
    return components, details


def _credential_components(provider: str) -> dict[str, bool]:
    return _credential_details(provider)[0]


def _readiness_state(
    provider: str, components: dict[str, bool], details: dict[str, dict[str, Any]]
) -> str:
    if not components:
        return "not_required"
    ready = any(components.values()) if provider in {"gsc", "ga4"} else all(components.values())
    if ready:
        return "configured_unverified"
    if any(item.get("state") == "invalid" for item in details.values()):
        return "invalid"
    return "missing"


def _input_schema(provider: str, operation: str) -> dict[str, Any]:
    return {
        "type": "object",
        "required": ["provider", "operation", "request"],
        "properties": {
            "provider": {"type": "string", "const": provider},
            "operation": {"type": "string", "const": operation},
            "request": {
                "type": "object",
                "additionalProperties": True,
                "description": "Provider-specific fields are validated by the selected operation adapter.",
            },
        },
    }


def _operation_contract(provider: str, operation: str) -> dict[str, Any]:
    if provider not in _REGISTRY or operation not in _REGISTRY[provider]["operations"]:
        return {"name": operation, "state": "unsupported", "surface": None}
    dedicated_surface = _DEDICATED_OPERATIONS.get(provider, {}).get(operation)
    if dedicated_surface:
        state, surface = (
            ("dedicated_write", dedicated_surface)
            if provider == "indexnow"
            else ("dedicated", dedicated_surface)
        )
    elif operation in _COLLECTABLE_OPERATIONS.get(provider, frozenset()):
        state, surface = "supported", "provider-collect"
    else:
        state, surface = "unsupported", None
    contract: dict[str, Any] = {"name": operation, "state": state, "surface": surface}
    if state == "supported":
        contract["input_schema"] = _input_schema(provider, operation)
    return contract


def _operation_catalog(provider: str) -> list[dict[str, Any]]:
    return [
        _operation_contract(provider, operation) for operation in _REGISTRY[provider]["operations"]
    ]


def provider_readiness(provider: str | None = None, operation: str | None = None) -> dict[str, Any]:
    """Describe credential configuration and operation routes without provider requests."""
    if operation is not None and (not isinstance(operation, str) or not operation.strip()):
        return {"ok": False, "state": "invalid", "error": "operation must be a non-empty string"}
    if operation is not None and provider is None:
        return {
            "ok": False,
            "state": "invalid",
            "error": "provider is required when selecting an operation",
        }
    if provider is not None and (not isinstance(provider, str) or not provider.strip()):
        return {"ok": False, "state": "invalid", "error": "provider must be a non-empty string"}
    provider_status = sources_doctor()["providers"]
    if provider is None:
        return {
            "ok": True,
            "format": "seohead.provider-readiness.v1",
            "verification_performed": False,
            "providers": provider_status,
            "note": "configured credentials are not verified account or target access",
        }
    provider = provider.strip()
    if provider not in _REGISTRY:
        return {"ok": False, "state": "unsupported", "error": "unsupported provider"}
    status = provider_status[provider]
    result: dict[str, Any] = {
        "ok": True,
        "format": "seohead.provider-readiness.v1",
        "provider": provider,
        "state": status["readiness_state"],
        "readiness_state": status["readiness_state"],
        "verified": False,
        "permission_state": "not_verified",
        "target_access": "not_requested",
        "credential_components": status["credential_components"],
        "credential_sources": status["credential_sources"],
        "access": _REGISTRY[provider]["access"],
        "quota_mode": _REGISTRY[provider]["quota_mode"],
        "privacy_class": _REGISTRY[provider]["privacy_class"],
        "operations": status["operations"],
        "verification_surface": (
            "provider-verify" if provider in {"gsc", "yandex_webmaster", "bing_webmaster"} else None
        ),
        "unavailable_reason": status.get("unavailable_reason"),
    }
    if operation is not None:
        operation = operation.strip()
        selected = next((item for item in status["operations"] if item["name"] == operation), None)
        if selected is None:
            return {
                **result,
                "ok": False,
                "state": "unsupported",
                "readiness_state": status["readiness_state"],
                "operation": {"name": operation, "state": "unsupported"},
                "error": "unsupported provider operation",
            }
        result["operations"] = [selected]
        result["operation"] = selected
        if selected["state"] == "unsupported":
            result["ok"] = False
            result["state"] = "unsupported"
            result["readiness_state"] = status["readiness_state"]
            result["error"] = "declared operation has no supported route"
    return result


def sources_doctor() -> dict[str, Any]:
    """Configuration inspection without secrets or a false provider-access claim."""
    providers = {}
    for name in _REGISTRY:
        components, sources = _credential_details(name)
        available = any(components.values()) if name in {"gsc", "ga4"} else all(components.values())
        readiness = _readiness_state(name, components, sources)
        providers[name] = {
            "state": (
                "credential_present"
                if components and available
                else "not_configured"
                if components
                else "not_required"
            ),
            "credential_components": components,
            "credential_sources": sources,
            "readiness_state": readiness,
            "permission_state": "not_verified",
            "target_access": "not_requested",
            "verified": False,
            "note": "run explicit provider-verify; configured credentials are not verified access",
            "operations": _operation_catalog(name),
            "quota_mode": _REGISTRY[name]["quota_mode"],
            "privacy_class": _REGISTRY[name]["privacy_class"],
        }
        if name in {"gsc", "ga4"}:
            providers[name]["service_account_status"] = credentials.gsc_service_account_status()
        if readiness in {"missing", "invalid"}:
            providers[name]["unavailable_reason"] = (
                "one or more required credential components are missing"
                if readiness == "missing"
                else "one or more configured credential sources are invalid"
            )
    return {"format": "seohead.provider-doctor.v1", "providers": providers}


def _reference(value: Any) -> str | None:
    """Public envelopes never carry a reversible or guessable target identifier."""
    return None


def _redacted_filters(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or not value:
        return {"state": "not_supplied", "keys": []}
    return {
        "state": "values_redacted",
        "keys": sorted(str(key) for key in value),
    }


def _now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _save_local_artifact(directory: str | Path, value: dict[str, Any]) -> str:
    root = Path(directory)
    if root.is_symlink():
        raise ValueError("provider artifact directory must not be a symlink")
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    root_info = root.lstat()
    if not stat.S_ISDIR(root_info.st_mode) or not credentials.is_private_mode(root_info.st_mode):
        raise ValueError("provider artifact directory must be a private directory")
    digest = hashlib.sha256(json.dumps(value, sort_keys=True, default=str).encode()).hexdigest()
    destination = root / f"provider-{digest[:16]}.json"
    atomic_write_bytes(
        destination,
        json.dumps(value, ensure_ascii=False, sort_keys=True).encode("utf-8"),
        mode=0o600,
    )
    return f"local-artifact:{digest}"


def _evidence(
    provider: str,
    operation: str,
    request: dict[str, Any],
    result: dict[str, Any],
    artifact: str | None,
) -> dict[str, Any]:
    state = result.get("state")
    if state not in _STATES:
        state = "complete" if result.get("ok") else "failed"
    rows = (
        result.get("rows")
        or result.get("samples")
        or result.get("url_facts")
        or result.get("summary")
        or []
    )
    return {
        "format": EVIDENCE_FORMAT,
        "provider": provider,
        "operation": operation,
        "retrieved_at": _now(),
        "period": request.get("period") or result.get("period") or result.get("collection_period"),
        "dimensions": request.get("dimensions") or result.get("dimensions") or [],
        "filters": _redacted_filters(request.get("filters")),
        "target_reference": _reference(
            request.get("site_url")
            or request.get("url")
            or request.get("property_id")
            or request.get("host_id")
        ),
        "pagination": {
            "returned": result.get("returned", len(rows)),
            "truncated": bool(result.get("truncated")),
        },
        "row_counts": {"returned": result.get("returned", len(rows))},
        "sampling": result.get(
            "sampled", result.get("sampling_or_thresholding", result.get("sampling", "unknown"))
        ),
        "privacy_thresholds": result.get(
            "thresholded", result.get("privacy_thresholds", "unknown")
        ),
        "timezone": result.get("timezone"),
        "attribution": result.get("attribution"),
        "quota_state": result.get("quota_mode") or _REGISTRY[provider]["quota_mode"],
        "status": state,
        "complete": state == "complete",
        "artifact_reference": artifact,
        "redaction": "target identifiers, filter values, and raw provider rows are restricted local artifacts by default",
        "error": result.get("error"),
        "failure_kind": result.get("failure_kind"),
    }


def _permission_failure_state(result: dict[str, Any]) -> str:
    """Map only explicit provider evidence to a permission failure category."""
    error = str(result.get("error") or "").lower().replace("_", " ")
    if "insufficient scope" in error or ("insufficient" in error and "scope" in error):
        return "insufficient_scope"
    if result.get("status") == 401 or "invalid grant" in error or "invalid token" in error:
        return "invalid"
    if result.get("token_status") in {"disabled", "expired"}:
        return "invalid"
    if result.get("status") == 403:
        return "not_granted"
    return "verification_failed"


def provider_verify(
    provider: str, request: dict[str, Any] | None = None, *, transport: Any = None
) -> dict[str, Any]:
    """Perform one declared bounded read when supported; credentials alone stay unverified."""
    if not isinstance(request, dict) and request is not None:
        return {
            "ok": False,
            "provider": provider,
            "state": "invalid",
            "readiness_state": "invalid",
            "permission_state": "invalid",
            "target_access": "not_requested",
            "verified": False,
            "error": "request must be an object",
        }
    request = request or {}
    if not isinstance(provider, str) or provider not in _REGISTRY:
        return {
            "ok": False,
            "provider": provider if isinstance(provider, str) else None,
            "state": "unsupported",
            "readiness_state": "unsupported",
            "permission_state": "unsupported",
            "target_access": "not_requested",
            "verified": False,
            "error": "unsupported provider",
        }
    components, sources = _credential_details(provider)
    readiness = _readiness_state(provider, components, sources)
    if not components:
        return {
            "ok": False,
            "provider": provider,
            "state": "not_required",
            "readiness_state": "not_required",
            "permission_state": "not_required",
            "target_access": "not_requested",
            "verified": False,
            "credential_components": components,
            "credential_sources": sources,
            "note": "this public source has no authenticated-access contract to verify",
        }
    ready = any(components.values()) if provider in {"gsc", "ga4"} else all(components.values())
    if not ready:
        missing = readiness == "missing"
        result = {
            "ok": False,
            "provider": provider,
            "state": "not_configured",
            "readiness_state": readiness,
            "permission_state": "missing" if missing else "invalid",
            "target_access": "not_requested",
            "verified": False,
            "credential_components": components,
            "credential_sources": sources,
        }
        result["error"] = (
            "required credential components are missing"
            if missing
            else "configured credential source is invalid"
        )
        if provider in {"gsc", "ga4"}:
            result["service_account_status"] = credentials.gsc_service_account_status()
        return result
    started = time.monotonic()
    if provider == "gsc":
        from seohead.data_sources.gsc import discover_properties

        result = discover_properties(transport=transport)
    elif provider == "yandex_webmaster":
        from seohead.data_sources.yandex_webmaster import collect

        result = collect("hosts", user_id=request.get("user_id"), transport=transport)
    elif provider == "bing_webmaster":
        from seohead.data_sources.bing_webmaster import collect

        result = collect("sites", site_url=request.get("site_url", ""), transport=transport)
    elif provider == "arsenkin":
        # Balance read (/info limits) is not a billed task; it only reports remaining credits.
        from seohead.data_sources.arsenkin import ArsenkinClient, ArsenkinError

        try:
            balance = ArsenkinClient().limits()
        except (ArsenkinError, ValueError) as exc:
            code = getattr(exc, "code", "invalid_response")
            result = {
                "ok": False,
                "status": int(code) if str(code).isdigit() else None,
                "error": str(code),
            }
        else:
            result = {"ok": True, "balance": balance}
    elif provider == "cloudflare":
        from seohead.data_sources.cloudflare import verify_token

        try:
            token = verify_token(transport=transport)
        except Exception:
            result = {"ok": False, "status": None, "error": "Cloudflare token verification failed"}
        else:
            token_status = token.get("token_status")
            result = {
                "ok": token.get("ok") is True and token_status == "active",
                "status": token.get("status"),
                "error": token.get("error")
                or ("Cloudflare API token is not active" if token_status != "active" else None),
                "token_status": token_status,
                "state": "invalid" if token_status in {"disabled", "expired"} else None,
                "verified_capabilities": ["api_token.active"] if token_status == "active" else [],
                "unverified_capabilities": ["zone and account resource access"],
            }
    elif provider == "yandex_cloud":
        from seohead.data_sources.yandex_cloud import HOST, Wordstat

        client = Wordstat()
        request_tree = transport or client._request
        try:
            status, body = request_tree(
                f"{HOST}/v2/wordstat/getRegionsTree", {"folderId": client.folder}
            )
        except Exception:
            result = {"ok": False, "status": None, "error": "provider connection failed"}
        else:
            valid_tree = (
                isinstance(body, dict)
                and isinstance(body.get("regions"), list)
                and bool(body["regions"])
            )
            result = {
                "ok": status == 200 and valid_tree,
                "status": status,
                "error": None
                if status == 200 and valid_tree
                else "unbilled getRegionsTree capability check failed",
                "quota_state": "not_reported",
                "verified_capabilities": ["wordstat.getRegionsTree"]
                if status == 200 and valid_tree
                else [],
                "unverified_capabilities": [
                    "wordstat.topRequests",
                    "wordstat.dynamics",
                    "web_search",
                ],
            }
    elif provider == "dataforseo_backlinks":
        from seohead.data_sources.dataforseo import DataForSEOClient, DataForSEOError

        try:
            account = DataForSEOClient(env="prod").balance()
        except DataForSEOError as exc:
            result = {
                "ok": False,
                "status": exc.status,
                "error": "DataForSEO account metadata check failed",
            }
        except Exception:
            result = {"ok": False, "status": None, "error": "provider connection failed"}
        else:
            valid_account = (
                isinstance(account, dict)
                and account.get("env") == "prod"
                and isinstance(account.get("limits"), dict)
                and isinstance(account.get("prices"), dict)
            )
            result = {
                "ok": valid_account,
                "error": None if valid_account else "account metadata response was incomplete",
                "quota_state": "reported" if valid_account else "unknown",
                "account_data_available": valid_account,
                "environment": account.get("env") if isinstance(account, dict) else None,
                "verified_capabilities": ["appendix.user_data"] if valid_account else [],
                "unverified_capabilities": ["backlinks subscription and collection access"],
            }
    elif provider in {"ga4", "metrika", "pagespeed", "crux"}:
        operation = {
            "ga4": "landing_pages",
            "metrika": "counters",
            "pagespeed": "mobile_samples",
            "crux": "current",
        }[provider]
        required = {
            "ga4": ("property_id", "start_date", "end_date"),
            "metrika": (),
            "pagespeed": ("urls",),
            "crux": (),
        }[provider]
        missing = any(not request.get(name) for name in required) or (
            provider == "crux" and bool(request.get("url")) == bool(request.get("origin"))
        )
        if missing:
            return {
                "ok": False,
                "provider": provider,
                "state": "verification_input_required",
                "readiness_state": readiness,
                "permission_state": "unknown",
                "authenticated_account": False,
                "target_access": "not_requested",
                "verified": False,
                "required_fields": list(required)
                or (["url or origin"] if provider == "crux" else []),
                "credential_components": components,
                "credential_sources": sources,
                "quota_state": "unknown",
            }
        result = provider_collect(provider, operation, request, transport=transport)
    else:
        return {
            "ok": False,
            "provider": provider,
            "state": "credential_present",
            "readiness_state": readiness,
            "permission_state": "unsupported",
            "target_access": "not_requested",
            "verified": False,
            "credential_components": components,
            "credential_sources": sources,
            "operation_status": "unsupported",
            "note": "this provider has no supported bounded verification route",
        }
    authenticated = bool(result.get("ok"))
    target = request.get("site_url") or request.get("host_id")
    target_access = "not_requested"
    if target:
        candidates: list[Any] = []
        if provider == "gsc":
            properties = result.get("properties")
            candidates = (
                [entry.get("site_url") for entry in properties if isinstance(entry, dict)]
                if isinstance(properties, list)
                else []
            )
        elif provider == "yandex_webmaster":
            data = result.get("data")
            candidates = [
                entry.get("host_id")
                for entry in (data.get("hosts", []) if isinstance(data, dict) else [])
                if isinstance(entry, dict)
            ]
        elif provider == "bing_webmaster":
            data = result.get("data")
            candidates = (
                [entry.get("Url") for entry in data if isinstance(entry, dict)]
                if isinstance(data, list)
                else []
            )
        target_access = (
            "verified" if target in candidates else "not_granted" if candidates else "unknown"
        )
    if authenticated:
        permission_state = (
            "verified"
            if target_access == "verified"
            else "not_granted"
            if target_access == "not_granted"
            else "authenticated_account"
        )
    else:
        permission_state = _permission_failure_state(result)
    return {
        "ok": authenticated,
        "provider": provider,
        "state": "authenticated"
        if authenticated
        else (result.get("state") or "verification_failed"),
        "readiness_state": readiness,
        "permission_state": permission_state,
        "authenticated_account": authenticated,
        "target_access": target_access,
        "verified": target_access == "verified",
        "credential_components": components,
        "credential_sources": sources,
        "operations": _operation_catalog(provider),
        "selected_reference": _reference(target),
        "granted_scopes": result.get("scopes") or "unknown",
        "quota_mode": _REGISTRY[provider]["quota_mode"],
        "quota_state": result.get("quota_state", "unknown"),
        "response_time_ms": round((time.monotonic() - started) * 1000, 1),
        **(
            {"environment": result["environment"]}
            if provider == "dataforseo_backlinks" and "environment" in result
            else {}
        ),
        **({"token_status": result["token_status"]} if "token_status" in result else {}),
        **(
            {
                "verified_capabilities": result["verified_capabilities"],
                "unverified_capabilities": result["unverified_capabilities"],
            }
            if "verified_capabilities" in result
            else {}
        ),
        "contract_compatible": authenticated and target_access in {"verified", "not_requested"},
        "error": result.get("error"),
    }


def provider_collect(
    provider: str,
    operation: str,
    request: dict[str, Any],
    *,
    transport: Any = None,
    artifact_dir: str | Path | None = None,
    event_sink: Any = None,
) -> dict[str, Any]:
    """Explicit provider collection; each dispatch is read-only and may return skipped evidence."""
    if provider not in _REGISTRY or operation not in _REGISTRY[provider]["operations"]:
        raise ValueError("unsupported provider operation")
    operation_contract = _operation_contract(provider, operation)
    if operation_contract["state"] == "dedicated":
        raise ValueError("this provider operation uses its existing dedicated route")
    if operation_contract["state"] == "dedicated_write":
        raise ValueError("this provider operation is a separately confirmed write action")
    if operation_contract["state"] != "supported":
        raise ValueError("unsupported provider operation")
    if not isinstance(request, dict):
        raise ValueError("request must be an object")
    if event_sink is not None:
        event_sink.emit(
            "provider_enrichment",
            {"provider": provider, "operation": operation, "state": "started", "rows": 0},
        )
    if provider == "gsc":
        from seohead.data_sources import gsc

        if operation == "properties":
            result = gsc.discover_properties(transport=transport)
        elif operation == "search_analytics":
            result = gsc.search_analytics_pages(transport=transport, **request)
        elif operation == "inspection":
            result = gsc.inspect_urls(transport=transport, **request)
        elif operation == "sitemaps":
            result = gsc.sitemap_status(transport=transport, **request)
        else:
            result = provider_verify("gsc", request, transport=transport)
    elif provider == "crux":
        from seohead.data_sources import crux

        result = (crux.history if operation == "history" else crux.query)(
            fetcher=transport, **request
        )
    elif provider == "pagespeed":
        from seohead.data_sources import pagespeed

        result = pagespeed.sample(
            desktop=operation == "desktop_samples", fetcher=transport, **request
        )
    elif provider == "ga4":
        from seohead.data_sources import ga4

        if operation == "page_views":
            from seohead.data_sources.ga4_content import page_views

            result = page_views(transport=transport, **request)
        else:
            result = ga4.landing_pages(transport=transport, **request)
    elif provider == "yandex_webmaster":
        from seohead.data_sources import yandex_webmaster

        result = yandex_webmaster.collect(operation, transport=transport, **request)
    elif provider == "bing_webmaster":
        from seohead.data_sources import bing_webmaster

        result = bing_webmaster.collect(operation, transport=transport, **request)
    elif provider == "dataforseo_backlinks":
        from seohead.data_sources import dataforseo

        result = dataforseo.backlink_call(operation, **request)
    elif provider == "wayback":
        from seohead.data_sources import wayback

        result = wayback.history(fetcher=transport, **request)
    elif provider == "crtsh":
        from seohead.data_sources import crtsh

        result = crtsh.subdomains(fetcher=transport, **request)
    elif provider == "cloudflare":
        from seohead.data_sources import cloudflare, cloudflare_history

        if operation == "traffic":
            result = cloudflare.traffic(transport=transport, **request)
        elif operation == "logpull":
            raw_transport = (
                (lambda url, token: transport("GET", url, None, token))
                if transport is not None
                else None
            )
            result = cloudflare.logpull(transport=transport, raw_transport=raw_transport, **request)
        else:
            result = cloudflare_history.list_snapshots(**request)
    elif provider == "metrika":
        if operation not in {"counters", "aggregate_report"}:
            raise ValueError("Metrika raw Logs API is intentionally unreachable")
        from seohead.data_sources.credentials import MissingCredential
        from seohead.data_sources.metrika import MetrikaClient, MetrikaError, rows_to_records

        try:
            client = MetrikaClient()
            if operation == "counters":
                counters = client.counters()
                result = {
                    "ok": True,
                    "state": "complete",
                    "returned": len(counters),
                    "rows": counters,
                }
            else:
                required = {"counter_id", "metrics", "date1", "date2"}
                if not required <= set(request):
                    raise ValueError(
                        "aggregate_report requires counter_id, metrics, date1, and date2"
                    )
                body = client.report(
                    {
                        "ids": request["counter_id"],
                        "metrics": request["metrics"],
                        "date1": request["date1"],
                        "date2": request["date2"],
                        **(
                            {"dimensions": request["dimensions"]}
                            if request.get("dimensions")
                            else {}
                        ),
                    },
                    paginate=bool(request.get("paginate")),
                    limit=int(request.get("limit", 100)),
                )
                result = {
                    "ok": True,
                    "state": "partial"
                    if body.get("capped") or body.get("incomplete")
                    else "complete",
                    "period": {"start_date": request["date1"], "end_date": request["date2"]},
                    "rows": rows_to_records(body),
                    "returned": len(body.get("data") or []),
                    "truncated": bool(body.get("capped")),
                    "incomplete": bool(body.get("incomplete")),
                    # ``sampled`` is three-state: a body that does not report it stays
                    # ``None`` — unknown must not read as "not sampled".
                    "sampled": body.get("sampled"),
                    "sample_share": body.get("sample_share"),
                    "accuracy": body.get("accuracy_used")
                    or (body.get("query") or {}).get("accuracy"),
                    "split": body.get("split"),
                }
        except MissingCredential as exc:
            result = {"ok": False, "state": "not_configured", "error": str(exc)}
        except MetrikaError as exc:
            result = {"ok": False, "state": "failed", "error": exc.message, "status": exc.status}
    else:  # pragma: no cover - registry and dispatch stay synchronized above.
        raise ValueError("unsupported provider operation")
    evidence = _evidence(provider, operation, request, result, None)
    artifact = (
        _save_local_artifact(artifact_dir, {"evidence": evidence, "result": result})
        if artifact_dir
        else None
    )
    evidence["artifact_reference"] = artifact
    if event_sink is not None:
        rows = (
            result.get("rows")
            or result.get("samples")
            or result.get("url_facts")
            or result.get("summary")
            or []
        )
        event_sink.emit(
            "provider_enrichment",
            {
                "provider": provider,
                "operation": operation,
                "state": str(result.get("state") or "unknown"),
                "rows": int(result.get("returned", len(rows))),
            },
        )
    return {"evidence": evidence, "result": None}


def provider_join(
    crawl_pages: list[dict[str, Any]],
    evidence_rows: list[dict[str, Any]],
    *,
    url_field: str = "url",
    url_column: str = "url",
    review_external_only: bool = False,
    adjustments: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    """Reproducibly join evidence; no external URL enters a crawl without explicit review."""
    joined = join_external_data(
        crawl_pages, evidence_rows, url_field=url_field, url_column=url_column
    )
    candidates = orphan_urls(joined, url_column=url_column) if review_external_only else []
    applied = []
    for rule in adjustments or []:
        required = {"source_fields", "period", "coverage", "adjustment"}
        if not isinstance(rule, dict) or not required <= set(rule):
            raise ValueError(
                "priority adjustment requires source fields, period, coverage, and adjustment"
            )
        if (
            not isinstance(rule["source_fields"], list)
            or not rule["source_fields"]
            or not all(isinstance(name, str) and name for name in rule["source_fields"])
        ):
            raise ValueError("priority adjustment requires named source fields")
        if rule["coverage"] not in {
            "complete",
            "partial",
            "sampled",
            "truncated",
            "unmatched",
            "privacy_thresholded",
        }:
            raise ValueError("priority adjustment has unsupported coverage")
        if rule["coverage"] != "complete":
            applied.append(
                {"applied": False, "reason": "prioritization evidence unavailable", "rule": rule}
            )
        else:
            matches = [
                entry
                for entry in joined["joined"]
                if all(
                    field in entry["external"] and entry["external"][field] is not None
                    for field in rule["source_fields"]
                )
            ]
            if not matches:
                applied.append(
                    {"applied": False, "reason": "no matched source-backed rows", "rule": rule}
                )
                continue
            applied.extend(
                {
                    "applied": True,
                    "url": entry["url"],
                    "adjustment": rule["adjustment"],
                    "source_fields": rule["source_fields"],
                    "period": rule["period"],
                    "technical_severity": entry["page"].get("severity"),
                    "technical_severity_changed": False,
                }
                for entry in matches
            )
    return {
        "format": "seohead.provider-join.v1",
        "join": joined,
        "list_crawl_candidates": candidates,
        "frontier_mutated": False,
        "priority_adjustments": applied,
    }


def provider_replay(
    input_path: str,
    evidence_file: str,
    out_dir: str,
    *,
    url_column: str = "url",
    review_external_only: bool = False,
) -> dict[str, Any]:
    """Join one restricted saved provider result to a saved scan, without network.

    Raw joins remain in a private local file. Returned counts identify unmatched
    populations without exposing URLs, queries, or property identifiers.
    """
    from seohead.storage import open_scan

    source = Path(evidence_file)
    if (
        source.is_symlink()
        or not source.is_file()
        or not credentials.is_private_mode(source.stat().st_mode)
        or source.stat().st_size > 16 * 1024 * 1024
    ):
        raise ValueError("evidence_file must be a private bounded regular JSON file")
    stored_bytes = source.read_bytes()
    stored = json.loads(stored_bytes)
    if not isinstance(stored, dict) or set(stored) != {"evidence", "result"}:
        raise ValueError("expected a saved provider collection envelope")
    evidence, result = stored["evidence"], stored["result"]
    if (
        not isinstance(evidence, dict)
        or evidence.get("format") != EVIDENCE_FORMAT
        or not isinstance(result, dict)
    ):
        raise ValueError("invalid saved provider evidence")
    rows = result.get("rows")
    if (
        not isinstance(rows, list)
        or len(rows) > 100_000
        or any(not isinstance(row, dict) for row in rows)
    ):
        raise ValueError("saved provider operation does not contain bounded joinable rows")
    dimensions = result.get("dimensions", [])
    if evidence.get("provider") == "gsc" and "page" in dimensions:
        index = dimensions.index("page")
        rows = [
            {
                **row,
                "url": row["keys"][index]
                if isinstance(row.get("keys"), list) and len(row["keys"]) > index
                else None,
            }
            for row in rows
        ]
        url_column = "url"
    con = open_scan(input_path, require_audit=False)
    try:
        if con.execute("SELECT COUNT(*) FROM pages").fetchone()[0] > 100_000:
            raise ValueError("saved provider join exceeds the page bound")
        pages = [
            dict(row)
            for row in con.execute(
                "SELECT u.url,p.status_code FROM pages p JOIN urls u USING(url_id) ORDER BY p.url_id"
            )
        ]
        scan_uuid = con.execute("SELECT scan_uuid FROM scan").fetchone()[0]
    finally:
        con.close()
    joined = provider_join(
        pages, rows, url_column=url_column, review_external_only=review_external_only
    )
    joined["source"] = {
        "scan_uuid": scan_uuid,
        "provider_evidence_sha256": hashlib.sha256(stored_bytes).hexdigest(),
        "provider": evidence.get("provider"),
        "period": evidence.get("period"),
        "status": evidence.get("status"),
        "sampling": evidence.get("sampling"),
    }
    reference = _save_local_artifact(out_dir, joined)
    return {
        "ok": True,
        "format": "seohead.provider-replay.v1",
        "counts": joined["join"]["summary"],
        "artifact_reference": reference,
        "provider_status": evidence.get("status"),
        "frontier_mutated": False,
        "priority_applied": False,
    }
