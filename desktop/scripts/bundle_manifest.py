#!/usr/bin/env python3
"""Create and finalize the provenance manifest for a co-shipped core bundle."""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

try:  # pragma: no cover - Python 3.11+ takes the first branch.
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - exercised by the Python 3.10 CI job.
    import tomli as tomllib


def _run(*args: str, cwd: Path) -> str:
    return subprocess.run(
        args,
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def _core_identity(source: Path) -> dict[str, str]:
    root = Path(_run("git", "rev-parse", "--show-toplevel", cwd=source))
    if _run("git", "status", "--porcelain", cwd=root):
        raise ValueError("core source must be a clean Git checkout")
    pyproject = tomllib.loads((root / "pyproject.toml").read_text(encoding="utf-8"))
    project = pyproject.get("project", {})
    if project.get("name") != "seohead-seotools":
        raise ValueError("core source is not the SEOHEAD toolkit")
    remote = _run("git", "config", "--get", "remote.origin.url", cwd=root)
    if "@" in remote:
        raise ValueError("core remote must not contain embedded credentials")
    archive = subprocess.run(
        ("git", "archive", "--format=tar", "HEAD"),
        cwd=root,
        check=True,
        capture_output=True,
    ).stdout
    return {
        "distribution": project["name"],
        "version": project["version"],
        "repository": remote,
        "commit": _run("git", "rev-parse", "HEAD", cwd=root),
        "source_archive_sha256": hashlib.sha256(archive).hexdigest(),
        "optional_dependency_declarations": project.get("optional-dependencies", {}),
    }


def desktop_identity(source: Path, *, require_clean: bool = True) -> dict:
    """Record the exact tracked/source bytes, including a local preview diff."""
    root = Path(_run("git", "rev-parse", "--show-toplevel", cwd=source))
    dirty = bool(_run("git", "status", "--porcelain", cwd=root))
    if dirty and require_clean:
        raise ValueError("desktop release source must be a clean Git checkout")
    names = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=root
    ).decode("utf-8").split("\0")
    entries = []
    for name in sorted(set(filter(None, names))):
        path = root / name
        if path.is_symlink():
            raise ValueError("desktop source must not contain symlinks")
        if path.is_file():
            entries.append({"path": name, "sha256": _sha256_file(path)})
    identity = json.dumps(entries, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return {
        "distribution": "seohead-desktop",
        "commit": _run("git", "rev-parse", "HEAD", cwd=root),
        "dirty": dirty,
        "source_tree_sha256": hashlib.sha256(identity).hexdigest(),
        "inventory": entries,
    }


def verify_import_sources(core_source: Path, desktop_source: Path) -> None:
    """PyInstaller collection must use the source checkouts named by the manifest."""
    expected = {
        "seohead": core_source / "seohead" / "__init__.py",
        "seohead_desktop": desktop_source / "src" / "seohead_desktop" / "__init__.py",
    }
    for package, path in expected.items():
        spec = importlib.util.find_spec(package)
        if spec is None or spec.origin is None or Path(spec.origin).resolve() != path.resolve():
            raise ValueError(f"build environment imports {package} from a different source; install the selected checkout into this environment")


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _inventory(root: Path) -> list[dict]:
    entries = []
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            target = path.resolve(strict=True)
            target.relative_to(root.resolve())
            entries.append({"path": relative, "kind": "symlink", "target": os.readlink(path)})
        elif path.is_file():
            entries.append({"path": relative, "kind": "file", "sha256": _sha256_file(path)})
        elif not path.is_dir():
            raise ValueError(f"unsupported core payload entry: {relative}")
    return entries


def create_manifest(
    core_source: Path, output: Path, platform: str,
    desktop_source: Path | None = None, *, preview: bool = False,
) -> dict:
    payload = {
        "schema": "seohead.desktop.core-manifest.v1",
        "platform": platform,
        "core": _core_identity(core_source.resolve()),
        "build_environment": {
            "python": sys.version,
            "installed_distributions": dict(
                sorted(
                    (
                        distribution.metadata["Name"].lower(),
                        distribution.version,
                    )
                    for distribution in importlib.metadata.distributions()
                    if distribution.metadata.get("Name")
                )
            ),
        },
    }
    if desktop_source is not None:
        payload["desktop"] = desktop_identity(desktop_source, require_clean=not preview)
        payload["build_mode"] = "developer-preview" if preview else "release"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return payload


def _payload(root: Path, executable: Path, manifest_root: Path) -> dict:
    return {
        "executable_relpath": executable.resolve().relative_to(manifest_root.resolve()).as_posix(),
        "executable_sha256": _sha256_file(executable),
        "root_relpath": root.resolve().relative_to(manifest_root.resolve()).as_posix(),
        "inventory": _inventory(root),
    }


def finalize_manifest(manifest: Path, cli: Path, agent: Path | None = None) -> dict:
    payload = json.loads(manifest.read_text(encoding="utf-8"))
    core = _payload(cli.parent, cli, manifest.parent)
    payload["core"].update(
        cli_relpath=core["executable_relpath"], cli_sha256=core["executable_sha256"],
        root_relpath=core["root_relpath"], inventory=core["inventory"],
    )
    if agent is not None:
        payload["agent"] = _payload(agent.parent, agent, manifest.parent)
    manifest.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return payload


def verify_sources(manifest: Path, core_source: Path, desktop_source: Path) -> None:
    """A source edit during freezing invalidates the artifact instead of relabeling it."""
    payload = json.loads(manifest.read_text(encoding="utf-8"))
    if payload["core"] != _core_identity(core_source):
        raise ValueError("core source changed during the build")
    if payload.get("desktop") != desktop_identity(
        desktop_source, require_clean=payload.get("build_mode") != "developer-preview"
    ):
        raise ValueError("desktop source changed during the build")


def cache_key(manifest: Path, component: str) -> str:
    payload = json.loads(manifest.read_text(encoding="utf-8"))
    inputs = {"platform": payload["platform"], "environment": payload["build_environment"]}
    inputs["source"] = payload["core"] if component == "core" else payload["desktop"]
    # A build-contract change invalidates every component, including core caches.
    inputs["packaging"] = [entry for entry in payload["desktop"]["inventory"] if entry["path"].startswith(("scripts/", "packaging/"))]
    return hashlib.sha256(json.dumps(inputs, sort_keys=True).encode("utf-8")).hexdigest()[:24]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create")
    create.add_argument("--core-source", type=Path, required=True)
    create.add_argument("--output", type=Path, required=True)
    create.add_argument("--platform", choices=("macos", "linux", "windows"), required=True)
    create.add_argument("--desktop-source", type=Path)
    create.add_argument("--preview", action="store_true")
    create.add_argument("--verify-imports", action="store_true")
    verify = commands.add_parser("verify-sources")
    verify.add_argument("--manifest", type=Path, required=True)
    verify.add_argument("--core-source", type=Path, required=True)
    verify.add_argument("--desktop-source", type=Path, required=True)
    cache = commands.add_parser("cache-key")
    cache.add_argument("--manifest", type=Path, required=True)
    cache.add_argument("--component", choices=("core", "agent", "app"), required=True)
    finalize = commands.add_parser("finalize")
    finalize.add_argument("--manifest", type=Path, required=True)
    finalize.add_argument("--cli", type=Path, required=True)
    finalize.add_argument("--agent", type=Path)
    args = parser.parse_args()
    if args.command == "create":
        if args.verify_imports:
            if args.desktop_source is None:
                parser.error("--verify-imports requires --desktop-source")
            verify_import_sources(args.core_source, args.desktop_source)
        create_manifest(args.core_source, args.output, args.platform, args.desktop_source, preview=args.preview)
    elif args.command == "verify-sources":
        verify_import_sources(args.core_source, args.desktop_source)
        verify_sources(args.manifest, args.core_source, args.desktop_source)
    elif args.command == "cache-key":
        print(cache_key(args.manifest, args.component))
    else:
        finalize_manifest(args.manifest, args.cli, args.agent)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
