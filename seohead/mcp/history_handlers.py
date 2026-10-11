"""Shared local interfaces for explicit saved-scan history actions."""

from __future__ import annotations

import json
import sqlite3
import time
from typing import Any
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

from seohead.storage import READ_TIMEOUT_SECONDS, open_scan, open_scan_mode
from seohead.storage.body_diff import body_diff
from seohead.storage.history import (
    inspect_scan,
    list_scans,
    pin_scan,
    prune_apply,
    prune_preview,
    snapshot_scan,
)
from seohead.storage.status import scan_status as _scan_status

_EFFECTIVE_LOOKUP_SECONDS = 0.5


def scan_graph(
    input_path: str,
    positions: list[str] | str | None = None,
    section: str | None = None,
    around: str | None = None,
    depth: int = 2,
    aggregate: str = "none",
    representation: str = "all",
    node_offset: int = 0,
    edge_offset: int = 0,
    node_limit: int = 1000,
    edge_limit: int = 2000,
    max_nodes: int = 5000,
    max_edges: int = 20000,
    compare_to: str | None = None,
    timeout_seconds: float = 1.0,
    max_bytes: int = 8 * 1024 * 1024,
) -> dict[str, Any]:
    """Read bounded graph chunks or SQL differences; never fetch or mutate a scan."""
    from seohead.storage.scan_graph import scan_graph as core

    try:
        if type(max_bytes) is not int or not 4096 <= max_bytes <= 8 * 1024 * 1024:
            raise ValueError("max_bytes must be 4096..8388608")
        result = core(
            _path(input_path, "scan"),
            positions=positions,
            section=section,
            around=around,
            depth=depth,
            aggregate=aggregate,
            representation=representation,
            node_offset=node_offset,
            edge_offset=edge_offset,
            node_limit=node_limit,
            edge_limit=edge_limit,
            max_nodes=max_nodes,
            max_edges=max_edges,
            compare_to=compare_to,
            timeout_seconds=timeout_seconds,
        )
        if len(json.dumps(result, ensure_ascii=False).encode("utf-8")) > max_bytes:
            return {
                "ok": False,
                "format": result["format"],
                "state": "limit_reached",
                "reason_code": "output_byte_limit_exceeded",
                "max_bytes": max_bytes,
            }
        return result
    except (ValueError, OSError, sqlite3.Error, TypeError) as exc:
        return {
            "ok": False,
            "state": "unavailable",
            "error": str(exc),
            "reason_code": getattr(exc, "reason_code", "invalid_graph_request"),
        }


