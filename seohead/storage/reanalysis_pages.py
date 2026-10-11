"""Pure, bounded reconstruction of one native scan page at a time."""

from __future__ import annotations

import dataclasses
import json
import sqlite3
from collections.abc import Iterator
from dataclasses import dataclass
from typing import Any, cast
from urllib.parse import urlsplit

from seohead.crawl.collect import PageRecord, _apply_body
from seohead.crawl.render_escalation import fold_rendered_evidence
from seohead.crawl.spider import LinkEdge, Scope
from seohead.crawl.sqlite_adapter import _document_batch, _parse_options

from . import ScanError
from .bodies import read_document

_TRANSPORT_FIELDS = {
    "url",
    "status_code",
    "content_type",
    "size_bytes",
    "response_time",
    "redirect_url",
    "x_robots",
    "http_refresh",
    "crawl_depth",
    "content_encoding",
    "error",
    "error_kind",
    "cache_status",
    "final_url",
    "final_canonical",
}


@dataclass(frozen=True)
class PageReplay:
    """One replayed page and its separately attributable document observations."""

    page: dict[str, Any]
    static_document_id: int | None
    selected_document_id: int | None
    links: dict[str, tuple[dict[str, Any], ...]]
    forms: dict[str, tuple[dict[str, Any], ...]]
    resources: dict[str, tuple[dict[str, Any], ...]]
    representation_facts: dict[str, dict[str, Any]]
    raw_html: str | None
    selected_html: str | None
    start_page_gate: dict[str, Any] | None


def _unavailable(url: str, representation: str, reason: str) -> ScanError:
    return ScanError(f"reanalysis unavailable: {representation} document for {url}: {reason}")


def _body_limit(config: dict[str, Any]) -> int:
    storage = config.get("storage")
    if not isinstance(storage, dict):
        raise ScanError("reanalysis unavailable: recorded scan has no body-retention configuration")
    limit = storage.get("max_body_bytes")
    if type(limit) is not int or limit <= 0:
        raise ScanError("reanalysis unavailable: recorded body byte limit is invalid")
    return limit


def _parse_limit(config: dict[str, Any]) -> int:
    limits = config.get("limits")
    value = limits.get("max_response_bytes") if isinstance(limits, dict) else None
    if type(value) is not int:
        raise ScanError("reanalysis unavailable: recorded parser byte limit is invalid")
    if value <= 0:
        raise ScanError("reanalysis unavailable: recorded parser byte limit is invalid")
    return value


def _record(row: sqlite3.Row) -> PageRecord:
    """Keep only static transport/discovery facts before parsing a retained body."""
    values = {name: row[name] for name in _TRANSPORT_FIELDS}
    values["redirect_chain"] = json.loads(row["redirect_chain_json"])
    # The canonical walk (#21) is a bounded set of *extra* fetches list mode made at
    # crawl time; reanalysis only reparses the retained body, so it cannot repeat
    # those requests. The chain is retained transport evidence, not a live result.
    stored_canonical_chain = row["canonical_chain_json"]
    values["canonical_chain"] = (
        [] if stored_canonical_chain is None else json.loads(stored_canonical_chain)
    )
    values["representation"] = "static"
    return PageRecord(**values)


def _document(con: sqlite3.Connection, document_id: int | None) -> sqlite3.Row | None:
    if document_id is None:
        return None
    return cast(
        sqlite3.Row | None,
        con.execute("SELECT * FROM documents WHERE document_id=?", (document_id,)).fetchone(),
    )


def _active_static_document(con: sqlite3.Connection, page: sqlite3.Row) -> int | None:
    if page["representation"] == "static":
        document_id = page["document_id"]
        return document_id if type(document_id) is int else None
    rows = list(
        con.execute(
            "SELECT d.document_id FROM documents d JOIN context_items c "
            "ON c.item_key='document:'||d.document_id "
            "WHERE c.kind='resource_inventory' AND d.url_id=? AND d.representation='static' "
            "AND NOT EXISTS(SELECT 1 FROM context_items later JOIN documents newer "
            "ON later.item_key='document:'||newer.document_id "
            "WHERE later.kind='resource_inventory' AND newer.url_id=d.url_id "
            "AND newer.representation=d.representation AND newer.document_id>d.document_id) "
            "ORDER BY d.document_id LIMIT 2",
            (page["url_id"],),
        )
    )
    if not rows:
        return None
    if len(rows) != 1:
        raise _unavailable(page["url"], "static", "ambiguous resource inventory")
    return int(rows[0][0])


