#!/usr/bin/env python3
"""Copy license files from the exact Python build environment into a bundle."""

from __future__ import annotations

import argparse
import importlib.metadata
import shutil
from pathlib import Path


def license_file(distribution_name: str) -> Path:
    distribution = importlib.metadata.distribution(distribution_name)
    candidates = [
        item
        for item in distribution.files or ()
        if item.name.lower() in {"license", "license.txt", "copying.txt"}
    ]
    if not candidates:
        raise ValueError(f"{distribution_name} does not expose a license file")
    return Path(distribution.locate_file(candidates[0]))


def copy_runtime_notices(output: Path) -> None:
    output.mkdir(parents=True, exist_ok=True)
    for distribution, name in (
        ("PyQt5-Qt5", "Qt-LGPL-3.0.txt"),
        ("PyQt5-sip", "PyQt5-sip-LICENSE.txt"),
        ("pyinstaller", "PyInstaller-COPYING.txt"),
    ):
        shutil.copyfile(license_file(distribution), output / name)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    copy_runtime_notices(args.output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
