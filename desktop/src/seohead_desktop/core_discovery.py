"""Find the seohead core CLI without depending on the launching shell's PATH."""

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path

from . import bundle

# Install locations that a Finder or Dock launch does not see on PATH.
COMMON_DIRECTORIES = ("~/.local/bin", "/opt/homebrew/bin", "/usr/local/bin")
SOURCE_VENV_CLI = Path(__file__).resolve().parents[3] / ".venv" / "bin" / "seohead"


def _usable(path):
    candidate = Path(path).expanduser()
    return str(candidate) if candidate.is_file() and os.access(candidate, os.X_OK) else None


def discover_core(prefs=None):
    """First usable core, or None. Order: the user's own path (when enabled), the verified bundled core,
    the desktop's own environment (its interpreter's directory, then sys.prefix), PATH, the source checkout's venv, then common install directories."""
    if prefs is not None and prefs.get("core.custom") and prefs.get("core.custom_path"):
        found = _usable(prefs.get("core.custom_path"))
        if found:
            return found
    bundled = bundle.bundled_core_cli()
    if bundled is not None and _usable(bundled):
        return str(bundled)
    for candidate in (Path(sys.executable).with_name("seohead"), Path(sys.prefix) / "bin" / "seohead"):
        found = _usable(candidate)
        if found:
            return found
    found = shutil.which("seohead") or _usable(SOURCE_VENV_CLI)
    if found:
        return found
    for directory in COMMON_DIRECTORIES:
        found = _usable(Path(directory).expanduser() / "seohead")
        if found:
            return found
    return None
