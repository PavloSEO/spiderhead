"""Settings → О программе (sheet SetAbout)."""

from __future__ import annotations

import os
import platform
from importlib import metadata

from PyQt5.QtCore import PYQT_VERSION_STR, QT_VERSION_STR, QLocale, Qt, QTimeZone
from PyQt5.QtWidgets import (
    QApplication,
    QFrame,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

from ... import brand, theming
from ...i18n import joined, tr, trf
from ..icons import material_icon
from .actions import Columns, action_button, button_row, format_size, key_values, meta_label
from .helpers import group_label, page

ID, ICON, TITLE = "about", "info", "О программе"
HINT = "Версии, лицензии и сведения для обращения в поддержку"
SCHEMA = ()


LICENSES = (
    ("SEOHEAD Desktop", "GPL v3+"),
    ("PyQt5 5.15", "GPL v3"),
    ("Qt 5.15", "LGPL v3"),
    ("seohead (ядро)", "MIT"),
    ("Roboto, Roboto Mono", "OFL 1.1"),
    ("Material Symbols", "Apache 2.0"),
)


def _core_version():
    try:
        return metadata.version("seohead-seotools")
    except metadata.PackageNotFoundError:
        return None


def _system_rows():
    system = f"macOS {platform.mac_ver()[0]}" if platform.system() == "Darwin" else f"{platform.system()} {platform.release()}"
    screen = QApplication.primaryScreen()
    screen_text = None
    if screen is not None:
        ratio = screen.devicePixelRatio()
        size = screen.size()
        screen_text = trf("{width} × {height} · масштаб {ratio}×", width=round(size.width() * ratio),
                          height=round(size.height() * ratio), ratio=f"{ratio:g}")
    try:
        memory = format_size(os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE"))
    except (AttributeError, ValueError, OSError):
        memory = None
    zone = bytes(QTimeZone.systemTimeZoneId()).decode(errors="replace")
    return [
        ("ОС", f"{system} · {platform.machine()}"),
        ("Экран", screen_text),
        ("Память", memory),
        ("Локаль", f"{QLocale().name()} · {zone}" if zone else QLocale().name()),
    ]


def _license_row(name, license_name):
    row = QFrame()
    row.setProperty("li", True)
    layout = QHBoxLayout(row)
    layout.setContentsMargins(0, 8, 0, 8)
    layout.addWidget(QLabel(name), 1)
    badge = QLabel(license_name)
    badge.setProperty("badge", "mut")
    layout.addWidget(badge)
    return row


def _header(version):
    box = QWidget()
    layout = QVBoxLayout(box)
    layout.setContentsMargins(0, 10, 0, 6)
    layout.setSpacing(6)
    logo = QLabel()
    logo.setPixmap(brand.render(brand.lockup_path(), 240))  # brandbook: About = lockup (mark + name); the product is «SEOHEAD Tools»
    logo.setAccessibleName("SEOHEAD Tools")
    layout.addWidget(logo, 0, Qt.AlignLeft)
    core = _core_version()
    parts = [trf("Desktop · версия {version}", version=version)]
    if core:
        parts.append(trf("ядро {core}", core=core))
    parts.append(tr("открытый исходный код · © 2026 Павел Борушко"))
    sub = meta_label(joined(" · ", parts))
    sub.setContentsMargins(0, 0, 0, 0)
    layout.addWidget(sub)
    return box


def _go_core(button):
    window = button.window()
    if hasattr(window, "show_section"):
        window.show_section("core")


def build_page(store, context):
    versions = [
        ("Приложение", context.app_version),
        ("Ядро seohead", _core_version()),
        ("Python", platform.python_version()),
        ("Qt / PyQt5", f"{QT_VERSION_STR} / {PYQT_VERSION_STR}"),
        ("MCP-протокол", None),
    ]
    system = _system_rows()

    def copy_info():
        lines = [trf("{key}: {value}", key=k, value="Нет данных" if v is None else v) for k, v in versions + system]
        QApplication.clipboard().setText("\n".join(lines))

    copy = QPushButton("Скопировать сведения")
    copy.setIcon(material_icon("content_copy", theming.roles()["text_2"]))
    copy.clicked.connect(copy_info)
    core = QPushButton("Диагностика ядра")
    core.setProperty("role", "text")
    core.clicked.connect(lambda: _go_core(core))
    left = [
        group_label("Версии"),
        key_values(versions),
        group_label("Сведения о системе"),
        key_values(system),
        button_row(copy, core),
    ]
    right = [
        group_label("Лицензии"),
        *(_license_row(*item) for item in LICENSES),
        button_row(action_button("Полный текст NOTICE", context, "open_notice")),
        group_label("Ссылки"),
        button_row(
            action_button("Руководство", context, "open_guide", icon="menu_book"),
            action_button("CLI", context, "open_cli", icon="terminal"),
            action_button("История изменений", context, "open_changelog", icon="history"),
        ),
        meta_label("Сайт продукта: seohead.tech/seotools · исходники: github.com/PavloSEO/seohead-tools"),
    ]
    holder = page(_header(context.app_version), Columns(left, right))
    return holder
