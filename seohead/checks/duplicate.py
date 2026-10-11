"""Find near-duplicate pages with SimHash and locality-sensitive hashing (LSH).

Large websites often contain tens or hundreds of thin or duplicated pages caused
by faceted pagination, copied content, and utility routes. Comparing every pair
is O(n²): 10,000 pages require roughly 50 million comparisons. SimHash plus LSH
reduces that workload by mapping each page to a 64-bit fingerprint, placing likely
matches in shared bands, and comparing only candidates from a common band.

The implementation uses Charikar's SimHash algorithm:
  1. Text is tokenized into overlapping k-shingles (k=3 by default).
  2. Each shingle is mapped to a deterministic 64-bit FNV-1a hash.
  3. For each bit, the hashes contribute +1 or -1; a positive sum sets that bit in
     the document fingerprint.
  4. Fingerprint similarity is ``1 - (Hamming distance / 64)``.

Every real site is templated: most of any page's text is shared chrome that
survived content-area scoping (see ``content_area.py``) — a repeated CTA, a
disclaimer, a related-items rail. Left alone, that shared text dominates step 3's
majority vote on every document, so the whole corpus converges on nearly the same
fingerprint regardless of how different each page's own content actually is, and
every LSH band collides into one bucket instead of pruning anything (issue #162).
Once a corpus is large enough for the statistic to mean something
(``_MIN_DOCS_FOR_TEMPLATE_FILTER``), ``find_duplicates`` first finds shingles that
occur in nearly every document (``_TEMPLATE_DOC_FREQ_RATIO``) and zeroes their
vote before hashing: they carry no information about which documents are alike,
so they should not get to decide the fingerprint. Below that corpus size no
shingle is discounted, because two or three documents legitimately sharing "all"
of their shingles is exactly the near-duplicate case this tool exists to catch,
not a template.

For candidate retrieval, LSH divides the (template-damped) 64-bit fingerprint
into fixed-width bands. Two documents become candidates when at least one band
matches, and the exact Hamming similarity must still meet the configured
threshold before an edge joins them. That per-edge check alone does not make a
valid cluster: chaining A-B and B-C edges connects A and C by transitivity even
when A and C were never compared and do not meet the threshold together (issue
#161). So each connected component is re-verified afterward and, if it is not
already a clique, split into complete-linkage subgroups — every member pair
inside a reported cluster, not just the edges LSH happened to follow, is
guaranteed to meet ``threshold``.

Exact duplicates (identical text) are found separately, by hashing each
document's text with SHA-1. A cluster whose members are all exact duplicates
of one another is reported only under ``exact_duplicates``, never under
``clusters`` — otherwise a byte-identical pair would be reported twice, once
as "exact" and once as its own trivial "near" cluster at similarity 1.0.
Hashing the caller-supplied text (rather than raw response bytes) is what
makes the comparison survive a page-unique CSRF token or timestamp: the text
extractor is expected to have already stripped markup and, ideally, scoped
itself to the page's content area (see ``content_area.py``).

Both exact and near comparisons default to indexable pages only — a page
canonicalised to another is an intended twin, not a defect — via the
``indexable`` flag on each item and the ``only_indexable`` parameter.

This module is pure and performs no network access. The caller supplies page text
from a Screaming Frog crawl, live ``parse`` calls, or another audit data source.
Because it is pure, re-running with a new threshold against a stored corpus
costs no requests.
"""

from __future__ import annotations

import hashlib
import math
import re
from collections import defaultdict
from typing import Any

# Standard 64-bit FNV-1a constants.
_FNV_OFFSET = 0xCBF29CE484222325
_FNV_PRIME = 0x100000001B3
_MASK64 = (1 << 64) - 1

# The helper's original split remains a default for direct callers;
# find_duplicates selects narrower, threshold-adaptive bands below.
_BANDS = 16
_BAND_BITS = 4
_DEFAULT_THRESHOLD = 0.92  # 1 - 5/64 ~= 0.922; lower similarity is not a match

# A shingle occurring in this fraction of the corpus or more is treated as the
# site's own template rather than page content (see the module docstring).
_TEMPLATE_DOC_FREQ_RATIO = 0.9
# Below this many documents, "shared by nearly everyone" is not a meaningful
# statistic — it is indistinguishable from a small set of genuine near-duplicates.
_MIN_DOCS_FOR_TEMPLATE_FILTER = 20


class DuplicateBudgetExceeded(ValueError):
    """A caller-selected finite duplicate-analysis work budget was exhausted."""


