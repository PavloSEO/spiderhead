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
    assert gsc["findings"]["GSC_ORPHAN"] == [
        {"url": "https://example.test/outside", "details": {"impressions": 10.0}}
    ]
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
