"""Explicit first-run CLI installation for the verified macOS app bundle."""
from __future__ import annotations

import shlex
import subprocess
import sys
from pathlib import Path

from PyQt5.QtCore import QThreadPool
from PyQt5.QtWidgets import QMessageBox, QPushButton

from .bundle import verified_bundled_core_identity
from .i18n import tr
from .integration_worker import run_background


def install_cli():
    """Called only after the install button's consent; macOS asks for admin authorization."""
    identity = verified_bundled_core_identity()
    if sys.platform != "darwin" or not getattr(sys, "frozen", False) or identity is None:
        raise ValueError(tr("Установка CLI доступна только из проверенного комплекта macOS."))
    app = Path(sys.executable).resolve().parents[2]
    if not str(app).endswith(".app") or app.parent != Path("/Applications"):
        raise ValueError(tr("Сначала перенесите приложение в «Программы»."))
    source = Path(__file__).parent / "assets/app/seohead-cli.sh"
    if not source.is_file():
        raise ValueError(tr("Обёртка CLI отсутствует в комплекте."))
    # All dynamic paths are shell-quoted. No secrets, network or agent files are involved.
    script = "set -eu\n" + "\n".join([
        "destination=/usr/local/bin/seohead",
        '[ ! -L "$destination" ] || exit 73',
        'if [ -e "$destination" ]; then',
        'grep -q SEOHEAD_BUNDLED_CLI_V1 "$destination" || exit 73',
        'backup="$destination.before-$(date -u +%Y%m%dT%H%M%SZ)"',
        'cp -p "$destination" "$backup"',
        '[ "$(shasum -a 256 < "$destination")" = "$(shasum -a 256 < "$backup")" ] || exit 74',
        'fi',
        'mkdir -p /usr/local/bin',
        f'cp {shlex.quote(str(source))} "$destination.tmp.$$"',
        'chmod 755 "$destination.tmp.$$"',
        'mv "$destination.tmp.$$" "$destination"',
    ])
    # AppleScript literals use JSON-compatible string quoting for quotes/backslashes.
    import json
    apple = "do shell script " + json.dumps(script) + " with administrator privileges"
    completed = subprocess.run(["osascript", "-e", apple], capture_output=True, timeout=120, check=False)
    if completed.returncode:
        raise ValueError(tr("Установка отменена или не выполнена. Чужая обёртка не заменяется."))
    return tr("Команда seohead установлена в /usr/local/bin.")


class InstallCLIButton(QPushButton):
    def __init__(self, parent=None):
        super().__init__(tr("Установить командную строку"), parent)
        self.setProperty("role", "secondary")
        self.setVisible(sys.platform == "darwin" and getattr(sys, "frozen", False))
        self.clicked.connect(self.confirm)

    def confirm(self):
        choice = QMessageBox.question(self, tr("Установить командную строку"), tr("Добавить seohead в /usr/local/bin? Потребуется пароль администратора macOS. Используется ядро из этого приложения."), QMessageBox.Yes | QMessageBox.Cancel, QMessageBox.Cancel)
        if choice != QMessageBox.Yes:
            return
        self.setEnabled(False)
        run_background(QThreadPool.globalInstance(), install_cli, self.completed, self)

    def completed(self, value):
        self.setEnabled(True)
        if isinstance(value, Exception):
            QMessageBox.warning(self, tr("Командная строка"), str(value))
        else:
            QMessageBox.information(self, tr("Командная строка"), value)
