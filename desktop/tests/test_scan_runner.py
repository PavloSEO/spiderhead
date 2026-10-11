"""Real owned-loopback acceptance for CLI checkpoint and Qt launch control."""

import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import threading
import time
import unittest
import uuid
from contextlib import closing, contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from PyQt5.QtCore import Qt, QTimer
from PyQt5.QtTest import QTest
from PyQt5.QtWidgets import QApplication, QCheckBox, QDialog, QPushButton

from seohead_desktop.app import MainWindow
from seohead_desktop.qt import app as qt_app
from seohead_desktop.scan_runner import LocalScanProcess, crawl_arguments


class _SlowSite(BaseHTTPRequestHandler):
    delay = 0.2

    def do_GET(self):
        self.server.hits.append(self.path)
        if self.path == "/robots.txt":
            body, media, status = "User-agent: *\nAllow: /\n", "text/plain", 200
        elif self.path == "/" or self.path.startswith("/page-"):
            if self.path != "/":
                time.sleep(self.delay)
            links = "".join(f'<a href="/page-{n}">Page {n}</a>' for n in range(6)) if self.path == "/" else ""
            body = f'<html><head><title>{self.path}</title></head><body><nav>NOISE_NAVIGATION</nav><main><p>OUTSIDE_REGION</p><section id="content"><h1>KEPT_CONTENT</h1><span class="omit">OMITTED_SELECTOR_ONLY</span>{links}</section></main></body></html>'
            media, status = "text/html; charset=utf-8", 200
        else:
            body, media, status = "Not found", "text/plain", 404
        encoded = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", media)
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        try:
            self.wfile.write(encoded)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *_args):
        pass


def core_cli(case):
    core = os.environ.get("SEOHEAD_DESKTOP_CORE_CLI")
    if not core or not Path(core).is_file():
        case.skipTest("set SEOHEAD_DESKTOP_CORE_CLI to run the owned-loopback gate")
    return core


def wait_for(case, predicate, message, timeout=45):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        QApplication.processEvents()
        if predicate():
            return
        QTest.qWait(25)
    case.fail(message() if callable(message) else message)


@contextmanager
def owned_site():
    previous = os.environ.get("SEOHEAD_ALLOW_PRIVATE_HOSTS")
    os.environ["SEOHEAD_ALLOW_PRIVATE_HOSTS"] = "crawl.localhost,127.0.0.1"
    server = ThreadingHTTPServer(("127.0.0.1", 0), _SlowSite)
    server.hits = []
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    scratch = Path(__file__).resolve().parents[1] / ".build"
    scratch.mkdir(exist_ok=True)
    try:
        with tempfile.TemporaryDirectory(prefix="owned-desktop-", dir=scratch) as temporary:
            yield Path(temporary), server
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        if previous is None:
            os.environ.pop("SEOHEAD_ALLOW_PRIVATE_HOSTS", None)
        else:
            os.environ["SEOHEAD_ALLOW_PRIVATE_HOSTS"] = previous


def create_project(core, root, server):
    project = root / "project"
    subprocess.run(
        [core, "project-new", "--directory", str(project), "--target", f"http://crawl.localhost:{server.server_port}/"],
        check=True, capture_output=True, text=True, timeout=20,
    )
    return project


def snapshot(scan):
    if not Path(scan).is_file():
        return {}
    with closing(sqlite3.connect(Path(scan).resolve().as_uri() + "?mode=ro", uri=True)) as con:
        con.row_factory = sqlite3.Row
        try:
            row = con.execute("SELECT * FROM scan").fetchone()
            if row is None:
                return {}
            return {"scan": dict(row), "pages": con.execute("SELECT COUNT(*) FROM pages").fetchone()[0]}
        except sqlite3.OperationalError as exc:
            if "no such table" in str(exc):
                return {}
            raise


def retained_pages(project):
    return {scan: snapshot(scan) for scan in (project / "scans").glob("*.sqlite") if snapshot(scan).get("pages", 0)}


def preserve(root, name, receipt, window=None):
    output = os.environ.get("SEOHEAD_DESKTOP_ACCEPTANCE_DIR")
    if not output:
        return
    destination = Path(output) / f"{name}-{root.name}"
    shutil.copytree(root, destination)
    (destination / "receipt.json").write_text(json.dumps(receipt, ensure_ascii=False, indent=2))
    if window is not None:
        window.grab().save(str(destination / "window.png"))


def close_window(case, window):
    window.close()
    wait_for(case, lambda: not window.isVisible(), "window did not finish owned shutdown", timeout=35)
    case.assertTrue(window.pool.waitForDone(10000), "MCP worker remained active after closing")


