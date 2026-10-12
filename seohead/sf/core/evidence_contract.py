"""Additive, body-free evidence references for saved audit documents.

The SF audit JSON is deliberately still ``2.0``.  This module adds an
optional contract around it rather than changing the historical wire format:
new native scans can name the saved SQLite ``audit`` observation that supports
each finding, while export-only and older documents explicitly say that such a
reference was not retained.  No helper here opens a page body or contacts a
target.
"""

from __future__ import annotations

import copy
import hashlib
import json
import re
import uuid
from collections.abc import Mapping
from typing import Any

from .registry import CHECKS
from .sf_issue_map import entries as issue_map_entries

CONTRACT_VERSION = "audit_evidence_contract.v1"
SAVED_CORPUS_VERSION = "saved_corpus_derivations.v2"
AUDIT_SCHEMA_VERSION = "2.0"
_ISSUE_ID = re.compile(r"ISSUE-[0-9]{6}")
_TABLE = re.compile(r"[a-z][a-z0-9_]{0,63}")


def stable_evidence_id(*, scan_uuid: str, source_table: str, observation_id: str) -> str:
    """Return an opaque stable identifier for one retained typed observation.

    ``source_table`` and ``observation_id`` remain separate fields in the
    contract for a human or reader to resolve.  The compact hash avoids turning
    arbitrary source identifiers into a report-facing path or payload.
    """
    try:
        normalized_uuid = str(uuid.UUID(scan_uuid))
    except (TypeError, ValueError, AttributeError) as exc:
        raise ValueError("evidence reference requires a UUID scan identity") from exc
    if not isinstance(source_table, str) or not _TABLE.fullmatch(source_table):
        raise ValueError("evidence reference has an invalid typed table")
    if not isinstance(observation_id, str) or not observation_id or len(observation_id) > 256:
        raise ValueError("evidence reference has an invalid observation identity")
    digest = hashlib.sha256(
        json.dumps(
            [CONTRACT_VERSION, normalized_uuid, source_table, observation_id],
            separators=(",", ":"),
        ).encode("utf-8")
    ).hexdigest()[:24]
    return f"evidence:v1:{normalized_uuid}:{source_table}:{digest}"


