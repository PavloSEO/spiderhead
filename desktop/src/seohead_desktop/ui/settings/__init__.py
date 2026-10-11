"""Settings dialog (design v2 sheets Set*): 11 sections, one module each.

A section module exposes:
  ID, ICON, TITLE, HINT   - nav entry and header
  SCHEMA                  - tuple of settings_store.Setting owned by the section (keys prefixed "<ID>.")
  build_page(store, context) -> QWidget   - the section body (rows built from ui.controls)
Rows are ``SettingRow`` widgets so the dialog search can match their titles.
"""

from __future__ import annotations

from importlib import import_module

SECTION_IDS = ("general", "view", "scan", "core", "mcp", "sources", "agent", "notify", "keys", "data", "updates", "about")


def sections():
    return [import_module(f"{__name__}.{name}") for name in SECTION_IDS]


def full_schema():
    from ...settings_store import Setting

    shell = (Setting("shell.display", "agent", str, choices=("agent", "simple")), Setting("shell.onboarding_done", False, bool))
    return shell + tuple(setting for module in sections() for setting in module.SCHEMA)
