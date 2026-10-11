"""Consent and background-only CLI contracts with fake core answers (no CLI, no real configuration)."""
import os
import threading
import time
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QLabel, QPushButton

from seohead_desktop.mcp_integration import IntegrationPanel, PermissionDialog
from seohead_desktop.qt import app as qt_app
from seohead_desktop.settings_store import AppSettings
from seohead_desktop.ui.controls import Switch
from seohead_desktop.ui.settings import full_schema

APP = qt_app()
HOME = os.path.expanduser("~")
STATUS = {"ok": True, "enabled": True, "profile": "full", "tools": 3, "by": "CLI", "by_label": "CLI · сегодня 10:00",
          "profiles": [{"id": "full", "label": "все", "tools": 3}, {"id": "audit", "label": "аудит", "tools": 2}],
          "clients": {"claude-code": {"registered": False, "file": HOME + "/.claude.json"},
                      "codex": {"registered": True, "file": HOME + "/.codex/config.toml"},
                      "cursor": {"registered": None, "file": HOME + "/.cursor/mcp.json"}}}
PLAN = {"ok": True, "file": HOME + "/.claude.json", "block": '{\n  "mcpServers": {"seohead": {}}\n}', "backup": HOME + "/.claude.json.seohead-x.bak",
        "sha256": "abc", "changed": True}


def wait_for(predicate):
    deadline = time.monotonic() + 4
    while time.monotonic() < deadline and not predicate():
        APP.processEvents()
        time.sleep(0.005)
    assert predicate()


def fake_core(calls, threads, status=STATUS, backups=()):
    def runner(_exe, arguments):
        calls.append(arguments)
        threads.append(threading.get_ident())
        if arguments[0] == "status":
            return dict(status)
        if arguments[0] == "backups":
            return {"ok": True, "backups": list(backups)}
        return dict(PLAN)
    return runner


class PermissionTests(unittest.TestCase):
    def test_plan_is_shown_as_added_lines_with_tilde_paths_and_cancel_writes_nothing(self):
        calls, threads = [], []
        dialog = PermissionDialog("/opt/seohead", ("claude-code",), runner=fake_core(calls, threads))
        wait_for(lambda: "claude-code" in dialog.plans and not dialog.busy)
        text = dialog.preview.text()
        self.assertIn('+   "mcpServers"', text)
        self.assertIn("~/.claude.json", text)
        self.assertNotIn(HOME, text + dialog.backup_label.text())
        self.assertFalse(dialog.boxes["cursor"].isEnabled())  # unreadable configuration
        dialog.reject()
        self.assertFalse([c for c in calls if "--yes" in c])
        self.assertTrue(all(t != threading.get_ident() for t in threads))

    def test_allow_writes_every_checked_client_with_the_reviewed_hash(self):
        calls, threads = [], []
        dialog = PermissionDialog("/opt/seohead", ("claude-code",), runner=fake_core(calls, threads))
        wait_for(lambda: "claude-code" in dialog.plans and not dialog.busy)
        dialog.boxes["codex"].setChecked(True)
        wait_for(lambda: "codex" in dialog.plans and not dialog.busy)
        dialog.allow.click()
        wait_for(lambda: dialog.allow.isHidden())
        writes = [c for c in calls if "--yes" in c]
        self.assertEqual([c[2] for c in writes], ["claude-code", "codex"])
        self.assertTrue(all("--expected-sha256" in c for c in writes))
        dialog.close()

    def test_error_never_writes(self):
        calls = []

        def runner(_exe, arguments):
            calls.append(arguments)
            raise ValueError("Core unavailable")

        dialog = PermissionDialog("/opt/seohead", ("cursor",), runner=runner)
        wait_for(lambda: not dialog.busy)
        self.assertFalse(dialog.allow.isEnabled())
        self.assertIn("Core unavailable", dialog.result_label.text())
        self.assertEqual(len(calls), 1)
        dialog.close()


class PanelTests(unittest.TestCase):
    def panel(self, calls, **kwargs):
        panel = IntegrationPanel("/opt/seohead", AppSettings(schema=full_schema()), runner=fake_core(calls, [], **kwargs), polling=False)
        panel.show()
        wait_for(lambda: panel.state is not None and not panel.busy)
        return panel

    def test_sheet_from_core_status_without_service_lines(self):
        calls = []
        panel = self.panel(calls)
        texts = " | ".join(label.text() for label in panel.findChildren(QLabel))
        self.assertIn("CLI · сегодня 10:00", texts)
        self.assertIn("Бэкапов ещё нет", texts)
        self.assertNotIn("mcp-state", texts)
        self.assertNotIn(HOME, texts)
        buttons = sorted(b.text() for b in panel.findChildren(QPushButton) if b.isEnabled() and b.text().endswith("…"))
        self.assertEqual(buttons, ["Изменить…", "Прописать…"])
        panel.close()

    def test_switch_goes_through_the_core(self):
        calls = []
        panel = self.panel(calls)
        switch = panel.findChildren(Switch)[0]
        switch.click()
        wait_for(lambda: not panel.busy)
        self.assertIn(["disable", "--actor", "SEOHEAD Desktop"], calls)
        panel.close()

    def test_only_the_newest_verified_backup_of_a_client_is_restorable(self):
        backups = [{"client": "codex", "path": HOME + "/.codex/config.toml.seohead-2.bak", "created_at": "2026-10-09T10:00:00+00:00", "reason": "install", "verified": True},
                   {"client": "codex", "path": HOME + "/.codex/config.toml.seohead-1.bak", "created_at": "2026-10-08T10:00:00+00:00", "reason": "install", "verified": True}]
        panel = self.panel([], backups=backups)
        restore = [b for b in panel.findChildren(QPushButton) if b.text() == "Восстановить"]
        self.assertEqual([b.isEnabled() for b in restore], [True, False])
        panel.close()


if __name__ == "__main__":
    unittest.main()
