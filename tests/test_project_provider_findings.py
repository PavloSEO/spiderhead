"""Project-bound SF findings from retained provider history only."""

from seohead.sf.core import provider_findings


def _coverage(complete=True):
    return {
        "period": {"from": "2026-09-01", "to": "2026-09-28"},
        "rows": [],
        "row_count": 0,
        "truncated": False,
        "omitted_rows_lower_bound": 0,
        "coverage_complete": complete,
        "days_complete": 28 if complete else 4,
        "days_empty": 0,
        "days_expected": 28,
    }


def test_project_gsc_history_is_joined_to_audit_pages_without_a_provider_call(monkeypatch):
    from seohead.projects import bindings

    monkeypatch.setattr(
        bindings,
        "list_bindings",
        lambda project: {
            "bindings": [
                {"service": "gsc", "resource": "sc-domain:example.test", "status": "available"}
            ]
        },
    )
    monkeypatch.setattr(provider_findings, "_origin", lambda project: "https://example.test")
    history = _coverage()
    history["rows"] = [
        {
            "page": "https://example.test/seen",
            "query": "pump",
            "date": "2026-09-01",
            "clicks": 1,
            "impressions": 5,
            "ctr": 0.2,
            "position": 3.0,
        },
        {
            "page": "https://example.test/outside",
            "query": "pump",
            "date": "2026-09-01",
            "clicks": 2,
            "impressions": 10,
            "ctr": 0.2,
            "position": 2.0,
        },
    ]
    monkeypatch.setattr(provider_findings, "_history", lambda *_: history)

    result = provider_findings.project_provider_findings(
        [{"url": "https://example.test/seen", "indexability": "Indexable"}],
        project_dir="project.test",
        crawl_partial=False,
    )

    gsc = result["sources"]["gsc"]
    assert gsc["state"] == "complete"
    orphans = gsc["findings"]["GSC_ORPHAN"]
    assert len(orphans) == 1
    orphan = orphans[0]
    assert orphan["url"] == "https://example.test/outside"
    assert orphan["safe_url"] == "https://example.test/outside"
    assert orphan["details"] == {"impressions": 10.0}
    assert orphan["url_id"].startswith("url:v1:")
    assert "project.test" not in str(result)


def test_incomplete_analytics_history_withholds_absence_findings(monkeypatch):
    from seohead.projects import bindings

    monkeypatch.setattr(
        bindings,
        "list_bindings",
        lambda project: {
            "bindings": [{"service": "ga4", "resource": "property-1", "status": "stale"}]
        },
    )
    monkeypatch.setattr(provider_findings, "_origin", lambda project: "https://example.test")
    history = _coverage(complete=False)
    history["rows"] = [{"landing_page": "/seen", "sessions": 1}]
    monkeypatch.setattr(provider_findings, "_history", lambda *_: history)

    result = provider_findings.project_provider_findings(
        [
            {"url": "https://example.test/seen", "indexability": "Indexable"},
            {"url": "https://example.test/quiet", "indexability": "Indexable"},
        ],
        project_dir="project.test",
        crawl_partial=False,
    )

    findings = result["sources"]["ga4"]["findings"]
    assert findings["GA_ORPHAN_URL"]["state"] == "withheld"
    assert findings["NO_GA_DATA"]["state"] == "withheld"
    assert findings["NON_INDEXABLE_WITH_GA_DATA"]["urls"] == []


def test_missing_bindings_are_named_unavailable_instead_of_clean(monkeypatch):
    from seohead.projects import bindings

    monkeypatch.setattr(bindings, "list_bindings", lambda project: {"bindings": []})
    result = provider_findings.project_provider_findings(
        [], project_dir="project.test", crawl_partial=False
    )

    assert result["state"] == "unavailable"
    assert all(value["state"] == "not_configured" for value in result["sources"].values())


