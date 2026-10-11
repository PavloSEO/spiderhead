import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QAbstractTableModel, QModelIndex, Qt
from PyQt5.QtGui import QPixmap
from PyQt5.QtWidgets import QPushButton, QTableView

from seohead_desktop import i18n, theming
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.kit import (
    BADGE_ROLE,
    BadgeDelegate,
    Kpi,
    PageHeader,
    StatePanel,
    style_table,
    waiting_badge,
)


class Rows(QAbstractTableModel):
    def rowCount(self, parent=QModelIndex()):  # noqa: B008
        return 3

    def columnCount(self, parent=QModelIndex()):  # noqa: B008
        return 2

    def data(self, index, role=Qt.DisplayRole):
        if role == Qt.DisplayRole:
            return f"r{index.row()}"
        if role == BADGE_ROLE and index.column() == 1:
            return ("ok", "200") if index.row() else None


class KitTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def test_missing_value_is_not_zero(self):
        kpi = Kpi("Страниц", None)
        self.assertEqual(kpi.number.text(), "Нет данных")
        kpi.set_value(0)
        self.assertEqual(kpi.number.text(), "0")  # a measured zero is a real zero

    def test_waiting_state_names_the_core_issue_and_has_no_numbers(self):
        panel = StatePanel("waiting", "Журнал по страницам", "Показаны последние 200 событий", issue=923)
        self.assertEqual(panel.issue_label.text(), "Недоступно в этой версии ядра")
        self.assertNotIn("923", panel.issue_label.text() + panel.issue_label.toolTip())
        self.assertEqual(panel.issue_label.property("waiting_issue"), 923)

    def test_state_panel_action_is_wired(self):
        seen = []
        panel = StatePanel("empty", "Нет проектов", action=("Открыть проект", lambda: seen.append(1)))
        panel.findChild(QPushButton).click()
        self.assertEqual(seen, [1])

    def test_page_header_actions_and_meta(self):
        header = PageHeader("Сканы")
        self.assertTrue(header.meta.isHidden())
        header.set_meta("6 запусков")
        self.assertEqual(header.meta.text(), "6 запусков")

    def test_badge_delegate_renders_text_with_the_badge_and_plain_cells_normally(self):
        table = QTableView()
        table.setModel(Rows())
        style_table(table)
        table.setItemDelegateForColumn(1, BadgeDelegate(table))
        table.resize(300, 160)
        table.show()
        self.app.processEvents()
        image = QPixmap(table.size())
        table.render(image)
        self.assertFalse(image.isNull())
        self.assertEqual(table.verticalHeader().defaultSectionSize(), theming.metrics()["row"]["standard"])

    def test_texts_follow_the_language(self):
        try:
            i18n.set_language("en")
            kpi = Kpi("Страниц", None)
            self.assertEqual(kpi.number.text(), "No data")
        finally:
            i18n.set_language("ru")

    def test_waiting_badge_text(self):
        self.assertEqual(waiting_badge(931).text(), "Недоступно в этой версии ядра")


if __name__ == "__main__":
    unittest.main()
