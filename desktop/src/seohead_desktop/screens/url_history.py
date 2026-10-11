"""UrlHistory (canvas UrlHistory): the selected URL across the saved scans of the project.

Every row is one ``scan-url-detail`` answer for the same URL, read one scan at a time through ``CoreJob`` (the core has
no cross-scan query for one URL). A scan without that URL is shown as «Не найден в скане», never as a zero. Incoming
links per scan are not in the core answer, so they stay «Недоступно в этой версии ядра».
"""

from __future__ import annotations

from PyQt5.QtCore import QPointF, Qt, pyqtSignal
from PyQt5.QtGui import QColor, QPainter, QPen
from PyQt5.QtWidgets import (
    QFrame,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QTableWidget,
    QTableWidgetItem,
    QVBoxLayout,
    QWidget,
)

from .. import theming
from ..i18n import joined, tr, trf
from ..ui.kit import StatePanel, style_table, unavailable_tip
from ..ui.presentation import short_run_id
from .scan_common import number
from .url_detail import clean, first_response, stamp_text
from .url_query import CoreJob, detail_arguments, reason_text, seconds_to_ms

MAX_SCANS = 12  # the selected scan and the most recent others; one core call per scan
COLUMNS = ("Скан", "Дата", "HTTP", "Title", "Слов", "Ответ, мс", "Изменения")


def clear_layout(layout):
    while layout.count():
        item = layout.takeAt(0)
        if item.widget() is not None:
            item.widget().deleteLater()


def entry_from(payload):
    """One scan's answer as the history needs it; None fields are «Нет данных», not zero."""
    if not isinstance(payload, dict) or payload.get("ok") is not True:
        return {
            "state": "error",
            "text": reason_text(payload)
            if isinstance(payload, dict)
            else tr("Ядро не выполнило запрос"),
        }
    page = payload.get("page")
    if payload.get("state") == "not_found" or not isinstance(page, dict):
        return {"state": "missing"}
    words = page.get("word_count")
    status = page.get("status_code")
    return {
        "state": "ok",
        "status": status if type(status) is int else None,
        "title": clean(page.get("title")),
        "words": words if type(words) is int else None,
        "ms": seconds_to_ms(page.get("response_time")),
        "received": first_response(payload).get("received_at"),
        "uuid": (payload.get("source") or {}).get("scan_uuid"),
    }


def changes(entry, previous):
    """Compare answers within the bounded selection, not the URL's entire history."""
    if entry["state"] == "missing":
        return tr("Не найден в скане")
    if entry["state"] == "error":
        return tr("Ядро не вернуло ответ")
    if previous is None:
        return tr("Первое появление в выборке")
    parts = []
    if entry["status"] != previous["status"]:
        parts.append(
            trf(
                "Ответ {now} (было {was})",
                now=entry["status"] or tr("Нет данных"),
                was=previous["status"] or tr("Нет данных"),
            )
        )
    if entry["title"] != previous["title"]:
        parts.append(tr("Title изменён"))
    if (
        entry["words"] is not None
        and previous["words"] is not None
        and entry["words"] != previous["words"]
    ):
        delta = entry["words"] - previous["words"]
        parts.append(trf("{d} слов", d=f"{delta:+,}".replace(",", " ")))
    return joined(" · ", parts) or tr("Без значимых изменений")


