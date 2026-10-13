import os
import unittest
from unittest.mock import patch
from uuid import UUID

from tests._qt import ui

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QCoreApplication, QEvent
from PyQt5.QtWidgets import QDialog, QLabel

from seohead_desktop import i18n, theming
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.local_owner_actions import LocalOwnerActions, manual_signoff_request
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.work import (
    TASK_STATES,
    TaskEditDialog,
    WorkScreen,
    goal_entry,
    local_stamp,
    progress_numbers,
)
from seohead_desktop.ui.kit import BADGE_ROLE, StatePanel
from seohead_desktop.ui.modal import ModalDialog
from tests._qt import sweep_widgets
from tests._screens_core import fixture, open_qa, texts


class WorkScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.window = MainWindow(persistent=False)
        self.screen = self.window.screens["work"]

    def tearDown(self):
        i18n.set_language("ru")
        self.window.close()
        self.app.processEvents()
        self.window.deleteLater()
        QCoreApplication.sendPostedEvents(None, QEvent.DeferredDelete)
        if theming.active_theme() != "light":
            load_theme(self.app, "light")

    def panel(self):
        return (
            self.screen.empty_holder.itemAt(0).widget()
            if self.screen.empty_holder.count()
            else None
        )

    def kpi(self, index):
        return self.screen.kpis[index]

    def test_screen_is_installed_in_the_work_slot(self):
        self.assertIsInstance(self.window.pages.widget(0), WorkScreen)
        self.assertIs(self.window.pages.widget(0), self.screen)

    def test_no_project_is_an_honest_empty_state_with_the_open_action(self):
        panel = self.panel()
        self.assertIsInstance(panel, StatePanel)
        self.assertEqual(panel.kind, "empty")
        self.assertFalse(self.screen.content.isVisibleTo(self.screen))
        with patch.object(
            self.window, "choose_project"
        ) as choose:  # the action is bound when the panel is built
            self.window.data_changed.emit("project")
            self.panel().action.click()
        choose.assert_called_once()

    def test_project_loading_state(self):
        self.window.project_directory = "/project/qa"
        self.window._project_loading = True
        self.window.data_changed.emit("project")
        self.assertEqual(self.panel().kind, "loading")

    def test_kpis_show_only_measured_numbers(self):
        open_qa(self.window)
        numbers = progress_numbers(self.window)
        self.assertEqual(
            (numbers["remaining"], numbers["complete"], numbers["total"]), (276, 0, 276)
        )
        self.assertEqual(self.kpi(0).number.text(), "276")
        self.assertEqual(self.kpi(1).number.text(), "0")
        self.assertIn(
            "in this core version", self.kpi(1).sub.text()
        )  # no audit plan: no denominator
        self.assertEqual(self.kpi(2).number.text(), ui("Нет данных"))  # not 0
        self.assertTrue(self.kpi(2).number.property("na"))
        self.assertIn("in this core version", self.kpi(2).sub.text())
        self.assertEqual(self.kpi(3).number.text(), ui("Нет"))  # no active run observed

    def test_a_measured_plan_gives_the_denominator_and_bar(self):
        open_qa(self.window)
        progress = fixture("progress.json")
        progress["audit_task_completion"] = {
            **progress["audit_task_completion"],
            "state": "measured",
            "numerator": 3,
            "denominator": 12,
        }
        self.window._work_progress = progress
        self.window.data_changed.emit("progress")
        self.assertEqual(self.kpi(1).number.text(), "3")
        self.assertIn("12", self.kpi(1).sub.text())
        self.assertFalse(self.screen._bars[1].isHidden())

    def test_no_demo_text_and_no_agent_activity_claim(self):
        open_qa(self.window)
        joined = "\n".join(texts(self.screen)).lower()
        self.assertNotIn("демо", joined)
        self.assertNotIn("агент работает", joined)
        self.assertNotIn("агент подключ", joined)
        self.assertNotIn("на связи", joined)

    def test_running_scan_comes_from_the_observed_run_only(self):
        open_qa(self.window)
        run = fixture("run_row.json")
        run["state"] = "running"
        self.window.observed_runs = [run]
        self.window.data_changed.emit("observer")
        self.assertEqual(self.kpi(3).number.text(), ui("Идёт {id}").format(id=run["id"][:10]))
        self.assertIn("20", self.kpi(3).sub.text())

    def test_task_table_rows_badges_and_filter(self):
        open_qa(self.window)
        model = self.screen.tasks
        self.assertEqual(len(model.rows), 3)
        row = model.rows[0]
        self.assertEqual(row["id"], "check:AJAX_CRAWLING_SCHEME_META_FRAGMENT")
        self.assertEqual(model.index(0, 1).data(BADGE_ROLE), ("info", ui("Новая")))
        self.assertEqual(model.index(0, 2).data(), "P1")
        self.assertEqual(model.index(0, 3).data(), ui("Проверка"))
        # the filter works on the loaded page: a closed row disappears from «Открытые» and stays in «Все»
        done = dict(self.window.task_model.rows[1], state="completed")
        self.window.task_model.replace([self.window.task_model.rows[0], done])
        self.window.data_changed.emit("tasks")
        self.assertEqual(len(self.screen.tasks.rows), 1)
        self.screen.filter_switch.setValue("all")
        self.screen.filter_switch.changed.emit("all")
        self.assertEqual(len(self.screen.tasks.rows), 2)
        self.assertEqual(self.screen.tasks.index(1, 1).data(BADGE_ROLE)[0], "ok")

    def test_task_search_matches_title_id_and_reason_without_changing_total(self):
        open_qa(self.window)
        total = self.window.task_total
        self.screen.search.setText("AJAX_CRAWLING_SCHEME_URL")
        self.screen.search_timer.timeout.emit()  # the filter waits for the debounce timer
        self.assertEqual(
            [row["id"] for row in self.screen.tasks.rows],
            ["check:AJAX_CRAWLING_SCHEME_URL"],
        )
        self.assertEqual(self.window.task_total, total)
        self.screen.search.clear()
        self.screen.search_timer.timeout.emit()
        self.assertEqual(len(self.screen.tasks.rows), 3)

    def test_every_core_task_state_has_a_badge(self):
        for state in (
            "remaining",
            "completed",
            "running",
            "blocked",
            "review",
            "stale",
            "not_agreed",
            "unavailable",
            "excluded",
            "deliverable",
        ):
            self.assertIn(state, TASK_STATES)
            self.assertIn(TASK_STATES[state][0], theming.theme()["badges"])

    def test_selecting_a_row_requests_the_detail_and_shows_it(self):
        open_qa(self.window)
        with patch.object(self.window, "select_project_task") as select:
            self.screen.task_table.selectRow(1)
        select.assert_called_once_with("check:AJAX_CRAWLING_SCHEME_URL")
        self.assertEqual(self.screen.selected_id, "check:AJAX_CRAWLING_SCHEME_URL")
        # an answer for another task is not shown as this task's detail: the panel says it is loading
        self.window.task_detail_result = fixture("task_detail.json")
        self.window.data_changed.emit("tasks")
        self.assertIn(ui("Загрузка…"), texts(self.screen.detail))
        self.screen.task_table.selectRow(0)
        self.window.data_changed.emit("tasks")
        detail = "\n".join(texts(self.screen.detail))
        self.assertIn("not attempted", detail)
        self.assertIn(ui("Детали задачи и ответственные пока не указаны."), detail)
        self.assertIn(ui("Записей в истории нет"), detail)

    def test_work_detail_displays_core_task_metadata_and_comments(self):
        open_qa(self.window)
        result = fixture("task_detail.json")
        result["item"]["task"] = {
            "executor": "owner",
            "actor": "agent/local",
            "next_step": "Review the evidence",
            "description": "Check the canonical target",
            "acceptance_criterion": "The page resolves to the preferred URL",
            "finding_ref": {"scan_id": "scan-1", "finding_id": "finding-2"},
            "method_ref": "scenario:technical-audit",
            "competitors": [{"url": "https://competitor.example.test/", "max_urls": 250}],
            "comments": [
                {
                    "actor": "owner",
                    "created_at": "2026-10-11T10:00:00Z",
                    "text": "Keep this scoped.",
                }
            ],
        }
        self.window.task_detail_result = result
        self.window.data_changed.emit("tasks")
        detail = "\n".join(texts(self.screen.detail))
        self.assertIn("Review the evidence", detail)
        self.assertIn("owner", detail)
        self.assertIn("Keep this scoped.", detail)
        self.assertIn("finding-2", detail)
        self.assertIn("scenario:technical-audit", detail)
        self.assertIn("250 URLs", detail)

    def test_task_editor_sends_explicit_nulls_and_clears_competitors(self):
        dialog = TaskEditDialog(
            {"id": "custom:owner-note", "title": "Review"},
            {
                "assignee": "owner",
                "description": "Old description",
                "finding_ref": {"scan_id": "scan-1", "finding_id": "finding-1"},
                "competitors": [{"url": "https://competitor.example.test/", "max_urls": 250}],
            },
        )
        dialog.assignee.clear()
        dialog.description.clear()
        dialog.finding_scan.clear()
        dialog.finding_id.clear()
        dialog.competitors.removeRow(0)
        dialog._accept()
        self.assertEqual(dialog.result_item["assignee"], None)
        self.assertEqual(dialog.result_item["description"], None)
        self.assertEqual(dialog.result_item["finding_ref"], None)
        self.assertEqual(dialog.result_item["competitors"], [])

    def test_owner_can_create_a_manual_task_with_the_current_checklist_revision(self):
        self.window.core_executable = "seohead"
        open_qa(self.window)
        sent = []
        with (
            patch("seohead_desktop.screens.work.TaskEditDialog") as editor,
            patch.object(self.window, "start_command", side_effect=lambda *args: sent.append(args)),
        ):
            editor.return_value.exec_.return_value = QDialog.Accepted
            editor.return_value.result_item = {
                "id": "custom:manual-review",
                "title": "Review a key page",
                "kind": "custom",
                "execution_kind": "manual",
            }
            self.screen._new_custom_task()
        definition = editor.call_args.args[0]
        self.assertTrue(definition["id"].startswith("custom:task-"))
        self.assertEqual(definition["execution_kind"], "manual")
        request_id, tool, arguments, _handler = sent[0]
        self.assertEqual((request_id, tool), ("task-update", "seo_project_checklist_update"))
        self.assertEqual(arguments["expected_revision"], self.window.task_revision)
        self.assertEqual(arguments["item"]["id"], "custom:manual-review")

    def test_task_attempt_requires_reason_and_never_submits_completion(self):
        open_qa(self.window)
        self.window.task_detail_result = fixture("task_detail.json")
        sent = []
        with (
            patch("seohead_desktop.screens.work.TaskAttemptDialog") as dialog_class,
            patch.object(self.window, "start_command", side_effect=lambda *args: sent.append(args)),
        ):
            dialog_class.return_value.exec_.return_value = QDialog.Accepted
            dialog_class.return_value.reason = "Started review"
            self.screen._record_task(self.screen.tasks.rows[0], "running")
        request_id, tool, arguments, _handler = sent[0]
        self.assertEqual((request_id, tool), ("task-record", "seo_project_checklist_record"))
        self.assertEqual(
            arguments["record"],
            {"status": "running", "reason": "Started review", "actor": "owner/desktop"},
        )
        self.assertNotEqual(arguments["record"]["status"], "succeeded")

    def test_manual_task_signoff_uses_confirmed_local_cli_route(self):
        open_qa(self.window)
        self.window.core_executable = "seohead"
        row = self.window.task_model.rows[0]
        row.update(kind="custom", execution_kind="manual", state="review")
        result = fixture("task_detail.json")
        result["item"]["kind"] = "custom"
        result["item"]["execution_kind"] = "manual"
        result["item"]["definition"] = {
            "id": row["id"],
            "kind": "custom",
            "execution_kind": "manual",
            "title": "Review evidence",
        }
        self.window.task_detail_result = result
        self.window.data_changed.emit("tasks")
        self.assertFalse(self.screen.detail.approve_button.isHidden())
        sent = []

        def submit(_service, executable, arguments, _success, _error, _owner, *, stdin_data=None):
            sent.append((executable, arguments, stdin_data))

        with (
            patch("seohead_desktop.screens.work.ManualTaskSignoffDialog") as dialog_class,
            patch.object(LocalOwnerActions, "submit", autospec=True, side_effect=submit),
            patch.object(self.window, "start_command") as mcp_call,
        ):
            dialog_class.return_value.exec_.return_value = QDialog.Accepted
            dialog_class.return_value.reason_text = "Owner checked the criteria"
            self.screen.detail._approve_manual_task()
        self.assertEqual(
            sent,
            [
                (
                    "seohead",
                    *manual_signoff_request(
                        self.window.project_directory,
                        row["id"],
                        result["revision"],
                        "Owner checked the criteria",
                    ),
                )
            ],
        )
        mcp_call.assert_not_called()

    def test_task_competitor_scan_requires_confirmation_and_revision(self):
        open_qa(self.window)
        self.window.core_executable = "seohead"
        task_id = "custom:competitor-review"
        row = {
            "id": task_id,
            "title": "Review competitor pages",
            "kind": "custom",
            "state": "remaining",
            "execution_kind": "manual",
            "task": {"competitors": [{"url": "https://competitor.example.test/", "max_urls": 300}]},
        }
        result = fixture("task_detail.json")
        result["revision"] = self.window.task_revision
        result["item"].update(
            {
                "id": task_id,
                "item_id": task_id,
                "kind": "custom",
                "execution_kind": "manual",
                "definition": {
                    "id": task_id,
                    "kind": "custom",
                    "execution_kind": "manual",
                    "title": row["title"],
                },
                "task": row["task"],
            }
        )
        self.window.task_detail_result = result
        self.screen.detail.show_item(row, result)
        submitted = []
        with (
            patch.object(ModalDialog, "exec_", return_value=QDialog.Accepted),
            patch.object(
                self.screen.detail.competitor_jobs,
                "submit",
                side_effect=lambda *args: submitted.append(args),
            ),
            patch("seohead_desktop.screens.work.run_competitor_scan_command") as run_command,
        ):
            self.screen.detail._scan_task_competitors()
            self.assertGreaterEqual(len(submitted), 1)
            submitted[-1][0]()
        self.assertEqual(run_command.call_args.args[0], "seohead")
        arguments = run_command.call_args.args[1]
        self.assertEqual(arguments[0], "project-competitors-scan")
        self.assertEqual(arguments[arguments.index("--task-id") + 1], task_id)
        self.assertEqual(
            arguments[arguments.index("--expected-revision") + 1],
            str(result["revision"]),
        )
        self.assertIn("--confirm", arguments)
        self.assertEqual(arguments[arguments.index("--concurrency") + 1], "3")
        self.assertEqual(UUID(arguments[arguments.index("--run-id") + 1]).version, 4)
        self.assertNotIn("--approve-large-crawl", arguments)

    def test_the_first_row_is_selected_without_a_second_detail_request(self):
        with patch.object(self.window, "start_command") as command:
            self.window.project_directory = "/project/qa"
            open_qa(self.window, inbox=False)
        self.assertEqual(self.screen.selected_id, "check:AJAX_CRAWLING_SCHEME_META_FRAGMENT")
        self.assertEqual(
            self.window.task_detail_requested, "check:AJAX_CRAWLING_SCHEME_META_FRAGMENT"
        )
        self.assertLessEqual(command.call_count, 1)

    def test_core_error_is_an_error_state_with_retry(self):
        open_qa(self.window)
        self.window.screen_errors = {"tasks": "project is locked"}
        with patch.object(self.window, "refresh_project") as refresh:
            self.window.data_changed.emit("tasks")
            panel = self.screen.task_state.itemAt(0).widget()
            self.assertEqual(panel.kind, "error")
            self.assertIn("project is locked", panel.text.text())
            self.assertFalse(self.screen.task_table.isVisibleTo(self.screen))
            panel.action.click()
        refresh.assert_called_once()

    def test_empty_checklist_is_not_zero_tasks_of_a_plan(self):
        open_qa(self.window, tasks=False)
        self.assertEqual(
            self.screen.task_state.itemAt(0).widget().kind, "loading"
        )  # not loaded yet
        self.window._work_progress = {"state": "not_initialized", "counts": {}}
        with patch.object(self.window, "start_command"):
            self.window.load_tasks({"items": [], "pagination": {"total": 0}})
        panel = self.screen.task_state.itemAt(0).widget()
        self.assertEqual(panel.kind, "empty")
        self.assertIn("has not been created", panel.title.text())
        self.assertEqual(
            self.kpi(0).number.text(), ui("Нет данных")
        )  # counters are null before the checklist exists
        self.assertEqual(self.screen.tabs.tabText(0), "Tasks · 0")

    def test_goal_header_from_the_inbox(self):
        open_qa(self.window)
        entry, accepted_at = goal_entry(self.window)
        self.assertEqual(entry["goal_state"], "accepted")
        self.assertIsNone(accepted_at)  # the core stores no acceptance time (#946)
        self.assertEqual(self.screen.goal_title.text(), "Убрать технические ошибки каталога")
        self.assertEqual(self.screen.eyebrow.text(), ui("Принятая цель"))
        self.assertTrue(self.screen.goal_waiting.isHidden())
        rows = self.window.inbox_model.rows
        proposed = dict(rows[1], goal_state="proposed")
        self.window.inbox_model.replace([rows[0], proposed])
        self.window.data_changed.emit("inbox")
        self.assertIn("awaiting acceptance", self.screen.eyebrow.text())
        self.window.inbox_model.replace([rows[0]])
        self.window.data_changed.emit("inbox")
        self.assertEqual(self.screen.goal_title.text(), ui("Цель ещё не принята"))

    def test_accepted_time_is_shown_when_a_goal_receipt_has_one(self):
        open_qa(self.window)
        rows = self.window.inbox_model.rows
        goal = dict(
            rows[1],
            triage=[
                {
                    "kind": "goal",
                    "goal_id": rows[1]["id"],
                    "actor": "agent/local",
                    "reason": "ok",
                    "recorded_at": "2026-10-09T09:09:19Z",
                }
            ],
        )
        self.window.inbox_model.replace([rows[0], goal])
        self.window.data_changed.emit("inbox")
        self.assertEqual(
            self.screen.eyebrow.text(), f"Accepted goal · {local_stamp('2026-10-09T09:09:19Z')}"
        )
        self.assertTrue(self.screen.goal_waiting.isHidden())

    def test_reacts_to_data_changed_and_ignores_other_kinds(self):
        open_qa(self.window)
        calls = []
        self.screen.refresh = lambda: calls.append(1)
        self.window.data_changed.emit("tasks")
        self.window.data_changed.emit("url-table")
        self.assertEqual(len(calls), 1)

    def test_runs_tab_and_activity_tab_state(self):
        open_qa(self.window)
        self.window.activity_model.replace(
            [
                {
                    "id": "r1",
                    "label": "09.10 11:21 · r1",
                    "kind": "crawl",
                    "state": "finished",
                    "fetched": 20,
                }
            ]
        )
        self.window.data_changed.emit("observer")
        self.assertEqual(self.screen.tabs.tabText(1), "Runs · 1")
        self.assertEqual(self.screen.runs.index(0, 3).data(), "20")
        self.assertIn("in this core version", self.screen.tabs.tabToolTip(2))

    def test_navigation_buttons(self):
        open_qa(self.window)
        self.screen.note_button.click()
        self.assertEqual(self.window.navigation.current_section(), "inbox")

    def test_english_and_back(self):
        open_qa(self.window)
        i18n.set_language("en")
        self.assertEqual(self.kpi(0).caption.text(), "Open tasks")
        self.assertEqual(self.kpi(2).number.text(), "No data")
        self.assertEqual(self.screen.tabs.tabText(0), "Tasks · 276")
        self.assertEqual(self.screen.eyebrow.text(), "Accepted goal")
        self.assertEqual(self.screen.tasks.index(0, 1).data(BADGE_ROLE), ("info", "New task"))
        i18n.set_language("ru")
        self.assertEqual(self.kpi(0).caption.text(), ui("Открытые задачи"))
        self.assertEqual(self.screen.tabs.tabText(0), "Задачи · 276")

    def test_narrow_window_hides_side_panel_and_does_not_scroll_sideways(self):
        open_qa(self.window)
        self.window.navigation.select_section("work")
        self.window.resize(800, 800)
        self.window.show()
        self.app.processEvents()
        self.assertTrue(self.screen.detail.isHidden())
        self.assertEqual(self.screen.task_table.horizontalScrollBar().maximum(), 0)
        self.screen.detail_toggle.click()
        self.assertFalse(self.screen.detail.isHidden())
        labels = [label for label in self.screen.findChildren(QLabel) if label.property("badge")]
        self.assertTrue(labels)

    def test_board_groups_core_rows_by_display_state(self):
        open_qa(self.window)
        board = self.screen.task_board
        self.screen.view_switch.setValue("board")
        self.screen.view_switch.changed.emit("board")
        self.assertEqual([column.count.text() for column in board.columns], ["3", "0", "0", "0"])
        first, second, third = (dict(row) for row in self.window.task_model.rows[:3])
        self.window.task_model.replace(
            [first, dict(second, state="blocked"), dict(third, state="completed")]
        )
        self.window.data_changed.emit("tasks")
        self.assertEqual([column.count.text() for column in board.columns], ["1", "1", "0", "1"])
        self.assertIn(ui("Заблокирована"), texts(board.columns[1]))

    def test_board_card_opens_the_task_in_the_list_and_widens_the_filter(self):
        open_qa(self.window)
        done_id = dict(self.window.task_model.rows[0], state="completed")
        self.window.task_model.replace([done_id, self.window.task_model.rows[1]])
        self.window.data_changed.emit("tasks")
        self.screen.view_switch.setValue("board")
        self.screen.view_switch.changed.emit("board")
        self.assertEqual(self.screen.filter, "open")
        with patch.object(self.window, "select_project_task") as select:
            self.screen.task_board.card_picked.emit(done_id["id"])
        self.assertEqual(self.screen.filter, "all")
        self.assertEqual(self.screen.view_switch.value(), "list")
        self.assertEqual(self.screen.selected_id, done_id["id"])
        select.assert_called_once_with(done_id["id"])


if __name__ == "__main__":
    unittest.main()
