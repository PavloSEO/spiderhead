"""Explicit CrUX field-data queries with a separate, unavailable PageSpeed lab state."""

from __future__ import annotations

import json
from urllib.parse import urlsplit

from PyQt5.QtCore import QAbstractTableModel, Qt
from PyQt5.QtWidgets import (
    QComboBox,
    QFileDialog,
    QHBoxLayout,
    QLabel,
    QMessageBox,
    QPlainTextEdit,
    QPushButton,
    QTableView,
    QVBoxLayout,
    QWidget,
)

from .url_query import CoreJob

METRICS = (
    "largest_contentful_paint",
    "interaction_to_next_paint",
    "cumulative_layout_shift",
)
FORM_FACTORS = ("PHONE", "DESKTOP", "TABLET")
MAX_SAMPLES = 25


class PageSpeedRows(QAbstractTableModel):
    COLUMNS = ("url", "strategy", "state", "lcp", "inp", "cls", "performance")
    AUDITS = {
        "lcp": "largest-contentful-paint",
        "inp": "interaction-to-next-paint",
        "cls": "cumulative-layout-shift",
    }

    def __init__(self, parent=None):
        super().__init__(parent)
        self.rows = []

    def rowCount(self, parent=None):
        return 0 if parent.isValid() else len(self.rows)

    def columnCount(self, parent=None):
        return 0 if parent.isValid() else len(self.COLUMNS)

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid() or role != Qt.DisplayRole:
            return None
        row = self.rows[index.row()]
        column = self.COLUMNS[index.column()]
        if column in {"url", "strategy", "state"}:
            return row.get(column, "—")
        if column == "performance":
            category = (row.get("categories") or {}).get("performance") or {}
            return category.get("score", "Not reported")
        audit = (row.get("audits") or {}).get(self.AUDITS[column]) or {}
        value = audit.get("display_value")
        if value is not None:
            return value
        value = audit.get("numeric_value")
        return value if value is not None else "Not reported"

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if role != Qt.DisplayRole or orientation != Qt.Horizontal or section >= len(self.COLUMNS):
            return None
        return {
            "url": "URL",
            "strategy": "Strategy",
            "state": "State",
            "lcp": "LCP",
            "inp": "INP",
            "cls": "CLS",
            "performance": "Lab performance",
        }[self.COLUMNS[section]]

    def set_rows(self, rows):
        bounded = (
            [row for row in rows[:MAX_SAMPLES] if isinstance(row, dict)]
            if isinstance(rows, list)
            else []
        )
        self.beginResetModel()
        self.rows = bounded
        self.endResetModel()


def crux_arguments(kind: str, targets: list[str], form_factor: str) -> list[str]:
    if form_factor not in FORM_FACTORS or not targets or len(targets) > MAX_SAMPLES:
        raise ValueError("Choose 1–25 targets and a supported form factor")
    for target in targets:
        parsed = urlsplit(target)
        if parsed.scheme not in {"http", "https"} or not parsed.netloc or target != target.strip():
            raise ValueError("Targets must be absolute HTTP(S) URLs or origins")
    arguments = ["crux-report", "--form-factor", form_factor, "--metrics", ",".join(METRICS)]
    if kind == "url" and len(targets) == 1:
        arguments += ["--url", targets[0]]
    elif kind == "origin" and len(targets) == 1:
        parsed = urlsplit(targets[0])
        if parsed.path not in ("", "/") or parsed.query or parsed.fragment:
            raise ValueError("Origin queries accept only scheme and host")
        arguments += ["--origin", targets[0]]
    elif kind == "samples":
        if any("," in target for target in targets):
            raise ValueError("Sample URLs cannot contain commas in this CLI input format")
        arguments += ["--urls", ",".join(targets), "--max-samples", str(len(targets))]
    else:
        raise ValueError("Select a URL, origin or bounded sample request")
    return arguments


def pagespeed_collect_arguments(targets: list[str], strategy: str, artifact_dir: str) -> list[str]:
    if not isinstance(targets, list) or not 1 <= len(targets) <= MAX_SAMPLES or not artifact_dir:
        raise ValueError("Choose a private evidence folder and 1–25 URLs")
    for target in targets:
        parsed = urlsplit(target)
        if parsed.scheme not in {"http", "https"} or not parsed.netloc or target != target.strip():
            raise ValueError("PageSpeed targets must be absolute HTTP(S) URLs")
    if any("," in target for target in targets):
        raise ValueError("PageSpeed targets cannot contain commas in this CLI input format")
    if strategy not in {"mobile_samples", "desktop_samples"}:
        raise ValueError("Choose a supported PageSpeed sample strategy")
    request = json.dumps({"urls": targets}, ensure_ascii=False, separators=(",", ":"))
    return [
        "provider-collect",
        "--provider",
        "pagespeed",
        "--operation",
        strategy,
        "--artifact-dir",
        artifact_dir,
        "--input",
        request,
    ]


