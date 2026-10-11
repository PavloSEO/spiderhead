"""Detail, «Как подключать» and spend views of Settings → Источники данных (sheets SetSourceDetail, SetSourcesAuth, SetSourceKey, SetSpend).

Everything is read from the core's answers kept on the page (``page.registry`` / ``readiness`` / ``spend`` / ``status``). The app never
shows or stores a key: it says whether a credential is set and where the core reads it from. Writing a key and the
browser OAuth need core contracts that do not exist yet, so those controls are a neutral «Недоступно в этой версии ядра».
"""

from __future__ import annotations

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QHBoxLayout, QLabel, QVBoxLayout, QWidget

from ...i18n import joined, tr, trf
from ...source_service import VERIFY_PROVIDERS
from ..controls import Note
from ..kit import waiting_badge
from .helpers import group_label, two_columns
from .helpers import page as stack
from .listing import action_button, badge, hint, key_values, list_item, no_data, terminal
from .source_catalogue import (
    COMPONENTS,
    HOWTO,
    METHOD_LABELS,
    SPEND_KEY,
    SYNC_SOURCE,
    credential_line,
    describe,
    name_of,
    paid_kind,
)
from .source_layout import back_header, service_tile, table

ACCESS = {"read_only": "только чтение", "read_only_paid": "только чтение · платный", "read_only_optional_paid": "только чтение · платный по желанию",
          "confirmed_write": "запись только после подтверждения"}
PRIVACY = {"aggregate": "агрегированные данные", "restricted": "данные сайта с ограниченным доступом"}
UNITS = {"requests": "запросов", "usd": "USD", "limits": "лимитов"}
OAUTH_GAP, KEY_GAP, CHECK_GAP, SPEND_GAP, SYNC_GAP = 964, 965, 934, 949, 990


def amount(units):
    """«169 857 лимитов» / «1 990 запросов»: the core's own units, never converted into roubles."""
    return joined(" · ", [f"{value:,.0f}".replace(",", " ") + " " + tr(UNITS.get(unit, unit)) if float(value).is_integer()
                          else f"{value:,.2f}".replace(",", " ") + " " + tr(UNITS.get(unit, unit)) for unit, value in units.items()])


def row_widget(*widgets, stacked=False):
    """Widgets in one line; ``stacked`` puts them under each other so a narrow window never scrolls sideways."""
    box = QWidget()
    layout = (QVBoxLayout if stacked else QHBoxLayout)(box)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(8)
    for widget in widgets:
        layout.addWidget(widget, 0, Qt.AlignLeft if stacked else Qt.Alignment())
    return box


def credential_rows(pid, registry, entry):
    rows = []
    components = list(registry.get("credential_components") or []) or list(entry.get("credential_components") or {})
    sources = entry.get("credential_sources") or {}
    for component in components:
        present = (entry.get("credential_components") or {}).get(component)
        source = sources.get(component) or {}
        reference = source.get("source_reference")
        accepted = source.get("accepted_source_references") or []
        if present is None:
            kind, text, sub = "mut", "нет данных", ""
        elif present:
            kind, text, sub = "ok", "задан", trf("хранится: {ref}", ref=reference) if reference else ""
        else:
            kind, text = "warn", "не задан"
            sub = trf("ядро читает из: {refs}", refs=", ".join(accepted)) if accepted else ""
        rows.append(list_item("key", COMPONENTS.get(component, component), sub, badge(kind, text, "check_circle" if present else "key" if present is False else "help"),
                              sub_mono=""))
    if not components:
        rows.append(hint("Публичный сервис: ключ не нужен."))
    return rows


def verify_text(page, pid):
    result = page.verified.get(pid)
    if result == "running":
        return "проверяем…"
    if result is None:
        return "не запускалась · по кнопке «Проверить доступ»"
    if result.get("error"):
        return result["error"]
    return "доступ подтверждён" if result.get("verified") else trf("не подтверждён · {state}", state=result.get("state") or "нет данных")


