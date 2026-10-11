from __future__ import annotations

import sqlite3
from concurrent.futures import ThreadPoolExecutor

import pytest

from seohead.checks import semantic_similarity
from seohead.checks.semantic_similarity import (
    EmbeddingCache,
    LocalEmbeddingAdapter,
    ProviderEmbeddingAdapter,
    analyze_semantic_documents,
    analyze_semantic_relevance,
)


def _documents():
    return [
        {
            "url": "https://example.test/pumps",
            "text": "pump stations for industrial water supply",
            "normalized_sha256": "a" * 64,
            "language": {"declared_primary": "en"},
        },
        {
            "url": "https://example.test/water",
            "text": "industrial water supply pump equipment",
            "normalized_sha256": "b" * 64,
            "language": {"declared_primary": "en"},
        },
        {
            "url": "https://example.test/cakes",
            "text": "cakes and pastries for a birthday party",
            "normalized_sha256": "c" * 64,
            "language": {"declared_primary": "en"},
        },
    ]


def test_local_adapter_groups_only_supplied_embedding_evidence_and_reuses_cache(tmp_path):
    calls = []

    def embed(texts):
        calls.append(list(texts))
        return [[1.0, 0.0] if "pump" in text or "water" in text else [0.0, 1.0] for text in texts]

    adapter = LocalEmbeddingAdapter("fixture", "1", "/models/fixture", embed)
    cache = EmbeddingCache(tmp_path / "semantic.sqlite")
    first = analyze_semantic_documents(_documents(), adapter, cache, threshold=0.9)
    second = analyze_semantic_documents(_documents(), adapter, cache, threshold=0.9)

    assert first["ok"] is True
    assert first["groups"][0]["kind"] == "semantic_similarity_candidate"
    assert {member["url"] for member in first["groups"][0]["members"]} == {
        "https://example.test/pumps",
        "https://example.test/water",
    }
    assert "duplicate" in first["groups"][0]["conclusion"]
    assert first["internal_link_candidates"][0]["kind"] == "semantic_internal_link_review_candidate"
    assert len(calls) == 1
    assert second["coverage"]["state"] == "complete"


def test_missing_adapter_is_unavailable_and_never_invents_a_lexical_score(tmp_path):
    result = analyze_semantic_documents(
        _documents(), None, EmbeddingCache(tmp_path / "cache.sqlite")
    )

    assert result["ok"] is False
    assert result["coverage"]["state"] == "unavailable"
    assert result["groups"] == []


def test_provider_is_opt_in_before_any_embed_call(tmp_path):
    adapter = ProviderEmbeddingAdapter(
        "fixture", "fixture", "1", lambda texts: (_ for _ in ()).throw(AssertionError("called"))
    )
    with pytest.raises(PermissionError, match="not explicitly authorized"):
        analyze_semantic_documents(_documents(), adapter, EmbeddingCache(tmp_path / "cache.sqlite"))


def test_cache_invalidates_model_configuration_and_discards_corrupt_vectors(tmp_path):
    calls = []

    def embed(texts):
        calls.append(texts)
        return [[1.0, 0.0] for _ in texts]

    cache = EmbeddingCache(tmp_path / "cache.sqlite", max_entries=3)
    docs = _documents()[:1]
    analyze_semantic_documents(docs, LocalEmbeddingAdapter("m", "1", "/m", embed), cache)
    analyze_semantic_documents(docs, LocalEmbeddingAdapter("m", "2", "/m", embed), cache)
    assert len(calls) == 2

    with sqlite3.connect(tmp_path / "cache.sqlite") as con:
        con.execute("UPDATE semantic_embeddings SET vector_json='not-json'")
    analyze_semantic_documents(docs, LocalEmbeddingAdapter("m", "2", "/m", embed), cache)
    assert len(calls) == 3


def test_candidate_budget_and_invalid_documents_are_explicit(tmp_path):
    adapter = LocalEmbeddingAdapter("m", "1", "/m", lambda texts: [[1.0, 0.0] for _ in texts])
    partial = analyze_semantic_documents(
        [*_documents()[:2], {"url": "https://example.test/missing"}],
        adapter,
        EmbeddingCache(tmp_path / "cache.sqlite"),
        max_candidate_comparisons=0,
    )

    assert partial["ok"] is False
    assert partial["reason"] == "semantic candidate-comparison budget exceeded"
    assert partial["coverage"]["omitted_documents"] == 1


def test_document_bound_is_partial_and_adapter_identity_does_not_leak_a_local_path(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(semantic_similarity, "MAX_DOCUMENTS", 2)
    adapter = LocalEmbeddingAdapter(
        "m", "1", "/private/model", lambda texts: [[1.0] for _ in texts]
    )
    result = analyze_semantic_documents(
        _documents(), adapter, EmbeddingCache(tmp_path / "cache.sqlite")
    )

    assert result["coverage"]["state"] == "partial"
    assert result["coverage"]["omission_reasons"] == {"semantic document limit exceeded": 1}
    assert "model_path" not in result["adapter"]


def test_cache_handles_concurrent_local_writers(tmp_path):
    cache_path = tmp_path / "shared.sqlite"

    def write(index):
        cache = EmbeddingCache(cache_path)
        cache.put(str(index) * 64, "a" * 64, {"model": "fixture"}, [float(index), 1.0])

    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(write, range(1, 9)))

    with sqlite3.connect(cache_path) as con:
        assert con.execute("SELECT COUNT(*) FROM semantic_embeddings").fetchone()[0] == 8


def test_cache_entry_quota_evicts_oldest_vectors(tmp_path):
    cache = EmbeddingCache(tmp_path / "bounded.sqlite", max_entries=1)
    cache.put("a" * 64, "a" * 64, {"model": "fixture"}, [1.0])
    cache.put("b" * 64, "b" * 64, {"model": "fixture"}, [2.0])

    assert cache.get("a" * 64) is None
    assert cache.get("b" * 64) == [2.0]


def test_relevance_reuses_body_vectors_and_reports_missing_headings(tmp_path):
    calls = []
    adapter = LocalEmbeddingAdapter(
        "m", "1", "/m", lambda texts: calls.append(list(texts)) or [[1, 0] for _ in texts]
    )
    cache = EmbeddingCache(tmp_path / "relevance.sqlite")
    docs = [{**_documents()[0], "title": "A pump title", "h1": ""}]
    analyze_semantic_documents(docs, adapter, cache)
    first = analyze_semantic_relevance(docs, adapter, cache)
    second = analyze_semantic_relevance(docs, adapter, cache)
    assert first == second
    assert first["missing_headings"] == {"title": 0, "h1": 1}
    assert first["scores"][0]["low_relevance"] is False
    assert len(calls) == 2 and len(calls[1]) == 1


@pytest.mark.parametrize("vector", ([0.0], [float("inf")], []))
def test_single_document_invalid_vector_never_reports_complete(tmp_path, vector):
    adapter = LocalEmbeddingAdapter("m", "1", "/m", lambda texts: [vector])
    with pytest.raises(ValueError, match="embedding vector"):
        analyze_semantic_documents(
            _documents()[:1], adapter, EmbeddingCache(tmp_path / "cache.sqlite")
        )


def test_extreme_finite_vectors_do_not_overflow_into_false_clean_results(tmp_path):
    adapter = LocalEmbeddingAdapter("m", "1", "/m", lambda texts: [[1e308, 1e308] for _ in texts])
    result = analyze_semantic_documents(
        _documents()[:2], adapter, EmbeddingCache(tmp_path / "cache.sqlite")
    )
    assert result["ok"] is True
    assert result["groups"][0]["evidence"][0]["similarity"] == 1
