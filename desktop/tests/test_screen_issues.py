import importlib.util
import os
import re
import unittest
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QLabel, QPushButton

from seohead_desktop import i18n
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.base import SLOTS
from seohead_desktop.screens.issues import (
    IssuesScreen,
    check_name,
    message_ru,
    sample_checks,
    skip_reason,
)
from seohead_desktop.ui.kit import StatePanel
from seohead_desktop.ui.workspace import VIEW_ALIASES, VIEW_IDS
from tests._qt import sweep_widgets
from tests._screens_core import fixture
from tests._screens_host import FakeHost


class IssuesBase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.host = FakeHost()
        self.host.opened = []
        self.host.open_url_filtered = lambda label, filters: self.host.opened.append((label, filters))
        self.host.resume_selected_scan = lambda: None
        self.scan = fixture("scan_row_shop_findings.json")
        self.host.scan_model.rows = [self.scan]
        self.host.selected_scan_path = self.scan["path"]
        self.screen = IssuesScreen(self.host)
        self.screen.resize(1440, 800)
        self.screen.show()
        self.app.processEvents()

    def tearDown(self):
        self.screen.close()
        self.screen.deleteLater()

    def texts(self):
        return [w.text() for w in self.screen.findChildren(QLabel)] + [b.text() for b in self.screen.findChildren(QPushButton)]