def _scan_uuid(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    try:
        return str(uuid.UUID(value))
    except ValueError:
        return None


def _items(value: Any) -> list[dict[str, str]]:
    """Return only closed skip/disable records from an audit document."""
    if not isinstance(value, list):
        return []
    result = []
    for item in value:
        if not isinstance(item, Mapping):
            continue
        check_id, reason = item.get("id"), item.get("reason")
        if isinstance(check_id, str) and check_id in CHECKS and isinstance(reason, str):
            result.append({"id": check_id, "reason": reason})
    return result


def _population(document: Mapping[str, Any]) -> dict[str, Any]:
    run = document.get("run") if isinstance(document.get("run"), Mapping) else {}
    summary = document.get("summary") if isinstance(document.get("summary"), Mapping) else {}
    totals = summary.get("totals") if isinstance(summary.get("totals"), Mapping) else {}
    urls = totals.get("urls_crawled")
    representations = totals.get("pages_by_representation")
    return {
        "urls_crawled": urls if type(urls) is int and urls >= 0 else None,
        "crawl_valid": run.get("crawl_valid") is not False,
        "crawl_partial": bool(run.get("crawl_partial")),
        "scope_reason": summary.get("health_score_scope")
        if isinstance(summary.get("health_score_scope"), str)
        else "",
        "representations": dict(representations) if isinstance(representations, Mapping) else {},
    }


def _issue_map_refs(check_id: str) -> list[dict[str, str]]:
    """Return only issue-map rows that explicitly name this registry check."""
    return [
        {"category": category, "name": entry.name, "status": entry.status}
        for category, entry in issue_map_entries()
        if check_id in entry.refs
    ]


def _prerequisites(check_id: str, source: str) -> dict[str, Any]:
    """Derive prerequisite labels from declared source metadata, never from check names."""
    unknown = {
        "state": "unknown",
        "value": None,
        "reason": "registry source does not declare this prerequisite",
    }
    result: dict[str, Any] = {
        "source_tag": source,
        "required_evidence": {
            "state": "unknown",
            "items": [],
            "reason": "registry source has no typed evidence declaration",
        },
        "population": dict(unknown),
        "representation": dict(unknown),
        "issue_map_refs": _issue_map_refs(check_id),
    }
    if source.startswith("SF:"):
        result["required_evidence"] = {
            "state": "derived",
            "items": [{"kind": "screaming_frog_export", "source": source}],
            "reason": "",
        }
        result["population"] = {
            "state": "derived",
            "value": "Screaming Frog export rows",
            "reason": "",
        }
    elif source.startswith("inlinks:"):
        result["required_evidence"] = {
            "state": "derived",
            "items": [{"kind": "screaming_frog_inlinks_export", "source": source}],
            "reason": "",
        }
        result["population"] = {"state": "derived", "value": "inlink relation rows", "reason": ""}
    elif source == "sitemap":
        result["required_evidence"] = {
            "state": "derived",
            "items": [{"kind": "sitemap_observations", "source": source}],
            "reason": "",
        }
        result["population"] = {"state": "derived", "value": "sitemap declarations", "reason": ""}
        result["representation"] = {
            "state": "not_applicable",
            "value": None,
            "reason": "sitemap declarations have no raw/rendered page representation",
        }
    elif source.startswith("crawl:"):
        result["required_evidence"] = {
            "state": "derived",
            "items": [{"kind": "native_crawl_evidence", "source": source}],
            "reason": "",
        }
        result["population"] = {
            "state": "derived",
            "value": "native crawl evidence records",
            "reason": "",
        }
    elif source.startswith("provider:"):
        result["required_evidence"] = {
            "state": "derived",
            "items": [{"kind": "retained_project_provider_history", "source": source}],
            "reason": "provider findings require an explicitly bound local property and retained coverage",
        }
        result["population"] = {
            "state": "derived",
            "value": "audited crawl URLs joined to the source's retained reporting interval",
            "reason": "the exact native provider interval and coverage are attached to the finding",
        }
        result["representation"] = {
            "state": "not_applicable",
            "value": None,
            "reason": "provider observations are not page-body representation checks",
        }
    return result


def capability_rows(document: Mapping[str, Any]) -> list[dict[str, Any]]:
    """Describe every registered check without treating missing data as clean.

    ``check_coverage`` and the run's named skipped/disabled declarations are
    the authoritative completion facts.  A check that has no finding and is
    not listed in the silent inventory is *unmeasured*, not a silent success.
    """
    run = document.get("run") if isinstance(document.get("run"), Mapping) else {}
    summary = document.get("summary") if isinstance(document.get("summary"), Mapping) else {}
    coverage = (
        summary.get("check_coverage")
        if isinstance(summary.get("check_coverage"), Mapping)
        else None
    )
    issues = document.get("issues") if isinstance(document.get("issues"), list) else []
    fired = {
        item.get("check")
        for item in issues
        if isinstance(item, Mapping)
        and isinstance(item.get("check"), str)
        and item["check"] in CHECKS
    }
    skipped = {item["id"]: item["reason"] for item in _items(run.get("checks_skipped"))}
    disabled = {item["id"]: item["reason"] for item in _items(run.get("checks_disabled"))}
    if isinstance(coverage, Mapping):
        coverage_disabled = coverage.get("checks_disabled_ids")
        if isinstance(coverage_disabled, list):
            for check_id in coverage_disabled:
                if isinstance(check_id, str) and check_id in CHECKS:
                    disabled.setdefault(check_id, "disabled by the saved audit configuration")
        silent_raw = coverage.get("checks_silent_ids")
        silent = (
            {
                check_id
                for check_id in silent_raw
                if isinstance(check_id, str) and check_id in CHECKS
            }
            if isinstance(silent_raw, list)
            else set()
        )
    else:
        silent = set()
    partial = bool(run.get("crawl_partial"))
    rows: list[dict[str, Any]] = []
    for check_id in sorted(CHECKS):
        meta = CHECKS[check_id]
        row: dict[str, Any] = {
            "check": check_id,
            "source": str(meta["source"]),
            "prerequisites": _prerequisites(check_id, str(meta["source"])),
            "state": "unmeasured",
            "capability": "unaccounted",
            "reason": "the saved audit does not account for this check",
        }
        if check_id in fired:
            row.update(state="measured", capability="finding_recorded", reason="")
        elif check_id in disabled:
            row.update(state="unmeasured", capability="disabled", reason=disabled[check_id])
        elif check_id in skipped:
            row.update(state="unmeasured", capability="skipped", reason=skipped[check_id])
        elif check_id in silent:
            row.update(
                state="partial" if partial else "measured",
                capability="ran_without_finding",
                reason=(
                    "crawl was partial; this clean result covers only the saved scope"
                    if partial
                    else ""
                ),
            )
        rows.append(row)
    return rows


def _finding_projection(issue: Mapping[str, Any], scan_uuid: str | None) -> dict[str, Any]:
    """Name the saved audit issue separately from its underlying observations."""
    issue_id = issue.get("id")
    if scan_uuid is None:
        return {
            "state": "unavailable",
            "reason": "legacy or export-only audit has no retained scan UUID",
        }
    if not isinstance(issue_id, str) or not _ISSUE_ID.fullmatch(issue_id):
        return {
            "state": "unavailable",
            "reason": "saved audit finding has no stable ISSUE identifier",
        }
    observation_id = f"issue:{issue_id}"
    return {
        "state": "measured",
        "role": "finding_projection",
        "id": stable_evidence_id(
            scan_uuid=scan_uuid, source_table="audit", observation_id=observation_id
        ),
        "scan_uuid": scan_uuid,
        "source_table": "audit",
        "observation_id": observation_id,
    }


def _saved_scan(con: Any, scan_uuid: str | None) -> tuple[str | None, str | None]:
    """Return the retained scan source or an explicit binding failure reason."""
    if con is None:
        return None, "underlying saved observations were not supplied"
    if scan_uuid is None:
        return None, "saved scan has no valid UUID binding"
    try:
        row = con.execute("SELECT scan_uuid,source_kind FROM scan WHERE singleton=1").fetchone()
    except Exception:
        return None, "saved scan cannot resolve retained observations"
    if row is None:
        return None, "saved scan header is unavailable"
    try:
        stored_uuid, source_kind = row[0], row[1]
    except (IndexError, KeyError):
        return None, "saved scan header has an unsupported shape"
    if _scan_uuid(stored_uuid) != scan_uuid:
        return None, "saved scan identity does not match the audit binding"
    if source_kind not in {"native", "reanalysis", "legacy_import"}:
        return None, "saved scan source kind is unsupported"
    return str(source_kind), None


def _stored_page_observations(
    con: Any, *, scan_uuid: str, url: str, role: str, source_kind: str
) -> list[dict[str, Any]]:
    """Resolve one audit URL to actual page/document/response records only."""
    try:
        row = con.execute(
            "SELECT p.url_id,p.document_id,p.representation,d.source_response_id,r.response_id "
            "FROM urls u JOIN pages p ON p.url_id=u.url_id "
            "LEFT JOIN documents d ON d.document_id=p.document_id "
            "LEFT JOIN responses r ON r.response_id=d.source_response_id WHERE u.url=?",
            (url,),
        ).fetchone()
    except Exception:
        return []
    if row is None:
        return []
    try:
        url_id, document_id, representation, source_response_id, response_id = row
    except (TypeError, ValueError):
        return []
    if type(url_id) is not int or url_id < 1:
        return []
    state = "imported_projection" if source_kind == "legacy_import" else "measured"
    output = [
        {
            "state": state,
            "role": role,
            "id": stable_evidence_id(
                scan_uuid=scan_uuid, source_table="pages", observation_id=f"url_id:{url_id}"
            ),
            "scan_uuid": scan_uuid,
            "source_table": "pages",
            "observation_id": f"url_id:{url_id}",
            "representation": representation if isinstance(representation, str) else "",
        }
    ]
    output.append(
        {
            "state": state,
            "role": role,
            "id": stable_evidence_id(
                scan_uuid=scan_uuid, source_table="urls", observation_id=f"url_id:{url_id}"
            ),
            "scan_uuid": scan_uuid,
            "source_table": "urls",
            "observation_id": f"url_id:{url_id}",
            "representation": "",
        }
    )
    if type(document_id) is int and document_id > 0:
        output.append(
            {
                "state": state,
                "role": role,
                "id": stable_evidence_id(
                    scan_uuid=scan_uuid,
                    source_table="documents",
                    observation_id=f"document_id:{document_id}",
                ),
                "scan_uuid": scan_uuid,
                "source_table": "documents",
                "observation_id": f"document_id:{document_id}",
                "representation": representation if isinstance(representation, str) else "",
            }
        )
    if (
        type(source_response_id) is int
        and source_response_id > 0
        and source_response_id == response_id
    ):
        output.append(
            {
                "state": state,
                "role": role,
                "id": stable_evidence_id(
                    scan_uuid=scan_uuid,
                    source_table="responses",
                    observation_id=f"response_id:{source_response_id}",
                ),
                "scan_uuid": scan_uuid,
                "source_table": "responses",
                "observation_id": f"response_id:{source_response_id}",
                "representation": representation if isinstance(representation, str) else "",
            }
        )
    return output


def _language_observations(
    con: Any, *, scan_uuid: str, observations: list[dict[str, Any]], source_kind: str
) -> list[dict[str, Any]]:
    """Return saved hreflang declaration observations for already-resolved documents."""
    if source_kind == "legacy_import":
        return []
    documents = [
        item
        for item in observations
        if item.get("source_table") == "documents" and item.get("role") == "target"
    ]
    output: list[dict[str, Any]] = []
    for document in documents:
        observation_id = document.get("observation_id")
        representation = document.get("representation")
        if not isinstance(observation_id, str) or not observation_id.startswith("document_id:"):
            continue
        try:
            document_id = int(observation_id.split(":", 1)[1])
        except ValueError:
            continue
        try:
            row = con.execute(
                "SELECT item_key,payload_json FROM context_items WHERE kind='language_evidence' "
                "AND item_key LIKE ? ORDER BY item_key LIMIT 1",
                (f"%:document:{document_id}:representation:%",),
            ).fetchone()
        except Exception:
            continue
        if row is None:
            continue
        try:
            item_key, payload_text = row
            payload = json.loads(payload_text)
            declarations = payload.get("declarations") if isinstance(payload, dict) else []
        except (TypeError, ValueError, json.JSONDecodeError):
            continue
        if not isinstance(item_key, str) or not isinstance(declarations, list):
            continue
        if not declarations:
            state = (
                payload.get("state")
                if isinstance(payload, dict) and isinstance(payload.get("state"), str)
                else "unknown"
            )
            output.append(
                {
                    "state": "measured",
                    "role": "declaration",
                    "id": stable_evidence_id(
                        scan_uuid=scan_uuid,
                        source_table="context_items",
                        observation_id=f"language_evidence:{item_key}:state:{state}",
                    ),
                    "scan_uuid": scan_uuid,
                    "source_table": "context_items",
                    "observation_id": f"language_evidence:{item_key}:state:{state}",
                    "representation": representation if isinstance(representation, str) else "",
                }
            )
        for declaration in declarations[:100]:
            if not isinstance(declaration, Mapping) or type(declaration.get("ordinal")) is not int:
                continue
            ordinal = declaration["ordinal"]
            output.append(
                {
                    "state": "measured",
                    "role": "declaration",
                    "id": stable_evidence_id(
                        scan_uuid=scan_uuid,
                        source_table="context_items",
                        observation_id=f"language_evidence:{item_key}:ordinal:{ordinal}",
                    ),
                    "scan_uuid": scan_uuid,
                    "source_table": "context_items",
                    "observation_id": f"language_evidence:{item_key}:ordinal:{ordinal}",
                    "representation": representation if isinstance(representation, str) else "",
                }
            )
    return output


def _image_resource_observations(con: Any, issue: Mapping[str, Any], scan_uuid: str):
    """Reference actual retained graph rows, not a source page's response."""
    if not str(issue.get("check", "")).startswith("IMG_"):
        return []
    details = issue.get("details") if isinstance(issue.get("details"), Mapping) else {}
    resource = details.get("resource_url") if "resource_url" in details else issue.get("target_url")
    if not isinstance(resource, str) or not resource:
        return []
    output = []

    def reference(table, ident, role, representation=""):
        return {
            "state": "measured",
            "role": role,
            "id": stable_evidence_id(scan_uuid=scan_uuid, source_table=table, observation_id=ident),
            "scan_uuid": scan_uuid,
            "source_table": table,
            "observation_id": ident,
            "representation": representation,
        }

    try:
        row = con.execute(
            "SELECT status_code,bytes_received FROM resource_graph_fetches WHERE resolved_url=?",
            (resource,),
        ).fetchone()
        if row is not None and (row[0] is not None or row[1] > 0):
            key = "url_sha256:" + hashlib.sha256(resource.encode("utf-8")).hexdigest()
            output.append(reference("resource_graph_fetches", key, "target_resource"))
        source = details.get("source_page")
        if isinstance(source, str) and source:
            document_id = details.get("document_id")
            where = " AND o.source_document_id=?" if type(document_id) is int else ""
            parameters = (resource, source, document_id) if where else (resource, source)
            row = con.execute(
                "SELECT o.occurrence_id,o.representation FROM resource_graph_occurrences o "
                "JOIN urls u ON u.url_id=o.page_url_id WHERE o.resolved_url=? AND u.url=? "
                + where
                + " ORDER BY o.occurrence_id LIMIT 1",
                parameters,
            ).fetchone()
            if row is not None:
                output.append(
                    reference(
                        "resource_graph_occurrences",
                        f"occurrence_id:{row[0]}",
                        "source_declaration",
                        row[1],
                    )
                )
    except Exception:
        pass  # Older artifacts may have no graph or declaration binding columns.
    return output


def _underlying_references(
    issue: Mapping[str, Any], *, con: Any, scan_uuid: str | None
) -> tuple[list[dict[str, Any]], str]:
    """Resolve target/source/declaration rows without making a network request."""
    source_kind, unavailable_reason = _saved_scan(con, scan_uuid)
    if unavailable_reason:
        return [], unavailable_reason
    assert source_kind is not None and scan_uuid is not None
    observations: list[dict[str, Any]] = []
    observations.extend(_image_resource_observations(con, issue, scan_uuid))
    target_url = issue.get("target_url")
    if isinstance(target_url, str) and target_url:
        observations.extend(
            _stored_page_observations(
                con, scan_uuid=scan_uuid, url=target_url, role="target", source_kind=source_kind
            )
        )
    sources: set[str] = set()
    details = issue.get("details")
    source_page = details.get("source_page") if isinstance(details, Mapping) else None
    if isinstance(source_page, str) and source_page:
        sources.add(source_page)
    locations = issue.get("locations")
    if isinstance(locations, list):
        for location in locations[:100]:
            source_url = location.get("source_url") if isinstance(location, Mapping) else None
            if isinstance(source_url, str) and source_url:
                sources.add(source_url)
    for source_url in sorted(sources):
        observations.extend(
            _stored_page_observations(
                con, scan_uuid=scan_uuid, url=source_url, role="source", source_kind=source_kind
            )
        )
    check = issue.get("check")
    if isinstance(check, str) and check.startswith("HREFLANG_"):
        observations.extend(
            _language_observations(
                con, scan_uuid=scan_uuid, observations=observations, source_kind=source_kind
            )
        )
    if observations:
        return observations, ""
    if source_kind == "legacy_import":
        return [], "legacy import has no captured page/document/response observations"
    return [], "no retained page/document/response observation matches this finding"


def _issue_reference(
    issue: Mapping[str, Any], *, con: Any, scan_uuid: str | None
) -> dict[str, Any]:
    """Return underlying observations first; the audit ordinal stays a projection."""
    finding = _finding_projection(issue, scan_uuid)
    observations, reason = _underlying_references(issue, con=con, scan_uuid=scan_uuid)
    if not observations:
        return {
            "state": "unavailable",
            "reason": reason,
            "finding": finding,
            "observations": [],
        }
    primary = dict(observations[0])
    primary["finding"] = finding
    primary["observations"] = observations
    return primary


def attach_contract(
    document: Mapping[str, Any], *, scan_uuid: str | None = None, con: Any = None
) -> dict[str, Any]:
    """Return a copied audit document with additive capability and evidence IDs.

    The function deliberately keeps ``schema_version`` at ``2.0``; callers can
    use it before reports/tasks are built without breaking existing audit JSON
    readers.  A UUID supplied by a scan header wins over any run field so an
    integration can bind an exported document to its retained scan explicitly.
    ``con`` is an already-open scan connection; when supplied, references bind
    to stored pages/documents/responses and (where present) language contexts.
    """
    projected = copy.deepcopy(dict(document))
    if projected.get("schema_version") != AUDIT_SCHEMA_VERSION:
        raise ValueError("evidence contract supports only audit.json schema_version 2.0")
    run = projected.get("run") if isinstance(projected.get("run"), dict) else {}
    if not isinstance(projected.get("run"), dict):
        projected["run"] = run
    summary = projected.get("summary") if isinstance(projected.get("summary"), dict) else {}
    if not isinstance(projected.get("summary"), dict):
        projected["summary"] = summary
    identity = _scan_uuid(scan_uuid) or _scan_uuid(run.get("scan_uuid"))
    summary["evidence_contract"] = {
        "schema_version": CONTRACT_VERSION,
        "audit_schema_version": projected.get("schema_version"),
        "scan_uuid": identity,
        "scan_identity_state": "measured" if identity else "unavailable",
        "scan_identity_reason": "" if identity else "no retained scan UUID in this audit document",
        "population": _population(projected),
        "capability_rows": capability_rows(projected),
    }
    issues = projected.get("issues")
    if isinstance(issues, list):
        for issue in issues:
            if not isinstance(issue, dict):
                continue
            saved_evidence = issue.get("evidence")
            evidence = dict(saved_evidence) if isinstance(saved_evidence, Mapping) else {}
            evidence["contract"] = _issue_reference(issue, con=con, scan_uuid=identity)
            issue["evidence"] = evidence
    return projected


def attach_contract_parts(header, issues, *, scan_uuid: str | None = None, con: Any = None):
    """Attach contract metadata while preserving a re-iterable issue collection."""
    projected = copy.deepcopy(dict(header))
    if projected.get("schema_version") != AUDIT_SCHEMA_VERSION:
        raise ValueError("evidence contract supports only audit.json schema_version 2.0")
    run = projected.get("run") if isinstance(projected.get("run"), dict) else {}
    projected["run"] = run
    summary = projected.get("summary") if isinstance(projected.get("summary"), dict) else {}
    projected["summary"] = summary
    identity = _scan_uuid(scan_uuid) or _scan_uuid(run.get("scan_uuid"))
    fired = sorted({item.get("check") for item in issues if isinstance(item.get("check"), str)})
    summary["evidence_contract"] = {
        "schema_version": CONTRACT_VERSION,
        "audit_schema_version": projected.get("schema_version"),
        "scan_uuid": identity,
        "scan_identity_state": "measured" if identity else "unavailable",
        "scan_identity_reason": "" if identity else "no retained scan UUID in this audit document",
        "population": _population(projected),
        "capability_rows": capability_rows(
            {**projected, "issues": [{"check": check} for check in fired]}
        ),
    }

    def rows():
        for issue in issues:
            item = dict(issue)
            evidence = dict(item.get("evidence") or {})
            evidence["contract"] = _issue_reference(item, con=con, scan_uuid=identity)
            item["evidence"] = evidence
            yield item

    from .models import _Rows

    return projected, _Rows(rows)


def attach_saved_corpus(
    document: Mapping[str, Any],
    con: Any,
    *,
    duplicate_threshold: float = 0.92,
    derived: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Attach already-stored corpus derivations at the audit boundary.

    ``con`` is the caller's already-open scan connection.  The only consumer is
    :func:`seohead.sf.core.corpus_derivations.derive`, which reads typed context
    items and does not fetch, render, or retain bodies.  This is intentionally a
    separate pure hook so legacy and export-only callers stay explicit.
    """
    from .corpus_derivations import derive

    projected = copy.deepcopy(dict(document))
    if projected.get("schema_version") != AUDIT_SCHEMA_VERSION:
        raise ValueError("saved corpus attachment supports only audit.json schema_version 2.0")
    summary = projected.get("summary") if isinstance(projected.get("summary"), dict) else {}
    if not isinstance(projected.get("summary"), dict):
        projected["summary"] = summary
    derived = (
        dict(derived)
        if derived is not None
        else derive(con, duplicate_threshold=duplicate_threshold)
    )
    if derived.get("schema_version") != SAVED_CORPUS_VERSION:
        raise ValueError("saved corpus derivation has an unsupported schema version")
    summary["saved_corpus_derivations"] = derived
    return projected


def attach_saved_corpus_header(
    header, con: Any, *, derived: Mapping[str, Any] | None = None, collections=None
):
    """Attach corpus derivations to an audit header without reading collections."""
    projected = copy.deepcopy(dict(header))
    if projected.get("schema_version") != AUDIT_SCHEMA_VERSION:
        raise ValueError("saved corpus attachment supports only audit.json schema_version 2.0")
    summary = projected.get("summary") if isinstance(projected.get("summary"), dict) else {}
    projected["summary"] = summary
    from .corpus_derivations import derive as derive_corpus

    payload = dict(derived) if derived is not None else derive_corpus(con)
    if payload.get("schema_version") != SAVED_CORPUS_VERSION:
        raise ValueError("saved corpus derivation has an unsupported schema version")
    if collections is not None:
        from .models import _Rows

        for section, names in {
            "duplicates": ("exact_groups", "near_witnesses", "excluded"),
            "structured": ("items",),
            "internationalization": ("items", "declarations"),
        }.items():
            block = dict(payload[section])
            payload[section] = block
            for name in names:
                rows = block[name]
                pointer = f"/summary/saved_corpus_derivations/{section}/{name}"
                collections[pointer] = _Rows(lambda rows=rows: iter(rows))
                block[name] = []
    summary["saved_corpus_derivations"] = payload
    return projected


def comparison_compatibility(
    before: Mapping[str, Any], after: Mapping[str, Any]
) -> list[dict[str, Any]]:
    """Return named comparison bases without inferring equality from absence."""

    def side(document: Mapping[str, Any], key: str) -> Any:
        run = document.get("run") if isinstance(document.get("run"), Mapping) else {}
        summary = document.get("summary") if isinstance(document.get("summary"), Mapping) else {}
        totals = summary.get("totals") if isinstance(summary.get("totals"), Mapping) else {}
        contract = (
            summary.get("evidence_contract")
            if isinstance(summary.get("evidence_contract"), Mapping)
            else {}
        )
        corpus = summary.get("saved_corpus_derivations")
        if key == "scope":
            if "crawl_valid" not in run or "crawl_partial" not in run:
                return None
            return {
                "crawl_valid": run.get("crawl_valid"),
                "crawl_partial": run.get("crawl_partial"),
                "scope": summary.get("health_score_scope"),
            }
        if key == "configuration":
            if not isinstance(run.get("crawl_config"), Mapping):
                return None
            return {
                "crawl_config": run.get("crawl_config"),
                "profile": run.get("profile"),
                "config_fingerprint": run.get("config_fingerprint"),
            }
        if key == "representation":
            representations = totals.get("pages_by_representation")
            return dict(representations) if isinstance(representations, Mapping) else None
        if key == "corpus":
            if not isinstance(corpus, Mapping):
                return None
            return {
                "corpus_partial": run.get("corpus_partial"),
                "schema_version": corpus.get("schema_version"),
                "scan_identity": contract.get("scan_identity_state"),
            }
        if not isinstance(run.get("input_mode"), str):
            return None
        return {
            "input_mode": run.get("input_mode"),
            "collector": run.get("collector"),
            "provider": run.get("provider"),
            "source_kind": run.get("source_kind"),
        }

    result: list[dict[str, Any]] = []
    for key in ("scope", "configuration", "representation", "corpus", "provider"):
        before_value, after_value = side(before, key), side(after, key)
        unknown = before_value is None or after_value is None
        # A dictionary with only absent values is also an unknown basis.
        if isinstance(before_value, dict) and not any(
            value is not None for value in before_value.values()
        ):
            unknown = True
        if isinstance(after_value, dict) and not any(
            value is not None for value in after_value.values()
        ):
            unknown = True
        if key == "corpus" and any(
            not isinstance(value, dict) or not isinstance(value.get("corpus_partial"), bool)
            for value in (before_value, after_value)
        ):
            unknown = True
        if unknown:
            state = "unknown"
        elif before_value == after_value:
            state = "compatible"
        else:
            state = "incompatible"
        result.append({"basis": key, "state": state, "before": before_value, "after": after_value})
    return result


def comparison_warnings(before: Mapping[str, Any], after: Mapping[str, Any]) -> list[str]:
    """Render comparison bases as concise warnings for a caller's preflight."""
    warnings = []
    for row in comparison_compatibility(before, after):
        if row["basis"] == "corpus":
            for side in ("before", "after"):
                value = row[side]
                if isinstance(value, dict) and value.get("corpus_partial") is True:
                    warnings.append(
                        f"{side} saved corpus is partial; missing evidence is not a verified fix"
                    )
        if row["state"] == "compatible":
            continue
        if row["state"] == "unknown":
            warnings.append(
                f"comparison {row['basis']} basis is unavailable; comparability is unknown"
            )
        else:
            warnings.append(f"comparison {row['basis']} basis differs between saved audits")
    return warnings
