"""The provider capability/workflow matrix stays bound to the code it describes."""

from __future__ import annotations

import inspect
import json

from seohead.cli import COMMANDS
from seohead.core.provider_matrix import (
    PROVIDER_REF_KINDS,
    SUPPORT_STATES,
    UNSUPPORTED_WORK,
    WORKFLOWS,
    ProviderRef,
    render,
)
from seohead.data_sources.providers import provider_registry


def _row(workflow: str):
    matches = [row for row in WORKFLOWS if row.workflow == workflow]
    assert len(matches) == 1, f"expected exactly one {workflow} row"
    return matches[0]


def test_every_workflow_status_is_a_declared_state():
    assert {row.status for row in WORKFLOWS} <= set(SUPPORT_STATES)


def test_every_provider_ref_kind_is_declared():
    for row in WORKFLOWS:
        for ref in row.providers:
            assert isinstance(ref, ProviderRef)
            assert ref.kind in PROVIDER_REF_KINDS, f"{row.workflow}: unknown kind {ref.kind}"


def test_every_surface_command_is_a_real_cli_command():
    referenced = {command for row in WORKFLOWS for command in row.surface}
    missing = referenced - set(COMMANDS)
    assert not missing, f"matrix references commands that do not exist: {sorted(missing)}"


def test_every_registry_provider_ref_is_registered():
    registered = set(provider_registry()["providers"])
    for row in WORKFLOWS:
        missing = {
            ref.name for ref in row.providers if ref.kind in {"registry", "declared"}
        } - registered
        assert not missing, f"{row.workflow} references unregistered providers: {missing}"


def test_every_registered_provider_is_named_in_some_workflow():
    # Workflow routes are typed (registry/dedicated/declared), so coverage is by
    # name across kinds — the registry inventory table itself renders straight
    # from provider_registry() and cannot drift ahead of it.
    covered = {ref.name for row in WORKFLOWS for ref in row.providers}
    missing = set(provider_registry()["providers"]) - covered
    assert not missing, f"registered providers absent from the matrix: {sorted(missing)}"


def test_dedicated_refs_resolve_to_real_integrations():
    # A "dedicated" ref must name an integration module the handlers actually call —
    # membership in the provider registry is neither required nor sufficient.
    import importlib

    dedicated = {ref.name for row in WORKFLOWS for ref in row.providers if ref.kind == "dedicated"}
    assert dedicated == {
        "dataforseo",
        "yandex_cloud",
        "arsenkin",
        "indexnow",
        "topvisor",
        "miratext",
        "backlink_indexes",
    }
    for name in dedicated:
        module = importlib.import_module(f"seohead.data_sources.{name}")
        assert getattr(module, "SOURCE", name) == name


def test_supported_rows_never_reference_a_route_provider_collect_refuses():
    """A supported workflow row must not type its provider as a registry route that
    provider-collect deliberately rejects — that would describe a callable path the
    code refuses to ship."""
    refused = {"arsenkin", "yandex_cloud", "indexnow"}
    for row in WORKFLOWS:
        if row.status in {"supported", "partial"}:
            bad = {
                ref.name for ref in row.providers if ref.kind == "registry" and ref.name in refused
            }
            assert not bad, f"{row.workflow} labels a refused dispatch as a registry route: {bad}"


def test_only_unsupported_rows_use_declared_refs():
    for row in WORKFLOWS:
        for ref in row.providers:
            if ref.kind == "declared":
                assert row.status == "unsupported", (
                    f"{row.workflow}: a declared-only ref cannot describe a shipped route"
                )


def test_local_refs_resolve_to_in_process_modules():
    import importlib

    for row in WORKFLOWS:
        for ref in row.providers:
            if ref.kind == "local":
                module = importlib.import_module(ref.name)
                assert module.__name__ == ref.name