class IssuesTests(IssuesBase):
    def test_severity_totals_come_from_the_core_row_not_from_the_sample(self):
        findings = self.scan["evidence"]["findings"]
        self.assertTrue(findings["truncated"])
        pill, _label = self.screen.pills["critical"]
        self.assertIn(str(findings["by_severity"]["critical"]), pill.text())
        self.assertIn("4 272", self.screen.pills["all"][0].text().replace("\u202f", " ").replace("\xa0", " "))
        self.assertEqual(self.screen.list_waiting.property("waiting_issue"), 981)

    def test_list_holds_only_checks_of_the_sample_and_filters_by_severity(self):
        checks = sample_checks(self.scan["evidence"]["findings"])
        self.assertEqual(set(self.screen.row_buttons), set(checks))
        self.screen.pills["critical"][0].click()
        self.assertTrue(all(checks[c]["severity"] == "critical" for c in self.screen.row_buttons))

    def test_check_detail_lists_its_sampled_urls_and_opens_them_in_url(self):
        check = next(iter(self.screen.row_buttons))
        self.screen.row_buttons[check].click()
        entry = self.screen.checks[check]
        self.assertEqual(self.screen.model.rowCount(), len(entry["items"]))
        self.assertTrue(self.screen.open_url.isEnabled())
        self.screen.open_url.click()
        _label, filters = self.host.opened[-1]
        self.assertEqual(filters[0]["column"], "url")
        self.assertEqual(filters[0]["op"], "in")
        self.assertEqual(filters[0]["value"], [i["target_url"] for i in entry["items"]])
        self.assertFalse(self.screen.to_task.isEnabled())

    def test_skipped_checks_are_collapsed_russian_and_never_raw(self):
        skipped = self.scan["evidence"]["skipped_checks"]
        self.assertFalse(self.screen.skipped_toggle.isChecked())
        self.assertTrue(self.screen.skipped.isHidden())
        self.assertEqual(self.screen.skipped_toggle.text(), f"Не выполнялось: {len(skipped)}")
        self.screen.skipped_toggle.click()
        self.app.processEvents()
        self.assertFalse(self.screen.skipped.isHidden())
        text = self.screen.skipped.text()
        self.assertEqual(len(text.splitlines()), len(skipped))
        self.assertIn("Изображения без alt — нет выгрузки в источнике", text)
        self.assertIn("Без H2 — отключено в профиле", text)
        for item in skipped:
            self.assertNotIn(item["id"], text)
        for english in ("missing", "export", "column", "not available", "configured"):
            self.assertNotIn(english, text)
        self.assertNotIn("причина не указана ядром", text)  # every reason of the fixture is mapped

    def test_unknown_reason_and_check_are_not_shown_raw(self):
        self.assertEqual(skip_reason("something unexpected"), "причина не указана ядром")
        self.assertEqual(check_name("BRAND_NEW_CHECK"), "Другая проверка")

    def test_every_check_of_the_core_registry_has_a_name(self):
        registry = Path(__file__).resolve().parents[2] / "seohead" / "sf" / "core" / "registry.py"
        if not registry.is_file():
            self.skipTest("core registry is not in this checkout")
        spec = importlib.util.spec_from_file_location("core_registry", registry)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertGreater(len(module.CHECKS), 100)
        self.assertEqual([code for code in module.CHECKS if check_name(code) == "Другая проверка"], [])

    def test_no_issue_numbers_and_no_service_footer_on_screen(self):
        self.screen.row_buttons[next(iter(self.screen.row_buttons))].click()
        self.app.processEvents()
        widgets = self.screen.findChildren(QLabel) + self.screen.findChildren(QPushButton)
        shown = " ".join(w.text() + " " + w.toolTip() for w in widgets) + " " + self.screen.foot.text()
        self.assertFalse(re.search(r"#\d{3}", shown), shown)
        self.assertNotIn("core:", shown)
        self.assertNotIn(self.scan["uuid"][:8], shown)
        self.assertIn("Недоступно в этой версии ядра", shown)

    def test_core_text_is_translated_and_raw_text_only_in_the_collapsed_block(self):
        findings = self.scan["evidence"]["findings"]
        for item in findings["items"]:
            self.assertFalse(re.search(r"[A-Za-z]{5,} [a-z]{3,} [a-z]{3,}", message_ru(item["message"])), item["message"])
        self.assertEqual(message_ru("Page returns a 4xx response (broken page)"), "Битая страница, ответ 4xx")
        self.assertIn("языке ядра", message_ru("Some phrase nobody translated"))
        self.screen.row_buttons["BROKEN_PAGE_4XX"].click()
        self.app.processEvents()
        raw = "Page returns a 4xx response"
        self.assertNotIn(raw, self.screen.desc.text())
        self.assertNotIn(raw, self.screen.model.data(self.screen.model.index(0, 2)))
        self.assertTrue(self.screen.raw.isHidden())
        self.assertEqual(self.screen.raw_toggle.text(), "Исходный ответ ядра")
        self.screen.raw_toggle.click()
        self.app.processEvents()
        self.assertFalse(self.screen.raw.isHidden())
        self.assertIn(raw, self.screen.raw.text())
        self.assertTrue(self.screen.raw.textInteractionFlags() & Qt.TextSelectableByMouse)

    def test_every_message_of_the_fixture_has_a_translation(self):
        for item in self.scan["evidence"]["findings"]["items"]:
            self.assertNotIn("языке ядра", message_ru(item["message"]), item["message"])

    def test_slot_is_issues(self):
        self.assertEqual(IssuesScreen.slot, "issues")
        self.assertIn("issues", SLOTS)
        self.assertNotIn("audit", SLOTS)
        self.assertEqual(VIEW_ALIASES["audit"], "issues")
        self.assertIn("issues", VIEW_IDS)

    def test_no_findings_state_is_an_honest_panel(self):
        self.scan["evidence"]["findings"] = {"state": "unavailable", "reason": "нет аудита"}
        self.screen.refresh()
        self.assertEqual(self.screen.stack.currentIndex(), 1)
        panel = self.screen.state_holder.findChild(StatePanel)
        self.assertEqual(panel.kind, "partial")

    def test_measured_zero_is_a_good_outcome(self):
        self.scan["evidence"]["findings"] = {"state": "available", "total": 0, "by_severity": {}, "items": []}
        self.screen.refresh()
        panel = self.screen.state_holder.findChild(StatePanel)
        self.assertEqual(panel.title.text(), "Проблем не найдено")
        skipped = len(self.scan["evidence"].get("skipped_checks") or [])
        badges = [label.text() for label in panel.findChildren(QLabel) if label.property("badge") == "mut"]
        self.assertEqual(badges, [f"{skipped} не измерялось"])  # measured count of checks that did not run, never «0 проблем»

    def test_no_project_and_no_scan(self):
        self.host.project_directory = None
        self.screen.refresh()
        self.assertEqual(self.screen.panel_state, "none")
        self.host.project_directory = "/project/shop"
        self.host.selected_scan_path = None
        self.screen.refresh()
        self.assertEqual(self.screen.panel_state, "noscan")

    def test_english(self):
        i18n.set_language("en")
        self.app.processEvents()
        i18n.retranslate(self.screen)
        self.assertEqual(self.screen.open_url.text(), "Open in URL")


if __name__ == "__main__":
    unittest.main()
