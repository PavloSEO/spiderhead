"""Machine-readable core identity for `seohead version --json` (issue #979).

Reads only constants and the packaged provenance manifest. No network, no
paths, no config values: the output is safe to show in a UI or paste in a bug
report.
"""

from __future__ import annotations

import re
from collections.abc import Sequence
from typing import Any, get_args

from seohead import __version__
from seohead.core.job_contracts import ScanSource

CORE_INFO_FORMAT = "seohead.core-info.v1"


def _revision() -> str | None:
    """Git revision of a clean build, or None for a dev tree without a manifest."""
    from seohead._build.provenance import packaged_provenance

    try:
        return packaged_provenance().revision
    except (OSError, ValueError):
        return None


def _build_metadata() -> dict[str, Any]:
    from seohead._build.provenance import packaged_provenance

    try:
        provenance = packaged_provenance()
    except (OSError, ValueError):
        return {
            "state": "unavailable",
            "revision": None,
            "built_at": None,
            "reason": "verified package build provenance is unavailable",
        }
    return {
        "state": "verified",
        "revision": provenance.revision,
        "built_at": provenance.built_at,
        "reason": (
            None
            if provenance.built_at is not None
            else "build time is unavailable in this older package manifest"
        ),
    }


def core_info(
    commands: Sequence[str],
    mcp_tools: Sequence[str] = (),
    required_core_revision: str | None = None,
) -> dict[str, Any]:
    if required_core_revision is not None and (
        not isinstance(required_core_revision, str)
        or not re.fullmatch(r"[0-9a-f]{40}", required_core_revision)
    ):
        raise ValueError("required_core_revision must be a full lowercase Git SHA")
    from seohead.projects.workspace import PROJECT_FORMAT, PROJECT_VERSION
    from seohead.storage.ledger import FORMAT_VERSION, USER_VERSION

    build = _build_metadata()
    compatible = (
        None
        if required_core_revision is None or build["revision"] is None
        else required_core_revision == build["revision"]
    )
    return {
        "format": CORE_INFO_FORMAT,
        "contract_version": CORE_INFO_FORMAT,
        "package_version": __version__,
        "revision": build["revision"],
        "build": build,
        "supported_tools": {"cli": list(commands), "mcp": list(mcp_tools)},
        "desktop_compatibility": {
            "policy": "exact_core_revision",
            "required_core_revision": required_core_revision,
            "compatible": compatible,
            "reason": (
                None
                if compatible is not None
                else (
                    "Desktop must provide its pinned core revision and the installed build "
                    "must be verified"
                )
            ),
        },
        "project": {"format": PROJECT_FORMAT, "version": PROJECT_VERSION},
        "ledger": {"format_version": FORMAT_VERSION, "user_version": USER_VERSION},
        "scan_formats": list(get_args(ScanSource.model_fields["format_version"].annotation)),
        "commands": list(commands),
    }
