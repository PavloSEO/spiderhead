"""Main-window methods: scans urls."""

from __future__ import annotations

import json
from pathlib import Path

from PyQt5.QtCore import (
    QModelIndex,
)
from PyQt5.QtWidgets import (
    QApplication,
    QMenu,
)

from .common import (  # noqa: F401
    CONSUMER_ID,
    PAGE_LIMIT,
    ROOT,
    TASK_PAGE_LIMIT,
    configure_table,
    plain,
    scan_request_key,
)
from .ui.components import PAGE_LIMIT as PANEL_PAGE_LIMIT
from .ui.icons import material_icon as icon
from .ui.presentation import (
    FIELDS,
    field_text,
    readable_record,
    short_run_id,
    state_text,
    value_text,
)


def default_scan_index(rows):
    """The scan to open when the owner has not picked one: the largest complete scan (a full audit), newest on a tie.

    List rows carry no page count, so size is the scan file's byte count. Without a complete scan, the newest row (index 0)."""
    complete = [
        (index, item)
        for index, item in enumerate(rows)
        if item.get("lifecycle") == "finished"
        and item.get("crawl_partial") is False
        and item.get("corpus_partial") is False
    ]
    if not complete:
        return 0
    return max(complete, key=lambda pair: (pair[1].get("bytes") or 0, -pair[0]))[0]


