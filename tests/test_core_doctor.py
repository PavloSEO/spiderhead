"""Read-only local diagnostics contract for issue #979."""

from __future__ import annotations

import json

from seohead.core import doctor as doctor_module


def test_doctor_reports_unknown_license_without_guessing_and_avoids_local_paths(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(
        doctor_module,
        "_optional_module",
        lambda name: {
            "state": "available",
            "version": "1.0",
            "reason": None,
        },
    )
    monkeypatch.setattr("seohead.sf.config.load_config", lambda _path: {})
    monkeypatch.setattr("seohead.sf.core.runner.find_sf_cli", lambda _config, _override: None)

    result = doctor_module.doctor(projects_directory=str(tmp_path))

    assert result["format"] == "seohead.doctor.v1"
    assert result["python"]["state"] == "available"
    assert result["screaming_frog"]["license"]["valid"] is None
    assert result["screaming_frog"]["license"]["checked"] is None
    assert result["screaming_frog"]["license"]["until"] is None
    assert result["screaming_frog"]["license"]["reason"]
    assert result["projects_directory"]["readable"] is True
    assert result["projects_directory"]["writable"] is True
    serialized = json.dumps(result)
    assert str(tmp_path) not in serialized


def test_doctor_leaves_unprovided_projects_directory_unmeasured(monkeypatch):
    monkeypatch.setattr("seohead.sf.config.load_config", lambda _path: {})
    monkeypatch.setattr("seohead.sf.core.runner.find_sf_cli", lambda _config, _override: None)

    result = doctor_module.doctor()

    assert result["projects_directory"] == {
        "state": "not_checked",
        "readable": None,
        "writable": None,
        "reason": "pass a projects directory to inspect its access flags",
    }
