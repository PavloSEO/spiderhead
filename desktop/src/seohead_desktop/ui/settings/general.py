"""Settings → Общие (sheet SetGeneral)."""

from __future__ import annotations

from ...settings_store import Setting
from ..controls import Note
from .helpers import (
    column_stack,
    group_label,
    number_row,
    page,
    segmented_row,
    switch_row,
    text_row,
)

ID, ICON, TITLE = "general", "settings", "Общие"
HINT = "Поведение приложения на этом компьютере"

SCHEMA = (
    Setting("general.start", "last", str, choices=("last", "list")),
    Setting("general.restore_tabs", True, bool),
    Setting("general.recent_count", 20, int, 5, 50, error="Введите число от 5 до 50"),
    Setting("general.projects_folder", "~/Documents/SEOHEAD", str),
    Setting("general.confirm_close_active_scan", True, bool),
    Setting("general.stay_in_menu_bar", False, bool),
    Setting("general.link_action", "details", str, choices=("details", "browser")),
    Setting(
        "general.autosave_seconds",
        30,
        int,
        5,
        600,
        error="Не меньше 5 секунд — иначе файл проекта пишется слишком часто",
    ),
    Setting(
        "general.date_format",
        "dd.MM.yyyy HH:mm",
        str,
        choices=("dd.MM.yyyy HH:mm", "yyyy-MM-dd HH:mm", "MM/dd/yyyy hh:mm a"),
    ),
    Setting("general.crash_stats", False, bool),
)


def build_page(store, context):
    from .helpers import choice_row

    left = [
        group_label("Запуск"),
        segmented_row(
            store,
            "general.start",
            "При запуске открывать",
            "Последний проект с вкладками или список проектов",
            [("last", "Последний проект"), ("list", "Список")],
        ),
        switch_row(
            store,
            "general.restore_tabs",
            "Восстанавливать вкладки",
            "Открытые таблицы, фильтры и позиции прокрутки",
        ),
        number_row(
            store,
            "general.recent_count",
            "Недавние проекты",
            "Сколько показывать на стартовом экране · 5–50",
        ),
        text_row(
            store,
            "general.projects_folder",
            "Папка проектов по умолчанию",
            "Куда создавать новые проекты",
        ),
        group_label("Закрытие"),
        switch_row(
            store,
            "general.confirm_close_active_scan",
            "Подтверждать закрытие при активном скане",
            "Скан продолжится в ядре и после закрытия окна",
        ),
        switch_row(
            store,
            "general.stay_in_menu_bar",
            "Оставаться в строке меню",
            "Иконка с прогрессом сканов, когда окно закрыто",
        ),
    ]
    right = [
        group_label("Поведение"),
        segmented_row(
            store,
            "general.link_action",
            "Открывать ссылки на URL",
            "Клик по адресу в таблице",
            [("details", "Детали"), ("browser", "Браузер")],
        ),
        number_row(
            store,
            "general.autosave_seconds",
            "Автосохранение представлений",
            "Интервал в секундах · 5–600",
        ),
        choice_row(
            store,
            "general.date_format",
            "Формат дат",
            "В таблицах, отчётах и журнале",
            [
                ("dd.MM.yyyy HH:mm", "09.10.2026 14:30"),
                ("yyyy-MM-dd HH:mm", "2026-10-09 14:30"),
                ("MM/dd/yyyy hh:mm a", "10/09/2026 02:30 PM"),
            ],
        ),
        switch_row(
            store,
            "general.crash_stats",
            "Анонимная статистика сбоев",
            "Только трассировка ошибки, без URL и данных проектов",
        ),
        Note(
            "info",
            "Что это меняет.",
            "Настройки этого компьютера. В файл проекта не пишутся: коллега с тем же проектом увидит свои значения. Профили скана и представления — в проекте.",
        ),
    ]
    return page(column_stack(left, right))
