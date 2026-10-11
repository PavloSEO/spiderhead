"""Pure, descriptor-backed native crawl drafts for the Desktop adapter.

No I/O, Qt, core imports, credential resolution, or process dispatch belongs here.
The descriptor supplies known paths/types/defaults. This module adds conservative
Desktop admission checks for its supported subset; the core remains the final
configuration validator because describe-settings does not expose a full schema.
"""

from __future__ import annotations

import math
import re
from collections.abc import Mapping
from copy import deepcopy

# Explicit controls are intentional: a newly described core path must not silently
# become an editable, executable capability in an older Desktop build.
_BOOLEAN_PATHS = {
    "cache.invalidate",
    "discovery.external.store",
    "discovery.follow_nofollow",
    "discovery.hyperlinks.crawl",
    "discovery.hyperlinks.store",
    "discovery.redirects.crawl",
    "discovery.resolve_canonical_destination",
    "discovery.resolve_redirect_destination",
    "link_attributes.capture",
    "link_position.classify",
    "resources.fetch",
    "robots.unavailable_means_stop",
    "sitemaps.auto_discover",
    "speed.adaptive",
    "rendering.artifacts.console_errors",
    "rendering.artifacts.screenshots",
    "rendering.browser.flatten_iframes",
    "rendering.browser.flatten_shadow_dom",
    "rendering.browser.mobile_emulation",
    "rendering.browser.resize_to_content",
    "rendering.browser.touch_emulation",
    "rendering.rendered_links.crawl",
    "rendering.rendered_links.store",
}
_LIST_PATHS = {
    "scope.exclude_extensions",
    "scope.include_extensions",
    "scope.exclude_hosts",
    "scope.exclude_media_types",
    "scope.include_media_types",
    "scope.exclude_patterns",
    "scope.include_patterns",
    "evidence.content_area.exclude_selectors",
    "evidence.content_area.exclude_tags",
}
_STRING_PATHS = {
    "http.user_agent",
    "robots.user_agent_token",
    "evidence.content_area.include_selector",
    "evidence.content_area.root_selector",
}
_CHOICES = {
    "cache.mode": ("off", "live", "replay"),
    "scope.internal": ("host", "registrable_domain"),
    "robots.policy": ("respect", "report_only", "ignore"),
    "rendering.mode": ("raw", "js"),
    "rendering.browser.engine": ("chromium", "firefox", "webkit"),
    "rendering.browser.viewport": ("desktop", "mobile"),
    "rendering.browser.wait_until": ("load", "domcontentloaded", "networkidle"),
    "rendering.escalation.policy": ("sampled", "full"),
    "storage.body_mode": ("off", "captured_entity_bytes"),
}
# Match the native SQLite core: zero disables user population/time budgets.
_INTEGER_BOUNDS = {
    "http.retry_on_timeout": (0, None),
    "limits.max_urls": (0, 1_000_000),
    "limits.max_requests": (0, 2_000_000),
    "limits.max_crawl_seconds": (0, None),
    "limits.max_depth": (-1, None),
    "limits.max_query_variants_per_path": (0, None),
    "limits.max_response_bytes": (1, None),
    "limits.max_url_length": (1, None),
    "speed.concurrency": (1, None),
    "speed.stop_after_consecutive_timeouts": (1, None),
    "rendering.browser.page_concurrency": (1, 16),
    "rendering.browser.viewport_width": (0, 16_384),
    "rendering.browser.viewport_height": (0, 16_384),
    "rendering.browser.resize_to_content_max_height_px": (1, None),
    "rendering.escalation.max_render_seconds": (0, None),
    "rendering.escalation.max_render_urls": (0, 50_000),
    "rendering.escalation.sample_per_pattern": (1, None),
    "resources.max_requests": (1, 2_000_000),
    "resources.max_response_bytes": (1, None),
    "storage.max_body_bytes": (1, None),
    "storage.max_body_store_bytes": (1, None),
    "storage.history_warning_bytes": (1, None),
    "storage.min_free_bytes": (10 * 1024**3, None),
}
_FLOAT_BOUNDS = {
    "http.timeout_seconds": (0, None),
    "speed.min_delay_seconds": (0, None),
    "speed.max_delay_seconds": (0.5, None),
    "rendering.browser.device_pixel_ratio": (0, None),
    "rendering.browser.script_timeout_seconds": (0, None),
}
_EXPECTED_TYPES = {
    **dict.fromkeys(_BOOLEAN_PATHS, "bool"),
    **dict.fromkeys(_LIST_PATHS, "list"),
    **dict.fromkeys(_STRING_PATHS | _CHOICES.keys(), "str"),
    **dict.fromkeys(_INTEGER_BOUNDS, "int"),
    **dict.fromkeys(_FLOAT_BOUNDS, "float"),
}
_LIST_ONLY = {
    "discovery.resolve_canonical_destination",
    "discovery.resolve_redirect_destination",
}
_SQLITE_ONLY = {
    "resources.fetch",
    "resources.max_requests",
    "resources.max_response_bytes",
}
_SECRET_PREFIXES = (
    "http.credential",
    "http.proxy",
    "rendering.browser.remote_",
    "rendering.browser.persistent_",
)


