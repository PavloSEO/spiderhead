"""Offline tests for provider/component slot resolution (issue #965, slice 2)."""

from __future__ import annotations

import pytest

from seohead.data_sources import providers

SECRET = "sk-test-0123456789-do-not-leak"


def test_resolves_stored_component_to_relative_reference(monkeypatch):
    monkeypatch.delenv("ARSENKIN_TOKEN", raising=False)

    slot = providers.credential_slot("arsenkin", "api_token")

    assert slot == {
        "provider": "arsenkin",
        "component": "api_token",
        "source_reference": "config:arsenkin/token",
        "env_reference": "env:ARSENKIN_TOKEN",
        "env_shadowed": False,
    }


def test_env_override_is_reported_without_its_value(monkeypatch):
    monkeypatch.setenv("ARSENKIN_TOKEN", SECRET)

    slot = providers.credential_slot("arsenkin", "api_token")

    assert slot["env_shadowed"] is True
    assert SECRET not in repr(slot)


def test_blank_env_value_does_not_shadow(monkeypatch):
    monkeypatch.setenv("ARSENKIN_TOKEN", "   ")

    assert providers.credential_slot("arsenkin", "api_token")["env_shadowed"] is False


@pytest.mark.parametrize(
    ("provider", "component"),
    [
        ("no_such_provider", "api_token"),
        ("arsenkin", "password"),
        ("gsc", "service_account"),
        ("wayback", "api_key"),
        ("dataforseo_backlinks", "api_token"),
    ],
)
def test_rejects_unknown_provider_or_non_secret_component(provider, component):
    with pytest.raises(ValueError) as excinfo:
        providers.credential_slot(provider, component)

    assert SECRET not in str(excinfo.value)


def test_multi_component_provider_resolves_each_component():
    login = providers.credential_slot("dataforseo_backlinks", "login")
    password = providers.credential_slot("dataforseo_backlinks", "password")

    assert login["source_reference"] == "config:dataforseo/login"
    assert password["source_reference"] == "config:dataforseo/password"


def test_moz_api_token_component_is_in_the_registry():
    slot = providers.credential_slot("moz", "api_token")

    assert slot["source_reference"] == "config:moz/api_token"
    assert slot["env_reference"] == "env:MOZ_API_TOKEN"
