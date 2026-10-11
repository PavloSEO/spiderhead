"""Real SDK/stdio lifecycle tests using owned offline JSON-RPC peers."""

import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

from PyQt5.QtCore import Qt

from seohead_desktop.mcp_gateway import TOOL_ALLOWLIST, PersistentMcpGateway
from seohead_desktop.qt import app as qt_app

PEER = r'''
import json
import os
from pathlib import Path
import signal
import sys
import time

root = Path(__file__).parent
config = json.loads((root / "peer.json").read_text())
mode = config["mode"]
if hasattr(signal, "SIGTERM"):
    signal.signal(signal.SIGTERM, signal.SIG_IGN)

def record(event, **fields):
    with (root / "events.jsonl").open("a") as stream:
        stream.write(json.dumps({"event": event, "pid": os.getpid(), **fields}) + "\n")

def block(phase):
    record("blocked", phase=phase)
    while not (root / "release").exists():
        time.sleep(0.01)

def first():
    marker = root / "claimed"
    if marker.exists():
        return False
    marker.touch()
    return True

record("started", ppid=os.getppid())
for line in sys.stdin:
    request = json.loads(line)
    identifier = request.get("id")
    if identifier is None:
        continue
    method = request["method"]
    if method == "initialize":
        if mode == "hang_initialize" or (mode == "first_initialize" and first()):
            block(method)
        result = {"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {"tools": {}},
                  "serverInfo": {"name": "owned-lifecycle-peer", "version": "1"}}
    elif method == "tools/list":
        if mode == "hang_list":
            block(method)
        result = {"tools": [{"name": name, "inputSchema": {"type": "object"}}
                            for name in config["tools"]]}
    elif method == "tools/call":
        name = request["params"]["name"]
        record("call", tool=name)
        if mode == "hang_call" or (mode == "first_call" and first()):
            block(name)
        if name == "seo_project_inbox_submit":
            record("note_written", text=request["params"]["arguments"].get("text"))
        if mode == "disconnect" and first():
            os._exit(0)
        if mode == "hold_write" and name == "seo_project_inbox_submit":
            block(name)
        if name == config.get("delay_tool"):
            time.sleep(config["delay"])
        data = {"ok": True, "pid": os.getpid(), "tool": name}
        result = {"content": [{"type": "text", "text": json.dumps(data)}],
                  "structuredContent": data, "isError": False}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": identifier, "result": result}), flush=True)
'''


