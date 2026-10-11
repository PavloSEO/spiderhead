"""Which stored settings already change application behaviour, and when the others will.

Every key of the settings schema must be classified here (tests enforce it). A row whose key is
not applied yet shows the badge «Заработает позже»; the same table is documented in
desktop/docs/spec/settings-wiring.ru.md (regenerate with scripts/gen_settings_wiring.py).
"""

from __future__ import annotations

from ...i18n import trf

# view.language is applied to the shell and the settings; the screens follow as they are rebuilt (roadmap step 5).
APPLIED = {"mcp.enabled", "mcp.profile", "view.details_position", "view.mono_urls", "view.status_badges", "view.theme", "view.density", "view.reduce_motion", "view.rail_when_narrow", "shell.display", "view.language",
           "shell.onboarding_done", "general.projects_folder",
           "scan.rate", "scan.url_limit", "scan.depth", "scan.robots", "scan.save_html"}
APPLIED_SHORTCUTS = {"palette", "new_scan", "settings", "stop_scan", "find_in_table", "expand_table", "copy_url", "help"}

# Core issue numbers behind a reason: shown in docs/spec/settings-wiring.ru.md only, never in the UI.
ISSUES = {"scan.default_profile": 941}

# key or "prefix." -> (roadmap step, what has to exist first)
LATER = {
    "view.zoom": (5, "масштаб при перестройке экранов"),
    "general.": (5, "экран «Старт · проекты», вкладки и таблицы"),
    "scan.parallel": (5, "менеджер запусков: число одновременных сканов"),
    "scan.sf_path": (5, "запуск Screaming Frog из приложения"),
    "scan.default_profile": (5, "профили скана в ядре"),
    "scan.": (5, "диалог «Новый скан»"),
    "core.poll": (5, "наблюдение за сканом"),
    "core.": (8, "адаптеры ядра: версия, диагностика, логи"),
    "mcp.": (7, "состояние MCP и запись конфигов агентов"),
    "agent.": (7, "подключение агента и журнал его действий"),
    "notify.": (7, "уведомления о сканах и агенте"),
    "data.": (8, "хранилище, журнал и экспорт настроек"),
    "updates.": (9, "после первого релиза"),
    "sources.": (7, "OAuth, ключи и расходы источников данных"),
}
LATER_SHORTCUTS = (5, "действие ещё не имеет экрана")


def status(key):
    """(None, "") when the key is applied, else (step, reason)."""
    if key in APPLIED:
        return None, ""
    if key.startswith("keys."):
        return (None, "") if key[5:] in APPLIED_SHORTCUTS else LATER_SHORTCUTS
    if key in LATER:
        return LATER[key]
    for prefix in sorted((k for k in LATER if k.endswith(".")), key=len, reverse=True):
        if key.startswith(prefix):
            return LATER[prefix]
    raise KeyError(f"setting {key!r} is not classified in ui/settings/wiring.py")


def apply(page):
    """Badge every row of a section page whose setting is stored but not applied yet."""
    from ..controls import SettingRow

    for row in page.findChildren(SettingRow):
        key = row.property("setting_key")
        if key:
            step, reason = status(key)
            if step is not None:
                row.mark_later(trf("Настройка сохраняется, но пока ничего не меняет: {reason} (шаг {step})", reason=reason, step=step))


def all_keys(schema):
    return [setting.key for setting in schema]


def table(schema):
    """Rows (key, step|None, reason) for the documentation."""
    return [(key, *status(key)) for key in all_keys(schema)]

