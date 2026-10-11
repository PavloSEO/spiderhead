"""The suite runner's discovery and progress options stay intentionally small."""

import importlib.util
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/run_tests.py"
SPEC = importlib.util.spec_from_file_location("desktop_test_runner", SCRIPT)
RUNNER = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(RUNNER)


class RunnerOptionsTests(unittest.TestCase):
    def test_verbose_discovery_and_duration_options(self):
        options, verbosity, durations = RUNNER._options(
            ["-v", "-p", "test_screen_*.py", "--durations", "10"]
        )
        self.assertEqual(options, {"pattern": "test_screen_*.py"})
        self.assertEqual(verbosity, 2)
        self.assertEqual(durations, 10)

    def test_unknown_and_incomplete_options_are_rejected(self):
        for args in (["--unknown"], ["-p"], ["--durations"], ["--durations", "-1"]):
            with self.subTest(args=args), self.assertRaises(SystemExit):
                RUNNER._options(args)


if __name__ == "__main__":
    unittest.main()
