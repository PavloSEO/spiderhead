"""What the Settings → Источники данных screens know about a provider: names, groups, logos, how to connect.

The provider list itself comes from the core (provider-registry / provider-readiness); this module only adds
presentation facts of the sheets (SetSources, SetSourcesAuth). A provider the core does not report is not shown.
"""

from __future__ import annotations

from ...i18n import joined, trf

# provider id -> (name, logo key of ui.brand_logos or None, monogram for a service without a shipped mark)
NAMES = {
    "gsc": ("Google Search Console", "gsc", "G"),
    "ga4": ("Google Analytics 4", "ga4", "GA"),
    "pagespeed": ("PageSpeed Insights", "psi", "PS"),
    "crux": ("CrUX", None, "CX"),
    "bing_webmaster": ("Bing Webmaster", "bing", "B"),
    "indexnow": ("IndexNow", None, "IN"),
    "yandex_webmaster": ("Яндекс Вебмастер", "webmaster", "Я"),
    "metrika": ("Яндекс Метрика", "metrika", "М"),
    "yandex_cloud": ("Яндекс Cloud / Wordstat", "yandex_cloud", "Я"),
    "arsenkin": ("Arsenkin", "arsenkin", "A"),
    "dataforseo_backlinks": ("DataForSEO · ссылки", "dataforseo", "D"),
    "miratext": ("Miratext", None, "MT"),
    "wayback": ("Wayback", None, "WB"),
    "crtsh": ("crt.sh", None, "CT"),
}
GROUPS = (
    ("Google", ("gsc", "ga4", "pagespeed", "crux")),
    ("Другие поисковики", ("bing_webmaster", "indexnow")),
    ("Яндекс", ("yandex_webmaster", "metrika", "yandex_cloud")),
    ("Позиции и ключи", ("arsenkin", "dataforseo_backlinks", "miratext")),
    ("Ссылки", ("wayback", "crtsh")),
)
OTHER = "Прочие"
# spend-report names a source differently from the registry for these providers
SPEND_KEY = {"dataforseo_backlinks": "dataforseo"}
SYNC_SOURCE = {"gsc": "gsc", "ga4": "ga4", "metrika": "metrika", "yandex_webmaster": "webmaster"}

COMPONENTS = {
    "api_key": "API-ключ", "api_token": "API-токен", "oauth_bearer": "OAuth-токен", "durable_oauth": "OAuth (вход через браузер)",
    "service_account": "Сервисный аккаунт", "login": "Логин", "password": "API-пароль", "submission_key": "Ключ отправки",
    "folder_id": "ID каталога",
}
METHOD_LABELS = {"oauth_bearer": "OAuth", "durable_oauth": "OAuth", "service_account": "сервисный аккаунт", "api_key": "API-ключ", "api_token": "API-токен",
                 "login": "логин + пароль", "submission_key": "ключ отправки"}

# state -> (badge kind, badge text, icon); the text always says what the core reported, never more
STATES = {
    "verified": ("ok", "подключено", "check_circle"),
    "not_required": ("ok", "без ключа", "public"),
    "configured_unverified": ("info", "ключ задан · не проверен", "key"),
    "missing": ("warn", "нужен ключ", "key"),
    "not_configured": ("warn", "нужен ключ", "key"),
    "expired": ("warn", "истёк токен", "schedule"),
    "revoked": ("err", "доступ отозван", "block"),
    "invalid": ("err", "ошибка проверки", "error"),
    "failed": ("err", "ошибка проверки", "error"),
    "waiting": ("info", "ждём ответа", "hourglass_top"),
}
UNKNOWN = ("mut", "Нет данных от ядра", "help")
VERIFIED = {"verified", "ready", "connected"}
PROBLEM = {"expired", "revoked", "invalid", "failed"}

