import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CREATES = re.compile(r"\bQApplication\s*\(")


class SingleApplicationTests(unittest.TestCase):
    def test_only_the_qt_helper_creates_the_application(self):
        offenders = []
        for folder in ("tests", "scripts", "src"):
            for path in (ROOT / folder).rglob("*.py"):
                if (path.name == "qt.py" and path.parent.name == "seohead_desktop") or path == Path(__file__).resolve():
                    continue
                for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
                    if CREATES.search(line) and not line.lstrip().startswith("#"):
                        offenders.append(f"{path.relative_to(ROOT)}:{number}")
        self.assertEqual(offenders, [])

    def test_importing_the_package_disables_destroy_on_exit(self):
        import subprocess
        import sys

        code = "import seohead_desktop; from PyQt5 import sip; print(sip.getdestroyonexit() if hasattr(sip, 'getdestroyonexit') else 'n/a')"
        out = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, cwd=ROOT / "src", check=False)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn(out.stdout.strip(), ("False", "n/a"))


if __name__ == "__main__":
    unittest.main()
