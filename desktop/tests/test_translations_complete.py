"""Static untranslated-string check (#1379): every Cyrillic literal that can reach the UI — through
tr()/trf() or a widget call that ``i18n.retranslate`` later walks — must have an English entry in
i18n/en.json. A literal without an entry shows Russian in the English UI and fails the suite.
"""

import ast
import json
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src" / "seohead_desktop"
CYRILLIC = re.compile(r"[А-Яа-яЁё]")
DICTIONARY = json.loads((SRC / "i18n" / "en.json").read_text())
TRANSLATORS = {"tr", "trf"}
UI_CALLS = {
    "QLabel",
    "QPushButton",
    "QToolButton",
    "QCheckBox",
    "QMenu",
    "StatePanel",
    "show_empty",
    "setText",
    "setToolTip",
    "setPlaceholderText",
    "setWindowTitle",
    "setAccessibleName",
    "showMessage",
    "addAction",
    "addItem",
    "setPrefix",
    "setSuffix",
    "setSpecialValueText",
}
EXEMPT_FILES = ("i18n",)  # the dictionary module itself


def literals(tree):
    """Every string constant and f-string part, with the innermost enclosing call name."""
    parents = {}
    for node in ast.walk(tree):
        for child in ast.iter_child_nodes(node):
            parents[child] = node
    for node in ast.walk(tree):
        texts = []
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            texts.append(node.value)
        elif isinstance(node, ast.JoinedStr):
            texts += [part.value for part in node.values if isinstance(part, ast.Constant)]
        for text in texts:
            if not CYRILLIC.search(text):
                continue
            caller = ""
            current = node
            while current in parents and not isinstance(
                parents[current],
                (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef, ast.Module),
            ):
                parent = parents[current]
                if isinstance(parent, ast.Call):
                    func = parent.func
                    caller = getattr(func, "id", None) or getattr(func, "attr", "") or caller
                    break
                current = parent
            yield node.lineno, text, caller


class TranslationCompletenessTests(unittest.TestCase):
    def test_every_cyrillic_ui_literal_has_an_english_entry(self):
        violations = []
        for path in sorted(SRC.rglob("*.py")):
            if any(part in EXEMPT_FILES for part in path.relative_to(SRC).parts):
                continue
            tree = ast.parse(path.read_text())
            for lineno, text, caller in literals(tree):
                if caller not in TRANSLATORS and caller not in UI_CALLS:
                    continue
                if text not in DICTIONARY:
                    where = f"{path.relative_to(SRC)}:{lineno}"
                    violations.append(
                        f"{where}: no English entry for {'tr()' if caller in TRANSLATORS else 'UI'}"
                        f" literal: {text[:60]!r}"
                    )
        self.assertEqual([], violations, "\n".join(violations))


if __name__ == "__main__":
    unittest.main()
