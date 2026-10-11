import os
import unittest
from unittest.mock import patch

from tests._qt import ui

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QCoreApplication, QEvent, Qt
from PyQt5.QtWidgets import QDialog

from seohead_desktop import i18n, theming
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.local_owner_actions import LocalOwnerActions, accept_goal_arguments
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.inbox import MAX_TEXT, InboxScreen, is_processed, stages
from seohead_desktop.ui.kit import BADGE_ROLE
from seohead_desktop.ui.modal import ModalDialog
from tests._qt import sweep_widgets
from tests._screens_core import DIRECTORY, fixture, open_qa, texts

ACCEPTED_GOAL = "inbox:8bd73606-681f-43d3-98b5-4aaf95ecc189"
REJECTED_NOTE = "inbox:ee029d9c-0487-41a5-88e1-ab121307ed5b"


class InboxScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.window = MainWindow(persistent=False)
        self.screen = self.window.screens["inbox"]
        self.composer = self.screen.composer

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

    def write(self, text, kind="note", references=None):
        self.composer.set_draft(
            text, kind, self.composer.references if references is None else references
        )
        self.window.update_note_controls()

    def fake_core(self):
        """start_command that behaves like a request the core has accepted but not answered yet."""

        def start(request_id, *_args):
            self.window.requests[request_id] = self.window.read_generation

        return patch.object(self.window, "start_command", side_effect=start)

    def wide(self):
        self.window.navigation.select_section("inbox")
        self.window.setAttribute(
            Qt.WA_DontShowOnScreen, True
        )  # the offscreen screen clamps real windows to 800 px
        self.window.resize(1440, 900)
        self.window.show()
        self.app.processEvents()
        self.window.resize(1440, 900)
        self.app.processEvents()

    def test_screen_is_installed_in_the_inbox_slot(self):
        self.assertIsInstance(self.window.pages.widget(6), InboxScreen)

    def test_states_without_data(self):
        self.assertEqual(self.panel().kind, "empty")
        self.window.project_directory = DIRECTORY
        self.window._project_loading = True
        self.window.data_changed.emit("project")
        self.assertEqual(self.panel().kind, "loading")
        self.window._project_loading = False
        self.window.screen_errors = {}
        self.window.inbox_revision = None
        self.window.data_changed.emit("project")
        self.assertEqual(self.screen.list_state.itemAt(0).widget().kind, "loading")
        open_qa(self.window, inbox=False)
        self.window.load_inbox({"revision": 0, "entries": [], "pagination": {"total": 0}})
        panel = self.screen.list_state.itemAt(0).widget()
        self.assertEqual(panel.kind, "empty")
        self.assertFalse(self.screen.table.isVisibleTo(self.screen))

    def test_core_error_state_with_retry(self):
        open_qa(self.window)
        self.window.screen_errors = {"observer": "project is locked"}
        with patch.object(self.window, "refresh_project") as refresh:
            self.window.data_changed.emit("inbox")
            panel = self.screen.list_state.itemAt(0).widget()
            self.assertEqual(panel.kind, "error")
            panel.action.click()
        refresh.assert_called_once()

    def test_entries_filters_and_counts_come_from_the_core_rows(self):
        open_qa(self.window)
        self.wide()
        self.assertEqual(
            [r["id"] for r in self.screen.model.rows], [ACCEPTED_GOAL, REJECTED_NOTE]
        )  # newest first
        self.assertEqual(self.screen.model.index(0, 0).data(BADGE_ROLE), ("goal", ui("Цель")))
        self.assertEqual(self.screen.model.index(1, 0).data(BADGE_ROLE), ("info", ui("Заметка")))
        self.assertEqual(
            [self.screen.tabs.tabText(i) for i in range(4)],
            ["All · 2", "Unread · 2", "Goals · 1", "Processed · 2"],
        )
        self.screen.tabs.setCurrentIndex(2)
        self.assertEqual([r["id"] for r in self.screen.model.rows], [ACCEPTED_GOAL])
        self.screen.tabs.setCurrentIndex(3)
        self.assertEqual(
            len(self.screen.model.rows), 2
        )  # a rejected note and an accepted goal are both processed
        self.assertEqual(self.screen.status.text(), "Unread: 2")

    def test_processed_and_stage_rules(self):
        i18n.set_language("ru")  # Stage tuples contain source labels before UI translation.
        goal, note = fixture("inbox_list.json")["entries"][::-1]
        self.assertTrue(is_processed(goal) and is_processed(note))
        self.assertEqual([s[0] for s in stages(note)], [True, False, True, True])
        self.assertEqual(stages(note)[3][2], ui("Отклонено"))
        fresh = dict(note, triage=[])
        self.assertFalse(is_processed(fresh))
        self.assertEqual([s[0] for s in stages(fresh)], [True, False, False, False])
        self.assertEqual(stages(fresh)[3][2], ui("Не подтверждено"))

    def test_detail_shows_triage_with_actor_and_waits_for_the_agent_reply(self):
        open_qa(self.window)
        self.screen.table.selectRow(1)  # the rejected note
        detail = "\n".join(texts(self.screen.detail))
        self.assertIn("Проверь robots.txt после релиза", detail)
        self.assertIn("agent/local", detail)
        self.assertIn("Не относится к целям проекта", detail)
        self.assertIn(ui("Отклонено"), detail)
        self.assertIn(
            "in this core version", detail
        )  # the agent's text reply is not in the core yet
        self.assertIn(ui("Сохранено"), detail)

    def test_the_read_stage_is_never_claimed_without_a_receipt(self):
        open_qa(self.window)
        self.screen.table.selectRow(0)
        self.assertFalse(stages(self.screen.model.rows[0])[1][0])
        self.assertTrue(any("in this core version" in text for text in texts(self.screen.detail)))

    def test_question_is_disabled_with_the_core_issue(self):
        self.assertFalse(self.composer.question.isEnabled())
        self.assertIn("in this core version", self.composer.question.toolTip())
        self.assertFalse(self.composer.question.isCheckable())

    def test_typing_never_writes_only_the_button_does(self):
        open_qa(self.window)
        with patch.object(self.window, "start_command") as command:
            self.write("Проверить футер")
            self.assertTrue(self.composer.send.isEnabled())
            command.assert_not_called()
        with patch.object(self.window, "submit_note") as submit:
            self.composer.send.click()
        submit.assert_called_once_with("Проверить футер", "note", source="screen", references=())

    def test_submit_button_is_disabled_for_empty_long_or_unready_text(self):
        self.write("текст")  # no project open yet
        self.assertFalse(self.composer.send.isEnabled())
        open_qa(self.window)
        self.write("   ")
        self.assertFalse(self.composer.send.isEnabled())
        self.write("а" * (MAX_TEXT + 1))
        self.assertFalse(self.composer.send.isEnabled())
        self.assertEqual(self.composer.counter.text(), f"{MAX_TEXT + 1} / {MAX_TEXT}")
        self.write("а" * MAX_TEXT)
        self.assertTrue(self.composer.send.isEnabled())

    def test_the_core_gets_exactly_the_note_with_references_and_the_revision(self):
        open_qa(self.window)
        self.window.selected_scan_path = fixture("scan_row.json")["path"]
        self.composer._fill_menu()
        scan_action = next(a for a in self.composer.context_menu.actions() if a.isEnabled())
        scan_action.trigger()
        self.assertEqual(self.composer.references, ["scan:" + fixture("scan_row.json")["uuid"]])
        self.write("Футер ссылается на старые акции", "proposed_goal")
        with self.fake_core() as command:
            self.composer.send.click()
        _id, tool, arguments, _handler = command.call_args.args
        self.assertEqual(tool, "seo_project_inbox_submit")
        self.assertEqual(arguments["text"], "Футер ссылается на старые акции")
        self.assertEqual(arguments["kind"], "proposed_goal")
        self.assertEqual(arguments["references"], ["scan:" + fixture("scan_row.json")["uuid"]])
        self.assertEqual(arguments["expected_revision"], 4)
        self.assertEqual(arguments["author_role"], "specialist")
        self.assertFalse(self.composer.send.isEnabled())  # saving: no double write
        self.assertEqual(self.composer.send.text(), ui("Сохранение…"))

    def proposed_goal(self):
        open_qa(self.window)
        rows = list(self.window.inbox_model.rows)
        goal = dict(rows[1], goal_state="proposed", triage=[])
        self.window.inbox_model.replace([rows[0], goal])
        self.window.inbox_revision = 4
        self.window.core_executable = "seohead"
        self.screen.selected_id = goal["id"]
        self.screen._fill_rows()
        return goal

    def test_owner_goal_acceptance_uses_confirmed_local_cli_not_mcp(self):
        goal = self.proposed_goal()
        self.assertFalse(self.screen.accept_goal_button.isHidden())
        submitted = []

        def submit(_service, executable, arguments, _success, _error, _owner):
            submitted.append((executable, arguments))

        with (
            patch.object(ModalDialog, "exec_", return_value=QDialog.Accepted),
            patch.object(LocalOwnerActions, "submit", autospec=True, side_effect=submit),
            patch.object(self.window, "start_command") as mcp_call,
        ):
            self.screen._accept_goal()
        self.assertEqual(
            submitted,
            [("seohead", accept_goal_arguments(DIRECTORY, goal["id"], 4))],
        )
        mcp_call.assert_not_called()

    def test_owner_goal_acceptance_cancel_has_no_local_write(self):
        self.proposed_goal()
        with (
            patch.object(ModalDialog, "exec_", return_value=QDialog.Rejected),
            patch.object(LocalOwnerActions, "submit", autospec=True) as submit,
        ):
            self.screen._accept_goal()
        submit.assert_not_called()

    def test_refused_write_keeps_the_draft_and_says_why(self):
        open_qa(self.window)
        self.write("Не потерять эту заметку")
        with self.fake_core():
            self.composer.send.click()
        self.assertIsNotNone(self.window._pending_note)
        self.window.command_failed("inbox-submit", "revision conflict", self.window.read_generation)
        self.assertIsNone(self.window._pending_note)
        self.assertEqual(self.composer.draft()[0], "Не потерять эту заметку")
        self.assertTrue(self.composer.send.isEnabled())
        self.assertFalse(self.composer.error.isHidden())
        self.assertIn("revision conflict", self.composer.error_text.text())
        self.assertIn("The text stays in the form", self.composer.error_text.text())

    def test_confirmed_write_clears_only_an_unchanged_draft(self):
        open_qa(self.window)
        self.write("Записать")
        with self.fake_core(), patch.object(self.window, "refresh_project"):
            self.composer.send.click()
            pending = self.window._pending_note
            self.write("Новый текст, пока шла запись")
            self.window.note_saved({}, pending)
        self.assertEqual(self.composer.draft()[0], "Новый текст, пока шла запись")
        with self.fake_core(), patch.object(self.window, "refresh_project"):
            self.write("Ещё одна")
            self.composer.send.click()
            self.window.note_saved({}, self.window._pending_note)
        self.assertEqual(self.composer.draft()[0], "")

    def test_the_draft_follows_the_project_and_survives_a_switch(self):
        open_qa(self.window)
        self.write("черновик проекта A", "proposed_goal")
        self.window.stash_note_drafts()
        self.window.project_directory = "/project/other"
        self.window.current_project_uuid = "other"
        self.window.restore_note_drafts()
        self.assertEqual(self.composer.draft()[:2], ("", "note"))
        self.write("черновик проекта B")
        self.window.stash_note_drafts()
        self.window.project_directory = DIRECTORY
        self.window.current_project_uuid = fixture("project_open.json")["project"]["project_uuid"]
        self.window.restore_note_drafts()
        self.assertEqual(self.composer.draft()[:2], ("черновик проекта A", "proposed_goal"))

    def test_a_pending_write_blocks_closing_or_switching_the_tab(self):
        open_qa(self.window)
        self.write("Ждём подтверждения")
        with self.fake_core():
            self.composer.send.click()
        tab = self.window._active_workspace_id
        self.assertFalse(self.window.close_workspace_tab(tab))
        self.assertIn("перед закрытием вкладки", self.window.notice.message.text())

    def test_unread_counter_and_navigation_badge(self):
        open_qa(self.window)
        self.assertEqual(self.window.inbox_unread, 2)
        self.assertEqual(self.screen.status.text(), "Unread: 2")
        self.window.load_unread({})
        self.assertEqual(self.screen.status.text(), ui("Непрочитанные не измерены"))

    def test_context_menu_without_scan_or_task(self):
        self.composer._fill_menu()
        actions = self.composer.context_menu.actions()
        self.assertEqual(len(actions), 1)
        self.assertFalse(actions[0].isEnabled())

    def test_english_and_no_demo_text(self):
        open_qa(self.window)
        self.wide()
        self.assertNotIn("демо", "\n".join(texts(self.screen)).lower())
        i18n.set_language("en")
        self.assertEqual(self.composer.send.text(), "Save to the inbox")
        self.assertEqual(self.screen.tabs.tabText(1), "Unread · 2")
        self.assertEqual(self.screen.model.index(0, 0).data(BADGE_ROLE), ("goal", "Goal"))

    def test_narrow_window_has_no_horizontal_scrolling(self):
        open_qa(self.window)
        self.window.navigation.select_section("inbox")
        self.window.resize(800, 800)
        self.window.show()
        self.app.processEvents()
        self.assertEqual(self.screen.scroll.horizontalScrollBar().maximum(), 0)
        self.assertEqual(self.screen.table.horizontalScrollBar().maximum(), 0)
        self.assertTrue(self.screen.tabs.tabText(1).startswith(ui("Новые")))


if __name__ == "__main__":
    unittest.main()