def _document_final_url(con: sqlite3.Connection, document: sqlite3.Row, fallback: str) -> str:
    if document["representation"] == "static":
        response_id = document["source_response_id"]
        if type(response_id) is not int:
            raise _unavailable(fallback, "static", "source response is absent")
        row = con.execute(
            "SELECT u.url FROM responses r JOIN urls u ON u.url_id=r.effective_url_id "
            "WHERE r.response_id=?",
            (response_id,),
        ).fetchone()
        if row is None:
            raise _unavailable(fallback, "static", "response effective URL is absent")
        return str(row[0])
    try:
        renderer = json.loads(document["renderer_json"])
    except (TypeError, ValueError) as exc:
        raise _unavailable(
            fallback, document["representation"], "invalid renderer provenance"
        ) from exc
    final_url_id = renderer.get("final_url_id") if isinstance(renderer, dict) else None
    if type(final_url_id) is not int:
        raise _unavailable(fallback, document["representation"], "missing renderer final URL")
    row = con.execute("SELECT url FROM urls WHERE url_id=?", (final_url_id,)).fetchone()
    if row is None:
        raise _unavailable(fallback, document["representation"], "renderer final URL is absent")
    return str(row[0])


def _facts(
    document_id: int | None, document: sqlite3.Row | None, batch: Any, parsed: Any
) -> dict[str, Any]:
    if document is None:
        return {
            "document_id": document_id,
            "state": "unavailable",
            "reason": "not_in_corpus",
            "partial_reasons": (),
            "resource_inventory_state": "unavailable",
            "resource_inventory_reason": "document is not in the retained corpus",
            "resource_omitted": 0,
            "head_links": None,
        }
    has_resources = "resource_declarations" in (parsed or {})
    omitted = int((parsed or {}).get("resource_declarations_omitted") or 0)
    resource_state = "partial" if omitted else "complete" if has_resources else "unavailable"
    return {
        "document_id": document_id,
        "state": document["body_state"],
        "reason": document["body_reason"],
        "partial_reasons": tuple(batch.partial_reasons),
        "resource_inventory_state": resource_state,
        "resource_inventory_reason": ""
        if has_resources
        else "resource declarations were not parsed",
        "resource_omitted": omitted,
        "head_links": (parsed or {}).get("head_links"),
    }


def _parse_document(
    con: sqlite3.Connection,
    document: sqlite3.Row,
    record: PageRecord,
    *,
    url: str,
    parse_options: dict[str, Any],
    body_limit: int,
    parse_limit: int,
) -> tuple[str, dict[str, Any] | None]:
    if document["body_state"] != "complete" or document["body_sha256"] is None:
        raise _unavailable(url, document["representation"], str(document["body_reason"]))
    html = read_document(con, int(document["document_id"]), max_decoded_bytes=body_limit)
    parsed = _apply_body(
        record,
        _document_final_url(con, document, url),
        html,
        parse_options=parse_options,
        max_response_bytes=parse_limit,
        size_bytes=record.size_bytes,
    )
    if record.is_html and html and parsed is None and record.body_unavailable != "oversized":
        raise _unavailable(url, document["representation"], "retained HTML could not be parsed")
    return html, parsed


def _observations(
    parsed: dict[str, Any] | None,
    *,
    url: str,
    depth: int,
    scope: Scope,
    host: str,
    config: dict[str, Any],
) -> tuple[Any, tuple[dict[str, Any], ...], tuple[dict[str, Any], ...], tuple[dict[str, Any], ...]]:
    batch = _document_batch(
        parsed, source_url=url, depth=depth, scope=scope, start_host=host, settings=config
    )
    resources = tuple((parsed or {}).get("resource_declarations") or ())
    return batch, tuple(batch.links), tuple(batch.forms), resources