def process_exists(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


class McpGatewayLifecycleTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        scratch = Path(__file__).resolve().parents[1] / ".build"
        scratch.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="mcp-lifecycle-", dir=scratch)
        self.root = Path(self.temporary.name)
        self.gateway = self.worker = None
        self.results, self.failures, self.transports, self.ready, self.stopped = [], [], [], [], []
        self.worker_errors = []

    def tearDown(self):
        # Release only this peer so even a failed regression remains bounded.
        (self.root / "release").touch()
        if self.gateway is not None:
            self.gateway.stop()
        if self.worker is not None:
            self.worker.join(4)
            self.assertFalse(self.worker.is_alive(), "owned gateway worker did not exit")
        self.assertFalse(self.worker_errors, self.worker_errors)
        for event in self.events("started"):
            self.assertEqual(event["ppid"], os.getpid())
            self.assertFalse(process_exists(event["pid"]), "owned MCP peer was not reaped")
        self.temporary.cleanup()

    def events(self, kind):
        path = self.root / "events.jsonl"
        if not path.exists():
            return []
        events = []
        for line in path.read_text().splitlines():
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue  # The peer may still be completing its final log write.
            if event["event"] == kind:
                events.append(event)
        return events

    def wait_for(self, predicate, message, timeout=5):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(0.01)
        self.fail(f"{message}; failures={self.failures}; transports={self.transports}")

    def start(self, mode, *, read_timeout=2, startup_timeout=3, tool_timeouts=None, **peer_config):
        peer = self.root / "peer.py"
        peer.write_text(f"#!{sys.executable}\n" + PEER)
        peer.chmod(0o700)
        (self.root / "peer.json").write_text(json.dumps({
            "mode": mode, "tools": sorted(TOOL_ALLOWLIST), **peer_config,
        }))
        self.gateway = PersistentMcpGateway(
            str(peer), startup_timeout=startup_timeout, read_timeout=read_timeout,
            tool_timeouts=tool_timeouts,
        )
        for signal, target in (
            (self.gateway.signals.result, self.results),
            (self.gateway.signals.failed, self.failures),
            (self.gateway.signals.transport_failed, self.transports),
            (self.gateway.signals.ready, self.ready),
            (self.gateway.signals.stopped, self.stopped),
        ):
            signal.connect(lambda *args, target=target: target.append(args), Qt.DirectConnection)

        def run():
            try:
                self.gateway.run()
            except BaseException as exc:
                self.worker_errors.append(repr(exc))

        self.worker = threading.Thread(target=run, daemon=True)
        self.worker.start()
        return self.gateway

    def submit(self, identifier="read", generation=1):
        self.gateway.submit(identifier, "seo_crawl_describe_settings", {}, generation)

    def assert_stops(self):
        started = time.monotonic()
        self.gateway.stop()
        self.worker.join(1)
        self.assertFalse(self.worker.is_alive(), "stop waited for a silent core")
        self.assertLess(time.monotonic() - started, 1)
        self.assertEqual(len(self.stopped), 1)
        self.assertFalse(self.results)

    def assert_hung_shutdown(self, mode):
        sentinel = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(30)"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        try:
            self.start(mode)
            self.submit()
            self.wait_for(lambda: self.events("blocked"), "peer did not enter controlled hang")
            self.assert_stops()
            self.assertIsNone(sentinel.poll(), "shutdown touched an external process")
            self.assertFalse(self.failures)
        finally:
            sentinel.terminate()
            sentinel.wait(timeout=2)

    def test_shutdown_interrupts_initialize_preserving_external_process(self):
        self.assert_hung_shutdown("hang_initialize")

    def test_shutdown_interrupts_list_preserving_external_process(self):
        self.assert_hung_shutdown("hang_list")

    def test_shutdown_interrupts_call_preserving_external_process(self):
        self.assert_hung_shutdown("hang_call")

    def test_startup_deadline_recovers_and_keeps_pending_request(self):
        self.start("first_initialize", startup_timeout=1.5)
        self.submit()
        self.wait_for(lambda: self.results, "startup deadline did not reconnect", timeout=6)
        self.assertEqual(self.results[0][0], "read")
        self.assertTrue(any("startup timed out" in error[0] for error in self.transports))
        self.assertEqual(len(self.events("started")), 2)
        self.assertFalse(process_exists(self.events("started")[0]["pid"]))
        self.assertFalse(self.failures)

    def test_call_deadline_reaps_peer_and_runs_new_pending_intent(self):
        self.start("first_call", read_timeout=0.2)
        self.submit("expired")
        self.wait_for(lambda: self.events("blocked"), "first call did not block")
        self.submit("latest", 2)
        self.wait_for(lambda: self.results, "read deadline left newer intent stuck")
        self.assertEqual([item[0] for item in self.results], ["latest"])
        self.assertEqual([item[0] for item in self.failures], ["expired"])
        self.assertIn("timed out", self.failures[0][1])
        self.assertEqual(len(self.ready), 2)
        self.assertFalse(process_exists(self.events("started")[0]["pid"]))

    def test_cancel_generation_aborts_active_read_and_drops_old_pending_payloads(self):
        self.start("first_call", read_timeout=30)
        self.submit("stale-active", 7)
        self.wait_for(lambda: self.events("blocked"), "first call did not block")
        self.submit("stale-pending", 7)
        self.gateway.cancel_generation(7)
        self.submit("latest", 8)
        self.wait_for(lambda: self.results, "cancelled read blocked latest generation")
        self.assertEqual([(item[0], item[2]) for item in self.results], [("latest", 8)])
        self.assertEqual(len(self.events("call")), 2)
        self.assertFalse(self.failures)
        self.assertFalse(self.transports)
        self.assertFalse(process_exists(self.events("started")[0]["pid"]))

    def test_changing_scope_aborts_old_read_before_next_project_result(self):
        self.start("first_call", read_timeout=30)
        projects = [self.root / name for name in ("old", "new")]
        for project in projects:
            project.mkdir()
            (project / "project.json").write_text("{}")
        self.gateway.set_project_scope(str(projects[0]))
        self.gateway.submit("old", "seo_project_observe", {"directory": str(projects[0])}, 1)
        self.wait_for(lambda: self.events("blocked"), "old project read did not block")
        self.gateway.set_project_scope(str(projects[1]))
        self.gateway.submit("new", "seo_project_observe", {"directory": str(projects[1])}, 2)
        self.wait_for(lambda: self.results, "old project read blocked new project")
        self.assertEqual([item[0] for item in self.results], ["new"])
        self.assertFalse(self.failures)

    def test_tool_override_allows_slow_comparison_and_keeps_one_session(self):
        gateway = self.start(
            "slow", read_timeout=0.1, tool_timeouts={"seo_compare_crawls": 3},
            delay_tool="seo_compare_crawls", delay=1.1,
        )
        (self.root / "project.json").write_text("{}")
        scans = self.root / "scans"
        scans.mkdir()
        for name in ("before", "after"):
            (scans / name).touch()
        gateway.set_project_scope(str(self.root))
        gateway.submit("compare", "seo_compare_crawls", {
            "before": str(scans / "before"), "after": str(scans / "after"),
            "out_dir": str(self.root / "reports" / "compare"),
        }, 1)
        self.wait_for(lambda: self.results, "comparison inherited short read timeout")
        self.submit("next-read")
        self.wait_for(lambda: len(self.results) == 2, "persistent session was not reusable")
        self.assertEqual([item[0] for item in self.results], ["compare", "next-read"])
        self.assertEqual(len(self.events("started")), 1)
        self.assertGreater(PersistentMcpGateway("unused").tool_timeouts["seo_compare_crawls"], 60)
        self.assertFalse(self.failures)

    def test_generation_cancel_does_not_interrupt_explicit_note_write(self):
        self.start("hold_write")
        (self.root / "project.json").write_text("{}")
        self.gateway.set_project_scope(str(self.root))
        self.gateway.submit("note", "seo_project_inbox_submit", {"directory": str(self.root)}, 1)
        self.wait_for(lambda: self.events("blocked"), "note write did not block")
        self.gateway.cancel_generation(1)
        self.submit("latest", 2)
        time.sleep(0.1)
        self.assertTrue(process_exists(self.events("started")[0]["pid"]))
        (self.root / "release").touch()
        self.wait_for(lambda: self.results, "new read did not follow completed write")
        self.assertEqual([item[0] for item in self.results], ["latest"])
        self.assertEqual(len(self.events("started")), 1)
        self.assertFalse(self.failures)

    def test_disconnected_peer_is_replaced_without_replaying_failed_call(self):
        self.start("disconnect")
        self.submit("disconnected")
        self.submit("latest", 2)
        self.wait_for(lambda: self.results, "disconnected peer was not replaced")
        self.assertEqual([item[0] for item in self.results], ["latest"])
        self.assertEqual([item[0] for item in self.failures], ["disconnected"])
        self.assertEqual(len(self.events("call")), 2)

    def test_note_accepted_before_disconnect_is_not_replayed_without_acknowledgement(self):
        self.start("disconnect")
        (self.root / "project.json").write_text("{}")
        self.gateway.set_project_scope(str(self.root))
        self.gateway.submit("unconfirmed-note", "seo_project_inbox_submit", {
            "directory": str(self.root), "text": "Synthetic note", "kind": "note",
            "author_role": "specialist", "expected_revision": 0,
        }, 1)
        self.submit("latest", 2)
        self.wait_for(lambda: self.results, "peer did not recover after losing the write acknowledgement")
        self.assertEqual([item[0] for item in self.results], ["latest"])
        self.assertEqual([item[0] for item in self.failures], ["unconfirmed-note"])
        self.assertEqual([item["text"] for item in self.events("note_written")], ["Synthetic note"])
        self.assertEqual(len(self.events("started")), 2)

    def test_stop_during_reconnect_backoff_and_before_start_is_idempotent(self):
        self.start("hang_initialize", startup_timeout=0.2)
        self.wait_for(lambda: self.transports, "startup deadline was not enforced")
        self.assert_stops()
        gateway = PersistentMcpGateway("unused")
        stopped = []
        gateway.signals.stopped.connect(lambda: stopped.append(True))
        gateway.stop()
        gateway.stop()
        gateway.run()
        self.assertEqual(stopped, [True])
        with self.assertRaises(RuntimeError):
            gateway.submit("late", "seo_crawl_describe_settings", {}, 1)

    def test_deadlines_reject_unbounded_or_unknown_values(self):
        for value in (0, -1, float("inf"), float("nan"), True):
            with self.subTest(value=value), self.assertRaises(ValueError):
                PersistentMcpGateway("unused", read_timeout=value)
        with self.assertRaises(ValueError):
            PersistentMcpGateway("unused", tool_timeouts={"not-a-tool": 1})


if __name__ == "__main__":
    unittest.main()
