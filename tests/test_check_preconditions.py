"""Evidence that never arrived must be reported as skipped, never as clean.

Before this, a check whose export was absent simply did not fire, and a report
renders "did not fire" as "found nothing". The arithmetic made it worse: the
health score is 100 - penalty/pages, so fewer checks running produced a *higher*
score and a partial evidence source flattered the site it measured least.

The score is deliberately not rescaled by coverage — estimating what the checks
that never ran would have found is invention. Comparability is refused instead:
coverage is machine-readable, and below a floor no score is emitted at all.
"""

import json
import shutil
from pathlib import Path

import pytest

from seohead.sf.config import load_config
from seohead.sf.core.audit import run_audit
from seohead.sf.core.registry import CHECK_REQUIRES, CHECKS, missing_requirements

EXPORTS = Path("docs/examples/exports")


def _audit(tmp_path: Path, drop: list[str] | None = None, disable: list[str] | None = None) -> dict:
    """Run the analyzer over a copy of the example exports, minus some files."""
    work = tmp_path / "exports"
    shutil.copytree(EXPORTS, work)
    for pattern in drop or []:
        for path in work.glob(pattern):
            path.unlink()
    config = load_config(None)
    for check_id in disable or []:
        config.setdefault("checks", {})[check_id] = {"enabled": False}
    result = run_audit(input_mode="parse-exports", exports_dir=str(work), config=config)
    return json.loads(json.dumps(result.to_json()))


def test_declared_requirements_name_real_checks_and_frames():
    assert set(CHECK_REQUIRES) <= set(CHECKS)
    for frames in CHECK_REQUIRES.values():
        assert frames and all(isinstance(f, str) and f for f in frames)


def test_missing_requirements_reports_only_absent_frames():
    assert missing_requirements("IMG_MISSING_ALT", {"images_missing_alt"}) == ()
    assert missing_requirements("IMG_MISSING_ALT", set()) == ("images_missing_alt",)
    assert missing_requirements("NOT_A_CHECK", set()) == ()


def test_a_declared_dependency_is_skipped_with_a_readable_reason(tmp_path):
    reasons = {s["id"]: s["reason"] for s in _audit(tmp_path)["run"]["checks_skipped"]}
    assert "IMG_MISSING_ALT" in reasons
    assert "missing export" in reasons["IMG_MISSING_ALT"]
    assert "images_missing_alt" in reasons["IMG_MISSING_ALT"]


