"""Spacing ratchet (#1374): margins and spacings should come from the token scale (0/2/4/8/12/16/20/24/32…).

Historical code still carries off-scale values (6, 10, 14, …). The suite fails only when their number
GROWS; bring the baseline down whenever you normalise a layout to the tokens.
"""

import ast
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src" / "seohead_desktop"
SCALE = {0, 2, 4, 8, 12, 16, 20, 24, 32, 36, 40, 48, 56, 64, 72, 96, 110, 118, 132, 150, 220, 236, 260, 296}
RATCHET = 0  # the scale is enforced: no off-scale margins or spacings anywhere (#1374)


def off_scale_calls():
    found = []
    for path in sorted(SRC.rglob("*.py")):
        if "i18n" in path.relative_to(SRC).parts:
            continue
        tree = ast.parse(path.read_text())
        for node in ast.walk(tree):
            if not isinstance(node, ast.Call):
                continue
            name = getattr(node.func, "attr", None) or getattr(node.func, "id", None)
            if name not in ("setContentsMargins", "setSpacing"):
                continue
            for argument in node.args:
                if isinstance(argument, ast.Constant) and isinstance(argument.value, int) and argument.value not in SCALE:
                    found.append(f"{path.relative_to(SRC)}:{node.lineno} {ast.unparse(node)[:80]}")
                    break
    return found


class SpacingScaleTests(unittest.TestCase):
    def test_off_scale_spacing_calls_never_grow(self):
        found = off_scale_calls()
        self.assertLessEqual(
            len(found),
            RATCHET,
            f"{len(found)} off-scale spacing calls (ratchet {RATCHET}); normalise the layout to tokens"
            f" and lower the ratchet:\n" + "\n".join(found[:10]),
        )


if __name__ == "__main__":
    unittest.main()