class ScansUrlsMixin:
    task_total = None  # screens read these; the loaders below fill them (None = not loaded yet)
    task_detail_result = None
    task_detail_requested = None

    def load_task_page(self, result):
        """Work list: read the checklist page by page (the core caps a page), then show every task as one list."""
        pagination = result.get("pagination") or {}
        offset = pagination.get("offset", 0)
        self._task_pages = (getattr(self, "_task_pages", []) if offset else []) + list(
            result.get("items") or []
        )
        following = pagination.get("next_offset")
        if following is not None and self.project_directory:
            self.start_command(
                "tasks",
                "seo_project_checklist_page",
                {
                    "directory": self.project_directory,
                    "limit": TASK_PAGE_LIMIT,
                    "offset": following,
                },
                self.load_task_page,
            )
            return
        self.load_tasks(
            {
                "items": self._task_pages,
                "pagination": {**pagination, "offset": 0, "next_offset": None},
            }
        )

    def load_tasks(self, result):
        rows = []
        for item in result.get("items") or []:
            rows.append(
                {
                    "id": item.get("id"),
                    "title": item.get("title"),
                    "kind": item.get("kind"),
                    "state": item.get("display_state"),
                    "reason": item.get("reason"),
                    "priority": item.get("priority"),
                    "execution_kind": item.get("execution_kind"),
                    "blocked_by": item.get("blocked_by") or [],
                }
            )
        self.task_model.replace(
            rows
        )  # every row the Work list shows; the panel below only gets one page
        pagination = result.get("pagination") or {}
        total = pagination.get("total", len(rows))
        self.task_total = total if type(total) is int else len(rows)
        self.navigation.set_count("work", str(total) if total else None)
        self.task_caption.setText(
            f"Задачи · {pagination.get('total', len(rows))} всего · показано {len(rows)}"
        )
        self.show_task_page(0)
        if rows:
            self.task_table.selectRow(0)

    def show_task_page(self, offset=0):
        """Project tasks panel: one presentation page cut from the rows already read, so any checklist size fits."""
        rows = self.task_model.rows
        offset = min(max(0, int(offset)), max(0, len(rows) - 1))
        window = rows[offset : offset + PANEL_PAGE_LIMIT]
        self.project_panels.set_page(
            "tasks",
            window,
            total=self.task_total,
            offset=offset,
            has_more=offset + PANEL_PAGE_LIMIT < len(rows),
            source="Checklist проекта · сохранённые данные",
        )

    def show_task(self, current, _previous):
        if not current.isValid() or not self.project_directory:
            return
        item = self.task_model.rows[current.row()]
        self.select_project_task(item.get("id"))

    def select_project_task(self, item_id):
        if not self.project_directory:
            return
        if isinstance(item_id, str):
            self.task_detail_requested = item_id
            self.start_command(
                "task-detail",
                "seo_project_task_detail",
                {"directory": self.project_directory, "item_id": item_id},
                self.load_task_detail,
            )

    def load_task_detail(self, result):
        self.task_detail_result = result
        self.task_detail.setPlainText(
            readable_record(result, heading="Задача · сохранённый контекст")
        )
        self.data_changed.emit("tasks")

    def load_scans(self, result):
        rows = [
            {
                **item,
                "partial": "да"
                if item.get("crawl_partial") is True or item.get("corpus_partial") is True
                else "нет"
                if item.get("crawl_partial") is False and item.get("corpus_partial") is False
                else None,
            }
            for item in result.get("items") or []
        ]
        old = next(
            (item for item in self.scan_model.rows if item.get("path") == self.selected_scan_path),
            None,
        )
        selection = self.scan_table.selectionModel()
        selection.blockSignals(True)
        self.scan_model.replace(rows)
        self.navigation.set_count("scans", str(result.get("total", len(rows))) if rows else None)
        self.project_panels.panel("compare").set_scans(rows)
        self.scan_picker.blockSignals(True)
        self.scan_picker.clear()
        for item in rows:
            stamp = field_text("finished_at", item.get("finished_at") or item.get("created_at"))
            self.scan_picker.addItem(
                f"{stamp} · {state_text(item.get('source_kind'))} · {short_run_id(item.get('uuid')) or 'ID неизвестен'}",
                item,
            )
        if not rows:
            self.scan_picker.addItem("В проекте нет сохранённых сканов", None)
        self.scan_picker.setEnabled(bool(rows))
        self.scan_picker.blockSignals(False)
        self.scan_caption.setText(
            f"Сканы · {result.get('total', len(rows))} сохранено · показано {len(rows)}"
        )
        self.project_panels.set_page(
            "scans",
            rows,
            total=result.get("total"),
            offset=(result.get("pagination") or {}).get("offset", 0),
            has_more=(result.get("pagination") or {}).get("next_offset") is not None,
            source="Сохранённые сканы проекта",
        )
        if (
            self.project_directory
            and str(Path(self.project_directory).resolve()) in self._known_projects
        ):
            self._known_projects[str(Path(self.project_directory).resolve())]["scans"] = rows
        restore = (
            self._workspace_restore
            if self._workspace_restore
            and self._workspace_restore["id"] == self._active_workspace_id
            else None
        )
        desired_uuid = restore.get("scan_uuid") if restore else None
        if desired_uuid:
            index = next(
                (index for index, item in enumerate(rows) if item.get("uuid") == desired_uuid), None
            )
        else:
            index = next(
                (
                    index
                    for index, item in enumerate(rows)
                    if item.get("path") == self.selected_scan_path
                ),
                None,
            )
        if restore and desired_uuid and index is None:
            selection.blockSignals(False)
            self.clear_scan_selection(
                "Скан этой вкладки отсутствует в текущей сохранённой выборке. Выберите доступный скан."
            )
            self.finish_workspace_restore()
            return
        if index is None:
            index = default_scan_index(rows)
        if rows:
            self.scan_table.selectRow(index)
            self.scan_picker.blockSignals(True)
            self.scan_picker.setCurrentIndex(index)
            self.scan_picker.blockSignals(False)
        selection.blockSignals(False)
        if rows:
            selected = rows[index]
            if (
                self._reload_selected_scan
                or selected.get("path") != self.selected_scan_path
                or selected != old
            ):
                self._reload_selected_scan = False
                self.select_project_scan(
                    selected, offset=restore.get("state", {}).get("url_offset", 0) if restore else 0
                )
        elif result.get("total") == 0 or not self.selected_scan_path:
            self.clear_scan_selection("В этом проекте нет сохранённых сканов")
            self.finish_workspace_restore()

    def clear_scan_selection(self, reason):
        self.content_search.clear(reason)
        self.selected_scan_path = None
        self.selected_scan_uuid = None
        self.update_content_search_context()
        self.clear_url_selection(reason)
        self.model.replace([])
        self.search.clear()
        self.search.setEnabled(False)
        self.audit_workspace.clear(reason)
        for view in (
            self.detail,
            self.debug_detail,
            self.link_detail,
            self.evidence_detail,
            self.scan_detail,
            self.headers_detail,
        ):
            view.setPlainText(reason)
        self.resume_scan_button.setEnabled(False)
        self._resume_eligible_path = None
        self.scan_progress.hide()
        self.scan_progress_label.setText("Прогресс выбранного скана не измерен")
        self.scan_state_badge.set_state("unknown")
        self._url_page_offset = 0
        self._url_page_has_more = False
        self.url_previous.setEnabled(False)
        self.url_next.setEnabled(False)
        self.url_page_label.setText("Выберите сохранённый скан")
        self.url_caption.setText("URL · данные не загружены")
        self.url_empty.setText(reason)
        self.url_empty.show()
        self.table.hide()

    def select_scan_from_picker(self):
        scan = self.scan_picker.currentData()
        if isinstance(scan, dict) and scan.get("path") != self.selected_scan_path:
            self.select_project_scan(scan)

    def show_scan(self, current, _previous):
        if not current.isValid():
            return
        scan = self.scan_model.rows[current.row()]
        self.select_project_scan(scan)

    def select_project_scan(self, scan, *, offset=0):
        path = scan.get("path")
        if not isinstance(path, str) or not path:
            self.scan_detail.setPlainText("Ядро не предоставило путь сохранённого скана.")
            return
        self.clear_scan_selection("Выбран другой скан. Загрузка сохранённых данных…")
        self.selected_scan_path = path
        self.selected_scan_uuid = scan.get("uuid") if isinstance(scan.get("uuid"), str) else None
        self.update_content_search_context()
        self.model.replace([])
        self.search.clear()
        self.search.setEnabled(False)
        self.audit_workspace.set_page(
            "internal",
            [],
            state="loading",
            source="Чтение сохранённого скана",
            reason="Выбран другой скан. Загрузка сохранённых данных…",
        )
        self.audit_workspace.set_page(
            "url_details",
            [],
            state="unavailable",
            reason="Выбран другой скан. Загрузка сохранённых данных…",
        )
        self.resume_scan_button.setEnabled(False)
        self.scan_detail.setPlainText(
            readable_record(
                {
                    key: scan.get(key)
                    for key in (
                        "start_url",
                        "uuid",
                        "source_kind",
                        "lifecycle",
                        "finish_reason",
                        "created_at",
                        "finished_at",
                        "crawl_partial",
                        "corpus_partial",
                        "writer_revision",
                        "path",
                    )
                },
                heading="Сохранённый скан",
            )
        )
        self.scan_state_badge.set_state(
            "partial"
            if scan.get("crawl_partial") or scan.get("corpus_partial")
            else scan.get("lifecycle")
        )
        self.scan_picker.blockSignals(True)
        for index in range(self.scan_picker.count()):
            item = self.scan_picker.itemData(index)
            if isinstance(item, dict) and item.get("path") == path:
                self.scan_picker.setCurrentIndex(index)
                selection = self.scan_table.selectionModel()
                selection.blockSignals(True)
                self.scan_table.selectRow(index)
                selection.blockSignals(False)
                break
        self.scan_picker.blockSignals(False)
        self.start_command(
            scan_request_key(self.current_project_uuid, path, "url-page"),
            "seo_scan_inspect",
            {
                "input_path": path,
                "table": "pages",
                "limit": PAGE_LIMIT,
                "offset": max(0, int(offset)),
            },
            lambda result, path=path: self.load_urls(result, path),
        )
        self.start_command(
            scan_request_key(self.current_project_uuid, path, "scan-status"),
            "seo_scan_status",
            {"input_path": path},
            lambda result, path=path: self.load_scan_status(result, path),
        )

    def load_scan_status(self, result, scan_path=None):
        if scan_path is not None and scan_path != self.selected_scan_path:
            return
        self.evidence_detail.setPlainText(
            readable_record(result, heading="Сохранённый снимок и происхождение")
        )
        source = result.get("source") if isinstance(result.get("source"), dict) else {}
        self.scan_state_badge.set_state(
            "partial"
            if source.get("crawl_partial") or source.get("corpus_partial")
            else source.get("lifecycle")
        )
        frontier = result.get("frontier") or {}
        counts = frontier.get("counts") or {}
        done, queued, inflight = (counts.get(key) for key in ("done", "queued", "inflight"))
        measured = frontier.get("state") == "available" and all(
            type(value) is int and value >= 0 for value in (done, queued, inflight)
        )
        total = done + queued + inflight if measured else 0
        self.scan_progress.setVisible(measured and total > 0)
        if measured and total > 0:
            self.scan_progress.setValue(round(1000 * done / total))
            self.scan_progress_label.setText(
                f"Сохранённый скан {self.selected_scan_uuid or source.get('scan_uuid') or 'ID неизвестен'} · обработано известных URL: {done} из {total} · очередь {queued} · в работе {inflight}. Размер сайта не измерен."
            )
        else:
            self.scan_progress_label.setText(
                "Прогресс известных URL не измерен: нет полного набора счётчиков."
            )
        self.summary_scan.setText(state_text(source.get("lifecycle")))
        outcomes = result.get("committed_page_outcomes")
        if isinstance(outcomes, dict):
            aggregate_rows = [
                {
                    "name": "Без ответа"
                    if key == "no_response"
                    else "Другие"
                    if key == "other"
                    else key,
                    "urls": value,
                }
                for key, value in outcomes.items()
            ]
            self.audit_workspace.set_page(
                "overview",
                aggregate_rows,
                total=len(aggregate_rows),
                source="Сохранённые HTTP-ответы · доля по всему сайту не измерена",
            )
        self._resume_eligible_path = (
            scan_path
            if scan_path and source.get("lifecycle") == "interrupted" and Path(scan_path).is_file()
            else None
        )
        self.update_resume_control()
        self.sync_workspace_identity()

    def load_urls(self, result, scan_path=None):
        if scan_path is not None and scan_path != self.selected_scan_path:
            return
        self.selected_url = None
        self.link_detail.setPlainText("Выберите URL в текущей странице")
        self.audit_workspace.set_page(
            "inlinks", [], state="unavailable", reason="Выберите URL в новой странице"
        )
        rows = []
        for page in result.get("rows") or []:
            rows.append(
                {
                    "url": page.get("url"),
                    "status": page.get("status_code", page.get("status")),
                    "type": page.get("content_type", page.get("type")),
                    "indexability": page.get("indexability", page.get("indexable")),
                    "title": page.get("title"),
                    "issues": page.get("finding_count"),
                    "_retained": page,
                }
            )
        self.clear_url_selection("Выберите URL в загруженной странице")
        self.model.replace(rows)
        self._url_page_offset = int(result.get("offset") or 0)
        self._url_page_has_more = bool(result.get("has_more"))
        self.url_previous.setEnabled(self._url_page_offset > 0)
        self.url_next.setEnabled(self._url_page_has_more and bool(rows))
        self.url_page_label.setText(
            f"Строки {self._url_page_offset + 1}–{self._url_page_offset + len(rows)} · сохранённая страница"
            if rows
            else "На этой странице нет URL"
        )
        self.search.setEnabled(True)
        self.search.setPlaceholderText("Поиск в загруженной странице сохранённого скана")
        self.update_url_count()
        self.url_scope_caption.setText(
            f"Скан {self.selected_scan_uuid or 'ID неизвестен'} · поиск и сортировка только в загруженной странице"
        )
        self.summary_records.setText(str(len(rows)))
        self.summary_source.setText("Native · сохранённый скан")
        self.summary_coverage.setText("Размер сайта не измерен")
        self.summary_note.setPlainText(
            readable_record(
                {
                    key: result.get(key)
                    for key in ("offset", "has_more", "next_offset", "truncated", "bytes")
                },
                heading="Текущая страница URL",
            )
            + "\n\nПоиск и сортировка применяются к загруженной странице, не ко всему скану."
        )
        self.audit_workspace.set_page(
            "internal",
            rows,
            total=None,
            offset=result.get("offset", 0),
            has_more=bool(result.get("has_more")),
            source=f"Скан {self.selected_scan_uuid or 'ID неизвестен'} · загруженная страница",
            available_filters=("all",),
        )
        if rows and self.navigation.currentRow() == 1 and self.proxy.rowCount():
            self.table.selectRow(0)
        elif (
            rows
            and self.navigation.currentRow() == 2
            and self.audit_workspace.main.current_id == "internal"
        ):
            panel = self.audit_workspace.panel("internal")
            if panel.proxy.rowCount():
                panel.table.selectRow(0)
        self.finish_workspace_restore()

    def clear_url_selection(self, reason="Выберите сохранённый URL"):
        self.selected_url = None
        self.url_selection_generation += 1
        self.pending_commands.pop("url-detail", None)
        self.pending_commands.pop("url-links", None)
        for view in (self.detail, self.debug_detail, self.link_detail, self.headers_detail):
            view.setPlainText(reason)
        self.audit_workspace.detail.clear(reason)

    def update_url_count(self):
        found, loaded = self.proxy.rowCount(), self.model.rowCount()
        self.url_caption.setText(
            f"URL · {found} из {loaded} на странице"
            if self.search.text()
            else f"URL · {loaded} записей"
        )
        if hasattr(self, "url_empty"):
            self.url_empty.setText(
                "По вашему поиску нет URL в этой странице"
                if self.search.text()
                else "Выберите сохранённый скан"
            )
            self.url_empty.setVisible(found == 0)
            self.table.setVisible(found > 0)

    def filter_urls(self, text):
        if not hasattr(self, "table"):
            return
        self.clear_url_selection("Поиск изменён. Выберите URL из результатов или нажмите Enter.")
        selection = self.table.selectionModel()
        selection.blockSignals(True)
        self.proxy.setFilterFixedString(text)
        self.table.clearSelection()
        self.table.setCurrentIndex(QModelIndex())
        selection.blockSignals(False)
        self.update_url_count()

    def show_url(self, current, _previous):
        source = self.proxy.mapToSource(current)
        if not source.isValid():
            self.clear_url_selection("Выберите сохранённый URL")
            return
        row = self.model.rows[source.row()]
        retained = row.get("_retained")
        if retained:
            self.detail.setPlainText(readable_record(retained, heading="Сохранённые данные URL"))
            self.debug_detail.setPlainText(json.dumps(retained, ensure_ascii=False, indent=2))
            self.show_retained_url(row)
            return
        self.detail.setPlainText(
            readable_record(
                {k: v for k, v in row.items() if not k.startswith("_")}, heading="Данные URL"
            )
        )
        self.debug_detail.setPlainText(json.dumps(row, ensure_ascii=False, indent=2))

    def load_url_links(self, result, scan_path=None, url=None, selection_generation=None):
        if (
            selection_generation is not None
            and selection_generation != self.url_selection_generation
        ):
            return
        if scan_path != self.selected_scan_path or url != self.selected_url:
            return
        self.link_detail.setPlainText(
            readable_record(result, heading="Сохранённые входящие ссылки")
        )
        self.audit_workspace.set_page(
            "inlinks",
            result.get("items") or [],
            total=result.get("total"),
            offset=result.get("offset", 0),
            has_more=bool(result.get("has_more")),
            source="Сохранённые входящие ссылки",
        )

    def load_url_detail(self, result, scan_path=None, url=None, selection_generation=None):
        if (
            selection_generation is not None
            and selection_generation != self.url_selection_generation
        ):
            return
        if scan_path != self.selected_scan_path or url != self.selected_url:
            return
        page = result.get("page") or {}
        visible = {
            key: page.get(key)
            for key in (
                "url",
                "status_code",
                "content_type",
                "title",
                "meta_description",
                "h1",
                "canonical",
                "meta_robots",
                "x_robots",
                "response_time",
                "size_bytes",
                "word_count",
                "crawl_depth",
                "outlinks",
                "external_outlinks",
            )
            if key in page
        }
        self.detail.setPlainText(
            readable_record(visible, heading="Сохранённые данные URL")
            + "\n\nИсточник и ревизия: "
            + value_text(result.get("source"))
        )
        self.debug_detail.setPlainText(json.dumps(result, ensure_ascii=False, indent=2))
        responses = (result.get("responses") or {}).get("items") or []
        header_rows = []
        if responses:
            response = responses[0]
            for direction, key in (("Запрос", "request_headers"), ("Ответ", "response_headers")):
                for name, value in (response.get(key) or [])[:40]:
                    redacted = str(name).casefold() in {
                        "authorization",
                        "proxy-authorization",
                        "cookie",
                        "set-cookie",
                    }
                    header_rows.append(
                        {
                            "name": str(name),
                            "value": "[скрыто]" if redacted else value,
                            "direction": direction,
                        }
                    )
            self.headers_detail.setPlainText(
                readable_record(
                    {
                        "response_id": response.get("response_id"),
                        "requested_at": response.get("requested_at"),
                        "received_at": response.get("received_at"),
                        "headers": header_rows,
                    },
                    heading="HTTP headers · первый сохранённый ответ URL",
                )
            )
            self.audit_workspace.set_page(
                "http_headers",
                header_rows,
                total=len(header_rows),
                source="Первый сохранённый ответ URL · чувствительные значения скрыты",
            )
        else:
            self.headers_detail.setPlainText(
                "В сохранённой детали нет HTTP-ответа. Новые запросы при выборе URL не выполняются."
            )
        rows = [{"name": FIELDS.get(key, key), "value": value} for key, value in page.items()]
        self.audit_workspace.set_page(
            "url_details",
            rows,
            total=len(rows),
            source="Сохранённая деталь URL · безопасная проекция ядра",
            state="ready" if result.get("state") == "available" else "unavailable",
            reason=result.get("reason", ""),
        )

    def handle_audit_intent(self, intent, payload):
        if intent == "clear_url":
            self.clear_url_selection("Выберите URL в текущем разделе")
            return
        if intent == "refresh":
            self.refresh_project()
            return
        if intent == "query" and isinstance(payload, dict) and payload.get("tab_id") == "internal":
            if not self.selected_scan_path:
                return
            self.start_command(
                scan_request_key(self.current_project_uuid, self.selected_scan_path, "url-page"),
                "seo_scan_inspect",
                {
                    "input_path": self.selected_scan_path,
                    "table": "pages",
                    "limit": min(100, int(payload.get("limit", PAGE_LIMIT))),
                    "offset": max(0, int(payload.get("offset", 0))),
                },
                lambda result, path=self.selected_scan_path: self.load_urls(result, path),
            )
            return
        if intent != "select_url" or not isinstance(payload, dict):
            return
        row = payload.get("row")
        if not isinstance(row, dict):
            return
        self.show_retained_url(row)

    def show_retained_url(self, row):
        if not self.selected_scan_path or not isinstance(row.get("url"), str):
            return
        self.clear_url_selection("Загрузка выбранного URL…")
        self.selected_url = row["url"]
        selection_generation = self.url_selection_generation
        self.link_detail.setPlainText("Загрузка ссылок выбранного URL…")
        self.headers_detail.setPlainText("HTTP headers выбранного URL загружаются…")
        self.audit_workspace.set_page(
            "inlinks", [], state="loading", reason="Загрузка выбранного URL…"
        )
        self.audit_workspace.set_page(
            "url_details", [], state="loading", reason="Загрузка выбранного URL…"
        )
        self.start_command(
            scan_request_key(self.current_project_uuid, self.selected_scan_path, "url-detail"),
            "seo_scan_url_detail",
            {
                "input_path": self.selected_scan_path,
                "url": row["url"],
                "response_limit": 10,
                "form_limit": 20,
                "max_bytes": 262144,
            },
            lambda result, path=self.selected_scan_path, url=row["url"], token=selection_generation: (
                self.load_url_detail(result, path, url, token)
            ),
        )
        self.start_command(
            scan_request_key(self.current_project_uuid, self.selected_scan_path, "url-links"),
            "seo_scan_link_inspect",
            {
                "input_path": self.selected_scan_path,
                "view": "inlinks",
                "target": row["url"],
                "limit": 20,
                "max_bytes": 262144,
            },
            lambda result, path=self.selected_scan_path, url=row["url"], token=selection_generation: (
                self.load_url_links(result, path, url, token)
            ),
        )

    def load_comparison(self, payload):
        panel = self.project_panels.panel("compare")
        panel.set_page(
            payload.get("rows") or [],
            total=payload.get("total"),
            offset=payload.get("offset", 0),
            has_more=bool(payload.get("has_more")),
            state=payload.get("state", "unavailable"),
            reason=payload.get("reason", ""),
            source=payload.get("source", "Сравнение сохранённых данных"),
            available_filters=("all",),
        )
        panel.set_summary(payload)

    def handle_project_intent(self, intent, payload):
        if intent == "preview_compare" and isinstance(payload, dict) and self.project_directory:
            self.comparison.start(payload.get("before"), payload.get("after"))
            return
        if intent != "query" or not isinstance(payload, dict) or not self.project_directory:
            return
        tab_id = payload.get("tab_id")
        limit = min(100, max(1, int(payload.get("limit", PAGE_LIMIT))))
        offset = max(0, int(payload.get("offset", 0)))
        if tab_id == "compare":
            self.comparison.page(offset, limit)
        elif tab_id == "tasks":
            self.show_task_page(offset)
        elif tab_id == "scans":
            self.start_command(
                "scans",
                "seo_project_scans",
                {"directory": self.project_directory, "limit": limit, "offset": offset},
                self.load_scans,
            )
        elif tab_id == "inbox":
            self.start_command(
                "inbox",
                "seo_project_inbox_list",
                {
                    "directory": self.project_directory,
                    "consumer": CONSUMER_ID,
                    "limit": limit,
                    "offset": offset,
                },
                self.load_inbox,
            )

    def request_url_page(self, offset):
        if not self.selected_scan_path:
            return
        self.clear_url_selection("Загрузка другой страницы URL…")
        self.url_previous.setEnabled(False)
        self.url_next.setEnabled(False)
        self.start_command(
            scan_request_key(self.current_project_uuid, self.selected_scan_path, "url-page"),
            "seo_scan_inspect",
            {
                "input_path": self.selected_scan_path,
                "table": "pages",
                "limit": PAGE_LIMIT,
                "offset": max(0, int(offset)),
            },
            lambda result, path=self.selected_scan_path: self.load_urls(result, path),
        )

    def url_context_menu(self, point):
        index = self.table.indexAt(point)
        if index.isValid():
            self.table.selectRow(index.row())
        if not self.table.currentIndex().isValid():
            return
        menu = QMenu(self)
        menu.addAction(
            icon("content_copy"),
            "Копировать URL",
            lambda: QApplication.clipboard().setText(
                str(self.proxy.index(self.table.currentIndex().row(), 0).data())
            ),
        )
        menu.addAction("Копировать строку (TSV)", self.copy_url_selection)
        menu.exec_(self.table.viewport().mapToGlobal(point))

    def remember_url_column_width(self, column, _old, width):
        if not self._fitting_url_columns and 0 <= column < len(self._url_column_bases):
            self._url_column_bases[column] = width

    def fit_url_columns(self):
        if not hasattr(self, "_url_column_bases") or self._fitting_url_columns:
            return
        widths = list(self._url_column_bases)
        extra = max(0, self.table.viewport().width() - sum(widths))
        widths[0] += round(extra * 0.6)
        widths[4] += extra - round(extra * 0.6)
        self._fitting_url_columns = True
        for column, width in enumerate(widths):
            self.table.setColumnWidth(column, width)
        self._fitting_url_columns = False

    def copy_url_selection(self):
        current = self.table.currentIndex()
        if current.isValid():
            QApplication.clipboard().setText(
                "\t".join(
                    str(self.proxy.index(current.row(), column).data())
                    for column in range(self.proxy.columnCount())
                    if not self.table.isColumnHidden(column)
                )
            )
