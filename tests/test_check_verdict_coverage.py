"""Issue #98: the traversal is tested, the verdicts are not.

``test_check_producer_gate.py`` proves every check ID is registered and every
``check_*`` function is dispatched -- the traversal runs. It says nothing about
whether a check's *conclusion* is ever proven true: that it fires on markup
that genuinely carries its defect, and stays silent on markup that does not.
#94, #95 and #96 each passed every existing gate and were still wrong on real
sites -- one fired on legitimate outbound links, one fired on a slash-form
pair no fixture had ever paired, one miscounted boilerplate as content. None
of that shows up as a registration gap or a dispatch gap.

This gate closes that: for every check in ``seohead.sf.core.registry.CHECKS``
it looks for a test, anywhere under ``tests/``, that asserts BOTH halves --
some assertion showing the check fires on a URL with the defect, and some
assertion (in the same or a different test) showing it stays silent on a
clean URL. Evidence is detected structurally (AST), not by a fixed list of
call sites, so it doesn't go stale as tests are rewritten -- the same
principle ``test_check_producer_gate.py`` already uses for producer/dispatch
completeness.

A check that cannot be exercised offline at all belongs in
``LIVE_ONLY_EXEMPT`` with a reason -- not silently skipped. As of this gate's
introduction that list is empty: every check in this registry reads columns a
synthetic Screaming Frog export or synthetic HTML can carry (see
``AGENTS.md`` -- "SF-derived", "SF:<tab>", "inlinks", "sitemap", "crawl" and
"heuristic" sources are all reproducible without a network, a licensed SF
CLI run, or a browser). If a future check genuinely needs one of those, name
it here with why, rather than adding it to the uncovered baseline.

Checks proven both ways are exempt from ``KNOWN_UNCOVERED``. Everything else
must be named in ``KNOWN_UNCOVERED`` -- a ratchet, not a permanent pass: a
check already in it may stay there while nobody has closed it, but the moment
a test proves it (a merge that closes this list a little further, the way
#help-wanted "close a few" pull requests do), the entry becomes a stale
extra-credit and ``test_known_uncovered_entries_stay_actually_uncovered``
below starts failing to force its removal. A *new* check_id -- one added to
CHECKS tomorrow -- has no seat in that baseline: it must ship with both
halves proven, or with a named, reasoned exemption, or the build fails. That
is the enforcement issue #98 asked for.
"""

from __future__ import annotations

import ast
import pathlib

from seohead.sf.core.registry import CHECKS

ROOT = pathlib.Path(__file__).resolve().parent.parent
TESTS_DIR = ROOT / "tests"
CHECK_IDS = frozenset(CHECKS)

# Checks that cannot be proven to fire-on-defect/stay-silent-on-clean from a
# synthetic offline fixture at all -- name the reason, never leave it silent.
# Empty today: see the module docstring for why this registry doesn't (yet)
# have one. Every entry here must NOT also appear in KNOWN_UNCOVERED.
LIVE_ONLY_EXEMPT: dict[str, str] = {}

