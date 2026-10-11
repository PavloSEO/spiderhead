"""Regressions from observed user journeys, not synthetic success projections."""

import tempfile
import unittest
from pathlib import Path

from PyQt5.QtCore import QTimer
from PyQt5.QtWidgets import QCheckBox, QLabel, QLineEdit, QPushButton, QToolButton

from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from tests.test_crawl_configuration import descriptor


class _OwnedManager:
    active_count = 0

    def __init__(self, project):
        self.rows = [{"id": key, "project": project, "project_uuid": "p", "observer_run_id": "core-" + key, "core_run_id": "core-" + key, "kind": "crawl", "state": "running", "output": "preserved " + key} for key in ("A", "B")]
        self.stopped = []
        self.resumed = []

    def snapshot(self, _project=None):
        return list(self.rows)

    def detail(self, identifier):
        return next((dict(row) for row in self.rows if row["id"] == identifier), None)

    def stop(self, identifier):
        self.stopped.append(identifier)
        return True

    def resume(self, **arguments):
        self.resumed.append(arguments)
        self.rows.append({"id": "R", "project": arguments["project"], "project_uuid": "p", "observer_run_id": "core-R", "state": "queued", "resume_path": arguments["artifact"], "kind": "resume"})
        return "R"

    def observe(self, *_args):
        pass


class UXSafetyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app)
        cls.scratch = Path(__file__).parents[1] / ".build/scratch"
        cls.scratch.mkdir(parents=True, exist_ok=True)

    def setUp(self):
        self.window = MainWindow(persistent=False, core_executable="")
        self.window.start_command = lambda *_args: None
        self.window.show()
        self.app.processEvents()
        self.addCleanup(self.window.close)

    def open_fake_project(self, name):
        self.window.refresh_project = lambda: None
        self.window.load_crawl_descriptor = lambda: None
        self.window.project_loaded({"path": "/" + name, "project": {"project_uuid": name, "site": {"host": name}}}, self.window.read_generation)

    def test_both_inbox_drafts_are_scoped_and_previous_detail_is_cleared(self):
        self.open_fake_project("A")
        self.window.load_inbox({"revision": 4, "entries": [{"id": "old-A", "text": "only A", "kind": "note"}]})
        panel = self.window.project_panels.panel("inbox")
        self.window.note_input.setText("main draft A")
        panel.note.setPlainText("panel draft A")
        self.open_fake_project("B")
        self.assertEqual(self.window.note_input.text(), "")
        self.assertEqual(panel.note.toPlainText(), "")
        self.assertNotIn("only A", self.window.inbox_detail.toPlainText())
        self.assertIsNone(self.window.inbox_revision)
        self.assertFalse(self.window.note_submit.isEnabled())
        self.window.load_inbox({"revision": 7, "entries": []})
        self.window.note_input.setText("main draft B")
        self.open_fake_project("A")
        self.assertEqual(self.window.note_input.text(), "main draft A")
        self.assertEqual(panel.note.toPlainText(), "panel draft A")
        self.assertFalse(self.window.note_submit.isEnabled())

    def test_unsupported_question_is_not_coerced_or_dispatched(self):
        self.open_fake_project("A")
        self.window.load_inbox({"revision": 1, "entries": []})
        calls = []
        self.window.start_command = lambda *args: calls.append(args)
        self.window.note_input.setText("keep question draft")
        self.window.submit_note("question text", "question")
        self.assertFalse(calls)
        self.assertEqual(self.window.note_input.text(), "keep question draft")
        panel = self.window.project_panels.panel("inbox")
        self.assertFalse(panel.kind.model().item(panel.kind.findData("question")).isEnabled())

    def test_note_receipt_only_clears_the_submitted_unchanged_form(self):
        self.open_fake_project("A")
        self.window.load_inbox({"revision": 1, "entries": []})
        panel = self.window.project_panels.panel("inbox")
        self.window.note_input.setText("sent")
        panel.note.setPlainText("other unsent draft")
        pending = {"key": self.window.note_project_key(), "source": "main", "text": "sent", "kind": "note"}
        self.window._pending_note = pending
        self.window.read_project("/not-opened-while-pending")
        self.assertEqual(self.window.notice.context, "inbox-submit")
        self.window.note_saved({}, pending)
        self.assertTrue(self.window.notice.isHidden())
        self.assertEqual(self.window.note_input.text(), "")
        self.assertEqual(panel.note.toPlainText(), "other unsent draft")
        self.window.note_input.setText("newly edited text")
        self.window.note_saved({}, pending)
        self.assertEqual(self.window.note_input.text(), "newly edited text")

    def test_project_panel_is_told_only_after_the_core_stored_the_note(self):
        self.open_fake_project("A")
        self.window.load_inbox({"revision": 1, "entries": []})
        panel = self.window.project_panels.panel("inbox")
        panel.note.setPlainText("sent from the panel")
        pending = {"key": self.window.note_project_key(), "source": "project", "text": "sent from the panel", "kind": "note"}
        self.window._pending_note = pending
        self.assertEqual(panel.note.toPlainText(), "sent from the panel")  # nothing cleared while the write is pending
        self.window.note_saved({}, pending)
        self.assertEqual(panel.note.toPlainText(), "")
        self.assertEqual(panel.note_status.text(), "Заметка сохранена ядром")

    def test_selected_observed_owned_run_is_the_exact_stop_target(self):
        self.window.project_directory = "/owned-project"
        self.window.current_project_uuid = "p"
        manager = self.window.scan_manager = _OwnedManager(self.window.project_directory)
        self.window.managed_scan_changed(manager.rows[-1])
        self.window.choose_owned_run("B")
        self.window.present_observed_runs([{"id": "core-A", "state": "running"}, {"id": "core-B", "state": "running"}, {"id": "external", "state": "running"}], None)
        self.window.show_observed_run(self.window.activity_model.index(0, 0), None)
        self.window.cancel_active_work()
        self.assertEqual(manager.stopped, ["A"])
        self.assertIn("A", self.window.cancel_button.toolTip())
        self.assertIn("A", self.window.owned_target_caption.text())
        self.window.managed_scan_changed(manager.rows[-1])
        self.assertEqual(self.window.selected_managed_run_id, "A")
        self.window.show_observed_run(self.window.activity_model.index(2, 0), None)
        self.assertIsNone(self.window.selected_managed_run_id)
        self.assertFalse(self.window.stop_run_button.isEnabled())
        self.window.cancel_active_work()
        self.assertEqual(manager.stopped, ["A"])

    def test_active_resume_for_same_artifact_cannot_be_queued_twice(self):
        self.window.project_directory = "/owned-project"
        self.window.current_project_uuid = "p"
        self.window.selected_scan_path = "/owned-project/scans/interrupted.sqlite"
        self.window._resume_eligible_path = self.window.selected_scan_path
        manager = self.window.scan_manager = _OwnedManager(self.window.project_directory)
        self.window.resume_selected_scan()
        self.window.resume_selected_scan()
        self.assertEqual(len(manager.resumed), 1)
        self.assertFalse(self.window.resume_scan_button.isEnabled())
        self.window.scan_poll_timer.stop()

    def test_manual_panel_and_navigation_intent_survive_resize(self):
        self.window.apply_layout("table")
        for width in (1440, 1024, 1440):
            self.window.resize(width, 900)
            self.app.processEvents()
            self.assertTrue(self.window.overview.isHidden())
            self.assertTrue(self.window.inspector.isHidden())
        self.window.set_reduced_motion(True)
        self.window.toggle_navigation()
        wanted = self.window.navigation.property("compact")
        for width in (1024, 1440):
            self.window.resize(width, 900)
            self.app.processEvents()
        self.assertEqual(self.window.navigation.property("compact"), wanted)

    def test_plan_descriptor_arrival_and_invalid_fragment_preserve_open_draft(self):
        with tempfile.TemporaryDirectory(dir=self.scratch) as directory:
            (Path(directory) / "project.json").write_text("{}")
            self.window.project_directory = directory
            self.window.current_project_uuid = "p"
            self.window.load_crawl_descriptor = lambda: None
            self.window.crawl_descriptor = None
            errors = []
            def inspect():
                dialog = self.app.activeModalWidget()
                try:
                    start = dialog.findChild(QPushButton, "scanStartButton")
                    self.assertFalse(start.isEnabled())
                    self.window.crawl_descriptor_loaded({**descriptor(), "capabilities": {"sitemap_only_retained": True, "full_site_native_sqlite": True}})
                    approval = dialog.findChild(QCheckBox, "scanLargeApproval")
                    approval.click()
                    self.assertTrue(start.isEnabled())
                    dialog.findChild(QToolButton, "scanSource_sitemap").click()
                    field = dialog.findChild(QLineEdit, "scanSitemapUrl")
                    for value in ("https://fixture.test/sitemap.xml#bad", "https://fixture.test:wrong/sitemap.xml", "https://fixture.test/" + "a" * 4096):
                        field.setText(value)
                        field.textEdited.emit(value)
                        approval.setChecked(True)
                        self.assertFalse(start.isEnabled())
                        self.assertTrue(dialog.isVisible())
                        self.assertEqual(field.text(), value)
                    field.setText("https://fixture.test/sitemap.xml")
                    field.textEdited.emit("https://fixture.test/sitemap.xml")
                    approval.setChecked(True)
                    self.assertTrue(start.isEnabled())
                    self.assertEqual(dialog.findChild(QLabel, "scanValidationFeedback").text(), "")
                except Exception as exc:
                    errors.append(exc)
                finally:
                    dialog.reject()
            QTimer.singleShot(20, inspect)
            self.window.scan_preview()
            if errors:
                raise errors[0]
            self.assertEqual(self.window.receivers(self.window.crawl_descriptor_changed), 0)
            self.assertIsNone(self.window.scan_manager)


if __name__ == "__main__":
    unittest.main()
