"""«Быстрый запуск»: the one-line scan launcher of the QuickScan sheet (URL, what to scan, quick limits, «Запустить»).

It starts a real scan only on the user's own click (or Enter in the URL field), through the same draft, plan and
``host.launch_scan`` as the «Новый скан» dialog. The launcher is bound to the OPEN project: its start URL is the
project's site. A crawl of an address outside the project needs the core's project-less crawler mode, which the core
does not have yet: that variant is shown as unavailable and never starts anything.
"""

from __future__ import annotations

from urllib.parse import urlsplit

from PyQt5.QtCore import QPoint, Qt, pyqtSignal
from PyQt5.QtWidgets import (
    QFrame,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QMenu,
    QProgressBar,
    QPushButton,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import theming
from ..i18n import tr, trf
from ..ui.controls import Switch
from ..ui.icons import MaterialIconLabel
from ..ui.icons import material_icon as icon
from ..ui.kit import waiting_badge
from ..ui.presentation import ElidedLabel, run_projection, short_run_id, state_text
from .new_scan_draft import ISSUE_OTHER, PlanError, ScanDraft, grouped, request_project_policy
from .new_scan_pages import HelpIcon, Stepper, number_edit
from .scan_common import RunRow, StatusBadge, number, now

ACTIVE_STATES = {"queued", "starting", "running", "stop_requested", "awaiting_core_status"}
# (key, icon, name, shortcut text); «list» is shown but cannot be launched from the application
MODES = (
    ("site", "travel_explore", "Сайт целиком", "⌘1"),
    ("sitemap", "account_tree", "Sitemap", "⌘2"),
    ("list", "format_list_bulleted", "Список URL", "⌘3"),
)
LIST_REASON = "Запуск списка URL из приложения недоступен в этой версии ядра"
OUTSIDE_REASON = "Краул адреса вне проекта: режим «Краулер без проекта» в ядре ещё не появился"


def _site_key(url):
    """Scheme-less, case-insensitive host plus path of an address, to compare what was typed with the project's site."""
    text = (url or "").strip()
    if not text:
        return ""
    parts = urlsplit(text if "://" in text else "https://" + text.lstrip("/"))
    return f"{(parts.hostname or '').lower()}{':' + str(parts.port) if parts.port else ''}{parts.path.rstrip('/')}"


def active_runs(host):
    rows = host.owned_runs_for_project() if hasattr(host, "owned_runs_for_project") else []
    return [row for row in rows if row.get("state") in ACTIVE_STATES]


class QuickScanBar(QFrame):
    """The 48 px command strip. ``started`` carries the id of the run the host accepted."""

    started = pyqtSignal(object)

    def __init__(self, host, parent=None):
        super().__init__(parent)
        self.host = host
        self.draft = None
        self.mode = "site"
        self._busy = False
        self.setObjectName("quickScanBar")
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(0)
        outer.addWidget(self._strip())
        self.message_row = self._message()
        outer.addWidget(self.message_row)
        self.live_row = self._live()
        outer.addWidget(self.live_row)
        host.crawl_descriptor_changed.connect(self._descriptor_changed)
        if host.crawl_descriptor is None and hasattr(host, "load_crawl_descriptor") and host.project_directory:
            host.load_crawl_descriptor()
        self._descriptor_changed()

    def release(self):
        """Stop following the host (the bar is closed or replaced)."""
        try:
            self.host.crawl_descriptor_changed.disconnect(self._descriptor_changed)
        except (TypeError, RuntimeError):
            pass

    # ---- frame ---------------------------------------------------------------------------------------------------
    def _strip(self):
        strip = QWidget()
        strip.setFixedHeight(48)
        layout = QHBoxLayout(strip)
        layout.setContentsMargins(12, 0, 12, 0)
        layout.setSpacing(8)
        cmd = QFrame()
        cmd.setProperty("cmd", True)
        cmd.setObjectName("quickScanCommand")
        cmd.setFixedHeight(32)
        cmd.setMinimumWidth(260)
        line = QHBoxLayout(cmd)
        line.setContentsMargins(10, 0, 0, 0)
        line.setSpacing(6)
        line.addWidget(MaterialIconLabel("language", 18, color="role:text_2"))
        self.url = QLineEdit()
        self.url.setObjectName("quickScanUrl")
        self.url.setAccessibleName(tr("Стартовый URL"))
        self.url.setPlaceholderText(tr("Адрес сайта проекта"))
        self.url.textChanged.connect(self._url_changed)
        self.url.returnPressed.connect(self.start)
        line.addWidget(self.url, 1)
        clear = QToolButton()
        clear.setProperty("role", "icon")
        clear.setIcon(icon("close"))
        clear.setIconSize(clear.iconSize() * 0.8)
        clear.setFixedSize(24, 24)
        clear.setAccessibleName(tr("Очистить URL"))
        clear.setToolTip(tr("Очистить"))
        clear.setFocusPolicy(Qt.NoFocus)
        clear.clicked.connect(self.url.clear)
        line.addWidget(clear)
        self.mode_button = QToolButton()
        self.mode_button.setObjectName("quickScanMode")
        self.mode_button.setProperty("cmd_mode", True)
        self.mode_button.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
        self.mode_button.setPopupMode(QToolButton.InstantPopup)
        self.mode_button.setToolTip(tr("Что сканировать"))
        self.mode_button.setAccessibleName(tr("Что сканировать"))
        self.mode_menu = QMenu(self.mode_button)
        self.mode_actions = {}
        for key, glyph, name, shortcut in MODES:
            action = self.mode_menu.addAction(icon(glyph), tr(name))
            action.setCheckable(True)
            action.setData(key)
            action.triggered.connect(lambda _c=False, k=key: self.set_mode(k))
            self.mode_actions[key] = action
        self.mode_actions["list"].setEnabled(False)
        self.mode_actions["list"].setToolTip(tr(LIST_REASON))
        self.mode_button.setMenu(self.mode_menu)
        line.addWidget(self.mode_button)
        layout.addWidget(cmd, 1)
        self.chip = QToolButton()
        self.chip.setObjectName("quickScanParams")
        self.chip.setProperty("quick_chip", True)
        self.chip.setIcon(icon("speed"))
        self.chip.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
        self.chip.setToolTip(tr("Скорость, лимит, хранение HTML"))
        self.chip.setAccessibleName(tr("Быстрые параметры"))
        self.chip.clicked.connect(self._toggle_params)
        layout.addWidget(self.chip)
        self.start_button = QPushButton(tr("Запустить"))
        self.start_button.setObjectName("quickScanStart")
        self.start_button.setProperty("role", "primary")
        self.start_button.setIcon(icon("play_arrow", theming.roles()["on_primary"]))
        self.start_button.clicked.connect(self.start)
        layout.addWidget(self.start_button)
        self.pause_button = QPushButton(tr("Пауза"))
        self.pause_button.setObjectName("quickScanPause")
        self.pause_button.setEnabled(False)
        self.pause_button.setToolTip(f"{tr('Недоступно в этой версии ядра')}: {tr('ядро не ставит скан на паузу')}")
        self.stop_button = QPushButton(tr("Стоп"))
        self.stop_button.setObjectName("quickScanStop")
        self.stop_button.setProperty("role", "danger")
        self.stop_button.setIcon(icon("stop", theming.roles()["error"]))
        self.stop_button.clicked.connect(self.stop)
        layout.addWidget(self.pause_button)
        layout.addWidget(self.stop_button)
        settings = QToolButton()
        settings.setObjectName("quickScanAllSettings")
        settings.setProperty("role", "icon")
        settings.setIcon(icon("settings"))
        settings.setAccessibleName(tr("Все параметры скана"))
        settings.setToolTip(tr("Все параметры скана"))
        settings.clicked.connect(self._open_dialog)
        layout.addWidget(settings)
        return strip

    def _message(self):
        row = QFrame()
        row.setFixedHeight(28)
        layout = QHBoxLayout(row)
        layout.setContentsMargins(12, 0, 12, 0)
        layout.setSpacing(8)
        self.message_icon = MaterialIconLabel("info", 16, color="role:text_2")
        self.message = ElidedLabel()
        self.message.setObjectName("quickScanMessage")
        self.message.setProperty("text_style", "meta")
        layout.addWidget(self.message_icon)
        layout.addWidget(self.message, 1)
        self.message_badge = waiting_badge(ISSUE_OTHER, OUTSIDE_REASON)
        layout.addWidget(self.message_badge)
        row.hide()
        return row

    def _live(self):
        row = QFrame()
        row.setFixedHeight(32)
        layout = QHBoxLayout(row)
        layout.setContentsMargins(12, 0, 12, 0)
        layout.setSpacing(12)
        self.live_state = StatusBadge()
        self.live_state.setObjectName("quickScanLiveState")
        self.live_id = QLabel()
        self.live_id.setProperty("text_style", "mono")
        self.live_bar = QProgressBar()
        self.live_bar.setObjectName("quickScanRunBar")
        self.live_bar.setTextVisible(False)
        self.live_bar.setFixedSize(160, 4)
        self.live_counts = QLabel()
        self.live_counts.setObjectName("quickScanRunCounts")
        self.live_counts.setProperty("text_style", "meta")
        self.live_rate = QLabel()
        self.live_rate.setObjectName("quickScanRunRate")
        self.live_rate.setProperty("text_style", "meta")
        link = QPushButton(tr("Живой прогресс"))
        link.setObjectName("quickScanProgress")
        link.setProperty("role", "text")
        link.setProperty("size", "sm")
        link.clicked.connect(self._show_scans)
        layout.addWidget(self.live_state)
        layout.addWidget(self.live_id)
        layout.addWidget(self.live_bar)
        layout.addWidget(self.live_counts)
        layout.addWidget(self.live_rate)
        layout.addStretch(1)
        layout.addWidget(link)
        row.hide()
        return row

    # ---- state ---------------------------------------------------------------------------------------------------
    def _site(self):
        return ((getattr(self.host, "project_result", None) or {}).get("project") or {}).get("site") or {}

    def _descriptor_changed(self):
        host = self.host
        if host.crawl_descriptor is not None and self.draft is None and host.project_directory:
            site = self._site()
            self.draft = draft = ScanDraft(host.crawl_descriptor, target=site.get("target") or "", host=site.get("host") or "",
                                           project_directory=host.project_directory or "", prefs=getattr(host, "prefs", None), parent=self)
            key = host.note_project_key() if hasattr(host, "note_project_key") else None
            draft.restore(getattr(host, "_scan_drafts", {}).get(key))
            self.mode = draft.source if draft.source in ("site", "sitemap") else "site"
            self.url.blockSignals(True)
            self.url.setText(draft.sitemap_url if self.mode == "sitemap" else draft.target)
            self.url.blockSignals(False)
            self._rps_edit, self._sync_rps = number_edit(draft, "rps", tr("Запросов в секунду"), 0, "quickScanRate")
            self._limit_edit, self._sync_limit = number_edit(draft, "limit", tr("Лимит URL"), 0, "quickScanLimit")
            draft.changed.connect(self.refresh)
            request_project_policy(self.host, self.draft)
        self.refresh()

    def set_mode(self, mode):
        if mode not in ("site", "sitemap") or self.draft is None:
            return
        self.mode = mode
        self.draft.source = mode
        self.url.blockSignals(True)
        if mode == "sitemap":
            self.url.setText(self.draft.sitemap_url)
            self.url.setPlaceholderText(tr("Адрес sitemap, например") + f" {self._site().get('target', '')}sitemap.xml")
        else:
            self.url.setText(self.draft.target)
            self.url.setPlaceholderText(tr("Адрес сайта проекта"))
        self.url.blockSignals(False)
        self.draft.emit_changed()

    def _url_changed(self, text):
        if self.draft is None:
            return
        if self.mode == "sitemap":
            self.draft.sitemap_url = text
        self.draft.emit_changed()

    def _outside(self):
        """True when the typed address is not the open project's: that crawl needs the project-less crawler mode."""
        text = self.url.text().strip()
        if not text:
            return False
        if self.mode == "sitemap":
            return _site_key(text).split("/")[0] != _site_key(self.draft.target).split("/")[0] if self.draft else True
        return _site_key(text) != _site_key(self.draft.target) if self.draft else True

    def verdict(self):
        """(can_start, tooltip) for the launcher right now; the reason is always one that the user can act on."""
        host, draft = self.host, self.draft
        if not host.project_directory or getattr(host, "_project_loading", False):
            return False, tr("Сначала откройте проект и дождитесь его данных")
        if draft is None:
            return False, tr("Параметры ядра загружаются…")
        if not self.url.text().strip():
            return False, tr("Введите адрес")
        if self._outside():
            return False, tr(OUTSIDE_REASON)
        if not draft.limit_enabled:
            return False, tr("Без лимита URL запуск одной строкой недоступен: откройте «Все параметры скана»")
        problems = draft.problems()
        if problems:
            return False, next(iter(problems.values()))
        return True, trf("Запустить скан: GET-запросы к сайту проекта, до {n} URL", n=grouped(draft.url_limit))

    def refresh(self):
        if self._busy:
            return
        self._busy = True
        try:
            self._sync()
        finally:
            self._busy = False

    def _sync_live(self, run):
        """Status, id, and counters of the run in progress; a number the core did not measure is shown as such, never as 0."""
        row = RunRow(run=run)
        self.live_state.set_state(row.badge_kind, row.state_label(now()), row.badge_icon)
        self.live_id.setText(short_run_id(row.id))
        self.live_id.setToolTip(row.id or "")
        if row.found:
            self.live_bar.setRange(0, row.found)
            self.live_bar.setValue(row.fetched or 0)
            share = round(100 * (row.fetched or 0) / row.found)
            self.live_counts.setText(trf("{done} / {found} · {share} %", done=number(row.fetched), found=number(row.found), share=share))
        else:
            self.live_bar.setRange(0, 1)
            self.live_bar.setValue(0)
            self.live_counts.setText(tr("Число обработанных и найденных URL не измерено"))
        self.live_rate.setText(run_projection(run)["rate"])

    def _sync(self):
        draft = self.draft
        runs = active_runs(self.host)
        busy = bool(runs)
        self.start_button.setVisible(not busy)
        self.pause_button.setVisible(busy)
        self.stop_button.setVisible(busy)
        self.live_row.setVisible(busy)
        if busy:
            self._sync_live(runs[-1])
        for key, action in self.mode_actions.items():
            action.setChecked(key == self.mode)
        name = next(tr(n) for k, _g, n, _s in MODES if k == self.mode)
        glyph = next(g for k, g, _n, _s in MODES if k == self.mode)
        self.mode_button.setText(name)
        self.mode_button.setIcon(icon(glyph, theming.roles()["primary"]))
        sitemap_declared = bool(draft and draft.capabilities.get("sitemap_only_retained") is True)
        self.mode_actions["sitemap"].setEnabled(sitemap_declared)
        self.mode_actions["sitemap"].setToolTip("" if sitemap_declared else tr("Возможность не объявлена ядром"))
        enabled = draft is not None
        self.url.setEnabled(enabled)
        self.chip.setEnabled(enabled)
        if draft is not None:
            for sync in (self._sync_rps, self._sync_limit):
                sync()
            rate = f"{draft.rps():g}".replace(".", ",") if draft.rps() else "—"
            limit = grouped(draft.url_limit) if draft.limit_enabled else tr("без лимита")
            html = tr("HTML") if draft.value("storage.body_mode") != "off" else tr("без HTML")
            self.chip.setText(f"{trf('{n} запр/с', n=rate)}  ·  {tr('лимит')} {limit}  ·  {html}")
            self._refresh_popover()
        outside = bool(draft) and self._outside()
        self.url.parentWidget().setProperty("invalid", outside)
        self.url.parentWidget().style().unpolish(self.url.parentWidget())
        self.url.parentWidget().style().polish(self.url.parentWidget())
        ok, reason = self.verdict()
        self.start_button.setEnabled(ok)
        self.start_button.setToolTip(reason)
        text, kind = "", ""
        if outside:
            text, kind = tr(OUTSIDE_REASON), "badge"
        elif draft is not None and not draft.limit_enabled and self.url.text().strip():
            text = tr("Без лимита URL запуск одной строкой недоступен: откройте «Все параметры скана»")
        elif draft is not None and draft.problems() and self.url.text().strip():
            text = next(iter(draft.problems().values()))
        elif self.host.crawl_descriptor is None and getattr(self.host, "_crawl_descriptor_error", None):
            text = tr("Настройки ядра недоступны")
        self.message.setText(text)
        self.message_badge.setVisible(kind == "badge")
        self.message_icon.set_material_icon("error" if text and kind != "badge" else "info", theming.roles()["text_2"])
        self.message_row.setVisible(bool(text))

    # ---- actions -------------------------------------------------------------------------------------------------
    def start(self):
        """The user's own click: build the same plan as the dialog and hand it to the host; nothing else starts a scan."""
        ok, reason = self.verdict()
        if not ok:
            self.message.setText(reason)
            self.message_row.show()
            return None
        draft = self.draft
        draft.source = self.mode
        try:
            plan = draft.build_plan()
        except PlanError as exc:
            self.message.setText(str(exc))
            self.message_row.show()
            return None
        self._save_draft()
        run_id = self.host.launch_scan(*plan)
        if run_id:
            self.started.emit(run_id)
        self.refresh()
        return run_id

    def stop(self):
        runs = active_runs(self.host)
        if runs and self.host.scan_manager is not None:
            self.host.scan_manager.stop(runs[-1]["id"])

    def _save_draft(self):
        key = self.host.note_project_key() if hasattr(self.host, "note_project_key") else None
        if self.draft is not None and key is not None and hasattr(self.host, "_scan_drafts"):
            self.host._scan_drafts[key] = self.draft.snapshot()

    def _show_scans(self):
        if hasattr(self.host, "navigation"):
            self.host.navigation.select_section("scans")
        self.window().close() if isinstance(self.window(), QuickScanPopup) else None

    def _open_dialog(self):
        """«Все параметры скана»: the full dialog starts from the same draft."""
        if self.draft is not None:
            self.draft.source = self.mode
            self._save_draft()
        popup = self.window()
        if isinstance(popup, QuickScanPopup):
            popup.close()
        self.host.scan_preview()

    # ---- quick parameters ----------------------------------------------------------------------------------------
    def _toggle_params(self):
        if self.draft is None:
            return
        if getattr(self, "popover", None) is None:
            self.popover = ParamsPopover(self)
        if self.popover.isVisible():
            self.popover.hide()
            return
        self.popover.place_under(self.chip)
        self.popover.show()

    def _refresh_popover(self):
        popover = getattr(self, "popover", None)
        if popover is not None:
            popover.sync()


class ParamsPopover(QFrame):
    """The small «Быстрые параметры» sheet: requests per second, URL limit, keeping HTML; the rest is in the dialog."""

    def __init__(self, bar):
        super().__init__(bar.window(), Qt.Popup)
        self.bar = bar
        draft = bar.draft
        self.setObjectName("quickScanPopover")
        self.setProperty("card", "panel")
        self.setFixedWidth(280)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(12, 10, 12, 10)
        layout.setSpacing(8)
        self.rps_edit, self._sync_rps = number_edit(draft, "rps", tr("Запросов в секунду"), 0, "quickScanPopRate")
        self.limit_edit, self._sync_limit = number_edit(draft, "limit", tr("Лимит URL"), 0, "quickScanPopLimit")
        self.rps = Stepper(self.rps_edit, lambda: self._step_rps(-1), lambda: self._step_rps(1))
        self.limit = Stepper(self.limit_edit, lambda: self._step_limit(-500), lambda: self._step_limit(500))
        self.rps.setFixedWidth(130)
        self.limit.setFixedWidth(130)
        layout.addLayout(self._line(tr("Запросов/с"), self.rps, tr("Бережно для боевого сайта: не больше 2")))
        layout.addLayout(self._line(tr("Лимит URL"), self.limit, tr("Скан остановится, когда найдёт столько URL")))
        self.html = Switch(tr("Сохранять HTML"), True)
        self.html.setObjectName("quickScanPopHtml")
        self.html.toggled.connect(lambda state: draft.set_value("storage.body_mode", "captured_entity_bytes" if state else "off"))
        self.html.setEnabled(draft.has("storage.body_mode"))
        layout.addLayout(self._line(tr("Сохранять HTML"), self.html, tr("Нужно для поиска в HTML и сравнений")))
        self.error = QLabel()
        self.error.setObjectName("quickScanPopError")
        self.error.setProperty("field_error", True)
        self.error.setProperty("text_style", "meta")
        self.error.setWordWrap(True)
        layout.addWidget(self.error)
        more = QPushButton(tr("Все параметры…"))
        more.setObjectName("quickScanPopMore")
        more.setProperty("role", "text")
        more.setProperty("size", "sm")
        more.setIcon(icon("settings"))
        more.clicked.connect(bar._open_dialog)
        layout.addWidget(more, 0, Qt.AlignLeft)
        self.sync()

    @staticmethod
    def _line(title, control, help_text):
        row = QHBoxLayout()
        row.setSpacing(6)
        label = QLabel(title)
        row.addWidget(label)
        row.addWidget(HelpIcon(help_text))
        row.addStretch(1)
        row.addWidget(control)
        return row

    def _step_rps(self, step):
        rate = self.bar.draft.rps() or 1
        self.bar.draft.set_text("rps", f"{max(1, round(rate) + step)}")

    def _step_limit(self, step):
        self.bar.draft.set_text("limit", f"{max(500, self.bar.draft.url_limit + step)}")

    def sync(self):
        draft = self.bar.draft
        for sync in (self._sync_rps, self._sync_limit):
            sync()
        problems = draft.problems()
        self.rps.set_invalid("rps" in problems)
        self.limit.set_invalid("limit" in problems)
        self.limit.set_enabled_value(draft.limit_enabled)
        saving = draft.value("storage.body_mode") != "off"
        if self.html.isChecked() != saving:
            self.html.blockSignals(True)
            self.html.setChecked(saving)
            self.html.blockSignals(False)
        self.error.setText(problems.get("rps") or problems.get("limit") or "")
        self.error.setVisible(bool(self.error.text()))

    def place_under(self, anchor):
        self.move(anchor.mapToGlobal(QPoint(0, anchor.height() + 4)))


class QuickScanPopup(QFrame):
    """The launcher as a bar under the top of the main window; closes after a start, on Esc or a click outside."""

    def __init__(self, host):
        super().__init__(host, Qt.Popup)
        self.setObjectName("quickScanPopup")
        self.setProperty("card", "panel")
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        self.bar = QuickScanBar(host, self)
        self.bar.started.connect(lambda _id: self.close())
        layout.addWidget(self.bar)
        self.host = host

    def closeEvent(self, event):
        self.bar.release()
        super().closeEvent(event)

    def place(self):
        host = self.host
        width = max(560, min(host.width() - 40, 1100))
        self.setFixedWidth(width)
        anchor = getattr(host, "project_button", None)
        top = anchor.mapTo(host, QPoint(0, anchor.height())).y() + 8 if anchor is not None and anchor.isVisible() else 56
        self.move(host.mapToGlobal(QPoint(max(0, (host.width() - width) // 2), top)))
        self.adjustSize()


def open_quick_scan(host):
    """Entry for the top bar, the profile menu and ⌘K: shows the launcher; it never starts anything by itself."""
    if not host.project_directory or getattr(host, "_project_loading", False):
        host.notice.show_error("Сначала откройте проект и дождитесь его данных")
        return None
    previous = getattr(host, "_quick_scan", None)
    if previous is not None:
        try:
            previous.close()
        except RuntimeError:
            pass
    popup = QuickScanPopup(host)
    popup.setAttribute(Qt.WA_DeleteOnClose)
    popup.place()
    popup.show()
    popup.bar.url.setFocus()
    host._quick_scan = popup
    return popup