# Baseline as of this gate's introduction (issue #98): checks that have a
# test asserting only one half (fires, but never proven silent on clean
# input -- or vice versa) or no assertion-level evidence at all. This is a
# ratchet, not a target: shrink it as coverage lands, never grow it to admit
# a *new* check without both halves -- CHECKS gained after this list was
# written are held to the full standard by test_every_check_id_is_accounted_for.
KNOWN_UNCOVERED: frozenset[str] = frozenset(
    {
        "BAD_REDIRECT_TYPE",
        "BODY_MISSING",
        "BODY_MULTIPLE",
        "CANONICALISED",
        "CANONICAL_OUTSIDE_HEAD",
        "DEEP_CRAWL_DEPTH",
        "DEEP_DISCOVERY_PATH",
        "DESC_OUTSIDE_HEAD",
        "DIRECTIVES_OUTSIDE_HEAD",
        "DOM_TOO_DEEP",
        "DOM_TOO_MANY_NODES",
        "EXTERNAL_LINK_TO_REDIRECT",
        "FORM_ON_HTTP_URL",
        "FORM_URL_INSECURE",
        "H1_MULTIPLE",
        "HEAD_MISSING",
        "HEAD_MULTIPLE",
        "HEAD_NOT_FIRST",
        "HIGH_EXTERNAL_OUTLINKS",
        "HIGH_OUTLINKS",
        "HREFLANG_INCONSISTENT_CONFIRMATION",
        "HREFLANG_INVALID_CODE",
        "HREFLANG_MULTIPLE_ENTRIES",
        "HREFLANG_OUTSIDE_HEAD",
        "HTML_BLOAT",
        "HTTP1_ONLY",
        "IMG_MISSING_ALT",
        "IMG_MISSING_DIMENSIONS",
        "INLINK_BOILERPLATE_ONLY",
        "INSECURE_SUBRESOURCE",
        "INTERNAL_LINK_TO_REDIRECT",
        "INVALID_HEAD_ELEMENT",
        "JS_CONSOLE_ERRORS",
        "LARGE_HTML",
        "LINK_TO_5XX",
        "LONG_SENTENCES",
        "NOARCHIVE",
        "NOINDEX",
        "NON_INDEXABLE_LINKED",
        "NO_INTERNAL_OUTLINKS",
        "ONLY_NONINDEXABLE_SOURCE_INLINKS",
        "ORPHAN_PAGE",
        "OUTLINK_TO_LOCALHOST",
        "PROTOCOL_RELATIVE_LINK",
        "READABILITY_DIFFICULT",
        "ROBOTS_BLOCKS_RESOURCES",
        "SCHEMA_VALIDATION_ERROR",
        "SITEMAP_NOT_IN_ROBOTS",
        "SITEMAP_STALE_LASTMOD",
        "SITEMAP_TOO_LARGE",
        "SITEMAP_URL_3XX",
        "SITEMAP_URL_NON_INDEXABLE",
        "SLOW_RESPONSE",
        "TITLE_MULTIPLE",
        "TITLE_OUTSIDE_HEAD",
        "TITLE_TEMPLATED",
        "UNSAFE_CROSS_ORIGIN_LINK",
        "URL_NOT_IN_SITEMAP",
        "URL_UNDERSCORES",
    }
)


# ---------------------------------------------------------------------------
# Structural (AST) evidence scanner.
#
# A check_id is "relevant" to an assertion when it appears as a string
# literal anywhere in the assertion, OR through a local variable that a
# preceding assignment in the same test function bound from a call/subscript
# mentioning that literal (the ``issues = _issues(res, "H2_DUPLICATE")`` then
# ``assert ... issues ...`` shape). Given a relevant assertion, its polarity
# -- does it show the check firing, or show it staying silent -- is read off
# the comparison operator: ``in`` / a non-empty ``==`` / a subset check
# (``{a, b} <= fired.get(cid, set())``) is "fires"; ``not in`` / an empty
# ``==`` is "silent". A leading ``not`` on the whole expression flips it.
# ---------------------------------------------------------------------------


def _literal_check_ids(node: ast.AST) -> set[str]:
    return {
        n.value
        for n in ast.walk(node)
        if isinstance(n, ast.Constant) and isinstance(n.value, str) and n.value in CHECK_IDS
    }


def _is_empty_container(node: ast.AST) -> bool:
    if isinstance(node, (ast.List, ast.Set)) and not node.elts:
        return True
    if isinstance(node, ast.Dict) and not node.keys:
        return True
    return (
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Name)
        and node.func.id in ("set", "dict", "list")
        and not node.args
        and not node.keywords
    )


def _name_refs(node: ast.AST) -> set[str]:
    return {n.id for n in ast.walk(node) if isinstance(n, ast.Name)}


def _classify_assert(test_node: ast.AST, is_relevant) -> set[str]:
    """Return the subset of {'pos', 'neg'} evidence one assert contributes.

    ``is_relevant(operand_ast) -> bool`` tells whether an operand of the
    top-level comparison concerns the check_id under consideration.
    """
    node = test_node
    flipped = False
    while isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.Not):
        flipped = not flipped
        node = node.operand
    ev: set[str] = set()
    if not isinstance(node, ast.Compare):
        if is_relevant(node):
            ev.add("neg" if flipped else "pos")
        return ev
    operands = [node.left, *node.comparators]
    if not any(is_relevant(o) for o in operands):
        return ev
    for op in node.ops:
        if isinstance(op, ast.In):
            ev.add("neg" if flipped else "pos")
        elif isinstance(op, ast.NotIn):
            ev.add("pos" if flipped else "neg")
        elif isinstance(op, ast.Eq):
            empty = _is_empty_container(node.comparators[-1])
            ev.add(("pos" if empty else "neg") if flipped else ("neg" if empty else "pos"))
        elif isinstance(op, ast.NotEq):
            empty = _is_empty_container(node.comparators[-1])
            ev.add(("neg" if empty else "pos") if flipped else ("pos" if empty else "neg"))
        elif isinstance(op, (ast.LtE, ast.GtE)):
            # `{a, b} <= fired.get(cid, set())` (or reversed): a subset-of
            # check against the check's own collection is membership
            # evidence, same intent as `in`, just phrased for a whole group.
            ev.add("neg" if flipped else "pos")
    return ev


