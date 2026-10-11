#!/usr/bin/env python3
"""Add Material Symbols Outlined SVGs to assets/icons and pin them in asset-manifest.json.

Usage: fetch_icons.py NAME [NAME ...]   (network: raw.githubusercontent.com/google/material-design-icons)
Existing icons are never overwritten; unknown names are reported and skipped.
"""

from __future__ import annotations

import hashlib
import json
import sys
import urllib.error
import urllib.request
from pathlib import Path

ASSETS = Path(__file__).resolve().parents[1] / "src" / "seohead_desktop" / "assets"
URL = ("https://raw.githubusercontent.com/google/material-design-icons/master/symbols/web/"
       "{name}/materialsymbolsoutlined/{name}_24px.svg")


def main(names):
    manifest_path = ASSETS / "asset-manifest.json"
    manifest = json.loads(manifest_path.read_text())
    known = {entry["path"] for entry in manifest}
    missing = []
    for name in sorted(set(names)):
        target = ASSETS / "icons" / f"{name}.svg"
        if target.exists():
            continue
        url = URL.format(name=name)
        try:
            with urllib.request.urlopen(url, timeout=20) as response:
                data = response.read()
        except urllib.error.URLError as exc:
            missing.append((name, str(exc)))
            continue
        if not data.lstrip().startswith(b"<svg"):
            missing.append((name, "not an SVG"))
            continue
        target.write_bytes(data)
        if target.relative_to(ASSETS).as_posix() not in known:
            manifest.append({"path": f"icons/{name}.svg", "url": url, "sha256": hashlib.sha256(data).hexdigest()})
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    for name, reason in missing:
        print(f"skipped {name}: {reason}", file=sys.stderr)
    return 1 if missing else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
