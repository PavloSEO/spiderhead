"""Thin shared-handler boundary for explicit provider evidence operations."""

from __future__ import annotations

from typing import Any

from seohead.data_sources.providers import (
    provider_collect as _collect,
)
from seohead.data_sources.providers import (
    provider_join as _join,
)
from seohead.data_sources.providers import (
    provider_readiness as _readiness,
)
from seohead.data_sources.providers import (
    provider_registry as _registry,
)
from seohead.data_sources.providers import (
    provider_verify as _verify,
)
from seohead.data_sources.providers import (
    sources_doctor as _doctor,
)


def provider_registry() -> dict[str, Any]:
    return _registry()


def provider_readiness(provider: str | None = None, operation: str | None = None) -> dict[str, Any]:
    """Inspect provider credential state and operation support without network access."""
    return _readiness(provider=provider, operation=operation)


def provider_doctor() -> dict[str, Any]:
    return _doctor()


def provider_verify(provider: str, request: dict[str, Any] | None = None) -> dict[str, Any]:
    """Explicit, bounded, read-only credential verification; it never mutates provider state."""
    return _verify(provider, request)


def provider_credential(
    provider: str,
    component: str,
    action: str = "status",
    confirm: bool = False,
    value: str | None = None,
    replace: bool = False,
) -> dict[str, Any]:
    """Inspect or explicitly revoke a registry-owned credential without reading its value."""
    from seohead.data_sources.providers import manage_credential

    if action == "set" and value is None:
        raise ValueError("credential writes require the local CLI stdin channel")
    if action != "set" and (value is not None or replace):
        raise ValueError("value and replace are valid only for local credential writes")
    return manage_credential(
        provider, component, action, value=value, replace=replace, confirm=confirm
    )


def provider_discover(provider: str, domain: str | None = None) -> dict[str, Any]:
    """Discover account-visible resources without selecting or binding them."""
    from seohead.data_sources.providers import provider_discover as core

    return core(provider, domain=domain)


def provider_collect(
    provider: str,
    operation: str,
    request: dict[str, Any],
    artifact_dir: str | None = None,
) -> dict[str, Any]:
    """Collect one declared read-only operation, with raw data retained only when requested locally."""
    return _collect(provider, operation, request, artifact_dir=artifact_dir)


def provider_join(
    crawl_pages: list[dict[str, Any]],
    evidence_rows: list[dict[str, Any]],
    review_external_only: bool = False,
    adjustments: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    return _join(
        crawl_pages,
        evidence_rows,
        review_external_only=review_external_only,
        adjustments=adjustments,
    )