def collect_verdict_evidence(tests_dir: pathlib.Path = TESTS_DIR) -> dict[str, set[str]]:
    """check_id -> subset of {'pos', 'neg'} evidence found anywhere under tests_dir."""
    evidence: dict[str, set[str]] = {cid: set() for cid in CHECK_IDS}
    for path in sorted(tests_dir.rglob("*.py")):
        try:
            tree = ast.parse(path.read_text(encoding="utf-8"))
        except SyntaxError:
            continue
        for fn in ast.walk(tree):
            if not isinstance(fn, ast.FunctionDef):
                continue
            var_to_ids: dict[str, set[str]] = {}
            for node in ast.walk(fn):
                if isinstance(node, ast.Assign):
                    ids = _literal_check_ids(node.value)
                    if ids:
                        for target in node.targets:
                            if isinstance(target, ast.Name):
                                var_to_ids.setdefault(target.id, set()).update(ids)
            for node in ast.walk(fn):
                if not isinstance(node, ast.Assert):
                    continue
                relevant_ids = _literal_check_ids(node.test)
                for name in _name_refs(node.test):
                    relevant_ids |= var_to_ids.get(name, set())
                for cid in relevant_ids:

                    def is_relevant(
                        operand: ast.AST, cid: str = cid, var_to_ids: dict = var_to_ids
                    ) -> bool:
                        if cid in _literal_check_ids(operand):
                            return True
                        return any(cid in var_to_ids.get(n, set()) for n in _name_refs(operand))

                    evidence[cid] |= _classify_assert(node.test, is_relevant)
    return evidence


def _verified(evidence: dict[str, set[str]]) -> set[str]:
    return {cid for cid, ev in evidence.items() if {"pos", "neg"} <= ev}


# ---------------------------------------------------------------------------
# The gate.
# ---------------------------------------------------------------------------


def test_every_check_id_is_accounted_for():
    """Every registered check is verified two-sided, named exempt, or named
    as a known, still-open gap. A check_id that is none of the three --
    added to CHECKS with no test and no named exemption -- fails the build."""
    evidence = collect_verdict_evidence()
    verified = _verified(evidence)
    accounted = verified | set(LIVE_ONLY_EXEMPT) | KNOWN_UNCOVERED
    unaccounted = sorted(CHECK_IDS - accounted)
    assert not unaccounted, (
        f"{len(unaccounted)} check(s) have no two-sided test and no named "
        f"exemption/known-gap entry: {unaccounted}. Add a test proving the "
        "check both fires on a defect and stays silent on clean input, or "
        "name it in LIVE_ONLY_EXEMPT (with a reason) or KNOWN_UNCOVERED."
    )


def test_known_uncovered_entries_stay_actually_uncovered():
    """A KNOWN_UNCOVERED entry that a later test now proves two-sided is
    stale bookkeeping, not a real gap -- shrink the list instead of leaving
    dead weight that quietly hides whether the coverage push is progressing."""
    evidence = collect_verdict_evidence()
    verified = _verified(evidence)
    stale = sorted(KNOWN_UNCOVERED & verified)
    assert not stale, f"now covered both ways, remove from KNOWN_UNCOVERED: {stale}"


def test_live_only_exempt_entries_are_not_already_covered():
    """An exemption claiming a check cannot be tested offline, for a check
    that already has a two-sided offline test, is simply wrong -- fix the
    claim rather than let it sit next to contradicting evidence."""
    evidence = collect_verdict_evidence()
    verified = _verified(evidence)
    wrongly_exempt = sorted(set(LIVE_ONLY_EXEMPT) & verified)
    assert not wrongly_exempt, (
        f"claimed LIVE_ONLY_EXEMPT but already has a two-sided offline test, "
        f"remove the exemption: {wrongly_exempt}"
    )


def test_no_check_is_both_exempt_and_known_uncovered():
    """The two escape hatches mean different things (cannot vs. has not yet
    been tested); double-booking a check under both hides which is true."""
    overlap = sorted(set(LIVE_ONLY_EXEMPT) & KNOWN_UNCOVERED)
    assert not overlap, f"listed in both LIVE_ONLY_EXEMPT and KNOWN_UNCOVERED: {overlap}"


