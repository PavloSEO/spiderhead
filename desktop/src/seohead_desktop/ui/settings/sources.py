"""Settings → Источники данных (sheets SetSources, SetSourceDetail, SetSourcesAuth, SetSourceKey, SetSpend).

Providers, readiness and spend come from the core (``provider-registry``, ``provider-readiness``, ``spend-report``, local, no network).
«Проверить все» runs ``sources-doctor`` (also local: it reports where credentials are found, it does not call the provider), so
«ключ задан» is never shown as «подключено». The app never reads or prints a key; see ``source_details`` for the controls that wait for the core.
"""

from __future__ import annotations

from datetime import datetime

from PyQt5.QtWidgets import QHBoxLayout, QVBoxLayout, QWidget

from ... import i18n
from ...i18n import tr, trf
from ..kit import Kpi, waiting_badge
from .helpers import group_label, page
from .listing import Columns, action_button, hint, no_data, terminal
from .source_catalogue import credential_line, describe, grouped, paid_kind
from .source_details import detail_view, howto_view, spend_view
from .source_layout import SourceRow

ID, ICON, TITLE = "sources", "dns", "Источники данных"
HINT = "Ключи и доступы ядра seohead · значения ключей приложение не показывает"
SCHEMA = ()
LOADING = "Читаю состояние источников у ядра…"
OTHER_GAP = 934


