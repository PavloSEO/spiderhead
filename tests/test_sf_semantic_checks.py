"""Offline retained-body semantic integration; the adapter is always a fixture."""

from __future__ import annotations

import csv

import pytest

from seohead.checks.semantic_similarity import LocalEmbeddingAdapter, ProviderEmbeddingAdapter
from seohead.sf.config import load_config
from seohead.sf.core import heuristics, semantic_checks
from seohead.sf.core.audit import run_audit
from seohead.sf.core.context import AuditContext
from seohead.sf.core.loader import load_exports
from seohead.sf.core.semantic_checks import SEMANTIC_CHECKS, check_semantic_content


@pytest.fixture
def ctx(tmp_path):
    with (tmp_path / "internal_all.csv").open("w", newline="") as stream:
        writer = csv.writer(stream)
        writer.writerow(
            ["Address", "Content Type", "Status Code", "Indexability", "Title 1", "H1-1"]
        )
        writer.writerows(
            [
                [
                    "https://example.test/a",
                    "text/html",
                    200,
                    "Indexable",
                    "unrelated title",
                    "water heading",
                ],
                [
                    "https://example.test/b",
                    "text/html",
                    200,
                    "Indexable",
                    "water title",
                    "unrelated heading",
                ],
            ]
        )
    context = AuditContext(load_exports(str(tmp_path)), load_config(None))
    context.config["semantic_similarity"] = {"cache_path": str(tmp_path / "cache.sqlite")}
    yield context
    context.close()


def _adapter(calls):
    def embed(texts):
        calls.append(list(texts))
        return [[0.0, 1.0] if "unrelated" in text else [1.0, 0.0] for text in texts]

    return LocalEmbeddingAdapter("fixture", "1", "/fixture", embed)


def _bodies(monkeypatch):
    monkeypatch.setattr(
        heuristics, "_stored_html_reader", lambda ctx: lambda url: "<main>water body</main>"
    )


def test_missing_adapter_never_reads_body_or_creates_cache(ctx, monkeypatch):
    monkeypatch.setattr(heuristics, "_stored_html_reader", lambda ctx: pytest.fail("body read"))
    result = check_semantic_content(ctx)
    assert result["coverage"]["state"] == "unavailable"
    assert {skip.id for skip in ctx.skipped} == set(SEMANTIC_CHECKS)
    assert "no semantic embedding adapter" in result["reason"]


def test_selected_local_adapter_groups_bodies_and_measures_title_h1(ctx, monkeypatch):
    _bodies(monkeypatch)
    calls = []
    ctx.semantic_adapter = _adapter(calls)
    result = check_semantic_content(ctx)
    assert result["coverage"]["state"] == "complete"
    assert {issue.check for issue in ctx.issues} == set(SEMANTIC_CHECKS)
    group = next(issue for issue in ctx.issues if issue.check == SEMANTIC_CHECKS[0])
    assert group.severity == "notice"
    assert "not a duplicate" in group.details["conclusion"]
    assert "model_path" not in group.details["adapter"]
    assert next(
        issue for issue in ctx.issues if issue.check == SEMANTIC_CHECKS[1]
    ).target_url.endswith("/a")
    assert next(
        issue for issue in ctx.issues if issue.check == SEMANTIC_CHECKS[2]
    ).target_url.endswith("/b")
    assert len(calls) == 2
    check_semantic_content(ctx)
    assert len(calls) == 2


def test_missing_bodies_are_unavailable_even_with_adapter(ctx, monkeypatch):
    calls = []
    ctx.semantic_adapter = _adapter(calls)
    monkeypatch.setattr(
        heuristics,
        "_stored_html_reader",
        lambda ctx: lambda url: (_ for _ in ()).throw(ValueError("missing")),
    )
    result = check_semantic_content(ctx)
    assert result["coverage"]["omitted_documents"] == 2
    assert result["coverage"]["state"] == "unavailable"
    assert not calls and not ctx.issues


def test_document_limit_preserves_explicit_partial_coverage(ctx, monkeypatch):
    _bodies(monkeypatch)
    ctx.semantic_adapter = _adapter([])
    ctx.config["semantic_similarity"]["max_documents"] = 1
    result = check_semantic_content(ctx)
    assert result["coverage"]["state"] == "partial"
    assert result["coverage"]["omission_reasons"] == {"semantic document limit exceeded": 1}
    assert result["coverage"]["analyzed_documents"] == 1


