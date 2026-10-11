"""Building blocks shared by the design-v2 screens: honest state panel, KPI card, page header, badge delegate, table styling."""

from __future__ import annotations

from PyQt5.QtCore import QRectF, QSize, Qt
from PyQt5.QtGui import QColor, QFont, QPainter
from PyQt5.QtWidgets import (
    QAbstractItemView,
    QApplication,
    QFrame,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QPushButton,
    QSizePolicy,
    QStackedWidget,
    QStyle,
    QStyledItemDelegate,
    QStyleOptionViewItem,
    QVBoxLayout,
    QWidget,
)

from .. import theming
from ..i18n import tr, trf
from .icons import MaterialIconLabel

STATE_ICONS = {"empty": "inbox", "loading": "hourglass_top", "error": "error", "partial": "incomplete_circle", "waiting": "schedule"}
UNAVAILABLE = "Недоступно в этой версии ядра"
# Core issue number -> what will appear once the core gives it. Numbers live in code only; the UI shows the hint.
ISSUE_HINTS = {
    920: "Запуск скана списка URL из приложения и его появление в наблюдении проекта",
    921: "Живой ход скана: скорость, ошибки, оставшееся время, пауза и остановка чужого запуска",
    922: "Исполнитель, описание, комментарии и следующий шаг задачи",
    923: "Журнал событий проекта: кто и что сделал (агент, вы, приложение)",
    924: "Список ссылок страницы: источник, анкор, rel, HTTP цели",
    926: "Создание задач из находок и перепроверка URL задач",
    927: "Сортировка и фильтры по всем сканам на стороне ядра",
    929: "Включение локального MCP-сервера из приложения",
    930: "Цепочка редиректов одного URL",
    931: "Оценка длительности и размера скана до запуска",
    933: "Хвост сохранённых страниц с временем получения",
    974: "Изображения, CSS и JS страницы со статусом и весом",
    936: "Исходник сохранённой страницы",
    938: "Сравнение по сегментам сайта и KPI до/после: медиана слов и ссылок по разделам",
    939: "Точный поиск в HTML на стороне ядра: фильтр и число вхождений",
    940: "Расписание сканов",
    941: "Именованные профили скана",
    942: "Краул без проекта: журнал запуска и ограничение скорости по хосту",
    944: "Вопрос агенту и его ответ во входящих",
    946: "План аудита: знаменатель выполнения и время принятия цели",
    947: "Настройки проекта",
    948: "Структурированные данные страницы: JSON-LD, microdata, ошибки разметки",
    950: "Проверка браузера для рендеринга до запуска скана",
    954: "Редактор правил извлечения данных",
    956: "Тайминги ответа: DNS, соединение, TLS, TTFB, загрузка",
    971: "Счётчик и список входящих ссылок страницы",
    972: "История URL по сканам",
    973: "Позиция ссылки на странице",
    975: "Граф ссылок вокруг страницы",
    979: "Версия ядра и совместимость",
    980: "Находки скана по проверкам и по одному URL",
    981: "Полный фильтр находок по проверке",
    990: "Выбор ресурса проекта у сервиса, время синхронизации и сохранение связей в проекте",
    991: "Количество страниц в каждой группе одним запросом",
    1163: "Результат проверки целостности базы скана и точка восстановления без полного открытия",
    992: "Глубина кликов от главной: сейчас ядро считает от стартовых адресов, а адреса из sitemap стартовые",
    1208: "Состояние стадий ядра, смета платных стадий и журнал запусков семантики проекта",
    1220: "Проверка обновлений, загрузка по запросу и журнал изменений приложения и ядра",
    999: "Профиль внешних ссылок проекта: ссылающиеся домены, ссылки и анкоры из подключённых источников",
    1213: "Размер сканов каждого проекта на диске",
    1205: "Импорт и экспорт ядра: разбор файла и превью, сопоставление колонок, сводка дублей до записи, выгрузка со счётчиками",
    1238: "Агент: когда был на связи и подключён ли сейчас",
}


class StatePanel(QFrame):
    """Empty / loading / error / partial / waiting state with an optional next-step button. Never shows a number.

    ``kind="waiting"`` shows the neutral «Недоступно в этой версии ядра» badge (tooltip: ``hint``), so the state is
    truthful about why nothing is shown instead of pretending the data is empty. ``issue`` is kept for code only.
    """

    def __init__(self, kind, title, text="", action=None, issue=None, parent=None, secondary=None, hint=""):
        super().__init__(parent)
        self.kind = kind
        self.setProperty("state_panel", kind)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(24, 32, 24, 32)
        layout.setSpacing(8)
        layout.setAlignment(Qt.AlignCenter)
        layout.addWidget(MaterialIconLabel(STATE_ICONS[kind], 32, color="role:text_muted"), 0, Qt.AlignHCenter)
        self.title = QLabel(tr(title))
        self.title.setProperty("text_style", "section")
        self.title.setAlignment(Qt.AlignCenter)
        self.title.setWordWrap(True)
        layout.addWidget(self.title)
        self.text = QLabel(tr(text))
        self.text.setProperty("text_style", "meta")
        self.text.setAlignment(Qt.AlignCenter)
        self.text.setWordWrap(True)
        self.text.setVisible(bool(text))
        layout.addWidget(self.text)
        self.issue_label = None
        if issue is not None:
            self.issue_label = waiting_badge(issue, hint)
            layout.addWidget(self.issue_label, 0, Qt.AlignHCenter)
        self.action = None
        if action is not None:
            label, callback = action
            self.action = QPushButton(tr(label))
            self.action.setProperty("role", "primary")
            self.action.clicked.connect(callback)
            layout.addWidget(self.action, 0, Qt.AlignHCenter)
        self.secondary = None
        if secondary is not None:
            label, callback = secondary
            self.secondary = QPushButton(tr(label))
            self.secondary.clicked.connect(callback)
            layout.addWidget(self.secondary, 0, Qt.AlignHCenter)


