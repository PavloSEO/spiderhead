"""Bounded immutable-package presentation and actual offline MCP comparison."""

import hashlib
import json
import os
import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from PyQt5.QtCore import QObject, Qt, QThreadPool, pyqtSignal
from PyQt5.QtTest import QTest

from seohead_desktop.comparison import ComparisonController, comparison_page, retained_pair
from seohead_desktop.mcp_gateway import PersistentMcpGateway
from seohead_desktop.qt import app as qt_app
from tests.test_scan_runner import close_window, core_cli, wait_for


class ComparisonTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        scratch = Path(__file__).resolve().parents[1] / ".build"
        scratch.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="comparison-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.project = Path(self.temporary.name)
        (self.project / "project.json").write_text("{}")
        (self.project / "scans").mkdir()
        self.pair = []
        for name in ("before", "after"):
            path = self.project / "scans" / f"{name}.sqlite"
            path.write_bytes(b"synthetic path fixture")
            self.pair.append({"uuid": name, "path": str(path)})

    def package(self, count=250):
        directory = self.project / "reports" / "pair" / "delta"
        directory.mkdir(parents=True)
        files = {}
        for kind in ("left", "entered", "appeared", "unchanged", "disappeared"):
            path = directory / f"{kind}.ndjson"
            with path.open("w") as handle:
                for index in range(count if kind == "left" else 0):
                    handle.write(json.dumps({"id": f"issue-{index}", "check": "TITLE_MISSING", "target_url": f"https://example.test/{index}", "message": f"Missing title {index}"}) + "\n")
            files[kind] = {"path": path.name, "format": "ndjson", "rows": count if kind == "left" else 0, "bytes": path.stat().st_size}
        (directory / "compare.json").write_text(json.dumps({"schema_version": "compare.v2", "files": files, "summary": {"left": count}, "compatibility": [{"basis": "scope", "state": "unknown"}], "warnings": ["Unknown population"]}))
        return directory

    def test_paging_never_turns_a_missing_finding_into_a_fix(self):
        package = self.package()
        first = comparison_page(package, self.project, offset=100, limit=100)
        self.assertEqual(first["total"], 250)
        self.assertEqual(len(first["rows"]), 100)
        self.assertEqual(first["rows"][0]["url"], "https://example.test/100")
        self.assertEqual({row["state"] for row in first["rows"]}, {"not_verifiable"})
        self.assertTrue(first["has_more"])
        last = comparison_page(package, self.project, offset=200)
        self.assertEqual(len(last["rows"]), 50)
        self.assertFalse(last["has_more"])
        self.assertEqual(last["compatibility"], [{"basis": "scope", "state": "unknown"}])
        with self.assertRaises(ValueError):
            comparison_page(package, self.project, limit=101)

    def test_scope_identical_pair_and_partition_escape_are_refused(self):
        retained_pair(self.project, *self.pair)
        with self.assertRaises(ValueError):
            retained_pair(self.project, self.pair[0], self.pair[0])
        outside = self.project / "outside.sqlite"
        outside.write_bytes(b"outside")
        with self.assertRaises(ValueError):
            retained_pair(self.project, self.pair[0], {"uuid": "outside", "path": str(outside)})
        package = self.package(1)
        manifest = package / "compare.json"
        doc = json.loads(manifest.read_text())
        doc["files"]["left"]["path"] = "../escape.ndjson"
        manifest.write_text(json.dumps(doc))
        with self.assertRaises(ValueError):
            comparison_page(package, self.project)

    def test_gateway_requires_offline_after_and_immutable_project_output(self):
        gateway = PersistentMcpGateway("/unused")
        gateway.set_project_scope(str(self.project))
        args = {"baseline": self.pair[0]["path"], "after": self.pair[1]["path"], "finding_ids": ["issue"], "out_dir": str(self.project / "reports" / "new")}
        gateway._validate("seo_verify_fixes", args)
        for change in ({"after": None}, {"out_dir": str(self.project / "outside")}, {"finding_ids": ["x"] * 101}, {"config": "secret"}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                gateway._validate("seo_verify_fixes", {**args, **change})
        compare = {"before": self.pair[0]["path"], "after": self.pair[1]["path"], "out_dir": str(self.project / "reports" / "new"), "force": True}
        with self.assertRaises(ValueError):
            gateway._validate("seo_compare_crawls", compare)

    def test_superseded_pair_results_and_errors_cannot_replace_new_selection(self):
        class Signals(QObject):
            failed = pyqtSignal(str, str, int)
        class Gateway:
            signals = Signals()
        class Window(QObject):
            def __init__(self, project):
                super().__init__()
                self.project_directory = str(project)
                self.read_generation = 1
                self.pool = QThreadPool(self)
                self.commands = []
            def ensure_mcp_gateway(self):
                return Gateway
            def start_command(self, *args):
                self.commands.append(args)
        window = Window(self.project)
        controller = ComparisonController(window)
        observed = []
        controller.changed.connect(observed.append)
        controller.start(*self.pair)
        old_request, _tool, _args, old_handler = window.commands[-1]
        controller.start(self.pair[1], self.pair[0])
        count = len(observed)
        old_handler({"schema_version": "compare.v2"})
        Gateway.signals.failed.emit(old_request, "old failure", 1)
        self.assertEqual(len(observed), count)
        window.read_generation += 1
        new_handler = window.commands[-1][-1]
        new_handler({"schema_version": "compare.v2"})
        self.assertEqual(len(observed), count)
        self.assertFalse(controller._worker)

    def test_real_mainwindow_compare_and_verify_without_network_or_source_mutation(self):
        core = core_cli(self)
        directory = os.environ.get("SEOHEAD_DESKTOP_COMPARE_PROJECT")
        names = [os.environ.get("SEOHEAD_DESKTOP_COMPARE_BEFORE"), os.environ.get("SEOHEAD_DESKTOP_COMPARE_AFTER")]
        if not directory or not all(names):
            self.skipTest("provide an existing same-project before/after pair for the offline MCP gate")
        from seohead_desktop.app import MainWindow
        project = Path(directory)
        scans = []
        hashes = {}
        for name in names:
            path = project / "scans" / name
            with closing(sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True)) as con:
                identity = con.execute("SELECT scan_uuid FROM scan").fetchone()[0]
            scans.append({"path": str(path), "uuid": identity})
            hashes[path] = hashlib.sha256(path.read_bytes()).hexdigest()
        window = MainWindow(persistent=False, core_executable=core)
        window.show()
        try:
            window.read_project(str(project))
            wait_for(self, lambda: window.project_directory and not window.requests, "project did not load")
            self.assertTrue(hasattr(window, "comparison"), "MainWindow compare hook is not connected")
            states = []
            window.comparison.changed.connect(states.append)
            window.open_comparison()
            panel = window.project_panels.panel("compare")
            for combo, scan in zip((panel.before, panel.after), scans):
                index = next(index for index in range(combo.count()) if (combo.itemData(index) or {}).get("uuid") == scan["uuid"])
                combo.setCurrentIndex(index)
            self.assertTrue(panel.compare_button.isEnabled())
            QTest.mouseClick(panel.compare_button, Qt.LeftButton)
            wait_for(self, lambda: states and states[-1]["state"] in {"ready", "unavailable"}, lambda: str(states[-1] if states else "No comparison state"), timeout=60)
            result = states[-1]
            self.assertEqual(result["state"], "ready", result)
            self.assertLessEqual(len(result["rows"]), 100)
            outcomes = {row["state"] for row in result["rows"]}
            self.assertIn("resolved", outcomes)
            self.assertIn("not_verifiable", outcomes)
            self.assertTrue(outcomes & {"changed", "persisting"})
            self.assertTrue(all(item["state"] == "compatible" for item in result["compatibility"]), result["compatibility"])
            self.assertGreater(result["summary"]["verified_page"]["resolved"], 0)
            for path, digest in hashes.items():
                self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), digest)
            first_rows = [(row["url"], row["check"]) for row in result["rows"]]
            QTest.mouseClick(panel.next_button, Qt.LeftButton)
            wait_for(self, lambda: states[-1]["state"] == "ready" and states[-1].get("offset") == 100, "comparison second page did not load")
            second = states[-1]
            self.assertEqual(len(second["rows"]), 45)
            self.assertFalse(second["has_more"])
            self.assertFalse(set(first_rows) & {(row["url"], row["check"]) for row in second["rows"]})
            QTest.mouseClick(panel.previous_button, Qt.LeftButton)
            wait_for(self, lambda: states[-1]["state"] == "ready" and states[-1].get("offset") == 0, "comparison previous page did not reload")
            report = Path(result["package"]).parent
            (report / "desktop-proof-page2.json").write_text(json.dumps(second, ensure_ascii=False, indent=2))
            (report / "desktop-proof.json").write_text(json.dumps(result, ensure_ascii=False, indent=2))
            window.grab().save(str(report / "desktop-comparison.png"))
        finally:
            close_window(self, window)


if __name__ == "__main__":
    unittest.main()
