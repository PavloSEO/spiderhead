"""«Вкладки и панели»: the tab-strip tune button opens it (canvas TabSettings).

It edits the real audit decks of the open project (visibility and order of the main, detail and right tab bars,
the detail/right panel toggles) and the table density. Without an open project the catalogue is shown read-only.
"""

from __future__ import annotations

from PyQt5.QtCore import Qt
from PyQt5.QtGui import QKeySequence
from PyQt5.QtWidgets import (
    QCheckBox,
    QDialog,
    QDialogButtonBox,
    QFrame,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QListWidget,
    QListWidgetItem,
    QPushButton,
    QShortcut,
    QVBoxLayout,
)

from ..i18n import tr
from .controls import Segmented
from .icons import material_icon
from .presentation import panel_title
from .tabcatalogue import DETAIL_TABS, MAIN_TABS, RIGHT_TABS

AREAS = (
    ("main", "Основная таблица", MAIN_TABS),
    ("detail", "Детали URL", DETAIL_TABS),
    ("right", "Сводка справа", RIGHT_TABS),
)
DENSITIES = (("compact", "28 px"), ("standard", "32 px"), ("comfortable", "40 px"))


class TabSettingsDialog(QDialog):
    def __init__(self, decks, density, on_density, parent=None):
        super().__init__(parent)
        self.decks = decks  # {"main"|"detail"|"right": TabDeck} of the open project; {} without one
        self.on_density = on_density
        self.setWindowTitle(tr("Вкладки и панели"))
        self.setAccessibleName(tr("Вкладки и панели"))
        self.resize(760, 520)
        self._area = "main"
        self._rows = {area: self._initial_rows(area) for area, _, _ in AREAS}
        self._panels = {
            "right": not decks or not decks["right"].isHidden(),
            "detail": not decks or not decks["detail"].isHidden(),
        }

        layout = QVBoxLayout(self)
        head = QHBoxLayout()
        title = QLabel(tr("Вкладки и панели"))
        title.setObjectName("sectionCaption")
        head.addWidget(title, 1)
        close = QPushButton(tr("Закрыть"))
        close.setFlat(True)
        close.clicked.connect(self.reject)
        head.addWidget(close)
        layout.addLayout(head)

        body = QHBoxLayout()
        left = QVBoxLayout()
        self.area_control = Segmented(
            [(area, tr(label)) for area, label, _ in AREAS], "main", tr("Область вкладок")
        )
        self.area_control.changed.connect(self._show_area)
        left.addWidget(self.area_control)
        self.search = QLineEdit()
        self.search.setPlaceholderText(tr("Найти вкладку…"))
        self.search.setAccessibleName(tr("Найти вкладку"))
        self.search.textChanged.connect(self._filter)
        left.addWidget(self.search)
        hint = QLabel(
            tr("Отмеченные показываются в этом порядке. Источник без данных покажет «Нет измерения», а не пустую таблицу.")
        )
        hint.setWordWrap(True)
        left.addWidget(hint)
        self.list = QListWidget()
        self.list.setAccessibleName(tr("Вкладки области"))
        self.list.itemChanged.connect(self._toggled)
        left.addWidget(self.list, 1)
        body.addLayout(left, 3)

        side = QFrame()
        side.setObjectName("tabSettingsSide")
        side_layout = QVBoxLayout(side)
        side_layout.addWidget(self._caption(tr("Порядок")))
        order = QHBoxLayout()
        for label, icon, step in ((tr("Выше"), "arrow_upward", -1), (tr("Ниже"), "arrow_downward", 1)):
            button = QPushButton(material_icon(icon), label)
            button.clicked.connect(lambda _checked=False, step=step: self._move(step))
            order.addWidget(button)
        side_layout.addLayout(order)
        keys = QLabel(tr("Без перетаскивания: ⌥↑ / ⌥↓"))
        side_layout.addWidget(keys)
        QShortcut(QKeySequence("Alt+Up"), self, activated=lambda: self._move(-1))
        QShortcut(QKeySequence("Alt+Down"), self, activated=lambda: self._move(1))

        side_layout.addWidget(self._caption(tr("Плотность таблиц")))
        self.density = Segmented(
            [(value, label) for value, label in DENSITIES], density, tr("Плотность таблиц")
        )
        side_layout.addWidget(self.density)

        side_layout.addWidget(self._caption(tr("Панели")))
        self.right_check = QCheckBox(tr("Сводка справа"))
        self.right_check.setChecked(self._panels["right"])
        self.detail_check = QCheckBox(tr("Детали снизу"))
        self.detail_check.setChecked(self._panels["detail"])
        for check in (self.right_check, self.detail_check):
            check.setEnabled(bool(decks))
            side_layout.addWidget(check)
        self.reset_area = QPushButton(material_icon("restart_alt"), tr("Сбросить этот вид"))
        self.reset_area.clicked.connect(self._reset_area)
        self.reset_all = QPushButton(material_icon("restart_alt"), tr("Сбросить все панели"))
        self.reset_all.clicked.connect(self._reset_all)
        for button in (self.reset_area, self.reset_all):
            button.setEnabled(bool(decks))
            side_layout.addWidget(button)
        side_layout.addStretch(1)
        body.addWidget(side, 2)
        layout.addLayout(body, 1)

        self.feedback = QLabel(tr("Данные проекта не меняются · только вид этого окна"))
        self.feedback.setWordWrap(True)
        layout.addWidget(self.feedback)
        buttons = QDialogButtonBox(QDialogButtonBox.Ok | QDialogButtonBox.Cancel)
        buttons.button(QDialogButtonBox.Ok).setText(tr("Применить"))
        buttons.button(QDialogButtonBox.Cancel).setText(tr("Отмена"))
        buttons.button(QDialogButtonBox.Ok).setEnabled(bool(decks))
        buttons.accepted.connect(self.accept)
        buttons.rejected.connect(self.reject)
        layout.addWidget(buttons)
        self._show_area("main")

    @staticmethod
    def _caption(text):
        label = QLabel(text)
        label.setObjectName("sectionCaption")
        return label

    def _initial_rows(self, area):
        """(id, shown) in display order: the open deck's visible tabs first, the rest hidden after them."""
        specs = next(specs for key, _, specs in AREAS if key == area)
        if not self.decks:
            return [[spec.id, True] for spec in specs]
        visible = list(self.decks[area].visible_ids)
        rest = [spec.id for spec in specs if spec.id not in visible]
        return [[id, True] for id in visible] + [[id, False] for id in rest]

    def _show_area(self, area):
        self._area = area
        self.list.blockSignals(True)
        self.list.clear()
        titles = {spec.id: panel_title(spec) for _, _, specs in AREAS for spec in specs}
        for id, shown in self._rows[area]:
            item = QListWidgetItem(titles[id])
            item.setData(Qt.UserRole, id)
            item.setFlags(item.flags() | Qt.ItemIsUserCheckable)
            item.setCheckState(Qt.Checked if shown else Qt.Unchecked)
            self.list.addItem(item)
        self.list.blockSignals(False)
        self._filter(self.search.text())

    def _filter(self, text):
        for index in range(self.list.count()):
            item = self.list.item(index)
            item.setHidden(text.casefold() not in item.text().casefold())

    def _toggled(self, item):
        row = self.list.row(item)
        self._rows[self._area][row][1] = item.checkState() == Qt.Checked

    def _move(self, step):
        row = self.list.currentRow()
        target = row + step
        rows = self._rows[self._area]
        if row < 0 or not 0 <= target < len(rows):
            return
        rows[row], rows[target] = rows[target], rows[row]
        self._show_area(self._area)
        self.list.setCurrentRow(target)

    def _reset_area(self):
        self._rows[self._area] = [[spec.id, True] for spec in next(s for k, _, s in AREAS if k == self._area)]
        self._show_area(self._area)

    def _reset_all(self):
        for area, _, _ in AREAS:
            self._rows[area] = [[spec.id, True] for spec in next(s for k, _, s in AREAS if k == area)]
        self.right_check.setChecked(True)
        self.detail_check.setChecked(True)
        self._show_area(self._area)

    def accept(self):
        """Apply the view to the open decks; rejected only when a bar would be left without tabs."""
        if any(not any(shown for _, shown in rows) for rows in self._rows.values()):
            self.feedback.setText(tr("Оставьте хотя бы одну видимую вкладку в каждой области."))
            return
        for area, deck in self.decks.items():
            deck.set_visible_tabs(tuple(id for id, shown in self._rows[area] if shown))
        self.decks["right"].setVisible(self.right_check.isChecked())
        self.decks["detail"].setVisible(self.detail_check.isChecked())
        self.on_density(self.density.value())
        super().accept()
