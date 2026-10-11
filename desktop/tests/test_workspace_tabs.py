"""Workspace identity, immutable capture, tab lifetime and keyboard contracts."""

import unittest
from dataclasses import FrozenInstanceError
from unittest.mock import Mock, patch

from PyQt5.QtCore import QByteArray, QEvent, QPoint, QPointF, Qt
from PyQt5.QtGui import QIcon, QKeySequence, QMouseEvent
from PyQt5.QtTest import QSignalSpy, QTest
from PyQt5.QtWidgets import (
    QApplication,
    QLineEdit,
    QMainWindow,
    QTabBar,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.workspace_tabs import WorkspaceContext, WorkspaceTabs
from tests._qt import sweep_widgets, ui


class WorkspaceTabsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        sweep_widgets()
        cls.app = qt_app()
        cls.app.setStyle("Fusion")

    def setUp(self):
        self.window = QMainWindow()
        central = QWidget()
        self.window.setCentralWidget(central)
        layout = QVBoxLayout(central)
        self.tabs = WorkspaceTabs()
        layout.addWidget(self.tabs)
        self.body = QLineEdit()
        layout.addWidget(self.body)
        self.window.resize(800, 260)

    def tearDown(self):
        self.window.close()
        self.window.deleteLater()
        self.app.processEvents()

    def add(self, id, **kwargs):
        return self.tabs.add(WorkspaceContext(id=id, project_label=id), **kwargs)

    def test_screen_caption_preserves_alias_and_workspace_identity(self):
        self.add("a")
        self.tabs.set_alias("a", "Client workspace")
        before = self.tabs.contexts()
        self.tabs.set_screen_presentation("Project settings", "settings")
        self.assertEqual(self.tabs.tabbar.tabText(0), "Project settings")
        self.assertEqual(self.tabs.contexts(), before)
        self.assertEqual(self.tabs.current_id, "a")
        self.tabs.set_screen_presentation()
        self.assertEqual(self.tabs.tabbar.tabText(0), "Client workspace")
        self.assertEqual(self.tabs.contexts(), before)

    def test_context_deep_freezes_state_and_returns_independent_capture(self):
        raw = {
            "search": "chair",
            "filter": {"http": [200, 301]},
            "offset": 100,
            "selected_url": "https://example.test/a",
            "splitter": QByteArray(b"saved-layout"),
        }
        context = WorkspaceContext(
            id="a",
            project_uuid="p",
            project_root="/project",
            scan_uuid="s",
            view_id="audit",
            state=raw,
        )
        raw["filter"]["http"].append(404)
        raw["splitter"].append(b"mutated")
        self.assertEqual(context.state["filter"]["http"], (200, 301))
        self.assertEqual(context.state["splitter"], b"saved-layout")
        with self.assertRaises(TypeError):
            context.state["offset"] = 0
        with self.assertRaises(TypeError):
            context.state["filter"]["http"] = ()
        with self.assertRaises(FrozenInstanceError):
            context.scan_uuid = "other"
        restored = context.state_dict()
        restored["filter"]["http"].append(500)
        self.assertEqual(context.state["filter"]["http"], (200, 301))

    def test_context_refuses_live_objects_cycles_and_nonfinite_state(self):
        with self.assertRaises(TypeError):
            WorkspaceContext(state={"manager": self.window})
        cycle = {}
        cycle["self"] = cycle
        with self.assertRaises(ValueError):
            WorkspaceContext(state=cycle)
        with self.assertRaises(ValueError):
            WorkspaceContext(state={"offset": float("nan")})

    def test_add_and_same_selection_emit_only_actual_id_changes(self):
        selected = QSignalSpy(self.tabs.selected)
        self.assertEqual(self.add("a"), "a")
        self.add("b", select=False)
        self.assertEqual(self.tabs.current_id, "a")
        self.tabs.select("a")
        self.assertEqual(list(selected), [["a"]])
        self.tabs.select("b")
        self.tabs.select("b")
        self.assertEqual(list(selected), [["a"], ["b"]])

    def test_moving_tabs_preserves_active_identity_and_context_order(self):
        for id in ("a", "b", "c"):
            self.add(id)
        selected = QSignalSpy(self.tabs.selected)
        self.tabs.tabbar.moveTab(2, 0)
        self.assertEqual([context.id for context in self.tabs.contexts()], ["c", "a", "b"])
        self.assertEqual(self.tabs.current_id, "c")
        self.assertEqual(len(selected), 0)

    def test_new_context_is_inserted_next_to_active_or_explicit_source(self):
        for id in ("a", "b", "c"):
            self.add(id)
        self.tabs.select("a")
        self.add("new")
        self.assertEqual([item.id for item in self.tabs.contexts()], ["a", "new", "b", "c"])
        self.add("copy", after_id="b", select=False)
        self.assertEqual([item.id for item in self.tabs.contexts()], ["a", "new", "b", "copy", "c"])
        self.assertEqual(self.tabs.current_id, "new")
        before = self.tabs.contexts()
        with self.assertRaises(KeyError):
            self.add("missing-anchor", after_id="unknown")
        self.assertEqual(self.tabs.contexts(), before)

    def test_alias_and_pin_survive_host_capture_without_mutating_project_identity(self):
        original = WorkspaceContext(
            id="a",
            project_uuid="project-a",
            project_root="/project-a",
            project_label="Project A",
            scan_uuid="scan-a",
            state={"offset": 100},
        )
        self.tabs.add(original)
        selected = QSignalSpy(self.tabs.selected)
        self.tabs.set_alias("a", "  Для клиента  ")
        self.tabs.set_pinned("a", True)
        self.tabs.update_context("a", project_label="Updated project")
        self.tabs.update_title("a", "Updated project · completed scan")
        updated = self.tabs.update_state("a", {"offset": 200})
        self.assertEqual(self.tabs.tabbar.tabText(0), "Для клиента")
        self.assertEqual(
            self.tabs.tabbar.tabToolTip(0),
            "Закреплена · Для клиента\nUpdated project · completed scan",
        )
        self.assertEqual(updated.display_alias, "Для клиента")
        self.assertTrue(updated.pinned)
        self.assertEqual(
            (updated.id, updated.project_uuid, updated.project_root, updated.scan_uuid),
            ("a", "project-a", "/project-a", "scan-a"),
        )
        self.assertEqual(original.state_dict(), {"offset": 100})
        self.assertIsNone(original.display_alias)
        self.assertFalse(original.pinned)
        self.assertEqual(list(selected), [])
        self.tabs.set_alias("a", "   ")
        self.assertEqual(self.tabs.tabbar.tabText(0), "Updated project · completed scan")

    def test_pinned_group_preserves_active_id_and_rejects_cross_group_drag(self):
        for id in ("a", "b", "c", "d"):
            self.add(id)
        selected = QSignalSpy(self.tabs.selected)
        self.tabs.set_pinned("b", True)
        self.tabs.set_pinned("d", True)
        self.assertEqual([item.id for item in self.tabs.contexts()], ["b", "d", "a", "c"])
        self.assertEqual(self.tabs.current_id, "d")
        self.tabs.tabbar.moveTab(1, 3)
        self.assertEqual([item.id for item in self.tabs.contexts()], ["b", "d", "a", "c"])
        self.tabs.tabbar.moveTab(3, 0)
        self.assertEqual([item.id for item in self.tabs.contexts()], ["b", "d", "c", "a"])
        self.tabs.tabbar.moveTab(1, 0)
        self.assertEqual([item.id for item in self.tabs.contexts()], ["d", "b", "c", "a"])
        self.tabs.set_pinned("d", False)
        self.assertEqual([item.id for item in self.tabs.contexts()], ["b", "d", "c", "a"])
        self.assertEqual(self.tabs.current_id, "d")
        self.assertEqual(list(selected), [])

    def test_mouse_drag_does_not_shuffle_tabs_across_pin_boundary(self):
        for id in ("a", "b", "c", "d"):
            self.add(id)
        self.tabs.set_pinned("b", True)
        self.window.show()
        self.app.processEvents()
        bar = self.tabs.tabbar

        def drag(source, destination):
            start, end = bar.tabRect(source).center(), bar.tabRect(destination).center()
            QTest.mousePress(bar, Qt.LeftButton, pos=start)
            for step in range(1, 21):
                point = QPoint(start.x() + (end.x() - start.x()) * step // 20, start.y())
                QApplication.sendEvent(
                    bar,
                    QMouseEvent(
                        QEvent.MouseMove,
                        QPointF(point),
                        QPointF(bar.mapToGlobal(point)),
                        Qt.NoButton,
                        Qt.LeftButton,
                        Qt.NoModifier,
                    ),
                )
                QTest.qWait(5)
            QTest.mouseRelease(bar, Qt.LeftButton, pos=end)
            QTest.qWait(150)

        drag(0, 3)
        self.assertEqual([item.id for item in self.tabs.contexts()], ["b", "a", "c", "d"])
        self.assertEqual(self.tabs.current_id, "b")
        drag(3, 0)
        self.assertEqual([item.id for item in self.tabs.contexts()], ["b", "d", "a", "c"])
        self.assertEqual(self.tabs.current_id, "d")
        self.tabs.set_pinned("a", True)
        drag(0, 3)
        self.assertEqual([item.id for item in self.tabs.contexts()], ["a", "b", "d", "c"])
        self.assertEqual(self.tabs.current_id, "b")
        drag(2, 3)
        self.assertEqual([item.id for item in self.tabs.contexts()], ["a", "b", "c", "d"])

    def test_new_tab_after_pinned_starts_unpinned_group_and_restored_pin_joins_prefix(
        self,
    ):
        self.add("a")
        self.add("b")
        self.tabs.set_pinned("a", True)
        self.tabs.select("a")
        self.add("new")
        self.tabs.add(WorkspaceContext(id="saved", pinned=True, display_alias="Saved"))
        self.assertEqual([item.id for item in self.tabs.contexts()], ["a", "saved", "new", "b"])
        self.assertEqual(self.tabs.tabbar.tabText(1), "Saved")

    def test_pinned_tabs_require_explicit_close_and_never_control_shared_services(self):
        self.add("a")
        self.add("b")
        manager, gateway = Mock(), Mock()
        self.window.scan_manager, self.window.mcp_gateway = manager, gateway
        closed = QSignalSpy(self.tabs.closeRequested)
        self.tabs.set_pinned("b", True)
        self.tabs.set_alias("b", "Review")
        self.tabs.request_close()
        self.tabs.tabbar.tabCloseRequested.emit(0)
        self.assertEqual(list(closed), [])
        for side in (QTabBar.LeftSide, QTabBar.RightSide):
            button = self.tabs.tabbar.tabButton(0, side)
            if button is not None:
                self.assertTrue(button.isHidden())
                self.assertFalse(button.isEnabled())
        self.tabs._rebuild_menu()
        next(
            action
            for action in self.tabs.menu.actions()
            if action.text() == ui("Закрыть закреплённую вкладку")
        ).trigger()
        self.assertEqual(list(closed), [["b"]])
        self.assertEqual(len(self.tabs.contexts()), 2)
        self.tabs.set_pinned("b", False)
        self.tabs.request_close()
        self.assertEqual(list(closed), [["b"], ["b"]])
        self.tabs.remove("b")
        self.assertEqual(manager.mock_calls, [])
        self.assertEqual(gateway.mock_calls, [])

    def test_rename_cancel_reset_and_context_menu_target_stable_identity(self):
        self.add("a")
        self.add("b")
        self.window.show()
        self.app.processEvents()
        self.tabs._show_context_menu(self.tabs.tabbar.tabRect(0).center())
        self.assertEqual(self.tabs.current_id, "b")
        actions = {action.text(): action for action in self.tabs.context_menu.actions()}
        with patch("seohead_desktop.ui.workspace_tabs.QInputDialog") as factory:
            dialog = factory.return_value
            dialog.exec_.side_effect = [1, 0, 1]
            dialog.textValue.side_effect = ["Review A", "Cancelled", ""]
            actions[ui("Переименовать вкладку…")].trigger()
            self.tabs.context_menu.close()
            self.assertEqual(self.tabs.contexts()[0].display_alias, "Review A")
            actions[ui("Закрепить вкладку")].trigger()
            self.assertTrue(self.tabs.contexts()[0].pinned)
            self.assertEqual(self.tabs.current_id, "b")
            self.tabs.request_rename("a")
            self.assertEqual(self.tabs.contexts()[0].display_alias, "Review A")
            self.tabs.request_rename("a")
            dialog.setCancelButtonText.assert_called_with(ui("Отмена"))
            dialog.setOkButtonText.assert_called_with(ui("Сохранить"))
            self.assertEqual(dialog.deleteLater.call_count, 3)
        self.assertIsNone(self.tabs.contexts()[0].display_alias)

    def test_context_rejects_invalid_alias_and_pin_types(self):
        for alias in (" ", 7, self.window):
            with self.assertRaises(ValueError):
                WorkspaceContext(display_alias=alias)
        for pinned in (1, "yes", None):
            with self.assertRaises(TypeError):
                WorkspaceContext(pinned=pinned)

    def test_host_can_revert_refused_switch_without_recursive_selection(self):
        self.add("a")
        self.add("b", select=False)
        selected = QSignalSpy(self.tabs.selected)
        self.tabs.select("b")
        self.tabs.blockSignals(True)
        self.tabs.select("a")
        self.tabs.blockSignals(False)
        self.assertEqual(list(selected), [["b"]])
        self.tabs.select("b")
        self.assertEqual(list(selected), [["b"], ["b"]])

    def test_inactive_removal_does_not_reselect_and_active_removal_selects_neighbor(
        self,
    ):
        for id in ("a", "b", "c"):
            self.add(id)
        selected = QSignalSpy(self.tabs.selected)
        self.assertEqual(self.tabs.remove("a").id, "a")
        self.assertEqual(self.tabs.current_id, "c")
        self.assertEqual(len(selected), 0)
        self.tabs.remove("c")
        self.assertEqual(self.tabs.current_id, "b")
        self.assertEqual(list(selected), [["b"]])
        self.tabs.remove("b")
        self.assertIsNone(self.tabs.current_id)
        self.assertEqual(self.tabs.contexts(), ())
        self.assertEqual(list(selected), [["b"]])
        self.assertIsNone(self.tabs.remove("unknown"))

    def test_close_is_an_intent_and_removal_keeps_host_and_shared_services_alive(self):
        self.add("a")
        self.add("b")
        shared_manager, shared_gateway = Mock(), Mock()
        self.window.scan_manager = shared_manager
        self.window.mcp_gateway = shared_gateway
        self.window.show()
        self.app.processEvents()
        closed = QSignalSpy(self.tabs.closeRequested)
        self.tabs.tabbar.tabCloseRequested.emit(1)
        self.assertEqual(list(closed), [["b"]])
        self.assertEqual(len(self.tabs.contexts()), 2)
        self.tabs.remove("b")
        self.assertTrue(self.window.isVisible())
        self.assertIs(self.window.scan_manager, shared_manager)
        self.assertIs(self.window.mcp_gateway, shared_gateway)
        shared_manager.stop.assert_not_called()
        shared_manager.stop_all_owned.assert_not_called()
        shared_gateway.stop.assert_not_called()

    def test_twelve_context_limit_blocks_new_and_duplicate_without_evicting(self):
        for number in range(12):
            self.add(str(number))
        before = self.tabs.contexts()
        new, duplicate = (
            QSignalSpy(self.tabs.newRequested),
            QSignalSpy(self.tabs.duplicateRequested),
        )
        self.assertFalse(self.tabs.new_button.isEnabled())
        self.tabs.request_new()
        self.tabs.request_duplicate()
        with self.assertRaises(ValueError):
            self.add("overflow")
        self.assertEqual(self.tabs.contexts(), before)
        self.assertEqual((len(new), len(duplicate)), (0, 0))
        self.tabs.remove("0")
        self.assertTrue(self.tabs.new_button.isEnabled())
        self.tabs.request_new()
        self.tabs.request_duplicate("1")
        self.assertEqual(list(new), [[]])
        self.assertEqual(list(duplicate), [["1"]])

    def test_updates_replace_snapshot_without_changing_id_or_selection(self):
        original = WorkspaceContext(id="a", project_label="Original", state={"offset": 0})
        self.tabs.add(original)
        selected = QSignalSpy(self.tabs.selected)
        updated = self.tabs.update_context(
            "a", scan_uuid="scan-2", view_id="urls", state={"offset": 100}
        )
        self.assertEqual(updated.id, "a")
        self.assertEqual(original.state["offset"], 0)
        self.assertEqual(updated.state["offset"], 100)
        self.tabs.update_title("a", "Project · scan-2", QIcon())
        self.assertEqual(self.tabs.tabbar.tabText(0), "Project · scan-2")
        close_buttons = [
            self.tabs.tabbar.tabButton(0, side) for side in (QTabBar.LeftSide, QTabBar.RightSide)
        ]
        self.assertTrue(
            any(
                button and button.accessibleName() == "Закрыть вкладку Project · scan-2"
                for button in close_buttons
            )
        )
        self.tabs.update_state("a", {"offset": 200})
        self.assertEqual(self.tabs.contexts()[0].state["offset"], 200)
        self.assertEqual(len(selected), 0)
        with self.assertRaises(ValueError):
            self.tabs.update_context("a", id="different")

    def test_duplicate_and_overflow_actions_use_stable_ids(self):
        self.add("a")
        self.add("b")
        duplicate = QSignalSpy(self.tabs.duplicateRequested)
        self.tabs._rebuild_menu()
        next(
            action
            for action in self.tabs.menu.actions()
            if action.text() == ui("Дублировать вкладку")
        ).trigger()
        self.assertEqual(list(duplicate), [["b"]])
        self.assertEqual(len(self.tabs.contexts()), 2)
        next(action for action in self.tabs.menu.actions() if action.data() == "a").trigger()
        self.assertEqual(self.tabs.current_id, "a")
        self.assertFalse(self.tabs.menu.isEmpty())
        self.assertTrue(self.tabs.tabbar.usesScrollButtons())
        self.assertTrue(self.tabs.tabbar.isMovable())
        self.assertTrue(self.tabs.tabbar.tabsClosable())
        old_action = next(action for action in self.tabs.menu.actions() if action.data() == "b")
        self.tabs.remove("b")
        old_action.trigger()
        self.assertEqual(self.tabs.current_id, "a")

    def test_plus_stays_next_to_tab_strip_at_wide_and_overflow_widths(self):
        self.window.show()
        for count in (1, 2, 12):
            while len(self.tabs.contexts()) < count:
                self.add(str(len(self.tabs.contexts())))
            for width in (360, 800, 1440):
                with self.subTest(count=count, width=width):
                    self.window.resize(width, 260)
                    self.app.processEvents()
                    bar = self.tabs.tabbar.geometry()
                    plus = self.tabs.new_button.geometry()
                    overflow = self.tabs.overflow_button.geometry()
                    self.assertEqual(plus.left(), bar.right() + 1)
                    self.assertLess(plus.right(), overflow.left())
                    self.assertTrue(self.tabs.rect().contains(plus))
                    self.assertTrue(self.tabs.rect().contains(overflow))
                    last = self.tabs.tabbar.tabRect(count - 1)
                    self.assertTrue(last.intersects(self.tabs.tabbar.rect()))
                    if count <= 2 and width >= 800:
                        self.assertEqual(plus.left(), bar.left() + last.right() + 1)
                        self.assertLess(plus.right(), width // 2)
                    self.assertEqual(self.tabs.new_button.isEnabled(), count < 12)
                    self.assertTrue(self.tabs.tabbar.usesScrollButtons())

    def test_themed_overflow_buttons_are_separate_and_reach_both_ends(self):
        from seohead_desktop.app import load_theme

        stylesheet = self.app.styleSheet()
        try:
            load_theme(self.app)
            for number in range(12):
                self.add(str(number), title=f"Project {number} · retained scan")
            self.window.resize(360, 260)
            self.window.show()
            self.tabs.select("0")
            self.app.processEvents()
            bar = self.tabs.tabbar
            buttons = sorted(
                (
                    button
                    for button in bar.findChildren(QToolButton)
                    if button.isVisible() and not button.property("tab_close")
                ),
                key=lambda button: button.x(),
            )
            self.assertEqual(len(buttons), 2)
            left, right = buttons
            # Native QTabBar shares a one-pixel border between the scroll buttons.
            self.assertLessEqual(left.geometry().intersected(right.geometry()).width(), 1)
            self.assertIs(bar.childAt(left.geometry().center()), left)
            self.assertIs(bar.childAt(right.geometry().center()), right)
            self.assertFalse(left.isEnabled())
            self.assertTrue(right.isEnabled())
            before = bar.tabRect(11).left()
            QTest.mouseClick(right, Qt.LeftButton)
            self.app.processEvents()
            self.assertLess(bar.tabRect(11).left(), before)
            self.assertEqual(self.tabs.current_id, "0")
            self.tabs.select("11")
            self.app.processEvents()
            self.assertTrue(left.isEnabled())
            self.assertFalse(right.isEnabled())
            self.assertLess(bar.tabRect(11).right(), left.x())
            self.assertEqual(self.tabs.new_button.x(), bar.geometry().right() + 1)
        finally:
            self.app.setStyleSheet(stylesheet)

    def test_window_shortcuts_work_with_body_focus_and_do_not_close_window(self):
        self.add("a")
        self.add("b")
        self.tabs.install_shortcuts(self.window)
        count = len(self.tabs._shortcuts)
        self.tabs.install_shortcuts(self.window)
        self.assertEqual(len(self.tabs._shortcuts), count)
        self.window.show()
        self.window.activateWindow()
        self.app.processEvents()
        self.body.setFocus()
        new, close = (
            QSignalSpy(self.tabs.newRequested),
            QSignalSpy(self.tabs.closeRequested),
        )
        QTest.keyClick(self.body, Qt.Key_T, Qt.ControlModifier)
        QTest.keyClick(self.body, Qt.Key_W, Qt.ControlModifier)
        self.assertEqual(list(new), [[]])
        self.assertEqual(list(close), [["b"]])
        self.assertTrue(self.window.isVisible())
        QTest.keyClick(self.body, Qt.Key_Tab, Qt.ControlModifier)
        self.assertEqual(self.tabs.current_id, "a")
        previous = next(
            shortcut.key()
            for shortcut in self.tabs._shortcuts
            if "Backtab" in shortcut.key().toString() or "Shift+Tab" in shortcut.key().toString()
        )
        combined = previous[0]
        modifiers = Qt.KeyboardModifiers(combined & int(Qt.KeyboardModifierMask))
        key = combined & ~int(Qt.KeyboardModifierMask)
        QTest.keyClick(self.body, key, modifiers)
        self.assertEqual(self.tabs.current_id, "b")
        self.assertEqual(
            QKeySequence("Ctrl+T"),
            next(s.key() for s in self.tabs._shortcuts if s.key() == QKeySequence("Ctrl+T")),
        )
        self.tabs.set_pinned("b", True)
        QTest.keyClick(self.body, Qt.Key_W, Qt.ControlModifier)
        self.assertEqual(list(close), [["b"]])
        QTest.keyClick(self.body, Qt.Key_Tab, Qt.ControlModifier)
        self.assertEqual(self.tabs.current_id, "a")
        QTest.keyClick(self.body, Qt.Key_W, Qt.ControlModifier)
        self.assertEqual(list(close), [["b"], ["a"]])
        self.assertTrue(self.window.isVisible())


if __name__ == "__main__":
    unittest.main()
