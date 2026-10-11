"""Sitemap-only native input remains a declared population, never a spider seed."""

import sqlite3
import tempfile
import unittest
from contextlib import closing
from http.server import BaseHTTPRequestHandler
from pathlib import Path

from PyQt5.QtCore import Qt, QTimer
from PyQt5.QtTest import QTest
from PyQt5.QtWidgets import (
    QCheckBox,
    QDialog,
    QLabel,
    QLineEdit,
    QPushButton,
    QToolButton,
)

from seohead_desktop.app import MainWindow
from seohead_desktop.qt import app as qt_app
from seohead_desktop.scan_runner import crawl_arguments
from tests._qt import ui
from tests.test_scan_runner import (
    close_window,
    core_cli,
    create_project,
    owned_site,
    preserve,
    snapshot,
    wait_for,
)


class _SitemapSite(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        self.server.hits.append(self.path)
        base = f"http://crawl.localhost:{self.server.server_port}"
        headers = {}
        if self.path == "/sitemap.xml":
            body = f'<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><url><loc>{base}/listed</loc></url><url><loc>{base}/listed-redirect</loc></url></urlset>'
            status, content_type = 200, "application/xml"
        elif self.path == "/robots.txt":
            body, status, content_type = "User-agent: *\nAllow: /\n", 200, "text/plain"
        elif self.path == "/listed":
            body = '<html><head><title>Listed page</title><link rel="canonical" href="/canonical-outside"></head><body><h1>Listed</h1><a href="/linked-outside">Must remain outside the population</a></body></html>'
            status, content_type = 200, "text/html"
        elif self.path == "/listed-redirect":
            body, status, content_type = "", 302, "text/html"
            headers["Location"] = "/redirect-outside"
        else:
            body, status, content_type = "This URL was not in the sitemap", 200, "text/html"
        encoded = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(encoded)))
        for key, value in headers.items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(encoded)


class SitemapPlanTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def test_spider_default_and_explicit_sitemap_source_arguments(self):
        scratch = Path(__file__).resolve().parents[1] / ".build"
        scratch.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="sitemap-plan-", dir=scratch) as temporary:
            root = Path(temporary)
            (root / "project.json").write_text("{}")
            self.assertNotIn("--sitemap-only", crawl_arguments(str(root), 10, "raw"))
            source = "https://example.test/sitemap.xml?part=a,b"
            command = crawl_arguments(str(root), 10, "raw", sitemap_url=source)
            self.assertIn("--sitemap-only", command)
            self.assertEqual(command[command.index("--sitemap") + 1], source)
            for invalid in (
                "",
                "sitemap.xml",
                "file:///tmp/sitemap.xml",
                "https://user:password@example.test/map.xml",
                "https://example.test/map.xml#fragment",
            ):
                with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                    crawl_arguments(str(root), 10, "raw", sitemap_url=invalid)

    def test_real_mainwindow_sitemap_retains_only_members_and_never_fetches_link_destinations(self):
        core = core_cli(self)
        with owned_site() as (root, server):
            server.RequestHandlerClass = _SitemapSite
            project = create_project(core, root, server)
            target = f"http://crawl.localhost:{server.server_port}"
            window = MainWindow(persistent=False, core_executable=core)
            window.show()
            try:
                window.read_project(str(project))
                wait_for(
                    self,
                    lambda: window.crawl_descriptor and not window.requests,
                    "project settings did not load",
                )
                self.assertTrue(
                    (window.crawl_descriptor.get("capabilities") or {}).get(
                        "sitemap_only_retained"
                    ),
                    "core has not advertised retained sitemap-only capture",
                )
                errors = []

                def accept_plan():
                    try:
                        dialog = self.app.activeModalWidget()
                        self.assertIsInstance(dialog, QDialog)
                        dialog.findChild(QToolButton, "scanSource_sitemap").click()
                        field = dialog.findChild(QLineEdit, "scanSitemapUrl")
                        field.setText(target + "/sitemap.xml")
                        field.textEdited.emit(target + "/sitemap.xml")
                        limit = dialog.findChild(QLineEdit, "scanUrlLimit")
                        limit.setText("10")
                        limit.textEdited.emit("10")
                        labels = "\n".join(label.text() for label in dialog.findChildren(QLabel))
                        self.assertIn(ui("По sitemap"), labels)
                        self.assertIn(ui("Только URL из sitemap.xml, без обхода ссылок"), labels)
                        self.assertEqual(
                            dialog.plan_values["address"][1].text(), target + "/sitemap.xml"
                        )
                        dialog.findChild(QCheckBox, "scanLargeApproval").click()
                        start = dialog.findChild(QPushButton, "scanStartButton")
                        self.assertTrue(start.isEnabled())
                        QTest.mouseClick(start, Qt.LeftButton)
                    except BaseException as exc:
                        errors.append(exc)
                        if self.app.activeModalWidget():
                            self.app.activeModalWidget().reject()

                QTimer.singleShot(100, accept_plan)
                QTest.mouseClick(window.new_scan, Qt.LeftButton)
                if errors:
                    raise errors[0]
                wait_for(
                    self,
                    lambda: (
                        window.scan_manager
                        and window.scan_manager.snapshot()[0]["state"]
                        in {"finished", "partial", "failed", "rejected", "status_unavailable"}
                    ),
                    lambda: str(
                        window.scan_manager.snapshot() if window.scan_manager else "No launch"
                    ),
                    timeout=60,
                )
                row = window.scan_manager.snapshot()[0]
                if row["state"] != "finished":
                    preserve(
                        root,
                        "sitemap-failed",
                        {"run": window.scan_manager.detail(row["id"]), "requests": server.hits},
                        window,
                    )
                self.assertEqual(row["state"], "finished", window.scan_manager.detail(row["id"]))
                self.assertEqual(row["input_mode"], "sitemap")
                self.assertEqual(row["sitemap_url"], target + "/sitemap.xml")
                self.assertEqual(row["observer_run_id"], row["core_run_id"])
                stored = snapshot(row["artifact"])
                self.assertEqual(stored["scan"]["lifecycle"], "finished")
                with closing(
                    sqlite3.connect(Path(row["artifact"]).as_uri() + "?mode=ro", uri=True)
                ) as con:
                    pages = dict(
                        con.execute("SELECT url,status_code FROM pages JOIN urls USING(url_id)")
                    )
                self.assertEqual(pages, {target + "/listed": 200, target + "/listed-redirect": 302})
                self.assertNotIn(
                    "/", server.hits, "project root was silently added to the sitemap population"
                )
                for path in ("/linked-outside", "/canonical-outside", "/redirect-outside"):
                    self.assertNotIn(
                        path, server.hits, "an out-of-population destination was fetched"
                    )
                preserve(
                    root,
                    "sitemap-only",
                    {"run": row, "pages": pages, "requests": server.hits, "scan": stored},
                    window,
                )
            finally:
                close_window(self, window)


if __name__ == "__main__":
    unittest.main()