def pagespeed_read_arguments(artifact_dir: str, artifact_ref: str) -> list[str]:
    if not artifact_dir or not artifact_ref.startswith("local-artifact:"):
        raise ValueError("Choose a retained PageSpeed artifact")
    return [
        "provider-artifact-read",
        "--artifact-ref",
        artifact_ref,
        "--artifact-dir",
        artifact_dir,
        "--offset",
        "0",
        "--limit",
        str(MAX_SAMPLES),
    ]


def pagespeed_projection(payload: dict) -> dict:
    """Render only the typed PageSpeed artifact-read fields, even if the payload grows later."""
    samples = []
    source_rows = payload.get("samples")
    source_rows = source_rows if isinstance(source_rows, list) else []
    for sample in source_rows[:MAX_SAMPLES]:
        if not isinstance(sample, dict):
            continue
        categories = sample.get("categories")
        audits = sample.get("audits")
        samples.append(
            {
                key: sample[key]
                for key in (
                    "url",
                    "final_url",
                    "strategy",
                    "state",
                    "fetch_time",
                    "lighthouse_version",
                    "lab_only",
                )
                if key in sample
            }
            | {
                "categories": {
                    name: {key: value[key] for key in ("score", "title") if key in value}
                    for name, value in categories.items()
                    if isinstance(name, str) and isinstance(value, dict)
                }
                if isinstance(categories, dict)
                else {},
                "audits": {
                    name: {
                        key: value[key]
                        for key in ("score", "numeric_value", "display_value", "score_display_mode")
                        if key in value
                    }
                    for name, value in audits.items()
                    if isinstance(name, str) and isinstance(value, dict)
                }
                if isinstance(audits, dict)
                else {},
                "lab_only": True,
            }
        )
    return {
        key: payload[key]
        for key in (
            "schema_version",
            "provider",
            "operation",
            "retrieved_at",
            "total",
            "offset",
            "limit",
            "has_more",
        )
        if key in payload
    } | {"lab_only": True, "samples": samples}


