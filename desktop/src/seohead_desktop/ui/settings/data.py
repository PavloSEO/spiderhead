"""Settings → Хранилище и данные (sheet SetData)."""

from __future__ import annotations

from ...i18n import tr, trf
from ...settings_store import Setting
from ..controls import Note
from ..kit import waiting_badge
from .actions import (
    Columns,
    action_button,
    button_row,
    format_size,
    key_values,
    measure,
    meta_label,
    mono,
)
from .helpers import group_label, page, segmented_row

ID, ICON, TITLE = "data", "database", "Хранилище и данные"
HINT = "Сканы лежат в папке проекта; здесь — данные приложения"

SCHEMA = (
    Setting("data.keep_scans", "20", str, choices=("10", "20", "all")),
    Setting("data.html_age", "90", str, choices=("30", "90", "never")),
)


def build_page(store, context):
    backend = getattr(store, "_backend", None)  # QSettings when persistent, None for in-memory
    settings_file = (
        backend.fileName() if backend is not None and hasattr(backend, "fileName") else None
    )
    values = key_values(
        [
            ("Сканы", "в папке проекта · scans/"),
            ("Настройки", settings_file),
            ("Кэш", context.data_directory),
            ("Логи", context.log_directory),
            ("Свободно", None),
        ],
        mono_rows=("Настройки", "Кэш", "Логи"),
    )
    left = [
        group_label("Места"),
        values,
        group_label("Сканы по проектам"),
        button_row(waiting_badge(1213)),
    ]
    right = [
        group_label("Очистка"),
        segmented_row(
            store,
            "data.keep_scans",
            "Хранить сканов на проект",
            "Старые без закрепления — в Корзину",
            [("10", "10"), ("20", "20"), ("all", "все")],
        ),
        segmented_row(
            store,
            "data.html_age",
            "Удалять HTML старше",
            "Таблицы и метрики остаются",
            [("30", "30 дн"), ("90", "90 дн"), ("never", "никогда")],
        ),
        button_row(
            action_button("Очистить кэш", context, "clear_cache"),
            action_button("Очистить логи", context, "clear_logs"),
        ),
        button_row(
            action_button(
                "Сбросить все панели", context, "reset_panels", icon="restart_alt", role="danger"
            )
        ),
        Note("warn", "Удаление идёт в Корзину, не насовсем.", "Закреплённые сканы не трогаются."),
        group_label("Настройки приложения"),
        button_row(
            action_button("Экспорт настроек…", context, "export_settings", icon="file_download"),
            action_button("Импорт…", context, "import_settings", icon="file_upload"),
        ),
        meta_label(
            trf(
                "Файл {file} без ключей и токенов — они остаются в {folder}",
                file=mono(".json"),
                folder=mono(tr("Папка конфигурации")),
            ),
            rich=True,
        ),
    ]

    holder = page(Columns(left, right))
    sinks = {
        k: p for k, p in (("Кэш", context.data_directory), ("Логи", context.log_directory)) if p
    }
    disk_path = context.data_directory or context.project_directory
    if sinks or disk_path:

        def show(result):
            for key, path in sinks.items():
                size = result.get(key)
                values.set_value(
                    key,
                    path
                    if size is None
                    else trf("{path} · {size}", path=path, size=format_size(size)),
                )
            disk = result.get("disk")
            values.set_value(
                "Свободно",
                None
                if disk is None
                else trf(
                    "{free} из {total}", free=format_size(disk[0]), total=format_size(disk[1])
                ),
            )

        measure(holder, sinks, disk_path, show)
    return holder