def no_project_panel(host, text):
    """«Проект не открыт» with both ways forward: open an existing project or create a new one."""
    return StatePanel("empty", "Проект не открыт", text, action=("Открыть проект…", host.choose_project),
                      secondary=("Создать проект", lambda: host.extra_screens["start"].new_project()))


class Gate(QStackedWidget):
    """Shows a legacy page only while it has real core data; otherwise an honest state (never a sample).

    ``state()`` returns "content", "open" (no project: «Откройте проект») or "partial" (screen not rebuilt yet).
    """

    def __init__(self, content, state, open_project, parent=None):
        super().__init__(parent)
        self.content = content
        self._state = state
        self.open_panel = StatePanel("empty", "Откройте проект", "Данные появятся из сохранённого проекта.",
                                     action=("Открыть проект…", open_project))
        self.partial_panel = StatePanel("partial", "Экран переделывается — ждёт шага 5",
                                        "Сохранённые данные проекта доступны в разделах «Сканы» и «Работа».")
        for widget in (content, self.open_panel, self.partial_panel):
            self.addWidget(widget)
        self.refresh()

    def refresh(self):
        self.setCurrentWidget({"open": self.open_panel, "partial": self.partial_panel}.get(self._state(), self.content))


class UnavailableBadge(QLabel):
    """Pill «Недоступно в этой версии ядра» that falls back, by whole words, to «Недоступно» when the row is narrow."""

    PAD = 20  # 8 + 8 padding and the border, see QLabel[badge] in theme.qss

    def __init__(self, parent=None):
        super().__init__(parent)
        self.full = tr(UNAVAILABLE)
        self.short = tr("Недоступно")
        self.setProperty("badge", "mut")
        self.setText(self.full)
        self.setSizePolicy(QSizePolicy.Preferred, QSizePolicy.Fixed)

    def _width(self, text):
        return self.fontMetrics().horizontalAdvance(text) + self.PAD

    def sizeHint(self):
        hint = super().sizeHint()
        hint.setWidth(self._width(self.full))
        return hint

    def minimumSizeHint(self):
        hint = super().minimumSizeHint()
        hint.setWidth(self._width(self.short))
        return hint

    def resizeEvent(self, event):
        super().resizeEvent(event)
        text = self.full if self.width() >= self._width(self.full) else self.short
        if text != self.text():
            self.setText(text)


def waiting_badge(issue, hint=""):
    """Neutral pill «Недоступно в этой версии ядра»; the tooltip says what will appear.

    ``issue`` is the core issue number: kept for code and tests (Qt property ``waiting_issue``), never shown.
    ``hint`` defaults to the ISSUE_HINTS entry of that issue. In a narrow row the pill shortens to «Недоступно».
    """
    label = UnavailableBadge()
    label.setProperty("waiting_issue", issue)
    hint = hint or ISSUE_HINTS.get(issue, "")
    label.setToolTip(trf("{state}\n{soon}: {hint}", state=UNAVAILABLE, soon="Появится", hint=hint) if hint else tr(UNAVAILABLE))
    return label


def unavailable_tip(text, issue=None):
    """Tooltip «<text> · Недоступно в этой версии ядра» (no issue number)."""
    return f"{tr(text)} · {tr(UNAVAILABLE)}" if text else tr(UNAVAILABLE)


class Kpi(QFrame):
    """Caption, value (22/500) and optional sub line. ``value`` None means not measured: «Нет данных», never 0."""

    def __init__(self, label, value=None, sub="", parent=None):
        super().__init__(parent)
        self.setProperty("kpi", True)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(14, 12, 14, 12)
        layout.setSpacing(4)
        self.caption = QLabel(tr(label))
        self.caption.setProperty("kpi_part", "label")
        self.number = QLabel()
        self.number.setProperty("kpi_part", "value")
        self.sub = QLabel()
        self.sub.setProperty("text_style", "meta")
        for widget in (self.caption, self.number, self.sub):
            layout.addWidget(widget)
        self.set_value(value, sub)

    def set_value(self, value, sub=""):
        measured = value is not None
        self.number.setText(str(value) if measured else tr("Нет данных"))
        self.number.setProperty("na", not measured)
        self.number.style().unpolish(self.number)
        self.number.style().polish(self.number)
        self.sub.setText(sub)
        self.sub.setVisible(bool(sub))


