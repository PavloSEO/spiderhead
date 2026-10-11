#!/usr/bin/env python3
"""Build unsigned app/pkg artifacts in desktop/dist without installing anything."""
from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile
from pathlib import Path


def run(*args):
    subprocess.run([str(arg) for arg in args], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core-source", type=Path, required=True)
    parser.add_argument("--python", default=sys.executable)
    parser.add_argument("--dmg", action="store_true")
    parser.add_argument("--output-dir", type=Path, help="new output directory under desktop/dist")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("macOS build tools are required")
    desktop = Path(__file__).resolve().parents[2]
    source = Path(__file__).resolve().parent
    dist = (args.output_dir or desktop / "dist").resolve()
    if not dist.is_relative_to((desktop / "dist").resolve()):
        parser.error("output must stay under desktop/dist")
    dist.mkdir(parents=True, exist_ok=True)
    bundle = dist / "SEOHEAD Desktop.app"
    package = dist / "SEOHEAD-Desktop.pkg"
    if bundle.exists() or package.exists():
        parser.error("refusing to overwrite an existing build")
    run("sh", desktop / "scripts/build_macos.sh", "--core-source", args.core_source.resolve(), "--output", bundle, "--python", args.python)
    with tempfile.TemporaryDirectory(prefix="pkg-", dir=desktop / ".build") as temporary:
        staging = Path(temporary)
        payload, scripts = staging / "payload", staging / "scripts"
        (payload / "Applications").mkdir(parents=True)
        (payload / "usr/local/bin").mkdir(parents=True)
        scripts.mkdir()
        run("ditto", bundle, payload / "Applications/SEOHEAD Desktop.app")
        for name, destination in (("seohead", payload / "usr/local/bin/seohead"), ("postinstall", scripts / "postinstall"), ("preinstall", scripts / "preinstall")):
            origin = desktop / "src/seohead_desktop/assets/app/seohead-cli.sh" if name == "seohead" else source / name
            destination.write_bytes(origin.read_bytes())
            destination.chmod(0o755)
        run("pkgbuild", "--root", payload, "--scripts", scripts, "--identifier", "tech.seohead.desktop", "--version", "0.1.0", "--install-location", "/", package)
    run("pkgutil", "--payload-files", package)
    if args.dmg:
        with tempfile.TemporaryDirectory(prefix="dmg-", dir=desktop / ".build") as temporary:
            staging = Path(temporary)
            run("ditto", bundle, staging / bundle.name)
            os.symlink("/Applications", staging / "Applications")
            run("hdiutil", "create", "-volname", "SEOHEAD Desktop", "-srcfolder", staging, "-format", "UDZO", dist / "SEOHEAD-Desktop.dmg")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
