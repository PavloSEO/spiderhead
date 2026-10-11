"""Golden audit parity for legacy graphs and the SQL-backed native scan path."""

from __future__ import annotations

import copy

import pytest

from seohead.checks.markup import A11Y_CODES, AMP_CODES
from seohead.crawl import security_headers
from seohead.crawl.settings import fingerprint, load
from seohead.crawl.sqlite_adapter import crawl_to_scan
from seohead.mcp.handlers import _audit_crawl_result
from seohead.mcp.scan_handlers import _rebuild_page_result
from seohead.sf.core.markup_rules import SCHEMA_CODES
from seohead.storage.native_scan import NativeScan
from tests.evidence_contract_assertions import assert_saved_contract, semantic_audit
from tests.test_scan_artifact_office import frozen_office_clock as frozen_office_clock
from tests.test_scan_crawl_parity import _fetcher, _legacy, _Response, _runtime_versions


def _fixture():
    """One graph deliberately carrying every outcome-sensitive edge distinction."""
    return {
        "https://example.test/robots.txt": _Response(
            200, "User-agent: SEOHEAD-Tools\nAllow: /\n", {"content-type": "text/plain"}
        ),
        "https://example.test/": _Response(
            200,
            "<html><head><title>Start</title></head><body>"
            "<nav><a href='/a/'>click here</a><a href='/a?x=1#part'>read more</a></nav>"
            "<main><a href='/b'>descriptive anchor</a><a href='/c' rel='nofollow'>details</a>"
            "<a href='/nofollow-only' rel='nofollow'>click here</a>"
            "<a href='/deep-1'>next</a></main></body></html>",
        ),
        "https://example.test/a/": _Response(
            200, "<html><body><a href='/target'>read more</a></body></html>"
        ),
        "https://example.test/a?x=1": _Response(
            200, "<html><body><a href='/target#part'>click here</a></body></html>"
        ),
        "https://example.test/b": _Response(
            200, "<html><body><a href='/target'>guide</a></body></html>"
        ),
        "https://example.test/c": _Response(
            200,
            "<html><head><meta name='robots' content='noindex'></head>"
            "<body><a href='/nonindex-only'>click here</a></body></html>",
        ),
        "https://example.test/nofollow-only": _Response(200, "<html><body>Nofollow</body></html>"),
        "https://example.test/nonindex-only": _Response(
            200, "<html><body>Noindex source</body></html>"
        ),
        "https://example.test/deep-1": _Response(
            200, "<html><body><a href='/deep-2'>next</a></body></html>"
        ),
        "https://example.test/deep-2": _Response(
            200, "<html><body><a href='/deep-3'>next</a></body></html>"
        ),
        "https://example.test/deep-3": _Response(
            200, "<html><body><a href='/deep-4'>next</a></body></html>"
        ),
        "https://example.test/deep-4": _Response(
            200, "<html><body><a href='/deep-5'>next</a></body></html>"
        ),
        "https://example.test/deep-5": _Response(
            200, "<html><body><a href='/deep-target'>click here</a></body></html>"
        ),
        "https://example.test/deep-target": _Response(200, "<html><body>Deep target</body></html>"),
        "https://example.test/target": _Response(200, "<html><body>Target</body></html>"),
    }


def _settings(mode):
    return load(
        overrides={
            "speed.min_delay_seconds": 0,
            "limits.max_urls": 5 if mode == "partial" else 20,
            "limits.max_depth": 8,
            "link_attributes.capture": True,
            "link_position.classify": True,
            "discovery.follow_nofollow": True,
        }
    )


def _audit(result, settings, *, stored_scan=None, stored_sitemap=None, sitemap_seed=None):
    return _audit_crawl_result(
        result,
        settings=settings,
        url="https://example.test/",
        sitemap_seed=sitemap_seed or {"sitemap_url": None, "sitemap_urls": [], "declared": []},
        discovery={"mode": "spider", "directive_policy": "respect", "robots_blocked": 0},
        stored_scan=stored_scan,
        stored_sitemap=stored_sitemap,
    )


