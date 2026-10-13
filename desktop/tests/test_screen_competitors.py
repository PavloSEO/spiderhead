"""Competitors screen: intake through the core, per-site cap, parallel submission through the owned scan queue."""

import os
import unittest
from unittest.mock import patch

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QObject, QTimer, pyqtSignal
from PyQt5.QtWidgets import QApplication, QDialog

from seohead_desktop import i18n
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.competitors import CompetitorsScreen
from seohead_desktop.ui.modal import ModalDialog
from tests._qt import sweep_widgets


class FakeManager:
    def __init__(self):
        self.submitted = []
        self.runs = []

    def snapshot(self, _uuid=None):
        return list(self.runs)

    def submit(self, **kwargs):
        if kwargs["project"].endswith("busy"):
            raise ValueError("refused by the core")
        self.submitted.append(kwargs)
        return "run-" + str(len(self.submitted))


class FakeHost(QObject):
    data_changed = pyqtSignal(str)

    def __init__(self, project="/owner", sites=()):
        super().__init__()
        self.project_directory = project
        self._project_loading = False
        self.screen_errors = {}
        self.observed_sites = list(sites)
        self.observed_policy = {"competitor_limit": 5}
        self.crawl_descriptor = {"capabilities": {}}
        self.current_project_uuid = "owner-uuid"
        self.core_executable = "/bin/seohead"
        self.scan_poll_timer = QTimer()
        self.manager = FakeManager()
        self.commands = []
        self.refreshed = 0

    def choose_project(self):
        self.commands.append(("choose_project",))

    def ensure_scan_manager(self):
        return self.manager

    def start_command(self, request_id, tool, arguments, handler):
        self.commands.append((request_id, tool, arguments, handler))

    def refresh_project(self):
        self.refreshed += 1


def _site(host, directory, scans=()):
    return {
        "role": "competitor",
        "site": {"target": f"https://{host}/"},
        "directory": directory,
        "project_uuid": "uuid-" + host,
        "candidate": {
            "state": "candidate; audit not run",
            "source": "desktop",
            "observed_at": None,
        },
        "scans": {"items": list(scans)},
    }


class CompetitorsScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")

    def tearDown(self):
        i18n.set_language("ru")
        sweep_widgets()

    def screen(self, host):
        return CompetitorsScreen(host)

    def test_the_project_menu_opens_the_competitors_screen(self):
        import shutil
        import tempfile

        from seohead_desktop.app import MainWindow, load_theme

        load_theme(self.app, "light")
        base = tempfile.TemporaryDirectory()
        self.addCleanup(base.cleanup)
        window = MainWindow(persistent=False, core_executable=shutil.which("python3"))
        try:
            window.prefs.set("general.projects_folder", base.name)
            actions = [
                action
                for menu in window.menuBar().actions()
                if menu.menu()
                for action in menu.menu().actions()
                if action.text() == "Competitors…"
            ]
            self.assertEqual(len(actions), 1)
            actions[0].trigger()
            self.app.processEvents()
            self.assertIs(window.pages.currentWidget(), window.extra_screens["competitors"])
        finally:
            window.close()
            window.deleteLater()
            self.app.processEvents()

    def test_without_project_shows_no_project_state_and_no_commands(self):
        host = FakeHost(project=None)
        screen = self.screen(host)

        self.assertEqual(screen.empty_holder.count(), 1)
        screen.add_typed()
        self.assertEqual(host.commands, [])

    def test_rows_show_finished_scan_state_from_observer_snapshot(self):
        done = {
            "lifecycle": "finished",
            "crawl_partial": False,
            "finished_at": "2026-10-11T10:00:00Z",
        }
        host = FakeHost(
            sites=[
                _site("one.example.test", "competitors/a", [done]),
                _site("two.example.test", "competitors/b"),
            ]
        )

        screen = self.screen(host)

        rows = screen.model.rows
        self.assertEqual(
            [row["state"] for row in rows], ["просканирован", "кандидат, аудит не запущен"]
        )
        self.assertEqual(screen.table.model().rowCount(), 2)

    def test_typed_entry_is_sent_to_the_core_with_provenance_and_known_urls_refused(self):
        host = FakeHost(sites=[_site("one.example.test", "competitors/a")])
        screen = self.screen(host)
        screen.entry.setText("one.example.test two.example.test")

        screen.add_typed()

        request_id, tool, arguments, handler = host.commands[-1]
        self.assertEqual((request_id, tool), ("competitors-add", "seo_project_competitors_add"))
        self.assertEqual(arguments["directory"], "/owner")
        self.assertEqual(
            [item["url"] for item in arguments["competitors"]], ["https://two.example.test"]
        )
        self.assertEqual(arguments["competitors"][0]["source"], "desktop manual entry")
        self.assertIn("уже в списке", screen.status.text())
        handler({"ok": True, "competitors": [{}, {}]})
        self.assertEqual(host.refreshed, 1)
        self.assertIn("2", screen.status.text())

    def test_clipboard_paste_uses_clipboard_text_and_clipboard_provenance(self):
        host = FakeHost()
        screen = self.screen(host)
        clipboard = QApplication.clipboard()
        clipboard.setText("https://a.example.test\nb.example.test")
        self.addCleanup(clipboard.clear)

        screen.add_from_clipboard()

        arguments = host.commands[-1][2]
        self.assertEqual(len(arguments["competitors"]), 2)
        self.assertEqual(arguments["competitors"][0]["source"], "desktop clipboard paste")

    def test_nothing_is_sent_when_no_address_is_pasted(self):
        host = FakeHost()
        screen = self.screen(host)
        screen.entry.setText("   ")

        screen.add_typed()

        self.assertEqual(host.commands, [])
        self.assertIn("В тексте нет адресов", screen.status.text())

    def test_scans_start_each_competitor_with_the_cap_and_skip_busy_or_unlocated_ones(self):
        host = FakeHost(
            sites=[
                _site("one.example.test", "competitors/a"),
                _site("busy.example.test", "competitors/busy"),
                {**_site("nodir.example.test", None)},
            ]
        )
        screen = self.screen(host)
        screen.cap.setValue(120)
        preview = {"overrides": {"limits.max_urls": 120, "rendering.mode": "raw"}}

        with patch(
            "seohead_desktop.screens.competitors.preview_configuration", return_value=preview
        ) as configured:
            screen.start_scans()

        self.assertEqual(configured.call_args.args[1]["limits.max_urls"], 120)
        self.assertEqual(configured.call_args.args[1]["limits.max_requests"], 360)
        self.assertEqual(configured.call_args.args[1]["limits.max_crawl_seconds"], 600)
        self.assertEqual(len(host.manager.submitted), 1)
        submitted = host.manager.submitted[0]
        self.assertTrue(submitted["project"].endswith("/owner/competitors/a"))
        self.assertEqual(submitted["max_urls"], 120)
        self.assertEqual(submitted["project_uuid"], "uuid-one.example.test")
        self.assertFalse(submitted["approve_large_crawl"])
        self.assertIn("не запущено: 2", screen.status.text())

    def test_busy_run_for_a_competitor_prevents_a_second_concurrent_crawl(self):
        host = FakeHost(sites=[_site("one.example.test", "competitors/a")])
        host.manager.runs = [{"project": "/owner/competitors/a", "state": "running"}]
        screen = self.screen(host)

        with patch(
            "seohead_desktop.screens.competitors.preview_configuration",
            return_value={"overrides": {}},
        ):
            screen.start_scans()

        self.assertEqual(host.manager.submitted, [])
        self.assertIn("уже идут: 1", screen.status.text())

    def test_removal_is_confirmed_and_only_removes_the_shortlist_entry(self):
        host = FakeHost(sites=[_site("one.example.test", "competitors/a")])
        screen = self.screen(host)
        screen.table.selectRow(0)
        with patch.object(ModalDialog, "exec_", return_value=QDialog.Accepted) as confirm:
            screen.remove_selected()
        confirm.assert_called_once()
        request_id, tool, arguments, handler = host.commands[-1]
        self.assertEqual(
            (request_id, tool), ("competitors-remove", "seo_project_competitors_remove")
        )
        self.assertEqual(arguments, {"directory": "/owner", "url": "https://one.example.test/"})
        handler(
            {"ok": True, "removed": "https://one.example.test/", "workspace_kept": "competitors/a"}
        )
        self.assertEqual(host.refreshed, 1)
        self.assertIn("данные сохранены", screen.status.text())

    def test_removal_cancel_does_not_write(self):
        host = FakeHost(sites=[_site("one.example.test", "competitors/a")])
        screen = self.screen(host)
        screen.table.selectRow(0)
        with patch.object(ModalDialog, "exec_", return_value=QDialog.Rejected):
            screen.remove_selected()
        self.assertEqual(host.commands, [])


if __name__ == "__main__":
    unittest.main()