def test_candidate_budget_skips_grouping_but_retains_heading_review(ctx, monkeypatch):
    _bodies(monkeypatch)
    ctx.semantic_adapter = _adapter([])
    ctx.config["semantic_similarity"]["max_candidate_comparisons"] = 0
    result = check_semantic_content(ctx)
    assert "budget exceeded" in result["grouping"]["reason"]
    assert {issue.check for issue in ctx.issues} == set(SEMANTIC_CHECKS[1:])
    assert {skip.id for skip in ctx.skipped} == {SEMANTIC_CHECKS[0]}
    assert result["coverage"]["state"] == "partial"


@pytest.mark.parametrize("vector", ([0.0, 0.0], [float("nan")], [], [1.0] * 8193))
def test_invalid_body_vectors_are_skipped(ctx, monkeypatch, vector):
    _bodies(monkeypatch)
    ctx.semantic_adapter = LocalEmbeddingAdapter(
        "fixture", "1", "/fixture", lambda texts: [vector for _ in texts]
    )
    result = check_semantic_content(ctx)
    assert "failed (ValueError)" in result["reason"]
    assert result["coverage"]["state"] == "unavailable"
    assert not ctx.issues


def test_provider_unauthorized_precedes_body_read(ctx, monkeypatch):
    monkeypatch.setattr(heuristics, "_stored_html_reader", lambda ctx: pytest.fail("body read"))
    ctx.semantic_adapter = ProviderEmbeddingAdapter(
        "fixture", "fixture", "1", lambda texts: pytest.fail("provider called")
    )
    result = check_semantic_content(ctx)
    assert "unauthorized" in result["reason"]
    assert not ctx.issues


def test_explicit_declared_vectors_require_no_model(ctx, monkeypatch):
    _bodies(monkeypatch)
    cfg = ctx.config["semantic_similarity"]
    cfg["adapter"] = _adapter([]).describe()
    cfg["embeddings"] = [
        {"url": page.url, "body": [1, 0], "title": [0, 1], "h1": [0, 1]} for page in ctx.pages
    ]
    result = check_semantic_content(ctx)
    assert result["coverage"]["state"] == "complete"
    assert len(ctx.issues) == 5


def test_body_only_vectors_keep_grouping_with_heading_checks_unavailable(ctx, monkeypatch):
    _bodies(monkeypatch)
    cfg = ctx.config["semantic_similarity"]
    cfg["adapter"] = _adapter([]).describe()
    cfg["embeddings"] = [{"url": page.url, "body": [1, 0]} for page in ctx.pages]
    result = check_semantic_content(ctx)
    assert result["groups"] and result["relevance"]["coverage"]["state"] == "partial"
    assert {skip.id for skip in ctx.skipped} == set(SEMANTIC_CHECKS[1:])


def test_adapter_failure_does_not_leak_exception_text(ctx, monkeypatch):
    _bodies(monkeypatch)
    ctx.semantic_adapter = LocalEmbeddingAdapter(
        "fixture",
        "1",
        "/fixture",
        lambda texts: (_ for _ in ()).throw(RuntimeError("sensitive-token")),
    )
    result = check_semantic_content(ctx)
    assert "sensitive-token" not in str(result)
    assert "RuntimeError" in result["reason"]


def test_corpus_byte_budget_stops_reading_but_counts_all_eligible_pages(ctx, monkeypatch):
    reads = []
    monkeypatch.setattr(semantic_checks, "MAX_CORPUS_INPUT_BYTES", 5)
    monkeypatch.setattr(
        heuristics,
        "_stored_html_reader",
        lambda ctx: lambda url: reads.append(url) or "<main>body</main>",
    )
    ctx.semantic_adapter = _adapter([])
    result = check_semantic_content(ctx)
    assert len(reads) == 1
    assert result["coverage"]["eligible_documents"] == 2
    assert result["coverage"]["omitted_documents"] == 2
    assert "input-byte limit" in result["reason"]


def test_partial_body_retains_exact_omission_reason_and_review_coverage(ctx, monkeypatch):
    def read(url):
        if url.endswith("/b"):
            raise ValueError("retained body checksum mismatch")
        return "<main>water body</main>"

    monkeypatch.setattr(heuristics, "_stored_html_reader", lambda ctx: read)
    ctx.semantic_adapter = _adapter([])
    result = check_semantic_content(ctx)
    assert result["coverage"]["omission_reasons"] == {"retained body checksum mismatch": 1}
    assert result["coverage"]["state"] == "partial"
    assert ctx.issues[0].details["coverage"]["state"] == "partial"