def test_dataforseo_registry_entry_covers_only_backlinks():
    # google-keywords/google-serp must not be attributed to the registry id that
    # declares only backlinks_summary and is off by default.
    entry = provider_registry()["providers"]["dataforseo_backlinks"]
    assert entry["operations"] == [
        "backlinks_summary",
        "backlinks",
        "referring_domains",
        "backlinks_history",
        "domain_pages_summary",
    ]
    assert entry["default_enabled"] is False
    for workflow in ("google-demand", "serp-collection"):
        names = {ref.name for ref in _row(workflow).providers}
        assert "dataforseo_backlinks" not in names
        assert any(
            ref.kind == "dedicated" and ref.name == "dataforseo" for ref in _row(workflow).providers
        )


def test_keywords_cluster_is_local_text_clustering_not_serp(monkeypatch):
    """keywords-cluster calls the in-process clusterer with a supplied keyword list.

    There is no provider transport on this path: the handler signature takes only
    params, and the clusterer takes a single params dict — no fetcher or URL.
    """
    from seohead.checks import clusterer
    from seohead.mcp import handlers

    params = inspect.signature(clusterer.run_clusterer).parameters
    assert list(params) == ["params"]

    captured = {}

    def fake_run_clusterer(p):
        captured.update(p)
        return {"ok": True, "clusters": [{"label": "x", "keywords": p["keywords"]}]}

    monkeypatch.setattr(clusterer, "run_clusterer", fake_run_clusterer)
    result = handlers.keywords_cluster(keywords=["synthetic alpha", "synthetic beta"])
    assert captured["keywords"] == ["synthetic alpha", "synthetic beta"]
    assert result["ok"] is True and "clusters" in result

    row = _row("keyword-text-clustering")
    assert row.surface == ("keywords-cluster",)
    assert {ref.kind for ref in row.providers} == {"local"}
    assert "arsenkin" not in {ref.name for ref in row.providers}
    # SERP clustering by results overlap stays a distinct, unsupported declared
    # operation; no shipped command reaches it.
    serp_row = _row("serp-clustering")
    assert serp_row.status == "unsupported" and serp_row.surface == ()
    assert [(ref.kind, ref.name) for ref in serp_row.providers] == [("declared", "arsenkin")]


def test_google_keywords_and_serp_use_the_dedicated_integration(monkeypatch):
    """google-keywords/google-serp dispatch to seohead.data_sources.dataforseo
    directly; they never pass through the provider registry."""
    from seohead.data_sources import dataforseo
    from seohead.mcp import handlers

    calls = []

    monkeypatch.setattr(
        dataforseo,
        "search_volume",
        lambda keywords, **kw: calls.append(("search_volume", keywords)) or {"ok": True},
    )
    monkeypatch.setattr(
        dataforseo,
        "serp",
        lambda query, **kw: calls.append(("serp", query)) or {"ok": True},
    )

    assert handlers.google_keywords(keywords=["buy shoes"])["ok"] is True
    assert handlers.google_serp(query="buy shoes")["ok"] is True
    assert calls == [("search_volume", ["buy shoes"]), ("serp", "buy shoes")]


def test_registry_backlinks_route_is_separate_from_demand_paths(monkeypatch):
    """provider-collect dataforseo_backlinks dispatches only backlinks_summary."""
    from seohead.data_sources import dataforseo, providers

    calls = []

    def fake_backlink_call(operation, **request):
        assert operation == "backlinks_summary"
        calls.append(request)
        return {"ok": True, "state": "complete", "summary": {"target": request["target"]}}

    monkeypatch.setattr(dataforseo, "backlink_call", fake_backlink_call)
    envelope = providers.provider_collect(
        "dataforseo_backlinks", "backlinks_summary", {"target": "example.com"}
    )
    assert calls == [{"target": "example.com"}]
    evidence = envelope["evidence"]
    assert evidence["provider"] == "dataforseo_backlinks"
    assert evidence["operation"] == "backlinks_summary"

    row = _row("link-evidence")
    assert [(ref.kind, ref.name) for ref in row.providers] == [
        ("registry", "dataforseo_backlinks"),
        ("registry", "moz"),
        ("registry", "yandex_webmaster"),
        ("registry", "bing_webmaster"),
        ("dedicated", "backlink_indexes"),
    ]