def spend_block(page, pid):
    key = SPEND_KEY.get(pid, pid)
    spend = page.spend or {}
    by_source = spend.get("by_source") or {}
    reg = page.registry.get(pid, {})
    if not paid_kind(reg) and key not in by_source:
        return []
    since = spend.get("since")
    rows = [("Использовано" if not since else trf("Использовано с {date}", date=since), amount(by_source[key]) if key in by_source else None),
            ("Лимит в месяц", row_widget(waiting_badge(SPEND_GAP, "Месячные лимиты и подтверждение трат платных API")))]
    operations = [(name.split(".", 1)[1], amount(units)) for name, units in (spend.get("by_operation") or {}).items() if name.startswith(key + ".")]
    operations.sort(key=lambda r: -float(next(iter(spend["by_operation"][key + "." + r[0]].values()))))
    out = [group_label("Расход"), key_values(rows)]
    if operations:
        out.append(table(("Операция", "Расход"), operations, "Расход по операциям", 5))
    return out


def sync_block(page, pid):
    out = [group_label("Ресурсы и синхронизация"),
           key_values([("Найденные ресурсы", row_widget(waiting_badge(SYNC_GAP, "Список ресурсов, доступных аккаунту, и их привязка к проектам")))])]
    source = SYNC_SOURCE.get(pid)
    if source:
        if page.status is None:
            out.append(hint("Откройте проект с сохранёнными данными источника: покажем, за какие дни они уже выгружены."))
        else:
            rows = [(e["resource"], e.get("last_date"), str(e.get("rows") or 0), e.get("last_fetched_at")) for e in page.status if e.get("source") == source]
            out.append(table(("Ресурс", "Данные до", "Строк", "Последняя выгрузка"), rows, "Сохранённые данные источника") if rows
                       else hint("В открытом проекте нет сохранённых данных этого источника."))
    return out


def detail_view(page, pid):
    reg, entry = page.registry.get(pid, {}), page.readiness.get(pid, {})
    state, kind, text, icon = describe(entry)
    verify = action_button("Проверить доступ", icon="stethoscope", role="primary", enabled=pid in VERIFY_PROVIDERS and page.verified.get(pid) != "running",
                           tooltip="Бесплатный пробный запрос к провайдеру" if pid in VERIFY_PROVIDERS else "Проверка доступа этого сервиса ядро пока не умеет")
    verify.clicked.connect(lambda: page.start_verify(pid))
    sub = joined(" · ", [tr(ACCESS.get(reg.get("access"), "")) if reg.get("access") else "", credential_line(reg, entry, refs=False)])
    status = [("Состояние", row_widget(badge(kind, text, icon))),
              ("Проверка доступа", verify_text(page, pid) if pid in VERIFY_PROVIDERS else row_widget(waiting_badge(CHECK_GAP, "Проверка доступа и срок токена для всех сервисов"))),
              ("Доступ", ACCESS.get(reg.get("access"))), ("Квота", reg.get("quota_mode") or None),
              ("Данные", PRIVACY.get(reg.get("privacy_class"), reg.get("privacy_class") or None))]
    howto = HOWTO.get(pid)
    parts = [back_header(name_of(pid), "Все источники", page.show_list, sub, pid=pid, actions=[verify], paid=paid_kind(reg)),
             key_values(status), group_label("Способы доступа"), *credential_rows(pid, reg, entry)]
    entry_key = row_widget(action_button("Ввести ключ…", icon="key", enabled=False, tooltip="Недоступно в этой версии ядра"),
                           waiting_badge(KEY_GAP, "Сохранение ключа источника из приложения"), stacked=True)
    methods = [METHOD_LABELS.get(c) for c in reg.get("credential_components") or []]
    if reg.get("credential_components"):
        parts.append(key_values([("Ввод из приложения", entry_key)]))
    if "OAuth" in methods and pid == "gsc":
        auth = page.auth
        parts.append(key_values([
            ("Аккаунт Google", row_widget(action_button("Подключить через браузер", icon="open_in_browser", enabled=False, tooltip="Недоступно в этой версии ядра"),
                                          waiting_badge(OAUTH_GAP, "Вход через браузер, обновление и отзыв токена из приложения"), stacked=True)),
            ("Токен обновления", None if auth is None else "задан · не проверен" if auth.get("configured") else "не задан")]))
    if howto and howto[1] != "—":
        parts += [group_label("Как подключить"), key_values([("Где взять", howto[1]), ("Срок токена", howto[0]), ("Что видно после подключения", howto[2])])]
    parts += [*spend_block(page, pid), *sync_block(page, pid)]
    if reg.get("operations"):
        parts += [group_label("Что умеет ядро"), hint(", ".join(reg["operations"]))]
    parts.append(Note("info", "Ключ вводите только вы.", "Агент видит лишь «задан / не задан» и не может его прочитать или заменить. Значение приложение не показывает."))
    parts.append(terminal("seohead provider-readiness", [trf("{pid}: {state} · доступ у провайдера не проверялся", pid=pid, state=state or "нет данных")]))
    return stack(*parts)