class VitalsPage(QWidget):
    """CrUX is an explicit keyed request; no provider calls happen while the page opens."""

    def __init__(self, host):
        super().__init__()
        self.host = host
        self.job = CoreJob(host, self)
        self.job.done.connect(self._done)
        self.job.failed.connect(self._failed)
        self._token = None
        root = QVBoxLayout(self)
        root.setContentsMargins(16, 14, 16, 14)
        root.setSpacing(10)
        title = QLabel("Core Web Vitals")
        title.setProperty("text_style", "title")
        root.addWidget(title)
        note = QLabel(
            "CrUX reports field data from real Chrome users. LCP, INP and CLS are shown only when returned by the source."
        )
        note.setProperty("text_style", "meta")
        note.setWordWrap(True)
        root.addWidget(note)
        controls = QHBoxLayout()
        self.kind = QComboBox()
        self.kind.addItem("Single URL", "url")
        self.kind.addItem("Origin", "origin")
        self.kind.addItem("Template sample URLs", "samples")
        controls.addWidget(self.kind)
        self.form_factor = QComboBox()
        for value in FORM_FACTORS:
            self.form_factor.addItem(value.title(), value)
        controls.addWidget(self.form_factor)
        self.query_button = QPushButton("Query CrUX…")
        self.query_button.setProperty("role", "primary")
        self.query_button.clicked.connect(self.query)
        controls.addWidget(self.query_button)
        root.addLayout(controls)
        self.targets = QPlainTextEdit()
        self.targets.setMaximumHeight(110)
        self.targets.setPlaceholderText(
            "https://example.test/page\nFor sample mode, enter up to 25 URLs, one per line."
        )
        root.addWidget(self.targets)
        self.status = QLabel(
            "No request has been made. Querying requires an API key and may consume the account's quota."
        )
        self.status.setProperty("text_style", "meta")
        self.status.setWordWrap(True)
        root.addWidget(self.status)
        self.result = QPlainTextEdit()
        self.result.setReadOnly(True)
        self.result.setPlaceholderText("Returned field metrics and provenance appear here.")
        root.addWidget(self.result, 1)
        lab_title = QLabel("PageSpeed Insights lab samples")
        lab_title.setProperty("text_style", "section")
        root.addWidget(lab_title)
        self.psi_strategy = QComboBox()
        self.psi_strategy.addItem("Mobile", "mobile_samples")
        self.psi_strategy.addItem("Mobile and desktop", "desktop_samples")
        root.addWidget(self.psi_strategy)
        psi_actions = QHBoxLayout()
        self.psi_button = QPushButton("Run PageSpeed sample…")
        self.psi_button.clicked.connect(self.collect_pagespeed)
        psi_actions.addWidget(self.psi_button)
        self.history = QComboBox()
        self.history.setMinimumWidth(240)
        psi_actions.addWidget(self.history, 1)
        self.load_history_button = QPushButton("Read saved sample")
        self.load_history_button.clicked.connect(self.read_saved_pagespeed)
        psi_actions.addWidget(self.load_history_button)
        root.addLayout(psi_actions)
        self.lab_status = QLabel(
            "Lab data is separate from CrUX field data. Requests require an API key and confirmation; saved results are read through a bounded field allowlist."
        )
        self.lab_status.setProperty("text_style", "meta")
        self.lab_status.setWordWrap(True)
        root.addWidget(self.lab_status)
        self.lab_result = QTableView()
        self.lab_model = PageSpeedRows(self.lab_result)
        self.lab_result.setModel(self.lab_model)
        self.lab_result.setAlternatingRowColors(True)
        self.lab_result.setSelectionBehavior(QTableView.SelectRows)
        root.addWidget(self.lab_result, 1)
        self._pagespeed_history = self._read_history()
        self._sync_history()

    def query(self):
        targets = [line.strip() for line in self.targets.toPlainText().splitlines() if line.strip()]
        try:
            arguments = crux_arguments(
                self.kind.currentData(), targets, self.form_factor.currentData()
            )
        except ValueError as exc:
            self.status.setText(str(exc))
            return
        description = (
            "one CrUX field-data request"
            if self.kind.currentData() != "samples"
            else f"a CrUX sample of {len(targets)} URLs"
        )
        answer = QMessageBox.question(
            self,
            "Query Chrome UX Report",
            f"This sends {description} using the configured Google API key. It can consume quota. Continue?",
            QMessageBox.Yes | QMessageBox.No,
            QMessageBox.No,
        )
        if answer != QMessageBox.Yes:
            return
        token = object()
        self._token = token
        self.query_button.setEnabled(False)
        self.status.setText("Waiting for the explicitly requested CrUX response…")
        self.result.clear()
        self.job.start(arguments, token, timeout_ms=120_000)

    def collect_pagespeed(self):
        if not self.host.project_directory or not getattr(self.host, "current_project_uuid", None):
            self.lab_status.setText("Open a project before retaining PageSpeed evidence.")
            return
        targets = [line.strip() for line in self.targets.toPlainText().splitlines() if line.strip()]
        if not 1 <= len(targets) <= MAX_SAMPLES:
            self.lab_status.setText(
                "Enter 1–25 absolute URLs, one per line, in the targets box above."
            )
            return
        artifact_dir = QFileDialog.getExistingDirectory(
            self, "Choose a private folder for the saved PageSpeed evidence"
        )
        if not artifact_dir:
            return
        strategy = self.psi_strategy.currentData()
        try:
            arguments = pagespeed_collect_arguments(targets, strategy, artifact_dir)
        except ValueError as exc:
            self.lab_status.setText(str(exc))
            return
        answer = QMessageBox.question(
            self,
            "Run PageSpeed sample",
            f"This sends {len(targets)} keyed Lighthouse lab sample request(s) to PageSpeed. The private results will be saved in the selected folder. Continue?",
            QMessageBox.Yes | QMessageBox.No,
            QMessageBox.No,
        )
        if answer != QMessageBox.Yes:
            return
        token = object()
        self._token = ("pagespeed_collect", token, artifact_dir, strategy)
        self.psi_button.setEnabled(False)
        self.lab_status.setText("Running the explicitly requested PageSpeed lab sample…")
        self.lab_model.set_rows([])
        self.job.start(arguments, token, timeout_ms=180_000)

    def read_saved_pagespeed(self):
        index = self.history.currentIndex()
        if index < 0 or index >= len(self._pagespeed_history):
            self.lab_status.setText("Choose a saved PageSpeed sample first.")
            return
        record = self._pagespeed_history[index]
        try:
            arguments = pagespeed_read_arguments(record["artifact_dir"], record["artifact_ref"])
        except (KeyError, ValueError) as exc:
            self.lab_status.setText(str(exc))
            return
        token = object()
        self._token = ("pagespeed_read", token)
        self.lab_status.setText("Reading allowlisted lab fields from the retained artifact…")
        self.job.start(arguments, token)

    def _read_history(self):
        project_uuid = getattr(self.host, "current_project_uuid", None)
        settings = getattr(self.host, "settings", None)
        if not project_uuid or settings is None:
            return []
        try:
            rows = json.loads(settings.value(f"pagespeed/history/{project_uuid}", "[]"))
        except (TypeError, ValueError):
            return []
        return [row for row in rows if isinstance(row, dict) and row.get("artifact_ref")]

    def refresh_history(self):
        self._pagespeed_history = self._read_history()
        self._sync_history()
        self.lab_model.set_rows([])
        self.lab_status.setText(
            "Lab data is separate from CrUX field data. Requests require an API key and confirmation; saved results are read through a bounded field allowlist."
        )

    def _save_history(self, row):
        self._pagespeed_history.insert(0, row)
        self._pagespeed_history = self._pagespeed_history[:25]
        project_uuid = getattr(self.host, "current_project_uuid", None)
        settings = getattr(self.host, "settings", None)
        if project_uuid and settings is not None:
            settings.setValue(
                f"pagespeed/history/{project_uuid}",
                json.dumps(self._pagespeed_history, ensure_ascii=False),
            )
        self._sync_history()

    def _sync_history(self):
        self.history.clear()
        for row in self._pagespeed_history:
            label = f"{row.get('retrieved_at', 'Saved sample')} · {row.get('strategy', 'lab')}"
            self.history.addItem(label)
        self.load_history_button.setEnabled(bool(self._pagespeed_history))

    def _done(self, token, payload):
        if isinstance(self._token, tuple) and token is self._token[1]:
            pending = self._token
            kind = pending[0]
            self._token = None
            if kind == "pagespeed_collect":
                self.psi_button.setEnabled(True)
                evidence = payload.get("evidence") if isinstance(payload, dict) else None
                reference = (
                    evidence.get("artifact_reference") if isinstance(evidence, dict) else None
                )
                if not isinstance(reference, str) or not reference.startswith("local-artifact:"):
                    self.lab_status.setText(
                        str(
                            (evidence or {}).get("status")
                            or "No retained PageSpeed artifact was created."
                        )[:300]
                    )
                    return
                artifact_dir, strategy = pending[2], pending[3]
                self._save_history(
                    {
                        "artifact_dir": artifact_dir,
                        "artifact_ref": reference,
                        "retrieved_at": evidence.get("retrieved_at"),
                        "strategy": strategy,
                    }
                )
                try:
                    arguments = pagespeed_read_arguments(artifact_dir, reference)
                except ValueError as exc:
                    self.lab_status.setText(str(exc))
                    return
                read_token = object()
                self._token = ("pagespeed_read", read_token)
                self.lab_status.setText(
                    "Reading allowlisted Lighthouse fields from the retained artifact…"
                )
                self.job.start(arguments, read_token)
                return
            if kind == "pagespeed_read":
                if payload.get("ok") is not True:
                    self.lab_status.setText(
                        str(payload.get("reason") or "Saved PageSpeed fields are unavailable.")[
                            :300
                        ]
                    )
                    return
                self.lab_model.set_rows(pagespeed_projection(payload).get("samples"))
                self.lab_status.setText("Sanitized Lighthouse lab fields · not CrUX field data")
                return
        if token is not self._token:
            return
        self._token = None
        self.query_button.setEnabled(True)
        self.status.setText(
            f"{payload.get('state', 'unknown')} · CrUX current field data · {payload.get('retrieved_at', 'time unavailable')}"
        )
        visible = _field_projection(payload)
        self.result.setPlainText(json.dumps(visible, ensure_ascii=False, indent=2))

    def _failed(self, token, reason):
        if isinstance(self._token, tuple) and token is self._token[1]:
            kind = self._token[0]
            self._token = None
            if kind == "pagespeed_collect":
                self.psi_button.setEnabled(True)
            self.lab_status.setText(str(reason)[:300])
            return
        if token is not self._token:
            return
        self._token = None
        self.query_button.setEnabled(True)
        self.status.setText(str(reason)[:300])


def _field_projection(payload):
    """Keep only report fields and provenance in the UI; unknown metrics remain absent."""
    if isinstance(payload.get("records"), list):
        return {
            "requested": payload.get("requested"),
            "sampled": payload.get("sampled"),
            "omitted": payload.get("omitted"),
            "records": [
                _field_projection(row) for row in payload["records"] if isinstance(row, dict)
            ],
        }
    return {
        key: payload[key]
        for key in (
            "state",
            "target",
            "target_kind",
            "form_factor",
            "metric_source",
            "retrieved_at",
            "collection_period",
            "metrics",
            "assessment",
            "note",
            "error",
        )
        if key in payload
    }
