"""Native interaction and data-boundary tests for reusable presentation panels."""

import unittest
from unittest.mock import patch

from PyQt5.QtCore import Qt, QTimer
from PyQt5.QtTest import QSignalSpy, QTest
from PyQt5.QtWidgets import QDialog, QTableView

from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.components import (
    PageModel,
    TabConfigurationDialog,
    TabDeck,
    TablePanel,
)
from seohead_desktop.ui.panels import (
    AuditWorkspace,
    ComparePanel,
    InboxPanel,
    ProjectPanels,
    SerpPreviewPanel,
    SourcePanel,
    component_stylesheet,
)
from seohead_desktop.ui.tabcatalogue import (
    ALL_TABS,
    DETAIL_TABS,
    MAIN_TABS,
    RIGHT_TABS,
    TAB_BY_ID,
)
from tests._screens_core import fixture


def _row(path, status=200, kind="text/html", indexability="Indexable", title="Page", issues=0, depth=1):
    return {"url": "https://catalog.example.test" + path, "status": status, "type": kind,
            "indexability": indexability, "title": title, "issues": issues, "crawl_depth": depth}


# Rows in the shape scans_urls builds from the core's URL page; scans and tasks are real core answers.
ROWS = [
    _row("/", title="Catalog", depth=0), _row("/chairs/", issues=1), _row("/chairs/oak/", depth=2),
    _row("/journal/care/", title="", issues=1, depth=2),
    _row("/archive/desk/", 301, indexability="Non-Indexable", title=None, issues=1, depth=2),
    _row("/search/?q=chair", indexability="Non-Indexable", title="Search results"),
    _row("/images/chair.webp", kind="image/webp", indexability=None, title=None, issues=None, depth=2),
    _row("/discontinued/", 404, indexability="Non-Indexable", issues=1, depth=2),
]
SCANS = [dict(fixture("scan_row.json"), uuid=f"scan-{n}", finished_at=f"2026-10-0{n}T10:00:00Z") for n in (2, 1)]
TASKS = fixture("checklist_page.json")["items"][:2]


class ComponentTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        cls.app.setStyle("Fusion")
        tokens = load_theme(cls.app)
        cls.app.setStyleSheet(cls.app.styleSheet() + component_stylesheet(tokens))

    def setUp(self):
        self.widgets = []

    def own(self, widget):
        self.widgets.append(widget)
        return widget

    def tearDown(self):
        for widget in self.widgets:
            widget.close()
            widget.deleteLater()
        self.app.processEvents()

    def test_catalogue_covers_observed_panes_with_unique_ids(self):
        self.assertEqual(
            (len(MAIN_TABS), len(DETAIL_TABS), len(RIGHT_TABS)), (21, 12, 6)
        )
        self.assertEqual(len(ALL_TABS), len(TAB_BY_ID))
        self.assertEqual(len(ALL_TABS), 54)
        self.assertTrue(
            all(tab.columns and tab.filters and tab.evidence for tab in ALL_TABS)
        )

    def test_model_bounded_generator_and_atomic_rejection(self):
        model = PageModel((("value", "Value"),))
        model.replace([{"value": "kept"}])
        consumed = []

        def rows():
            for i in range(1000000):
                consumed.append(i)
                yield {"value": i}

        with self.assertRaises(ValueError):
            model.replace(rows())
        self.assertEqual(len(consumed), 101)
        self.assertEqual(model.rows, [{"value": "kept"}])

    def test_unknown_zero_false_and_empty_are_distinct(self):
        model = PageModel((("value", "Value"),))
        model.replace([{}, {"value": 0}, {"value": False}, {"value": ""}])
        self.assertEqual(
            [model.index(row, 0).data() for row in range(4)],
            ["Не измерено", "0", "Нет", ""],
        )

    def test_numeric_sort_and_filtered_selection_keep_source_identity(self):
        panel = self.own(TablePanel(TAB_BY_ID["internal"]))
        rows = [
            {"url": "https://example.test/b", "status": 404},
            {"url": "https://example.test/a", "status": 99},
        ]
        panel.set_page(rows, total=2, source="Test page")
        panel.table.sortByColumn(2, Qt.AscendingOrder)
        self.assertEqual(panel.proxy.index(0, 2).data(), "99")
        seen = QSignalSpy(panel.intent_requested)
        panel.search.setText("/b")
        panel.table.selectRow(0)
        self.assertEqual(panel.proxy.rowCount(), 1)
        self.assertEqual(seen[-1][0], "select_url")
        self.assertEqual(seen[-1][1]["row"]["url"], rows[0]["url"])
        self.assertIn("найдено на странице: 1", panel.count_label.text())

    def test_pagination_emits_bounded_intent_without_core_calls(self):
        panel = self.own(TablePanel(TAB_BY_ID["tasks"]))
        seen = QSignalSpy(panel.intent_requested)
        panel.set_page(
            TASKS, total=400, offset=100, has_more=True, source="Core fixture"
        )
        panel.next_button.click()
        self.assertEqual(
            seen[-1][1],
            {"tab_id": "tasks", "offset": 102, "limit": 100, "filter_id": "all"},
        )
        panel.previous_button.click()
        self.assertEqual(seen[-1][1]["offset"], 0)
        self.assertEqual(panel.model.rowCount(), 2)

    def test_unsupported_filters_and_unavailable_counts_remain_explicit(self):
        panel = self.own(TablePanel(TAB_BY_ID["page_titles"]))
        self.assertEqual(panel.count_label.text(), "Количество не измерено")
        self.assertFalse(panel.next_button.isEnabled())
        panel.set_page(ROWS, total=8, source="Test page")
        self.assertTrue(panel.filter.model().item(0).isEnabled())
        self.assertFalse(panel.filter.model().item(1).isEnabled())
        self.assertIn("недоступен", panel.filter.model().item(1).toolTip())
        panel.set_page([], total=0, source="Measured empty fixture")
        self.assertEqual(panel.count_label.text(), "Строки 0 из 0")
        panel.set_page([], state="error", reason="Retained evidence could not be read")
        self.assertIn("Retained evidence", panel.message.text())
        self.assertNotIn("из 0", panel.count_label.text())

    def test_tab_creation_is_lazy_and_keyboard_navigation_works(self):
        deck = self.own(TabDeck(MAIN_TABS))
        self.assertEqual(tuple(deck._panels), ("internal",))
        deck.resize(800, 500)
        deck.show()
        deck.tabbar.setFocus()
        QTest.keyClick(deck.tabbar, Qt.Key_Right)
        self.app.processEvents()
        self.assertEqual(deck.current_id, "external")
        self.assertEqual(len(deck._panels), 2)
        deck.select_tab("structured_data")
        self.assertEqual(deck.current_id, "structured_data")
        self.assertEqual(len(deck._panels), 3)

    def test_hidden_tabs_reorder_restore_and_empty_guard(self):
        deck = self.own(TabDeck(MAIN_TABS))
        self.assertFalse(deck.menu.isEmpty())
        self.assertEqual(len(deck.menu.actions()), len(MAIN_TABS) + 3)
        deck.set_visible_tabs(("page_titles", "internal"))
        self.assertEqual(deck.visible_ids, ("page_titles", "internal"))
        deck.select_tab("images")
        self.assertEqual(deck.visible_ids, ("page_titles", "internal", "images"))
        deck.tabbar.moveTab(2, 0)
        self.assertEqual(deck.visible_ids[0], "images")
        dialog = self.own(TabConfigurationDialog(MAIN_TABS, deck.visible_ids))
        dialog.visible_list.clear()
        dialog.accept()
        self.assertEqual(dialog.result(), QDialog.Rejected)
        self.assertIn("хотя бы одну", dialog.feedback.text())
        dialog.show_all()
        self.assertEqual(len(dialog.visible_ids), 21)
        dialog.reject()
        self.assertEqual(deck.visible_ids[0], "images")
        with self.assertRaises(ValueError):
            deck.set_visible_tabs(())

    def test_overflow_menu_opens_on_first_mouse_click(self):
        deck = self.own(TabDeck(MAIN_TABS))
        deck.resize(800, 500)
        deck.show()
        self.app.processEvents()
        observed = []

        def inspect_menu():
            observed.append(deck.menu.isVisible())
            deck.menu.close()

        QTimer.singleShot(50, inspect_menu)
        QTest.mouseClick(deck.menu_button, Qt.LeftButton)
        QTest.qWait(80)
        self.assertEqual(observed, [True])

    def test_url_selection_clears_stale_details_and_exposes_only_supplied_fields(self):
        workspace = self.own(AuditWorkspace())
        workspace.set_page(
            "http_headers",
            [{"name": "old", "value": "old URL evidence"}],
            source="old scan",
        )
        workspace.set_page("internal", ROWS, source="new retained scan")
        workspace.main.panel("internal").table.selectRow(1)
        self.assertEqual(workspace.panel("http_headers").state, "unavailable")
        self.assertEqual(workspace.panel("http_headers").model.rowCount(), 0)
        detail = workspace.panel("url_details")
        self.assertEqual(detail.model.rows[0]["value"], ROWS[1]["url"])
        self.assertEqual(detail.source_label.text(), "new retained scan")
        workspace.set_page("internal", [], total=0, source="new page")
        self.assertEqual(detail.state, "unavailable")

    def test_project_semantic_signals_preserve_task_and_scan_identity(self):
        panels = self.own(ProjectPanels())
        task_spy, scan_spy = (
            QSignalSpy(panels.select_task),
            QSignalSpy(panels.select_scan),
        )
        panels.set_page("tasks", TASKS, source="Fixture")
        panels.panel("tasks").table.selectRow(0)
        self.assertEqual(task_spy[-1][0], TASKS[0]["id"])
        scans = [dict(SCANS[0], path="/trusted/retained/scan")]
        panels.set_page("scans", scans, source="Fixture")
        panels.panel("scans").table.selectRow(0)
        self.assertEqual(scan_spy[-1][0], scans[0])

    def test_note_submission_requires_explicit_enabled_form_action(self):
        panel = self.own(InboxPanel(TAB_BY_ID["inbox"]))
        seen = QSignalSpy(panel.intent_requested)
        panel.note.setPlainText("A note")
        panel.submit_button.click()
        self.assertEqual(len(seen), 0)
        panel.set_submission_enabled(True)
        self.assertEqual(len(seen), 0)
        panel.submit_button.click()
        self.assertEqual(seen[-1], ["submit_note", {"text": "A note", "kind": "note"}])
        self.assertEqual(panel.note.toPlainText(), "A note")
        panel.note.setPlainText("x" * 4001)
        self.assertFalse(panel.submit_button.isEnabled())
        panel.submission_succeeded()
        self.assertEqual(panel.note.toPlainText(), "")

    def test_comparison_requires_two_distinct_retained_ids(self):
        panel = self.own(ComparePanel(TAB_BY_ID["compare"]))
        panel.set_scans(SCANS)
        panel.before.setCurrentIndex(1)
        panel.after.setCurrentIndex(1)
        self.assertFalse(panel.compare_button.isEnabled())
        panel.after.setCurrentIndex(2)
        self.assertTrue(panel.compare_button.isEnabled())
        seen = QSignalSpy(panel.intent_requested)
        panel.compare_button.click()
        self.assertEqual(seen[-1][0], "preview_compare")
        self.assertEqual(seen[-1][1]["before"]["uuid"], SCANS[0]["uuid"])
        panel.set_page([], state="unavailable")
        self.assertFalse(panel.compare_button.isEnabled())

    def test_source_is_literal_bounded_and_cleared_on_new_context(self):
        panel = self.own(SourcePanel(TAB_BY_ID["view_source"]))
        text = "<script>alert('fixture')</script>" + "x" * 80000
        panel.set_page([{"source": text}], source="Retained fixture")
        self.assertEqual(len(panel.viewer.toPlainText()), 65536)
        self.assertTrue(panel.viewer.toPlainText().startswith("<script>"))
        self.assertTrue(panel.viewer.isReadOnly())
        panel.set_page([], state="unavailable")
        self.assertEqual(panel.viewer.toPlainText(), "")

    def test_serp_preview_escapes_markup_and_reset_does_not_write(self):
        panel = self.own(SerpPreviewPanel(TAB_BY_ID["serp_snippet"]))
        row = {
            "url": "https://example.test/",
            "title": "<b>Fixture</b>",
            "description": "Description",
        }
        panel.set_page([row], source="Fixture")
        self.assertEqual(panel.preview_title.textFormat(), Qt.PlainText)
        panel.title_edit.setText("Changed locally")
        self.assertEqual(panel.preview_title.text(), "Changed locally")
        panel.reset_values()
        self.assertEqual(panel.title_edit.text(), row["title"])
        self.assertEqual(row["title"], "<b>Fixture</b>")

    def test_sensitive_header_cookie_and_config_values_are_not_displayed(self):
        for id, name in (
            ("cookies", "session"),
            ("http_headers", "Authorization"),
            ("config", "provider.api_key"),
        ):
            panel = self.own(TablePanel(TAB_BY_ID[id]))
            row = {"name": name, "value": "sensitive-value"}
            panel.set_page([row], source="Fixture")
            self.assertEqual(panel.model.rows[0]["value"], "[скрыто]")
            self.assertEqual(row["value"], "sensitive-value")

    def test_every_catalogue_panel_can_render_empty_and_unavailable_states(self):
        audit = self.own(AuditWorkspace())
        projects = self.own(ProjectPanels())
        with patch(
            "subprocess.run", side_effect=AssertionError("UI cannot invoke core")
        ):
            for spec in ALL_TABS:
                owner = projects if spec.pane == "project" else audit
                owner.set_page(spec.id, [], total=0, source="Measured empty fixture")
                self.assertEqual(owner.panel(spec.id).state, "ready")
                owner.set_page(
                    spec.id, [], state="unavailable", reason="No retained evidence"
                )
                self.assertEqual(owner.panel(spec.id).model.rowCount(), 0)
        self.assertEqual(len(audit.findChildren(QTableView)), 39)
        self.assertEqual(len(projects.findChildren(QTableView)), 15)

    def test_audit_workspace_desktop_and_compact_keep_operable_tables(self):
        window = self.own(AuditWorkspace())
        window.set_page("internal", ROWS, total=len(ROWS), source="Test page")
        window.show()
        for size in ((1440, 900), (1024, 720)):
            window.resize(*size)
            self.app.processEvents()
            self.assertEqual((window.width(), window.height()), size)
            panel = window.main.panel("internal")
            self.assertGreater(panel.table.width(), 300)
            self.assertGreater(panel.table.height(), 180)
            self.assertGreater(panel.search.width(), 100)
            right = window.right.panel("overview")
            self.assertGreater(right.search.width(), 140)
        window.main.tabbar.setFocus()
        QTest.keyClick(window.main.tabbar, Qt.Key_Right)
        self.assertEqual(window.main.current_id, "external")
        self.assertEqual(window.panel("url_details").state, "unavailable")

if __name__ == "__main__":
    unittest.main()
