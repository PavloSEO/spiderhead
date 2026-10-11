#!/usr/bin/env python3
"""Validate a packaged bundle without opening a target, project, or network connection."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
from pathlib import Path


def resource_directory(bundle: Path) -> Path:
    mac_resources = bundle / "Contents" / "Resources"
    return mac_resources if mac_resources.is_dir() else bundle / "resources"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def inventory(root: Path) -> list[dict]:
    entries = []
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            target = path.resolve(strict=True)
            target.relative_to(root.resolve())
            entries.append({"path": relative, "kind": "symlink", "target": os.readlink(path)})
        elif path.is_file():
            entries.append({"path": relative, "kind": "file", "sha256": sha256_file(path)})
        elif not path.is_dir():
            raise ValueError(f"unsupported core payload entry: {relative}")
    return entries


def check_bundle(bundle: Path) -> dict:
    resources = resource_directory(bundle)
    manifest_path = resources / "core-manifest.json"
    if not manifest_path.is_file():
        raise ValueError("missing core-manifest.json")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if manifest.get("schema") != "seohead.desktop.core-manifest.v1":
        raise ValueError("unsupported core manifest schema")
    core = manifest.get("core")
    if not isinstance(core, dict):
        raise ValueError("core manifest has no core section")
    relative = core.get("cli_relpath")
    if not isinstance(relative, str):
        raise ValueError("core manifest has no CLI path")
    executable = (resources / relative).resolve()
    executable.relative_to(resources.resolve())
    if not executable.is_file():
        raise ValueError("bundled core executable is missing")
    expected_hash = core.get("cli_sha256")
    actual_hash = sha256_file(executable)
    if actual_hash != expected_hash:
        raise ValueError("bundled core executable hash does not match the manifest")
    root_relative = core.get("root_relpath")
    root = (resources / root_relative).resolve() if isinstance(root_relative, str) else None
    if root is None or not root.is_dir() or root != executable.parent:
        raise ValueError("bundled core root is invalid")
    if inventory(root) != core.get("inventory"):
        raise ValueError("bundled core payload does not match the manifest")
    agent = manifest.get("agent")
    if not isinstance(agent, dict):
        raise ValueError("missing bundled Desktop control agent")
    agent_executable = (resources / str(agent.get("executable_relpath", ""))).resolve()
    agent_root = (resources / str(agent.get("root_relpath", ""))).resolve()
    agent_executable.relative_to(resources.resolve())
    agent_root.relative_to(resources.resolve())
    if (
        not agent_executable.is_file()
        or agent_executable.parent != agent_root
        or sha256_file(agent_executable) != agent.get("executable_sha256")
        or inventory(agent_root) != agent.get("inventory")
    ):
        raise ValueError("bundled Desktop control agent does not match the manifest")
    response = subprocess.run(
        [str(executable), "--version"],
        check=True,
        capture_output=True,
        text=True,
        timeout=15,
    )
    agent_help = subprocess.run(
        [str(agent_executable), "--help"],
        check=True,
        capture_output=True,
        text=True,
        timeout=15,
    )
    return {
        "core_version_output": response.stdout.strip(),
        "core_commit": core.get("commit"),
        "core_source_archive_sha256": core.get("source_archive_sha256"),
        "agent_help": agent_help.stdout.splitlines()[0] if agent_help.stdout else "",
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", required=True, type=Path)
    args = parser.parse_args()
    print(json.dumps(check_bundle(args.bundle), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
