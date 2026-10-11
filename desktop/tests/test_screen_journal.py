import os
import re
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QLabel, QPushButton

from seohead_desktop import i18n
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.journal import LIMIT, JournalScreen, collect_events
from tests._qt import sweep_widgets
from tests._scan_fixtures import NOW, events, live_run, run
from tests._screens_host import FakeHost


class JournalTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        clock = patch("seohead_desktop.screens.scan_common.now", return_value=NOW)
        clock.start()
        self.addCleanup(clock.stop)
        self.addCleanup(i18n.set_language, "ru")
        self.host = FakeHost()
        self.screen = JournalScreen(self.host)
        self.screen.resize(1440, 800)
        self.screen.show()

    def tearDown(self):
        self.screen.close()
        self.screen.deleteLater()
        self.app.processEvents()

    def feed(self, runs, kind="observer"):
        self.host.set_state(runs=runs, kind=kind)
        self.app.processEvents()

    def column(self, number):
        model = self.screen.model
        return [model.index(row, number).data() for row in range(model.rowCount())]

    def test_states_for_no_project_loading_empty_and_content(self):
        self.host.project_directory = None
        self.feed([])
        self.assertEqual(self.screen.state, "noproject")
        self.host.project_directory = "/project/qa"
        self.host._project_loading = True
        self.feed([])
        self.assertEqual(self.screen.state, "loading")
        self.host._project_loading = False
        self.feed([])
        self.assertEqual(self.screen.state, "empty")
        self.feed([run()])
        self.assertIsNone(self.screen.state)
        self.assertEqual(self.screen.model.rowCount(), 4)

    def test_events_come_from_run_lifecycles_newest_first_without_invented_texts(self):
        self.feed([run(), live_run()])
        times = self.column(0)
        self.assertEqual(len(times), 8)
        stamps = [event.at for event, _group, _role in self.screen.model.rows]
        self.assertEqual(stamps, sorted(stamps, reverse=True))
        self.assertTrue(all(source.startswith("Скан · встроенный краулер") for source in self.column(1)))
        self.assertIn("Запуск принят · проверка лимитов", self.column(2))
        self.assertIn("Обновление счётчиков · сбор страниц", self.column(2))
        self.assertEqual(set(self.column(3)), {"r-" + run()["id"][:4], "r-" + live_run()["id"][:4]})

    def test_a_message_from_the_core_is_shown_as_is(self):
        item = run(event_list=[{"at": "2026-10-09T09:00:00Z", "code": "progress", "phase": "collection", "message": "checkpoint 400"}])
        self.feed([item])
        self.assertEqual(self.column(2), ["checkpoint 400"])

    def test_only_the_last_200_events_are_kept(self):
        many = run(event_list=[{"at": f"2026-10-09T08:{i // 60:02d}:{i % 60:02d}Z", "code": "progress", "phase": "collection"} for i in range(300)])
        shown, total = collect_events([many])
        self.assertEqual((len(shown), total), (LIMIT, 300))
        self.assertGreater(shown[0].at, shown[-1].at)
        self.feed([many])
        self.assertEqual(sum(len(group) for group in self.screen.model.groups), LIMIT)
        self.assertTrue(self.screen.foot_text.full_text.startswith("Показаны последние 200 событий"))

    def test_identical_events_in_a_row_fold_into_one_unfoldable_row(self):
        many = run(event_list=[{"at": f"2026-10-09T08:00:{i:02d}Z", "code": "progress", "phase": "collection"} for i in range(42)])
        self.feed([many])
        self.assertEqual(self.screen.model.rowCount(), 1)
        head = self.column(2)[0]
        self.assertIn("Обновление счётчиков · сбор страниц ×42", head)
        self.assertRegex(head, r"×42, \d\d:\d\d:\d\d–\d\d:\d\d:\d\d")
        self.screen.model.toggle(0)
        self.assertEqual(self.screen.model.rowCount(), 43)
        self.screen.model.toggle(0)
        self.assertEqual(self.screen.model.rowCount(), 1)

    def test_source_filters_without_data_are_disabled_and_explained(self):
        self.feed([run()])
        pills = self.screen.pills
        self.assertTrue(pills["all"].isEnabled() and pills["scan"].isEnabled())
        for key in ("agent", "me", "app"):
            self.assertFalse(pills[key].isEnabled())
            self.assertIn("Источник не указан", pills[key].toolTip())
            self.assertIn("в этой версии ядра", pills[key].toolTip())
        pills["scan"].click()
        self.assertEqual(self.screen.filter, "scan")
        self.assertEqual(self.screen.model.rowCount(), 4)  # every event of the core is a run event

    def test_footer_limit_note_is_a_single_line_and_the_button_opens_app_logs(self):
        self.feed([run()])
        self.assertEqual(self.screen.foot_text.full_text, "Показаны последние 4 событий: постраничное чтение журнала ещё не поддержано ядром")
        self.assertEqual(self.screen.open_folder.text(), "Открыть папку логов")
        self.screen.resize(800, 800)
        self.app.processEvents()
        label = self.screen.foot_text
        self.assertLessEqual(label.fontMetrics().horizontalAdvance(label.text()), label.width())

    def test_search_filters_the_loaded_events_and_offers_a_reset(self):
        self.feed([run(), live_run()])
        self.screen.search.setText("анализ")
        self.assertEqual(self.screen.model.rowCount(), 0 if "анализ" not in " ".join(self.column(2)).lower() else self.screen.model.rowCount())
        self.screen.search.setText("завершено")
        self.assertEqual(self.column(2), ["Завершено · сохранение результата"])
        self.screen.search.setText(live_run()["id"][:8])
        self.assertEqual(sum(len(group) for group in self.screen.model.groups), 4)
        self.screen.search.setText("такого нет")
        self.assertEqual(self.screen.state, "nomatch")
        self.screen.panel = None
        self.screen.stack.widget(1).action.click()
        self.assertEqual(self.screen.search.text(), "")
        self.assertIsNone(self.screen.state)
        self.assertEqual(sum(len(group) for group in self.screen.model.groups), 8)

    def test_data_changes_refresh_and_the_button_opens_the_app_logs_folder_without_the_core(self):
        self.feed([run()])
        self.assertEqual(self.screen.model.rowCount(), 4)
        self.feed([run(), live_run()])
        self.assertEqual(self.screen.model.rowCount(), 8)
        with tempfile.TemporaryDirectory() as home, patch("seohead_desktop.screens.journal.app_logs_directory", return_value=Path(home, "SEOHEAD")):
            with patch("seohead_desktop.screens.journal.QDesktopServices.openUrl") as opened:
                self.screen.open_folder.click()
            self.assertTrue(Path(home, "SEOHEAD").is_dir())
        self.assertEqual(opened.call_args[0][0].toLocalFile(), str(Path(home, "SEOHEAD")))

    def test_english_leaves_no_russian_in_visible_texts(self):
        self.feed([run(), live_run()])
        i18n.set_language("en")
        i18n.retranslate(self.screen)
        texts = [label.text() for label in self.screen.findChildren(QLabel)] + [b.text() for b in self.screen.findChildren(QPushButton)]
        texts += [self.screen.model.index(r, c).data() for r in range(self.screen.model.rowCount()) for c in (1, 2)]
        texts += [self.screen.model.headerData(c, Qt.Horizontal) for c in range(4)] + [p.text() for p in self.screen.pills.values()]
        self.assertEqual([t for t in texts if t and re.search("[Ѐ-ӿ]", t) and not t.startswith("Недоступно")], [])


class WindowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def test_the_screen_replaces_the_journal_page(self):
        sweep_widgets()
        window = MainWindow(persistent=False)
        try:
            self.assertIs(window.pages.widget(8), window.screens["journal"])
            window.navigation.select_section("log")
            self.assertIs(window.pages.currentWidget(), window.screens["journal"])
            window.project_directory = "/project/qa"
            window.observed_runs = [run(event_list=events(("started", "admission")))]
            window.data_changed.emit("observer")
            self.assertEqual(window.screens["journal"].model.rowCount(), 1)
        finally:
            window.close()
            self.app.processEvents()
            window.deleteLater()


if __name__ == "__main__":
    unittest.main()
