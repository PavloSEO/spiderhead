"""Settings → MCP-сервер (sheet SetMcp).

The same sheet is drawn in two ways: live (``mcp_integration.IntegrationPanel`` reads ``seohead mcp status --json`` and
``seohead mcp backups --json`` on a worker and passes ``handlers``) and, without a core, from ``context.request``
answers with every action disabled. Nothing here runs the CLI or writes a configuration.
"""

from __future__ import annotations

import os
from datetime import datetime

from PyQt5.QtWidgets import QHBoxLayout, QWidget

from ...i18n import joined, tr, trf
from ...settings_store import Setting
from ..controls import Note, Segmented, SettingRow, Switch
from .helpers import group_label, keyed, page, switch_row
from .listing import UNAVAILABLE, Columns, action_button, badge, hint, list_item, mono_html, no_data, terminal, tilde

ID, ICON, TITLE = "mcp", "hub", "MCP-сервер"
HINT = "Доступ агентов к проектам через локальный MCP"

PROFILES = ("full", "audit", "quick-check", "router", "infra")
USER_PROFILES = ("full", "audit", "quick-check")  # infra and router are CLI-only (sheet SetMcp)

SCHEMA = (
    Setting("mcp.enabled", True, bool),
    Setting("mcp.profile", "full", str, choices=PROFILES),
    Setting("mcp.journal", True, bool),
)

CLIENTS = (
    ("claude-code", "Claude Code", "~/.claude.json"),
    ("claude-desktop", "Claude Desktop", "claude_desktop_config.json"),
    ("codex", "Codex", "~/.codex/config.toml"),
    ("cursor", "Cursor", "~/.cursor/mcp.json"),
)
NAMES = {cid: name for cid, name, _path in CLIENTS}
REASONS = {"install": "до прописки", "uninstall": "до восстановления"}
JOURNAL_GAP = 945


def _status(context):
    status = context.request("mcp_status")
    return status if isinstance(status, dict) else None


def server_row(store, status, on_toggle=None):
    switch = Switch("MCP-сервер", bool(status["enabled"]) if status and "enabled" in status else bool(store.get("mcp.enabled")))

    def toggled(checked):
        store.set("mcp.enabled", checked)
        if on_toggle:
            on_toggle(checked)

    switch.toggled.connect(toggled)
    if status is None or "enabled" not in status:
        state = badge("mut", "нет данных", "help")
    elif status["enabled"]:
        state = badge("ok", "включён", "check_circle")
    else:
        state = badge("mut", "выключен", "pause_circle")
    box = QWidget()
    layout = QHBoxLayout(box)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(12)
    layout.addWidget(state)
    layout.addWidget(switch)
    tools = (status or {}).get("tools")
    profile = (status or {}).get("profile")
    count = [trf("{n} инструментов в профиле {profile}", n=tools, profile=profile) if profile else trf("{n} инструментов", n=tools)] if tools else []
    return keyed(SettingRow("Локальный MCP-сервер", joined(" · ", ["stdio · без сети", *count]), box), "mcp.enabled")


def profile_row(store, status, on_profile=None):
    known = [p.get("id") for p in (status or {}).get("profiles") or [] if isinstance(p, dict)]
    options = [(p, p) for p in USER_PROFILES if not known or p in known]
    current = (status or {}).get("profile") or store.get("mcp.profile")
    seg = Segmented(options, current if current in dict(options) else None, accessible_name="Профиль инструментов")

    def changed(value):
        store.set("mcp.profile", value)
        if on_profile:
            on_profile(value)

    seg.changed.connect(changed)
    row = SettingRow("Профиль инструментов", "full — все; audit — чтение и аудит; quick-check — быстрые проверки. Служебные infra и router — только из CLI", seg)
    return keyed(row, "mcp.profile")


def journal_row(store):
    row = switch_row(store, "mcp.journal", "Писать журнал вызовов", "Каждый вызов инструмента — в журнал агента")
    row.control.setEnabled(False)
    row.mark_later("Журнал вызовов MCP появится в ядре")
    return row


