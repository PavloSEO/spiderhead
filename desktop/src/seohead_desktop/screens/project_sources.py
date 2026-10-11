"""Project ↔ resources connections (sheet ProjSources): what the core really knows, as plain data.

The core reports the readiness of every provider (``provider-readiness``: credential present or missing, never a
network check), but it cannot yet store or list which resource (site, property, container, counter, project) a
project is connected to. So a row carries the readiness of its service and nothing about a chosen resource: the
resource, the sync time and the link actions stay honestly unavailable (core gap #990). Nothing is written.
"""

from __future__ import annotations

from dataclasses import dataclass

from ..i18n import joined, trf

GAP = 990  # project <-> resource connection storage is missing in the core (number lives in code only)
GAP_HINT = "Выбор ресурса проекта у сервиса, время синхронизации и сохранение связей в проекте"
# Presentation order of the sheet: key, service name, readiness provider ids, what the project would be linked to
SERVICES = (
    ("gsc", "Search Console", ("gsc",), "сайт Search Console"),
    ("ga4", "Google Analytics 4", ("ga4",), "ресурс GA4"),
    ("gtm", "Google Tag Manager", (), "контейнер GTM"),
    ("metrika", "Яндекс Метрика", ("metrika",), "счётчик Метрики"),
    ("webmaster", "Яндекс Вебмастер", ("yandex_webmaster",), "сайт Вебмастера"),
    ("bing", "Bing Webmaster", ("bing_webmaster",), "сайт Bing"),
    ("topvisor", "Topvisor", (), "проект Topvisor"),
    ("psi", "PageSpeed / CrUX", ("pagespeed", "crux"), "адрес проекта"),
)
CREDENTIALS = {
    "api_key": "API-ключ", "api_token": "API-токен", "oauth_bearer": "OAuth", "durable_oauth": "OAuth",
    "service_account": "сервисный аккаунт",
}
VERIFIED = {"verified", "ready", "connected"}
# (badge kind, text, icon) per readiness state; ranking decides which provider speaks for a combined service
STATES = {
    "verified": ("ok", "подключено", "check_circle"),
    "not_required": ("ok", "без ключа", "public"),
    "configured_unverified": ("info", "ключ задан · не проверен", "key"),
    "missing": ("warn", "нужен ключ", "key"),
    "invalid": ("err", "ключ не читается", "error"),
}
RANK = {"invalid": 0, "missing": 1, "configured_unverified": 2, "not_required": 3, "verified": 4}
NAMES = {"pagespeed": "PageSpeed", "crux": "CrUX"}
UNKNOWN = ("mut", "Нет данных от ядра", "help")


@dataclass(frozen=True)
class SourceRow:
    key: str
    service: str
    resource_kind: str
    state: str | None    # readiness state, None when the core does not report this service
    kind: str            # badge kind: ok | info | warn | err | mut
    text: str
    icon: str
    access: str          # sub line: credential kinds the core knows
    providers: tuple     # readiness provider ids behind the row

    @property
    def has_access_settings(self):
        return bool(self.providers)

    @property
    def ready(self):
        return self.state in {"verified", "not_required", "configured_unverified"}


def readiness_state(entry):
    state = str((entry or {}).get("readiness_state") or (entry or {}).get("state") or "")
    if state in VERIFIED or (entry or {}).get("verified") is True:
        return "verified"
    return state if state in STATES else None


def credential_kinds(entries):
    kinds = []
    for entry in entries:
        for component, present in (entry.get("credential_components") or {}).items():
            label = CREDENTIALS.get(component)
            if label and label not in kinds:
                kinds.append(label)
    return kinds


def build_rows(providers):
    """SourceRow per sheet service from the ``providers`` mapping of provider-readiness (None/empty: nothing known)."""
    providers = providers if isinstance(providers, dict) else {}
    rows = []
    for key, service, ids, resource_kind in SERVICES:
        entries = [providers[pid] for pid in ids if isinstance(providers.get(pid), dict)]
        states = [readiness_state(entry) for entry in entries]
        known = [state for state in states if state]
        if not known or len(entries) != len(ids) or len(known) != len(entries):
            rows.append(SourceRow(key, service, resource_kind, None, *UNKNOWN, "", tuple(ids)))
            continue
        state = min(known, key=RANK.get)
        kind, text, icon = STATES[state]
        kinds = credential_kinds(entries)
        if len(ids) > 1:  # a combined service: say which of its parts has the access
            access = joined(" · ", [trf("{name}: {value}", name=NAMES.get(pid, pid),
                                        value=joined(" / ", credential_kinds([entry])) if readiness_state(entry) != "missing" else "не задан")
                                    for pid, entry in zip(ids, entries)])
        elif state == "missing":
            access = trf("{kinds} не задан", kinds=joined(" / ", kinds)) if kinds else "доступ не задан"
        else:
            access = joined(" / ", kinds) if kinds else "публичный сервис"
        rows.append(SourceRow(key, service, resource_kind, state, kind, text, icon, access, tuple(ids)))
    return rows


def access_count(rows):
    """How many services have access the core can use (key present or not needed) - never «connected»."""
    return sum(1 for row in rows if row.ready)
