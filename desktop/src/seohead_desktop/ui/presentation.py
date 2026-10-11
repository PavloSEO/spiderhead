"""Small, bounded display helpers; all values remain owned by the core."""

from collections.abc import Mapping
from datetime import datetime
from itertools import islice

from PyQt5.QtCore import QPointF, QRectF, QSize, Qt, pyqtSignal
from PyQt5.QtGui import QColor, QPainter, QPalette, QPen
from PyQt5.QtWidgets import (
    QCheckBox,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QSizePolicy,
    QSplitter,
    QSplitterHandle,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import theming


def theme_tokens():
    """Pre-v2 token shape filled from the active v2 theme (see seohead_desktop.theming)."""
    return theming.legacy_tokens(theming.active_theme())


class SwitchCheckBox(QCheckBox):
    """A rounded indicator; native checkbox input, state and accessibility."""

    def sizeHint(self):
        return QSize(
            self.fontMetrics().horizontalAdvance(self.text()) + 44,
            max(28, self.fontMetrics().height() + 8),
        )

    def hitButton(self, position):
        return self.rect().contains(position)

    def paintEvent(self, event):
        colors = theme_tokens()["colors"]
        painter = QPainter(self)
        painter.setRenderHint(QPainter.Antialiasing)
        track = QRectF(3, (self.height() - 18) / 2, 32, 18)
        if self.hasFocus():
            painter.setPen(QPen(QColor(colors["primary"]), 1.5))
            painter.setBrush(Qt.NoBrush)
            painter.drawRoundedRect(track.adjusted(-2, -2, 2, 2), 11, 11)
        painter.setPen(Qt.NoPen)
        painter.setBrush(
            QColor(
                colors["primary"]
                if self.isChecked() and self.isEnabled()
                else colors["outline_variant"]
            )
        )
        painter.drawRoundedRect(track, 9, 9)
        painter.setBrush(
            QColor(colors["on_primary"] if self.isEnabled() else colors["surface_container_high"])
        )
        painter.drawEllipse(
            QRectF(track.left() + (16 if self.isChecked() else 2), track.top() + 2, 14, 14)
        )
        self.style().drawItemText(
            painter,
            self.rect().adjusted(44, 0, 0, 0),
            Qt.AlignLeft | Qt.AlignVCenter,
            self.palette(),
            self.isEnabled(),
            self.text(),
            QPalette.WindowText,
        )


STATES = {
    "resolved": "Исправлено · проверено",
    "persisting": "Сохранилось",
    "changed": "Изменилось",
    "not_verifiable": "Нельзя подтвердить",
    "new": "Новая находка",
    "not_initialized": "План не настроен",
    "unknown": "Неизвестно",
    "not_agreed": "Не согласовано",
    "remaining": "Ожидает выполнения",
    "started": "Начало запуска",
    "entered": "Вход в этап",
    "progress": "Обновление счётчиков",
    "running": "Выполняется",
    "starting": "Запускается",
    "queued": "В очереди",
    "completed": "Выполнено",
    "complete": "Полные данные",
    "finished": "Завершён",
    "blocked": "Заблокировано",
    "stale": "Устарело",
    "unavailable": "Недоступно",
    "review": "Нужна проверка",
    "deliverable": "Результат подготовлен",
    "excluded": "Исключено",
    "partial": "Частичный результат",
    "interrupted": "Прерван",
    "failed": "Ошибка",
    "error": "Ошибка",
    "stop_requested": "Остановка запрошена",
    "cancel_requested": "Отмена запрошена",
    "cancelled": "Отменён",
    "cancelled_before_start": "Отменён до старта",
    "awaiting_core_status": "Ожидание статуса ядра",
    "status_unavailable": "Статус не подтверждён",
    "rejected": "Отклонено",
    "available": "Доступно",
    "fresh": "Актуально",
    "retained": "Сохранённое измерение",
    "idle": "Нет активной работы",
    "not_requested": "Не запрашивалось",
    "unsupported": "Не поддерживается",
    "proposed": "Предложено",
    "accepted": "Принято",
    "acknowledged": "Обработка подтверждена",
    "read": "Прочитано",
    "unread": "Не прочитано",
    "triaged": "Разобрано",
    "measured": "Измерено",
    "native": "Native",
    "sitemap": "Sitemap",
    "sf": "Screaming Frog",
    "crawl": "Новый скан",
    "resume": "Продолжение",
    "raw": "Исходный HTML",
    "js": "JavaScript",
    "admission": "Проверка лимитов",
    "collection": "Сбор",
    "analysis": "Анализ",
    "finalizing": "Сохранение результата",
    "note": "Заметка",
    "question": "Вопрос",
    "proposed_goal": "Предложенная цель",
    "specialist": "Специалист",
    "agent": "Агент",
    "primary": "Основной сайт",
    "competitor": "Конкурент",
    "check": "Проверка",
    "custom": "Задача",
    "method": "Метод",
    "scope": "Объём работ",
    "Indexable": "Индексируется",
    "Non-indexable": "Не индексируется",
    "Redirect": "Редирект",
}

FIELDS = {
    "id": "ID",
    "uuid": "ID скана",
    "scan_uuid": "ID скана",
    "project_uuid": "ID проекта",
    "title": "Title",
    "text": "Текст",
    "state": "Состояние",
    "display_state": "Состояние",
    "reason": "Причина",
    "status_reason": "Причина статуса",
    "kind": "Тип",
    "scope": "Объём",
    "url": "URL",
    "start_url": "Начальный URL",
    "target": "Цель",
    "path": "Файл",
    "artifact": "Сохранённый результат",
    "lifecycle": "Состояние скана",
    "source_kind": "Источник",
    "created_at": "Создано",
    "started_at": "Начало",
    "finished_at": "Завершено",
    "observed_at": "Наблюдение",
    "sampled_at": "Измерение",
    "updated_at": "Обновлено",
    "status_code": "HTTP",
    "status": "HTTP",
    "content_type": "Тип содержимого",
    "type": "Тип",
    "indexability": "Индексация",
    "indexable": "Индексируется",
    "meta_description": "Description",
    "h1": "H1",
    "h2": "H2",
    "canonical": "Canonical",
    "meta_robots": "Meta robots",
    "x_robots": "X-Robots-Tag",
    "response_time": "Время ответа, с",
    "size_bytes": "Размер, байт",
    "word_count": "Слов",
    "crawl_depth": "Глубина",
    "outlinks": "Исходящих ссылок",
    "external_outlinks": "Внешних ссылок",
    "representation": "Представление",
    "final_url": "Конечный URL",
    "redirect_url": "Редирект",
    "url_id": "ID URL",
    "document_id": "ID документа",
    "evidence_revision": "Ревизия данных",
    "format_version": "Формат",
    "writer_version": "Версия ядра",
    "writer_revision": "Сборка ядра",
    "crawl_partial": "Частичный сбор",
    "corpus_partial": "Частичный корпус",
    "finish_reason": "Причина завершения",
    "source": "Источник",
    "config_fingerprint": "Конфигурация",
    "revision": "Ревизия",
    "priority": "Приоритет",
    "next_action": "Следующий шаг",
    "action": "Действие",
    "definition": "Определение",
    "evidence": "Доказательства",
    "consumer": "Получатель",
    "author_role": "Автор",
    "goal_state": "Состояние цели",
    "task": "Задача",
    "item": "Пункт",
    "events": "События",
    "phase": "Этап",
    "code": "Событие",
    "at": "Время",
    "complete": "Завершено",
    "remaining": "Осталось",
    "stale": "Устарело",
    "total": "Всего",
    "done": "Обработано",
    "fetched": "Получено",
    "queued": "В очереди",
    "inflight": "В работе",
    "excluded": "Исключено",
    "failed": "С ошибкой",
    "max_urls": "Лимит URL",
    "max_requests": "Лимит HTTP-запросов",
    "max_crawl_seconds": "Лимит времени, с",
    "max_requests_per_second": "Лимит запросов/с",
    "rate_per_second": "Последняя измеренная скорость",
    "rate_window_seconds": "Окно измерения, с",
    "age_seconds": "Возраст измерения, с",
    "unit": "Единица",
    "telemetry": "Измерения",
    "controller": "Контроллер",
    "collector": "Сборщик",
    "counters": "Счётчики",
    "core_state": "Состояние ядра",
    "core_run_id": "ID запуска ядра",
    "observer_run_id": "ID наблюдения",
    "rendering_mode": "Режим",
    "owned": "Запущен этим окном",
    "resume_path": "Продолжение из файла",
    "offset": "Смещение",
    "has_more": "Есть следующая страница",
    "bytes": "Размер ответа, байт",
    "truncated": "Ответ ограничен",
    "next_offset": "Следующее смещение",
}
PANEL_LABELS = {
    "internal": "Внутренние",
    "external": "Внешние",
    "security": "Безопасность",
    "response_codes": "Ответы",
    "page_titles": "Title",
    "meta_description": "Description",
    "meta_keywords": "Keywords",
    "content": "Контент",
    "images": "Изображения",
    "canonicals": "Canonical",
    "pagination": "Пагинация",
    "directives": "Директивы",
    "links": "Ссылки",
    "structured_data": "Разметка",
    "sitemaps": "Sitemap",
    "validation": "Валидация",
    "url_details": "Сведения",
    "inlinks": "Входящие",
    "outlinks": "Исходящие",
    "image_details": "Изображение",
    "resources": "Ресурсы",
    "serp_snippet": "Сниппет",
    "view_source": "Исходный HTML",
    "http_headers": "HTTP headers",
    "duplicate_details": "Дубликаты",
    "structured_data_details": "Детали разметки",
    "spelling_grammar_details": "Орфография",
    "overview": "Сводка",
    "issues": "Проблемы",
    "site_structure": "Структура",
    "response_times": "Время ответа",
    "spelling_grammar": "Орфография",
}
COLUMN_LABELS = {
    "Address": "URL",
    "Content Type": "Тип",
    "Status Code": "HTTP",
    "Status": "Статус",
    "Indexability": "Индексация",
    "Indexability Status": "Причина",
    "Title 1": "Title",
    "Findings": "Проблемы",
    "Crawl Depth": "Глубина",
    "Inlinks": "Входящие",
    "Outlinks": "Исходящие",
    "Name": "Поле",
    "Value": "Значение",
    "Category / filter": "Категория",
    "% of Total": "% всего",
    "Issue Name": "Проблема",
    "Issue Type": "Тип",
    "Issue Priority": "Приоритет",
    "From": "Откуда",
    "To": "Куда",
    "Anchor Text": "Анкор",
    "Type": "Тип",
    "State": "Состояние",
    "Reason": "Причина",
    "Observed at": "Наблюдение",
}


def panel_title(spec):
    return PANEL_LABELS.get(spec.id, spec.title)


STATE_FIELDS = {
    "state",
    "display_state",
    "lifecycle",
    "kind",
    "source_kind",
    "goal_state",
    "author_role",
    "phase",
    "core_state",
    "rendering_mode",
    "finish_reason",
    "code",
}


def state_text(value):
    return STATES.get(value, str(value)) if isinstance(value, str) else value_text(value)


def value_text(value):
    if value is None:
        return "Не измерено"
    if isinstance(value, bool):
        return "Да" if value else "Нет"
    if value == "":
        return "Пустое значение"
    if isinstance(value, float):
        return f"{value:.3f}".rstrip("0").rstrip(".")
    if isinstance(value, (tuple, list)):
        return ", ".join(value_text(item) for item in value[:8]) if value else "Нет записей"
    if isinstance(value, Mapping):
        return (
            "; ".join(
                f"{FIELDS.get(key, key)}: {value_text(item)}"
                for key, item in islice(value.items(), 8)
            )
            or "Нет полей"
        )
    return str(value)


def field_text(key, value):
    if key in STATE_FIELDS:
        return state_text(value)
    if (key.endswith("_at") or key == "at") and isinstance(value, str):
        try:
            stamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
            if stamp.tzinfo is None:
                return value + " · часовой пояс не задан"
            return stamp.astimezone().strftime("%d.%m.%Y %H:%M:%S %Z")
        except (ValueError, OverflowError):
            pass
    return value_text(value)


def readable_record(record, *, heading="", max_lines=100):
    """Readable bounded metadata, not another persistence or inference layer."""
    lines = [heading, ""] if heading else []

    def visit(value, depth=0):
        if len(lines) >= max_lines:
            return
        if isinstance(value, Mapping):
            for key, item in value.items():
                if len(lines) >= max_lines:
                    break
                if key == "ok":
                    continue
                name = FIELDS.get(key, key.replace("_", " "))
                if isinstance(item, (Mapping, list, tuple)) and item and depth < 2:
                    lines.append(f"{'  ' * depth}{name}")
                    visit(item, depth + 1)
                    if depth == 0:
                        lines.append("")
                else:
                    lines.append(f"{'  ' * depth}{name}: {field_text(key, item)[:700]}")
        elif isinstance(value, (list, tuple)):
            for item in value[:20]:
                visit(item, depth)
        else:
            lines.append("  " * depth + value_text(value)[:700])

    visit(record)
    if len(lines) >= max_lines:
        lines.append("… Показана ограниченная часть метаданных.")
    return "\n".join(lines)[:24000]


def run_projection(run):
    counters = run.get("counters") or {}
    telemetry = run.get("telemetry") or {}
    collector = run.get("collector") or {}
    rate = telemetry.get("current_rate_per_second")
    units = {
        "pages": "стр./с",
        "sitemap_documents": "sitemap/с",
        "urls_including_resources": "URL/с",
    }
    rate_text = "Не измерено"
    if (
        telemetry.get("state") == "fresh"
        and isinstance(rate, (float, int))
        and not isinstance(rate, bool)
    ):
        rate_text = f"{value_text(rate)} {units.get(telemetry.get('unit'), '/с')}"
    elif telemetry.get("state") == "retained":
        rate_text = "Нет текущей"
    elif telemetry.get("state") == "stale":
        rate_text = "Устарело"
    label = "Запуск"
    if isinstance(run.get("started_at"), str):
        try:
            started = datetime.fromisoformat(run["started_at"].replace("Z", "+00:00"))
            if started.tzinfo is not None:
                label = started.astimezone().strftime("%d.%m %H:%M")
        except (ValueError, OverflowError):
            pass
    identity = run.get("id")
    if isinstance(identity, str) and identity:
        label += " · " + short_run_id(identity)
    return {
        "id": identity,
        "label": label,
        "label_tooltip": f"Начало: {field_text('started_at', run.get('started_at'))}\nID: {value_text(identity)}",
        "kind": run.get("kind"),
        "state": run.get("state"),
        "phase": (run.get("events") or [{}])[-1].get("phase"),
        "fetched": counters.get("fetched"),
        "queued": counters.get("queued"),
        "inflight": counters.get("inflight"),
        "rate": rate_text,
        "rate_limit": collector.get("max_requests_per_second"),
        "sampled_at": telemetry.get("sampled_at"),
        "_run": run,
    }


def short_run_id(value):
    """The one short form of a run / scan id everywhere in the UI: «r-» + the first 4 characters; the full id goes to tooltips."""
    text = str(value or "")
    return "r-" + text[:4] if text else ""


class ElidedLabel(QLabel):
    """Single-line context stays readable without widening the window."""

    def __init__(self, text="", parent=None, elide=Qt.ElideMiddle):
        super().__init__(parent)
        self._full_text = ""
        self._elide_mode = elide
        self.setTextFormat(Qt.PlainText)
        self.setSizePolicy(QSizePolicy.Ignored, QSizePolicy.Preferred)
        self.setMinimumWidth(40)
        self.setText(text)

    def setText(self, text):
        self._full_text = str(text)
        self.setToolTip(self._full_text)
        self.setAccessibleName(self._full_text)
        self._elide()

    def text(self):
        return self._full_text

    def _elide(self):
        super().setText(
            self.fontMetrics().elidedText(
                self._full_text, self._elide_mode, max(40, self.width() - 8)
            )
        )

    def resizeEvent(self, event):
        self._elide()
        super().resizeEvent(event)


class StateBadge(QLabel):
    def __init__(self, text="Не измерено", parent=None):
        super().__init__(parent)
        self.setObjectName("stateBadge")
        self.setSizePolicy(QSizePolicy.Maximum, QSizePolicy.Fixed)
        self.setTextFormat(Qt.PlainText)
        self.set_state(text)

    def set_state(self, state):
        from ..i18n import tr

        self.setText(tr(state_text(state)))
        tone = (
            "warning"
            if state
            in {
                "partial",
                "interrupted",
                "stale",
                "blocked",
                "review",
                "not_initialized",
                "status_unavailable",
            }
            else "error"
            if state in {"failed", "error", "rejected"}
            else "active"
            if state in {"running", "starting", "queued"}
            else "neutral"
        )
        if self.property("tone") != tone:
            self.setProperty("tone", tone)
            self.style().unpolish(self)
            self.style().polish(self)
        self.setAccessibleName(tr("Состояние") + ": " + self.text())


class InlineNotice(QWidget):
    """Readable, dismissible errors stay beside the workspace until addressed."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setObjectName("inlineNotice")
        self.context = None
        layout = QHBoxLayout(self)
        layout.setContentsMargins(12, 8, 8, 8)
        self.message = QLabel()
        self.message.setTextFormat(Qt.PlainText)
        self.message.setWordWrap(True)
        self.message.setTextInteractionFlags(Qt.TextSelectableByMouse)
        layout.addWidget(self.message, 1)
        close = QToolButton()
        close.setText("Закрыть")
        close.setProperty("role", "quiet")
        close.setAccessibleName("Скрыть сообщение об ошибке")
        close.clicked.connect(self.hide)
        layout.addWidget(close)
        self.hide()

    def show_error(self, message, context=None):
        self.context = context
        self.message.setText(str(message)[:1200])
        self.setAccessibleName("Ошибка: " + str(message)[:1200])
        self.show()


class WorkspaceSplitterHandle(QSplitterHandle):
    def __init__(self, orientation, splitter):
        super().__init__(orientation, splitter)
        self.setFocusPolicy(Qt.StrongFocus)
        self.setAccessibleName("Изменить размер панелей")
        self.setToolTip(
            "Перетащите границу или используйте стрелки. Двойной щелчок — восстановить размеры."
        )

    def paintEvent(self, event):
        super().paintEvent(event)
        painter = QPainter(self)
        painter.setRenderHint(QPainter.Antialiasing)
        colors = theme_tokens()["colors"]
        painter.setPen(Qt.NoPen)
        painter.setBrush(
            QColor(colors["primary"] if self.hasFocus() or self.underMouse() else colors["outline"])
        )
        center = self.rect().center()
        for offset in (-5, 0, 5):
            point = (
                QPointF(center.x(), center.y() + offset)
                if self.orientation() == Qt.Horizontal
                else QPointF(center.x() + offset, center.y())
            )
            painter.drawEllipse(point, 1.25, 1.25)
        painter.end()

    def keyPressEvent(self, event):
        keys = (
            (Qt.Key_Left, Qt.Key_Right)
            if self.orientation() == Qt.Horizontal
            else (Qt.Key_Up, Qt.Key_Down)
        )
        if event.key() in keys:
            splitter = self.splitter()
            index = next(i for i in range(1, splitter.count()) if splitter.handle(i) is self)
            position = sum(splitter.sizes()[:index]) + splitter.handleWidth() * (index - 1)
            step = 40 if event.modifiers() & Qt.ShiftModifier else 16
            splitter.moveSplitter(position + (-step if event.key() == keys[0] else step), index)
            event.accept()
        else:
            super().keyPressEvent(event)

    def mouseDoubleClickEvent(self, event):
        if event.button() == Qt.LeftButton:
            self.splitter().restore_sizes()
            event.accept()
        else:
            super().mouseDoubleClickEvent(event)


class WorkspaceSplitter(QSplitter):
    def __init__(self, orientation, parent=None):
        super().__init__(orientation, parent)
        self._default_sizes = None
        self.setHandleWidth(theme_tokens()["splitter_width"])

    def createHandle(self):
        return WorkspaceSplitterHandle(self.orientation(), self)

    def setSizes(self, sizes):
        if self._default_sizes is None and len(sizes) == self.count():
            self._default_sizes = list(sizes)
        super().setSizes(sizes)

    def restore_sizes(self):
        if self._default_sizes:
            super().setSizes(self._default_sizes)


def content_spacing(width):
    """Shared spacious page rhythm; table row density remains independent."""
    layout = theme_tokens()["layout"]
    wide = width >= 900
    return (
        layout["content_margin" if wide else "content_margin_narrow"],
        layout["section_spacing" if wide else "section_spacing_narrow"],
    )


class ProjectEmptyState(QWidget):
    """A single action for an unbound workspace; no backend or project discovery."""

    openRequested = pyqtSignal()

    def __init__(self, parent=None):
        super().__init__(parent)
        from .icons import material_icon

        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 24, 0, 0)
        layout.setSpacing(16)
        title = QLabel("Откройте проект")
        title.setObjectName("sectionTitle")
        title.setSizePolicy(QSizePolicy.Preferred, QSizePolicy.Fixed)
        caption = QLabel("Сохранённые сканы, задачи и запуски появятся здесь.")
        caption.setObjectName("sectionCaption")
        caption.setWordWrap(True)
        caption.setSizePolicy(QSizePolicy.Preferred, QSizePolicy.Fixed)
        button = self.open_button = QPushButton("Открыть проект…")
        button.setProperty("role", "primary")
        button.setIcon(material_icon("folder_open", theme_tokens()["colors"]["on_primary"]))
        button.setAccessibleName("Открыть существующий локальный проект")
        button.clicked.connect(self.openRequested)
        layout.addWidget(title)
        layout.addWidget(caption)
        layout.addWidget(button, 0, Qt.AlignLeft)
        layout.addStretch(1)
