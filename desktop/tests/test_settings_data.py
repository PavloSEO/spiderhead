import tempfile
import unittest
from pathlib import Path

from PyQt5.QtCore import QThreadPool
from PyQt5.QtWidgets import QPushButton, QWidget

from seohead_desktop.ui.settings import data
from seohead_desktop.ui.settings.context import SettingsContext
from tests._settings_support import all_text, app, make, row


class DataSectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = app()

    def test_defaults(self):
        store, _dialog, _page = make("data")
        self.assertEqual(store.get("data.keep_scans"), "20")
        self.assertEqual(store.get("data.html_age"), "90")

    def test_invalid_value_stores_nothing(self):
        store, _dialog, _page = make("data")
        self.assertTrue(store.set("data.keep_scans", "5"))
        self.assertTrue(store.set("data.html_age", "7"))
        self.assertFalse(store.is_set("data.keep_scans"))

    def test_segmented_writes_and_reset_restores(self):
        store, dialog, page = make("data")
        seg = row(page, "Хранить сканов на проект").control
        seg._buttons["10"].click()
        self.assertEqual(store.get("data.keep_scans"), "10")
        dialog.reset_section()
        self.assertEqual(store.get("data.keep_scans"), "20")
        seg = row(dialog._pages["data"][1], "Хранить сканов на проект").control
        self.assertEqual(seg.value(), "20")

    def test_no_context_means_no_data_not_zero(self):
        _store, _dialog, page = make("data")
        text = all_text(page)
        self.assertIn("Нет данных", text)
        self.assertNotIn("0 МБ", text)
        self.assertNotIn("0 Б", text)
        self.assertNotIn("Демо", text)
        self.assertNotIn("демо", text.lower())

    def test_actions_disabled_with_tooltip(self):
        _store, _dialog, page = make("data")
        for caption in ("Очистить кэш", "Очистить логи", "Сбросить все панели", "Экспорт настроек…", "Импорт…"):
            button = next(b for b in page.findChildren(QPushButton) if b.text() == caption)
            self.assertFalse(button.isEnabled(), caption)
            self.assertEqual(button.toolTip(), "Недоступно в этой сборке")

    def test_action_enabled_when_app_provides_it(self):
        calls = []
        context = SettingsContext(actions={"clear_logs": lambda: calls.append(1)})
        _store, _dialog, page = make("data", context)
        button = next(b for b in page.findChildren(QPushButton) if b.text() == "Очистить логи")
        self.assertTrue(button.isEnabled())
        button.click()
        self.assertEqual(calls, [1])

    def test_search_finds_rows(self):
        _store, dialog, _page = make("data")
        dialog.filter_sections("удалять html старше")
        self.assertEqual(dialog.current_section(), "data")

    def test_sizes_measured_off_thread_from_context_paths(self):
        with tempfile.TemporaryDirectory() as tmp:
            logs = Path(tmp, "logs")
            logs.mkdir()
            (logs / "a.log").write_bytes(b"x" * 3 * 1024 * 1024)
            context = SettingsContext(log_directory=str(logs), data_directory=str(Path(tmp, "missing")))
            _store, _dialog, page = make("data", context)
            QThreadPool.globalInstance().waitForDone()
            self.app.processEvents()
            text = all_text(page)
            self.assertIn(f"{logs} · 3,0 МБ", text)
            self.assertIn(" ГБ из ", text)
            self.assertNotIn(f"{Path(tmp, 'missing')} ·", text)

    def test_per_project_sizes_are_waiting_badge_not_numbers(self):
        _store, _dialog, page = make("data")
        badges = [w for w in page.findChildren(QWidget) if w.property("waiting_issue") == 1213]
        self.assertEqual(len(badges), 1)
        self.assertNotIn("ГБ", all_text(page))

    def test_reset_all_panels_is_not_clipped_in_its_own_row(self):
        _store, _dialog, page = make("data")
        button = next(b for b in page.findChildren(QPushButton) if b.text() == "Сбросить все панели")
        self.assertEqual(button.parentWidget().layout().count(), 2)  # button + stretch, alone in its row

    def test_format_size(self):
        from seohead_desktop.ui.settings.actions import format_size
        self.assertEqual(format_size(420 * 1024 * 1024), "420 МБ")
        self.assertEqual(format_size(int(6.1 * 1024 ** 3)), "6,1 ГБ")

    def test_schema_keys_are_prefixed(self):
        self.assertTrue(all(s.key.startswith("data.") for s in data.SCHEMA))


if __name__ == "__main__":
    unittest.main()
