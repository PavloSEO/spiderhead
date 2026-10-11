"""Transaction-scoped writer for bounded captured transport observations."""

from __future__ import annotations

import hashlib
import json
import math
import sqlite3
from datetime import datetime
from email.message import Message
from typing import Any

from seohead.crawl.cache import parse_cache_control
from seohead.crawl.capture import CaptureEvent
from seohead.crawl.settings import MAX_RENDER_PAGE_CONCURRENCY

from . import ScanError
from .bodies import decode_entity, encode_body
from .retention import validate_policy

_PURPOSES = {"page", "script", "stylesheet", "robots", "sitemap"}
_UNAVAILABLE = {"not_fetched", "not_in_corpus", "legacy_not_retained", "fetch_failed"}
_OMITTED = {
    "not_enabled",
    "cache_control_no_store",
    "credentialed",
    "unsupported_media",
    "body_budget_exhausted",
    "resource_budget_exhausted",
}
_REDIRECT_KEYS = {"request_url", "status_code", "location_raw", "next_url", "blocked"}


def _text(value: Any, label: str, *, nonempty: bool = False) -> str:
    if type(value) is not str or (nonempty and not value):
        raise ScanError(f"captured {label} must be a{' nonempty' if nonempty else ''} string")
    return value


def _timestamp(value: Any, label: str) -> str:
    value = _text(value, label, nonempty=True)
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as exc:
        raise ScanError(f"captured {label} must be an RFC3339 timestamp") from exc
    if parsed.tzinfo is None or parsed.utcoffset() is None:
        raise ScanError(f"captured {label} must include an offset")
    return value


def _status(value: Any, label: str, *, nullable: bool = True) -> int | None:
    if value is None and nullable:
        return None
    if type(value) is not int or not 100 <= value <= 599:
        raise ScanError(f"captured {label} must be an HTTP status code")
    return value


def _intern_url(con: sqlite3.Connection, value: str) -> int:
    _text(value, "response URL", nonempty=True)
    con.execute("INSERT OR IGNORE INTO urls(url) VALUES(?)", (value,))
    row = con.execute("SELECT url_id FROM urls WHERE url=?", (value,)).fetchone()
    if row is None:
        raise ScanError("captured response URL could not be interned")
    return int(row[0])


def _checked_pairs(pairs: Any, label: str) -> tuple[tuple[str, str], ...]:
    sensitive = {"authorization", "cookie", "set-cookie", "proxy-authorization", "x-api-key"}
    if not isinstance(pairs, tuple) or any(
        not isinstance(item, tuple)
        or len(item) != 2
        or any(type(part) is not str for part in item)
        or item[0].lower() in sensitive
        for item in pairs
    ):
        raise ScanError(f"captured {label} headers must be redacted string pairs")
    return pairs


def _pairs(pairs: tuple[tuple[str, str], ...], label: str) -> str:
    _checked_pairs(pairs, label)
    return json.dumps([list(item) for item in pairs], ensure_ascii=False, separators=(",", ":"))


def _no_store(pairs: tuple[tuple[str, str], ...]) -> bool:
    return any(
        name.lower() == "cache-control" and "no-store" in parse_cache_control(value)
        for name, value in pairs
    )


def _content_length(pairs: tuple[tuple[str, str], ...]) -> int | None:
    values = [value.strip() for name, value in pairs if name.lower() == "content-length"]
    if not values or any(not value.isascii() or not value.isdigit() for value in values):
        return None
    parsed = {int(value) for value in values}
    return parsed.pop() if len(parsed) == 1 else None


def _declared_charset(content_type: str) -> str:
    message = Message()
    message["content-type"] = content_type
    charset = message.get_content_charset()
    return charset if type(charset) is str else ""


