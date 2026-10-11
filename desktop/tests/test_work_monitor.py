"""Snapshot, identity, telemetry and native interaction contracts for WorkMonitor."""

import unittest
from copy import deepcopy
from unittest.mock import patch

from PyQt5.QtCore import QAbstractAnimation, Qt
from PyQt5.QtTest import QSignalSpy, QTest

from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.work_monitor import WorkMonitor


def observed(identity="native-run", **changes):
    row = {
        "id": identity, "kind": "native", "state": "running", "artifact": "scans/synthetic.sqlite",
        "collector": {"origin": "synthetic.test", "mode": "spider", "max_urls": 100, "max_requests_per_second": 2},
        "counters": {"fetched": 20, "queued": 40, "inflight": 2, "excluded": 3, "rate_per_second": 99},
        "telemetry": {"state": "fresh", "current_rate_per_second": 1.25, "unit": "pages", "sampled_at": "2026-10-07T16:00:00Z", "age_seconds": 2, "rate_window_seconds": 5, "queue_semantics": "separate"},
        "events": [{"at": "2026-10-07T16:00:00Z", "phase": "collection", "code": "progress"}],
    }
    return {**row, **changes}


class WorkMonitorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app)

    def setUp(self):
        with patch("seohead_desktop.ui.work_monitor.system_reduced_motion", return_value=False):
            self.monitor = WorkMonitor()
        self.monitor.resize(1024, 720)
        self.monitor.show()
        QTest.qWait(20)

    def tearDown(self):
        self.monitor.close()
        self.monitor.deleteLater()
        self.app.processEvents()

    def test_exact_core_identity_survives_reordering_and_missing_selected_run(self):
        seen = QSignalSpy(self.monitor.runSelected)
        self.monitor.set_observation([observed("first"), observed("second")], {}, "2026-10-07T16:00:02Z")
        self.monitor.set_selected_run("second")
        self.monitor.set_observation([observed("second"), observed("first")], {}, None)
        self.assertTrue(self.monitor.run_cards["second"].isChecked())
        self.assertEqual(len(seen), 0)
        self.monitor.set_observation([observed("first")], {}, None)
        self.assertEqual(self.monitor.selected_run_id, "second")
        self.assertFalse(self.monitor.selected.isVisible())
        self.assertFalse(self.monitor.result_button.isEnabled())

    def test_owned_queue_joins_reserved_id_without_fabricating_measurements(self):
        owner = {"id": "manager-id", "observer_run_id": "reserved-core-id", "core_run_id": None, "owned": True, "state": "queued", "kind": "crawl", "max_urls": 30}
        self.monitor.set_observation({"items": [], "owned": [owner]}, {}, None)
        self.assertEqual(list(self.monitor.run_cards), ["reserved-core-id"])
        self.assertIn("Очередь этого окна", self.monitor.run_cards["reserved-core-id"].activity.text())
        self.assertEqual(self.monitor.metric_values["fetched"].text(), "—")
        self.assertFalse(self.monitor.result_button.isEnabled())
        self.monitor.set_observation({"items": [observed("reserved-core-id")], "owned": [owner]}, {}, None)
        self.assertEqual(len(self.monitor.run_cards), 1)
        self.assertEqual(self.monitor.metric_values["fetched"].text(), "20")
        self.assertEqual(self.monitor.selected_badge.text(), "Выполняется")

    def test_rate_uses_fresh_core_measurement_and_never_old_counter_rate(self):
        row = observed()
        self.monitor.set_observation([row], {}, None)
        self.assertEqual(self.monitor.metric_values["rate"].text(), "1.25 стр./с")
        self.assertIn("Возраст: 2 с", self.monitor.sample_label.text())
        for state in ("retained", "stale", "unavailable"):
            row["telemetry"]["state"] = state
            self.monitor.set_observation([row], {}, None)
            self.assertNotIn("1.25", self.monitor.metric_values["rate"].text())
            self.assertNotIn("99", self.monitor.metric_values["rate"].text())
        row["telemetry"].update(state="fresh", current_rate_per_second=float("nan"))
        self.monitor.set_observation([row], {}, None)
        self.assertEqual(self.monitor.metric_values["rate"].text(), "—")
        self.assertIn("Нет текущей скорости", self.monitor.metric_labels["rate"].text())

    def test_sources_units_and_stage_use_supplied_evidence(self):
        sf = observed("sf", kind="screaming_frog", collector={"mode": "sf_live"})
        sf["telemetry"].update(unit="urls_including_resources", queue_semantics="outstanding")
        sitemap = observed("map", kind="sitemap", collector={"mode": "sitemap"})
        sitemap["telemetry"]["unit"] = "sitemap_documents"
        self.monitor.set_observation([sf, sitemap], {}, None)
        self.assertEqual(self.monitor.run_cards["sf"].source.text(), "Screaming Frog")
        self.assertEqual(self.monitor.metric_labels["fetched"].text(), "URL и ресурсов")
        self.assertIn("включает", self.monitor.queue_label.text())
        self.monitor.set_selected_run("map")
        self.assertEqual(self.monitor.run_cards["map"].source.text(), "Sitemap")
        self.assertEqual(self.monitor.metric_labels["fetched"].text(), "Sitemap-документов")
        self.assertIn("Сбор", self.monitor.phase_label.text())
        self.monitor.set_observation([observed("map", events=[])], {}, None)
        self.assertEqual(self.monitor.phase_label.text(), "Этап ещё не сообщён")

    def test_plan_percentage_needs_measured_denominator_and_budget_is_not_completion(self):
        partial = observed(state="partial", counters={"fetched": 100, "queued": 0, "inflight": 0, "excluded": 0})
        self.monitor.set_observation([partial], {}, None)
        self.assertFalse(self.monitor.plan_progress.isVisible())
        self.assertIn("URL: 100", self.monitor.budget_label.text())
        self.assertIn("размер сайта не измерен", self.monitor.queue_label.text())
        self.assertEqual(self.monitor.selected_badge.text(), "Частичный результат")
        plan = {"audit_task_completion": {"state": "measured", "numerator": 3, "denominator": 12}, "counts": {"remaining": 9, "stale": 0}}
        self.monitor.set_observation([partial], plan, None)
        self.assertEqual(self.monitor.plan_progress.value(), 250)
        self.assertIn("3 из 12 согласованных задач", self.monitor.plan_label.text())
        for numerator, denominator in ((0, 0), (True, 2), (3, 2), (1, None)):
            plan["audit_task_completion"].update(numerator=numerator, denominator=denominator)
            self.monitor.set_observation([partial], plan, None)
            self.assertFalse(self.monitor.plan_progress.isVisible())

    def test_events_and_runs_are_bounded_and_updates_reuse_cards(self):
        events = [{"at": str(i), "phase": "collection", "code": "progress"} for i in range(80)]
        row = observed(events=events)
        original = deepcopy(row)
        consumed = []
        def rows():
            for index in range(1000000):
                consumed.append(index)
                yield observed(str(index), events=events)
        self.monitor.set_observation(rows(), {}, None)
        self.assertEqual(len(consumed), 101)
        self.assertEqual(len(self.monitor.run_cards), 6)
        self.assertEqual(len(self.monitor.runs), 100)
        self.assertEqual(self.monitor.events_model.rowCount(), 20)
        self.assertEqual(self.monitor.events_model.rows[0]["at"], "60")
        self.assertIn("показаны не все", self.monitor.observation.text())
        self.monitor.set_selected_run(None)
        self.monitor.set_observation([row], {}, None)
        card = self.monitor.run_cards[row["id"]]
        self.monitor.set_observation([row], {}, None)
        self.assertIs(self.monitor.run_cards[row["id"]], card)
        self.assertEqual(row, original)

    def test_overview_caps_widgets_but_keeps_all_supplied_runs_selectable(self):
        rows = [observed(str(index), state="finished", finished_at="2026-10-07T16:00:00Z") for index in range(50)]
        rows[-1]["state"] = "running"
        self.monitor.set_observation({"items": rows, "active_total": 1, "total": 50}, {}, None)
        self.assertEqual(len(self.monitor.run_cards), 6)
        self.assertEqual(len(self.monitor.runs), 50)
        self.assertEqual(next(iter(self.monitor.run_cards)), "49")
        self.monitor.set_selected_run("30")
        self.assertEqual(self.monitor.selected_run_id, "30")
        self.assertTrue(self.monitor.selected.isVisible())
        self.assertIn("Незавершённых: 1", self.monitor.observation.text())

    def test_unobserved_terminal_owners_cannot_displace_external_active_run(self):
        owners = [{"id": str(index), "core_run_id": "old-" + str(index), "state": "finished", "kind": "crawl", "owned": True} for index in range(100)]
        self.monitor.set_observation({"items": [observed("external-active")], "owned": owners}, {}, None)
        self.assertIn("external-active", self.monitor.runs)
        self.assertEqual(list(self.monitor.run_cards), ["external-active"])

    def test_single_run_card_has_natural_maximum_width(self):
        self.monitor.resize(1440, 900)
        self.monitor.set_observation([observed()], {}, None)
        self.app.processEvents()
        self.assertLessEqual(next(iter(self.monitor.run_cards.values())).width(), 380)

    def test_wrapped_overview_rows_keep_natural_height_after_resize(self):
        for count in (3, 6):
            self.monitor.set_observation([observed(str(index), state="finished", finished_at="2026-10-07T16:00:00Z") for index in range(count)], {}, None)
            for width in (1440, 740, 685, 1440, 740):
                self.monitor.resize(width, 740)
                QTest.qWait(20)
                cards = list(self.monitor.run_cards.values())
                columns = self.monitor._columns
                if count > columns:
                    first_bottom = max(card.geometry().bottom() for card in cards[:columns])
                    gap = cards[columns].y() - first_bottom - 1
                    self.assertLessEqual(gap, self.monitor.cards.verticalSpacing() + 1)
                    self.assertGreaterEqual(gap, 0)
                self.assertLessEqual(self.monitor.cards_scroll.height(), 2 * max(card.height() for card in cards) + self.monitor.cards.verticalSpacing() + 1)

    def test_unknown_plan_is_one_line_but_measured_zero_task_counts_remain(self):
        self.monitor.set_observation([observed()], {"counts": {"complete": None, "remaining": None, "stale": None}}, None)
        self.assertFalse(self.monitor.plan_note.isVisible())
        self.assertIn("не измерено", self.monitor.plan_label.text())
        self.assertIn("согласованный план", self.monitor.plan_label.toolTip())
        self.monitor.set_observation([observed()], {"counts": {"complete": 0, "remaining": 0, "stale": 0}}, None)
        self.assertTrue(self.monitor.plan_note.isVisible())
        self.assertIn("Завершено: 0", self.monitor.plan_note.text())

    def test_native_selection_and_result_intents_do_not_dispatch(self):
        selected, result = QSignalSpy(self.monitor.runSelected), QSignalSpy(self.monitor.showResult)
        with patch("subprocess.run", side_effect=AssertionError("View cannot dispatch")):
            self.monitor.set_observation([observed()], {}, None)
            QTest.qWait(20)
            card = self.monitor.run_cards["native-run"]
            card.setFocus()
            QTest.keyClick(card, Qt.Key_Space)
            self.assertEqual(selected[-1], ["native-run"])
            QTest.mouseClick(self.monitor.result_button, Qt.LeftButton)
            self.assertEqual(result[-1], ["native-run"])

    def test_value_transition_preserves_exact_count_and_respects_reduced_motion(self):
        row = observed()
        self.monitor.set_observation([row], {}, None)
        QTest.qWait(20)
        row["counters"]["fetched"] = 30
        self.monitor.set_observation([row], {}, None)
        self.assertEqual(self.monitor.metric_values["fetched"].text(), "30")
        self.assertEqual(self.monitor.transition.duration(), 120)
        self.assertEqual(self.monitor.transition.state(), QAbstractAnimation.Running)
        QTest.qWait(160)
        self.assertEqual(self.monitor.effect.opacity(), 1.0)
        self.monitor.set_reduced_motion(True)
        row["counters"]["fetched"] = 40
        self.monitor.set_observation([row], {}, None)
        self.assertEqual(self.monitor.metric_values["fetched"].text(), "40")
        self.assertEqual(self.monitor.transition.state(), QAbstractAnimation.Stopped)
        self.assertEqual(self.monitor.effect.opacity(), 1.0)

    def test_four_cards_reflow_at_640_1024_and_1440_without_widening_window(self):
        self.monitor.set_observation([observed(str(i)) for i in range(4)], {}, None)
        for width in (640, 1024, 1440):
            self.monitor.resize(width, 720)
            QTest.qWait(40)
            self.assertEqual(self.monitor.width(), width)
            self.assertEqual(self.monitor._columns, 4 if width == 1440 else 3 if width == 1024 else 2)
            for card in self.monitor.run_cards.values():
                self.assertTrue(self.monitor.cards_widget.rect().contains(card.geometry()))
                self.assertGreater(card.width(), 220)


if __name__ == "__main__":
    unittest.main()
