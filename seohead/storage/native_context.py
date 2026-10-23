"""Closed versioned native context; observations remain data, never instructions."""

import json
import math
from typing import Any

from . import ScanError, _insert

# Settings that live in the native_config_extension context item instead of the
# frozen config header, so an older reader never meets a key it does not know.
# seohead.storage.native_scan._CONFIG_EXTENSION_DEFAULTS mirrors these names
# with their legacy defaults; a test pins the two sets together.
CONFIG_EXTENSION_PATHS = (
    "scope.normalize_tracking_parameters",
    "http.auth",
    "http.auth_acknowledged",
    "http.zstd",
    "rendering.actions",
    "rendering.presets",
    "rendering.artifacts.screenshot_mode",
    "analysis.redirect_hop_hsts_probe",
)


def _validate_extraction_rule_evidence(con: Any, item: dict[str, Any], payload: Any) -> None:
    """Validate closed rule results and bind their envelope to one document."""
    from seohead.checks.extraction_rules import validate_result

    try:
        validate_result(payload)
    except ValueError as exc:
        raise ScanError("native extraction rule evidence is invalid") from exc
    if (
        len(payload["rules"]) > 100
        or item["completeness"] != payload["state"]
        or item["reason"] != payload["reason"]
    ):
        raise ScanError("native extraction rule evidence envelope disagrees")
    prefix, marker, suffix = item["item_key"].partition(":document:")
    if not prefix.startswith("page:") or marker != ":document:" or ":representation:" not in suffix:
        raise ScanError("native extraction rule evidence key is invalid")
    document_text, representation = suffix.split(":representation:", 1)
    try:
        page_url_id, document_id = int(prefix[5:]), int(document_text)
    except ValueError as exc:
        raise ScanError("native extraction rule evidence key is invalid") from exc
    if (
        page_url_id < 1
        or document_id < 1
        or representation != payload["representation"]
        or not con.execute(
            "SELECT 1 FROM documents WHERE document_id=? AND url_id=? AND representation=?",
            (document_id, page_url_id, representation),
        ).fetchone()
    ):
        raise ScanError("native extraction rule evidence binds the wrong document")


