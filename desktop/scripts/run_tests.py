#!/usr/bin/env python3
"""Run the desktop test-suite and leave without interpreter finalisation (PyQt crashes there with SIGSEGV).

Usage (from desktop/): python scripts/run_tests.py [unittest-discover arguments, e.g. -p "test_screen_*.py"]
"""

import os
import sys
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
sys.path[:0] = [str(ROOT), str(ROOT / "src")]

from seohead_desktop import qt  # noqa: E402


class IsolatedResult(unittest.TextTestResult):
    def stopTest(self, test):
        started = getattr(self, "_started_at", None)
        if started is not None:
            self.durations.append((time.perf_counter() - started, test.id()))
        super().stopTest(test)

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.durations = []

    def startTest(self, test):
        self._started_at = time.perf_counter()
        from seohead_desktop import i18n

        # Legacy translation fixtures must not inherit a preceding window's language.
        # MainWindow independently chooses English; explicit fixture languages still win.
        i18n.set_language("ru")
        super().startTest(test)


class _FlushingStream:
    """Flush progress dots as well as verbose lines for callers piping the runner."""

    def __init__(self, stream):
        self.stream = stream

    def write(self, text):
        self.stream.write(text)
        self.stream.flush()

    def flush(self):
        self.stream.flush()


def main(argv):
    options, verbosity, duration_count = _options(argv)
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(line_buffering=True, write_through=True)
    suite = unittest.defaultTestLoader.discover(str(ROOT / "tests"), **options)
    result = unittest.TextTestRunner(
        stream=_FlushingStream(sys.stdout),
        verbosity=verbosity,
        resultclass=IsolatedResult,
    ).run(suite)
    if duration_count:
        print(f"\nSlowest {min(duration_count, len(result.durations))} tests:", flush=True)
        for seconds, test_id in sorted(result.durations, reverse=True)[:duration_count]:
            print(f"{seconds:.2f}s {test_id}", flush=True)
    return 0 if result.wasSuccessful() else 1


def _options(argv):
    options = {}
    verbosity = 2 if "-v" in argv or "--verbose" in argv else 1
    duration_count = 0
    index = 0
    while index < len(argv):
        arg = argv[index]
        if arg == "-p":
            index += 1
            if index >= len(argv):
                raise SystemExit("-p requires a discovery pattern")
            options["pattern"] = argv[index]
        elif arg == "--durations":
            index += 1
            if index >= len(argv):
                raise SystemExit("--durations requires a count")
            try:
                duration_count = int(argv[index])
            except ValueError as exc:
                raise SystemExit("--durations requires a non-negative integer") from exc
            if duration_count < 0:
                raise SystemExit("--durations requires a non-negative integer")
        elif arg not in ("-v", "--verbose"):
            raise SystemExit(f"unknown option: {arg}")
        index += 1
    return options, verbosity, duration_count


if __name__ == "__main__":
    qt.exit_now(main(sys.argv[1:]))
