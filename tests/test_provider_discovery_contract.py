"""Offline resource discovery previews for source accounts (#990)."""

from __future__ import annotations

import pytest

from seohead.data_sources import providers


def test_google_discovery_returns_unselected_project_match_hints(monkeypatch):
    monkeypatch.setattr(
        "seohead.data_sources.gsc.discover_properties",
        lambda transport=None: {
            "ok": True,
            "properties": [
                {"site_url": "sc-domain:example.test", "permission_level": "siteOwner"},
                {"site_url": "https://other.test/", "permission_level": "siteRestrictedUser"},
            ],
        },
    )

    result = providers.provider_discover("google", domain="example.test")

    assert result["count"] == 2
    assert result["resources"][0]["domain_match"] is True
    assert result["resources"][1]["domain_match"] is False
    assert result["selected"] is False
    assert all("permission_level" not in row for row in result["resources"])


def test_discovery_refuses_unknown_provider_without_calling_a_provider():
    with pytest.raises(ValueError, match="google, yandex, or topvisor"):
        providers.provider_discover("telegram")


def test_topvisor_discovery_keeps_pagination_visible_and_unselected(monkeypatch):
    monkeypatch.setattr(
        "seohead.data_sources.topvisor.fetch",
        lambda operation, params: {
            "ok": True,
            "operation": operation,
            "limit": params["limit"],
            "offset": params["offset"],
            "nextOffset": 100,
            "result": [{"id": 42, "name": "Example account"}],
        },
    )

    result = providers.provider_discover("topvisor")

    assert result["resources"] == [
        {"service": "topvisor", "resource": "42", "label": "Example account"}
    ]
    assert result["truncated"] is True
    assert result["selected"] is False