def _tokenize(text: str) -> list[str]:
    """Return lowercase alphanumeric tokens with at least two characters.

    Punctuation and HTML entities are discarded. The tokenizer is language
    independent because Python's Unicode-aware ``\\w`` also covers Cyrillic.
    """
    if not text:
        return []
    return [t for t in re.findall(r"\w{2,}", text.lower(), flags=re.UNICODE) if len(t) >= 2]


def shingles(tokens: list[str], k: int = 3) -> list[tuple[str, ...]]:
    """Return overlapping k-shingles, or one shingle when fewer than k tokens exist."""
    if not tokens:
        return []
    if len(tokens) <= k:
        return [tuple(tokens)]
    return [tuple(tokens[i : i + k]) for i in range(len(tokens) - k + 1)]


def fnv1a_64(data: str) -> int:
    """Return a deterministic, dependency-free 64-bit FNV-1a string hash."""
    h = _FNV_OFFSET
    for byte in data.encode("utf-8"):
        h ^= byte
        h = (h * _FNV_PRIME) & _MASK64
    return h


def _weighted_simhash(
    sh: list[tuple[str, ...]], weight: dict[tuple[str, ...], float] | None = None
) -> int:
    """Return a 64-bit SimHash fingerprint from precomputed shingles.

    ``weight`` scales an individual shingle's vote; a shingle absent from the
    mapping (or ``weight=None``) counts fully, at 1.0. ``find_duplicates`` uses
    this to zero out shingles that turn out to be the corpus's shared template
    rather than page content (see ``_template_shingles``).
    """
    if not sh:
        return 0
    # Accumulate each bit's +1/-1 vote, scaled by shingle weight, across all shingles.
    v = [0.0] * 64
    for piece in sh:
        w = 1.0 if weight is None else weight.get(piece, 1.0)
        if w == 0.0:
            continue
        h = fnv1a_64(" ".join(piece))
        for i in range(64):
            v[i] += w if (h >> i) & 1 else -w
    fp = 0
    for i in range(64):
        if v[i] > 0:
            fp |= 1 << i
    return fp


def simhash(text: str, k: int = 3) -> int:
    """Return a 64-bit SimHash fingerprint; identical texts produce the same hash."""
    return _weighted_simhash(shingles(_tokenize(text), k))


def hamming(a: int, b: int) -> int:
    """Return the Hamming distance between two 64-bit fingerprints."""
    return (a ^ b).bit_count()


def similarity(a: int, b: int) -> float:
    """Return similarity in [0, 1] as ``1 - Hamming distance / 64``."""
    return 1.0 - hamming(a, b) / 64.0


def content_hash(text: str) -> str:
    """Return a SHA-1 hex digest of ``text``, for exact-duplicate grouping.

    Hashing the extracted text rather than raw response bytes means a
    per-request CSRF token or timestamp embedded elsewhere in the markup does
    not defeat the comparison.
    """
    return hashlib.sha1((text or "").encode("utf-8"), usedforsecurity=False).hexdigest()


def _band_keys(fp: int, bands: int = _BANDS, band_bits: int = _BAND_BITS) -> list[int]:
    """Return the fingerprint value for each LSH band as its bucket key."""
    keys: list[int] = []
    mask = (1 << band_bits) - 1
    for i in range(bands):
        keys.append((fp >> (i * band_bits)) & mask)
    return keys


def _template_shingles(
    shingle_lists: list[list[tuple[str, ...]]],
) -> set[tuple[str, ...]]:
    """Return shingles occurring in at least ``_TEMPLATE_DOC_FREQ_RATIO`` of the corpus.

    Below ``_MIN_DOCS_FOR_TEMPLATE_FILTER`` documents this returns an empty set: with
    only a handful of documents, "shared by nearly all of them" describes the exact
    near-duplicate case this tool exists to find, not a template to discount.
    """
    n = len(shingle_lists)
    if n < _MIN_DOCS_FOR_TEMPLATE_FILTER:
        return set()
    doc_freq: dict[tuple[str, ...], int] = defaultdict(int)
    for sh in shingle_lists:
        for piece in set(sh):
            doc_freq[piece] += 1
    cutoff = n * _TEMPLATE_DOC_FREQ_RATIO
    return {piece for piece, df in doc_freq.items() if df >= cutoff}