def iterate_reparsed_pages(
    source_con: sqlite3.Connection, config: dict[str, Any]
) -> Iterator[PageReplay]:
    """Yield re-parsed source pages without fetching, writing, or retaining a run-wide graph."""
    if not isinstance(source_con, sqlite3.Connection) or not isinstance(config, dict):
        raise ScanError("offline reanalysis requires a SQLite source and recorded configuration")
    body_limit = _body_limit(config)
    parse_limit = _parse_limit(config)
    parse_options = _parse_options(config, None)
    scope = Scope.from_config(config.get("scope"))
    scan = source_con.execute("SELECT start_url FROM scan WHERE singleton=1").fetchone()
    if scan is None or not isinstance(scan[0], str) or not scan[0]:
        raise ScanError("reanalysis unavailable: source scan has no start URL")
    start_url = scan[0]
    from seohead.crawl.list_scan import is_list_scan

    list_mode = is_list_scan(source_con)
    host = urlsplit(start_url).hostname or ""
    if not host and not list_mode:
        raise ScanError("reanalysis unavailable: source start URL has no host")
    cursor = source_con.execute(
        "SELECT p.*,u.url FROM pages p JOIN urls u USING(url_id) ORDER BY p.page_ordinal"
    )
    for page in cursor:
        if list_mode:
            host = urlsplit(page["url"]).hostname or ""
        record = _record(page)
        static_id = _active_static_document(source_con, page)
        selected_id = page["document_id"]
        static_doc = _document(source_con, static_id)
        selected_doc = _document(source_con, selected_id)
        raw_html: str | None = None
        selected_html: str | None = None
        start_page_gate: dict[str, Any] | None = None
        raw_parsed: dict[str, Any] | None = None
        raw_batch: Any = _document_batch(
            None,
            source_url=record.url,
            depth=record.crawl_depth,
            scope=scope,
            start_host=host,
            settings=config,
        )
        if record.is_html and record.status_code is not None:
            if static_doc is None:
                raise _unavailable(record.url, "static", "no active resource-inventory document")
            html, raw_parsed = _parse_document(
                source_con,
                static_doc,
                record,
                url=record.url,
                parse_options=parse_options,
                body_limit=body_limit,
                parse_limit=parse_limit,
            )
            raw_batch, raw_links, raw_forms, raw_resources = _observations(
                raw_parsed,
                url=record.url,
                depth=record.crawl_depth,
                scope=scope,
                host=host,
                config=config,
            )
            if record.url == start_url:
                raw_html = html
                start_page_gate = {
                    "html": html,
                    "outlinks": record.outlinks,
                    "external_outlinks": record.external_outlinks,
                }
        else:
            raw_links = raw_forms = raw_resources = ()

        links: dict[str, tuple[dict[str, Any], ...]] = {"static": raw_links}
        forms: dict[str, tuple[dict[str, Any], ...]] = {"static": raw_forms}
        resources: dict[str, tuple[dict[str, Any], ...]] = {"static": raw_resources}
        facts = {"static": _facts(static_id, static_doc, raw_batch, raw_parsed)}

        if page["representation"] == "static":
            selected_html = raw_html
        else:
            representation = str(page["representation"])
            if selected_doc is None:
                raise _unavailable(record.url, representation, "selected document is absent")
            selected_html_value, selected_parsed = _parse_document(
                source_con,
                selected_doc,
                dataclasses.replace(record),
                url=record.url,
                parse_options=parse_options,
                body_limit=body_limit,
                parse_limit=parse_limit,
            )
            raw_edges = [LinkEdge(**edge) for edge in raw_links]
            _ignored, _degenerate = fold_rendered_evidence(
                record,
                raw_edges,
                record.url,
                {
                    "html": selected_html_value,
                    "final_url": _document_final_url(source_con, selected_doc, record.url),
                },
                representation,
                parse_options=parse_options,
                max_response_bytes=parse_limit,
            )
            selected_batch, selected_links, selected_forms, selected_resources = _observations(
                selected_parsed,
                url=record.url,
                depth=record.crawl_depth,
                scope=scope,
                host=host,
                config=config,
            )
            links[representation] = selected_links
            forms[representation] = selected_forms
            resources[representation] = selected_resources
            facts[representation] = _facts(
                selected_id, selected_doc, selected_batch, selected_parsed
            )
            if record.url == start_url:
                selected_html = selected_html_value

        yield PageReplay(
            page=dataclasses.asdict(record),
            static_document_id=static_id,
            selected_document_id=selected_id,
            links=links,
            forms=forms,
            resources=resources,
            representation_facts=facts,
            raw_html=raw_html,
            selected_html=selected_html,
            start_page_gate=start_page_gate,
        )
