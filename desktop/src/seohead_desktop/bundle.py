"""Locate the bundled, version-pinned SEOHEAD core without absolute paths."""

from __future__ import annotations

import hashlib
import json
import os
import re
import sys
from collections.abc import Iterable
from pathlib import Path

_HEX = re.compile(r"^[0-9a-f]{64}$")
_COMMIT = re.compile(r"^[0-9a-f]{40}$")
_IDENTITY_CACHE: dict[tuple[str, int, int], dict | None] = {}


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _tree_entries(root: Path) -> list[dict] | None:
    entries = []
    try:
        for path in sorted(root.rglob("*")):
            relative = path.relative_to(root).as_posix()
            if path.is_symlink():
                target = path.resolve(strict=True)
                target.relative_to(root.resolve())
                entries.append({"path": relative, "kind": "symlink", "target": os.readlink(path)})
            elif path.is_file():
                entries.append({"path": relative, "kind": "file", "sha256": _sha256_file(path)})
            elif not path.is_dir():
                return None
    except (OSError, ValueError):
        return None
    return entries


def _resource_roots(executable: Path | None = None) -> Iterable[Path]:
    """Yield possible PyInstaller resource locations in deterministic order."""
    executable = executable or Path(sys.executable).resolve()
    meipass = getattr(sys, "_MEIPASS", None)
    if meipass:
        yield Path(meipass)
    yield executable.parent / "Resources"
    yield executable.parent.parent / "Resources"
    yield executable.parent / "resources"


def bundled_core_manifest(executable: Path | None = None) -> tuple[Path, dict] | None:
    """Return the manifest next to a frozen desktop bundle, if present and valid."""
    for root in _resource_roots(executable):
        manifest = root / "core-manifest.json"
        if not manifest.is_file():
            continue
        try:
            payload = json.loads(manifest.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if payload.get("schema") != "seohead.desktop.core-manifest.v1":
            continue
        if not isinstance(payload.get("core"), dict):
            continue
        return manifest, payload
    return None


def bundled_core_cli(executable: Path | None = None) -> Path | None:
    """Return a bundled CLI only when its manifest identity verifies."""
    identity = verified_bundled_core_identity(executable)
    return None if identity is None else identity["cli"]


def verified_bundled_core_identity(executable: Path | None = None) -> dict | None:
    """Verify the co-shipped core path, recorded commit, and executable digest."""
    located = bundled_core_manifest(executable)
    if located is None:
        return None
    manifest_path, payload = located
    relative = payload["core"].get("cli_relpath")
    if not isinstance(relative, str) or not relative:
        return None
    candidate = (manifest_path.parent / relative).resolve()
    try:
        candidate.relative_to(manifest_path.parent.resolve())
    except ValueError:
        return None
    core_root = payload["core"].get("root_relpath")
    inventory = payload["core"].get("inventory")
    commit = payload["core"].get("commit")
    expected_hash = payload["core"].get("cli_sha256")
    if not candidate.is_file() or not isinstance(commit, str) or not _COMMIT.fullmatch(commit):
        return None
    if not isinstance(expected_hash, str) or not _HEX.fullmatch(expected_hash):
        return None
    if not isinstance(core_root, str) or not isinstance(inventory, list):
        return None
    root = (manifest_path.parent / core_root).resolve()
    try:
        root.relative_to(manifest_path.parent.resolve())
    except ValueError:
        return None
    if not root.is_dir() or candidate.parent != root:
        return None
    cache_key = (str(manifest_path), manifest_path.stat().st_mtime_ns, root.stat().st_mtime_ns)
    if cache_key in _IDENTITY_CACHE:
        return _IDENTITY_CACHE[cache_key]
    if _sha256_file(candidate) != expected_hash or _tree_entries(root) != inventory:
        _IDENTITY_CACHE[cache_key] = None
        return None
    identity = {"cli": candidate, "commit": commit, "manifest": manifest_path, "payload": payload}
    _IDENTITY_CACHE[cache_key] = identity
    return identity


def package_arguments(arguments: list[str], core_cli: Path | None = None) -> list[str]:
    """Add the bundled core only when callers did not choose another executable."""
    if "--core-cli" in arguments:
        return arguments
    core_cli = bundled_core_cli() if core_cli is None else core_cli
    return arguments if core_cli is None else ["--core-cli", str(core_cli), *arguments]