def _validate_event(event: Any) -> CaptureEvent:
    if not isinstance(event, CaptureEvent):
        raise ScanError("corpus writer requires a CaptureEvent")
    _text(event.method, "method", nonempty=True)
    _text(event.requested_url, "requested URL", nonempty=True)
    _text(event.effective_url, "effective URL")
    _timestamp(event.requested_at, "requested_at")
    _timestamp(event.received_at, "received_at")
    _status(event.status_code, "status")
    _status(event.effective_status_code, "effective status")
    _checked_pairs(event.request_headers, "request")
    _checked_pairs(event.response_headers, "response")
    _checked_pairs(event.effective_headers, "effective")
    if type(event.credentials_used) is not bool:
        raise ScanError("captured credentials flag must be boolean")
    for name in (
        "content_type",
        "content_encoding",
        "body_fidelity",
        "body_state",
        "body_reason",
        "error",
        "error_kind",
    ):
        _text(getattr(event, name), name)
    if event.entity_bytes is not None and type(event.entity_bytes) is not bytes:
        raise ScanError("captured entity must be bytes or unavailable")
    if event.response_time is not None and (
        type(event.response_time) not in {int, float}
        or not math.isfinite(event.response_time)
        or event.response_time < 0
    ):
        raise ScanError("captured response time must be a finite nonnegative number")
    if event.http_version is not None and (
        not isinstance(event.http_version, str) or len(event.http_version) > 32
    ):
        raise ScanError("captured HTTP protocol is invalid")
    if event.timing_state not in {"partial", "unavailable"}:
        raise ScanError("captured timing state is invalid")
    if not isinstance(event.redirect_history, tuple):
        raise ScanError("captured redirect history must be a tuple")
    for hop in event.redirect_history:
        if not isinstance(hop, dict) or set(hop) != _REDIRECT_KEYS:
            raise ScanError("captured redirect history has invalid fields")
        _text(hop["request_url"], "redirect request URL", nonempty=True)
        _status(hop["status_code"], "redirect status", nullable=False)
        _text(hop["location_raw"], "redirect location")
        if type(hop["blocked"]) is not bool:
            raise ScanError("captured redirect blocked flag must be boolean")
        if hop["blocked"]:
            if hop["next_url"] is not None:
                raise ScanError("blocked redirect must not name a next URL")
        else:
            _text(hop["next_url"], "redirect next URL", nonempty=True)
    return event


def _state(
    event: CaptureEvent,
    purpose: str,
    policy: dict[str, Any],
    effective_headers: tuple[tuple[str, str], ...],
    *,
    retain_no_store: bool = False,
) -> tuple[str, str]:
    if event.body_state == "truncated" or event.body_reason == "truncated":
        return "truncated", "truncated"
    if event.body_state == "omitted" and event.body_reason in _OMITTED:
        return "omitted", event.body_reason
    if event.body_state != "complete" or event.entity_bytes is None:
        reason = event.body_reason if event.body_reason in _UNAVAILABLE else "fetch_failed"
        return "unavailable", reason
    if event.credentials_used:
        return "omitted", "credentialed"
    if (_no_store(event.response_headers) or _no_store(effective_headers)) and not retain_no_store:
        return "omitted", "cache_control_no_store"
    if policy["body_mode"] == "off":
        return "omitted", "not_enabled"
    if purpose == "page" and "html" not in event.content_type.lower():
        return "omitted", "unsupported_media"
    if purpose in {"script", "stylesheet"}:
        from .resources import media_matches

        if not media_matches(purpose, event.content_type):
            return "omitted", "unsupported_media"
    if len(event.entity_bytes) > policy["max_body_bytes"]:
        return "omitted", "body_budget_exhausted"
    return "complete", "none"


def _redirect_chain(con: sqlite3.Connection, history: tuple[dict[str, Any], ...]) -> str:
    rows = []
    for hop in history:
        next_url = hop["next_url"]
        rows.append(
            {
                "request_url_id": _intern_url(con, hop["request_url"]),
                "status_code": hop["status_code"],
                "location_raw": hop["location_raw"],
                "next_url_id": None if next_url is None else _intern_url(con, next_url),
                "blocked": hop["blocked"],
            }
        )
    return json.dumps(rows, ensure_ascii=False, separators=(",", ":"))