def scan_link_inspect(
    input_path: str,
    view: str = "path",
    seed: str | None = None,
    target: str | None = None,
    representation: str = "all",
    cursor: str | None = None,
    link_id: int | None = None,
    document_id: int | None = None,
    offset: int = 0,
    limit: int = 100,
    max_bytes: int = 1_048_576,
    max_body_bytes: int = 5 * 1024 * 1024,
    max_nodes: int = 10_000,
    max_edges: int = 200_000,
    max_depth: int = 20,
    timeout_seconds: float = 15.0,
    url: str | None = None,
    direction: str = "out",
    link_type: str = "all",
    follow: str = "all",
    status_class: str = "all",
    contains: str | None = None,
    sort: str = "order",
    preset: str | None = None,
) -> dict[str, Any]:
    """One bounded saved-scan link query, shared by the CLI and local MCP."""
    if not isinstance(view, str) or view not in {"path", "inlinks", "context", "links"}:
        return {
            "ok": False,
            "view": "invalid",
            "error": "view must be path, inlinks, context, or links",
        }
    try:
        path = _path(input_path, "scan")
        links_only = (url, direction, link_type, follow, status_class, contains, sort, preset) != (
            None,
            "out",
            "all",
            "all",
            "all",
            None,
            "order",
            None,
        )
        if links_only and view != "links":
            raise ValueError("url, direction and link filters belong to view=links")
        if type(max_bytes) is not int or not 4096 <= max_bytes <= 8 * 1024 * 1024:
            raise ValueError("max_bytes must be 4096..8388608")
        if view == "links":
            if not url:
                raise ValueError("links view requires a url")
            if preset is not None:
                definitions = {
                    "inlinks": {"direction": "in", "link_type": "internal", "status_class": "all"},
                    "broken_links": {
                        "direction": "out",
                        "link_type": "internal",
                        "status_class": "broken",
                    },
                }
                if preset not in definitions:
                    raise ValueError("preset must be inlinks or broken_links")
                defaults = {"direction": "out", "link_type": "all", "status_class": "all"}
                current = {
                    "direction": direction,
                    "link_type": link_type,
                    "status_class": status_class,
                }
                for key, value in definitions[preset].items():
                    if current[key] not in {defaults[key], value}:
                        raise ValueError(f"preset {preset} conflicts with explicit {key}")
                direction = definitions[preset]["direction"]
                link_type = definitions[preset]["link_type"]
                status_class = definitions[preset]["status_class"]
            if any(value is not None for value in (seed, target, cursor, link_id, document_id)) or (
                max_nodes,
                max_edges,
                max_depth,
                max_body_bytes,
            ) != (10_000, 200_000, 20, 5 * 1024 * 1024):
                raise ValueError("links view does not accept seed, target, cursor, IDs or budgets")
            from seohead.storage.url_links import url_links

            result = url_links(
                path,
                url,
                direction=direction,
                representation=representation,
                link_type=link_type,
                follow=follow,
                status_class=status_class,
                contains=contains,
                sort=sort,
                offset=offset,
                limit=limit,
                timeout_seconds=timeout_seconds,
            )
        elif view == "path":
            if not seed or not target:
                raise ValueError("path view requires seed and target URLs")
            if (
                any(value is not None for value in (cursor, link_id, document_id))
                or offset
                or limit != 100
            ):
                raise ValueError(
                    "path view does not accept cursor, link/document ID, offset or limit"
                )
            if max_body_bytes != 5 * 1024 * 1024:
                raise ValueError("path view does not read a body")
            from seohead.storage.link_queries import shortest_observed_path

            result = shortest_observed_path(
                path,
                seed,
                target,
                representation=representation,
                max_nodes=max_nodes,
                max_edges=max_edges,
                max_depth=max_depth,
                timeout_seconds=timeout_seconds,
            )
        elif view == "inlinks":
            if not target:
                raise ValueError("inlinks view requires a target URL")
            if seed is not None or link_id is not None or document_id is not None or offset:
                raise ValueError("inlinks view does not accept seed, link/document ID or offset")
            if (max_nodes, max_edges, max_depth, max_body_bytes) != (
                10_000,
                200_000,
                20,
                5 * 1024 * 1024,
            ):
                raise ValueError("inlinks view does not accept path or body budgets")
            from seohead.storage.link_queries import reverse_inlinks

            result = reverse_inlinks(
                path,
                target,
                representation=representation,
                cursor=cursor,
                limit=limit,
                max_bytes=max_bytes,
                timeout_seconds=timeout_seconds,
            )
        elif view == "context":
            if (link_id is None) == (document_id is None):
                raise ValueError("context view requires exactly one of link_id or document_id")
            if seed is not None or target is not None or cursor is not None:
                raise ValueError("context view does not accept seed, target or cursor")
            if (max_nodes, max_edges, max_depth, timeout_seconds) != (10_000, 200_000, 20, 15.0):
                raise ValueError("context view does not accept path budgets")
            from seohead.storage.link_context import context_for_link, contexts_for_document

            if link_id is not None:
                if offset or limit != 100:
                    raise ValueError("single-link context does not accept offset or limit")
                result = context_for_link(
                    path, link_id, max_body_bytes=max_body_bytes, max_result_bytes=max_bytes
                )
            else:
                result = contexts_for_document(
                    path,
                    document_id,
                    offset=offset,
                    limit=limit,
                    max_body_bytes=max_body_bytes,
                    max_result_bytes=max_bytes,
                )
            if representation != "all" and result["representation"] != representation:
                raise ValueError("context representation differs from the requested filter")
        answer = {"ok": True, "view": view, **result, **({"preset": preset} if preset else {})}
        size = len(json.dumps(answer, ensure_ascii=False, default=str).encode("utf-8"))
        if size > max_bytes:
            return {
                "ok": False,
                "view": view,
                "state": "limit_reached",
                "reason": "output_byte_limit_exceeded",
                "scan_uuid": result.get("scan_uuid"),
                "evidence_revision": result.get("evidence_revision"),
                "max_bytes": max_bytes,
                "bytes_required": size,
            }
        return answer
    except (ValueError, OSError, sqlite3.Error, TypeError) as exc:
        failure = {"ok": False, "view": view, "error": str(exc)}
        if getattr(exc, "reason_code", None):
            failure["reason_code"] = exc.reason_code
        return failure