def howto_view(page):
    parts = [back_header("Как подключать источники", "Все источники", page.show_list,
                         "Способы доступа по провайдерам · всё только чтение · экран показывает только способы, которые есть у провайдера")]
    for pid in page.provider_ids():
        reg = page.registry.get(pid, {})
        info = HOWTO.get(pid)
        methods = joined(" / ", dict.fromkeys(METHOD_LABELS[c] for c in reg.get("credential_components", []) if c in METHOD_LABELS)) or "без ключа"
        sub = [trf("Способы: {methods}", methods=methods)]
        if info and info[1] != "—":
            sub.append(trf("Где взять: {where}", where=info[1]))
            sub.append(trf("Срок: {term}", term=info[0]))
        row = list_item("lock", name_of(pid), joined(" · ", sub), badge("mut", "чтение", "lock"))
        row.layout().insertWidget(0, service_tile(pid, 32))
        parts.append(row)
    parts += [group_label("Сервисный аккаунт или API-ключ"),
              hint("1. Получить файл или ключ у провайдера. 2. Положить по пути, который читает ядро (он указан в карточке источника), либо задать переменную окружения. "
                   "3. В карточке источника нажать «Проверить доступ», если для него есть проверка. Приложение значение ключа не читает и не показывает."),
              group_label("OAuth через браузер"),
              row_widget(waiting_badge(OAUTH_GAP, "Вход через браузер, обновление и отзыв токена из приложения"), QLabel(tr("Вход через браузер появится после контракта ядра"))),
              terminal("seohead provider-registry", ["# тот же список из терминала"])]
    return stack(*parts)


def spend_view(page):
    spend = page.spend
    since = (spend or {}).get("since")
    head = back_header("Расходы платных API", "Все источники", page.show_list,
                       trf("С {date} · журнал ядра · единицы как записал провайдер", date=since or "начала журнала"),
                       actions=(action_button("Экспорт CSV", icon="download", enabled=False),))
    if spend is None:
        return stack(head, no_data(), terminal("seohead spend-report", ["Нет данных"]))
    by_source = [(name_of(next((p for p, k in SPEND_KEY.items() if k == source), source)), amount(units)) for source, units in spend.get("by_source", {}).items()]
    by_operation = sorted(spend.get("by_operation", {}).items(), key=lambda kv: -float(next(iter(kv[1].values()))))
    by_day = sorted(spend.get("by_day", {}).items(), reverse=True)
    waiting = lambda: row_widget(waiting_badge(SPEND_GAP, "Месячные лимиты и подтверждение трат платных API"))
    left = [group_label("Журнал"),
            key_values([("вызовов в журнале", spend.get("calls")), ("с неизвестной стоимостью", spend.get("uncertain_count"))]),
            group_label("По провайдерам"), key_values(by_source) if by_source else no_data(),
            group_label("Лимиты и подтверждения"), key_values([("Месячные лимиты", waiting()), ("Ждут подтверждения", waiting())]),
            Note("info", "Деньги не пересчитываются.", "Журнал ядра хранит запросы, USD и лимиты провайдера. Рубли и месячные лимиты появятся вместе с контрактом ядра.")]
    right = [group_label("Последние дни"), table(("День", "Расход"), [(d, amount(u)) for d, u in by_day], "Расход по дням", 7) if by_day else no_data(),
             group_label("По операциям"), table(("Операция", "Расход"), [(n, amount(u)) for n, u in by_operation], "Расход по операциям", 10) if by_operation else no_data()]
    if len(by_operation) > 10:
        right.append(hint(trf("Показаны 10 из {n} операций", n=len(by_operation))))
    return stack(head, two_columns(left, right),
                 terminal("seohead spend-report", [trf("вызовов: {n}", n=spend.get("calls") if spend.get("calls") is not None else "нет данных")]))