def _retained_body_bytes(con: sqlite3.Connection) -> int:
    """Count once per writer connection, then follow transactional body changes.

    TEMP state preserves the durable scan format and rolls back with its writes.
    Reopened connections rebuild from retained evidence before enforcing budgets.
    """
    if not con.execute(
        "SELECT 1 FROM sqlite_temp_master WHERE name='native_body_store_bytes'"
    ).fetchone():
        con.execute("SAVEPOINT native_body_bytes_init")
        try:
            con.execute("CREATE TEMP TABLE native_body_store_bytes(value INTEGER NOT NULL)")
            con.execute(
                "INSERT INTO native_body_store_bytes SELECT COALESCE(SUM(stored_bytes),0) FROM bodies"
            )
            for action, delta in (
                ("INSERT", "NEW.stored_bytes"),
                ("DELETE", "-OLD.stored_bytes"),
                ("UPDATE OF stored_bytes", "NEW.stored_bytes-OLD.stored_bytes"),
            ):
                name = action.split()[0].lower()
                con.execute(
                    f"CREATE TEMP TRIGGER native_body_bytes_{name} AFTER {action} ON main.bodies "
                    f"BEGIN UPDATE native_body_store_bytes SET value=value+({delta}); END"
                )
        except BaseException:
            con.execute("ROLLBACK TO native_body_bytes_init")
            raise
        finally:
            con.execute("RELEASE native_body_bytes_init")
    return con.execute("SELECT value FROM temp.native_body_store_bytes").fetchone()[0]


def _reserve_unique_bytes(
    con: sqlite3.Connection, encoded: dict[str, object], policy: dict[str, Any]
) -> bool:
    existing = con.execute("SELECT 1 FROM bodies WHERE sha256=?", (encoded["sha256"],)).fetchone()
    if existing is not None:
        return True
    budget = policy["max_body_store_bytes"]
    stored_bytes = int(encoded["stored_bytes"])
    if stored_bytes and (_retained_body_bytes(con) + stored_bytes > budget):
        return False
    con.execute(
        "INSERT INTO bodies(sha256,codec,decoded_bytes,stored_bytes,data) VALUES(?,?,?,?,?)",
        (
            encoded["sha256"],
            encoded["codec"],
            encoded["decoded_bytes"],
            encoded["stored_bytes"],
            encoded["data"],
        ),
    )
    return True


def _renderer_bool(value: Any, label: str) -> bool:
    if type(value) is not bool:
        raise ScanError(f"rendered {label} must be boolean")
    return value


def _renderer_count(value: Any, label: str) -> int:
    if type(value) is not int or value < 0:
        raise ScanError(f"rendered {label} must be a nonnegative integer")
    return value


