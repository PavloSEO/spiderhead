"""Bounded native guidance and supplied issue context; navigation intents only."""

from collections.abc import Mapping

from PyQt5.QtCore import Qt, pyqtSignal
from PyQt5.QtWidgets import (
    QDialog,
    QDialogButtonBox,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QScrollArea,
    QTabWidget,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .icons import MaterialIconLabel, material_icon
from .presentation import StateBadge

START = (
    ("folder_open", "1. Откройте проект", "Выберите папку проекта через кнопку с папкой. Вверху всегда проверяйте проект и сохранённый скан: все таблицы относятся к этому контексту.", "work"),
    ("tune", "2. Проверьте настройки скана", "«Новый скан»: источник URL, исходный HTML или JavaScript, запросы в секунду и потоки. Лимит URL включается отдельно; HTTP- и временной бюджеты — в расширенных настройках. Полный режим требует поддержки ядра. Сбор начинается только по кнопке «Запустить».", "scans"),
    ("manage_search", "3. Найдите и изучите проблему", "Выберите сохранённый скан → URL или Аудит → строку таблицы. Смотрите сообщение, затронутый URL и доказательство. Поиск в таблице фильтрует текущую страницу; «Поиск в содержимом» проверяет сохранённый корпус.", "issues"),
    ("compare_arrows", "4. Проверьте исправление", "Сохраните скан «до» → исправьте сайт → явно запустите новый скан с сопоставимым объёмом → выберите пару в «Сравнение». Исчезновение строки само по себе не подтверждает исправление.", "compare"),
)

SECTIONS = (
    ("dashboard", "Работа и журнал", "«Работа» показывает план и отдельные запуски. Текущая скорость относится к запуску, а выполнение задач — к согласованному плану. В журнале можно проверить последовательность событий.", "work"),
    ("table_chart", "URL и Аудит", "URL — строки выбранного сохранённого скана. Аудит — тематические таблицы и доказательства. Выберите строку для подробностей; пустая страница и недоступные данные означают разное.", "url"),
    ("search", "Поиск в содержимом", "Укажите буквальную строку и область: код <head>, весь HTML, текст <body> или CSS-элемент. Проверьте исходный HTML / сохранённый DOM и регистр. «Не содержит» применим только к доступному содержимому; найденный маркер не доказывает работу тега.", "content_search"),
    ("history", "Сканы и управление", "Выбор сохранённого скана только читает его данные. «Остановить» относится к выбранному запуску, которым владеет это окно. Продолжение доступно лишь для подходящего прерванного скана; внешний запуск показан как наблюдение.", "scans"),
    ("edit_note", "Задачи и входящие", "Согласованный план задаёт знаменатель выполнения задач. Во входящих можно явно сохранить заметку или предложенную цель. Черновики привязаны к проекту; сохранение ждёт свежего состояния и не запускает агента.", "inbox"),
    ("view_agenda", "Вкладки и вид", "Рабочая вкладка хранит проект, скан и представление. Закрытие вкладки не останавливает скан. В меню «Вид» можно восстановить панели и изменить плотность; F6 переключает рабочие области.", None),
    ("settings", "Подключение агента", "Меню «Агент» → «Подключить агента…» показывает путь для локального CLI/MCP. Передавайте путь файла подключения, а не его содержимое. После перезапуска приложения подключение нужно обновить; запуск и остановка требуют явного разрешения.", None),
)

EXPLANATIONS = (
    ("finished", "Запуск завершён", "Процесс закончил работу. Отдельно проверьте полноту сохранённого результата и причину завершения."),
    ("complete", "Полные данные", "Полнота подтверждена ядром для указанного объёма и источника. Это не означает, что SEO-ошибок нет."),
    ("partial", "Частичный результат", "Сохранена часть данных. Причина может быть в лимите, прерывании или недоступном источнике — смотрите фактическое сообщение."),
    ("unknown", "Не измерено / неизвестно", "Источник не сообщил значение. Это не ноль и не положительный результат. Без подтверждённого знаменателя план не выражается процентом."),
    ("not_verifiable", "Нельзя подтвердить", "Доказательств недостаточно. Отсутствие находки не считается исправлением, а недоступный HTML — отсутствием строки."),
    ("unavailable", "Недоступно / пропущено", "Источник или проверка не выполнены либо не подключены. Это не успешная проверка; сначала прочитайте причину и доступный следующий шаг."),
)

ISSUE_FIELDS = (("message", "Сообщение"), ("reason", "Причина из источника"), ("recommendation", "Рекомендация из источника"))
SOURCE_FIELDS = (("code", "Код"), ("state", "Исходное состояние"), ("url", "URL"), ("source", "Источник"))
KNOWN_VIEWS = frozenset(item[3] for item in START + SECTIONS if item[3])
VIEW_TITLES = {"work": "Работа", "scans": "Сканы", "issues": "Проблемы", "compare": "Сравнение", "url": "URL", "content_search": "Содержимое", "inbox": "Входящие"}


def _text(value, limit=900):
    if not isinstance(value, str):
        return ""
    value = value.strip()
    return value if len(value) <= limit else value[:limit] + "… [текст сокращён]"


def _label(text="", name=""):
    label = QLabel(text)
    label.setTextFormat(Qt.PlainText)
    label.setWordWrap(True)
    label.setMinimumWidth(0)
    label.setTextInteractionFlags(Qt.TextSelectableByMouse | Qt.TextSelectableByKeyboard)
    if name:
        label.setObjectName(name)
    return label


class HelpGuideDialog(QDialog):
    """A local guide; the host handles menu/F1 and ``navigateRequested``.

    ``available_views`` restricts navigation, never advertises executable tools.
    ``issue`` accepts already-redacted string fields only: title, message,
    reason, recommendation, state, code, url and source. Unknown fields and
    nested objects are ignored. No issue is interpreted as a successful check.
    """

    navigateRequested = pyqtSignal(str)

    def __init__(self, parent=None, *, available_views=None, issue=None):
        super().__init__(parent)
        self.available_views = KNOWN_VIEWS if available_views is None else KNOWN_VIEWS.intersection(available_views)
        self.setObjectName("helpGuide")
        self.setWindowTitle("Как пользоваться SEOHEAD")
        self.setAccessibleName("Краткий гайд SEOHEAD")
        self.resize(720, 700)
        self.setMinimumSize(440, 420)
        outer = QVBoxLayout(self)
        outer.setContentsMargins(16, 16, 16, 12)
        heading = QHBoxLayout()
        heading.addWidget(MaterialIconLabel("help", 24, self))
        heading.addWidget(_label("От проекта к проверенному исправлению", "sectionTitle"), 1)
        outer.addLayout(heading)
        outer.addWidget(_label("Короткий маршрут, назначение разделов и смысл состояний.", "metadata"))
        self.tabs = QTabWidget()
        self.tabs.setAccessibleName("Темы справки")
        outer.addWidget(self.tabs, 1)
        self.navigation_buttons = {}
        for title, entries in (("Быстрый старт", START), ("Разделы и настройки", SECTIONS)):
            layout = self._page(title)
            for icon, heading, text, view in entries:
                layout.addWidget(self._card(icon, heading, text, view))
            layout.addStretch()
        states = self._page("Данные и ошибки")
        self.issue_box = QGroupBox("Выбранное сообщение")
        issue_layout = QVBoxLayout(self.issue_box)
        self.issue_title = _label(name="sectionTitle")
        issue_layout.addWidget(self.issue_title)
        self.issue_labels = {}
        for key, title in ISSUE_FIELDS:
            label = _label()
            self.issue_labels[key] = label
            issue_layout.addWidget(label)
        self.source_toggle = QToolButton()
        self.source_toggle.setText("Источник сообщения")
        self.source_toggle.setIcon(material_icon("chevron_right"))
        self.source_toggle.setCheckable(True)
        self.source_toggle.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
        self.source_toggle.setAccessibleName("Показать исходный код, состояние и источник сообщения")
        issue_layout.addWidget(self.source_toggle, 0, Qt.AlignLeft)
        self.issue_source = _label(name="metadata")
        issue_layout.addWidget(self.issue_source)
        self.source_toggle.toggled.connect(self._toggle_source)
        states.addWidget(self.issue_box)
        for state, title, text in EXPLANATIONS:
            box = self._card("info", title, text)
            badge = StateBadge(state)
            box.layout().insertWidget(1, badge, 0, Qt.AlignLeft)
            states.addWidget(box)
        states.addWidget(self._card("rule", "Как читать находку", "Начните с названия проверки, сообщения и рекомендации, затем откройте URL и доказательство. Сравните источник, дату, объём и пропущенные проверки. Если причина или рекомендация не переданы, гайд не подставляет их по коду ошибки."))
        states.addWidget(self._card("help", "Если действие недоступно", "Прочитайте причину рядом с кнопкой: может требоваться проект, сохранённый скан или поддержка подключённого ядра. Недоступный раздел не означает, что ошибок нет. Если причина не указана, сохраните точный текст сообщения и контекст проекта для разбора."))
        states.addStretch()
        buttons = QDialogButtonBox(QDialogButtonBox.Close)
        buttons.button(QDialogButtonBox.Close).setText("Закрыть")
        buttons.rejected.connect(self.reject)
        outer.addWidget(buttons)
        self.set_issue(issue)

    def _page(self, title):
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QScrollArea.NoFrame)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        scroll.setAccessibleName(title)
        content = QWidget()
        layout = QVBoxLayout(content)
        layout.setContentsMargins(8, 8, 12, 8)
        layout.setSpacing(10)
        scroll.setWidget(content)
        self.tabs.addTab(scroll, title)
        return layout

    def _card(self, icon, title, text, view=None):
        box = QGroupBox()
        layout = QVBoxLayout(box)
        header = QHBoxLayout()
        header.addWidget(MaterialIconLabel(icon, 20, box))
        label = _label(title)
        font = label.font()
        font.setBold(True)
        label.setFont(font)
        header.addWidget(label, 1)
        if view:
            button = QPushButton(VIEW_TITLES[view])
            button.setIcon(material_icon("open_in_new"))
            button.setProperty("role", "quiet")
            button.setAccessibleName("Открыть раздел: " + VIEW_TITLES[view])
            button.setEnabled(view in self.available_views)
            button.setToolTip("Открыть раздел, без запуска операций" if button.isEnabled() else "Этот раздел недоступен в текущем контексте приложения")
            button.clicked.connect(lambda checked=False, view=view: self._navigate(view))
            header.addWidget(button)
            self.navigation_buttons.setdefault(view, []).append(button)
        layout.addLayout(header)
        layout.addWidget(_label(text))
        return box

    def _navigate(self, view):
        if view in self.available_views:
            self.navigateRequested.emit(view)
            self.accept()

    def _toggle_source(self, shown):
        self.issue_source.setVisible(shown)
        self.source_toggle.setIcon(material_icon("chevron_down" if shown else "chevron_right"))

    def set_issue(self, issue=None):
        """Replace the supplied context, clearing every previous field."""
        issue = issue if isinstance(issue, Mapping) else {}
        content = {key: _text(issue.get(key)) for key, _ in ISSUE_FIELDS}
        self.issue_title.setText(_text(issue.get("title"), 140) or ("Сообщение текущего раздела" if any(content.values()) else "Конкретное сообщение не выбрано"))
        for key, title in ISSUE_FIELDS:
            value = content[key]
            self.issue_labels[key].setText(f"{title}: {value}" if value else "")
            self.issue_labels[key].setVisible(bool(value))
        source = [f"{title}: {value}" for key, title in SOURCE_FIELDS if (value := _text(issue.get(key), 300))]
        self.issue_source.setText("\n".join(source))
        self.source_toggle.setChecked(False)
        self.source_toggle.setVisible(bool(source))
        self.issue_source.hide()
        if any(content.values()) or _text(issue.get("title"), 140):
            self.tabs.setCurrentIndex(2)