def _path(value: str, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise ValueError(f"{label} is required")
    return value


def scan_list(directory: str, *, offset: int = 0, limit: int = 100) -> dict[str, Any]:
    return list_scans(_path(directory, "directory"), offset=offset, limit=limit)


def scan_inspect(
    input_path: str,
    *,
    table: str = "pages",
    offset: int = 0,
    limit: int = 100,
    max_bytes: int = 1_048_576,
    columns: list[str] | None = None,
    total: bool = False,
) -> dict[str, Any]:
    return inspect_scan(
        _path(input_path, "input"),
        table=table,
        offset=offset,
        limit=limit,
        max_bytes=max_bytes,
        columns=columns,
        total=total,
    )


def scan_status(input_path: str, full_validation: bool = False) -> dict[str, Any]:
    """Summarize saved frontier work and committed page outcomes offline.

    ``validation`` in the result is ``"light"`` (header/schema check) or ``"full"``
    (``full_validation=True``, or the same bytes were already fully validated).
    """
    return _scan_status(_path(input_path, "input"), full_validation=full_validation)


_DETAIL_SENSITIVE_HEADERS = frozenset(
    {"authorization", "cookie", "set-cookie", "proxy-authorization", "x-api-key", "x-auth-token"}
)
_DETAIL_PAGE_URL_FIELDS = frozenset(
    {"canonical", "redirect_url", "final_url", "og_image", "og_url", "meta_refresh", "http_refresh"}
)
_DETAIL_NESTED_URL_FIELDS = frozenset(
    {
        "url",
        "href",
        "action",
        "canonical",
        "next_url",
        "request_url",
        "location_raw",
        "source",
        "destination",
    }
)


def _detail_url(value: Any) -> Any:
    """Keep a retained URL's identity while never returning a query value."""
    if not isinstance(value, str) or "?" not in value:
        return value
    try:
        parts = urlsplit(value)
        pairs = parse_qsl(parts.query, keep_blank_values=True)
    except ValueError:
        return "[query-redacted]"
    if not pairs:
        return urlunsplit((parts.scheme, parts.netloc, parts.path, "[redacted]", parts.fragment))
    return urlunsplit(
        (
            parts.scheme,
            parts.netloc,
            parts.path,
            urlencode([(name, "[redacted]") for name, _value in pairs]),
            parts.fragment,
        )
    )


def _detail_headers(value: Any, label: str) -> list[list[str]]:
    """Decode already-redacted headers and fail closed on an unsafe old artifact."""
    try:
        pairs = json.loads(value)
    except (TypeError, ValueError) as exc:
        raise ValueError(f"{label} are not valid retained header pairs") from exc
    if not isinstance(pairs, list):
        raise ValueError(f"{label} are not valid retained header pairs")
    safe: list[list[str]] = []
    for pair in pairs:
        if (
            not isinstance(pair, list)
            or len(pair) != 2
            or any(type(part) is not str for part in pair)
        ):
            raise ValueError(f"{label} are not valid retained header pairs")
        name, header_value = pair
        if name.lower() in _DETAIL_SENSITIVE_HEADERS:
            safe.append(["X-SEOHEAD-Redacted-Headers", name.lower()])
        else:
            safe.append([name, _detail_url(header_value)])
    return safe


def _detail_json(value: Any, label: str) -> Any:
    try:
        return json.loads(value)
    except (TypeError, ValueError) as exc:
        raise ValueError(f"{label} is not valid retained JSON") from exc


def _detail_message(value: Any) -> str:
    """Reuse the restricted browser-artifact redactor for retained transport text."""
    from seohead.storage.browser_artifacts import _redact

    return _redact(value)


def _detail_nested_urls(value: Any, *, key: str | None = None, depth: int = 0) -> Any:
    """Redact query values in the known URL fields of structured page evidence."""
    if depth > 8:
        return value
    if isinstance(value, dict):
        return {
            item_key: _detail_nested_urls(item, key=item_key, depth=depth + 1)
            for item_key, item in value.items()
        }
    if isinstance(value, list):
        return [_detail_nested_urls(item, key=key, depth=depth + 1) for item in value]
    return _detail_url(value) if key in _DETAIL_NESTED_URL_FIELDS else value


def _detail_redirects(value: Any) -> list[dict[str, Any]]:
    chain = _detail_json(value, "redirect chain")
    if not isinstance(chain, list) or any(not isinstance(item, dict) for item in chain):
        raise ValueError("redirect chain is not an ordered retained object list")
    return [_detail_nested_urls(entry) for entry in chain]


def _detail_page(row: sqlite3.Row) -> dict[str, Any]:
    page = dict(row)
    page["url"] = _detail_url(page["url"])
    for key in _DETAIL_PAGE_URL_FIELDS:
        if key in page:
            page[key] = _detail_url(page[key])
    for key in (
        "hreflang_json",
        "heading_outline_json",
        "link_placement_json",
        "trust_signals_json",
        "duplicate_ids_json",
        "canonical_chain_json",
    ):
        if page.get(key) is not None:
            page[key] = _detail_nested_urls(_detail_json(page[key], key))
    page["redirect_chain"] = _detail_redirects(page.pop("redirect_chain_json"))
    for key in ("error", "body_unavailable"):
        if key in page:
            page[key] = _detail_message(page[key])
    return page


def _detail_response(row: sqlite3.Row) -> dict[str, Any]:
    item = dict(row)
    for key in ("request_url", "effective_url"):
        item[key] = _detail_url(item[key])
    item["redirect_chain"] = _detail_redirects(item.pop("redirect_chain_json"))
    item["request_headers"] = _detail_headers(
        item.pop("request_headers_redacted_json"), "request headers"
    )
    item["response_headers"] = _detail_headers(
        item.pop("response_headers_redacted_json"), "response headers"
    )
    item["effective_headers"] = _detail_headers(
        item.pop("effective_headers_redacted_json"), "effective headers"
    )
    item["error"] = _detail_message(item["error"])
    return item


def _detail_forms(rows: list[sqlite3.Row]) -> list[dict[str, Any]]:
    return [
        {
            **dict(row),
            "action": _detail_url(row["action"]),
            "has_password": bool(row["has_password"]),
        }
        for row in rows
    ]


def _scan_url_detail(
    input_path: str,
    url: str,
    *,
    response_offset: int = 0,
    response_limit: int = 10,
    form_offset: int = 0,
    form_limit: int = 20,
    max_bytes: int = 1_048_576,
    body_offset: int = 0,
    body_length: int = 4096,
    max_body_bytes: int = 8 * 1024 * 1024,
) -> dict[str, Any]:
    """Read one native scan URL and its bounded retained transport evidence offline."""
    path = _path(input_path, "input")
    if not isinstance(url, str) or not url or len(url) > 8192:
        raise ValueError("url must be nonempty exact retained URL text of at most 8192 characters")
    if type(response_offset) is not int or response_offset < 0:
        raise ValueError("response_offset must be a nonnegative integer")
    if type(form_offset) is not int or form_offset < 0:
        raise ValueError("form_offset must be a nonnegative integer")
    if type(response_limit) is not int or not 1 <= response_limit <= 100:
        raise ValueError("response_limit must be 1..100")
    if type(form_limit) is not int or not 1 <= form_limit <= 100:
        raise ValueError("form_limit must be 1..100")
    if type(max_bytes) is not int or not 4096 <= max_bytes <= 8 * 1024 * 1024:
        raise ValueError("max_bytes must be 4096..8388608")
    if type(body_offset) is not int or body_offset < 0:
        raise ValueError("body_offset must be a nonnegative integer")
    if type(body_length) is not int or not 1 <= body_length <= 64 * 1024:
        raise ValueError("body_length must be 1..65536")
    if type(max_body_bytes) is not int or not 1 <= max_body_bytes <= 64 * 1024 * 1024:
        raise ValueError("max_body_bytes must be 1..67108864")
    if path.lower().endswith(".json"):
        return {
            "ok": True,
            "state": "unavailable",
            "reason": "Screaming Frog audit JSON does not retain native per-URL transport evidence",
            "source": {
                "source_kind": "screaming_frog",
                "scan_uuid": None,
                "evidence_revision": None,
            },
        }
    con, validation = open_scan_mode(path, require_audit=False, light=True)
    try:
        source_row = con.execute(
            "SELECT scan_uuid,format_version,source_kind,evidence_revision,lifecycle,finish_reason,"
            "crawl_partial,corpus_partial FROM scan WHERE singleton=1"
        ).fetchone()
        if source_row is None:
            raise ValueError("scan source identity is unavailable")
        source = dict(source_row)
        source["crawl_partial"] = bool(source["crawl_partial"])
        source["corpus_partial"] = bool(source["corpus_partial"])
        if source["source_kind"] != "native":
            return {
                "validation": validation,
                "ok": True,
                "state": "unavailable",
                "reason": "this saved source does not retain native per-URL transport evidence",
                "source": source,
            }
        url_row = con.execute("SELECT url_id,url FROM urls WHERE url=?", (url,)).fetchone()
        if url_row is None:
            return {
                "ok": True,
                "state": "not_found",
                "source": source,
                "url": _detail_url(url),
                "validation": validation,
            }
        page = con.execute(
            "SELECT p.*,u.url FROM pages p JOIN urls u USING(url_id) WHERE p.url_id=?",
            (url_row["url_id"],),
        ).fetchone()
        if page is None:
            return {
                "validation": validation,
                "ok": True,
                "state": "unavailable",
                "reason": "URL was retained without a page record",
                "source": source,
                "url": _detail_url(url_row["url"]),
                "url_id": url_row["url_id"],
            }
        # responses has an index on request_url_id only: the effective-URL (redirect target)
        # match is a table scan, so it runs under a short budget and is reported when skipped.
        ids = [
            row[0]
            for row in con.execute(
                "SELECT response_id FROM responses WHERE request_url_id=?", (url_row["url_id"],)
            )
        ]
        effective_state: dict[str, Any] = {"state": "complete"}
        deadline = time.monotonic() + _EFFECTIVE_LOOKUP_SECONDS
        con.set_progress_handler(lambda: int(time.monotonic() > deadline), 10000)
        try:
            ids += [
                row[0]
                for row in con.execute(
                    "SELECT response_id FROM responses WHERE effective_url_id=? AND request_url_id!=?",
                    (url_row["url_id"], url_row["url_id"]),
                )
            ]
        except sqlite3.OperationalError as exc:
            if "interrupted" not in str(exc).lower():
                raise
            effective_state = {
                "state": "skipped",
                "reason": "redirect-target lookup exceeds the light-read budget; "
                "only responses requested for this URL are listed",
            }
        finally:
            rest = time.monotonic() + READ_TIMEOUT_SECONDS
            con.set_progress_handler(lambda: int(time.monotonic() > rest), 10000)
        response_rows = con.execute(
            "SELECT r.response_id,r.request_ordinal,r.request_url_id,r.effective_url_id,"
            "request_url.url AS request_url,effective_url.url AS effective_url,r.redirect_chain_json,"
            "r.method,r.purpose,r.requested_at,r.received_at,r.request_headers_redacted_json,"
            "r.credentials_used,r.variant_key,r.status_code,r.effective_status_code,"
            "r.response_headers_redacted_json,r.effective_headers_redacted_json,r.content_type,"
            "r.charset,r.content_encoding,r.reported_size_bytes,r.response_time,r.transport_source,"
            "r.cache_status,r.source_response_id,r.body_sha256,r.body_fidelity,r.body_state,"
            "r.body_reason,r.error,r.error_kind "
            "FROM responses r JOIN urls request_url ON request_url.url_id=r.request_url_id "
            "LEFT JOIN urls effective_url ON effective_url.url_id=r.effective_url_id "
            "WHERE r.response_id IN (SELECT value FROM json_each(?)) ORDER BY r.request_ordinal "
            "LIMIT ? OFFSET ?",
            (json.dumps(ids), response_limit + 1, response_offset),
        ).fetchall()
        form_rows = con.execute(
            "SELECT form_id,ordinal,source_document_id,evidence_representation,method,action,has_password "
            "FROM forms WHERE page_url_id=? ORDER BY form_id LIMIT ? OFFSET ?",
            (url_row["url_id"], form_limit + 1, form_offset),
        ).fetchall()
        from seohead.storage.bodies import read_page_document_content

        retained = read_page_document_content(
            con,
            url,
            representation="static",
            max_decoded_bytes=max_body_bytes,
        )
        if retained["state"] == "available":
            text = retained["text"]
            window = text[body_offset : body_offset + body_length]
            source_html = {
                "state": "available",
                "representation": "static",
                "document_id": retained["document_id"],
                "offset": body_offset,
                "length": len(window),
                "total_chars": retained["total_chars"],
                "truncated": body_offset + len(window) < len(text),
                "lines": retained["lines"],
                "encoding": retained["encoding"],
                "sha256": retained["sha256"],
                "raw_body_sha256": retained["raw_body_sha256"],
                "text": window,
            }
        else:
            source_html = {
                "state": "unavailable",
                "reason": retained.get("reason", "not_retained"),
                "representation": "static",
            }
            if retained.get("document_id") is not None:
                source_html["document_id"] = retained["document_id"]
        detail = {
            "ok": True,
            "state": "available",
            "validation": validation,
            "source": source,
            "url": _detail_url(url_row["url"]),
            "url_id": url_row["url_id"],
            "page": _detail_page(page),
            "responses": {
                "offset": response_offset,
                "limit": response_limit,
                "items": [_detail_response(row) for row in response_rows[:response_limit]],
                "has_more": len(response_rows) > response_limit,
                "next_offset": response_offset + min(len(response_rows), response_limit),
                "redirect_target_lookup": effective_state,
            },
            "forms": {
                "offset": form_offset,
                "limit": form_limit,
                "items": _detail_forms(form_rows[:form_limit]),
                "has_more": len(form_rows) > form_limit,
                "next_offset": form_offset + min(len(form_rows), form_limit),
            },
            "source_html": source_html,
            "scope": "bounded retained HTML and native metadata only; "
            "no network request or artifact mutation",
        }
        size = len(json.dumps(detail, ensure_ascii=False, default=str).encode("utf-8"))
        if size > max_bytes:
            return {
                "validation": validation,
                "ok": False,
                "state": "limit_reached",
                "reason": "output_byte_limit_exceeded",
                "source": source,
                "url": _detail_url(url_row["url"]),
                "max_bytes": max_bytes,
                "bytes_required": size,
            }
        return detail
    finally:
        con.close()


def scan_url_detail(
    input_path: str,
    url: str,
    *,
    response_offset: int = 0,
    response_limit: int = 10,
    form_offset: int = 0,
    form_limit: int = 20,
    max_bytes: int = 1_048_576,
    body_offset: int = 0,
    body_length: int = 4096,
    max_body_bytes: int = 8 * 1024 * 1024,
) -> dict[str, Any]:
    """Return structured result data instead of mistaking an unreadable scan for no detail."""
    try:
        return _scan_url_detail(
            input_path,
            url,
            response_offset=response_offset,
            response_limit=response_limit,
            form_offset=form_offset,
            form_limit=form_limit,
            max_bytes=max_bytes,
            body_offset=body_offset,
            body_length=body_length,
            max_body_bytes=max_body_bytes,
        )
    except (ValueError, OSError, sqlite3.Error, TypeError) as exc:
        return {"ok": False, "state": "invalid", "error": str(exc)}


def scan_body_read(
    input_path: str,
    url: str,
    *,
    representation: str = "static",
    variant_key: str | None = None,
    offset: int = 0,
    length: int = 4096,
    max_decoded_bytes: int = 8 * 1024 * 1024,
) -> dict[str, Any]:
    """Read one bounded, hash-verified retained body window by exact URL."""
    from seohead.storage.bodies import MAX_WINDOW_CHARS, read_page_document_content

    if not isinstance(url, str) or not url or len(url) > 8192:
        raise ValueError("url must be exact nonempty text of at most 8192 characters")
    if representation not in {"static", "rendered", "legacy_fragment"}:
        raise ValueError("representation must be static, rendered, or legacy_fragment")
    if type(offset) is not int or offset < 0:
        raise ValueError("offset must be a nonnegative integer")
    if type(length) is not int or not 1 <= length <= MAX_WINDOW_CHARS:
        raise ValueError(f"length must be 1..{MAX_WINDOW_CHARS}")
    con, validation = open_scan_mode(_path(input_path, "input"), require_audit=False, light=True)
    try:
        document = read_page_document_content(
            con,
            url,
            representation=representation,
            variant_key=variant_key,
            max_decoded_bytes=max_decoded_bytes,
        )
        if document["state"] != "available":
            return {
                "ok": True,
                **document,
                "url": _detail_url(url),
                "validation": validation,
            }
        text = document.pop("text")
        window = text[offset : offset + length]
        return {
            "ok": True,
            "state": "available",
            "validation": validation,
            **document,
            "url": _detail_url(url),
            "offset": offset,
            "length": len(window),
            "truncated": offset + len(window) < len(text),
            "text": window,
        }
    finally:
        con.close()


def scan_rendered_routes(input_path: str) -> dict[str, Any]:
    """Read stored route observations without crawling or rendering."""
    from seohead.storage.rendered_routes import read

    con = open_scan(_path(input_path, "input"), require_audit=False)
    try:
        return read(con)
    finally:
        con.close()


def scan_snapshot(input_path: str, out: str) -> dict[str, Any]:
    return {"snapshot": snapshot_scan(_path(input_path, "input"), _path(out, "out"))}


def scan_pin(input_path: str, *, pinned: bool = True) -> dict[str, Any]:
    path = _path(input_path, "input")
    if type(pinned) is not bool:
        raise ValueError("pinned must be a boolean")
    pin_scan(path, pinned)
    return {"input": path, "pinned": pinned}


def scan_requeue(
    input_path: str,
    *,
    where: str,
    backup_path: str,
    from_scan: str | None = None,
) -> dict[str, Any]:
    """Explicitly upgrade one scan for a selected retry, retaining a verified backup."""
    from seohead.storage.retry import requeue_scan

    return requeue_scan(
        _path(input_path, "input"),
        where=_path(where, "where"),
        backup_path=_path(backup_path, "backup_path"),
        from_scan=_path(from_scan, "from_scan") if from_scan is not None else None,
    )


def scan_import_urls(input_path: str, *, urls_file: str, backup_path: str) -> dict[str, Any]:
    """Add an external URL list through a scan's stored admission policy."""
    from seohead.storage.retry import scan_import_urls as core

    return core(
        _path(input_path, "input"),
        urls_file=_path(urls_file, "urls_file"),
        backup_path=_path(backup_path, "backup_path"),
    )


def _plan(value: dict[str, Any] | str | None) -> dict[str, Any]:
    if isinstance(value, str) and value:
        with open(value, "rb") as handle:
            raw = handle.read(64 * 1024 * 1024 + 1)
        if len(raw) > 64 * 1024 * 1024:
            raise ValueError("prune plan exceeds 64 MiB")
        value = json.loads(raw)
    if isinstance(value, dict):
        # Accept the exact preview envelope emitted by CLI/MCP, so redirecting
        # stdout to a file produces the same reviewable plan passed on apply.
        if set(value) == {"applied", "plan"} and value["applied"] is False:
            value = value["plan"]
        if isinstance(value, dict):
            return value
    raise ValueError("prune --apply requires a reviewed plan object or JSON file")


def scan_prune(
    directory: str,
    *,
    older_than_days: int = 30,
    keep_newest: int = 5,
    plan: dict[str, Any] | str | None = None,
    apply: bool = False,
) -> dict[str, Any]:
    root = _path(directory, "directory")
    if type(apply) is not bool:
        raise ValueError("apply must be a boolean")
    if not apply:
        if plan is not None:
            raise ValueError("a prune plan is only accepted with apply=true")
        return {
            "applied": False,
            "plan": prune_preview(root, older_than_days=older_than_days, keep_newest=keep_newest),
        }
    removed = prune_apply(root, _plan(plan))
    return {"applied": True, "removed": removed}


def scan_body_diff(
    left: str,
    right: str,
    url: str,
    *,
    variant_key: str | None = None,
    representation: str = "static",
    text: bool = False,
    max_bytes: int = 5 * 1024 * 1024,
    max_lines: int = 10_000,
    left_representation: str | None = None,
    right_representation: str | None = None,
) -> dict[str, Any]:
    left_con = open_scan(_path(left, "left"), require_audit=False)
    try:
        right_con = open_scan(_path(right, "right"), require_audit=False)
        try:
            return body_diff(
                left_con,
                right_con,
                _path(url, "url"),
                variant_key=variant_key,
                representation=representation,
                text=text,
                max_bytes=max_bytes,
                max_lines=max_lines,
                left_representation=left_representation,
                right_representation=right_representation,
            )
        finally:
            right_con.close()
    finally:
        left_con.close()


__all__ = [
    "scan_body_diff",
    "scan_import_urls",
    "scan_inspect",
    "scan_list",
    "scan_pin",
    "scan_prune",
    "scan_rendered_routes",
    "scan_requeue",
    "scan_snapshot",
]