def _rendered_provenance(
    con: sqlite3.Connection, renderer: Any, *, navigation_transform: str = "direct"
) -> tuple[str, bool, bool]:
    """Translate the safe render adjunct result into the schema's provenance shape."""
    if type(renderer) is not dict:
        raise ScanError("rendered document provenance must be an object")
    if navigation_transform not in {"direct", "legacy_escaped_fragment"}:
        raise ScanError("rendered navigation transform is unsupported")
    supplied_transform = renderer.get("navigation_transform")
    if supplied_transform is not None and supplied_transform != navigation_transform:
        raise ScanError("rendered navigation transform disagrees with representation")
    if navigation_transform == "legacy_escaped_fragment" and supplied_transform is None:
        raise ScanError("legacy-fragment renderer must declare its navigation transform")
    engine = _text(renderer.get("engine"), "renderer engine", nonempty=True)
    version = _text(renderer.get("engine_version"), "renderer version", nonempty=True)
    raw_settings = renderer.get("settings")
    raw_navigation = renderer.get("navigation")
    raw_transforms = renderer.get("transforms")
    raw_policy = renderer.get("policy", {})
    if type(raw_settings) is not dict or type(raw_navigation) is not dict:
        raise ScanError("rendered document lacks settings or navigation provenance")
    if type(raw_transforms) is not dict or type(raw_policy) is not dict:
        raise ScanError("rendered document lacks transform or policy provenance")
    page_concurrency = renderer.get("page_concurrency", 1)
    if (
        type(page_concurrency) is not int
        or not 1 <= page_concurrency <= MAX_RENDER_PAGE_CONCURRENCY
    ):
        raise ScanError(
            "rendered page_concurrency must be an integer between 1 and "
            f"{MAX_RENDER_PAGE_CONCURRENCY}"
        )

    viewport = raw_settings.get("viewport")
    if (
        type(viewport) is not dict
        or set(viewport) != {"width", "height"}
        or any(type(value) is not int or value < 1 for value in viewport.values())
    ):
        raise ScanError("rendered viewport provenance is invalid")
    device_pixel_ratio = raw_settings.get("device_pixel_ratio")
    if (
        type(device_pixel_ratio) not in {int, float}
        or not math.isfinite(device_pixel_ratio)
        or device_pixel_ratio <= 0
    ):
        raise ScanError("rendered device-pixel-ratio provenance is invalid")
    script_timeout = raw_settings.get("script_timeout_seconds")
    resize_cap = raw_settings.get("resize_to_content_max_height_px")
    if (
        type(script_timeout) not in {int, float}
        or not math.isfinite(script_timeout)
        or script_timeout < 0
        or type(resize_cap) is not int
        or resize_cap < 1
    ):
        raise ScanError("rendered timing or resize provenance is invalid")
    settings = {
        "viewport": {"width": viewport["width"], "height": viewport["height"]},
        "device_pixel_ratio": device_pixel_ratio,
        "mobile_emulation": _renderer_bool(
            raw_settings.get("mobile_emulation"), "mobile-emulation setting"
        ),
        "touch_emulation": _renderer_bool(
            raw_settings.get("touch_emulation"), "touch-emulation setting"
        ),
        "script_timeout_seconds": script_timeout,
        "resize_to_content": _renderer_bool(
            raw_settings.get("resize_to_content"), "resize setting"
        ),
        "resize_to_content_max_height_px": resize_cap,
        "persistent_profile": _renderer_bool(
            raw_settings.get("persistent_profile"), "persistent-profile setting"
        ),
        "transforms": {
            "flatten_shadow_dom_requested": _renderer_bool(
                raw_transforms.get("flatten_shadow_dom_requested"), "shadow-DOM request"
            ),
            "flatten_shadow_dom_applied": _renderer_count(
                raw_transforms.get("flatten_shadow_dom_applied"), "shadow-DOM count"
            ),
            "flatten_iframes_requested": _renderer_bool(
                raw_transforms.get("flatten_iframes_requested"), "iframe request"
            ),
            "flatten_iframes_applied": _renderer_count(
                raw_transforms.get("flatten_iframes_applied"), "iframe count"
            ),
        },
    }
    requested_url = _text(
        raw_navigation.get("requested_url"), "renderer requested URL", nonempty=True
    )
    final_url = _text(raw_navigation.get("final_url"), "renderer final URL", nonempty=True)
    wait_until = _text(raw_navigation.get("wait_until"), "renderer wait strategy", nonempty=True)
    navigation_timeout = raw_navigation.get("timeout_seconds")
    if (
        type(navigation_timeout) not in {int, float}
        or not math.isfinite(navigation_timeout)
        or navigation_timeout <= 0
    ):
        raise ScanError("rendered navigation-timeout provenance is invalid")
    settings["navigation"] = {
        "wait_until": wait_until,
        "timeout_seconds": navigation_timeout,
    }
    credentials_used = _renderer_bool(
        raw_policy.get("credentials_used", False), "credential policy fact"
    )
    cache_control_no_store = _renderer_bool(
        raw_policy.get("cache_control_no_store", False), "cache-control policy fact"
    )
    from seohead.checks.navigation import retain_navigation

    try:
        navigation = retain_navigation(raw_navigation, lambda url: _intern_url(con, url))
    except ValueError as exc:
        raise ScanError(f"rendered navigation evidence is invalid: {exc}") from exc
    # scan.v1 readers already accept extensible renderer settings, while the
    # top-level renderer envelope is closed. Keep rollback readers compatible.
    settings["navigation_evidence"] = navigation
    # Asset summaries travel with their original renderer settings on offline replay.
    # Never copy source bodies or an unbounded arbitrary object into this adjunct.
    assets = raw_settings.get("asset_evidence")
    if assets is not None:
        if not isinstance(assets, dict):
            raise ScanError("rendered asset evidence must be an object")
        encoded_assets = json.dumps(assets, ensure_ascii=True, allow_nan=False)
        if len(encoded_assets) > 1024 * 1024:
            raise ScanError("rendered asset evidence exceeds the retained record budget")
        settings["asset_evidence"] = assets
    payload = {
        "engine": engine,
        "engine_version": version,
        "page_concurrency": page_concurrency,
        "settings": settings,
        "flattened_iframes": bool(settings["transforms"]["flatten_iframes_applied"]),
        "capture_limitations": [],
        "navigation_url_id": _intern_url(con, requested_url),
        "final_url_id": _intern_url(con, final_url),
        "navigation_transform": navigation_transform,
    }
    return (
        json.dumps(payload, ensure_ascii=False, separators=(",", ":")),
        credentials_used,
        cache_control_no_store,
    )


