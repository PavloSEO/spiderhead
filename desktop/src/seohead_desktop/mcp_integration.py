"""Permissioned MCP setup through the core CLI (``seohead mcp … --json``), always on a worker thread.

The core owns the state (``~/.config/seohead/mcp-state.json``), the client registration and the adjacent backups; the
app shows the core's plan and writes only after an explicit «Разрешить». Paths are shown with ``~``.
"""
from __future__ import annotations

import json
import subprocess

from PyQt5.QtCore import Qt, QThreadPool, QTimer
from PyQt5.QtWidgets import (
    QCheckBox,
    QDialog,
    QFrame,
    QHBoxLayout,
    QLabel,
    QMessageBox,
    QPushButton,
    QScrollArea,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from . import i18n
from .i18n import joined, tr, trf
from .integration_worker import run_background
from .ui.controls import Note
from .ui.icons import MaterialIconLabel, material_icon
from .ui.settings import mcp as sheet
from .ui.settings.listing import tilde

CLIENTS = tuple((cid, name) for cid, name, _path in sheet.CLIENTS)
ACTOR = "SEOHEAD Desktop"
SERVER_KEY = {"codex": "mcp_servers.seohead"}


def call_core(executable, arguments):
    """Capture only the core's JSON; never expose arbitrary process stderr."""
    if not executable:
        raise ValueError(tr("CLI ядра seohead не найден"))
    try:
        completed = subprocess.run([executable, "mcp", *arguments, "--json"], capture_output=True, text=True, timeout=30, check=False)
        result = json.loads(completed.stdout)
    except (OSError, subprocess.TimeoutExpired, ValueError):
        raise ValueError(tr("Ядро не отвечает или не поддерживает настройку MCP.")) from None
    if not isinstance(result, dict) or not result.get("ok"):
        raise ValueError((result.get("error") if isinstance(result, dict) else None) or tr("Ошибка настройки MCP"))
    return result


def diff_lines(block):
    """The core's fragment as added lines («+ …»)."""
    return "\n".join("+ " + line for line in str(block).rstrip("\n").splitlines())


class PermissionDialog(QDialog):
    """Sheet Modals «Разрешить запись MCP»: clients with check boxes, the core's dry-run plan as a «+» diff, then one write per client."""

    def __init__(self, executable, chosen=("claude-code",), parent=None, runner=call_core):
        super().__init__(parent)
        self.executable, self.runner = executable, runner
        self.status = None
        self.plans = {}
        self.errors = {}
        self.busy = False
        self.done = []
        self.setWindowTitle(tr("Разрешить запись MCP"))
        self.setModal(True)
        self.setMinimumWidth(560)
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        head = QHBoxLayout()
        head.setContentsMargins(24, 18, 16, 8)
        head.setSpacing(12)
        head.addWidget(MaterialIconLabel("hub", 24, color="role:primary"), 0, Qt.AlignTop)
        title = QLabel(tr("Разрешить запись MCP"))
        title.setProperty("text_style", "dialog")
        head.addWidget(title, 1)
        close = QToolButton()
        close.setProperty("role", "icon")
        close.setIcon(material_icon("close", "role:text_2"))
        close.setAccessibleName(tr("Закрыть"))
        close.clicked.connect(self.reject)
        head.addWidget(close, 0, Qt.AlignTop)
        root.addLayout(head)
        body = QVBoxLayout()
        body.setContentsMargins(24, 4, 24, 16)
        body.setSpacing(12)
        intro = QLabel(tr("SEOHEAD изменит файлы настроек выбранных агентов. Перед записью — копия каждого файла рядом (*.seohead-<время>-<хэш>.bak, хэш SHA-256 сверяется); отменить можно в Настройках → MCP-сервер."))
        intro.setWordWrap(True)
        body.addWidget(intro)
        self.boxes, self.paths = {}, {}
        for cid, name in CLIENTS:
            row = QFrame()
            row.setProperty("list_item", True)
            line = QHBoxLayout(row)
            line.setContentsMargins(0, 6, 0, 6)
            box = QCheckBox(name)
            box.setChecked(cid in chosen)
            box.setEnabled(False)
            box.toggled.connect(lambda checked, c=cid: self._toggled(c, checked))
            path = QLabel()
            path.setProperty("text_style", "meta")
            path.setTextFormat(Qt.PlainText)
            path.setWordWrap(True)
            line.addWidget(box)
            line.addWidget(path, 1)
            body.addWidget(row)
            self.boxes[cid], self.paths[cid] = box, path
        self.preview = QLabel()
        self.preview.setProperty("text_style", "mono")
        self.preview.setTextFormat(Qt.PlainText)
        self.preview.setTextInteractionFlags(Qt.TextSelectableByMouse)
        self.preview.setWordWrap(True)
        self.preview.setAccessibleName(tr("Что добавится в конфиги"))
        self.preview.setObjectName("mcpDiff")
        self.preview_scroll = QScrollArea()
        self.preview_scroll.setWidgetResizable(True)
        self.preview_scroll.setFrameShape(QFrame.NoFrame)
        self.preview_scroll.setMaximumHeight(240)  # several clients: the diff scrolls, the footer stays visible
        self.preview_scroll.setWidget(self.preview)
        body.addWidget(self.preview_scroll)
        self.backup_label = QLabel()
        self.backup_label.setProperty("text_style", "meta")
        self.backup_label.setTextFormat(Qt.PlainText)
        self.backup_label.setWordWrap(True)
        body.addWidget(self.backup_label)
        body.addWidget(Note("info", "Токены и пароли не записываются.", "Остальные строки файлов не меняются."))
        self.result_label = QLabel(tr("Чтение состояния ядра…"))
        self.result_label.setWordWrap(True)
        self.result_label.setTextFormat(Qt.PlainText)
        self.result_label.setProperty("text_style", "meta")
        body.addWidget(self.result_label)
        root.addLayout(body)
        footer = QFrame()
        footer.setObjectName("dialogFooter")
        foot = QHBoxLayout(footer)
        foot.setContentsMargins(24, 12, 24, 12)
        foot.addStretch(1)
        self.cancel = QPushButton(tr("Отмена"))
        self.cancel.setProperty("role", "text")
        self.cancel.clicked.connect(self.reject)
        self.allow = QPushButton(tr("Разрешить"))
        self.allow.setProperty("role", "primary")
        self.allow.setEnabled(False)
        self.allow.clicked.connect(self.approve)
        foot.addWidget(self.cancel)
        foot.addWidget(self.allow)
        root.addWidget(footer)
        i18n.retranslate(self)
        self._run(["status"], self._status)

    # core calls
    def _run(self, arguments, callback):
        self.busy = True
        self.allow.setEnabled(False)
        run_background(QThreadPool.globalInstance(), lambda: self.runner(self.executable, arguments), lambda value: self._delivered(value, callback), self)

    def _delivered(self, value, callback):
        self.busy = False
        if isinstance(value, Exception):
            self.result_label.setText(str(value))
            self.cancel.setEnabled(True)
            self._update()
        else:
            callback(value)

    def _status(self, status):
        self.status = status
        for cid, box in self.boxes.items():
            info = (status.get("clients") or {}).get(cid) or {}
            box.setEnabled(info.get("registered") is not None)
            if info.get("registered") is None:
                box.setChecked(False)
            self.paths[cid].setText(tilde(info.get("file") or ""))
        self._plan_next()

    def _toggled(self, cid, checked):
        if checked and cid not in self.plans and not self.busy:
            self._plan_next()
        self._update()

    def _plan_next(self):
        missing = [cid for cid, box in self.boxes.items() if box.isChecked() and cid not in self.plans and cid not in self.errors]
        if not missing:
            self.result_label.setText(tr("Запись произойдёт только после «Разрешить»."))
            self._update()
            return
        cid = missing[0]
        self._run(["install", "--client", cid, "--dry-run", "--command", self.executable], lambda plan: self._planned(cid, plan))

    def _planned(self, cid, plan):
        self.plans[cid] = plan
        self._plan_next()

    def chosen(self):
        return [cid for cid, box in self.boxes.items() if box.isChecked() and cid in self.plans]

    def _update(self):
        for cid, box in self.boxes.items():
            info = ((self.status or {}).get("clients") or {}).get(cid) or {}
            plan = self.plans.get(cid)
            what = tr("уже прописано") if plan and not plan.get("changed", True) else trf("добавить {key}", key=SERVER_KEY.get(cid, "mcpServers.seohead")) if box.isChecked() else ""
            self.paths[cid].setText(joined(" · ", [p for p in (tilde(info.get("file") or ""), what) if p]))
        chosen = [cid for cid in self.chosen() if self.plans[cid].get("changed", True)]
        self.preview.setText("\n".join(f"{tilde(self.plans[c]['file'])}\n{diff_lines(self.plans[c]['block'])}" for c in chosen))
        self.preview_scroll.setVisible(bool(chosen))
        self.backup_label.setText("\n".join(trf("Бэкап: {path}", path=tilde(self.plans[c]["backup"])) for c in chosen))
        self.backup_label.setVisible(bool(chosen))
        self.allow.setEnabled(bool(chosen) and not self.busy and not self.done)
        self.adjustSize()

    def approve(self):
        pending = [cid for cid in self.chosen() if self.plans[cid].get("changed", True) and cid not in self.done]
        if not pending or self.busy:
            return
        for box in self.boxes.values():
            box.setEnabled(False)
        self.cancel.setEnabled(False)
        cid, plan = pending[0], self.plans[pending[0]]
        self._run(["install", "--client", cid, "--yes", "--command", self.executable, "--expected-sha256", plan["sha256"], "--backup", plan["backup"]],
                  lambda result: self._installed(cid, result))

    def _installed(self, cid, _result):
        self.done.append(cid)
        if [c for c in self.chosen() if self.plans[c].get("changed", True) and c not in self.done]:
            self.approve()
            return
        self.result_label.setText(trf("MCP прописан: {clients}", clients=", ".join(dict(CLIENTS)[c] for c in self.done)))
        self.cancel.setEnabled(True)
        self.cancel.setText(tr("Готово"))
        self.cancel.clicked.disconnect()
        self.cancel.clicked.connect(self.accept)
        self.allow.hide()

    def reject(self):
        if not (self.busy and self.cancel.isEnabled() is False):
            super().reject()

    def closeEvent(self, event):
        if self.busy and not self.cancel.isEnabled():
            event.ignore()
        else:
            super().closeEvent(event)


class IntegrationPanel(QWidget):
    """Settings → MCP-сервер, live: the sheet drawn from ``mcp status --json`` and ``mcp backups --json``."""

    def __init__(self, executable, store, parent=None, runner=call_core, polling=True):
        super().__init__(parent)
        self.executable, self.store, self.runner = executable, store, runner
        self.state = None
        self.backups = None
        self.busy = False
        self.message = ""
        self._layout = QVBoxLayout(self)
        self._layout.setContentsMargins(0, 0, 0, 0)
        self._body = None
        self._render()
        self.timer = QTimer(self)
        self.timer.setInterval(2000)
        self.timer.timeout.connect(self.refresh)
        if polling:
            self.timer.start()

    def handlers(self):
        if self.state is None:
            return {}
        return {"toggle": self.toggle, "profile": self.change_profile, "connect": self.connect_client, "restore": self.restore}

    def _render(self):
        body = sheet.sections(self.store, self.state, self.backups, self.handlers())
        if self.message:
            feedback = QLabel(self.message)
            feedback.setWordWrap(True)
            feedback.setTextFormat(Qt.PlainText)
            feedback.setProperty("text_style", "meta")
            body.layout().insertWidget(0, feedback)
        if self._body is not None:
            self._layout.removeWidget(self._body)
            self._body.deleteLater()
        self._body = body
        i18n.retranslate(body)
        self._layout.addWidget(body)

    def showEvent(self, event):
        super().showEvent(event)
        self.refresh()

    def _request(self, function, callback):
        if self.busy:
            return
        self.busy = True
        run_background(QThreadPool.globalInstance(), function, lambda value: self._received(value, callback), self)

    def _received(self, value, callback):
        self.busy = False
        if isinstance(value, Exception):
            self.message = str(value)
            self._render()
        else:
            callback(value)

    def _read(self):
        status = self.runner(self.executable, ["status"])
        try:
            backups = self.runner(self.executable, ["backups"]).get("backups")
        except ValueError:
            backups = None
        return status, backups

    def refresh(self):
        if self.isVisible():  # a hidden or closed page never keeps calling the core
            self._request(self._read, self.loaded)

    def loaded(self, answer):
        status, backups = answer
        if (status, backups) == (self.state, self.backups) and not self.message:
            return
        self.state, self.backups, self.message = status, backups, ""
        if self.store:
            self.store.set("mcp.enabled", bool(status.get("enabled")))
            if status.get("profile") in sheet.PROFILES:
                self.store.set("mcp.profile", status["profile"])
        self._render()

    def _change(self, arguments):
        def run():
            self.runner(self.executable, arguments)
            return self._read()

        self._request(run, self.loaded)

    def toggle(self, enabled):
        self._change(["enable" if enabled else "disable", "--actor", ACTOR])

    def change_profile(self, profile):
        self._change(["enable" if (self.state or {}).get("enabled") else "disable", "--profile", profile, "--actor", ACTOR])

    def connect_client(self, client):
        PermissionDialog(self.executable, (client,), self, self.runner).exec_()
        self.refresh()

    def restore(self, client):
        choice = QMessageBox.question(self, tr("Восстановить конфиг MCP"), tr("Восстановить конфиг клиента из последнего проверенного бэкапа? Запись SEOHEAD будет убрана."),
                                      QMessageBox.Yes | QMessageBox.Cancel, QMessageBox.Cancel)
        if choice == QMessageBox.Yes:
            self._change(["uninstall", "--client", client, "--yes"])