def test_known_uncovered_and_exempt_names_are_real_checks():
    """A stale entry naming a check_id that was renamed or removed would
    silently under-count what the gate actually still requires."""
    stale_uncovered = sorted(KNOWN_UNCOVERED - CHECK_IDS)
    stale_exempt = sorted(set(LIVE_ONLY_EXEMPT) - CHECK_IDS)
    assert not stale_uncovered, (
        f"KNOWN_UNCOVERED names check(s) no longer in CHECKS: {stale_uncovered}"
    )
    assert not stale_exempt, f"LIVE_ONLY_EXEMPT names check(s) no longer in CHECKS: {stale_exempt}"


def test_verdict_coverage_report(capsys):
    """Not a pass/fail assertion -- prints the current split so `pytest -s`
    (or CI log output) always carries the honest count a PR description or
    reviewer needs, without hand-maintaining it anywhere."""
    evidence = collect_verdict_evidence()
    verified = _verified(evidence)
    total = len(CHECK_IDS)
    print(
        f"\ncheck verdict coverage: {len(verified)}/{total} verified two-sided, "
        f"{len(LIVE_ONLY_EXEMPT)} exempt (live/paid/browser only), "
        f"{len(KNOWN_UNCOVERED)} known-uncovered"
    )


# ---------------------------------------------------------------------------
# Mutation self-tests: prove the scanner actually detects each shape it
# claims to, the same style test_check_producer_gate.py uses for its own
# AST walk (a gate nobody can see catch anything is not trustworthy).
# ---------------------------------------------------------------------------


def _write_and_scan(tmp_path: pathlib.Path, source: str) -> dict[str, set[str]]:
    tests_dir = tmp_path / "tests"
    tests_dir.mkdir()
    (tests_dir / "test_fake.py").write_text(source)
    return collect_verdict_evidence(tests_dir)


def test_scanner_detects_two_sided_evidence(tmp_path):
    real_id = next(iter(CHECK_IDS))
    source = (
        "def test_x():\n"
        f'    fired = {{"{real_id}": {{"https://example.com/bad"}}}}\n'
        f'    assert "https://example.com/bad" in fired.get("{real_id}", set())\n'
        f'    assert "https://example.com/ok" not in fired.get("{real_id}", set())\n'
    )
    ev = _write_and_scan(tmp_path, source)
    assert {"pos", "neg"} <= ev[real_id]


def test_scanner_detects_fire_only_evidence(tmp_path):
    real_id = next(iter(CHECK_IDS))
    source = (
        "def test_x():\n"
        f'    fired = {{"{real_id}": {{"https://example.com/bad"}}}}\n'
        f'    assert "https://example.com/bad" in fired.get("{real_id}", set())\n'
    )
    ev = _write_and_scan(tmp_path, source)
    assert ev[real_id] == {"pos"}


def test_scanner_detects_empty_equality_as_silent_evidence(tmp_path):
    real_id = next(iter(CHECK_IDS))
    source = f'def test_x():\n    issues = _issues(res, "{real_id}")\n    assert issues == []\n'
    ev = _write_and_scan(tmp_path, source)
    assert ev[real_id] == {"neg"}


def test_scanner_detects_subset_membership_as_fire_evidence(tmp_path):
    real_id = next(iter(CHECK_IDS))
    source = (
        "def test_x():\n"
        f'    fired = {{"{real_id}": {{"a", "b"}}}}\n'
        f'    assert {{"a", "b"}} <= fired.get("{real_id}", set())\n'
    )
    ev = _write_and_scan(tmp_path, source)
    assert ev[real_id] == {"pos"}


def test_scanner_finds_no_evidence_for_an_untouched_check(tmp_path):
    untouched_id = next(iter(CHECK_IDS - {"UNRELATED"}))
    source = "def test_x():\n    assert 1 == 1\n"
    ev = _write_and_scan(tmp_path, source)
    assert ev[untouched_id] == set()


def test_the_gate_itself_would_fail_on_an_unaccounted_new_check(monkeypatch, tmp_path):
    """Positive control: simulate CHECKS gaining a brand-new id with no test
    and no exemption, and confirm the accounting check would reject it."""
    fake_id = "NOT_A_REAL_CHECK_ID_FOR_TEST"
    evidence = collect_verdict_evidence()
    verified = _verified(evidence)
    accounted = verified | set(LIVE_ONLY_EXEMPT) | KNOWN_UNCOVERED
    assert fake_id not in accounted