def _rendered_input_state(
    html: Any, body_state: Any, body_reason: Any
) -> tuple[bytes | None, str, str]:
    state = _text(body_state, "document body state", nonempty=True)
    reason = _text(body_reason, "document body reason", nonempty=True)
    if state == "complete":
        if reason != "none" or html is None:
            raise ScanError("complete rendered document requires DOM bytes and reason none")
        if type(html) is str:
            try:
                raw = html.encode("utf-8", errors="strict")
            except UnicodeError as exc:
                raise ScanError("rendered DOM cannot be encoded as UTF-8") from exc
        elif type(html) is bytes:
            raw = html
        else:
            raise ScanError("rendered DOM must be exact UTF-8 bytes or text")
        try:
            raw.decode("utf-8", errors="strict")
        except UnicodeError as exc:
            raise ScanError("rendered DOM bytes are not valid UTF-8") from exc
        return raw, state, reason
    if html is not None:
        raise ScanError("non-complete rendered document cannot retain DOM bytes")
    if state == "truncated" and reason == "truncated":
        return None, state, reason
    if state == "unavailable" and reason in _UNAVAILABLE:
        return None, state, reason
    if state == "omitted" and reason in _OMITTED:
        return None, state, reason
    raise ScanError("rendered document body state or reason is invalid")


