"""Settings → Вид (sheet SetView). The application reacts to these keys through AppSettings.changed."""

from __future__ import annotations

from PyQt5.QtCore import QRectF, QSize, Qt
from PyQt5.QtGui import QColor, QPainter, QPainterPath, QPen
from PyQt5.QtWidgets import (
    QButtonGroup,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QSlider,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from ... import i18n, theming
from ...i18n import tr, trf
from ...settings_store import Setting
from ..controls import Note, SettingRow
from .helpers import column_stack, keyed, page, segmented_row, switch_row

ID, ICON, TITLE = "view", "palette", "Вид"
HINT = "Тема, язык, плотность и раскладка"

THEME_CHOICES = ("light", "dark", "hc", "system")
CARD_W, CARD_H, SAMPLE_H = 152, 142, 84
THEME_NOTES = {"light": "по умолчанию", "dark": "вечером и в презентациях", "hc": "доступность"}

SCHEMA = (
    Setting("view.theme", "light", str, choices=THEME_CHOICES),
    Setting("view.language", "en", str, choices=("en",)),
    Setting("view.density", "standard", str, choices=("compact", "standard", "comfortable")),
    Setting("view.details_position", "bottom", str, choices=("bottom", "right")),
    Setting("view.zoom", 100, int, 80, 150, error="Масштаб — от 80 до 150 %"),
    Setting("view.reduce_motion", False, bool),
    Setting("view.mono_urls", True, bool),
    Setting("view.rail_when_narrow", True, bool),
    Setting("view.status_badges", True, bool),
)


class _ThemeCard(QToolButton):
    """Theme choice drawn from the theme's own tokens: a miniature shell with no data in it."""

    def __init__(self, name):
        super().__init__()
        self.theme_name = name
        self.setCheckable(True)
        self.setCursor(Qt.PointingHandCursor)
        self.setFixedSize(CARD_W, CARD_H)
        self.setAccessibleName(trf("Тема: {name}", name=self._title()))
        i18n.signals.changed.connect(self._language_changed)

    def _language_changed(self, _lang):
        self.setAccessibleName(trf("Тема: {name}", name=self._title()))
        self.update()

    def sizeHint(self):  # Qt override: the global QSS would otherwise shrink the card
        return QSize(CARD_W, CARD_H)

    minimumSizeHint = sizeHint

    def _title(self):
        return theming.theme(self.theme_name)["name"][i18n.language()]

    def paintEvent(self, _event):
        roles = theming.roles(self.theme_name)
        badges = theming.theme(self.theme_name)["badges"]
        painter = QPainter(self)
        painter.setRenderHint(QPainter.Antialiasing)
        checked = self.isChecked()
        frame = QRectF(self.rect()).adjusted(1, 1, -1, -1)
        painter.setPen(
            QPen(QColor(roles["primary" if checked else "divider"]), 2 if checked else 1)
        )
        painter.setBrush(QColor(roles["base"]))
        painter.drawRoundedRect(frame, 10, 10)
        if self.hasFocus():
            painter.setPen(QPen(QColor(roles["ring"]), 2))
            painter.setBrush(Qt.NoBrush)
            painter.drawRoundedRect(QRectF(self.rect()).adjusted(0, 0, -1, -1), 11, 11)

        sample = QRectF(8, 8, CARD_W - 16, SAMPLE_H)
        clip = QPainterPath()
        clip.addRoundedRect(sample, 6, 6)
        painter.save()
        painter.setClipPath(clip)
        painter.fillRect(sample, QColor(roles["surface"]))
        x0, y0, width, height = sample.left(), sample.top(), sample.width(), sample.height()
        painter.fillRect(QRectF(x0, y0, width, 14), QColor(roles["base"]))
        painter.fillRect(QRectF(x0, y0 + 14, width, 1), QColor(roles["divider"]))
        painter.fillRect(QRectF(x0 + width - 30, y0 + 4, 22, 6), QColor(roles["primary"]))
        painter.fillRect(QRectF(x0, y0 + 15, 34, height - 24), QColor(roles["raised"]))
        painter.fillRect(QRectF(x0 + 34, y0 + 15, 1, height - 24), QColor(roles["divider"]))
        for index in range(4):
            top = y0 + 19 + index * 12
            if index == 1:
                painter.fillRect(QRectF(x0 + 2, top - 2, 30, 12), QColor(roles["selected"]))
            painter.fillRect(
                QRectF(x0 + 6, top, 22, 4), QColor(roles["on_selected" if index == 1 else "text_2"])
            )
        painter.fillRect(QRectF(x0 + 35, y0 + 15, width - 35, height - 24), QColor(roles["base"]))
        painter.fillRect(QRectF(x0 + 35, y0 + 15, width - 35, 9), QColor(roles["raised"]))
        for index, kind in enumerate(("ok", "warn", "err")):
            top = y0 + 26 + index * 12
            if index == 1:
                painter.fillRect(
                    QRectF(x0 + 35, top - 2, width - 35, 12), QColor(roles["selected"])
                )
            painter.fillRect(QRectF(x0 + 40, top + 1, 40, 4), QColor(roles["text_muted"]))
            painter.fillRect(QRectF(x0 + width - 28, top, 22, 7), QColor(badges[kind][0]))
            painter.fillRect(QRectF(x0 + width - 24, top + 2, 14, 3), QColor(badges[kind][1]))
            painter.fillRect(
                QRectF(x0 + 35, top + 10, width - 35, 1), QColor(roles["divider_inner"])
            )
        painter.fillRect(QRectF(x0, y0 + height - 9, width, 9), QColor(roles["raised"]))
        painter.fillRect(QRectF(x0, y0 + height - 9, width, 1), QColor(roles["divider"]))
        painter.restore()

        text_width = CARD_W - 16
        name_font = self.font()
        name_font.setBold(True)
        painter.setFont(name_font)
        painter.setPen(QColor(roles["text"]))
        metrics = painter.fontMetrics()
        painter.drawText(
            QRectF(8, 100, text_width, 18),
            Qt.AlignLeft | Qt.AlignVCenter,
            metrics.elidedText(self._title(), Qt.ElideRight, text_width),
        )
        note_font = self.font()
        note_font.setBold(False)
        painter.setFont(note_font)
        painter.setPen(QColor(roles["text_2"]))
        note = tr(THEME_NOTES[self.theme_name]) if self.theme_name in THEME_NOTES else ""
        painter.drawText(
            QRectF(8, 118, text_width, 16),
            Qt.AlignLeft | Qt.AlignVCenter,
            painter.fontMetrics().elidedText(note, Qt.ElideRight, text_width),
        )
        painter.end()


def _theme_picker(store):
    box = QWidget()
    layout = QVBoxLayout(box)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(8)
    cards = QHBoxLayout()
    cards.setSpacing(8)
    cards.setContentsMargins(0, 0, 0, 0)
    group = QButtonGroup(box)
    group.setExclusive(True)
    for value in ("light", "dark", "hc"):
        card = _ThemeCard(value)
        card.setChecked(store.get("view.theme") == value)
        group.addButton(card)
        card.clicked.connect(lambda _c, v=value: store.set("view.theme", v))
        cards.addWidget(card)
    cards.addStretch(1)
    layout.addLayout(cards)
    system = QToolButton()
    system.setProperty("pill", "group")
    system.setCheckable(True)
    system.setText(tr("Как в системе"))
    system.setAccessibleName(trf("Тема: {name}", name=tr("Как в системе")))
    system.setChecked(store.get("view.theme") == "system")
    group.addButton(system)
    system.clicked.connect(lambda _c: store.set("view.theme", "system"))
    system_row = QHBoxLayout()
    system_row.addWidget(system)
    system_row.addStretch(1)
    layout.addLayout(system_row)
    return box


def _zoom_control(store):
    box = QWidget()
    layout = QHBoxLayout(box)
    layout.setContentsMargins(0, 0, 0, 0)
    label = QLabel()

    def refresh():
        label.setText(f"{store.get('view.zoom')} %")

    def step(delta):
        store.set("view.zoom", max(80, min(150, store.get("view.zoom") + delta)))
        refresh()

    down, up = QPushButton("−"), QPushButton("+")
    down.setAccessibleName("Уменьшить масштаб")
    up.setAccessibleName("Увеличить масштаб")
    slider = QSlider(Qt.Horizontal)
    slider.setRange(80, 150)
    slider.setSingleStep(10)
    slider.setPageStep(10)
    slider.setFixedWidth(110)
    slider.setAccessibleName("Масштаб интерфейса")
    slider.valueChanged.connect(lambda value: store.set("view.zoom", value))
    down.clicked.connect(lambda: step(-10))
    up.clicked.connect(lambda: step(10))
    for widget in (down, slider, up):
        layout.addWidget(widget)
    layout.addSpacing(8)
    layout.addWidget(label)

    def sync_slider(_key=None):
        slider.blockSignals(True)
        slider.setValue(store.get("view.zoom"))
        slider.blockSignals(False)
        refresh()

    store.changed.connect(sync_slider)
    sync_slider()
    return box


def _preview(store):
    """Row preview that follows density / mono / badge settings."""
    box = QWidget()
    layout = QVBoxLayout(box)
    layout.setContentsMargins(0, 12, 0, 0)
    title = QLabel("ПРЕДПРОСМОТР СТРОКИ ТАБЛИЦЫ")
    title.setProperty("text_style", "overline")
    layout.addWidget(title)
    row = QLabel()
    row.setProperty("card", "panel")
    row.setTextFormat(Qt.RichText)
    layout.addWidget(row)

    def refresh(_key=None):
        height = {"compact": 28, "standard": 32, "comfortable": 40}[store.get("view.density")]
        row.setFixedHeight(height + 8)
        badge = "200" if not store.get("view.status_badges") else "<b>200</b>"
        family = theming.substitution()["mono_family"] if store.get("view.mono_urls") else "Roboto"
        row.setText(
            trf(
                "&nbsp;{badge} &nbsp; <span style=\"font-family:'{family}'\">https://shop.example.test/catalog/chair-oak/</span>"
                " &nbsp; индексируется · 1,2 с",
                badge=badge,
                family=family,
            )
        )

    store.changed.connect(refresh)
    refresh()
    return box


def build_page(store, context):
    themes = keyed(
        SettingRow(
            "Тема",
            "Системный «высокий контраст» включает контрастную тему сама",
            _theme_picker(store),
        ),
        "view.theme",
    )
    rows = [
        segmented_row(
            store,
            "view.language",
            "Язык интерфейса",
            "Меню, подписи и подсказки. Перезапуск не нужен",
            [("en", "English")],
        ),
        segmented_row(
            store,
            "view.density",
            "Плотность таблиц",
            "Высота строки 28 / 32 / 40 px",
            [("compact", "Плотно"), ("standard", "Стандарт"), ("comfortable", "Просторно")],
        ),
        segmented_row(
            store,
            "view.details_position",
            "Детали URL",
            "Панель выбранного адреса",
            [("bottom", "Снизу"), ("right", "Справа")],
        ),
        keyed(SettingRow("Масштаб интерфейса", "80–150 %", _zoom_control(store)), "view.zoom"),
        switch_row(
            store,
            "view.reduce_motion",
            "Уменьшить движение",
            "Отключает анимации панелей, тостов и меню. По умолчанию — как в системе",
        ),
        switch_row(
            store,
            "view.mono_urls",
            "Моноширинный шрифт для URL",
            "Roboto Mono в колонках адресов и путей",
        ),
        switch_row(
            store,
            "view.rail_when_narrow",
            "Сворачивать навигацию в узком окне",
            "Меньше 900 px — только иконки",
        ),
        switch_row(
            store,
            "view.status_badges",
            "Цветные статусы в таблице",
            "Плашки статусов вместо текста",
        ),
    ]
    note = Note(
        "info",
        "Что это меняет.",
        "Только внешний вид. Данные сканов, фильтры и экспорт не зависят от темы и плотности.",
    )
    return page(themes, column_stack(rows[:4], rows[4:]), _preview(store), note)
