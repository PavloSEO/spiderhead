import os
import subprocess
import sys
import unittest
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")


from seohead_desktop.qt import app as qt_app
from seohead_desktop.settings_store import AppSettings
from seohead_desktop.ui.controls import SettingRow
from seohead_desktop.ui.settings import full_schema, wiring
from seohead_desktop.ui.settings.context import SettingsContext
from seohead_desktop.ui.settings.dialog import SettingsDialog

ROOT = Path(__file__).resolve().parents[1]


class WiringTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def test_every_schema_key_is_classified(self):
        for key in wiring.all_keys(full_schema()):
            wiring.status(key)

    def test_unknown_key_is_refused_rather_than_assumed_applied(self):
        with self.assertRaises(KeyError):
            wiring.status("nonsense.key")

    def test_documentation_is_current(self):
        result = subprocess.run([sys.executable, str(ROOT / "scripts/gen_settings_wiring.py")], capture_output=True, text=True,
                                env={**os.environ, "QT_QPA_PLATFORM": "offscreen"})
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = (ROOT / "docs/spec/settings-wiring.ru.md").read_text()
        for key in wiring.all_keys(full_schema()):
            self.assertIn(f"`{key}`", doc)

    def test_rows_of_unapplied_settings_carry_the_badge_and_applied_ones_do_not(self):
        store = AppSettings(schema=full_schema())
        dialog = SettingsDialog(store, SettingsContext(), section="view")
        rows = {r.property("setting_key"): r for r in dialog.findChildren(SettingRow) if r.property("setting_key")}
        dialog.show()
        self.app.processEvents()
        self.assertFalse(rows["view.density"].later_badge.isVisible())
        self.assertFalse(rows["view.theme"].later_badge.isVisible())
        self.assertTrue(rows["view.zoom"].later_badge.isVisible())
        self.assertEqual(rows["view.zoom"].later_badge.text(), "Заработает позже")
        self.assertIn("шаг 5", rows["view.zoom"].later_badge.toolTip())
        dialog.close()

    def test_footer_does_not_claim_instant_effect(self):
        dialog = SettingsDialog(AppSettings(schema=full_schema()), SettingsContext())
        self.assertNotIn("сразу", dialog.footer_hint.text())


if __name__ == "__main__":
    unittest.main()