def test_cloudflare_history_preserves_native_period_stale_state_and_omitted_facts(monkeypatch):
    from datetime import datetime, timezone

    from seohead.data_sources import cloudflare_history

    monkeypatch.setattr(
        cloudflare_history,
        "list_snapshots",
        lambda **kwargs: {
            "rows": [
                {
                    "state": "complete",
                    "sampled": False,
                    "observed_at": "2026-09-01T00:00:00Z",
                    "period": {"from": "2026-08-25", "to": "2026-08-31"},
                    "period_days": 7,
                    "truncated_days": [],
                    "url_facts": [{"path": "/a"}],
                    "url_facts_omitted": 12,
                    "url_facts_source_truncated": True,
                    "url_facts_page_omitted": 0,
                }
            ]
        },
    )
    result = provider_findings._cloudflare_history(
        "project.test",
        {"resource": "zone-1"},
        as_of=datetime(2026, 10, 12, tzinfo=timezone.utc),
    )
    assert result["state"] == "partial"
    assert result["period"] == {"from": "2026-08-25", "to": "2026-08-31"}
    assert result["period_days"] == 7
    assert result["omitted_url_facts"] == 12
    assert "older than 28 days" in result["reason"]


def test_provider_findings_enter_registered_issue_stream_and_missing_sources_skip():
    from seohead.sf.core.registry import CHECKS
    from seohead.sf.core.sf_issue_map import entries

    class Context:
        def __init__(self):
            self.issues = []
            self.skipped = {}

        def settled(self, check_id):
            return check_id in self.skipped or any(i["check"] == check_id for i in self.issues)

        def skip(self, check_id, reason):
            self.skipped[check_id] = reason

        def enabled(self, _check_id):
            return True

        def add(self, check_id, **kwargs):
            self.issues.append({"check": check_id, **kwargs})

    context = Context()
    assert set(provider_findings.GSC_ANALYTICS_CHECKS) <= set(CHECKS)
    assert set(provider_findings.GSC_INSPECTION_CHECKS) <= set(CHECKS)
    assert {
        check
        for mapping in provider_findings.ANALYTICS_CHECKS.values()
        for check in mapping.values()
    } <= set(CHECKS)
    registered_ids = set(provider_findings.GSC_ANALYTICS_CHECKS)
    registered_ids.update(provider_findings.GSC_INSPECTION_CHECKS)
    registered_ids.update(
        check
        for mapping in provider_findings.ANALYTICS_CHECKS.values()
        for check in mapping.values()
    )
    mapped_ids = {check for _category, entry in entries() for check in entry.refs}
    assert registered_ids <= mapped_ids
    provider_findings.emit_provider_issues(
        context,
        {
            "state": "partial",
            "sources": {
                "gsc": {
                    "state": "complete",
                    "coverage": {"period": {"from": "2026-01-01", "to": "2026-01-28"}},
                    "findings": {
                        "GSC_ORPHAN": [
                            {"url": "https://example.test/gone", "details": {"impressions": 4}}
                        ],
                        "NO_SEARCH_DATA": [],
                        "NON_INDEXABLE_WITH_SEARCH_DATA": [],
                    },
                    "inspection": {
                        "inspected": 1,
                        "failed": 0,
                        "deferred": 0,
                        "findings": {
                            check: [] for check in provider_findings.GSC_INSPECTION_CHECKS
                        },
                    },
                },
                "ga4": {"state": "not_configured", "reason": "no GA4 binding"},
                "metrika": {"state": "not_configured", "reason": "no Metrika binding"},
            },
        },
    )
    assert context.issues[0]["check"] == "GSC_ORPHAN"
    assert context.issues[0]["source"] == "retained GSC Search Analytics"
    assert context.skipped["NO_GA_DATA"] == "no GA4 binding"
    assert context.skipped["NO_METRIKA_DATA"] == "no Metrika binding"
    unavailable = Context()
    provider_findings.emit_provider_issues(
        unavailable,
        {"state": "unavailable", "reason": "no project", "sources": {}},
    )
    assert registered_ids <= unavailable.skipped.keys()


def test_analytics_aggregation_keeps_all_null_metrics_unreadable():
    rows, visits_column, bounce_column, unreadable = provider_findings._analytics_rows(
        "ga4",
        {"rows": [{"landing_page": "/page", "sessions": None}]},
        "https://example.test",
    )
    assert visits_column == "sessions"
    assert bounce_column is None
    assert rows == [{"url": "https://example.test/page", "sessions": None}]
    assert unreadable == 1
    assert (
        provider_findings._public_url("https://user:secret@example.test/page?token=private#x")
        == "https://example.test/page"
    )