def _complete_linkage_groups(
    members: list[str], fingerprints: dict[str, int], threshold: float
) -> list[list[str]]:
    """Split a union-find component into groups where every internal pair meets ``threshold``.

    LSH candidate retrieval only guarantees that each edge it followed meets the
    threshold, not every pair reachable by chaining edges together — a component
    is not automatically a valid cluster (issue #161). Finding the true maximum
    clique cover is NP-hard; this greedily grows one clique at a time, starting
    from the strongest remaining edge, so the most similar pairs are the ones
    that survive — never dropped in favor of a weaker pair that happened to claim
    a shared member first. Every group returned really is a clique, though a
    weaker edge whose endpoints both end up claimed by a stronger clique is
    reported as no cluster at all rather than forced into an invalid one.
    """
    adjacency: dict[str, set[str]] = {m: set() for m in members}
    edges: list[tuple[float, str, str]] = []
    for i, a in enumerate(members):
        for b in members[i + 1 :]:
            sim = similarity(fingerprints[a], fingerprints[b])
            if sim >= threshold:
                adjacency[a].add(b)
                adjacency[b].add(a)
                edges.append((sim, a, b))
    edges.sort(key=lambda e: (-e[0], e[1], e[2]))

    remaining = set(members)
    groups: list[list[str]] = []
    for _sim, a, b in edges:
        if a not in remaining or b not in remaining:
            continue  # one endpoint already claimed by a stronger clique
        clique = {a, b}
        for node in sorted(remaining - clique):
            if clique <= adjacency[node]:  # node is adjacent to every member so far
                clique.add(node)
        remaining -= clique
        groups.append(sorted(clique))
    return groups


_UNMEASURED_REASON = (
    "no pair of comparable documents was compared; duplicate content is unmeasured, not absent"
)