def store_response(
    con: sqlite3.Connection,
    event: CaptureEvent,
    *,
    purpose: str,
    policy: dict[str, Any],
    retain_no_store: bool = False,
    logical_url: str | None = None,
    representation: str = "static",
    renderer: dict[str, Any] | None = None,
) -> tuple[int, int | None]:
    """Write one event inside the caller's transaction; never commits or associates pages."""
    if not isinstance(con, sqlite3.Connection):
        raise ScanError("corpus writer requires a sqlite3 connection")
    if (
        type(purpose) is not str
        or purpose not in _PURPOSES
        or type(representation) is not str
        or representation not in {"static", "legacy_fragment"}
    ):
        raise ScanError("captured response purpose or representation is unsupported")
    if representation == "static" and renderer is not None:
        raise ScanError("static documents cannot carry renderer provenance")
    if representation == "legacy_fragment" and type(renderer) is not dict:
        raise ScanError("legacy-fragment documents require renderer provenance")
    if representation == "legacy_fragment" and purpose != "page":
        raise ScanError("legacy-fragment response purpose must be page")
    if logical_url is not None:
        _text(logical_url, "logical URL", nonempty=True)
    policy = validate_policy(policy)
    event = _validate_event(event)
    response_headers = event.response_headers
    effective_status = event.effective_status_code
    effective_headers = event.effective_headers
    if effective_status is None:
        effective_status = event.status_code
    if not effective_headers:
        effective_headers = response_headers
    if event.redirect_history and not event.effective_url:
        raise ScanError("captured redirect response lacks an effective URL")
    request_url_id = _intern_url(con, event.requested_url)
    effective_url_id = _intern_url(con, event.effective_url) if event.effective_url else None
    body_state, body_reason = _state(
        event, purpose, policy, effective_headers, retain_no_store=retain_no_store
    )
    if body_state == "complete" and effective_url_id is None:
        raise ScanError("complete captured response lacks an effective URL")
    body_sha = None
    fidelity = "unavailable"
    if body_state == "complete":
        encoded = encode_body(event.entity_bytes or b"")
        if _reserve_unique_bytes(con, encoded, policy):
            body_sha = str(encoded["sha256"])
            fidelity = "entity_bytes"
        else:
            body_state, body_reason = "omitted", "body_budget_exhausted"
    ordinal = con.execute("SELECT COALESCE(MAX(request_ordinal)+1,0) FROM responses").fetchone()[0]
    request_headers = _pairs(event.request_headers, "request")
    variant_key = hashlib.sha256(
        json.dumps(
            {"method": event.method.upper(), "request_headers": json.loads(request_headers)},
            ensure_ascii=False,
            separators=(",", ":"),
        ).encode("utf-8")
    ).hexdigest()
    reported_size_bytes = _content_length(response_headers)
    if reported_size_bytes is None:
        reported_size_bytes = _content_length(effective_headers)
    con.execute(
        "INSERT INTO responses(request_url_id,request_ordinal,effective_url_id,redirect_chain_json,"
        "method,purpose,requested_at,received_at,request_headers_redacted_json,credentials_used,"
        "variant_key,status_code,effective_status_code,response_headers_redacted_json,"
        "effective_headers_redacted_json,content_type,charset,content_encoding,reported_size_bytes,"
        "response_time,transport_source,cache_status,source_response_id,body_sha256,body_fidelity,"
        "body_state,body_reason,error,error_kind) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (
            request_url_id,
            ordinal,
            effective_url_id,
            _redirect_chain(con, event.redirect_history),
            event.method.upper(),
            purpose,
            event.requested_at,
            event.received_at or None,
            request_headers,
            int(event.credentials_used),
            variant_key,
            event.status_code,
            effective_status,
            _pairs(response_headers, "response"),
            _pairs(effective_headers, "effective"),
            event.content_type,
            _declared_charset(event.content_type),
            event.content_encoding,
            reported_size_bytes,
            event.response_time,
            "network",
            "",
            None,
            body_sha,
            fidelity,
            body_state,
            body_reason,
            event.error,
            event.error_kind,
        ),
    )
    response_id = int(con.execute("SELECT last_insert_rowid()").fetchone()[0])
    from .transport import record as record_transport

    record_transport(con, response_id, event)
    document_id = None
    if purpose == "page":
        url_id = _intern_url(con, logical_url if logical_url is not None else event.requested_url)
        if body_state == "complete":
            _decoded_text, decoder = decode_entity(event.entity_bytes or b"", event.content_type)
        else:
            decoder = {
                "decoder_version": "scan_decoder.v1",
                "decoder_source": "not_applicable",
                "decoder_charset": "unknown",
                "decoder_errors": "not_applicable",
            }
        renderer_json = "{}"
        if representation == "legacy_fragment":
            renderer_json, _credentials, _no_store_fact = _rendered_provenance(
                con, renderer, navigation_transform="legacy_escaped_fragment"
            )
            renderer_data = json.loads(renderer_json)
            if renderer_data["navigation_url_id"] != request_url_id:
                raise ScanError(
                    "legacy-fragment renderer navigation URL must match response request URL"
                )
            renderer_data["capture_limitations"] = [] if body_state == "complete" else [body_reason]
            renderer_json = json.dumps(renderer_data, ensure_ascii=False, separators=(",", ":"))
        con.execute(
            "INSERT INTO documents(url_id,representation,source_response_id,body_sha256,captured_at,"
            "decoder_version,decoder_source,decoder_charset,decoder_errors,fidelity,body_state,"
            "body_reason,renderer_json) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                url_id,
                representation,
                response_id,
                body_sha,
                event.received_at or event.requested_at,
                decoder["decoder_version"],
                decoder["decoder_source"],
                decoder["decoder_charset"],
                decoder["decoder_errors"],
                fidelity,
                body_state,
                body_reason,
                renderer_json,
            ),
        )
        document_id = int(con.execute("SELECT last_insert_rowid()").fetchone()[0])
    return response_id, document_id


