"""Offline tests for near-duplicate detection with SimHash and LSH."""

import random

import pytest

from seohead.checks import duplicate as D

TEXT_A = (
    "Search engine optimization is the process of improving the quality and "
    "quantity of website traffic from search engines. SEO targets unpaid traffic "
    "rather than direct traffic or paid traffic."
)
TEXT_A_COPY = (
    "Search engine optimization is the process of improving the quality and "
    "quantity of website traffic from search engines. SEO targets unpaid traffic "
    "rather than direct traffic or paid traffic."
)
TEXT_B = (
    "Today is an excellent day to prepare dinner with fresh vegetables and fish. "
    "The recipe is simple and quick enough even for a beginner cook."
)
TEXT_C = "Company contact information: telephone number and email address."


@pytest.mark.parametrize(
    "threshold,distance", [(0.92, 5), (0.8, 12), (0.5, 32), (1.0, 0), (0.0, 64)]
)
def test_threshold_adaptive_bands_retain_every_qualifying_pair(monkeypatch, threshold, distance):
    rng = random.Random(1307)
    for _ in range(30):
        left = rng.getrandbits(64)
        right = left
        for bit in rng.sample(range(64), distance):
            right ^= 1 << bit
        values = iter((left, right))
        monkeypatch.setattr(D, "_weighted_simhash", lambda *args, values=values: next(values))
        result = D.find_duplicates(
            [
                {"id": "a", "text": "alpha distinct text"},
                {"id": "b", "text": "beta distinct text"},
            ],
            threshold=threshold,
        )
        assert result["clusters"][0]["members"] == ["a", "b"]


def test_adaptive_bands_keep_thousand_fingerprints_within_finite_work_budget(monkeypatch):
    rng = random.Random(1307)
    values = iter(rng.getrandbits(64) for _ in range(1000))
    monkeypatch.setattr(D, "_weighted_simhash", lambda *args: next(values))
    result = D.find_duplicates(
        [{"id": str(i), "text": f"synthetic unique document {i}"} for i in range(1000)],
        max_candidate_comparisons=250000,
    )
    assert result["count"] == 1000 and not result["clusters"]


@pytest.mark.parametrize("threshold", [-0.1, 1.1, float("nan"), float("inf")])
def test_invalid_duplicate_threshold_is_rejected(threshold):
    with pytest.raises(ValueError, match="threshold"):
        D.find_duplicates([], threshold=threshold)


def test_simhash_identical_texts_match():
    assert D.simhash(TEXT_A) == D.simhash(TEXT_A_COPY)
    assert D.similarity(D.simhash(TEXT_A), D.simhash(TEXT_A_COPY)) == 1.0


def test_simhash_different_texts_differ():
    fp_a = D.simhash(TEXT_A)
    fp_b = D.simhash(TEXT_B)
    assert fp_a != fp_b
    assert D.similarity(fp_a, fp_b) < 0.92


def test_fnv1a_is_deterministic():
    assert D.fnv1a_64("hello") == D.fnv1a_64("hello")
    assert D.fnv1a_64("hello") != D.fnv1a_64("world")


def test_shingles_overlap():
    sh = D.shingles(["a", "b", "c", "d"], k=3)
    assert sh == [("a", "b", "c"), ("b", "c", "d")]


def test_find_duplicates_reports_exact_pair_as_exact_not_near():
    # /page-a and /page-a-copy are byte-for-byte identical text: an exact
    # duplicate, not a "near" one, so it must not double-report as a cluster.
    items = [
        {"id": "/page-a", "text": TEXT_A},
        {"id": "/page-a-copy", "text": TEXT_A_COPY},
        {"id": "/page-b", "text": TEXT_B},
        {"id": "/contacts", "text": TEXT_C},
    ]
    r = D.find_duplicates(items)
    assert r["ok"] is True
    assert r["count"] == 4
    assert r["clusters"] == []
    assert len(r["exact_duplicates"]) == 1
    assert set(r["exact_duplicates"][0]["members"]) == {"/page-a", "/page-a-copy"}