def find_duplicates(
    items: list[dict[str, Any]],
    threshold: float = _DEFAULT_THRESHOLD,
    k: int = 3,
    with_fingerprints: bool = False,
    only_indexable: bool = True,
    max_shingles: int | None = None,
    max_candidate_comparisons: int | None = None,
) -> dict[str, Any]:
    """Find exact and near-duplicate groups in a list of documents.

    Each item is ``{"id": str, "text": str}``, where ``id`` may be a URL or
    any stable key, plus an optional ``indexable`` flag (default True). A
    cluster is complete-linkage: every pair of its members, not only the pairs
    LSH happened to compare, has exact Hamming similarity at least
    ``threshold`` (see the module docstring for both the LSH candidate step
    and the template-damping that keeps a templated corpus sub-quadratic).

    ``only_indexable`` (default True) drops non-indexable items before either
    comparison: a page canonicalised to another is an intended twin, not a
    defect. Set it to False to audit the canonical tags themselves. Items
    without usable text or a unique identifier are excluded and counted rather
    than represented as duplicate-content evidence.

    ``with_fingerprints`` includes each document fingerprint. It is disabled by
    default because the mapping can dominate output for hundreds of pages and is
    mainly useful for debugging fingerprint behavior.
    """
    if not math.isfinite(threshold) or not 0 <= threshold <= 1:
        raise ValueError("duplicate threshold must be finite and in [0, 1]")
    excluded_non_indexable = 0
    if only_indexable:
        kept = [it for it in items if it.get("indexable", True)]
        excluded_non_indexable = len(items) - len(kept)
        items = kept

    if not items:
        out: dict[str, Any] = {
            "ok": True,
            "measured": False,
            "reason": _UNMEASURED_REASON,
            "count": 0,
            "candidate_pairs_checked": 0,
            "clusters": [],
            "exact_duplicates": [],
            "excluded_non_indexable": excluded_non_indexable,
            "excluded_no_id": 0,
            "excluded_no_text": 0,
            "excluded_duplicate_id": 0,
        }
        if with_fingerprints:
            out["fingerprints"] = {}
        return out

    texts: dict[str, str] = {}
    doc_shingles: dict[str, list[tuple[str, ...]]] = {}
    shingle_count = 0
    excluded_no_id = 0
    excluded_no_text = 0
    excluded_duplicate_id = 0
    for it in items:
        doc_id = it.get("id") or it.get("url") or ""
        text = it.get("text")
        if not doc_id:
            excluded_no_id += 1
        elif not isinstance(text, str) or not text.strip():
            excluded_no_text += 1
        else:
            doc_id = str(doc_id)
            if doc_id in texts:
                excluded_duplicate_id += 1
                continue
            texts[doc_id] = text
            pieces = shingles(_tokenize(text), k)
            shingle_count += len(pieces)
            if max_shingles is not None and shingle_count > max_shingles:
                raise DuplicateBudgetExceeded("duplicate shingle budget exceeded")
            doc_shingles[doc_id] = pieces

    # A shingle in nearly every document is the site's template, not distinguishing
    # content; damping it before hashing is what keeps a templated corpus's
    # fingerprints — and therefore its LSH buckets — from collapsing onto each
    # other (see the module docstring).
    template = _template_shingles(list(doc_shingles.values()))
    weight = {piece: 0.0 for piece in template} if template else None
    fingerprints: dict[str, int] = {
        doc_id: _weighted_simhash(sh, weight) for doc_id, sh in doc_shingles.items()
    }

    # Exact groups are found by content hash, independent of the near-duplicate
    # pass below, so they are unaffected by threshold or LSH banding.
    hash_groups: dict[str, list[str]] = defaultdict(list)
    for doc_id, text in texts.items():
        hash_groups[content_hash(text)].append(doc_id)
    exact_duplicates = [
        {"hash": h, "members": sorted(members)}
        for h, members in hash_groups.items()
        if len(members) > 1
    ]
    exact_duplicates.sort(key=lambda g: g["hash"])
    hash_of: dict[str, str] = {
        doc_id: h for h, members in hash_groups.items() for doc_id in members
    }

    # At most d differing bits fit the threshold. Splitting disjoint bits
    # across d+1 bands guarantees at least one equal band (pigeonhole), while
    # avoiding the broad 4-bit buckets at the usual 0.92 threshold. Any unused
    # high bits only add candidates; full 64-bit Hamming still verifies them.
    bands = min(64, math.floor(64 * (1 - threshold)) + 1)
    band_bits = 64 // bands
    buckets: dict[tuple[int, int], list[str]] = defaultdict(list)
    for doc_id, fp in fingerprints.items():
        keys = _band_keys(fp, bands, band_bits) if threshold > 0 else [0]
        for band_idx, key in enumerate(keys):
            buckets[(band_idx, key)].append(doc_id)

    # Union-find converts matching candidate pairs into transitive clusters.
    parent: dict[str, str] = {doc_id: doc_id for doc_id in fingerprints}

    def find(x: str) -> str:
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a: str, b: str) -> None:
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[rb] = ra

    candidate_pairs: set[tuple[str, str]] = set()
    candidate_comparisons = 0
    for members in buckets.values():
        if len(members) < 2:
            continue
        for i in range(len(members)):
            for j in range(i + 1, len(members)):
                candidate_comparisons += 1
                if (
                    max_candidate_comparisons is not None
                    and candidate_comparisons > max_candidate_comparisons
                ):
                    raise DuplicateBudgetExceeded("duplicate candidate-comparison budget exceeded")
                a, b = members[i], members[j]
                if similarity(fingerprints[a], fingerprints[b]) >= threshold:
                    candidate_pairs.add(tuple(sorted((a, b))))
                    union(a, b)

    # Group documents by their union-find root. Candidate retrieval only connects
    # pairs that individually cleared the threshold, so a component is a set of
    # documents reachable through such edges — not yet a verified cluster (issue
    # #161: B can bridge unrelated A and C by being near both).
    components: dict[str, list[str]] = defaultdict(list)
    for doc_id in fingerprints:
        components[find(doc_id)].append(doc_id)

    clusters: list[dict[str, Any]] = []
    for members in components.values():
        if len(members) < 2:
            continue  # A singleton cannot be a duplicate cluster.
        # Split into complete-linkage subgroups so every reported cluster's pairs,
        # not only the edges LSH followed, meet the threshold.
        for clique in _complete_linkage_groups(members, fingerprints, threshold):
            # A clique where every member shares the same content hash is fully
            # explained by exact duplication and already listed in
            # exact_duplicates; reporting it again here would double-report it.
            if len({hash_of[m] for m in clique}) == 1:
                continue
            pairs = [
                {
                    "a": a,
                    "b": b,
                    "similarity": round(similarity(fingerprints[a], fingerprints[b]), 4),
                }
                for i, a in enumerate(clique)
                for b in clique[i + 1 :]
            ]
            clusters.append(
                {
                    "members": clique,
                    "pairs": pairs,
                    "min_similarity": min(p["similarity"] for p in pairs),
                }
            )

    clusters.sort(key=lambda c: -c["min_similarity"])
    result: dict[str, Any] = {
        "ok": True,
        "count": len(fingerprints),
        "threshold": threshold,
        "clusters": clusters,
        "exact_duplicates": exact_duplicates,
        "candidate_pairs_checked": len(candidate_pairs),
        "excluded_non_indexable": excluded_non_indexable,
        "excluded_no_id": excluded_no_id,
        "excluded_no_text": excluded_no_text,
        "excluded_duplicate_id": excluded_duplicate_id,
    }
    # Zero compared pairs means no duplicate was measured at all. An empty
    # clusters list then reads as "no duplicates", which this run never showed.
    result["measured"] = bool(candidate_pairs)
    if not candidate_pairs:
        result["reason"] = _UNMEASURED_REASON
    if with_fingerprints:
        result["fingerprints"] = fingerprints
    return result