def test_one_missing_heading_vector_does_not_hide_other_relevance(ctx, monkeypatch):
    _bodies(monkeypatch)
    cfg = ctx.config["semantic_similarity"]
    cfg["adapter"] = _adapter([]).describe()
    cfg["embeddings"] = [{"url": page.url, "body": [1, 0], "title": [0, 1]} for page in ctx.pages]
    result = check_semantic_content(ctx)
    assert {issue.check for issue in ctx.issues} == set(SEMANTIC_CHECKS[:2])
    assert {skip.id for skip in ctx.skipped} == {SEMANTIC_CHECKS[2]}
    assert result["relevance"]["unavailable_vectors"] == {"title": 0, "h1": 2}


def test_inconsistent_dimensions_fail_without_fabricated_groups(ctx, monkeypatch):
    monkeypatch.setattr(
        heuristics, "_stored_html_reader", lambda ctx: lambda url: f"<main>{url}</main>"
    )
    cfg = ctx.config["semantic_similarity"]
    cfg["adapter"] = _adapter([]).describe()
    cfg["embeddings"] = [
        {"url": page.url, "body": vector}
        for page, vector in zip(ctx.pages, ([1], [1, 0]), strict=True)
    ]
    result = check_semantic_content(ctx)
    assert "inconsistent dimensions" in result["grouping"]["reason"]
    assert not ctx.issues


def test_sf_run_invokes_explicit_semantic_step_and_retains_summary(ctx, tmp_path, monkeypatch):
    _bodies(monkeypatch)
    cfg = ctx.config["semantic_similarity"]
    cfg["adapter"] = _adapter([]).describe()
    cfg["embeddings"] = [
        {"url": page.url, "body": [1, 0], "title": [0, 1], "h1": [0, 1]} for page in ctx.pages
    ]
    audit = run_audit(
        input_mode="parse-exports",
        exports_dir=str(tmp_path),
        config_overrides={"semantic_similarity": cfg},
        log=lambda message: None,
    )
    assert set(SEMANTIC_CHECKS).issubset({issue.check for issue in audit.issues})
    assert audit.summary["semantic_similarity"]["coverage"]["state"] == "complete"
    assert len(audit.summary["semantic_similarity"]["relevance"]["scores"]) == 4


def test_disabled_grouping_never_runs_pair_comparisons(ctx, monkeypatch):
    _bodies(monkeypatch)
    ctx.semantic_adapter = _adapter([])
    ctx.config["checks"][SEMANTIC_CHECKS[0]] = {"enabled": False}
    monkeypatch.setattr(
        semantic_checks,
        "analyze_semantic_documents",
        lambda *args, **kwargs: pytest.fail("pair comparisons ran"),
    )
    result = check_semantic_content(ctx)
    assert result["grouping"]["state"] == "disabled"
    assert {issue.check for issue in ctx.issues} == set(SEMANTIC_CHECKS[1:])


def test_disabled_heading_is_never_embedded(ctx, monkeypatch):
    _bodies(monkeypatch)
    calls = []
    ctx.semantic_adapter = _adapter(calls)
    ctx.config["checks"][SEMANTIC_CHECKS[2]] = {"enabled": False}
    result = check_semantic_content(ctx)
    assert {score["field"] for score in result["relevance"]["scores"]} == {"title"}
    assert all("heading" not in text for batch in calls for text in batch)


def test_grouping_only_does_not_call_heading_adapter(ctx, monkeypatch):
    _bodies(monkeypatch)
    ctx.semantic_adapter = _adapter([])
    for check in SEMANTIC_CHECKS[1:]:
        ctx.config["checks"][check] = {"enabled": False}
    monkeypatch.setattr(
        semantic_checks,
        "analyze_semantic_relevance",
        lambda *args, **kwargs: pytest.fail("heading adapter ran"),
    )
    result = check_semantic_content(ctx)
    assert result["relevance"]["state"] == "disabled"
    assert {issue.check for issue in ctx.issues} == {SEMANTIC_CHECKS[0]}