def _catalogue(descriptor):
    if not isinstance(descriptor, Mapping) or not isinstance(
        descriptor.get("settings"), list
    ):
        raise TypeError("crawl-describe-settings must return a settings list")
    result = {}
    for item in descriptor["settings"]:
        if not isinstance(item, Mapping) or not isinstance(item.get("path"), str):
            raise TypeError("Invalid crawl setting descriptor")
        path = item["path"]
        if path in result:
            raise ValueError(f"Duplicate setting descriptor: {path}")
        if (
            item.get("type") not in {"str", "bool", "int", "float", "list", "dict"}
            or "default" not in item
        ):
            raise ValueError(f"Invalid setting type/default: {path}")
        result[path] = item
    return result


def _unavailable_reason(path, item, input_mode, storage_backend):
    if path.startswith(_SECRET_PREFIXES) or path == "http.headers":
        return "Credential, proxy, remote-browser and header configuration uses a separate trusted workflow"
    if path not in _EXPECTED_TYPES:
        return "Known core setting; no validated Desktop editor is connected"
    if item["type"] != _EXPECTED_TYPES[path]:
        return "Core setting type changed; this Desktop editor must be updated"
    if path in _LIST_ONLY and input_mode != "list":
        return "Only available for an explicit URL list; not a site crawl"
    if (
        path in _SQLITE_ONLY or path.startswith("storage.")
    ) and storage_backend != "sqlite":
        return "Requires the native retained SQLite route"
    return ""


def describe_controls(descriptor, *, input_mode="site", storage_backend="sqlite"):
    """Return all described fields, including truthful unavailable editor states."""
    if input_mode not in {"site", "list"} or storage_backend not in {"sqlite", "json"}:
        raise ValueError("Unknown native crawl context")
    rows = []
    for path, item in _catalogue(descriptor).items():
        reason = _unavailable_reason(path, item, input_mode, storage_backend)
        row = {
            "path": path,
            "group": path.split(".", 1)[0],
            "type": item["type"],
            "description": str(item.get("description", "")),
            "results_affecting": item.get("results_affecting") is True,
            "default": None
            if path.startswith(_SECRET_PREFIXES) or path == "http.headers"
            else deepcopy(item["default"]),
            "editable": not reason,
            "unavailable_reason": reason,
        }
        if path in _CHOICES:
            row["choices"] = _CHOICES[path]
        if path in _INTEGER_BOUNDS or path in _FLOAT_BOUNDS:
            row["minimum"], row["maximum"] = (_INTEGER_BOUNDS | _FLOAT_BOUNDS)[path]
        rows.append(row)
    return rows


def _validate_value(path, value, expected):
    if expected == "float":
        if type(value) not in {int, float}:
            raise ValueError(f"{path}: expected a finite number")
        try:
            value = float(value)
        except OverflowError:
            raise ValueError(f"{path}: expected a finite number") from None
        if not math.isfinite(value):
            raise ValueError(f"{path}: expected a finite number")
    elif type(value).__name__ != expected:
        raise ValueError(f"{path}: expected {expected}")
    if expected == "int" and value > 2**63 - 1:
        raise ValueError(f"{path}: outside Desktop integer range")
    if path in _CHOICES and value not in _CHOICES[path]:
        raise ValueError(f"{path}: unsupported choice")
    bounds = (_INTEGER_BOUNDS | _FLOAT_BOUNDS).get(path)
    if bounds:
        minimum, maximum = bounds
        if value < minimum or (maximum is not None and value > maximum):
            raise ValueError(f"{path}: outside Desktop admission bounds")
        if (
            path in {"http.timeout_seconds", "rendering.browser.device_pixel_ratio"}
            and value == 0
        ):
            raise ValueError(f"{path}: must be positive")
    if expected == "str" and (
        len(value) > 4096 or any(char in value for char in "\r\n\x00")
    ):
        raise ValueError(f"{path}: requires a bounded single-line value")
    if expected == "list":
        if len(value) > 256 or any(
            type(v) is not str or not v or len(v) > 4096 or "\x00" in v for v in value
        ):
            raise ValueError(f"{path}: requires at most 256 non-empty bounded strings")
        if path.endswith("_patterns"):
            for pattern in value:
                try:
                    re.compile(pattern)
                except re.error:
                    raise ValueError(f"{path}: invalid regex") from None
        if path.endswith("_extensions"):
            normalized = [v.removeprefix(".").lower() for v in value]
            if any(not re.fullmatch(r"[a-z0-9][a-z0-9_-]*", v) for v in normalized):
                raise ValueError(f"{path}: invalid filename suffix")
            if len(set(normalized)) != len(normalized):
                raise ValueError(f"{path}: duplicate normalized suffix")
        if path.endswith("_media_types"):
            token = r"[A-Za-z0-9!#$%&'+\-.^_`|~]+"
            if any(not re.fullmatch(rf"{token}/(?:{token}|\*)", v) for v in value):
                raise ValueError(f"{path}: invalid media type or type wildcard")
            if len({v.lower() for v in value}) != len(value):
                raise ValueError(f"{path}: duplicate normalized media type")
    return deepcopy(value)


