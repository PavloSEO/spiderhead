import os
import unittest
from unittest import mock

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from seohead_desktop.common import TASK_PAGE_LIMIT
from seohead_desktop.models import RecordModel
from seohead_desktop.scans_urls import ScansUrlsMixin


class _Host:
    """Only what load_task_page touches: the project, the command starter and the single-list loader."""

    load_task_page = ScansUrlsMixin.load_task_page

    def __init__(self):
        self.project_directory = "/projects/owner"
        self.started = []
        self.loaded = []

    def start_command(self, operation, tool, arguments, handler):
        self.started.append((operation, tool, arguments, handler))

    def load_tasks(self, result):
        self.loaded.append(result)


def _page(offset, count, total, next_offset):
    items = [{"id": f"check:{n}", "title": f"Task {n}", "kind": "check", "display_state": "remaining"} for n in range(offset, offset + count)]
    return {"items": items, "pagination": {"total": total, "offset": offset, "limit": TASK_PAGE_LIMIT, "next_offset": next_offset}}


class WorkTaskPagesTests(unittest.TestCase):
    def test_every_page_is_read_and_shown_as_one_list(self):
        host = _Host()
        load = ScansUrlsMixin.load_task_page
        load(host, _page(0, 100, 277, 100))
        self.assertEqual(len(host.started), 1)
        operation, tool, arguments, handler = host.started[0]
        self.assertEqual((operation, tool), ("tasks", "seo_project_checklist_page"))
        self.assertEqual(arguments, {"directory": "/projects/owner", "limit": TASK_PAGE_LIMIT, "offset": 100})
        self.assertEqual(host.loaded, [])
        handler(_page(100, 100, 277, 200))
        self.assertEqual(host.started[-1][2]["offset"], 200)
        host.started[-1][3](_page(200, 77, 277, None))
        self.assertEqual(len(host.loaded), 1)
        merged = host.loaded[0]
        self.assertEqual(len(merged["items"]), 277)
        self.assertEqual(merged["items"][-1]["id"], "check:276")
        self.assertEqual(merged["pagination"]["total"], 277)
        self.assertIsNone(merged["pagination"]["next_offset"])
        self.assertEqual(merged["pagination"]["offset"], 0)

    def test_single_page_is_shown_without_another_request(self):
        host = _Host()
        ScansUrlsMixin.load_task_page(host, _page(0, 3, 3, None))
        self.assertEqual(host.started, [])
        self.assertEqual([len(result["items"]) for result in host.loaded], [3])

    def test_a_new_first_page_discards_pages_of_an_earlier_read(self):
        host = _Host()
        ScansUrlsMixin.load_task_page(host, _page(0, 100, 150, 100))
        ScansUrlsMixin.load_task_page(host, _page(0, 2, 2, None))
        self.assertEqual([len(result["items"]) for result in host.loaded], [2])


class _Panels:
    def __init__(self):
        self.calls = []

    def set_page(self, name, items, **kwargs):
        self.calls.append((name, list(items), kwargs))


class _ShownHost(_Host):
    """Work list with the real load_tasks and show_task_page; the Qt widgets are stand-ins."""

    load_tasks = ScansUrlsMixin.load_tasks
    show_task_page = ScansUrlsMixin.show_task_page

    def __init__(self):
        super().__init__()
        self.task_model = RecordModel((("Задача", "title"),))
        self.task_table = mock.MagicMock()
        self.navigation = mock.MagicMock()
        self.task_caption = mock.MagicMock()
        self.project_panels = _Panels()
        self.task_total = None


class WorkTaskRowCountTests(unittest.TestCase):
    def test_checklist_of_any_size_is_kept_whole_and_paged_for_the_panel(self):
        host = _ShownHost()
        host.load_tasks(_page(0, 277, 277, None))
        self.assertEqual(len(host.task_model.rows), 277)
        self.assertEqual(host.task_total, 277)
        name, window, kwargs = host.project_panels.calls[-1]
        self.assertEqual((name, len(window), kwargs["offset"], kwargs["has_more"]), ("tasks", 100, 0, True))
        host.show_task_page(200)
        _name, window, kwargs = host.project_panels.calls[-1]
        self.assertEqual((window[0]["id"], len(window), kwargs["offset"], kwargs["has_more"]), ("check:200", 77, 200, False))

    def test_a_row_past_the_panel_page_limit_does_not_raise(self):
        host = _ShownHost()
        host.load_tasks(_page(0, 101, 101, None))
        self.assertEqual(len(host.project_panels.calls[-1][1]), 100)


if __name__ == "__main__":
    unittest.main()