def test_silent_checks_are_named_so_the_gap_is_visible(tmp_path):
    """Checks that neither fired, nor were skipped, nor were disabled are the
    actual defect (issue #177) — and the population must be nameable, not just
    countable, or a reader has to diff CHECKS against fired/skipped themselves.
    """
    audit = _audit(tmp_path)
    coverage = audit["summary"]["check_coverage"]
    fired = {i["check"] for i in audit["issues"]}
    skipped = {s["id"] for s in audit["run"]["checks_skipped"]}
    disabled = {d["id"] for d in audit["run"]["checks_disabled"]}
    silent = set(coverage["checks_silent_ids"])

    # A property of the partition, not the complement's own definition: every
    # id in the silent list is independently absent from every other bucket,
    # checked against the raw fired/skipped/disabled records rather than by
    # recomputing the same "total minus everything else" formula the
    # production code uses.
    assert not (silent & fired)
    assert not (silent & skipped)
    assert not (silent & disabled)
    assert silent | fired | skipped | disabled == set(CHECKS)
    assert coverage["checks_silent"] == len(silent)

    # A ratchet: this may fall as declarations are added, never rise unnoticed.
    # Raised from 56 to 59 when NOTRANSLATE, UNAVAILABLE_AFTER and
    # CANONICAL_FRAGMENT were added (issue #30): the example fixture has no
    # page that trips them, so they run clean rather than declare a skip.
    # Raised from 59 to 60 when INLINK_BOILERPLATE_ONLY was added (issue #20):
    # like SITEMAP_ORPHAN and URL_NOT_IN_SITEMAP before it, it is added
    # directly from evidence a native crawl produces (crawl/linkgraph.py),
    # never through this SF-export fixture, so it has no export-based
    # dependency to declare and is silent here by the same construction.
    # Raised from 60 to 62 when the sitemap protocol-limit checks were added
    # (issue #124): they need a live sitemap parse rather than an export
    # frame, so on an export-only fixture they run clean, exactly as
    # SITEMAP_ORPHAN and URL_NOT_IN_SITEMAP already do.
    # Raised from 62 to 66 when the four security-header checks were added (issue #1013):
    # they read each page's stored response headers, which only a retained native crawl
    # carries, so this SF-export fixture runs them silent by the same construction as the
    # link-security checks above.
    # 62 -> 58, and the number is measured after the merge rather than
    # reconciled by hand: two changes moved it in opposite directions.
    # run_sitemap gained explicit skips for its network-only checks (#165),
    # which moves ids out of the silent bucket, while the six
    # link-security/forms checks (#125) read a native crawl's own
    # LinkEdge/FormEdge evidence this SF-export fixture never produces, which
    # moves ids into it.
    # 58 -> 59 when H2_TOO_LONG was added (#385): unlike the other nine checks
    # added alongside it, H2_TOO_LONG reads the same H2-1 column an SF export
    # already carries (no native-only dependency to declare a skip for), and
    # this fixture's own H2 values are all short, so it runs clean rather than
    # skip or fire. Its sibling H2_DUPLICATE, added in the same change, reads
    # the same column and happens to fire here instead (two fixture rows
    # already share an H2), which is why only one id moved, not two.
    # 59 -> 62 after the bounded native-only duplicate-id check, URL hygiene,
    # and literal placeholder-marker check joined the registry. The fixture
    # has no parser evidence for duplicate ids and no marker/session/slash
    # candidate, so these remain named silent checks rather than manufactured
    # findings.
    # 62 -> 66 after the batch-4 merge train added four checks that the export
    # fixture cannot trip by construction: INTERNAL_NOFOLLOW_OUTLINKS and
    # JS_CONSOLE_ERRORS need native-crawl evidence, NOODP and NOYDIR fire only
    # on a robots directive the fixture does not carry.
    # 62 -> 63 when HTML_OVER_2MB joined the registry: it needs a native HTML
    # body above 2 MB, which the SF-export fixture never carries, so it is
    # a named silent check rather than a manufactured finding.
    # 67 = 66 (batch-4) + 1 (HTML_OVER_2MB); +1 INTERNAL_LINK_SPONSORED_UGC; +1 URL_HAS_PARAMETERS;
    # +4 per-URL security-header checks (#1013) that stay named silent without a stored crawl.
    # +1 DUPLICATE_SECURITY_HEADER: the same per-URL evidence, silent without a stored crawl.
    # +1 CANONICAL_CROSS_ORIGIN (#1305): all fixture canonical authorities match.
    # Raw annotation attributes are absent from exports and must remain unmeasured.
    assert "CANONICAL_CROSS_ORIGIN" in silent
    assert "CANONICAL_INVALID_ATTRIBUTE" in skipped
    # URL_INTERNAL_SEARCH reads the retained URL column on this export fixture;
    # its ordinary non-search URLs prove one additional clean, invoked check.
    assert "URL_INTERNAL_SEARCH" in silent
    assert coverage["checks_silent"] <= 76


def test_a_disabled_check_is_its_own_bucket_never_silent_or_clean(tmp_path):
    """An operator's own config switch must be visible as disabled, not as a
    clean/silent result (issue #177) — regression for context.py's ``ctx.add``
    silently returning ``None`` for a disabled check with nothing recording it.
    """
    baseline = _audit(tmp_path / "on")
    disabled_run = _audit(tmp_path / "off", disable=["BROKEN_PAGE_4XX"])

    assert "BROKEN_PAGE_4XX" in {i["check"] for i in baseline["issues"]}
    assert "BROKEN_PAGE_4XX" not in {i["check"] for i in disabled_run["issues"]}

    coverage = disabled_run["summary"]["check_coverage"]
    assert coverage["checks_disabled_ids"] == ["BROKEN_PAGE_4XX"]
    assert coverage["checks_disabled"] == 1
    assert "BROKEN_PAGE_4XX" not in coverage["checks_silent_ids"]
    assert coverage["checks_fired"] == baseline["summary"]["check_coverage"]["checks_fired"] - 1

    disabled_reasons = {d["id"]: d["reason"] for d in disabled_run["run"]["checks_disabled"]}
    assert disabled_reasons["BROKEN_PAGE_4XX"]
    assert "BROKEN_PAGE_4XX" not in {s["id"] for s in disabled_run["run"]["checks_skipped"]}