def _drop_retained_only_checks(audit: dict) -> dict:
    """Remove checks whose evidence exists only inside the retained artifact.

    Bookmark, DOM and content duplicate checks read retained destination
    bodies: the SQL path carries them, the legacy graph keeps no bodies at
    all, so these checks can only diverge by construction. Dropping
    it -- issues, skip declarations, coverage rows and their dependent totals --
    keeps this test measuring the shared-evidence parity it exists for.
    """
    # The security-header checks (#1013, and the repeated-header check) read each page's stored
    # response headers, which the legacy graph never keeps: the same retained-only shape as
    # BROKEN_BOOKMARK.
    # Markup findings require document-bound contexts (#1304); the legacy
    # graph deliberately retains neither bodies nor those source contexts.
    retained_only = {
        "BROKEN_BOOKMARK",
        "DOM_TOO_DEEP",
        "DOM_TOO_MANY_NODES",
        "NEAR_DUPLICATE",
        "DUPLICATE_BY_HASH",
        *security_headers.CHECK_IDS,
        *A11Y_CODES,
        *AMP_CODES,
        "AMP_MISSING_HTML_ATTRIBUTE",
        *SCHEMA_CODES,
    }
    document = copy.deepcopy(audit)
    summary = document.get("summary") if isinstance(document.get("summary"), dict) else {}
    summary.pop("fragment_links", None)

    removed_ids = {
        issue["id"] for issue in document.get("issues", []) if issue.get("check") in retained_only
    }
    removed_checks = {
        issue["check"]
        for issue in document.get("issues", [])
        if issue.get("check") in retained_only
    }
    issues = [
        issue for issue in document.get("issues", []) if issue.get("check") not in retained_only
    ]
    id_map = {}
    for n, issue in enumerate(issues, start=1):
        new_id = f"ISSUE-{n:06d}"
        id_map[issue.get("id")] = new_id
        issue["id"] = new_id
    document["issues"] = issues
    document["groups"] = [
        group for group in document.get("groups", []) if group.get("check") not in retained_only
    ]
    for page in document.get("pages", []):
        page["issue_ids"] = [
            id_map.get(issue_id, issue_id)
            for issue_id in page.get("issue_ids", [])
            if issue_id not in removed_ids
        ]
        page["issues"] = [c for c in page.get("issues", []) if c not in retained_only]
        for key in ("dom_depth", "dom_nodes"):
            page.get("metrics", {}).pop(key, None)

    run = document.get("run") if isinstance(document.get("run"), dict) else {}
    for key in ("checks_skipped", "checks_disabled"):
        if isinstance(run.get(key), list):
            run[key] = [row for row in run[key] if row.get("id") not in retained_only]

    totals = summary.get("totals") if isinstance(summary.get("totals"), dict) else None
    if totals is not None:
        totals["issues_total"] = len(issues)
        totals["groups_total"] = len(document["groups"])
    if isinstance(summary.get("by_check"), dict):
        summary["by_check"] = {
            check: count
            for check, count in summary["by_check"].items()
            if check not in retained_only
        }
    if isinstance(summary.get("by_severity"), dict):
        from collections import Counter

        by_severity = Counter(issue.get("severity") for issue in issues)
        summary["by_severity"] = {
            level: by_severity.get(level, 0) for level in ("critical", "warning", "notice")
        }
    if isinstance(summary.get("implausible_checks"), list):
        summary["implausible_checks"] = [
            row for row in summary["implausible_checks"] if row.get("check") not in retained_only
        ]
    coverage = summary.get("check_coverage")
    if isinstance(coverage, dict):
        for key in ("checks_disabled_ids", "checks_silent_ids"):
            if isinstance(coverage.get(key), list):
                coverage[key] = [i for i in coverage[key] if i not in retained_only]
        if removed_checks and coverage.get("checks_fired"):
            coverage["checks_fired"] -= len(removed_checks)
        skipped_id_set = {s.get("id") for s in run.get("checks_skipped") or []}
        disabled_id_set = set(coverage.get("checks_disabled_ids") or [])
        fired_id_set = {issue.get("check") for issue in issues}
        coverage["checks_skipped"] = len(skipped_id_set - disabled_id_set - fired_id_set)
        coverage["checks_silent"] = len(coverage.get("checks_silent_ids") or [])
        coverage["checks_disabled"] = len(coverage.get("checks_disabled_ids") or [])
        if coverage.get("checks_total"):
            coverage["checks_total"] -= len(retained_only)
        available = (
            coverage["checks_total"]
            - coverage.get("checks_skipped", 0)
            - coverage.get("checks_disabled", 0)
        )
        coverage["coverage"] = (
            round(available / coverage["checks_total"], 3) if coverage["checks_total"] else None
        )
    # Score and its basis strings are pure derivations of the issue severity
    # counts and coverage numbers recomputed above; once those agree the
    # derived values cannot disagree, so they add nothing to this comparison.
    for key in ("health_score", "health_score_basis", "health_score_reason"):
        summary.pop(key, None)
    return document