def test_near_cluster_found_while_exact_pair_excluded_from_it():
    # A known near-duplicate cluster (similar wording, not identical) alongside
    # a known exact-duplicate pair: the near pass must find the former and
    # must not also list the latter as a cluster.
    common = "the quick brown fox jumps over the lazy dog every single morning near the office"
    near_1 = common + " during the alpha release cycle"
    near_2 = common + " during the beta release cycle"
    items = [
        {"id": "/near-1", "text": near_1},
        {"id": "/near-2", "text": near_2},
        {"id": "/exact-a", "text": TEXT_A},
        {"id": "/exact-b", "text": TEXT_A_COPY},
    ]
    r = D.find_duplicates(items, threshold=0.8)
    assert r["ok"] is True
    cluster_members = {frozenset(c["members"]) for c in r["clusters"]}
    assert frozenset({"/near-1", "/near-2"}) in cluster_members
    assert not any({"/exact-a", "/exact-b"} <= set(c["members"]) for c in r["clusters"])
    assert {"/exact-a", "/exact-b"} == set(r["exact_duplicates"][0]["members"])


def test_rerun_with_new_threshold_is_pure_and_needs_no_new_data():
    # A stored corpus can be re-run at a different threshold at zero request
    # cost because find_duplicates never performs I/O.
    items = [{"id": "1", "text": TEXT_A}, {"id": "2", "text": TEXT_B}]
    first = D.find_duplicates(items, threshold=0.5)
    second = D.find_duplicates(items, threshold=0.99)
    assert first == D.find_duplicates(items, threshold=0.5)  # deterministic
    assert second != first


def test_only_indexable_excludes_non_indexable_items_by_default():
    items = [
        {"id": "/canonical-target", "text": TEXT_A, "indexable": True},
        {"id": "/canonicalized-twin", "text": TEXT_A_COPY, "indexable": False},
    ]
    default = D.find_duplicates(items)
    assert default["count"] == 1
    assert default["excluded_non_indexable"] == 1
    assert default["exact_duplicates"] == []

    audit_canonicals = D.find_duplicates(items, only_indexable=False)
    assert audit_canonicals["count"] == 2
    assert audit_canonicals["excluded_non_indexable"] == 0
    assert set(audit_canonicals["exact_duplicates"][0]["members"]) == {
        "/canonical-target",
        "/canonicalized-twin",
    }


def test_threshold_respected():
    # Two moderately similar texts share a prefix but have different endings.
    base = "SEO audit checks titles meta headings links images structured data."
    t1 = base + " Unique tail one about technical crawling and indexing."
    t2 = base + " Completely different ending about content marketing strategy."
    items = [{"id": "1", "text": t1}, {"id": "2", "text": t2}]
    # The loose threshold forms a cluster; the strict threshold does not.
    loose = D.find_duplicates(items, threshold=0.5)
    strict = D.find_duplicates(items, threshold=0.99)
    assert len(loose["clusters"]) >= 1
    assert len(strict["clusters"]) == 0


def test_empty_input_returns_empty():
    r = D.find_duplicates([])
    assert r["count"] == 0 and r["clusters"] == []


def test_lsh_finds_candidates_without_pairwise_all():
    # A is near B and B is near C, so transitivity should place all three together.
    common = "the quick brown fox jumps over the lazy dog every single morning"
    items = [
        {"id": "A", "text": common + " alpha version notes for release one"},
        {"id": "B", "text": common + " beta version notes for release two"},
        {"id": "C", "text": common + " gamma version notes for release three"},
    ]
    r = D.find_duplicates(items, threshold=0.7)
    assert len(r["clusters"]) >= 1
    members = set()
    for c in r["clusters"]:
        members |= set(c["members"])
    assert {"A", "B", "C"} <= members


