import os
import unittest
from unittest.mock import patch

from tests._qt import ui

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QLabel, QProgressBar, QPushButton

from seohead_desktop import i18n
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.scan_common import RunRow, active_scan_summary, build_rows
from seohead_desktop.screens.scans import ROW_ROLE, ScansScreen
from seohead_desktop.ui.kit import BADGE_ROLE
from tests._qt import sweep_widgets
from tests._scan_fixtures import NOW, live_run, owned, run, scan, stale_run, stamp
from tests._screens_host import FakeHost, FakeManager

CYRILLIC = "[Ѐ-ӿ]"


def texts(root):
    return [label.text() for label in root.findChildren(QLabel)] + [
        button.text() for button in root.findChildren(QPushButton)
    ]


class Base(unittest.TestCase):
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
        self.app.processEvents()

    def tearDown(self):
        self.screen.close()
        self.screen.deleteLater()
        self.app.processEvents()

    def feed(self, **state):
        state.setdefault("observed_at", stamp(0.5))
        self.host.set_state(**state)
        self.app.processEvents()

    def cell(self, row, column, role=Qt.DisplayRole):
        return self.screen.proxy.index(row, column).data(role)


class RowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        i18n.set_language("ru")

    def test_runs_scans_and_owned_runs_are_joined_by_artifact_and_id(self):
        finished, other = (
            run(),
            run(
                "4b80b8ea140a4cdb86b529def1fc14fc",
                artifact="scans/20261009T082041Z_crawl.localhost_6c672516.sqlite",
            ),
        )
        host = FakeHost()
        host.observed_runs = [finished, other, live_run()]
        host.scan_model.rows = [
            scan(),
            scan("11111111-2222-3333-4444-555555555555", "scans/imported_without_run.sqlite"),
        ]
        host.owned = [owned(), owned("b" * 32, "queued", core=None)]
        rows = build_rows(host)
        self.assertEqual(
            len(rows), 5
        )  # 3 runs + a queued run without a core record + a saved scan without a run
        by_id = {row.id: row for row in rows}
        self.assertIs(by_id[finished["id"]].scan["uuid"], host.scan_model.rows[0]["uuid"])
        self.assertIsNone(
            by_id[other["id"]].scan
        )  # the artifact is not in the loaded page of scans
        self.assertEqual(by_id[live_run()["id"]].owned["state"], "running")
        self.assertTrue(
            any(
                row.run is None
                and row.scan
                and row.scan["path"].endswith("imported_without_run.sqlite")
                for row in rows
            )
        )
        self.assertTrue(
            any(row.run is None and row.owned and row.state == "queued" for row in rows)
        )

    def test_no_project_gives_no_rows(self):
        host = FakeHost(project=None)
        host.observed_runs = [run()]
        self.assertEqual(build_rows(host), [])

    def test_unmeasured_counters_stay_none_and_found_is_the_sum_only_when_complete(self):
        failed = RunRow(
            run(
                state="failed",
                counters={
                    "fetched": 0,
                    "queued": 0,
                    "inflight": None,
                    "excluded": None,
                    "rate_per_second": None,
                },
                finish_reason="ValueError",
            )
        )
        self.assertEqual((failed.fetched, failed.queued), (0, 0))
        self.assertIsNone(failed.inflight)
        self.assertIsNone(failed.found)
        self.assertEqual(failed.group, "failed")
        live = RunRow(live_run())
        self.assertEqual(
            (live.fetched, live.queued, live.inflight, live.excluded, live.found), (5, 13, 0, 0, 18)
        )
        self.assertEqual(live.urls_text, "5 / 18")

    def test_rate_is_shown_only_for_a_fresh_measurement(self):
        self.assertEqual(RunRow(live_run()).rate, 1.0)
        self.assertIsNone(RunRow(stale_run()).rate)
        self.assertIsNone(
            RunRow(run()).rate
        )  # a finished run keeps its last rate in counters; it is not «now»

    def test_groups_and_badges_follow_the_core_state_and_telemetry(self):
        cases = {
            "live": (RunRow(live_run()), "info"),
            "stale": (RunRow(stale_run()), "warn"),
            "failed": (RunRow(run(state="failed")), "err"),
            "done": (RunRow(run()), "ok"),
            "partial": (RunRow(run(state="partial")), "warn"),
        }
        for group, (row, badge) in cases.items():
            with self.subTest(group=group):
                self.assertEqual((row.group, row.badge_kind), (group, badge))
        self.assertEqual(
            RunRow(stale_run(), observed_at=stamp(0)).state_label(NOW), "Устарело · 1 мин 35 с"
        )
        self.assertEqual(RunRow(run(), scan=scan(crawl_partial=True)).group, "partial")
        self.assertEqual(RunRow(None, owned("c" * 32, "starting", core=None)).group, "pending")
        self.assertEqual(RunRow(None, owned("c" * 32, "stop_requested")).group, "live")

    def test_completeness_does_not_claim_more_than_the_scan_flags_say(self):
        self.assertEqual(
            RunRow(run(), scan=scan(crawl_partial=False, corpus_partial=False)).completeness,
            ui("полный"),
        )
        self.assertEqual(
            RunRow(run(), scan=scan(crawl_partial=False, corpus_partial=True)).completeness,
            ui("обход полный · корпус неполный"),
        )
        self.assertEqual(RunRow(run(), scan=scan(crawl_partial=True)).completeness, ui("частичный"))
        self.assertIsNone(RunRow(run(state="failed")).completeness)  # no saved scan: «Нет данных»
        self.assertEqual(RunRow(live_run()).completeness, ui("идёт"))

    def test_navigation_card_uses_measured_counters_only(self):
        host = FakeHost()
        self.assertIsNone(active_scan_summary(host))
        host.observed_runs = [run()]
        self.assertIsNone(active_scan_summary(host))  # a finished run is not an active scan
        host.observed_runs = [live_run()]
        self.assertEqual(
            active_scan_summary(host),
            {"text": "Скан · 5 / 18 URL", "done": 5, "total": 18, "stale": False},
        )
        host.observed_runs = [stale_run()]
        summary = active_scan_summary(host)
        self.assertTrue(summary["stale"])
        self.assertIn("наблюдение устарело", summary["text"])
        unmeasured = live_run(
            counters=dict.fromkeys(("fetched", "queued", "inflight", "excluded", "rate_per_second"))
        )
        host.observed_runs = [unmeasured]
        summary = active_scan_summary(host)
        self.assertEqual((summary["total"], summary["done"]), (None, None))
        self.assertEqual(summary["text"], ui("Скан идёт · число страниц не измерено"))
        host.observed_runs = [live_run(), live_run("f" * 32)]
        self.assertTrue(active_scan_summary(host)["text"].endswith("· ещё 1"))