_RETAINED_RUN_FIELDS = {"scan_uuid", "config_fingerprint", "corpus_partial", "source_kind"}


# Skip reasons for these checks are worded in native terms on a stored native scan and in SF
# export terms on the legacy path (epic #1314, native-checks). Their id, finding and task must
# still match exactly; only the reason text is allowed to differ.
_PATH_WORDED_SKIPS = frozenset(
    {
        "IMG_MISSING_ALT",
        "IMG_MISSING_DIMENSIONS",
        "IMG_OVER_KB",
        "MIXED_CONTENT",
        "TITLE_MULTIPLE",
        "SITEMAP_URL_3XX",
        "SITEMAP_URL_4XX_5XX",
        "SITEMAP_URL_NON_INDEXABLE",
    }
    | {
        "HREFLANG_BROKEN_TARGET",
        "HREFLANG_ERROR",
        "HREFLANG_INCONSISTENT_CONFIRMATION",
        "HREFLANG_INVALID_CODE",
        "HREFLANG_MISSING_RETURN_LINK",
        "HREFLANG_MISSING_SELF_REFERENCE",
        "HREFLANG_MISSING_XDEFAULT",
        "HREFLANG_MULTIPLE_ENTRIES",
        "HREFLANG_NOINDEX_TARGET",
        "HREFLANG_NOT_CANONICAL",
        "HREFLANG_UNLINKED_TARGET",
    }
)


def _label_budget_stop(audit):
    """Name a URL-budget stop the same way on both paths (see the parity tests below)."""
    run = audit["run"]
    if run.get("crawl_finish_reason") in {"stopped_by_budget", "url_limit"}:
        run["crawl_finish_reason"] = "budget_stop"
        run["crawl_stopped_reason"] = "budget_stop"


def _outcome(audit, *, retained_run=None):
    """The whole audit contract except the report clock and measured response durations.

    ``generated_at`` is a report wall clock.  Every ``response_time`` in this
    contract is independently measured by each crawl, including a copied value
    in a finding's details, so a numeric duration cannot decide graph parity.
    Missing or invalid duration values, plus all thresholds, counts, URLs, and
    findings, stay strict.
    """
    # These fields describe the retained artifact, which the legacy graph has
    # never created. Validate the raw values before excluding them from verdict
    # parity; in particular corpus completeness is not crawl completeness.
    if retained_run is not None:
        assert set(retained_run) == _RETAINED_RUN_FIELDS
        for key, expected in retained_run.items():
            assert type(audit["run"].get(key)) is type(expected), key
            assert audit["run"][key] == expected, key
    measured_duration_fields = {"response_time"}

    def normalize(value):
        if isinstance(value, dict):
            path_worded = value.get("id") in _PATH_WORDED_SKIPS and "reason" in value
            return {
                key: "path-specific wording"
                if path_worded and key == "reason"
                else 0.0
                if key in measured_duration_fields and type(child) in (int, float)
                else normalize(child)
                for key, child in value.items()
            }
        if isinstance(value, list):
            return [normalize(item) for item in value]
        return value

    result = _drop_retained_only_checks(semantic_audit(audit))
    # Skip records are a set of (check, reason): the two paths reach the same skips in a
    # different order, and the order of skip calls is not part of the audit contract.
    if "checks_skipped" in result["run"]:
        result["run"]["checks_skipped"] = sorted(
            result["run"]["checks_skipped"], key=lambda item: item["id"]
        )
    result["run"].pop("generated_at")
    for key in retained_run or {}:
        result["run"].pop(key)
    return normalize(result)


def _different_paths(left, right, path="$"):
    if type(left) is not type(right):
        return [path]
    if isinstance(left, dict):
        paths = []
        for key in sorted(set(left) | set(right), key=str):
            child = f"{path}.{key}"
            if key not in left or key not in right:
                paths.append(child)
            else:
                paths.extend(_different_paths(left[key], right[key], child))
        return paths
    if isinstance(left, list):
        paths = []
        for index in range(max(len(left), len(right))):
            child = f"{path}[{index}]"
            if index >= len(left) or index >= len(right):
                paths.append(child)
            else:
                paths.extend(_different_paths(left[index], right[index], child))
        return paths
    return [] if left == right else [path]


