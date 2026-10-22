"""Read-only local environment diagnostics for the CLI and MCP clients."""

from __future__ import annotations

import importlib.metadata
import importlib.util
import os
import shutil
import sqlite3
import sys
from pathlib import Path
from typing import Any

DOCTOR_FORMAT = "seohead.doctor.v1"


def _optional_module(name: str) -> dict[str, Any]:
    try:
        spec = importlib.util.find_spec(name)
    except (ImportError, ValueError):
        spec = None
    if spec is None:
        return {"state": "unavailable", "version": None, "reason": f"{name} is not installed"}
    try:
        version = importlib.metadata.version(name)
    except (importlib.metadata.PackageNotFoundError, ValueError):
        version = None
    return {"state": "available", "version": version, "reason": None}


def _project_directory(path: str | None) -> dict[str, Any]:
    if path is None:
        return {
            "state": "not_checked",
            "readable": None,
            "writable": None,
            "reason": "pass a projects directory to inspect its access flags",
        }
    if not isinstance(path, str) or not path or len(path) > 4096:
        return {
            "state": "unavailable",
            "readable": None,
            "writable": None,
            "reason": "the selected projects directory path is invalid",
        }
    root = Path(path).expanduser()
    try:
        is_directory = root.is_dir()
    except OSError:
        is_directory = False
    if not is_directory:
        return {
            "state": "unavailable",
            "readable": None,
            "writable": None,
            "reason": "the selected projects directory is missing or unreadable",
        }
    readable = os.access(root, os.R_OK | os.X_OK)
    writable = os.access(root, os.W_OK | os.X_OK)
    return {
        "state": "available" if readable else "unavailable",
        "readable": readable,
        "writable": writable,
        "reason": None if readable and writable else "directory access flags are incomplete",
    }


def doctor(
    *,
    projects_directory: str | None = None,
    config_path: str | None = None,
    sf_cli: str | None = None,
) -> dict[str, Any]:
    """Report local prerequisites without making requests or writing files.

    Directory write access is an OS permission check only; this diagnostic does
    not create a probe file and therefore cannot guarantee a later write.
    """
    from seohead.sf.config import load_config
    from seohead.sf.core.runner import find_sf_cli

    try:
        config = load_config(config_path or "config.json")
    except (OSError, ValueError):
        config = {}
        config_reason = "SF configuration could not be read"
    else:
        config_reason = None
    try:
        sf_path = find_sf_cli(config, sf_cli)
    except (OSError, TypeError, ValueError):
        sf_path = None
        config_reason = "SF CLI discovery configuration is invalid"
    browser = next(
        (
            name
            for candidate in ("chromium", "chromium-browser", "google-chrome", "chrome", "msedge")
            if (name := shutil.which(candidate))
        ),
        None,
    )
    try:
        from seohead.core.filesystem import require_locking

        require_locking()
        locking = {"state": "available", "reason": None}
    except OSError as exc:
        locking = {"state": "unavailable", "reason": type(exc).__name__}
    return {
        "format": DOCTOR_FORMAT,
        "python": {
            "state": "available",
            "version": ".".join(map(str, sys.version_info[:3])),
            "reason": None,
        },
        "playwright": _optional_module("playwright"),
        "chromium": {
            "state": "available" if browser else "unavailable",
            "executable": Path(browser).name if browser else None,
            "reason": (
                None
                if browser
                else "no supported Chromium executable is on PATH;"
                " rendered pages need it — install one (e.g."
                " `playwright install chromium`, or a Chrome/Chromium/Edge binary on PATH)"
            ),
        },
        "screaming_frog": {
            "state": "available" if sf_path else "unavailable",
            "executable": Path(sf_path).name if sf_path else None,
            "license": {
                "valid": None,
                "checked": None,
                "until": None,
                "reason": "license status is not established by executable discovery",
            },
            "reason": (None if sf_path else config_reason or "Screaming Frog CLI was not found"),
        },
        "mcp": _optional_module("mcp"),
        "sqlite": {
            "state": "available",
            "version": sqlite3.sqlite_version,
            "locking": locking,
            "reason": None,
        },
        "projects_directory": _project_directory(projects_directory),
    }
