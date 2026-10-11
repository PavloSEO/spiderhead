"""UrlGraph: the bounded walk over ``scan-link-inspect`` answers (a stand-in core with a small fixed link set)."""

import unittest

from seohead_desktop import qt
from seohead_desktop.screens import url_graph
from seohead_desktop.screens.url_graph import GraphPage

# (url, direction) -> links the scan holds for that URL in that direction; A is the centre of the walk
LINKS = {
    ("A", "out"): [("A", "B", "to b"), ("A", "C", "to c")],
    ("A", "in"): [("D", "A", "from d")],
    ("B", "out"): [("B", "E", "to e")],
    ("B", "in"): [("A", "B", "to b")],
    ("C", "out"): [("C", "A", "back")],
    ("C", "in"): [("A", "C", "to c")],
    ("D", "out"): [("D", "A", "from d")],
    ("D", "in"): [],
    ("E", "out"): [],
    ("E", "in"): [("B", "E", "to e")],
}


class Ctx:
    host_ref = None

    def __init__(self, url):
        self.scan = "/scan.sqlite"
        self.url = url
        self.row = {}
        self.detail = None

    @property
    def page(self):
        return {}


class FakeCore:
    """Answers each CoreJob.start synchronously, the way the real job answers through its done signal."""

    def __init__(self, page, answers=None):
        self.page = page
        self.calls = []
        self.answers = answers or {}
        page.job.start = self.start

    def start(self, arguments, token):
        self.calls.append((arguments[arguments.index("--url") + 1], arguments[arguments.index("--direction") + 1]))
        payload = self.answers.get(token[2:], None) or self.answer(arguments)
        self.page._done(token, payload)

    @staticmethod
    def answer(arguments):
        url = arguments[arguments.index("--url") + 1]
        direction = arguments[arguments.index("--direction") + 1]
        items = [{"source_url": s, "target_url": t, "anchor": a, "source_status": 200, "target_status": 200}
                 for s, t, a in LINKS.get((url, direction), [])]
        return {"ok": True, "items": items, "total": len(items), "has_more": False}


class GraphWalkTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt.app()

    def page(self, depth=1):
        page = GraphPage(Ctx("A"))
        page.depth = depth
        core = FakeCore(page)
        page.activate()
        return page, core

    def test_depth_one_draws_direct_neighbours_only(self):
        page, _core = self.page(depth=1)
        self.assertEqual(set(page.nodes), {"A", "B", "C", "D"})
        self.assertEqual(page.side["B"], "out")
        self.assertEqual(page.side["D"], "in")
        self.assertIn(("D", "A"), page.edges)
        self.assertNotIn("E", page.nodes)
        self.assertEqual(page.stack.currentIndex(), 0)

    def test_depth_two_reaches_the_next_ring_and_keeps_the_branch_side(self):
        page, _core = self.page(depth=2)
        self.assertEqual(page.nodes["E"], 2)
        self.assertEqual(page.side["E"], "out")
        self.assertEqual(page.edges[("B", "E")], "to e")

    def test_changing_depth_reuses_cached_answers(self):
        page, core = self.page(depth=2)
        before = len(core.calls)
        page.set_depth(1)
        self.assertEqual(len(core.calls), before)
        self.assertEqual(set(page.nodes), {"A", "B", "C", "D"})

    def test_node_cap_marks_the_graph_as_partial(self):
        original = url_graph.MAX_NODES
        url_graph.MAX_NODES = 3
        try:
            page, _core = self.page(depth=2)
        finally:
            url_graph.MAX_NODES = original
        self.assertLessEqual(len(page.nodes), 3)
        self.assertTrue(page.capped)

    def test_core_error_shows_the_error_state(self):
        page = GraphPage(Ctx("A"))
        page.depth = 1
        core = FakeCore(page)
        core.answer = lambda arguments: {"ok": False, "error": "no scan"}
        page.activate()
        self.assertEqual(page.stack.currentIndex(), 1)

    def test_no_links_at_all_is_an_honest_empty_state(self):
        page = GraphPage(Ctx("Z"))
        page.depth = 2
        FakeCore(page)
        page.activate()
        self.assertEqual(page.stack.currentIndex(), 1)

    def test_radial_layout_puts_the_centre_at_origin_and_depth_on_rings(self):
        page, _core = self.page(depth=2)
        page.layout_mode = "radial"
        pos = page.positions()
        self.assertEqual((pos["A"].x(), pos["A"].y()), (0, 0))
        self.assertAlmostEqual(((pos["E"].x() ** 2) + (pos["E"].y() ** 2)) ** 0.5, url_graph.RING * 2, places=6)

    def test_left_right_layout_puts_incoming_links_left(self):
        page, _core = self.page(depth=1)
        pos = page.positions()
        self.assertLess(pos["D"].x(), 0)
        self.assertGreater(pos["B"].x(), 0)


if __name__ == "__main__":
    unittest.main()


class LinkGraphScreenTests(unittest.TestCase):
    """The «Граф ссылок» section follows the URL selected on the URL screen and stays empty without one."""

    @classmethod
    def setUpClass(cls):
        cls.app = qt.app()

    def host(self, current_url):
        class UrlScreen:
            detail_data = None

        class Host:
            selected_scan_path = "/scan.sqlite"

        host = Host()
        url_screen = UrlScreen()
        url_screen.current_url = current_url
        host.screens = {"url": url_screen}
        return host, url_screen

    def test_empty_state_without_a_url(self):
        host, _ = self.host(None)
        screen = url_graph.LinkGraphScreen(host)
        screen.refresh()
        self.assertIs(screen.stack.currentWidget(), screen.empty)

    def test_follows_the_selected_url(self):
        host, url_screen = self.host("A")
        screen = url_graph.LinkGraphScreen(host)
        FakeCore(screen.graph)
        screen.refresh()
        self.assertIs(screen.stack.currentWidget(), screen.graph)
        self.assertEqual(screen.graph.nodes.get("A"), 0)
        url_screen.current_url = "B"
        screen.refresh()
        self.assertEqual(screen.graph.nodes.get("B"), 0)
        self.assertEqual(screen.graph.nodes.get("A"), 1)  # the previous centre is now a neighbour of B