def test_outcome_normalizes_measured_durations_but_keeps_issues_strict():
    """Parity ignores separate clocks, never a different audit conclusion."""
    left = {
        "run": {"generated_at": "2026-09-06T00:00:00Z"},
        "pages": [{"metrics": {"response_time": 0.001, "word_count": 10}}],
        "issues": [
            {
                "check": "SLOW_RESPONSE_TIME",
                "details": {"response_time": 0.001, "max_s": 1.5},
            }
        ],
    }
    right = copy.deepcopy(left)
    right["run"]["generated_at"] = "2026-09-06T00:00:01Z"
    right["pages"][0]["metrics"]["response_time"] = 0.999
    right["issues"][0]["details"]["response_time"] = 0.999

    assert _outcome(left) == _outcome(right)

    missing_duration = copy.deepcopy(right)
    missing_duration["pages"][0]["metrics"]["response_time"] = None
    assert _outcome(left) != _outcome(missing_duration)

    strict_number = copy.deepcopy(right)
    strict_number["pages"][0]["metrics"]["word_count"] = 11
    assert _outcome(left) != _outcome(strict_number)

    right["issues"].append({"check": "UNEXPECTED_PARITY_BREAK", "details": {}})
    with pytest.raises(AssertionError):
        assert _outcome(left) == _outcome(right)


@pytest.mark.parametrize("field", sorted(_RETAINED_RUN_FIELDS))
def test_outcome_validates_retained_provenance_before_excluding_it(field):
    provenance = {
        "scan_uuid": "00000000-0000-4000-8000-000000000001",
        "config_fingerprint": "a" * 16,
        "corpus_partial": False,
        "source_kind": "native",
    }
    legacy = {"run": {"generated_at": "2026-10-06T00:00:00Z", "crawl_partial": False}}
    native = copy.deepcopy(legacy)
    native["run"].update(provenance)
    assert _outcome(native, retained_run=provenance) == _outcome(legacy)
    changed = copy.deepcopy(native)
    changed["run"][field] = True if field == "corpus_partial" else "wrong"
    with pytest.raises(AssertionError, match=field):
        _outcome(changed, retained_run=provenance)
    changed = copy.deepcopy(native)
    del changed["run"][field]
    with pytest.raises(AssertionError, match=field):
        _outcome(changed, retained_run=provenance)
    native["run"]["crawl_partial"] = True
    assert _outcome(native, retained_run=provenance) != _outcome(legacy)