class PageHeader(QWidget):
    """Screen title (20/500) with a meta line and a right-aligned row of actions."""

    def __init__(self, title, meta="", parent=None):
        super().__init__(parent)
        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(8)
        texts = QVBoxLayout()
        texts.setSpacing(2)
        self.title = QLabel(tr(title))
        self.title.setProperty("text_style", "title")
        self.meta = QLabel(meta)
        self.meta.setProperty("text_style", "meta")
        self.meta.setVisible(bool(meta))
        texts.addWidget(self.title)
        texts.addWidget(self.meta)
        layout.addLayout(texts, 1)
        self.actions = QHBoxLayout()
        self.actions.setSpacing(8)
        layout.addLayout(self.actions)

    def add_action(self, widget):
        self.actions.addWidget(widget)
        return widget

    def set_meta(self, text):
        self.meta.setText(text)
        self.meta.setVisible(bool(text))


BADGE_ROLE = Qt.UserRole + 40  # (kind, text) pair for BadgeDelegate


class BadgeDelegate(QStyledItemDelegate):
    """Draws a status badge (22 px, radius 6) from the item's BADGE_ROLE value; plain text when the role is empty.

    Badges are painted by the delegate (never a widget per cell). The text always accompanies the colour.
    """

    def paint(self, painter, option, index):
        value = index.data(BADGE_ROLE)
        if not value:
            return super().paint(painter, option, index)
        kind, text = value
        pair = theming.theme()["badges"].get(kind) or theming.theme()["badges"]["mut"]
        background, ink = pair
        painter.save()
        painter.setRenderHint(QPainter.Antialiasing)
        cell = QStyleOptionViewItem(option)
        self.initStyleOption(cell, index)
        cell.text = ""
        (option.widget.style() if option.widget else QApplication.style()).drawControl(QStyle.CE_ItemViewItem, cell, painter, option.widget)
        font = QFont(painter.font())
        font.setPixelSize(12)
        font.setWeight(QFont.Medium)
        painter.setFont(font)
        width = painter.fontMetrics().horizontalAdvance(text) + 16
        rect = QRectF(option.rect.left() + 10, option.rect.center().y() - 11, min(width, option.rect.width() - 20), 22)
        painter.setPen(Qt.NoPen)
        painter.setBrush(QColor(background))
        painter.drawRoundedRect(rect, 6, 6)
        painter.setPen(QColor(ink))
        painter.drawText(rect, Qt.AlignCenter, painter.fontMetrics().elidedText(text, Qt.ElideRight, int(rect.width()) - 8))
        painter.restore()

    def sizeHint(self, option, index):
        return QSize(option.rect.width(), theming.metrics()["row"]["standard"])


def style_table(table, density="standard"):
    """Apply the v2 table contract: row height by density, zebra, no grid, row selection, per-pixel scrolling."""
    table.setAlternatingRowColors(True)
    table.setSelectionBehavior(QAbstractItemView.SelectRows)
    table.setSelectionMode(QAbstractItemView.SingleSelection)
    table.setEditTriggers(QAbstractItemView.NoEditTriggers)
    table.setShowGrid(False)
    table.setWordWrap(False)
    table.verticalHeader().setVisible(False)
    table.verticalHeader().setDefaultSectionSize(theming.metrics()["row"][density])
    table.setHorizontalScrollMode(QAbstractItemView.ScrollPerPixel)
    table.setVerticalScrollMode(QAbstractItemView.ScrollPerPixel)
    header = table.horizontalHeader()
    header.setSectionResizeMode(QHeaderView.Interactive)
    header.setStretchLastSection(True)
    header.setDefaultAlignment(Qt.AlignLeft | Qt.AlignVCenter)
    header.setFixedHeight(theming.metrics()["row"]["header"])
    return table


def clear_layout(layout):
    while layout.count():
        item = layout.takeAt(0)
        widget = item.widget()
        if widget is not None:
            widget.setParent(None)
            widget.deleteLater()
        elif item.layout() is not None:
            clear_layout(item.layout())


def show_empty(holder, content, panel):
    """Put ``panel`` into the empty holder and hide ``content``; with no panel, hide the holder instead."""
    clear_layout(holder)
    margin = 24 if panel is not None else 0
    holder.setContentsMargins(margin, margin, margin, margin)
    if panel is not None:
        holder.addWidget(panel)
    content.setVisible(panel is None)


def set_panel_state(owner, kind, panel=None):
    """Show ``panel`` as the state of ``owner`` (its stack, state_layout, panel_state); replace it only when ``kind`` changes."""
    if kind != owner.panel_state:
        owner.panel_state = kind
        while owner.state_layout.count():
            item = owner.state_layout.takeAt(0)
            if item.widget():
                item.widget().deleteLater()
        if panel is not None:
            owner.state_layout.addWidget(panel)
    owner.stack.setCurrentIndex(1 if panel is not None else 0)