class LocalScanRunnerTests(unittest.TestCase):
    def test_full_site_arguments_disable_user_budgets_and_keep_selected_speed(self):
        with tempfile.TemporaryDirectory() as directory:
            project = Path(directory)
            (project / "project.json").write_text("{}")
            args = crawl_arguments(str(project), 0, "raw", max_urls_per_second=10.0)
            self.assertEqual(args[args.index("--max-urls") + 1], "0")
            overrides = json.loads(args[args.index("--input") + 1])["overrides"]
            self.assertEqual(overrides["limits.max_depth"], -1)
            self.assertEqual(overrides["limits.max_requests"], 0)
            self.assertEqual(overrides["limits.max_crawl_seconds"], 0)
            self.assertEqual(overrides["speed.min_delay_seconds"], 0.1)
            self.assertEqual(overrides["storage.min_free_bytes"], 12 * 1024**3)

    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def test_raw_and_js_arguments_preserve_typed_overrides_in_real_cli(self):
        core = core_cli(self)
        scratch = Path(__file__).resolve().parents[1] / ".build"
        scratch.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="typed-cli-", dir=scratch) as temporary:
            project = Path(temporary)
            (project / "project.json").write_text("{}")
            for mode in ("raw", "js"):
                with self.subTest(mode=mode):
                    arguments = crawl_arguments(
                        str(project), 21, mode,
                        overrides=(("limits.max_requests", 31), ("limits.max_crawl_seconds", 41),
                                   ("scope.include_patterns", ["page-[0,12]$"]),
                                   ("evidence.content_area.include_selector", "main"),
                                   ("speed.min_delay_seconds", 0.75)),
                    )
                    code = (
                        "import json,sys; from seohead.cli import build_parser,_build_kwargs; "
                        "args=build_parser().parse_args(json.loads(sys.argv[1])); "
                        "print(json.dumps(_build_kwargs(args.command,args)[1]))"
                    )
                    result = json.loads(subprocess.run(
                        [str(Path(core).parent / "python"), "-c", code, json.dumps(arguments)],
                        check=True, capture_output=True, text=True, timeout=20,
                    ).stdout)
                    self.assertEqual(result["max_urls"], 21)
                    self.assertEqual(result["project"], str(project))
                    self.assertEqual(result["overrides"], {
                        "rendering.mode": mode, "limits.max_requests": 31,
                        "limits.max_crawl_seconds": 41, "scope.include_patterns": ["page-[0,12]$"],
                        "evidence.content_area.include_selector": "main", "speed.min_delay_seconds": 0.75,
                    })

    def test_owned_loopback_capture_interrupt_and_resume(self):
        core = core_cli(self)
        with owned_site() as (root, server):
            project = create_project(core, root, server)
            runner = LocalScanProcess(core)
            try:
                runner.start(str(project), 20, "raw", (("limits.max_requests", 100), ("limits.max_crawl_seconds", 60)), observer_run_id=str(uuid.uuid4()))
                wait_for(self, lambda: retained_pages(project), "crawl never retained its start page")
                scan = next(iter(retained_pages(project)))
                runner.request_stop()
                wait_for(self, lambda: not runner.active, "owned crawl did not stop")
                interrupted = snapshot(scan)
                self.assertEqual(interrupted["scan"]["lifecycle"], "interrupted")
                self.assertEqual(interrupted["scan"]["crawl_partial"], 1)
                self.assertGreaterEqual(interrupted["pages"], 1)
                root_hits = server.hits.count("/")
                runner.resume(str(scan), str(project), str(uuid.uuid4()))
                wait_for(self, lambda: not runner.active, "resume did not finish")
                completed = snapshot(scan)
                self.assertEqual(completed["scan"]["lifecycle"], "finished")
                self.assertEqual(completed["pages"], 7)
                self.assertEqual(server.hits.count("/"), root_hits, "resume refetched retained start page")
                preserve(root, "runner-resume", {"interrupted": interrupted, "completed": completed})
            finally:
                if runner.active:
                    runner.request_stop()
                    wait_for(self, lambda: not runner.active, "fixture child remained active", timeout=30)

    def test_gui_dialog_stops_with_real_checkpoint_and_final_observer(self):
        core = core_cli(self)
        with owned_site() as (root, server):
            project = create_project(core, root, server)
            window = MainWindow(persistent=False, core_executable=core)
            window.show()
            try:
                window.read_project(str(project))
                wait_for(self, lambda: window.crawl_descriptor and window.project_directory and not window.requests, "project MCP did not load")
                errors = []

                def accept_plan():
                    try:
                        dialog = self.app.activeModalWidget()
                        self.assertIsInstance(dialog, QDialog)
                        self.assertEqual((dialog.draft.value("limits.max_requests"), dialog.draft.value("limits.max_crawl_seconds")), (0, 0))
                        dialog.findChild(QCheckBox, "scanLargeApproval").click()
                        QTest.mouseClick(dialog.findChild(QPushButton, "scanStartButton"), Qt.LeftButton)
                    except BaseException as exc:
                        errors.append(exc)
                        if self.app.activeModalWidget():
                            self.app.activeModalWidget().reject()

                QTimer.singleShot(100, accept_plan)
                QTest.mouseClick(window.new_scan, Qt.LeftButton)
                if errors:
                    raise errors[0]
                wait_for(self, lambda: retained_pages(project), "GUI crawl never retained a page")
                run_id = window.selected_managed_run_id
                window.cancel_active_work()
                wait_for(self, lambda: window.scan_manager.detail(run_id)["state"] == "partial", lambda: json.dumps(window.scan_manager.detail(run_id)))
                self.assertEqual(window.scan_manager.active_count, 0)
                scan = Path(window.scan_manager.detail(run_id)["artifact"])
                self.assertEqual(snapshot(scan)["scan"]["lifecycle"], "interrupted")
                wait_for(self, lambda: window.resume_scan_button.isEnabled(), "retained interrupted scan did not offer resume")
                preserve(root, "gui-stop", window.scan_manager.detail(run_id), window)
            finally:
                close_window(self, window)


if __name__ == "__main__":
    unittest.main()
