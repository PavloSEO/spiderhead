"""Settings → Источники данных and the profile menu for capture_screens.py: saved real core answers, no CLI, no network.

States: list | doctor | key (nothing configured) | failed | partial | loading | noactions | gsc | arsenkin | pagespeed | wayback | howto | spend.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from seohead_desktop.settings_store import AppSettings
from seohead_desktop.ui.settings import full_schema
from seohead_desktop.ui.settings.context import SettingsContext
from seohead_desktop.ui.settings.dialog import SettingsDialog
from tests._screens_core import fixture
from tests._sources_support import PROJECT, sources_hook

DETAILS = {"gsc", "arsenkin", "pagespeed", "wayback", "metrika"}


def dialog(state, store=None):
    overrides, pending = {}, None
    if state == "key":
        data = fixture("provider_readiness.json")
        for entry in data["providers"].values():
            if entry["readiness_state"] != "not_required":
                entry["readiness_state"] = "missing"
                entry["credential_components"] = {k: False for k in entry["credential_components"]}
                for source in entry["credential_sources"].values():
                    source["source_reference"] = None
        overrides["provider-readiness"] = data
    elif state == "failed":
        overrides["provider-readiness"] = ValueError("x")
        overrides["provider-registry"] = ValueError("x")
    elif state == "partial":
        overrides["provider-readiness"] = {"ok": True, "providers": {}}
        overrides["provider-registry"] = {"ok": True, "providers": {}}
    elif state == "loading":
        pending = []
    hook = sources_hook(overrides, pending)
    context = SettingsContext(project_directory=PROJECT, actions={} if state == "noactions" else {"sources": hook})
    dlg = SettingsDialog(store or AppSettings(schema=full_schema()), context, section="sources")
    page = dlg._pages["sources"][1]
    if state == "doctor":
        page.check_all()
    elif state in DETAILS:
        page.show_detail(state)
    elif state in ("howto", "spend"):
        page._open((state,))
    dlg.capture_max = (1000, 760)
    return dlg
