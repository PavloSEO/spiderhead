"""Six bounded summary blocks for an existing, offline comparison page."""

from collections.abc import Mapping

from PyQt5.QtCore import QSize, Qt, pyqtSignal
from PyQt5.QtWidgets import (
    QGridLayout,
    QLabel,
    QPlainTextEdit,
    QPushButton,
    QSizePolicy,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .components import ASSET_ROOT, material_icon
from .icons import MaterialIconLabel
from .presentation import StateBadge, content_spacing, readable_record, theme_tokens

STATUSES = (
    ("resolved", "Исправлено", "Подтверждено проверкой", "check_circle", "success"),
    ("persisting", "Сохранилось", "Находка осталась после проверки", "pause_circle", "neutral"),
    ("changed", "Изменилось", "Находка изменилась", "change_circle", "active"),
    ("not_verifiable", "Нельзя подтвердить", "Недостаточно данных для вывода", "help", "warning"),
    ("new", "Новые", "Новое наблюдение; регрессия не подтверждена", "add_circle", "active"),
)


def _count(value):
    return value if type(value) is int and value >= 0 else None


def _number(value):
    return f"{value:,}".replace(",", "\u202f") if value is not None else "—"


class _MetricCard(QPushButton):
    """A native focusable button with a fixed number of transparent labels."""

    def __init__(self, state, title, note, icon, tone, parent):
        super().__init__(parent)
        self.setObjectName("comparisonMetric")
        self.setProperty("state", state)
        self.setProperty("tone", tone)
        self.setCheckable(True)
        self.setMinimumHeight(96)
        self.setFocusPolicy(Qt.StrongFocus)
        self.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Preferred)
        self.title, self.note = title, note
        layout = QGridLayout(self)
        layout.setContentsMargins(12, 8, 12, 8)
        layout.setHorizontalSpacing(6)
        layout.setVerticalSpacing(3)
        self.symbol = MaterialIconLabel(icon, size=18, parent=self, color=theme_tokens()["colors"][{"success": "success", "warning": "on_warning_container", "active": "primary"}.get(tone, "on_surface_variant")])
        has_icon = (ASSET_ROOT / f"{icon}.svg").is_file()
        self.symbol.setFixedSize(18, 18)
        self.symbol.setVisible(has_icon)
        label = QLabel(title)
        label.setObjectName("comparisonStep")
        label.setWordWrap(True)
        self.value = QLabel("—")
        self.value.setObjectName("comparisonValue")
        self.value.setProperty("role", "comparisonMetricValue")
        layout.addWidget(self.symbol, 0, 0, Qt.AlignTop)
        layout.addWidget(label, 0, 1 if has_icon else 0, 1, 1 if has_icon else 2)
        layout.addWidget(self.value, 1, 0, 1, 2)
        layout.setColumnStretch(1, 1)
        for child in (self.symbol, label, self.value):
            child.setTextFormat(Qt.PlainText)
            child.setAttribute(Qt.WA_TransparentForMouseEvents)

    def sizeHint(self):
        return self.layout().sizeHint().expandedTo(QSize(112, 72))

    def minimumSizeHint(self):
        return QSize(112, self.layout().minimumSize().height())

    def set_count(self, count, scope):
        self.value.setText(_number(count))
        measured = _number(count) if count is not None else "не измерено"
        self.setAccessibleName(f"{self.title}: {measured}. {scope}. {self.note}")
        self.setToolTip(
            f"{self.note}. {scope}.\n"
            + ("Показать этот статус в текущей странице таблицы" if count else
               "На этой странице нет таких находок" if count == 0 else "Результат ещё не измерен")
        )
        self.setEnabled(count is not None and count > 0)