# provider -> (token lifetime, where to take it, what it shows after connecting) as printed on the sheet SetSourcesAuth
HOWTO = {
    "gsc": ("токен сервисного аккаунта на 1 ч, приложение обновляет сам; OAuth в статусе «Testing» — 7 дней",
            "Google Cloud → IAM → Сервисные аккаунты → Ключи (JSON); e-mail аккаунта добавить пользователем в каждый ресурс",
            "свойства Search Console"),
    "ga4": ("как у Search Console", "Google Cloud → IAM → Сервисные аккаунты; e-mail добавить в «Управление доступом» ресурса GA4",
            "ресурсы и потоки GA4"),
    "pagespeed": ("до отзыва", "Google Cloud → API и сервисы → Учётные данные → API-ключ", "публичные данные PageSpeed"),
    "crux": ("до отзыва", "Google Cloud → API и сервисы → Учётные данные → API-ключ", "публичные данные CrUX"),
    "yandex_webmaster": ("~1 год, дату истечения покажем после появления в ядре", "oauth.yandex.ru → приложение → разрешить доступ", "сайты Вебмастера"),
    "metrika": ("~1 год, дату истечения покажем после появления в ядре", "oauth.yandex.ru → приложение → разрешить доступ", "счётчики Метрики"),
    "yandex_cloud": ("ключ — до отзыва", "консоль Yandex Cloud → Сервисные аккаунты → ключи", "каталог Cloud, квота Wordstat"),
    "bing_webmaster": ("ключ — до отзыва", "Bing Webmaster → Настройки → API access", "сайты аккаунта Bing"),
    "arsenkin": ("до отзыва", "кабинет Arsenkin → API", "баланс; проекты не связываются"),
    "dataforseo_backlinks": ("до смены пароля; sandbox для проверки бесплатный", "app.dataforseo.com → API Access", "баланс и лимиты, ссылки домена"),
    "miratext": ("ключ — до отзыва", "кабинет Miratext → API", "SEO-анализ текста"),
    "indexnow": ("ключ — до смены", "файл ключа на сайте проекта", "отправка адресов в поисковики"),
    "wayback": ("—", "—", "публичные архивы"),
    "crtsh": ("—", "—", "публичные сертификаты"),
}


def name_of(pid):
    return NAMES.get(pid, (pid, None, pid[:2].upper()))[0]


def state_of(entry):
    """Readiness state of one provider entry, ``None`` when the core did not say."""
    state = str((entry or {}).get("readiness_state") or (entry or {}).get("state") or "")
    if state in VERIFIED or (entry or {}).get("verified") is True:
        return "verified"
    return state if state in STATES else None


def describe(entry):
    """(state, badge kind, badge text, icon)."""
    state = state_of(entry)
    return (state, *(STATES[state] if state else UNKNOWN))


def paid_kind(registry_entry):
    access = str((registry_entry or {}).get("access") or "")
    return "paid" if access.endswith("_paid") and "optional" not in access else "optional" if "optional_paid" in access else ""


def credential_line(registry_entry, entry, refs=True):
    """«сервисный аккаунт / OAuth-токен · хранится: config:…» from component names and reference paths only."""
    names = (registry_entry or {}).get("credential_components") or list((entry or {}).get("credential_components") or {})
    kinds = []
    for component in names:
        label = METHOD_LABELS.get(component)
        if label and label not in kinds:
            kinds.append(label)
    parts = [joined(" / ", kinds)] if kinds else []
    found = [s.get("source_reference") for s in ((entry or {}).get("credential_sources") or {}).values() if s.get("source_reference")]
    if refs and found:
        parts.append(trf("хранится: {refs}", refs=", ".join(dict.fromkeys(found))))
    return joined(" · ", parts)


def grouped(ids):
    """[(group title, [provider id])] in sheet order; ids the sheet does not know go to «Прочие»."""
    placed, result = set(), []
    for title, members in GROUPS:
        found = [pid for pid in members if pid in ids]
        placed.update(found)
        if found:
            result.append((title, found))
    rest = [pid for pid in ids if pid not in placed]
    if rest:
        result.append((OTHER, rest))
    return result