def store_rendered_document(
    con: sqlite3.Connection,
    *,
    logical_url: str,
    html: bytes | str | None,
    renderer: dict[str, Any],
    policy: dict[str, Any],
    retain_no_store: bool = False,
    captured_at: str,
    body_state: str = "complete",
    body_reason: str = "none",
) -> int:
    """Store one browser-returned serialized DOM inside the caller's transaction.

    This is intentionally separate from :func:`store_response`: a browser DOM is not an
    HTTP entity and therefore cannot acquire a fabricated response observation or charset.
    """
    if not isinstance(con, sqlite3.Connection):
        raise ScanError("rendered document writer requires a sqlite3 connection")
    _text(logical_url, "logical URL", nonempty=True)
    _timestamp(captured_at, "rendered captured_at")
    policy = validate_policy(policy)
    raw, stored_state, stored_reason = _rendered_input_state(html, body_state, body_reason)
    renderer_json, credentials_used, cache_control_no_store = _rendered_provenance(con, renderer)
    body_sha = None
    if stored_state == "complete":
        if credentials_used:
            stored_state, stored_reason = "omitted", "credentialed"
        elif cache_control_no_store and not retain_no_store:
            stored_state, stored_reason = "omitted", "cache_control_no_store"
        elif policy["body_mode"] == "off":
            stored_state, stored_reason = "omitted", "not_enabled"
        elif len(raw or b"") > policy["max_body_bytes"]:
            stored_state, stored_reason = "omitted", "body_budget_exhausted"
        else:
            encoded = encode_body(raw or b"")
            if _reserve_unique_bytes(con, encoded, policy):
                body_sha = str(encoded["sha256"])
            else:
                stored_state, stored_reason = "omitted", "body_budget_exhausted"
    limitations = [] if stored_state == "complete" else [stored_reason]
    renderer_data = json.loads(renderer_json)
    renderer_data["capture_limitations"] = limitations
    con.execute(
        "INSERT INTO documents(url_id,representation,source_response_id,body_sha256,captured_at,"
        "decoder_version,decoder_source,decoder_charset,decoder_errors,fidelity,body_state,"
        "body_reason,renderer_json) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (
            _intern_url(con, logical_url),
            "rendered",
            None,
            body_sha,
            captured_at,
            "scan_decoder.v1",
            "renderer_utf8" if stored_state == "complete" else "not_applicable",
            "utf-8" if stored_state == "complete" else "unknown",
            "not_applicable",
            "serialized_dom" if stored_state == "complete" else "unavailable",
            stored_state,
            stored_reason,
            json.dumps(renderer_data, ensure_ascii=False, separators=(",", ":")),
        ),
    )
    return int(con.execute("SELECT last_insert_rowid()").fetchone()[0])


def html_body_retention(con: sqlite3.Connection) -> dict[str, Any]:
    """Count the HTML page bodies this corpus kept, and name why it dropped the rest.

    ``capabilities['html_bodies']`` says ``partial`` whether one body is missing or
    four fifths of them are, so a run summary cannot be built from it: the crawl
    behind #647 reported pages and links normally while 33 001 of 40 920 bodies had
    been discarded, and everything computed from stored HTML silently described the
    7 903 that remained. These are the counts that make that sayable.
    """
    rows = con.execute(
        "SELECT d.body_state,d.body_reason,COUNT(*) FROM documents d "
        "LEFT JOIN responses r ON r.response_id=d.source_response_id "
        "WHERE d.representation IN ('static','legacy_fragment') "
        "AND lower(COALESCE(r.content_type,'')) LIKE '%html%' "
        "GROUP BY 1,2 ORDER BY 1,2"
    ).fetchall()
    total = sum(count for _state, _reason, count in rows)
    omitted = {reason: count for state, reason, count in rows if state == "omitted"}
    return {"total": total, "retained": total - sum(omitted.values()), "omitted": omitted}