@pytest.mark.parametrize("mode", ("complete", "partial", "empty", "unclassified"))
def test_sql_graph_audit_matches_legacy_without_building_all_inlinks(
    tmp_path, monkeypatch, frozen_office_clock, mode
):
    """F must preserve findings/order while the SQL path has no legacy edge materialization."""
    import seohead.crawl.evidence as evidence
    from seohead.crawl.sql_sitemap import prepare_sitemap_reconciliation
    from seohead.sf.tasks import build_tasks

    settings = _settings(mode)
    responses = _fixture()
    if mode == "empty":
        responses["https://example.test/"] = _Response(200, "<html><body>Empty</body></html>")
    if mode == "unclassified":
        settings["link_position"]["classify"] = False
    legacy = _legacy(
        settings,
        _fetcher(responses),
        tmp_path / "legacy-decisions.jsonl",
        content_area_config=None,
    )
    path = tmp_path / "scan.sqlite"
    run = crawl_to_scan(
        "https://example.test/",
        scan_out=str(path),
        settings=settings,
        producer_version="3.0.0",
        producer_revision="a" * 40,
        runtime_versions=_runtime_versions(),
        fetcher=_fetcher(responses),
        sleeper=lambda _seconds: None,
    )
    with NativeScan.open(path) as scan:
        sitemap_url = "https://example.test/sitemap.xml"
        sitemap_id = scan.declare_sitemap(sitemap_url, "explicit", 0)
        declared = [page.url for page in legacy.pages]
        scan.write_sitemap_members(sitemap_id, list(enumerate(declared)))
        scan.finish_sitemap(sitemap_id, True, "")
        sitemap_seed = {
            "sitemap_url": sitemap_url,
            "sitemap_urls": [sitemap_url],
            "declared": declared,
        }
        # The protocol fetch is outside the graph parity fixture; both paths receive
        # the same complete selected-root reconciliation below.
        monkeypatch.setattr(
            "seohead.sf.core.sitemap_coverage.run_sitemap",
            lambda *_args, **_kwargs: {"sitemaps": []},
        )
        _legacy_response, legacy_audit = _audit(legacy, settings, sitemap_seed=sitemap_seed)
        # The SQL route receives pages and the transient start gate, never legacy links/forms.
        sql_result = _rebuild_page_result(scan)
        sql_result.start_page_evidence = dict(run.start_page_gate or {})
        sql_result.resumed = False
        sql_result.finish_reason = run.finish_reason

        with prepare_sitemap_reconciliation(scan.con, start_url="https://example.test/") as sitemap:
            monkeypatch.setattr(
                evidence,
                "_inlinks_frame",
                lambda *_args, **_kwargs: pytest.fail("SQL path materialized all_inlinks"),
            )
            _sql_response, sql_audit = _audit_crawl_result(
                sql_result,
                settings=settings,
                url="https://example.test/",
                sitemap_seed=sitemap_seed,
                discovery={"mode": "spider", "directive_policy": "respect", "robots_blocked": 0},
                stored_scan=scan,
                stored_sitemap=sitemap,
            )
            assert_saved_contract(sql_audit, scan.con)
            retained_run = dict(
                scan.con.execute(
                    "SELECT scan_uuid,config_fingerprint,corpus_partial,source_kind "
                    "FROM scan WHERE singleton=1"
                ).fetchone()
            )
            retained_run["corpus_partial"] = bool(retained_run["corpus_partial"])
            assert retained_run["config_fingerprint"] == fingerprint(settings)
            assert retained_run["source_kind"] == "native"
            assert retained_run["corpus_partial"] is run.corpus_partial

    sql_outcome = _outcome(sql_audit, retained_run=retained_run)
    legacy_outcome = _outcome(legacy_audit)
    # A URL-budget stop is labelled stopped_by_budget by the native scan (finished, see
    # stop_by_budget) and url_limit by the legacy spider. Both are the same stop, so the
    # run label is compared as a budget stop; the findings and tasks still compare exactly.
    _label_budget_stop(sql_outcome)
    _label_budget_stop(legacy_outcome)
    paths = _different_paths(sql_outcome, legacy_outcome)
    (tmp_path / "scan-analysis-parity-diff.txt").write_text("\n".join(paths) + "\n")
    assert sql_outcome == legacy_outcome, "\n".join(paths)
    assert build_tasks(sql_outcome, None) == build_tasks(legacy_outcome, None)
    if mode == "complete":
        assert {issue["check"] for issue in sql_audit["issues"]} >= {
            "ONLY_NOFOLLOW_INLINKS",
            "ONLY_NONINDEXABLE_SOURCE_INLINKS",
            "DEEP_DISCOVERY_PATH",
        }
    # Report writers receive the same semantic audit input. The saved-reference
    # contracts were independently validated above, and are deliberately local
    # to the scan rather than a renderer-parity dimension.
    legacy_render_audit = _outcome(legacy_audit)
    sql_render_audit = _outcome(sql_audit, retained_run=retained_run)
    for audit in (legacy_render_audit, sql_render_audit):
        audit["run"]["generated_at"] = legacy_audit["run"]["generated_at"]
        _label_budget_stop(audit)
    from seohead.reports import build_report

    for fmt in ("json", "md", "csv", "xlsx", "docx"):
        left, right = tmp_path / f"legacy.{fmt}", tmp_path / f"sql.{fmt}"
        legacy_rendered = build_report(legacy_render_audit, fmt, str(left))
        sql_rendered = build_report(sql_render_audit, fmt, str(right))
        assert legacy_rendered["ok"], legacy_rendered
        assert sql_rendered["ok"], sql_rendered
        assert left.read_bytes() == right.read_bytes()
        if fmt == "csv":
            for suffix in (".pages.csv", ".scope.csv"):
                assert (
                    left.with_suffix(suffix).read_bytes() == right.with_suffix(suffix).read_bytes()
                )