def test_fingerprints_hidden_by_default():
    items = [{"id": "a", "text": TEXT_A}, {"id": "b", "text": TEXT_B}]
    out = D.find_duplicates(items)
    assert out["ok"] and "fingerprints" not in out
    assert out["count"] == 2


def test_fingerprints_on_request():
    items = [{"id": "a", "text": TEXT_A}, {"id": "b", "text": TEXT_B}]
    out = D.find_duplicates(items, with_fingerprints=True)
    assert set(out["fingerprints"]) == {"a", "b"}


def test_fingerprints_empty_input_respects_flag():
    assert "fingerprints" not in D.find_duplicates([])
    assert D.find_duplicates([], with_fingerprints=True)["fingerprints"] == {}


def _make_templated_corpus(n, rng, template_tokens=600, tail_tokens=30):
    """A shared "site template" plus a per-document unique random tail, so every
    document is genuinely distinct despite sharing most of its text."""
    template = " ".join(f"tpl{i:04d}" for i in range(template_tokens))
    items = []
    for i in range(n):
        tail = " ".join(f"u{i}_{j}_{rng.randint(0, 10**9)}" for j in range(tail_tokens))
        items.append({"id": f"/page-{i}", "text": template + " " + tail})
    return items


def test_bridging_pair_is_not_joined_into_a_transitive_mega_cluster():
    # Issue #161: A is near B (0.9219) and B is near C (0.9531), but A and C
    # (0.9062) never clear the threshold together. Transitive union-find must
    # not silently place all three in one cluster on the strength of edges that
    # never included the A-C pair.
    base = " ".join(f"shared{i:03d}" for i in range(300))
    tail_a = " ".join(f"onlyA{i}" for i in range(12))
    tail_c = " ".join(f"onlyC{i}" for i in range(12))
    items = [
        {"id": "A", "text": base + " " + tail_a},
        {"id": "B", "text": base},
        {"id": "C", "text": base + " " + tail_c},
    ]
    threshold = 0.92
    r = D.find_duplicates(items, threshold=threshold)
    all_members = {m for c in r["clusters"] for m in c["members"]}
    assert not {"A", "B", "C"} <= all_members
    # The module's contract: every pair inside every reported cluster clears
    # the threshold, not merely the edges LSH happened to compare.
    for c in r["clusters"]:
        assert c["min_similarity"] >= threshold
        for p in c["pairs"]:
            assert p["similarity"] >= threshold


def test_every_reported_cluster_is_a_verified_clique():
    # General form of the bridging fixture: for any cluster this module
    # returns, every pairwise similarity it reports must clear the threshold —
    # a cluster is complete-linkage, not just transitively connected.
    common = "the quick brown fox jumps over the lazy dog every single morning"
    items = [
        {"id": "A", "text": common + " alpha version notes for release one"},
        {"id": "B", "text": common + " beta version notes for release two"},
        {"id": "C", "text": common + " gamma version notes for release three"},
    ]
    threshold = 0.7
    r = D.find_duplicates(items, threshold=threshold)
    for c in r["clusters"]:
        for p in c["pairs"]:
            assert p["similarity"] >= threshold


def test_templated_corpus_does_not_collapse_into_one_false_cluster():
    # Issue #162: on a corpus dominated by shared template text, LSH bands
    # used to fill one giant bucket and union-find then merged nearly the
    # whole corpus into a single NEAR_DUPLICATE cluster, even though every
    # page's tail here is independently random — no two pages are alike.
    rng = random.Random(1234)
    items = _make_templated_corpus(200, rng)
    r = D.find_duplicates(items, threshold=0.92)
    assert r["count"] == 200
    assert r["clusters"] == []