def test_crux_is_field_data_and_pagespeed_stays_lab_only():
    """CrUX returns real-user field metrics; the PSI parser returns Lighthouse lab
    samples marked lab_only — the matrix must keep the two measurement types apart."""
    from seohead.data_sources import crux, pagespeed

    crux_body = {
        "record": {
            "key": {"origin": "https://example.com"},
            "metrics": {
                "largest_contentful_paint": {"percentiles": {"p75": 2500}},
            },
        }
    }
    field = crux.query(
        origin="https://example.com",
        api_key="synthetic-key",
        fetcher=lambda _payload, _key: json.dumps(crux_body),
    )
    assert field["ok"] and field["metrics"]["largest_contentful_paint"]["p75"] == 2500

    psi_body = {
        "lighthouseResult": {
            "finalUrl": "https://example.com/",
            "fetchTime": "2026-01-01T00:00:00Z",
            "lighthouseVersion": "12.0.0",
            "categories": {"performance": {"score": 0.9, "title": "Performance"}},
            "audits": {},
        }
    }
    lab = pagespeed.sample(
        ["https://example.com/"],
        api_key="synthetic-key",
        fetcher=lambda _url, _key: json.dumps(psi_body),
    )
    sample = lab["samples"][0]
    assert sample["lab_only"] is True
    assert "not CrUX field data" in sample["note"]

    assert [ref.name for ref in _row("field-vitals").providers] == ["crux"]
    lab_row = _row("lab-vitals")
    assert [ref.name for ref in lab_row.providers] == ["pagespeed"]
    assert "lab_only" in lab_row.limitations
    assert "real-user" in lab_row.limitations


def test_yandex_demand_and_serp_use_the_dedicated_module(monkeypatch):
    """keywords-expand/keywords-seasonality/serp-fetch call the dedicated
    yandex_cloud integration directly; provider-collect refuses the contract."""
    import pytest

    from seohead.data_sources import providers, yandex_cloud
    from seohead.mcp import handlers

    calls = []

    class FakeWordstat:
        def expand(self, phrase, *, limit, regions):
            calls.append(("expand", phrase))
            return {"synthetic seed": 42}, {
                "totalCount": 1,
                "results": 1,
                "associations": 0,
                "origin": {"synthetic seed": "refinement"},
            }

        def dynamics(self, phrase, from_date, to_date, *, period, regions):
            calls.append(("dynamics", phrase))
            return {"dynamics": []}

    class FakeWebSearch:
        def search_batch(self, queries, *, region, groups):
            calls.append(("search_batch", list(queries)))
            return {q: {"docs": [], "status": "done"} for q in queries}

    monkeypatch.setattr(yandex_cloud, "Wordstat", FakeWordstat)
    monkeypatch.setattr(yandex_cloud, "WebSearch", FakeWebSearch)

    assert handlers.keywords_expand(phrase="synthetic seed")["ok"] is True
    assert (
        handlers.keywords_seasonality(
            phrase="synthetic seed",
            from_date="2026-01-01T00:00:00Z",
            to_date="2026-02-01T00:00:00Z",
        )["ok"]
        is True
    )
    assert handlers.serp_fetch(query="synthetic query")["ok"] is True
    assert calls == [
        ("expand", "synthetic seed"),
        ("dynamics", "synthetic seed"),
        ("search_batch", ["synthetic query"]),
    ]

    # These are dedicated routes, not registry dispatch.
    assert [(ref.kind, ref.name) for ref in _row("yandex-demand").providers] == [
        ("dedicated", "yandex_cloud")
    ]
    serp_row = _row("serp-collection")
    assert ("dedicated", "yandex_cloud") in [(ref.kind, ref.name) for ref in serp_row.providers]
    for operation in providers.provider_registry()["providers"]["yandex_cloud"]["operations"]:
        with pytest.raises(ValueError):
            providers.provider_collect("yandex_cloud", operation, {})


