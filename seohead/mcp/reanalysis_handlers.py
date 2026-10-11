"""Offline scan orchestration over the existing parser and audit registry."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any


def reanalyze_scan(
    input_path: str,
    out: str,
    producer_build: str | None = None,
    step: str = "all",
    max_documents: int | None = None,
) -> dict[str, Any]:
    """Publish a new analysis only after retained inputs and output validate."""
    if not isinstance(input_path, str) or not input_path:
        raise ValueError("input_path must name a retained SQLite scan")
    if not isinstance(out, str) or not out:
        raise ValueError("out must name a new derived SQLite scan")
    if step not in {"all", "links.position"}:
        raise ValueError("step must be all or links.position")
    if max_documents is not None and (
        step != "links.position" or type(max_documents) is not int or max_documents < 1
    ):
        raise ValueError("max_documents is a positive batch budget for step=links.position")

    from seohead.crawl.list_scan import is_list_scan
    from seohead.crawl.sql_sitemap import prepare_sitemap_reconciliation
    from seohead.mcp.handlers import _audit_crawl_result
    from seohead.mcp.scan_handlers import (
        _producer_provenance,
        _rebuild_page_result,
    )
    from seohead.storage.link_positions import backfill_positions
    from seohead.storage.native_audit import AuditSizeError
    from seohead.storage.reanalysis import derived_scan, replace_reparsed_page
    from seohead.storage.reanalysis_pages import iterate_reparsed_pages

    version, revision, runtime = _producer_provenance(producer_build)
    with derived_scan(
        input_path,
        out,
        producer_version=version,
        producer_revision=revision,
        runtime_versions=runtime,
    ) as (scan, source):
        parent = dict(source.execute("SELECT * FROM scan WHERE singleton=1").fetchone())
        settings = json.loads(parent["config_json"])
        captured_settings = settings
        list_mode = is_list_scan(source)
        count = source.execute("SELECT COUNT(*) FROM pages").fetchone()[0]
        from seohead.storage.audit_v2 import AuditV2Reader, audit_v2_path

        if audit_v2_path(input_path).exists():
            with AuditV2Reader(input_path) as captured:
                captured_run = dict(captured.header.get("run") or {})
        else:
            old_audit = source.execute(
                "SELECT document_json FROM audit WHERE singleton=1"
            ).fetchone()
            captured_run = json.loads(old_audit[0])["run"] if old_audit is not None else {}
        start_gate = None
        selected_start_html = None
        for replay in iterate_reparsed_pages(source, settings) if step == "all" else ():
            if list_mode:
                from dataclasses import replace

                replay = replace(
                    replay,
                    links={key: () for key in replay.links},
                    forms={key: () for key in replay.forms},
                )
            replace_reparsed_page(scan, replay)
            if replay.start_page_gate is not None:
                start_gate = dict(replay.start_page_gate)
                selected_start_html = replay.selected_html

        link_positions = backfill_positions(scan, max_documents=max_documents, resume=step != "all")
        if step == "links.position" and not list_mode:
            from seohead.storage import ScanError
            from seohead.storage.bodies import read_document
            from seohead.storage.reanalysis_pages import _active_static_document

            start_page = source.execute(
                "SELECT p.*,u.url FROM pages p JOIN urls u USING(url_id) WHERE u.url=?",
                (parent["start_url"],),
            ).fetchone()
            if start_page is not None:
                document_id = _active_static_document(source, start_page)
                if document_id is not None:
                    try:
                        html = read_document(source, document_id, max_decoded_bytes=8 * 1024 * 1024)
                        start_gate = {
                            "html": html,
                            "outlinks": start_page["outlinks"],
                            "external_outlinks": start_page["external_outlinks"],
                        }
                        selected_start_html = html
                    except (ScanError, UnicodeError):
                        pass
        # Classification is an offline analyzer policy; the copied capture configuration
        # still records whether positions were measured during the original crawl.
        settings = {**settings, "link_position": {**settings["link_position"], "classify": True}}

        reason = ""
        audit = None
        if start_gate is None and not list_mode:
            reason = "reanalysis unavailable: start-page raw evidence is not_in_corpus"
        else:
            # Reanalysis has already persisted its reparsed rows. Re-open them
            # as a SQLite view for audit assembly instead of duplicating every
            # PageRecord in memory before the analyzer projects its input.
            result = _rebuild_page_result(scan, page_view=True)
            summary_row = None
            if scan.con.execute(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='external_check_summary'"
            ).fetchone():
                summary_row = scan.con.execute(
                    "SELECT payload_json FROM external_check_summary WHERE singleton=1"
                ).fetchone()
            # Reanalysis never repeats external requests.  It projects the
            # immutable capture summary into the new audit header so readers
            # retain the distinction between a disabled phase and a completed
            # one without treating retained evidence as a new measurement.
            result.external_summary = (
                json.loads(summary_row[0]) if summary_row is not None else None
            )
            provenance = json.loads(
                scan.con.execute(
                    "SELECT payload_json FROM context_items WHERE kind='reanalysis_provenance' AND item_key='run'"
                ).fetchone()[0]
            )
            result.finish_reason = provenance["capture_run"]["finish_reason"]
            result.stopped_reason = result.finish_reason if result.partial else ""
            if result.partial and result.stopped_reason in {"", "running", "finished"}:
                result.stopped_reason = "source coverage or offline parsed observations are partial"
            result.start_page_evidence = start_gate
            if selected_start_html is not None:
                result._rendered_start_html = selected_start_html
            roots = scan.sitemap_roots()
            sitemap_seed = {
                "sitemap_url": roots[0]["url"] if roots else None,
                "sitemap_urls": [root["url"] for root in roots],
                "declared": [],
            }
            discovery = {
                "mode": "list" if list_mode else "spider",
                "directive_policy": settings["robots"]["policy"],
                "robots_blocked": len(result.robots_blocked),
                "sitemap_url": sitemap_seed["sitemap_url"],
                "sitemap_urls": sitemap_seed["sitemap_urls"],
                "sitemap_seeded": len(result.seed_urls),
            }
            with prepare_sitemap_reconciliation(
                scan.con, start_url=parent["start_url"]
            ) as reconciliation:
                _, audit = _audit_crawl_result(
                    result,
                    settings=settings,
                    url=None if list_mode else parent["start_url"],
                    sitemap_seed=sitemap_seed,
                    discovery=discovery,
                    out_dir=None,
                    pages_resume_path=None,
                    stored_scan=scan,
                    stored_sitemap=reconciliation,
                    offline=True,
                    captured_render_summary=captured_run.get("render_escalation"),
                    streaming=True,
                )
            header, collections = audit
            from seohead.crawl.settings import manifest

            header["run"]["crawl_config"] = manifest(captured_settings)
            header["run"]["reanalysis"] = {
                "parent_scan_uuid": parent["scan_uuid"],
                "parent_writer_version": parent["writer_version"],
                "parent_writer_revision": parent["writer_revision"],
                "analyzer_revision": revision,
                "network": "disabled",
                "step": step,
                "link_positions": link_positions,
                "analysis_policy": {"link_position.classify": True},
            }
            try:
                scan.save_audit_v2(header, collections)
            except AuditSizeError as exc:
                reason = str(exc)
        if reason:
            scan.note_audit_unavailable(reason)
        header = dict(scan.con.execute("SELECT * FROM scan WHERE singleton=1").fetchone())
        response = {
            "ok": True,
            "scan": str(Path(out).absolute()),
            "scan_uuid": header["scan_uuid"],
            "parent_scan_uuid": parent["scan_uuid"],
            "source_kind": "reanalysis",
            "analyzer_revision": revision,
            "pages": count,
            "link_positions": link_positions,
            "audit_available": not bool(reason),
            "audit_reason": reason,
            "crawl_partial": bool(header["crawl_partial"]),
            "corpus_partial": bool(header["corpus_partial"]),
            "capabilities": json.loads(header["capabilities_json"]),
        }
    return response