def test_templated_corpus_still_finds_a_genuine_duplicate_pair():
    # Damping the shared template must not blind the tool to two documents
    # that are genuinely alike beyond the template: they share a second
    # "extra" block that only they share, so it is not damped away.
    rng = random.Random(55)
    items = _make_templated_corpus(40, rng, template_tokens=200, tail_tokens=20)
    extra = " ".join(f"extra{i:03d}" for i in range(40))
    template = " ".join(f"tpl{i:04d}" for i in range(200))
    items[0]["text"] = template + " " + extra + " tailone"
    items[1]["text"] = template + " " + extra + " tailtwo"
    r = D.find_duplicates(items, threshold=0.92)
    cluster_members = {frozenset(c["members"]) for c in r["clusters"]}
    assert frozenset({"/page-0", "/page-1"}) in cluster_members
    assert len(r["clusters"]) == 1  # nothing else in the corpus is genuinely alike


def test_template_shingles_ignored_below_minimum_corpus_size():
    # Below _MIN_DOCS_FOR_TEMPLATE_FILTER, "shared by nearly everyone" describes
    # a real near-duplicate set, not a template, so nothing is damped.
    shared = D.shingles(["shared"] * 10, k=3)
    docs = [shared for _ in range(3)]
    assert D._template_shingles(docs) == set()


def test_template_shingles_detected_above_minimum_corpus_size():
    template_tokens = [f"shared{i}" for i in range(50)]
    docs = []
    for i in range(25):
        tail = [f"only{i}_{j}" for j in range(5)]
        docs.append(D.shingles(template_tokens + tail, k=3))
    template = D._template_shingles(docs)
    assert D.shingles(template_tokens, k=3)[0] in template
    # A shingle unique to one document is never treated as template text.
    assert D.shingles(["only0_0", "only0_1", "only0_2"], k=3)[0] not in template


def test_items_with_no_id_or_url_are_counted_not_silently_dropped():
    """Issue #477: an item with neither id nor url must be visibly excluded,
    with count plus exclusion counters reconciling to the input size."""
    items = [
        {"id": "a", "text": "x" * 50},
        {"text": "y" * 50},
        {"id": "b", "text": "z" * 50},
    ]
    r = D.find_duplicates(items)
    assert r["excluded_no_id"] == 1
    assert r["count"] + r["excluded_non_indexable"] + r["excluded_no_id"] == 3


def test_all_items_with_ids_leave_excluded_no_id_at_zero():
    """Negative control: every item has a valid id, so the new counter must
    stay at 0 and count must equal len(items) as before."""
    items = [{"id": "a", "text": "x" * 50}, {"id": "b", "text": "z" * 50}]
    r = D.find_duplicates(items)
    assert r["excluded_no_id"] == 0
    assert r["count"] == len(items)


def test_empty_or_missing_text_is_excluded_from_duplicate_evidence():
    """Issues #579: failed extraction must not become an exact-duplicate claim."""
    items = [
        {"id": "empty", "text": ""},
        {"id": "missing", "text": None},
        {"id": "whitespace", "text": " \n\t "},
        {"id": "content", "text": TEXT_A},
    ]

    r = D.find_duplicates(items)

    assert r["count"] == 1
    assert r["exact_duplicates"] == []
    assert r["excluded_no_text"] == 3
    assert r["count"] + r["excluded_non_indexable"] + r["excluded_no_id"] + r[
        "excluded_no_text"
    ] + r["excluded_duplicate_id"] == len(items)


def test_duplicate_ids_are_counted_without_overwriting_the_first_item():
    """Issue #580: keep one stable item and account for every colliding input."""
    items = [
        {"id": "a", "text": TEXT_A},
        {"id": "a", "text": TEXT_B},
        {"id": "b", "text": TEXT_A_COPY},
    ]

    r = D.find_duplicates(items)

    assert r["count"] == 2
    assert r["excluded_duplicate_id"] == 1
    assert r["excluded_no_text"] == 0
    assert r["exact_duplicates"] == [
        {
            "hash": D.content_hash(TEXT_A),
            "members": ["a", "b"],
        }
    ]
    assert r["count"] + r["excluded_non_indexable"] + r["excluded_no_id"] + r[
        "excluded_no_text"
    ] + r["excluded_duplicate_id"] == len(items)