def test_fired_and_skipped_for_one_id_cannot_both_be_counted(tmp_path, monkeypatch):
    """issue #136/#177: a check that fires from one source must not also be
    counted (or reported) as skipped just because ``ctx.skip`` was separately
    called for it — the two counts are derived from one partition, not two.
    """
    from seohead.sf.core import audit as audit_module

    real_run_rules = audit_module.run_rules

    def run_rules_then_skip_a_fired_check(ctx):
        real_run_rules(ctx)
        assert "BROKEN_PAGE_4XX" in {i.check for i in ctx.issues}, "fixture must fire this check"
        ctx.skip("BROKEN_PAGE_4XX", "evidence for another source was absent")

    monkeypatch.setattr(audit_module, "run_rules", run_rules_then_skip_a_fired_check)
    audit = _audit(tmp_path)

    coverage = audit["summary"]["check_coverage"]
    fired = {i["check"] for i in audit["issues"]}
    skipped = {s["id"] for s in audit["run"]["checks_skipped"]}
    assert "BROKEN_PAGE_4XX" in fired
    assert "BROKEN_PAGE_4XX" not in skipped
    assert (
        coverage["checks_fired"]
        + coverage["checks_skipped"]
        + coverage["checks_disabled"]
        + coverage["checks_silent"]
        == coverage["checks_total"]
    )


def test_removing_evidence_only_ever_grows_the_skip_set(tmp_path):
    baseline = _audit(tmp_path / "a")
    reduced = _audit(tmp_path / "b", drop=["*4xx*"])
    base_ids = {s["id"] for s in baseline["run"]["checks_skipped"]}
    less_ids = {s["id"] for s in reduced["run"]["checks_skipped"]}
    assert base_ids <= less_ids
    assert less_ids - base_ids, "removing an export must skip something new"
    assert (
        reduced["summary"]["check_coverage"]["coverage"]
        < baseline["summary"]["check_coverage"]["coverage"]
    )


def test_coverage_is_machine_readable_so_consumers_can_refuse_a_verdict(tmp_path):
    coverage = _audit(tmp_path)["summary"]["check_coverage"]
    assert set(coverage) == {
        "checks_total",
        "checks_fired",
        "checks_skipped",
        "checks_disabled",
        "checks_disabled_ids",
        "checks_silent",
        "checks_silent_ids",
        "coverage",
    }
    assert coverage["checks_total"] == len(CHECKS)
    assert coverage["checks_skipped"] > 0


def test_a_partial_run_states_that_its_score_is_not_comparable(tmp_path):
    summary = _audit(tmp_path)["summary"]
    assert "not comparable" in summary["health_score_basis"]


def test_too_little_evidence_suppresses_the_score_entirely(tmp_path, monkeypatch):
    """A source serving a fraction of the registry must not grade a site."""
    from seohead.sf.core import aggregate

    monkeypatch.setattr(aggregate, "MIN_COVERAGE_TO_SCORE", 0.99)
    summary = _audit(tmp_path)["summary"]
    assert summary["health_score"] is None
    assert "coverage" in summary["health_score_reason"]


def test_a_full_registry_run_keeps_its_score(tmp_path, monkeypatch):
    from seohead.sf.core import aggregate

    monkeypatch.setattr(aggregate, "MIN_COVERAGE_TO_SCORE", 0.0)
    assert isinstance(_audit(tmp_path)["summary"]["health_score"], int)


@pytest.mark.parametrize("check_id", sorted(CHECK_REQUIRES))
def test_every_declared_check_reports_its_frame_as_missing_when_absent(check_id):
    assert missing_requirements(check_id, set()) == CHECK_REQUIRES[check_id]
