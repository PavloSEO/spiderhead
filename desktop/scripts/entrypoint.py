"""Packager entrypoint; application logic stays inside the package."""

from __future__ import annotations

import sys

from seohead_desktop.app import main
from seohead_desktop.bundle import package_arguments

if __name__ == "__main__":
    sys.argv[1:] = package_arguments(sys.argv[1:])
    raise SystemExit(main())