def validate_context(
    con: Any, item: dict[str, Any], *, sitemap_roots: set[int] | None = None
) -> None:
    if set(item) != {
        "kind",
        "item_key",
        "payload_version",
        "payload_json",
        "completeness",
        "reason",
    }:
        raise ScanError("native context has unknown or missing fields")
    if any(type(item[key]) is not str for key in item):
        raise ScanError("native context metadata must be strings")
    if item["completeness"] not in {"complete", "partial", "unavailable"}:
        raise ScanError("native context completeness is invalid")
    if item["payload_version"] != "scan_context.v1":
        raise ScanError("native scan context payload version is invalid")
    try:
        payload = json.loads(item["payload_json"])
    except (TypeError, ValueError) as exc:
        raise ScanError("native scan context payload is invalid JSON") from exc
    from . import sitemaps

    if item["kind"] == "native_config_extension":
        # The vocabulary seohead.storage.native_scan._CONFIG_EXTENSION_DEFAULTS
        # also declares (with values); a test pins the two sets together.
        fields = set(CONFIG_EXTENSION_PATHS)
        fingerprint = payload.get("effective_fingerprint") if isinstance(payload, dict) else None
        if (
            item["item_key"] != "run"
            or item["completeness"] != "complete"
            or item["reason"]
            or not isinstance(payload, dict)
            or set(payload) != {"schema_version", "settings", "effective_fingerprint"}
            or payload["schema_version"] != "native_config_extension.v1"
            or not isinstance(payload["settings"], dict)
            or set(payload["settings"]) != fields
            or not isinstance(fingerprint, str)
            or len(fingerprint) != 64
            or any(char not in "0123456789abcdef" for char in fingerprint)
        ):
            raise ScanError("native config extension context is invalid")
        return

    if item["kind"] == "analysis_asset_thresholds":
        from seohead.crawl.settings import DEFAULTS

        if (
            item["item_key"] != "run"
            or item["completeness"] != "complete"
            or item["reason"]
            or not isinstance(payload, dict)
            or set(payload) != {"schema_version", "thresholds"}
            or payload["schema_version"] != "analysis_asset_thresholds.v1"
            or not isinstance(payload["thresholds"], dict)
            or set(payload["thresholds"]) != set(DEFAULTS["analysis"]["asset_thresholds"])
            or any(
                type(value) is not int or not 0 < value < 2**63
                for value in payload["thresholds"].values()
            )
        ):
            raise ScanError("native analysis asset thresholds context is invalid")
        return

    if item["kind"] == "links.position":
        from .link_positions import validate_checkpoint

        validate_checkpoint(con, item, payload)
        return

    if item["kind"] == "list_mode":
        if (
            item["item_key"] != "run"
            or payload != {"mode": "list"}
            or item["completeness"] != "complete"
            or item["reason"]
        ):
            raise ScanError("invalid explicit-list mode context")
        return
    if item["kind"] == "list_input":
        if (
            item["item_key"] != "run"
            or item["completeness"] != "complete"
            or item["reason"]
            or not isinstance(payload, dict)
            or set(payload) != {"sha256", "raw_count", "counts"}
            or type(payload["raw_count"]) is not int
            or payload["raw_count"] < 0
            or type(payload["sha256"]) is not str
            or len(payload["sha256"]) != 64
            or any(char not in "0123456789abcdef" for char in payload["sha256"])
            or not isinstance(payload["counts"], dict)
            or set(payload["counts"]) != {"blank", "url_too_long", "accepted", "duplicate"}
            or any(type(value) is not int or value < 0 for value in payload["counts"].values())
            or sum(payload["counts"].values()) != payload["raw_count"]
        ):
            raise ScanError("invalid explicit-list input identity")
        return
    if item["kind"] == "list_robots":
        if (
            not isinstance(payload, dict)
            or set(payload) != {"origin", "parsed"}
            or not isinstance(payload["origin"], list)
            or len(payload["origin"]) != 2
            or any(type(part) is not str or not part for part in payload["origin"])
            or item["item_key"] != json.dumps(payload["origin"], separators=(",", ":"))
        ):
            raise ScanError("invalid explicit-list robots origin")
        validate_context(
            con,
            {
                **item,
                "kind": "robots_summary",
                "item_key": "run",
                "payload_json": json.dumps(
                    {
                        "policy": "respect",
                        "token": "*",
                        "fetch_state": "fetched",
                        "final_response_id": None,
                        "note": "",
                        "parsed": payload["parsed"],
                    }
                ),
            },
        )
        return

    if item["kind"] in sitemaps.KINDS:
        sitemaps.validate_context(con, item, payload, sitemap_roots)
        return
    if item["kind"] == "render_phase_summary":
        from .render_summary import validate

        validate(payload)
        if (
            not item["item_key"].startswith("phase:")
            or item["completeness"] != "complete"
            or item["reason"]
        ):
            raise ScanError("invalid render phase summary envelope")
        return
    if item["kind"] in {"external_check", "external_checks_summary"}:
        from .external_checks import validate_check, validate_summary

        (validate_check if item["kind"] == "external_check" else validate_summary)(item, payload)
        return
    if item["kind"] in {"render_elapsed", "list_elapsed"}:
        if (
            not isinstance(payload, dict)
            or set(payload) != {"schema_version", "seconds", "active"}
            or payload["schema_version"] != item["kind"] + ".v1"
            or type(payload["seconds"]) not in {int, float}
            or not math.isfinite(payload["seconds"])
            or payload["seconds"] < 0
            or type(payload["active"]) is not bool
            or item["item_key"] != "run"
            or item["completeness"] != "complete"
            or item["reason"]
        ):
            raise ScanError(
                f"native {'render' if item['kind'] == 'render_elapsed' else 'list'} elapsed context is invalid"
            )
        return
    if item["kind"] == "resource_inventory":
        from .resources import validate_inventory_context

        validate_inventory_context(con, item)
        return
    if item["kind"] == "resource_graph_coverage":
        from .resource_graph import validate_coverage_context

        validate_coverage_context(con, item, payload)
        return
    if item["kind"] in {
        "rendered_route_ledger",
        "rendered_route_coverage",
        "rendered_route_run_coverage",
    }:
        from .rendered_routes import validate_context as validate_rendered_routes

        validate_rendered_routes(con, item, payload)
        return
    if item["kind"] == "content_evidence":
        from .content_evidence import validate_context as validate_content_evidence

        validate_content_evidence(con, item, payload)
        return
    if item["kind"] == "markup_evidence":
        from .markup_evidence import validate_context as validate_markup_evidence

        validate_markup_evidence(con, item, payload)
        return
    if item["kind"] == "browser_artifacts":
        from .browser_artifacts import validate_context as validate_browser_artifacts

        validate_browser_artifacts(con, item, payload)
        return
    if item["kind"] in {"structured_evidence", "language_evidence"}:
        from .structured_evidence import validate_context as validate_structured_evidence

        validate_structured_evidence(con, item, payload)
        return
    if item["kind"] == "extraction_rule_evidence":
        _validate_extraction_rule_evidence(con, item, payload)
        return
    if item["kind"] == "head_links":
        from .head_links import validate_context as validate_head_links

        validate_head_links(con, item, payload)
        return
    if item["kind"] == "target_probe":
        from .target_probes import validate_context as validate_target_probe

        validate_target_probe(item, payload)
        return
    if item["kind"] == "redirect_hop_probe":
        from ..crawl.security_headers import validate_redirect_hop_probe

        validate_redirect_hop_probe(item, payload)
        return
    if item["kind"] == "resource_commit":
        if (
            not isinstance(payload, dict)
            or set(payload) != {"digest", "requests_used"}
            or type(payload["digest"]) is not str
            or len(payload["digest"]) != 64
            or any(ch not in "0123456789abcdef" for ch in payload["digest"])
            or type(payload["requests_used"]) is not int
            or payload["requests_used"] < 0
            or item["completeness"] != "complete"
            or item["reason"]
            or not item["item_key"].startswith("resource:")
            or not item["item_key"][9:].isascii()
            or not item["item_key"][9:].isdigit()
        ):
            raise ScanError("native resource commit context is invalid")
        return
    if item["kind"] == "reanalysis_provenance":
        if (
            item["item_key"] != "run"
            or item["completeness"] != "complete"
            or item["reason"] != "offline reanalysis"
            or not isinstance(payload, dict)
            or set(payload)
            != {
                "parent_scan_uuid",
                "capture_scan_uuid",
                "source_evidence_revision",
                "derived_evidence_revision",
                "source_audit_sha256",
                "source_writer_version",
                "source_writer_revision",
                "source_runtime_versions_json",
                "source_config_sha256",
                "capture_writer_version",
                "capture_writer_revision",
                "capture_runtime_versions_json",
                "capture_config_sha256",
                "capture_run",
            }
            or not isinstance(payload["parent_scan_uuid"], str)
            or not isinstance(payload["capture_scan_uuid"], str)
            or type(payload["source_evidence_revision"]) is not int
            or payload["source_evidence_revision"] < 0
            or type(payload["derived_evidence_revision"]) is not int
            or payload["derived_evidence_revision"] != payload["source_evidence_revision"] + 1
            or (
                payload["source_audit_sha256"] is not None
                and (
                    not isinstance(payload["source_audit_sha256"], str)
                    or len(payload["source_audit_sha256"]) != 64
                    or any(ch not in "0123456789abcdef" for ch in payload["source_audit_sha256"])
                )
            )
            or not isinstance(payload["source_writer_version"], str)
            or not isinstance(payload["source_writer_revision"], str)
            or len(payload["source_writer_revision"]) != 40
            or any(ch not in "0123456789abcdef" for ch in payload["source_writer_revision"])
            or not isinstance(payload["source_runtime_versions_json"], str)
            or not isinstance(payload["source_config_sha256"], str)
            or len(payload["source_config_sha256"]) != 64
            or any(ch not in "0123456789abcdef" for ch in payload["source_config_sha256"])
            or not isinstance(payload["capture_writer_version"], str)
            or not isinstance(payload["capture_writer_revision"], str)
            or len(payload["capture_writer_revision"]) != 40
            or any(ch not in "0123456789abcdef" for ch in payload["capture_writer_revision"])
            or not isinstance(payload["capture_runtime_versions_json"], str)
            or not isinstance(payload["capture_config_sha256"], str)
            or len(payload["capture_config_sha256"]) != 64
            or any(ch not in "0123456789abcdef" for ch in payload["capture_config_sha256"])
        ):
            raise ScanError("native reanalysis provenance is invalid")
        capture = payload["capture_run"]
        if (
            not isinstance(capture, dict)
            or set(capture)
            != {"lifecycle", "finish_reason", "crawl_partial", "created_at", "finished_at"}
            or capture["lifecycle"] not in {"running", "interrupted", "finished", "failed"}
            or not isinstance(capture["finish_reason"], str)
            or type(capture["crawl_partial"]) is not int
            or capture["crawl_partial"] not in (0, 1)
        ):
            raise ScanError("reanalysis capture state is invalid")
        from .corpus_validation import _timestamp

        _timestamp(capture["created_at"], "capture created_at")
        _timestamp(capture["finished_at"], "capture finished_at", nullable=True)
        return
    if item["kind"] == "credential_context":
        verifier = payload.get("verifier") if isinstance(payload, dict) else object()
        if (
            item["item_key"] != "run"
            or item["completeness"] != "complete"
            or item["reason"]
            or not isinstance(payload, dict)
            or set(payload) != {"verifier", "implicit_state"}
            or (verifier is not None and not isinstance(verifier, str))
            or (
                isinstance(verifier, str)
                and (
                    len(verifier) != 64
                    or any(character not in "0123456789abcdef" for character in verifier)
                )
            )
            or type(payload["implicit_state"]) is not bool
        ):
            raise ScanError("native credential context is invalid")
        return
    if item["kind"] == "anonymous_session":
        if (
            item["item_key"] != "run"
            or item["completeness"] != "complete"
            or item["reason"]
            or payload != {"server_set_cookie": True}
        ):
            raise ScanError("native anonymous session context is invalid")
        return
    if item["kind"] == "anonymous_session_resume":
        if (
            item["item_key"] != "run"
            or item["completeness"] != "complete"
            or item["reason"]
            or payload != {"fresh_session": True}
        ):
            raise ScanError("native anonymous session resume context is invalid")
        return
    if item["kind"] == "native_commit":
        if (
            not item["item_key"].isascii()
            or not item["item_key"].isdigit()
            or str(int(item["item_key"])) != item["item_key"]
            or not isinstance(payload, dict)
            or set(payload) != {"digest"}
            or not isinstance(payload["digest"], str)
            or len(payload["digest"]) != 64
            or any(char not in "0123456789abcdef" for char in payload["digest"])
            or item["completeness"] != "complete"
            or item["reason"] != "atomic page commit"
        ):
            raise ScanError("native scan commit idempotency context is invalid")
        if not con.execute(
            "SELECT 1 FROM frontier WHERE queue_ordinal=? AND state='done'",
            (int(item["item_key"]),),
        ).fetchone():
            raise ScanError("native scan commit context does not name a completed lease")
        return
    if item["kind"] == "seed_url":
        if (
            not isinstance(payload, dict)
            or set(payload) != {"url_id", "depth", "source"}
            or type(payload["url_id"]) is not int
            or payload["depth"] != 0
            or payload["source"] != "sitemap"
            or item["item_key"] != f"url:{payload['url_id']}"
        ):
            raise ScanError("native seed context is invalid")
        if not con.execute(
            "SELECT 1 FROM frontier WHERE url_id=? AND depth=0 AND state!='excluded'",
            (payload["url_id"],),
        ).fetchone():
            raise ScanError("native seed context must reference an accepted seed")
        return
    if item["kind"] == "robots_summary":
        if (
            item["item_key"] != "run"
            or not isinstance(payload, dict)
            or set(payload)
            != {"policy", "token", "fetch_state", "final_response_id", "note", "parsed"}
        ):
            raise ScanError("native robots summary shape is invalid")
        if (
            payload["policy"] not in {"respect", "report_only", "ignore"}
            or type(payload["token"]) is not str
            or type(payload["note"]) is not str
            or payload["fetch_state"] not in {"fetched", "unavailable", "not_fetched"}
            or payload["final_response_id"] is not None
        ):
            raise ScanError("native robots summary provenance is invalid")
        parsed = payload["parsed"]
        if (
            not isinstance(parsed, dict)
            or set(parsed) != {"groups", "sitemaps"}
            or not isinstance(parsed["groups"], list)
            or not isinstance(parsed["sitemaps"], list)
            or any(type(value) is not str for value in parsed["sitemaps"])
        ):
            raise ScanError("native parsed robots summary is invalid")
        if payload["fetch_state"] != "fetched" and parsed != {"groups": [], "sitemaps": []}:
            raise ScanError("unavailable robots summary cannot invent parsed rules")
        for group in parsed["groups"]:
            legacy_keys = {
                "user_agents",
                "allow",
                "disallow",
                "crawl_delay",
            }
            current_keys = legacy_keys | {"request_rate_delay"}
            if not isinstance(group, dict) or set(group) not in (legacy_keys, current_keys):
                raise ScanError("native robots group shape is invalid")
            if any(
                not isinstance(group[key], list)
                or any(type(value) is not str for value in group[key])
                for key in ("user_agents", "allow", "disallow")
            ):
                raise ScanError("native robots rules must be string lists")
            delay = group["crawl_delay"]
            if delay is not None and (
                type(delay) not in (int, float) or not math.isfinite(delay) or delay < 0
            ):
                raise ScanError("native robots delay must be finite and nonnegative")
            rate_delay = group.get("request_rate_delay")
            if rate_delay is not None and (
                type(rate_delay) not in (int, float)
                or not math.isfinite(rate_delay)
                or rate_delay <= 0
            ):
                raise ScanError("native robots request-rate delay must be finite and positive")
        return
    if item["kind"] != "robots_blocked_url":
        raise ScanError(
            "native context kind is unsupported; expected native_commit, "
            "robots_blocked_url, robots_summary or seed_url"
        )
    if (
        not isinstance(payload, dict)
        or set(payload) != {"url_id", "token", "policy"}
        or type(payload["url_id"]) is not int
        or payload["url_id"] <= 0
        or item["item_key"] != f"url:{payload['url_id']}"
        or type(payload["token"]) is not str
        or payload["policy"] not in {"respect", "report_only"}
    ):
        raise ScanError("native scan robots_blocked_url context is invalid")
    url = con.execute("SELECT url FROM urls WHERE url_id=?", (payload["url_id"],)).fetchone()
    if url is None:
        raise ScanError("native scan robots context references an unknown URL")
    if (
        payload["policy"] == "respect"
        and not con.execute(
            "SELECT 1 FROM decisions WHERE url=? AND reason='blocked_by_robots'", (url[0],)
        ).fetchone()
    ):
        raise ScanError("native robots exclusion lacks its blocked_by_robots decision")


def put_context(con: Any, item: dict[str, Any], *, sitemap_roots: set[int] | None = None) -> None:
    validate_context(con, item, sitemap_roots=sitemap_roots)
    existing = con.execute(
        "SELECT * FROM context_items WHERE kind=? AND item_key=?", (item["kind"], item["item_key"])
    ).fetchone()
    if existing is not None:
        if item["kind"] in {"render_elapsed", "list_elapsed"}:
            previous = json.loads(existing["payload_json"])
            current = json.loads(item["payload_json"])
            if current["seconds"] < previous["seconds"]:
                raise ScanError(
                    f"{'render' if item['kind'] == 'render_elapsed' else 'list'} elapsed seconds cannot decrease"
                )
            con.execute(
                "UPDATE context_items SET payload_json=? WHERE kind=? AND item_key='run'",
                (item["payload_json"], item["kind"]),
            )
            return
        if dict(existing) != item:
            raise ScanError("native context retry disagrees with committed observation")
        return
    _insert(con, "context_items", item)
