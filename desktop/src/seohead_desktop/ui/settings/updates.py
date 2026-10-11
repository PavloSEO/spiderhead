"""Settings → Обновления (sheet SetUpdates)."""

from __future__ import annotations

from importlib import metadata

from ...settings_store import Setting
from ..controls import Note
from ..kit import waiting_badge
from .actions import Columns, action_button, button_row, key_values, spacer
from .helpers import group_label, page, segmented_row, switch_row

ID, ICON, TITLE = "updates", "system_update", "Обновления"
CHANGELOG_GAP = 1220  # core update contract, see ui/kit.ISSUE_HINTS
HINT = "Приложение и совместимое ядро обновляются вместе"

SCHEMA = (
    Setting("updates.channel", "stable", str, choices=("stable", "beta")),
    Setting("updates.auto_check", True, bool),
    Setting("updates.background_download", False, bool),
)


def _core_version():
    try:
        return metadata.version("seohead-seotools")
    except metadata.PackageNotFoundError:
        return None


def build_page(store, context):
    left = []
    if not context.can("check_updates"):
        left += [Note("info", "Проверка обновлений недоступна в этой сборке."), spacer(12)]
    left += [
        key_values([("Установлено", context.app_version), ("Ядро", _core_version()), ("Последняя проверка", None)]),
        segmented_row(store, "updates.channel", "Канал", "beta — раньше на неделю, возможны ошибки",
                      [("stable", "stable"), ("beta", "beta")]),
        switch_row(store, "updates.auto_check", "Проверять автоматически", "Раз в день; без загрузки без вашего согласия"),
        switch_row(store, "updates.background_download", "Загружать в фоне", "Установка — только по кнопке"),
        button_row(action_button("Проверить сейчас", context, "check_updates", icon="refresh")),
    ]
    right = [
        group_label("История изменений"),
        button_row(waiting_badge(CHANGELOG_GAP)),
        Note("info", "Что это меняет.", "Приложение и ядро — вместе. Обновление ставит совместимое ядро; CLI в PATH тоже обновится."),
    ]
    return page(Columns(left, right))