class SourcesPage(QWidget):
    def __init__(self, context):
        super().__init__()
        self._context = context
        self.registry, self.readiness, self.spend = {}, {}, None
        self.status, self.auth, self.verified, self.checked_at, self.doctor_lines = None, None, {}, None, None
        self.range = "month"
        self.view = ("list",)
        self.loaded = self._busy = False
        self._layout = QVBoxLayout(self)
        self._layout.setContentsMargins(0, 0, 0, 0)
        self._body = None
        if context.can("sources"):
            self._show(hint(LOADING))
            self._ask("snapshot", self._snapshot)
        else:
            self._show(no_data())

    # data
    def _ask(self, operation, callback, on_error=None, **kwargs):
        self._context.request("sources", operation=operation, callback=callback, on_error=on_error or self._failed, owner=self,
                              project=self._context.project_directory, **kwargs)

    def _snapshot(self, result):
        self.loaded = True
        self.registry, self.readiness = result.get("registry") or {}, result.get("readiness") or {}
        self.spend, self.errors = result.get("spend"), result.get("errors") or []
        if not self.registry and not self.readiness:
            self._show(hint("Ядро не вернуло список источников. Проверьте его командой seohead provider-readiness."))
            return
        if self._context.project_directory:
            self._ask("status", self._got_status, on_error=lambda _reason: None)
        self.render()

    def _got_status(self, result):
        self.status = result.get("sources")
        if self.view[0] == "detail":
            self.render()

    def _failed(self, reason):
        self._show(hint(trf("Ядро не вернуло состояние источников: {reason}", reason=reason)))

    def provider_ids(self):
        ids = list(dict.fromkeys([*self.registry, *self.readiness]))
        return [pid for _title, members in grouped(ids) for pid in members]

    # actions
    def check_all(self):
        self._busy = True
        self.render()
        self._ask("doctor", self._doctor_done, on_error=self._doctor_failed)

    def _doctor_done(self, result):
        self._busy = False
        self.readiness = result.get("providers") or self.readiness
        self.checked_at = datetime.now().strftime("%H:%M")
        self.render()

    def _doctor_failed(self, reason):
        self._busy = False
        self.doctor_lines = [reason]
        self.render()

    def start_verify(self, pid):
        self.verified[pid] = "running"
        self.render()
        self._ask("verify", lambda r: self._verified(pid, r), on_error=lambda text: self._verified(pid, {"error": text}), provider=pid)

    def _verified(self, pid, result):
        self.verified[pid] = result
        if result.get("verified") and pid in self.readiness:
            self.readiness[pid]["readiness_state"], self.readiness[pid]["verified"] = "verified", True
        self.render()

    def set_range(self, value):
        self.range = value
        self._ask("spend", self._got_spend, since=datetime.now().strftime("%Y-%m-01") if value == "month" else None)

    def _got_spend(self, result):
        self.spend = result.get("spend")
        self.render()

    # views
    def show_list(self):
        self._open(("list",))

    def show_detail(self, pid):
        if pid == "gsc" and self.auth is None:
            self._ask("auth-status", self._got_auth, on_error=lambda _r: None, provider="gsc")
        self._open(("detail", pid))

    def _got_auth(self, result):
        self.auth = result
        if self.view == ("detail", "gsc"):
            self.render()

    def _open(self, view):
        self.view = view
        self.render()

    def render(self):
        kind = self.view[0]
        body = {"list": self._list, "detail": lambda: detail_view(self, self.view[-1]), "howto": lambda: howto_view(self), "spend": lambda: spend_view(self)}[kind]()
        self._show(body)

    def _show(self, widget):
        if self._body is not None:
            self._layout.removeWidget(self._body)
            self._body.deleteLater()
        self._body = widget
        i18n.retranslate(widget)
        self._layout.addWidget(widget)

    def _list(self):
        ids = self.provider_ids()
        described = {pid: describe(self.readiness.get(pid, {})) for pid in ids}

        def count(*states):
            return sum(1 for state, *_rest in described.values() if state in states)

        head = QWidget()
        head_layout = QHBoxLayout(head)
        head_layout.setContentsMargins(0, 0, 0, 0)
        head_layout.addStretch(1)
        guide = action_button("Как подключать", icon="menu_book", role="text")
        guide.clicked.connect(lambda: self._open(("howto",)))
        spend = action_button("Расходы", icon="payments")
        spend.clicked.connect(lambda: self._open(("spend",)))
        check = action_button("Проверить все", icon="stethoscope", role="primary", enabled=not self._busy)
        check.clicked.connect(self.check_all)
        for button in (guide, spend, check):
            head_layout.addWidget(button)
        kpis = QWidget()
        kpi_layout = QHBoxLayout(kpis)
        kpi_layout.setContentsMargins(0, 8, 0, 8)
        for kpi in (Kpi(trf("подключено и проверено из {total}", total=len(ids)), count("verified")), Kpi("ключ задан, не проверен", count("configured_unverified")),
                    Kpi("истёк токен / ошибка", count("expired", "revoked", "invalid", "failed")), Kpi("нужен ключ", count("missing", "not_configured"))):
            kpi.caption.setWordWrap(True)
            kpi.setMinimumWidth(0)
            kpi_layout.addWidget(kpi)
        left, right = [], []
        for index, (title, members) in enumerate(grouped(ids)):
            column = left if index % 2 == 0 else right
            column.append(group_label(title))
            for pid in members:
                _state, kind, text, icon = described[pid]
                sub = credential_line(self.registry.get(pid), self.readiness.get(pid), refs=False)
                if self.checked_at:
                    sub = (sub + " · " if sub else "") + trf("проверено {time}", time=self.checked_at)
                row = SourceRow(pid, paid_kind(self.registry.get(pid)), kind, text, icon, sub)
                row.opened.connect(self.show_detail)
                column.append(row)
        right.append(group_label("Другие источники"))
        right.append(_waiting_row())
        lines = self._doctor_text(described)
        return page(head, kpis, Columns(left, right, breakpoint=1120),
                    terminal("seohead sources-doctor", lines),
                    hint("Состояние берётся из локальной настройки ядра. Проверка доступа у самого провайдера выполняется отдельно, в карточке источника."))

    def _doctor_text(self, described):
        if self.doctor_lines:
            return self.doctor_lines
        if not self.checked_at:
            return [tr("# «Проверить все»: где ядро нашло ключи · сеть не используется, доступ у провайдера не проверяется")]
        names = [pid for pid, (state, *_rest) in described.items() if state in {"missing", "not_configured"}]
        found = sum(1 for state, *_rest in described.values() if state in {"configured_unverified", "verified", "not_required"})
        lines = [trf("✓ {n} источников настроено", n=found)]
        if names:
            lines.append(trf("! нужен ключ: {names}", names=", ".join(names)))
        return lines


def _waiting_row():
    from PyQt5.QtWidgets import QLabel

    box = QWidget()
    layout = QHBoxLayout(box)
    layout.setContentsMargins(0, 8, 0, 8)
    layout.setSpacing(8)
    label = QLabel(tr("Topvisor, DataForSEO SERP, Screaming Frog"))
    label.setProperty("text_style", "meta")
    layout.addWidget(label, 1)
    layout.addWidget(waiting_badge(OTHER_GAP, "Реестр источников ядра: Topvisor, DataForSEO, Screaming Frog, дата и срок токена"))
    return box


def build_page(store, context):
    return SourcesPage(context)
