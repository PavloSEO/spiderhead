#!/usr/bin/env python3
"""Render the brandbook SVGs (assets/brand) into native icon containers with Qt, deterministically.

Brandbook: sizes 48 and up use the medium logo (brand/seohead-logo.svg); 16-32 px (incl. the .ico/.icns small frames and the
tray) use the spider (brand/seohead-spider.svg). app/seohead.svg and app/seohead-small.svg are copies of those two sources.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import struct
import subprocess
import tempfile
from pathlib import Path

from PyQt5.QtCore import Qt
from PyQt5.QtGui import QGuiApplication, QImage, QPainter
from PyQt5.QtSvg import QSvgRenderer

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "src" / "seohead_desktop" / "assets"
SIZES = (16, 24, 32, 48, 64, 128, 256, 512, 1024)
SMALL_MAX = 32


def source_for(size: int, source: Path, small: Path) -> Path:
    return small if size <= SMALL_MAX and small.exists() else source


def render(source: Path, output: Path, small: Path) -> list[Path]:
    renderers = {path: QSvgRenderer(str(path)) for path in {source_for(s, source, small) for s in SIZES}}
    if not all(renderer.isValid() for renderer in renderers.values()):
        raise ValueError("invalid SEOHEAD source SVG")
    output.mkdir(parents=True, exist_ok=True)
    paths = []
    for size in SIZES:
        canvas = QImage(size, size, QImage.Format_ARGB32_Premultiplied)
        canvas.fill(Qt.transparent)
        painter = QPainter(canvas)
        renderers[source_for(size, source, small)].render(painter)
        painter.end()
        path = output / f"seohead-{size}.png"
        if not canvas.save(str(path), "PNG"):
            raise OSError(f"could not write icon: {path}")
        paths.append(path)

    # ICO supports PNG payloads; keep every size rather than upscaling one bitmap.
    frames = [
        (size, (output / f"seohead-{size}.png").read_bytes())
        for size in SIZES
        if size <= 256
    ]
    offset = 6 + 16 * len(frames)
    directory = []
    for size, data in frames:
        directory.append(
            struct.pack(
                "<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset
            )
        )
        offset += len(data)
    ico = output / "seohead.ico"
    ico.write_bytes(
        struct.pack("<HHH", 0, 1, len(frames))
        + b"".join(directory)
        + b"".join(data for _, data in frames)
    )
    paths.append(ico)

    iconutil = shutil.which("iconutil")
    if iconutil:
        scratch = ROOT / ".build" / "scratch"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="icons-", dir=scratch) as temporary:
            iconset = Path(temporary) / "SEOHEAD.iconset"
            iconset.mkdir()
            for size in (16, 32, 128, 256, 512):
                for scale in (1, 2):
                    suffix = "@2x" if scale == 2 else ""
                    shutil.copyfile(
                        output / f"seohead-{size * scale}.png",
                        iconset / f"icon_{size}x{size}{suffix}.png",
                    )
            icns = output / "seohead.icns"
            subprocess.run(
                [iconutil, "--convert", "icns", "--output", str(icns), str(iconset)],
                check=True,
            )
            paths.append(icns)
    return paths


def derived_from(path: Path, source: Path, small: Path) -> str:
    if path.suffix == ".svg":
        return f"brand/{(small if path.stem.endswith('small') else source).name}"
    stem = path.stem.rsplit("-", 1)[-1]
    if stem.isdigit():
        return f"brand/{source_for(int(stem), source, small).name}"
    # ICO/ICNS mix both sources: the spider for the small frames, the logo above.
    return "brand/seohead-logo.svg + brand/seohead-spider.svg"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=ASSETS / "app")
    args = parser.parse_args()
    app = QGuiApplication.instance() or QGuiApplication([])
    app.setApplicationName("SEOHEAD icon export")
    source, small = ASSETS / "brand" / "seohead-logo.svg", ASSETS / "brand" / "seohead-spider.svg"
    shutil.copyfile(source, ASSETS / "app" / "seohead.svg")
    shutil.copyfile(small, ASSETS / "app" / "seohead-small.svg")
    paths = render(source, args.output.resolve(), small)
    paths += [ASSETS / "app" / "seohead.svg", ASSETS / "app" / "seohead-small.svg"]  # copies of the brandbook sources
    if args.output.resolve() == (ASSETS / "app").resolve():
        manifest_path = ASSETS / "asset-manifest.json"
        entries = json.loads(manifest_path.read_text(encoding="utf-8"))
        updated_paths = {path.relative_to(ASSETS).as_posix() for path in paths}
        entries = [entry for entry in entries if entry["path"] not in updated_paths]
        entries.extend(
            {
                "path": path.relative_to(ASSETS).as_posix(),
                "derived_from": derived_from(path, source, small),
                "generator": "scripts/render_app_icon.py (Qt SVG; iconutil for ICNS)",
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            }
            for path in paths
        )
        manifest_path.write_text(json.dumps(entries, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"icons": [path.name for path in paths]}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