def validate_overrides(
    descriptor,
    overrides,
    *,
    base_values=None,
    input_mode="site",
    storage_backend="sqlite",
):
    """Validate a draft atomically and return explicit overrides, never defaults.

    base_values is an optional dotted, nonsecret subset supplied by the caller's
    trusted project projection. It is used only for cross-field checks and is
    never serialized into the returned overrides. This is not a core dry run.
    """
    if not isinstance(overrides, Mapping) or (
        base_values is not None and not isinstance(base_values, Mapping)
    ):
        raise ValueError("Crawl overrides and base values must be mappings")
    fields = {
        row["path"]: row
        for row in describe_controls(
            descriptor, input_mode=input_mode, storage_backend=storage_backend
        )
    }
    values = {
        path: deepcopy(row["default"])
        for path, row in fields.items()
        if row["editable"]
    }
    validated = {}
    for incoming, is_override in ((base_values or {}, False), (overrides, True)):
        for path, value in incoming.items():
            if not isinstance(path, str) or path not in fields:
                raise ValueError("Unknown crawl setting path")
            row = fields[path]
            if not row["editable"]:
                raise ValueError(f"{path}: {row['unavailable_reason']}")
            checked = _validate_value(path, value, row["type"])
            values[path] = checked
            if is_override:
                validated[path] = deepcopy(checked)

    if values.get("cache.mode") == "replay" and values.get("cache.invalidate"):
        raise ValueError("cache.invalidate cannot be combined with replay mode")
    low, high = (
        values.get("speed.min_delay_seconds"),
        values.get("speed.max_delay_seconds"),
    )
    if low is not None and high is not None and low > high:
        raise ValueError("speed.max_delay_seconds must cover the minimum delay")
    width, height = (
        values.get("rendering.browser.viewport_width", 0),
        values.get("rendering.browser.viewport_height", 0),
    )
    if bool(width) != bool(height):
        raise ValueError(
            "rendering.browser viewport width and height must be set together"
        )
    if values.get("rendering.browser.engine") == "firefox" and values.get(
        "rendering.browser.mobile_emulation"
    ):
        raise ValueError(
            "rendering.browser.engine firefox cannot enable mobile_emulation"
        )
    if values.get("rendering.rendered_links.crawl") and not values.get(
        "rendering.rendered_links.store"
    ):
        raise ValueError(
            "rendering.rendered_links.crawl requires retained route evidence"
        )
    if values.get("resources.fetch") and storage_backend != "sqlite":
        raise ValueError("resources.fetch requires native SQLite retention")
    return validated


def preview_configuration(descriptor, overrides, **context):
    """Build a reviewable data-only draft; no filesystem/profile write occurs."""
    validated = validate_overrides(descriptor, overrides, **context)
    controls = {
        row["path"]: row
        for row in describe_controls(
            descriptor,
            input_mode=context.get("input_mode", "site"),
            storage_backend=context.get("storage_backend", "sqlite"),
        )
    }
    return {
        "state": "draft",
        "requires_core_validation": True,
        "overrides": validated,
        "changes": [
            {
                "path": path,
                "value": deepcopy(value),
                "results_affecting": controls[path]["results_affecting"],
                "description": controls[path]["description"],
            }
            for path, value in sorted(validated.items())
        ],
        "limitations": [
            "Final project/file/environment precedence and core validation happen at explicit launch",
            "JS engine availability, retained field coverage and renderer budgets are runtime evidence",
            "Cache replay may fetch a URL that has no cache entry; it is not offline reanalysis",
        ],
    }