def test_keywords_exact_uses_the_dedicated_arsenkin_client(monkeypatch):
    """keywords-exact bills through ArsenkinClient.set_task directly; generic
    provider-collect refuses both declared arsenkin operations."""
    import pytest

    from seohead.data_sources import arsenkin, providers
    from seohead.mcp import handlers

    calls = []

    class FakeArsenkinClient:
        def set_task(self, tools_name, data):
            calls.append((tools_name, data))
            return {"task_id": 1, "cost": 5, "raw": {}}

    monkeypatch.setattr(arsenkin, "ArsenkinClient", FakeArsenkinClient)

    result = handlers.keywords_exact(keywords=["synthetic kw"], wait=False)
    assert result["ok"] is True and result["task_id"] == 1
    assert calls == [
        (
            "wordstat",
            {
                "type": 1,
                "regions": [225],
                "ws": ["base", "overal"],
                "queries": ["synthetic kw"],
            },
        )
    ]

    assert [(ref.kind, ref.name) for ref in _row("yandex-exact-frequency").providers] == [
        ("dedicated", "arsenkin")
    ]
    # The declared serp_clustering operation stays a declaration, not a shipped
    # route: the matrix marks it "declared" and provider-collect refuses it.
    serp_row = _row("serp-clustering")
    assert serp_row.status == "unsupported" and serp_row.surface == ()
    assert [(ref.kind, ref.name) for ref in serp_row.providers] == [("declared", "arsenkin")]
    for operation in providers.provider_registry()["providers"]["arsenkin"]["operations"]:
        with pytest.raises(ValueError):
            providers.provider_collect("arsenkin", operation, {})


def test_indexnow_submit_uses_the_dedicated_write_path(monkeypatch):
    """indexnow-submit calls seohead.data_sources.indexnow.submit directly —
    a confirmed write that provider-collect refuses by contract."""
    import pytest

    from seohead.data_sources import indexnow, providers
    from seohead.mcp import handlers

    calls = []

    def fake_submit(urls, *, host, key=None, key_location=None, fetcher=None):
        calls.append({"urls": list(urls), "host": host, "key_location": key_location})
        return {"ok": True, "submitted": len(urls), "not_adopted_by": list(indexnow.NOT_ADOPTED_BY)}

    monkeypatch.setattr(indexnow, "submit", fake_submit)

    result = handlers.indexnow_submit(
        urls=["https://example.com/a"], host="example.com", key_location="https://example.com/k.txt"
    )
    assert result["ok"] is True
    assert calls == [
        {
            "urls": ["https://example.com/a"],
            "host": "example.com",
            "key_location": "https://example.com/k.txt",
        }
    ]

    row = _row("url-submission")
    assert row.status == "supported"
    assert [(ref.kind, ref.name) for ref in row.providers] == [("dedicated", "indexnow")]
    # The registry keeps the write off by default and the collect dispatch
    # refuses it: a submission is never a collection.
    entry = providers.provider_registry()["providers"]["indexnow"]
    assert entry["default_enabled"] is False and entry["access"] == "confirmed_write"
    with pytest.raises(ValueError):
        providers.provider_collect("indexnow", "submit", {})


def test_unsupported_work_is_named_not_silent():
    assert UNSUPPORTED_WORK, "unsupported work must stay explicit"
    rendered = render()
    for name, _ in UNSUPPORTED_WORK:
        assert name in rendered


def test_no_provider_is_labelled_live_verified():
    # "verified" is deliberately not a SUPPORT_STATE: a declared registry entry is not
    # evidence of live access, so no status cell may claim it.
    assert "verified" not in SUPPORT_STATES and "live-verified" not in SUPPORT_STATES
    assert "nothing in this matrix is labelled live-verified" in render()


def test_render_is_deterministic_and_covers_the_registry():
    first, second = render(), render()
    assert first == second
    for name in provider_registry()["providers"]:
        assert f"`{name}`" in first