def corpus_summary(con: sqlite3.Connection, policy: dict[str, Any]) -> dict[str, Any]:
    """Derive capability coverage over requested captures, not unrequested lanes."""
    policy = validate_policy(policy)
    response_total = con.execute("SELECT COUNT(*) FROM responses").fetchone()[0]
    missing_pages = con.execute(
        "SELECT COUNT(*) FROM pages p WHERE NOT EXISTS(SELECT 1 FROM responses r WHERE r.request_url_id=p.url_id AND r.purpose='page')"
    ).fetchone()[0]
    responses = {
        "state": "unavailable"
        if not response_total
        else "partial"
        if missing_pages
        else "complete",
        "reason": "no captured responses"
        if not response_total
        else "some page response observations are missing"
        if missing_pages
        else "",
    }

    def body_state(where: str, label: str) -> dict[str, str]:
        if policy["body_mode"] == "off":
            return {"state": "unavailable", "reason": "body retention is disabled"}
        total, complete = con.execute(
            "SELECT COUNT(*),COALESCE(SUM(d.body_state='complete'),0) FROM documents d "
            "LEFT JOIN responses r ON r.response_id=d.source_response_id WHERE " + where
        ).fetchone()
        if not total:
            return {"state": "unavailable", "reason": f"no captured {label}"}
        if complete == total and not missing_pages:
            return {"state": "complete", "reason": ""}
        return {"state": "partial", "reason": f"some {label} are missing or not retained"}

    html = body_state(
        "d.representation IN ('static','legacy_fragment') AND lower(COALESCE(r.content_type,'')) LIKE '%html%'",
        "HTML documents",
    )
    rendered = body_state("d.representation='rendered'", "rendered documents")
    missing_body = (
        con.execute(
            "SELECT 1 FROM responses WHERE body_state!='complete' UNION ALL "
            "SELECT 1 FROM documents WHERE body_state!='complete' LIMIT 1"
        ).fetchone()
        is not None
    )
    retained = con.execute("SELECT 1 FROM bodies LIMIT 1").fetchone() is not None
    summary = {
        "capabilities": {"responses": responses, "html_bodies": html, "rendered_bodies": rendered},
        "corpus_partial": policy["body_mode"] == "off"
        or not retained
        or bool(missing_pages)
        or missing_body,
    }
    config_row = con.execute("SELECT config_json FROM scan").fetchone()
    config = json.loads(config_row[0]) if config_row else {}
    if "resources" in config:
        from .resources import resource_capabilities

        resource_states = resource_capabilities(con, fetch_enabled=config["resources"]["fetch"])
        summary["capabilities"].update(resource_states)
        if (
            config["resources"]["fetch"]
            and resource_states["resource_bodies"]["state"] != "complete"
        ):
            summary["corpus_partial"] = True
    return summary


def rendered_body_retention(con: sqlite3.Connection) -> dict[str, Any]:
    """Count the rendered DOMs this corpus kept, and name why it dropped the rest.

    ``capabilities['rendered_bodies']`` says ``partial`` whether one DOM is missing
    or every one of them is, so a run summary cannot be built from it: the rendered
    crawl behind #656 discarded a DOM for every page whose own session cookie came
    back on one subresource request, and reported its pages, links and escalation
    counts exactly as a complete run would. These are the counts that make that
    sayable, and they are read back out of the artifact, so a finished scan still
    answers the same question afterwards.
    """
    rows = con.execute(
        "SELECT body_state,body_reason,COUNT(*) FROM documents "
        "WHERE representation='rendered' GROUP BY 1,2 ORDER BY 1,2"
    ).fetchall()
    total = sum(count for _state, _reason, count in rows)
    omitted = {reason: count for state, reason, count in rows if state != "complete"}
    return {"total": total, "retained": total - sum(omitted.values()), "omitted": omitted}
