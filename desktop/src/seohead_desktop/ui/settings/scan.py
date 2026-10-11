"""Settings → Сканы по умолчанию (sheet SetScan)."""

from __future__ import annotations

import math
import os

from PyQt5.QtWidgets import QLineEdit, QVBoxLayout, QWidget

from ... import i18n
from ...i18n import joined, tr, trf
from ...settings_store import Setting
from ..controls import Note, SettingRow
from .helpers import group_label, keyed, page, segmented_row, switch_row
from .listing import Columns, action_button, badge, buttons_row, list_item

ID, ICON, TITLE = "scan", "manage_search", "Сканы по умолчанию"
HINT = "Значения для новых сканов на этом компьютере"

PROFILES = (
    ("quick", "Быстрый аудит", "1 500 URL · 2 запр/с · без HTML"),
    ("full", "Полный краул", "50 000 URL · 2 запр/с · HTML"),
    ("sitemap", "Только sitemap", "список из sitemap.xml · 1 запр/с"),
    ("js", "JS-рендер", "500 URL · Chromium · 1 запр/с"),
)

SCHEMA = (
    Setting("scan.rate", 2.0, float, 0.1, 10, error="Максимум 10 запр/с; больше — только профиль «Свой сайт»"),
    Setting("scan.url_limit", 1500, int, 1, 1_000_000, error="Введите число от 1 до 1 000 000"),
    Setting("scan.depth", 10, int, 0, 100, error="Введите число от 0 до 100"),
    Setting("scan.robots", True, bool),
    Setting("scan.save_html", True, bool),
    Setting("scan.parallel", 3, int, 1, 4),
    Setting("scan.sf_path", "/Applications/Screaming Frog SEO Spider.app", str),
    Setting("scan.default_profile", "quick", str, choices=tuple(p[0] for p in PROFILES)),
)


def _spaced(value):
    return f"{value:,}".replace(",", " ")


def _rate_text(value):
    return f"{value:g}".replace(".", ",")


def _rate_precheck(text):
    try:
        value = float(text.replace(",", ".").replace(" ", ""))
    except ValueError:
        value = math.nan
    if not math.isfinite(value):
        return "Введите число от 0,1 до 10"
    return "Не меньше 0,1 запр/с" if value < 0.1 else None


def _number_row(store, key, title, description, fmt, precheck=None):
    """Field that stores only valid input; otherwise keeps the text and shows the error under the description."""
    field = QLineEdit(fmt(store.get(key)))
    field.setFixedWidth(96)
    row = keyed(SettingRow(title, description, field), key)

    def commit():
        text = field.text()
        error = precheck(text) if precheck else None
        if error is None:
            error = store.set(key, text)
        row.set_error(error)
        if not error:
            field.setText(fmt(store.get(key)))

    field.editingFinished.connect(commit)
    field.textEdited.connect(lambda _t: row.set_error(None))
    return row


def _sf_path_row(store):
    base = "Для режима «краул через SF»"
    field = QLineEdit(store.get("scan.sf_path"))
    field.setFixedWidth(320)
    field.setCursorPosition(0)
    field.setProperty("mono", True)
    row = keyed(SettingRow("Путь к CLI", base, field), "scan.sf_path")

    def refresh():
        path = os.path.expanduser(store.get("scan.sf_path"))
        row.description.setText(tr(base) if path and os.path.exists(path) else trf("{base} · не найден по этому пути", base=base))

    def commit():
        row.set_error(store.set("scan.sf_path", field.text().strip()))
        refresh()

    field.editingFinished.connect(commit)
    refresh()
    return row


def _license_row(context):
    info = context.request("sf_license")
    if not isinstance(info, dict):
        control = badge("mut", "нет данных", "help")
        description = "Проверка лицензии недоступна в этой сборке"
    else:
        valid = bool(info.get("valid"))
        control = badge("ok" if valid else "err", "действует" if valid else "не действует", "verified" if valid else "cancel")
        description = joined(" · ", [p for p in (trf("Проверено {when}", when=info["checked"]) if info.get("checked") else "",
                                            trf("до {when}", when=info["until"]) if info.get("until") else "") if p]) or tr("Нет данных")
    return SettingRow("Лицензия", description, control)


def _profiles(store):
    box = QWidget()
    layout = QVBoxLayout(box)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(0)

    def refresh(key="scan.default_profile"):
        if key != "scan.default_profile":
            return
        while layout.count():
            item = layout.takeAt(0)
            widget = item.widget()
            widget.setParent(None)
            widget.deleteLater()
        current = store.get("scan.default_profile")
        for profile_id, name, summary in PROFILES:
            if profile_id == current:
                trailing = badge("info", "по умолчанию", "star")
            else:
                trailing = action_button("Сделать по умолчанию", role="text", size="pill")
                trailing.clicked.connect(lambda _c, pid=profile_id: store.set("scan.default_profile", pid))
            layout.addWidget(list_item("tune", name, summary, trailing))
        i18n.retranslate(box)

    store.changed.connect(refresh)
    box.destroyed.connect(lambda: _disconnect(store, refresh))
    refresh()
    return box


def _disconnect(store, slot):
    try:
        store.changed.disconnect(slot)
    except (TypeError, RuntimeError):
        pass


def build_page(store, context):
    left = [
        group_label("Нагрузка на сайт"),
        _number_row(store, "scan.rate", "Запросов в секунду", "Для боевых сайтов 1–2. Больше 2 — с подтверждением", _rate_text, _rate_precheck),
        _number_row(store, "scan.url_limit", "Лимит URL", "Защита от бесконечных фильтров и календарей", _spaced),
        _number_row(store, "scan.depth", "Глубина обхода", "Кликов от стартовой; 0 — без ограничения", str),
        switch_row(store, "scan.robots", "Соблюдать robots.txt", "Выключение — только для своих сайтов"),
        switch_row(store, "scan.save_html", "Сохранять HTML страниц", "Поиск в HTML и сравнение текстов · ~ 250 КБ на URL"),
        segmented_row(store, "scan.parallel", "Одновременных сканов", "Остальные ждут в очереди", [(n, str(n)) for n in (1, 2, 3, 4)]),
    ]
    doctor = action_button("Диагностика SF", "stethoscope", enabled=context.can("sf_doctor"))
    doctor.clicked.connect(lambda: context.request("sf_doctor"))
    new_scan = action_button("Открыть «Новый скан»", role="text", enabled=context.can("new_scan"))
    new_scan.clicked.connect(lambda: context.request("new_scan"))
    add_profile = action_button("Новый профиль", "add", enabled=context.can("new_profile"))
    add_profile.clicked.connect(lambda: context.request("new_profile"))
    right = [
        group_label("Screaming Frog"),
        _sf_path_row(store),
        _license_row(context),
        buttons_row(doctor, new_scan),
        group_label("Профили скана"),
        _profiles(store),
        buttons_row(add_profile),
    ]
    note = Note("info", "Что это меняет.", "Подставляется в «Новый скан». Профиль проекта важнее этих значений; уже запущенные сканы не меняются.")
    return page(Columns(left, right), note)