def test_partial_body_without_findings_is_skipped_in_check_coverage(ctx, monkeypatch):
    def read(url):
        if url.endswith("/b"):
            raise ValueError("retained body checksum mismatch")
        return "<main>water body</main>"

    monkeypatch.setattr(heuristics, "_stored_html_reader", lambda ctx: read)
    ctx.semantic_adapter = LocalEmbeddingAdapter(
        "fixture", "1", "/fixture", lambda texts: [[1, 0] for _ in texts]
    )
    result = check_semantic_content(ctx)
    assert result["coverage"]["state"] == "partial" and not ctx.issues
    assert {skip.id for skip in ctx.skipped} == set(SEMANTIC_CHECKS)
    assert all("prepared 1 of 2" in skip.reason for skip in ctx.skipped)


def test_partial_heading_vectors_without_findings_are_skipped(ctx, monkeypatch):
    _bodies(monkeypatch)
    cfg = ctx.config["semantic_similarity"]
    cfg["adapter"] = _adapter([]).describe()
    cfg["embeddings"] = [
        {"url": ctx.pages[0].url, "body": [1, 0], "title": [1, 0], "h1": [1, 0]},
        {"url": ctx.pages[1].url, "body": [1, 0], "h1": [1, 0]},
    ]
    check_semantic_content(ctx)
    skip = next(skip for skip in ctx.skipped if skip.id == SEMANTIC_CHECKS[1])
    assert "prepared 1 of 2" in skip.reason and "embedding unavailable (1)" in skip.reason


def test_adapter_declaration_failure_is_result_data_before_body_read(ctx, monkeypatch):
    class BrokenAdapter:
        def describe(self):
            raise RuntimeError("sensitive-token")

    ctx.semantic_adapter = BrokenAdapter()
    monkeypatch.setattr(heuristics, "_stored_html_reader", lambda ctx: pytest.fail("body read"))
    result = check_semantic_content(ctx)
    assert result["reason"] == "semantic adapter declaration failed (RuntimeError)"
    assert "sensitive-token" not in str(result)
    assert {skip.id for skip in ctx.skipped} == set(SEMANTIC_CHECKS)


def test_semantic_checks_fire_on_defect_and_stay_silent_on_clean_urls(tmp_path, monkeypatch):
    rows = [
        ["https://example.test/a", "text/html", 200, "Indexable", "cakes title", "water heading"],
        ["https://example.test/b", "text/html", 200, "Indexable", "water title", "cakes heading"],
        ["https://example.test/c", "text/html", 200, "Indexable", "cakes title", "cakes heading"],
    ]
    with (tmp_path / "internal_all.csv").open("w", newline="") as stream:
        writer = csv.writer(stream)
        writer.writerow(
            ["Address", "Content Type", "Status Code", "Indexability", "Title 1", "H1-1"]
        )
        writer.writerows(rows)
    context = AuditContext(load_exports(str(tmp_path)), load_config(None))
    context.config["semantic_similarity"]["cache_path"] = str(tmp_path / "cache.sqlite")
    context.semantic_adapter = LocalEmbeddingAdapter(
        "two-sided-fixture",
        "1",
        "/fixture",
        lambda texts: [[0.0, 1.0] if "cakes" in text else [1.0, 0.0] for text in texts],
    )
    monkeypatch.setattr(
        heuristics,
        "_stored_html_reader",
        lambda ctx: (
            lambda url: (
                "<main>cakes pastry body</main>"
                if url.endswith("/c")
                else "<main>water supply body</main>"
            )
        ),
    )
    try:
        check_semantic_content(context)
        fired = {}
        for issue in context.issues:
            fired.setdefault(issue.check, set()).add(issue.target_url)
        members = {group.check: set(group.urls) for group in context.groups}
        assert "https://example.test/a" in fired.get("TITLE_LOW_SEMANTIC_RELEVANCE", set())
        assert "https://example.test/b" not in fired.get("TITLE_LOW_SEMANTIC_RELEVANCE", set())
        assert "https://example.test/b" in fired.get("H1_LOW_SEMANTIC_RELEVANCE", set())
        assert "https://example.test/a" not in fired.get("H1_LOW_SEMANTIC_RELEVANCE", set())
        assert "https://example.test/a" in members.get("SEMANTIC_SIMILARITY_CANDIDATE", set())
        assert "https://example.test/c" not in members.get("SEMANTIC_SIMILARITY_CANDIDATE", set())
    finally:
        context.close()