class ScreenTests(Base):
    def test_states_without_project_loading_and_empty(self):
        self.host.project_directory = None
        self.feed()
        self.assertEqual(self.screen.panel_state, "noproject")
        self.assertFalse(self.screen.aside.isVisible())
        self.host.project_directory = "/project/qa"
        self.host._project_loading = True
        self.feed()
        self.assertEqual(self.screen.panel_state, "loading")
        self.host._project_loading = False
        self.feed()
        self.assertEqual(self.screen.panel_state, "empty")
        empty = self.screen.panel.widget(1)
        empty.action.click()
        self.assertEqual(self.host.calls[-1][0], "scan_preview")
        self.feed(runs=[run()], scans=[scan()])
        self.assertIsNone(self.screen.panel_state)
        self.assertTrue(self.screen.aside.isVisible())

    def test_table_shows_runs_with_badges_and_honest_missing_values(self):
        self.feed(
            runs=[
                run(),
                run(
                    "14337d5c" + "0" * 24,
                    "failed",
                    artifact="scans/x.sqlite",
                    started=620,
                    finished=618,
                    counters={
                        "fetched": 0,
                        "queued": 0,
                        "inflight": None,
                        "excluded": None,
                        "rate_per_second": None,
                    },
                    finish_reason="ValueError",
                ),
            ],
            scans=[scan()],
        )
        self.assertEqual(self.screen.proxy.rowCount(), 2)
        states = {self.cell(r, 3, BADGE_ROLE)[0] for r in range(2)}
        self.assertEqual(states, {"ok", "err"})
        failed_row = next(r for r in range(2) if self.cell(r, 3, BADGE_ROLE)[0] == "err")
        self.assertEqual(
            self.cell(failed_row, 6), ui("Нет данных")
        )  # no saved scan: completeness unknown, not «0» or empty
        self.assertEqual(self.cell(failed_row, 4), "0")  # measured zero stays 0
        ok_row = 1 - failed_row
        self.assertEqual(self.cell(ok_row, 4), "20")
        self.assertEqual(self.cell(ok_row, 6), ui("обход полный · корпус неполный"))
        self.assertIn("2 запусков в загруженной странице", self.screen.header.meta.text())

    def test_live_run_is_selected_and_its_aside_shows_measured_counters(self):
        self.feed(runs=[run(), live_run()], scans=[scan()], owned=[owned()])
        row = self.screen.current_row()
        self.assertEqual(
            row.group, "live"
        )  # an active run is followed until the user picks another row
        aside = self.screen.aside
        self.assertEqual(aside.stack.currentIndex(), 1)
        self.assertEqual(aside.live_ratio.text(), "5 / 18")
        self.assertEqual((aside.live_bar.maximum(), aside.live_bar.value()), (18, 5))
        values = {key: cell.value.text() for key, cell in aside.cells.items()}
        self.assertEqual(
            (
                values["found"],
                values["queued"],
                values["inflight"],
                values["fetched"],
                values["excluded"],
            ),
            ("18", "13", "0", "5", "0"),
        )
        self.assertTrue(aside.cells["errors"].value.isHidden())  # errors wait for #921, no number
        self.assertIn("1,0 · окно 5 с", aside.live_pairs.values[ui("Сейчас, запросов/с")].text())
        self.assertEqual(aside.live_pairs.values[ui("Связан с задачами")].text(), ui("Нет данных"))
        self.assertIn("наблюдение 0 с назад", aside.live_age.text())
        self.assertEqual(aside.live_badge.text(), ui("Идёт"))
        self.assertIn(ui("сбор страниц"), aside.live_events.text())

    def test_user_choice_is_kept_when_data_refreshes(self):
        self.feed(runs=[run(), live_run()], scans=[scan()])
        finished = next(r for r in range(2) if self.cell(r, 0, ROW_ROLE).group == "done")
        self.screen.table.selectRow(finished)
        self.app.processEvents()
        self.assertEqual(self.screen.current_row().group, "done")
        self.assertEqual(self.host.calls[-1], ("select_project_scan", scan()["uuid"]))
        self.feed(runs=[run(), live_run(telemetry={"age_seconds": 0.9})], scans=[scan()])
        self.assertEqual(self.screen.current_row().group, "done")
        self.assertEqual(self.screen.aside.stack.currentIndex(), 3)

    def test_stale_run_has_its_own_state_and_no_busy_animation(self):
        self.feed(runs=[stale_run()])
        aside = self.screen.aside
        self.assertEqual(aside.stack.currentIndex(), 2)
        self.assertIn("Наблюдение устарело · 1 мин 35 с", aside.stale_note.text())
        self.assertEqual(aside.stale_pairs.values[ui("Скорость")].text(), ui("Счётчик недоступен"))
        self.assertEqual(aside.stale_pairs.values[ui("Последний счётчик")].text()[:5], "5 URL")
        self.assertEqual(self.cell(0, 3, BADGE_ROLE)[0], "warn")
        for bar in self.screen.findChildren(QProgressBar):
            self.assertFalse(bar.minimum() == bar.maximum() == 0, "no busy bar on stale data")
        aside.stack.widget(2).findChildren(QPushButton)[0].click()
        self.assertEqual(self.host.calls[-1][0], "refresh_project")

    def test_stop_is_offered_only_for_runs_of_this_window(self):
        self.feed(runs=[live_run()], owned=[owned()])
        stop = self.screen.aside.stop_button
        self.assertTrue(stop.isEnabled())
        stop.click()
        self.assertEqual(
            self.host.calls[-2:], [("choose_owned_run", owned()["id"]), ("cancel_active_work",)]
        )
        self.host.calls.clear()
        self.feed(runs=[live_run()], owned=[])
        self.assertFalse(self.screen.aside.stop_button.isEnabled())
        self.assertIn("в этой версии ядра", self.screen.aside.stop_button.toolTip())
        self.screen.aside.stop_button.click()
        self.assertEqual(self.host.calls, [])

    def test_finished_scan_actions_open_the_result_and_resume_only_when_eligible(self):
        partial = scan(lifecycle="interrupted", crawl_partial=True)
        self.feed(runs=[run(state="partial")], scans=[partial])
        aside = self.screen.aside
        self.assertEqual(aside.stack.currentIndex(), 3)
        self.assertFalse(aside.resume_button.isHidden())
        self.assertFalse(
            aside.resume_button.isEnabled()
        )  # the host has not confirmed that this snapshot can be resumed
        self.host.selected_scan_path = partial["path"]
        self.host.resume_scan_button.setEnabled(True)
        self.feed(runs=[run(state="partial")], scans=[partial], kind="scan_status")
        self.assertTrue(aside.resume_button.isEnabled())
        aside.resume_button.click()
        self.assertEqual(self.host.calls[-1][0], "resume_selected_scan")
        aside.open_button.click()
        self.assertEqual(self.host.navigation.sections[-1], "url")
        self.assertNotIn(
            "select_project_scan", [c[0] for c in self.host.calls[-1:]]
        )  # already the selected scan
        self.feed(runs=[run()], scans=[scan()])
        self.assertTrue(self.screen.aside.resume_button.isHidden())

    def test_source_filter_and_sort_act_on_the_loaded_page(self):
        sitemap = run(
            "a" * 32, kind="sitemap", artifact="scans/s.sqlite", started=300, finished=290
        )
        self.feed(runs=[run(), sitemap], scans=[scan()])
        self.assertEqual(self.screen.proxy.rowCount(), 2)
        self.assertEqual(self.screen.source_menu.actions()[0].text(), ui("Все источники"))
        self.screen.set_source("По sitemap")
        self.assertEqual(self.screen.proxy.rowCount(), 1)
        self.assertEqual(self.cell(0, 2), ui("По sitemap"))
        self.assertIn("показано 1 из 2", self.screen.foot_text.text())
        self.screen.set_source(None)
        self.screen.table.sortByColumn(5, Qt.AscendingOrder)
        first = self.cell(0, 0, ROW_ROLE)
        self.screen.table.sortByColumn(5, Qt.DescendingOrder)
        self.assertNotEqual(first.id, self.cell(0, 0, ROW_ROLE).id)

    def test_footer_says_filters_work_only_on_the_loaded_page_with_core_issues(self):
        self.feed(runs=[run()], scans=[scan()])
        waiting = {
            label.property("waiting_issue")
            for label in self.screen.foot.findChildren(QLabel)
            if label.property("waiting_issue")
        }
        self.assertEqual(waiting, {927})
        self.assertIn("только в загруженной странице", self.screen.foot_text.text())
        self.host.scan_manager = FakeManager()
        self.feed(runs=[run()], scans=[scan()])
        self.assertIn("0 из 3", self.screen.foot_owned.text())

    def test_double_click_opens_the_monitor_for_active_runs_and_the_result_otherwise(self):
        self.feed(runs=[run(), live_run()], scans=[scan()])
        live = next(r for r in range(2) if self.cell(r, 0, ROW_ROLE).active)
        self.screen._row_activated(self.screen.proxy.index(live, 0))
        self.assertEqual(self.screen.stack.currentIndex(), 1)
        self.screen.show_list()
        self.screen._row_activated(self.screen.proxy.index(1 - live, 0))
        self.assertEqual(self.host.navigation.sections[-1], "url")

    def test_buttons_and_new_scan_use_host_actions(self):
        self.feed(runs=[run()], scans=[scan()])
        self.screen.new_scan.click()
        self.assertEqual(self.host.calls[-1][0], "scan_preview")
        self.screen.aside.window_button.click()
        self.assertEqual(self.host.calls[-1][0], "open_monitor_window")

    def test_english_leaves_no_russian_in_visible_texts(self):
        self.feed(runs=[run(), live_run(), stale_run()], scans=[scan()], owned=[owned()])
        i18n.set_language("en")
        i18n.retranslate(self.screen)
        import re

        leftovers = [
            t
            for t in texts(self.screen)
            if re.search(CYRILLIC, t) and not t.startswith(ui("Недоступно"))
        ]
        self.assertEqual(leftovers, [])
        self.assertEqual(self.screen.proxy.headerData(1, Qt.Horizontal), "Scan run")


class WindowIntegrationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        clock = patch("seohead_desktop.screens.scan_common.now", return_value=NOW)
        clock.start()
        self.addCleanup(clock.stop)
        self.window = MainWindow(persistent=False)

    def tearDown(self):
        self.window.close()
        self.app.processEvents()
        self.window.deleteLater()

    def open_project(self, runs, scans=()):
        window = self.window
        window.project_directory = "/project/qa"
        window.scan_model.replace(list(scans))
        window.observed_runs, window.observed_at = list(runs), stamp(0.5)
        window.data_changed.emit("observer")
        self.app.processEvents()

    def test_the_screen_replaces_the_legacy_page_and_follows_window_state(self):
        window = self.window
        screen = window.screens["scans"]
        self.assertIsInstance(screen, ScansScreen)
        self.assertIs(window.pages.widget(5), screen)
        window.project_directory = (
            "/p"  # without a project «Сканы» is the «Раздел готовится» placeholder
        )
        window.navigation.select_section("scans")
        self.assertIs(window.pages.currentWidget(), screen)
        self.open_project([run(), live_run()], [scan()])
        self.assertEqual(screen.proxy.rowCount(), 2)
        self.assertEqual(screen.current_row().group, "live")

    def test_the_navigation_card_shows_real_counters_and_never_animates_when_stale(self):
        window = self.window
        window.project_directory = "/project/qa"
        window.observed_runs, window.observed_at = [live_run()], stamp(0.5)
        window.update_active_scan_card()
        card = window.navigation.card
        self.assertTrue(card.active)
        self.assertEqual(card.text.text(), "Scan · 5 / 18 URL")
        self.assertEqual((card.bar.maximum(), card.bar.value()), (18, 5))
        window.observed_runs = [stale_run()]
        stale = live_run()
        stale["counters"] = {
            "fetched": None,
            "queued": None,
            "inflight": None,
            "excluded": None,
            "rate_per_second": None,
        }
        stale["telemetry"] = {**stale["telemetry"], "state": "stale"}
        window.observed_runs = [stale]
        window.update_active_scan_card()
        self.assertEqual(
            card.text.text(), "Scan running · page count not measured · observation is stale"
        )
        self.assertFalse(card.bar.minimum() == card.bar.maximum() == 0)
        window.observed_runs = [
            live_run(
                counters={
                    "fetched": None,
                    "queued": None,
                    "inflight": None,
                    "excluded": None,
                    "rate_per_second": None,
                }
            )
        ]
        window.update_active_scan_card()
        self.assertTrue(
            card.bar.minimum() == card.bar.maximum() == 0
        )  # live and unmeasured: honest indeterminate bar
        window.observed_runs = [run()]
        window.update_active_scan_card()
        self.assertFalse(card.active)

    def test_open_observation_goes_to_the_live_monitor(self):
        window = self.window
        self.open_project([live_run()])
        window.navigation.openScanRequested.emit()
        screen = window.screens["scans"]
        self.assertIs(window.pages.currentWidget(), screen)
        self.assertEqual(screen.stack.currentIndex(), 1)
        self.assertEqual(screen.monitor.row.group, "live")


if __name__ == "__main__":
    unittest.main()
