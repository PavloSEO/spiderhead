import os
import unittest
from unittest.mock import patch

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import re

from PyQt5.QtWidgets import QLabel, QProgressBar, QPushButton

from seohead_desktop import i18n
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.scans import ScansScreen
from tests._qt import sweep_widgets
from tests._scan_fixtures import NOW, live_run, owned, run, scan, stale_run, stamp
from tests._screens_host import FakeHost


def waiting(widget):
    return sorted(label.property("waiting_issue") for label in widget.findChildren(QLabel) if label.property("waiting_issue"))


class ScanRunTests(unittest.TestCase):
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
        self.screen = ScansScreen(self.host)
        self.screen.resize(1440, 800)
        self.screen.show()
        self.page = self.screen.monitor

    def tearDown(self):
        self.screen.close()
        self.screen.deleteLater()
        self.app.processEvents()

    def feed(self, **state):
        state.setdefault("observed_at", stamp(0.5))
        self.host.set_state(**state)
        self.app.processEvents()

    def test_without_an_active_run_the_monitor_is_an_empty_state(self):
        self.screen.show_monitor()
        self.assertEqual(self.page.pages.currentIndex(), 0)
        self.assertFalse(self.page.stop.isEnabled())
        self.assertFalse(self.page.badge.isVisible())

    def test_live_monitor_shows_only_what_the_core_measures(self):
        self.feed(runs=[run(), live_run()], scans=[scan()])
        self.screen.show_monitor()
        page = self.page
        self.assertEqual(page.pages.currentIndex(), 1)
        self.assertEqual(page.progress_text.text(), "Обработано 5 из 18 найденных URL")
        self.assertEqual((page.bar.maximum(), page.bar.value()), (18, 5))
        kpi = {key: widget.number.text() for key, widget in page.kpi.items()}
        self.assertEqual(kpi, {"queued": "13", "fetched": "5", "errors": "Нет данных", "excluded": "0", "rate": "1,0"})
        self.assertTrue(page.kpi["errors"].number.property("na"))
        self.assertEqual(page.badge.text(), "Идёт")
        self.assertEqual(page.age.text(), "Наблюдение 0 с назад")
        self.assertEqual(page.pairs.values["Осталось"].text(), "Недоступно")  # no ETA, no percentage
        self.assertEqual(page.pairs.values["Прошло"].text(), "8 с")
        self.assertEqual(page.pairs.values["Охват"].text(), "лимит 60 URL")
        self.assertIn(921, waiting(page))
        self.assertIn(933, waiting(page))

    def test_percent_comes_from_the_counters_and_the_speed_limit_from_the_collector(self):
        self.feed(runs=[live_run()], owned=[owned()])
        self.screen.show_monitor()
        self.assertEqual(self.page.percent.text(), "28%")  # 5 of 18 fetched
        self.assertEqual(self.page.limit.text(), "лимит 2 запр./с")
        self.assertEqual(self.page.bar.height(), 10)

    def test_pause_waits_for_the_core_and_the_chart_and_stream_are_not_invented(self):
        self.feed(runs=[live_run()], owned=[owned()])
        self.screen.show_monitor()
        self.assertFalse(self.page.pause.isEnabled())
        self.assertIn("в этой версии ядра", self.page.pause.toolTip())
        body = " ".join(label.text() for label in self.page.findChildren(QLabel))
        self.assertIn("график не рисуется", body)
        self.assertIn(933, waiting(self.page))

    def test_stop_and_banner_depend_on_who_started_the_run(self):
        self.feed(runs=[live_run()], owned=[owned()])
        self.screen.show_monitor()
        self.assertTrue(self.page.stop.isEnabled())
        self.assertIn("при закрытии окна он будет остановлен", self.page.banner_text.text())
        self.page.stop.click()
        self.assertEqual(self.host.calls[-2:], [("choose_owned_run", owned()["id"]), ("cancel_active_work",)])
        self.host.calls.clear()
        self.feed(runs=[live_run()], owned=[])
        self.assertFalse(self.page.stop.isEnabled())
        self.assertIn("окно можно закрыть", self.page.banner_text.text())
        self.assertIn("в этой версии ядра", self.page.stop.toolTip())
        self.page.stop.click()
        self.assertEqual(self.host.calls, [])

    def test_stale_observation_is_labelled_and_never_animated(self):
        self.feed(runs=[stale_run()])
        self.screen.show_monitor()
        page = self.page
        self.assertEqual(page.badge.text(), "Устарело · 1 мин 35 с")
        self.assertIn("Наблюдение устарело", page.age.text())
        self.assertEqual(page.kpi["rate"].number.text(), "Нет данных")
        self.assertEqual(page.kpi["rate"].sub.text(), "нет свежего измерения")
        for bar in page.findChildren(QProgressBar):
            self.assertFalse(bar.minimum() == bar.maximum() == 0)

    def test_unmeasured_counters_give_no_progress_numbers(self):
        self.feed(runs=[live_run(counters=dict.fromkeys(("fetched", "queued", "inflight", "excluded", "rate_per_second")))])
        self.screen.show_monitor()
        self.assertEqual(self.page.progress_text.text(), "Число обработанных и найденных URL не измерено")
        self.assertEqual(self.page.kpi["queued"].number.text(), "Нет данных")
        self.assertEqual(self.page.bar.value(), 0)

    def test_back_returns_to_the_list_and_saved_result_opens_the_url_table(self):
        self.feed(runs=[live_run()], scans=[scan(artifact="scans/live.sqlite")])
        self.screen.show_monitor()
        self.assertFalse(self.page.saved.isEnabled())  # the live run has no saved scan in the loaded page
        self.page.back_button.click()
        self.assertEqual(self.screen.stack.currentIndex(), 0)
        finished = run(artifact="scans/live.sqlite", identity=live_run()["id"], state="finished")
        self.feed(runs=[finished], scans=[scan(artifact="scans/live.sqlite")])
        self.screen.selected_key = "run:" + finished["id"]
        self.page.row = self.screen.current_row()
        self.page._render()
        self.assertTrue(self.page.saved.isEnabled())
        self.page.saved.click()
        self.assertEqual(self.host.navigation.sections[-1], "url")

    def test_the_monitor_follows_data_changes(self):
        self.feed(runs=[live_run()])
        self.screen.show_monitor()
        self.assertEqual(self.page.progress_text.text(), "Обработано 5 из 18 найденных URL")
        self.feed(runs=[live_run(counters={"fetched": 9, "queued": 9})])
        self.assertEqual(self.page.progress_text.text(), "Обработано 9 из 18 найденных URL")
        self.feed(runs=[run(identity=live_run()["id"])])  # the same run finished: the monitor keeps showing its final counters
        self.assertEqual(self.page.age.text(), "Запуск завершён: показаны итоговые счётчики")
        self.assertFalse(self.page.stop.isEnabled())

    def test_english_leaves_no_russian(self):
        self.feed(runs=[live_run()], owned=[owned()])
        self.screen.show_monitor()
        i18n.set_language("en")
        i18n.retranslate(self.screen)
        texts = [label.text() for label in self.page.findChildren(QLabel)] + [b.text() for b in self.page.findChildren(QPushButton)]
        leftovers = [t for t in texts if re.search("[\u0400-\u04ff]", t) and not t.startswith("Недоступно")]  # kit badges are not retranslated
        self.assertEqual(leftovers, [])


if __name__ == "__main__":
    unittest.main()