class Sparkline(QWidget):
    """One metric over the scans. A missing value breaks the line; it is never drawn as zero."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.values = []
        self.setFixedSize(56, 34)

    def set_values(self, values):
        self.values = list(values)
        self.update()

    def paintEvent(self, _event):
        known = [(i, v) for i, v in enumerate(self.values) if v is not None]
        if len(known) < 2:
            return
        low = min(v for _i, v in known)
        span = (max(v for _i, v in known) - low) or 1
        last = len(self.values) - 1 or 1
        points = {
            i: QPointF(
                3 + i * (self.width() - 6) / last, 4 + (self.height() - 8) * (1 - (v - low) / span)
            )
            for i, v in known
        }
        painter = QPainter(self)
        painter.setRenderHint(QPainter.Antialiasing)
        pen = QPen(QColor(theming.roles()["primary"]))
        pen.setWidth(2)
        painter.setPen(pen)
        for i in points:
            if i + 1 in points:
                painter.drawLine(points[i], points[i + 1])
        painter.end()


class Metric(QFrame):
    """Caption, value and an optional sparkline: the four cells of the strip above the table."""

    def __init__(self, caption, value_style="section", sparkline=True, parent=None):
        super().__init__(parent)
        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 0, 12, 0)
        layout.setSpacing(8)
        text = QVBoxLayout()
        text.setSpacing(0)
        self.caption = QLabel(caption)
        self.caption.setProperty("text_style", "meta")
        self.value = QLabel()
        self.value.setProperty("text_style", value_style)
        self.value.setWordWrap(True)
        self.sub = QLabel()
        self.sub.setProperty("text_style", "meta")
        for widget in (self.caption, self.value, self.sub):
            text.addWidget(widget)
        layout.addLayout(text, 1)
        self.spark = Sparkline()
        if sparkline:
            layout.addWidget(self.spark, 0, Qt.AlignBottom)


class HistoryPage(QWidget):
    counted = pyqtSignal(object)  # number of scans in the history (the tab counter)

    def __init__(self, ctx, parent=None):
        super().__init__(parent)
        self.ctx = ctx
        self.generation = 0
        self.loaded_for = None
        self.scans = []
        self.total_scans = 0
        self.entries = []
        self.job = CoreJob(ctx.host_ref, self)
        self.job.done.connect(self._done)
        self.job.failed.connect(self._failed)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(20, 14, 20, 14)
        layout.setSpacing(10)
        self.strip_box = QWidget()
        strip = QHBoxLayout(self.strip_box)
        strip.setContentsMargins(0, 0, 0, 0)
        strip.setSpacing(16)
        self.metrics = {
            key: Metric(
                tr(caption), "meta" if key == "in" else "section", sparkline=key in ("words", "ms")
            )
            for key, caption in (
                ("in", "Входящих ссылок"),
                ("words", "Слов в контенте"),
                ("ms", "Ответ, мс"),
                ("stable", "Стабильность"),
            )
        }
        for metric in self.metrics.values():
            strip.addWidget(metric, 1)
        self.metrics["in"].value.setText(tr("Недоступно в этой версии ядра"))
        self.metrics["in"].sub.setToolTip(unavailable_tip("Входящие ссылки по сканам"))
        layout.addWidget(self.strip_box)
        self.table = QTableWidget(0, len(COLUMNS))
        self.table.setHorizontalHeaderLabels([tr(column) for column in COLUMNS])
        self.table.verticalHeader().hide()
        style_table(self.table, density="compact")
        header = self.table.horizontalHeader()
        header.setMinimumSectionSize(48)
        for column in (0, 1, 2, 4, 5):
            header.setSectionResizeMode(column, QHeaderView.ResizeToContents)
        for column in (3, 6):
            header.setSectionResizeMode(column, QHeaderView.Stretch)
        self.table.setMinimumHeight(132)
        layout.addWidget(self.table, 1)
        self.note = QLabel()
        self.note.setProperty("text_style", "meta")
        self.note.setWordWrap(True)
        layout.addWidget(self.note)
        self.state_host = QWidget()
        self.state_box = QVBoxLayout(self.state_host)
        self.state_box.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(self.state_host)
        self._show(None)

    @property
    def busy(self):
        return self.job.busy

    def context_changed(self):
        """A different URL: the old answers are dropped; the history is read again when the tab is shown."""
        self.job.cancel()
        self.generation += 1
        self.loaded_for = None
        self.entries = []
        self.scans = []
        self._render()

    def activate(self):
        scans = self._scans()
        key = (self.ctx.scan, self.ctx.url, self.total_scans, tuple(row["path"] for row in scans))
        if not self.ctx.url or not scans:
            self.loaded_for = None
            return self._render()
        if self.loaded_for == key:
            return
        self.loaded_for = key
        self.generation += 1
        self.scans = scans
        self.entries = []
        self._next()

    def _scans(self):
        rows = getattr(self.ctx.host_ref, "scan_model", None)
        rows = [
            row
            for row in (rows.rows if rows is not None else [])
            if isinstance(row.get("path"), str)
        ]
        selected = next((row for row in rows if row["path"] == self.ctx.scan), None)
        if selected is None and self.ctx.scan:
            source = (self.ctx.detail or {}).get("source") or {}
            selected = {"path": self.ctx.scan, "uuid": source.get("scan_uuid")}
            rows.append(selected)
        rows.sort(key=lambda row: str(row.get("created_at") or ""))
        self.total_scans = len(rows)
        recent = rows[-MAX_SCANS:]
        if selected is not None and selected not in recent:
            recent = [selected, *recent[1:]]
        return recent

    def _next(self):
        if len(self.entries) >= len(self.scans):
            return self._render()
        self._render()
        self.job.start(
            detail_arguments(self.scans[len(self.entries)]["path"], self.ctx.url),
            ("hist", self.generation),
        )

    def _done(self, token, payload):
        if token != ("hist", self.generation):
            return
        self.entries.append(entry_from(payload))
        self._next()

    def _failed(self, token, text):
        if token == ("hist", self.generation):
            self.entries.append({"state": "error", "text": text})
            self._next()

    def retranslate(self):
        self._render()

    def _render(self):
        """One state at a time: nothing (no URL or no scans), loading, the table with its strip, or an error."""
        if not self.ctx.url or not self.scans:
            self.counted.emit(None)
            return self._show(None)
        self.counted.emit(len(self.entries) or None)
        if len(self.entries) < len(self.scans):
            failed = [entry for entry in self.entries if entry["state"] == "error"]
            if failed and not self.job.busy:
                return self._show("error", tr("Не удалось прочитать историю"), failed[-1]["text"])
            return self._show(
                "loading",
                tr("Читаю историю URL…"),
                trf("Скан {n} из {total}", n=len(self.entries) + 1, total=len(self.scans)),
            )
        if not any(entry["state"] == "ok" for entry in self.entries):
            failed = [entry for entry in self.entries if entry["state"] == "error"]
            if failed:
                return self._show("error", tr("Не удалось прочитать историю"), failed[-1]["text"])
            return self._show(
                "empty",
                tr("В прочитанных сканах нет этого URL"),
                tr(
                    "Этот адрес не найден в прочитанных сканах. Другие сканы проекта могли остаться за пределами выборки."
                ),
            )
        self._show("table")
        self._fill_table()

    def _show(self, kind, title="", text=""):
        clear_layout(self.state_box)
        self.state_host.setVisible(kind not in (None, "table"))
        self.table.setVisible(kind == "table")
        self.strip_box.setVisible(kind == "table")
        self.note.setVisible(False)
        if kind not in (None, "table"):
            self.state_box.addWidget(StatePanel(kind, title, text))

    def _fill_table(self):
        self.table.setRowCount(0)
        previous = None
        for scan, entry in zip(self.scans, self.entries):
            ok = entry["state"] == "ok"
            status = entry.get("status") if ok else None
            uuid = entry.get("uuid") or scan.get("uuid")
            current = tr("Текущий скан") if scan["path"] == self.ctx.scan else ""
            cells = [
                joined(" · ", [short_run_id(uuid) or tr("Нет данных"), current]),
                stamp_text(scan.get("created_at")) or tr("Нет данных"),
                str(status) if status is not None else tr("Нет данных"),
                (entry.get("title") if ok else None) or tr("Нет данных"),
                (number(entry.get("words")) if ok else None) or tr("Нет данных"),
                (number(entry.get("ms")) if ok else None) or tr("Нет данных"),
                changes(entry, previous),
            ]
            row = self.table.rowCount()
            self.table.insertRow(row)
            for column, text in enumerate(cells):
                item = QTableWidgetItem(text)
                if column == 0:
                    item.setToolTip(str(uuid or ""))
                if column in (2, 4, 5):
                    item.setTextAlignment(Qt.AlignRight | Qt.AlignVCenter)
                self.table.setItem(row, column, item)
            if ok:
                previous = entry
        self._set_metrics()
        notes = []
        if self.total_scans > len(self.scans):
            notes.append(
                trf(
                    "Показано {shown} из {total} сканов: выбранный и самые новые. Изменения сравниваются только внутри этой выборки.",
                    shown=len(self.scans),
                    total=self.total_scans,
                )
            )
        if any(
            entry.get("status") is not None and entry["status"] >= 500 for entry in self.entries
        ):
            notes.append(
                tr("Ответ 5xx в скане: время и слова там не сохранены — «Нет данных», а не ноль.")
            )
        self.note.setText(joined("\n", notes))
        self.note.setVisible(bool(notes))

    def _set_metrics(self):
        ok = [entry for entry in self.entries if entry["state"] == "ok"]
        words = [entry.get("words") for entry in ok]
        ms = [entry.get("ms") for entry in ok]
        last_words = next((w for w in reversed(words) if w is not None), None)
        last_ms = next((m for m in reversed(ms) if m is not None), None)
        self.metrics["words"].value.setText(number(last_words) or tr("Нет данных"))
        self.metrics["words"].spark.set_values(words)
        self.metrics["ms"].value.setText(
            number(round(last_ms)) if last_ms is not None else tr("Нет данных")
        )
        self.metrics["ms"].spark.set_values(ms)
        stable = sum(1 for entry in self.entries if entry.get("status") == 200)
        self.metrics["stable"].value.setText(f"{stable} / {len(self.entries)}")
        self.metrics["stable"].sub.setText(tr("скана с ответом 200"))
