"""Qt interaction and contained profile gates for the actual settings dialog."""

import json
import os
import tempfile
import unittest
from copy import deepcopy
from pathlib import Path

from PyQt5.QtCore import Qt
from PyQt5.QtTest import QSignalSpy, QTest
from PyQt5.QtWidgets import QDialog

from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.crawl_configuration_dialog import (
    MAX_PROFILE_BYTES,
    PROFILE_SCHEMA,
    CrawlConfigurationDialog,
)


def descriptor():
    values = {
        "scope.internal": "host",
        "scope.include_patterns": [],
        "scope.exclude_patterns": [],
        "limits.max_urls": 200,
        "limits.max_depth": 5,
        "limits.max_requests": 20000,
        "limits.max_crawl_seconds": 0,
        "speed.adaptive": True,
        "speed.min_delay_seconds": 0.5,
        "speed.max_delay_seconds": 60.0,
        "cache.mode": "off",
        "cache.invalidate": False,
        "rendering.mode": "raw",
        "rendering.browser.viewport_width": 0,
        "rendering.browser.viewport_height": 0,
        "http.user_agent": "",
        "http.headers": {},
        "http.proxy": "",
    }
    return {
        "settings": [
            {
                "path": path,
                "type": type(value).__name__,
                "default": value,
                "description": "Core descriptor fixture",
                "results_affecting": True,
            }
            for path, value in values.items()
        ]
    }


class ConfigurationDialogTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        cls.app.setStyle("Fusion")

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="seohead-config-dialog-")
        self.root = Path(self.temporary.name) / "project"
        self.root.mkdir()
        (self.root / "project.json").write_text("{}")
        self.dialogs = []

    def tearDown(self):
        for dialog in self.dialogs:
            dialog.close()
            dialog.deleteLater()
        self.app.processEvents()
        self.temporary.cleanup()

    def dialog(self, **kwargs):
        kwargs.setdefault("project_directory", self.root)
        dialog = CrawlConfigurationDialog(descriptor(), **kwargs)
        self.dialogs.append(dialog)
        return dialog

    def profile(self, name="fixture.json", overrides=None, **extra):
        directory = self.root / "profiles"
        directory.mkdir(exist_ok=True)
        payload = {
            "schema": PROFILE_SCHEMA,
            "input_mode": "site",
            "storage_backend": "sqlite",
            "overrides": overrides or {"limits.max_urls": 50},
            **extra,
        }
        path = directory / name
        path.write_text(json.dumps(payload))
        return path

    def test_open_cancel_and_apply_leave_caller_defaults_unchanged(self):
        payload = descriptor()
        original = deepcopy(payload)
        overrides = {"limits.max_urls": 40}
        dialog = CrawlConfigurationDialog(payload, overrides=overrides)
        self.dialogs.append(dialog)
        dialog.select_path("limits.max_urls")
        dialog.int_editor.setText("70")
        dialog.stage_current()
        self.assertEqual(dialog.get_overrides(), overrides)
        dialog.reject()
        self.assertEqual(dialog.result(), QDialog.Rejected)
        self.assertEqual(overrides, {"limits.max_urls": 40})
        self.assertEqual(payload, original)

    def test_pending_integer_edit_is_validated_and_returned_only_on_apply(self):
        dialog = self.dialog()
        seen = QSignalSpy(dialog.overrides_accepted)
        native_seen = QSignalSpy(dialog.accepted)
        dialog.select_path("limits.max_urls")
        dialog.int_editor.setText("99")
        self.assertEqual(dialog.get_overrides(), {})
        dialog.apply_button.click()
        self.assertEqual(dialog.result(), QDialog.Accepted)
        self.assertEqual(dialog.get_overrides(), {"limits.max_urls": 99})
        self.assertEqual(len(seen), 1)
        self.assertEqual(len(native_seen), 1)
        result = dialog.get_overrides()
        result["limits.max_urls"] = 10
        self.assertEqual(dialog.get_overrides()["limits.max_urls"], 99)

    def test_boolean_choice_float_and_string_editors_keep_json_types(self):
        dialog = self.dialog()
        dialog.select_path("speed.adaptive")
        dialog.bool_editor.setChecked(False)
        dialog.stage_current()
        dialog.select_path("cache.mode")
        dialog.choice_editor.setCurrentText("live")
        dialog.stage_current()
        dialog.select_path("speed.min_delay_seconds")
        dialog.float_editor.setText("2,5")
        dialog.stage_current()
        dialog.select_path("http.user_agent")
        dialog.text_editor.setText("SEOHEAD fixture")
        dialog.apply_button.click()
        self.assertEqual(
            dialog.get_overrides(),
            {
                "speed.adaptive": False,
                "cache.mode": "live",
                "speed.min_delay_seconds": 2.5,
                "http.user_agent": "SEOHEAD fixture",
            },
        )

    def test_multiline_regex_and_char_classes_are_lists_not_json_syntax(self):
        dialog = self.dialog()
        dialog.select_path("scope.include_patterns")
        dialog.list_editor.setPlainText("[a-z]\n/catalog/\n")
        dialog.stage_current()
        self.assertIn("Изменение в черновике", dialog.availability.text())
        dialog.apply_button.click()
        self.assertEqual(
            dialog.get_overrides(), {"scope.include_patterns": ["[a-z]", "/catalog/"]}
        )

    def test_invalid_regex_and_numeric_values_do_not_apply(self):
        dialog = self.dialog()
        dialog.select_path("scope.exclude_patterns")
        dialog.list_editor.setPlainText("[")
        dialog.stage_current()
        self.assertFalse(dialog.apply_button.isEnabled())
        self.assertIn("regex", dialog.feedback.text())
        dialog.accept()
        self.assertEqual(dialog.result(), QDialog.Rejected)
        self.assertEqual(dialog.get_overrides(), {})
        dialog.override_table.selectRow(0)
        dialog.remove_selected()
        dialog.select_path("limits.max_urls")
        dialog.int_editor.setText("1000001")
        self.assertFalse(dialog.apply_button.isEnabled())
        dialog.int_editor.setText("1.5")
        self.assertFalse(dialog.apply_button.isEnabled())

    def test_cross_field_draft_can_be_completed_before_apply(self):
        dialog = self.dialog()
        dialog.select_path("rendering.browser.viewport_width")
        dialog.int_editor.setText("390")
        self.assertTrue(dialog.stage_current())
        self.assertFalse(dialog.apply_button.isEnabled())
        dialog.select_path("rendering.browser.viewport_height")
        dialog.int_editor.setText("844")
        dialog.stage_current()
        self.assertTrue(dialog.apply_button.isEnabled())
        dialog.accept()
        self.assertEqual(
            dialog.get_overrides(),
            {
                "rendering.browser.viewport_width": 390,
                "rendering.browser.viewport_height": 844,
            },
        )

    def test_readonly_field_is_selectable_with_reason_but_cannot_be_staged(self):
        dialog = self.dialog()
        dialog.select_path("http.proxy")
        self.assertFalse(dialog.editor_stack.isEnabled())
        self.assertFalse(dialog.stage_button.isEnabled())
        self.assertIn("Недоступно", dialog.availability.text())
        self.assertFalse(dialog.stage_current())
        self.assertEqual(dialog.get_overrides(), {})

    def test_group_search_and_keyboard_selection_do_not_create_overrides(self):
        dialog = self.dialog()
        dialog.show()
        self.app.processEvents()
        dialog.group.setCurrentIndex(dialog.group.findData("scope"))
        self.assertEqual(dialog.path.count(), 3)
        dialog.search.setText("exclude")
        self.assertEqual(dialog.path.currentData(), "scope.exclude_patterns")
        self.assertEqual(dialog.path.count(), 1)
        dialog.path.setFocus()
        QTest.keyClick(dialog.path, Qt.Key_Tab)
        self.assertIs(dialog.focusWidget(), dialog.list_editor)
        self.assertEqual(dialog.get_overrides(), {})

    def test_save_and_load_profile_are_explicit_and_do_not_apply(self):
        dialog = self.dialog(overrides={"limits.max_urls": 40})
        self.assertFalse((self.root / "profiles").exists())
        dialog.select_path("scope.exclude_patterns")
        dialog.list_editor.setPlainText("/private/\n/logout/")
        saved = dialog.save_profile("bounded.json")
        payload = json.loads(saved.read_text())
        self.assertEqual(payload["schema"], PROFILE_SCHEMA)
        self.assertEqual(
            payload["overrides"],
            {
                "limits.max_urls": 40,
                "scope.exclude_patterns": ["/private/", "/logout/"],
            },
        )
        self.assertEqual(dialog.get_overrides(), {"limits.max_urls": 40})
        self.assertEqual(dialog.override_model.rowCount(), 2)
        if os.name != "nt":
            self.assertEqual(saved.stat().st_mode & 0o777, 0o600)
        other = self.dialog()
        other.load_profile(saved)
        self.assertEqual(other.get_overrides(), {})
        other.accept()
        self.assertEqual(other.get_overrides(), payload["overrides"])

    def test_profile_overwrite_traversal_and_symlink_are_refused(self):
        dialog = self.dialog(overrides={"limits.max_urls": 40})
        saved = dialog.save_profile("bounded.json")
        original = saved.read_bytes()
        with self.assertRaises(ValueError):
            dialog.save_profile("bounded.json")
        self.assertEqual(saved.read_bytes(), original)
        for path in (self.root / "escape.json", "../escape.json", "other.txt"):
            with self.subTest(path=str(path)), self.assertRaises(ValueError):
                dialog.save_profile(path)
        if hasattr(os, "symlink"):
            link = self.root / "profiles" / "linked.json"
            try:
                link.symlink_to(saved)
            except OSError:
                return
            with self.assertRaises(ValueError):
                dialog.load_profile(link)

    def test_symlinked_profile_directory_cannot_escape_selected_project(self):
        outside = Path(self.temporary.name) / "outside"
        outside.mkdir()
        try:
            (self.root / "profiles").symlink_to(outside, target_is_directory=True)
        except OSError:
            self.skipTest("Symlinks unavailable on this platform")
        dialog = self.dialog()
        with self.assertRaises(ValueError):
            dialog.save_profile("escape.json")
        self.assertFalse((outside / "escape.json").exists())

    def test_untrusted_profiles_fail_before_changing_draft(self):
        dialog = self.dialog(overrides={"limits.max_urls": 40})
        profiles = [
            self.profile("unknown.json", {"unknown.setting": 1}),
            self.profile("secret.json", {"http.proxy": "synthetic-secret-marker"}),
            self.profile("wrong-version.json", schema="other.v1"),
            self.profile("wrong-mode.json", input_mode="list"),
            self.profile("extra.json", extra_key="refused"),
        ]
        for path in profiles:
            with self.subTest(path=path.name), self.assertRaises(ValueError) as caught:
                dialog.load_profile(path)
            self.assertNotIn("synthetic-secret-marker", str(caught.exception))
            self.assertEqual(dialog.get_overrides(), {"limits.max_urls": 40})
        dialog.accept()
        self.assertEqual(dialog.get_overrides(), {"limits.max_urls": 40})

    def test_profile_size_duplicate_keys_and_missing_project_are_rejected(self):
        path = self.profile()
        path.write_bytes(b" " * (MAX_PROFILE_BYTES + 1))
        dialog = self.dialog()
        with self.assertRaises(ValueError):
            dialog.load_profile(path)
        path.write_text('{"schema":"one","schema":"two"}')
        with self.assertRaises(ValueError):
            dialog.load_profile(path)
        missing = self.dialog(project_directory=None)
        self.assertFalse(missing.save_button.isEnabled())
        with self.assertRaises(ValueError):
            missing.save_profile("not-allowed.json")


if __name__ == "__main__":
    unittest.main()