class ComparisonSummary(QWidget):
    """Display core counts; emit explicit filters for the already loaded page.

    ``set_payload`` accepts ComparisonController.changed payloads. Missing values
    stay unknown; omitted Counter keys become zero only for a fully accounted
    page. ``filterRequested`` emits one of the five core statuses in STATUSES.
    The owner applies that filter and may synchronize ``set_active_filter``.
    This widget never reads packages, runs verification, or requests another page.
    """

    filterRequested = pyqtSignal(str)

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setObjectName("comparisonSummary")
        self.setAccessibleName("Сводка сравнения сохранённых сканов")
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(8)
        self.delta = QWidget()
        self.delta.setObjectName("comparisonDelta")
        delta_layout = self.delta_layout = QGridLayout(self.delta)
        delta_layout.setContentsMargins(0, 0, 0, 0)
        delta_layout.setHorizontalSpacing(8)
        delta_layout.setVerticalSpacing(4)
        self.whole_scope = QLabel()
        self.whole_scope.setObjectName("comparisonStep")
        self.whole_scope.setWordWrap(True)
        self.coverage_badge = StateBadge()
        self.coverage_badge.setWordWrap(True)
        self.coverage_badge.setMinimumHeight(28)
        self.details_toggle = QToolButton()
        self.details_toggle.setText("Как читать сравнение")
        self.details_toggle.setProperty("role", "quiet")
        self.details_toggle.setCheckable(True)
        self.details_toggle.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
        self.details_toggle.setIcon(material_icon("chevron_down"))
        self.details_toggle.setFocusPolicy(Qt.StrongFocus)
        self.raw_summary = QLabel()
        self.raw_summary.setObjectName("comparisonDeltaCounts")
        self.raw_summary.setWordWrap(True)
        delta_layout.addWidget(self.whole_scope, 0, 0)
        delta_layout.addWidget(self.coverage_badge, 0, 1)
        delta_layout.addWidget(self.details_toggle, 0, 2)
        delta_layout.addWidget(self.raw_summary, 1, 0, 1, 3)
        delta_layout.setColumnStretch(0, 1)
        layout.addWidget(self.delta)
        self.state_line = QLabel()
        self.state_line.setObjectName("comparisonStep")
        self.state_line.setWordWrap(True)
        self.state_line.setTextFormat(Qt.PlainText)
        self.state_line.hide()
        layout.addWidget(self.state_line)
        self.page_scope = QLabel()
        self.page_scope.setObjectName("comparisonStep")
        self.page_scope.setWordWrap(True)
        layout.addWidget(self.page_scope)
        self.cards = QGridLayout()
        self.cards.setContentsMargins(0, 0, 0, 0)
        self.cards.setSpacing(12)
        self.metric_buttons = {}
        for state, title, note, icon, tone in STATUSES:
            button = _MetricCard(state, title, note, icon, tone, self)
            button.clicked.connect(lambda _checked, state=state: self._request_filter(state))
            self.metric_buttons[state] = button
        layout.addLayout(self.cards)
        self.details = QPlainTextEdit()
        self.details.setReadOnly(True)
        self.details.setAccessibleName("Как читать сравнение: ограничения и источники")
        self.details.setMaximumHeight(144)
        self.details.hide()
        layout.addWidget(self.details)
        self.details_toggle.toggled.connect(self._show_details)
        for label in (self.whole_scope, self.page_scope, self.raw_summary):
            label.setTextFormat(Qt.PlainText)
        self._columns = 0
        self._narrow = None
        self._arrange_cards()
        self.set_payload({})

    def set_compact(self, compact):
        self.raw_summary.hide()
        self.details_toggle.setToolButtonStyle(Qt.ToolButtonIconOnly if compact else Qt.ToolButtonTextBesideIcon)
        self.details_toggle.setToolTip("Как читать сравнение: полная сводка и ограничения")

    def minimumSizeHint(self):
        size = super().minimumSizeHint()
        size.setWidth(304)
        return size

    def _arrange_cards(self):
        width = self.width()
        _margin, spacing = content_spacing(width)
        self.layout().setSpacing(spacing)
        narrow = width < 650
        if narrow != self._narrow:
            self._narrow = narrow
            for widget in (self.whole_scope, self.coverage_badge, self.details_toggle, self.raw_summary):
                self.delta_layout.removeWidget(widget)
            self.delta_layout.addWidget(self.whole_scope, 0, 0, 1, 3 if narrow else 1)
            self.delta_layout.addWidget(self.coverage_badge, 1 if narrow else 0, 0 if narrow else 1)
            self.delta_layout.addWidget(self.details_toggle, 1 if narrow else 0, 1 if narrow else 2, 1, 2 if narrow else 1)
            self.delta_layout.addWidget(self.raw_summary, 2 if narrow else 1, 0, 1, 3)
        columns = 5 if width >= 630 else 3 if width >= 400 else 2 if width >= 304 else 1
        if columns == self._columns:
            return
        for column in range(5):
            self.cards.setColumnStretch(column, 0)
        for index, button in enumerate(self.metric_buttons.values()):
            self.cards.removeWidget(button)
            self.cards.addWidget(button, index // columns, index % columns)
        for column in range(columns):
            self.cards.setColumnStretch(column, 1)
        self._columns = columns

    def resizeEvent(self, event):
        self._arrange_cards()
        super().resizeEvent(event)

    def _request_filter(self, state):
        self.set_active_filter(state)
        self.filterRequested.emit(state)

    def set_active_filter(self, state):
        for key, button in self.metric_buttons.items():
            button.setChecked(key == state and button.isEnabled())

    def _show_details(self, visible):
        self.details.setVisible(visible)
        self.details_toggle.setIcon(material_icon("chevron_up" if visible else "chevron_down"))
        self.details_toggle.setAccessibleName(
            ("Свернуть" if visible else "Раскрыть") + " пояснения, ограничения и источники сравнения"
        )

    def set_payload(self, payload):
        ready = payload.get("state") == "ready"
        summary = payload.get("summary") or {}
        delta = summary.get("delta") or {} if ready else {}
        verified = summary.get("verified_page") if ready else None
        rows = payload.get("rows") if ready else None
        count = len(rows) if isinstance(rows, (list, tuple)) else None
        total = _count(payload.get("total")) if ready else None
        offset = _count(payload.get("offset")) if ready else None
        before, after = (payload.get(key) or {} for key in ("before", "after"))
        source_scope = f"{_number(_count(before.get('urls_crawled')))} → {_number(_count(after.get('urls_crawled')))} URL"
        self.whole_scope.setText(f"Все наблюдения · {_number(total)} находок · {source_scope}" if ready else "Все наблюдения · ещё не измерено")
        entered, appeared = _count(delta.get("entered")), _count(delta.get("appeared"))
        new = entered + appeared if entered is not None and appeared is not None else None
        self.raw_summary.hide()
        self.raw_summary.setText(
            f"Не обнаружены: {_number(_count(delta.get('left')))}   ·   Новые: {_number(new)}   ·   "
            f"В обоих: {_number(_count(delta.get('unchanged')))}   ·   На отсутствующих URL: {_number(_count(delta.get('disappeared')))}"
        )
        extent = f"{offset + 1}–{offset + count}" if count and offset is not None else "0" if count == 0 else "—"
        scope = f"Строки {extent} из {_number(total)}; только текущая страница"
        self.page_scope.setText("Перепроверка · " + scope if ready else "Перепроверка страницы · ещё не измерено")
        complete = (
            isinstance(verified, Mapping) and count is not None
            and all(_count(value) is not None for value in verified.values())
            and sum(verified.values()) == count
        )
        for state, button in self.metric_buttons.items():
            value = _count(verified.get(state, 0 if complete else None)) if isinstance(verified, Mapping) else None
            button.set_count(value, scope if ready else "Текущая страница ещё не измерена")
        other = sum(value for key, value in verified.items() if key not in self.metric_buttons) if complete else 0
        if other:
            self.page_scope.setText(self.page_scope.text() + f" · Другие статусы: {_number(other)}")
        self.set_active_filter("all")
        reason = str(payload.get("reason") or "").strip()[:800]
        self.state_line.setText(reason)
        self.state_line.setVisible(not ready and bool(reason))
        warnings = [str(item)[:800] for item in (payload.get("warnings") or [])[:20]] if ready else []
        compatibility = payload.get("compatibility") or [] if ready else []
        partial = warnings or any(source.get("crawl_partial") or source.get("corpus_partial") for source in (before, after))
        compatible = bool(compatibility) and all(item.get("state") == "compatible" for item in compatibility)
        badge_state, badge_text = (
            ("unknown", "Покрытие не измерено") if not ready else
            ("partial", "Охват ограничен") if partial else
            ("retained", "Совместимость подтверждена") if compatible else
            ("review", "Совместимость не подтверждена")
        )
        self.coverage_badge.set_state(badge_state)
        self.coverage_badge.setText(badge_text)
        self.coverage_badge.setAccessibleName(badge_text)
        self.coverage_badge.setToolTip("Ограничения и исходные сведения доступны в «Как читать сравнение»")
        explanation = (
            "Все наблюдения — находки в сравнении двух сохранённых сканов. «Не обнаружены» не означает «исправлены».\n\n"
            "Карточки показывают результаты перепроверки только загруженной страницы. Нажатие фильтрует эту страницу. "
            "«Исправлено» подтверждено проверкой; «Новые» означает новое наблюдение, а не подтверждённую регрессию. "
            "Ноль — измеренное отсутствие; тире — результат не измерен.\n\n"
            f"Новые на известных URL: {_number(entered)}. На новых URL: {_number(appeared)}.\n\n"
        )
        source = {"До": before, "После": after, "Совместимость": compatibility,
                  "Пакет сравнения": payload.get("package") if ready else None}
        self.details.setPlainText(self.raw_summary.text() + "\n\n" + explanation + ("Ограничения охвата\n" + "\n\n".join(warnings) + "\n\n" if warnings else "")
                                  + readable_record(source, heading="Источники", max_lines=45))
        self.details_toggle.setChecked(False)
        self._show_details(False)
