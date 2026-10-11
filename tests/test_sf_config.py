"""sf/config.py: profile override wins even when config.json pins a profile."""

from __future__ import annotations

import json

import pytest

from seohead.sf.config import ConfigError, apply_profile, load_config, validate_config


def test_load_config_does_not_preexpand_profile(tmp_path):
    p = tmp_path / "config.json"
    with p.open("w", encoding="utf-8") as stream:
        json.dump({"profile": "lite"}, stream)
    cfg = load_config(str(p))
    # profile recorded, but exports NOT yet collapsed to lite (so an override can win)
    assert cfg["profile"] == "lite"
    assert "Sitemaps:Orphan URLs" in cfg["exports"]["tabs"]
    # override to full -> full export set; collapse to lite -> orphan tab gone
    cfg["profile"] = "full"
    assert "Sitemaps:Orphan URLs" in apply_profile(cfg)["exports"]["tabs"]
    cfg["profile"] = "lite"
    assert "Sitemaps:Orphan URLs" not in apply_profile(cfg)["exports"]["tabs"]


# ── validate_config (issue #211) ───────────────────────────────────────────
#
# An invalid severity_overrides value silently dropped its issues out of
# by_severity and the weighted penalty, inflating the health score exactly
# when a check was supposed to be hurting it, and made the emitted audit.json
# fail its own bundled schema. validate_config is the boundary that must
# catch this before a single check runs.


def test_default_config_validates_clean():
    validate_config(load_config(None))  # must not raise


@pytest.mark.parametrize(
    "name,value",
    [
        ("threshold", float("nan")),
        ("threshold", 0),
        ("threshold", True),
        ("relevance_threshold", 1.1),
        ("max_documents", 10001),
        ("max_documents", True),
        ("max_candidate_comparisons", -1),
        ("max_candidate_comparisons", 250001),
        ("adapter", "implicit-model"),
        ("embeddings", {}),
        ("cache_path", []),
    ],
)
def test_semantic_configuration_rejects_invalid_thresholds_and_budgets(name, value):
    cfg = load_config(None)
    cfg["semantic_similarity"][name] = value
    with pytest.raises(ConfigError, match=f"semantic_similarity.{name}"):
        validate_config(cfg)


def test_semantic_configuration_requires_object():
    cfg = load_config(None)
    cfg["semantic_similarity"] = []
    with pytest.raises(ConfigError, match="semantic_similarity must be an object"):
        validate_config(cfg)


def test_valid_severity_override_validates_clean():
    cfg = load_config(None)
    cfg["severity_overrides"] = {"TITLE_MISSING": "notice"}
    validate_config(cfg)  # must not raise


def test_invalid_severity_override_is_rejected():
    cfg = load_config(None)
    cfg["severity_overrides"] = {"TITLE_MISSING": "urgent"}
    with pytest.raises(ConfigError, match="TITLE_MISSING"):
        validate_config(cfg)


def test_invalid_per_check_severity_is_rejected():
    cfg = load_config(None)
    cfg["checks"] = {"TITLE_MISSING": {"severity": "urgent"}}
    with pytest.raises(ConfigError, match="TITLE_MISSING"):
        validate_config(cfg)


def test_unknown_check_id_in_severity_overrides_is_rejected():
    cfg = load_config(None)
    cfg["severity_overrides"] = {"NOT_A_REAL_CHECK": "critical"}
    with pytest.raises(ConfigError, match="NOT_A_REAL_CHECK"):
        validate_config(cfg)


def test_unknown_check_id_in_checks_is_rejected():
    cfg = load_config(None)
    cfg["checks"] = {"NOT_A_REAL_CHECK": {"enabled": False}}
    with pytest.raises(ConfigError, match="NOT_A_REAL_CHECK"):
        validate_config(cfg)


@pytest.mark.parametrize("weight", [float("nan"), float("inf"), -1, "high", True])
def test_invalid_scoring_weight_is_rejected(weight):
    cfg = load_config(None)
    cfg["scoring"]["weights"] = {"critical": weight}
    with pytest.raises(ConfigError, match="weights"):
        validate_config(cfg)


# ── validate_config: checks.<ID>.enabled must be a real bool (issue #463) ──
#
# AuditContext.enabled() reads checks[<ID>]["enabled"] with no type coercion,
# so a JSON string like "false" is truthy in Python and the check stays on
# despite the config explicitly asking to turn it off. validate_config must
# catch this the same way it already catches a bad severity.


def test_string_enabled_value_is_rejected():
    cfg = load_config(None)
    cfg["checks"] = {"TITLE_MISSING": {"enabled": "false"}}
    with pytest.raises(ConfigError, match="TITLE_MISSING"):
        validate_config(cfg)


def test_real_bool_enabled_value_validates_clean():
    cfg = load_config(None)
    cfg["checks"] = {"TITLE_MISSING": {"enabled": False}}
    validate_config(cfg)  # must not raise


def test_well_formed_checks_override_still_validates_clean():
    """Negative control: valid enabled/severity together must not add errors."""
    cfg = load_config(None)
    cfg["checks"] = {"TITLE_MISSING": {"enabled": True, "severity": "notice"}}
    validate_config(cfg)  # must not raise