def console(status):
    if status is None:
        return terminal("seohead mcp status", [tr("Нет данных")])
    enabled = status.get("enabled")
    head = trf("● {state}", state={True: "включён", False: "выключен"}.get(enabled, "нет данных"))
    lines = [head]
    if status.get("profile"):
        lines.append(joined(" · ", [trf("профиль {profile}", profile=status["profile"]),
                                    *([trf("кем: {by}", by=status["by_label"])] if status.get("by_label") else [])]))
    registered = [name for cid, name, _path in CLIENTS if ((status.get("clients") or {}).get(cid) or {}).get("registered")]
    if registered:
        lines.append(trf("клиенты: {names}", names=", ".join(registered)))
    return terminal("seohead mcp status", lines)


def client_row(client, status, on_connect=None):
    cid, name, default_path = client
    clients = (status or {}).get("clients")
    info = clients.get(cid) if isinstance(clients, dict) else None
    path = tilde(info.get("file")) if info and info.get("file") else default_path
    live = on_connect is not None
    if info is None or info.get("registered") is None:
        state = badge("mut", "нет данных", "help")
        button = action_button("Прописать…", role="tonal", size="pill", enabled=False, tooltip=None if live else UNAVAILABLE)
    elif info["registered"]:
        state = badge("ok", "прописано", "check_circle")
        button = action_button("Изменить…", role="text", size="pill", enabled=live)
    else:
        state = badge("mut", "не прописано", "remove_circle")
        button = action_button("Прописать…", role="tonal", size="pill", enabled=live)
    if live:
        button.clicked.connect(lambda: on_connect(cid))
    return list_item("terminal", name, "", state, button, sub_mono=path)


def _when(created_at):
    try:
        moment = datetime.fromisoformat(created_at).astimezone()
    except (TypeError, ValueError):
        return ""
    day = tr("сегодня") if moment.date() == datetime.now().astimezone().date() else moment.strftime("%d.%m")
    return f"{day} {moment:%H:%M}"


def backup_rows(backups, on_restore=None):
    """Rows of ``seohead mcp backups --json``; the core restores the newest verified backup of a client only."""
    if not isinstance(backups, list):
        return [no_data()]
    if not backups:
        return [no_data("Бэкапов ещё нет")]
    rows, newest = [], set()
    for item in backups[:20]:
        if not isinstance(item, dict):
            continue
        client = item.get("client")
        restorable = on_restore is not None and item.get("verified") is True and client not in newest
        newest.add(client)
        button = action_button("Восстановить", role="text", size="pill", enabled=restorable,
                               tooltip=None if restorable else UNAVAILABLE if on_restore is None else "Ядро восстанавливает последний проверенный бэкап клиента")
        if restorable:
            button.clicked.connect(lambda _checked=False, c=client: on_restore(c))
        sub = joined(" · ", [_when(item.get("created_at")), REASONS.get(item.get("reason"), item.get("reason") or ""), NAMES.get(client, client or "")])
        rows.append(list_item("history", os.path.basename(str(item.get("path") or "")).split(".seohead-")[0], sub, button, title_mono=True))
    return rows or [no_data()]


def sections(store, status, backups, handlers=None):
    handlers = handlers or {}
    left = [
        server_row(store, status, handlers.get("toggle")),
        profile_row(store, status, handlers.get("profile")),
        journal_row(store),
        console(status),
        hint(trf("Одно состояние для приложения и CLI: {command} выключит переключатель здесь.", command=mono_html("seohead mcp disable")), rich=True),
        Note("info", "Что это меняет.", "Что видят агенты. Выключение отключает инструменты у всех клиентов сразу; конфиги клиентов не трогаются."),
    ]
    right = [
        group_label("Клиенты"),
        *(client_row(client, status, handlers.get("connect")) for client in CLIENTS),
        group_label("Бэкапы конфигов"),
        *backup_rows(backups, handlers.get("restore")),
        hint(trf("Перед каждой пропиской ядро кладёт копию рядом с конфигом: {pattern} · список — {command}",
                 pattern=mono_html("<config>.seohead-<time>-<hash>.bak"), command=mono_html("seohead mcp backups")), rich=True),
    ]
    return page(Columns(left, right))


def build_page(store, context):
    if context.core_executable:
        from ...mcp_integration import IntegrationPanel

        return IntegrationPanel(context.core_executable, store)
    return sections(store, _status(context), context.request("mcp_backups"),
                    {"toggle": lambda enabled: context.request("mcp_set_enabled", enabled=enabled)})
