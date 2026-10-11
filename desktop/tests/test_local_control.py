"""Real user-local socket and in-memory MCP tests; no core or crawler process."""

import asyncio
import json
import os
import stat
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

from PyQt5.QtCore import QThread
from PyQt5.QtNetwork import QLocalServer, QLocalSocket

from seohead_desktop.control_cli import create_mcp_server
from seohead_desktop.local_control import (
    MAX_REQUEST_BYTES,
    MAX_RESPONSE_BYTES,
    PROTOCOL,
    ControlError,
    DesktopControlServer,
    prepare_endpoint,
    read_endpoint,
    request,
    validate_arguments,
)
from seohead_desktop.qt import app as qt_app


class LocalControlTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        directory = os.environ.get("SEOHEAD_DESKTOP_TEST_TMPDIR")
        self.temporary = tempfile.TemporaryDirectory(prefix="ctl-", dir=directory)
        self.root = Path(self.temporary.name).resolve()
        self.endpoint = prepare_endpoint(self.root)
        self.calls = []

        def dispatch(operation, arguments):
            self.assertEqual(QThread.currentThread(), self.app.thread())
            self.calls.append((operation, arguments))
            if operation == "stop_run" and arguments["run_id"] != "owned-run":
                raise ControlError(
                    "not_owned", "Only this Desktop's owned runs can be stopped"
                )
            if operation == "new_scan":
                return {"state": "queued", "run_id": "owned-run"}
            return {"operation": operation, "arguments": arguments, "state": "ready"}

        self.server = DesktopControlServer(self.endpoint, dispatch)
        self.server.start()

    def tearDown(self):
        self.server.close()
        self.app.processEvents()
        self.temporary.cleanup()

    def worker(self, function):
        result, failure = [], []

        def run():
            try:
                result.append(function())
            except Exception as exc:
                failure.append(exc)

        thread = threading.Thread(target=run)
        thread.start()
        deadline = time.monotonic() + 12
        while thread.is_alive() and time.monotonic() < deadline:
            self.app.processEvents()
            time.sleep(0.002)
        thread.join(0.1)
        self.assertFalse(thread.is_alive(), "Bounded IPC worker did not finish")
        if failure:
            raise failure[0]
        return result[0]

    def call(self, operation="status", arguments=None, **kwargs):
        return self.worker(
            lambda: request(
                self.endpoint.descriptor_path, operation, arguments, **kwargs
            )
        )

    def frame(self, **changes):
        value = {
            "protocol": PROTOCOL,
            "instance_id": self.endpoint.instance_id,
            "token": self.endpoint.token,
            "id": "probe",
            "operation": "status",
            "arguments": {},
        }
        value.update(changes)
        return json.dumps(value).encode() + b"\n"

    def raw(self, data, *, timeout_ms=1500):
        def send():
            socket = QLocalSocket()
            socket.connectToServer(self.endpoint.socket_name)
            self.assertTrue(socket.waitForConnected(timeout_ms))
            socket.write(data)
            if socket.bytesToWrite():
                socket.waitForBytesWritten(timeout_ms)
            received = bytearray()
            deadline = time.monotonic() + timeout_ms / 1000
            while b"\n" not in received and time.monotonic() < deadline:
                received.extend(bytes(socket.readAll()))
                if b"\n" not in received:
                    socket.waitForReadyRead(
                        max(1, int((deadline - time.monotonic()) * 1000))
                    )
            socket.abort()
            return json.loads(received)

        return self.worker(send)

    def test_real_socket_dispatches_in_qt_thread_and_no_core_is_created(self):
        self.assertEqual(self.call()["operation"], "status")
        self.assertEqual(self.calls, [("status", {})])
        self.assertFalse(hasattr(self.server, "gateway"))
        self.assertFalse(hasattr(self.server, "scan_manager"))

    def test_private_metadata_and_cleanup_preserve_unrelated_files(self):
        path = self.endpoint.descriptor_path
        if os.name != "nt":
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            self.assertEqual(stat.S_IMODE(path.parent.stat().st_mode), 0o700)
        self.assertEqual(read_endpoint(path).instance_id, self.endpoint.instance_id)
        self.assertNotIn(self.endpoint.token, repr(self.endpoint))
        unrelated = path.parent / "keep.txt"
        unrelated.write_text("keep")
        self.server.close()
        self.assertFalse(path.exists())
        self.assertEqual(unrelated.read_text(), "keep")

    def test_existing_directory_permissions_unchanged_and_symlink_rejected(self):
        old_mode = stat.S_IMODE(self.root.stat().st_mode)
        link = self.root / "linked"
        link.symlink_to(self.endpoint.descriptor_path.parent, target_is_directory=True)
        with self.assertRaises(ControlError):
            prepare_endpoint(link)
        self.assertEqual(stat.S_IMODE(self.root.stat().st_mode), old_mode)
        with self.assertRaises(ControlError):
            read_endpoint(link / "control.json")

    @unittest.skipIf(os.name == "nt", "POSIX permission gate")
    def test_client_rejects_world_readable_descriptor_before_read(self):
        os.chmod(self.endpoint.descriptor_path, 0o644)
        try:
            with self.assertRaises(ControlError) as caught:
                request(self.endpoint.descriptor_path, "status")
            self.assertEqual(caught.exception.code, "unsafe_endpoint")
            self.assertEqual(self.calls, [])
        finally:
            os.chmod(self.endpoint.descriptor_path, 0o600)

    def test_wrong_token_and_instance_never_dispatch(self):
        for changes in ({"token": "0" * 64}, {"instance_id": "wrong"}):
            with self.subTest(changes=list(changes)):
                response = self.raw(self.frame(**changes))
                self.assertEqual(response["error"]["code"], "unauthorized")
                self.assertNotIn(self.endpoint.token, json.dumps(response))
        self.assertEqual(self.calls, [])

    def test_collision_does_not_unlink_or_stop_the_original(self):
        other = DesktopControlServer(self.endpoint, lambda *_: {})
        with self.assertRaises(ControlError):
            other.start()
        other.close()
        self.assertTrue(self.endpoint.descriptor_path.exists())
        self.assertEqual(self.call()["state"], "ready")

    @unittest.skipIf(os.name == "nt", "Unix pathname boundary")
    def test_100_byte_socket_path_avoids_qt_permission_staging_overflow(self):
        with tempfile.TemporaryDirectory(prefix="", dir=self.root.parent) as temporary:
            parent = Path(temporary).resolve()
            # prepare_endpoint adds /d-<16 hex>/ipc: 23 bytes. Exercise the
            # declared upper bound, independent of the runner's TMPDIR length.
            padding = 77 - len(os.fsencode(parent)) - 1
            self.assertGreaterEqual(padding, 1, "Use a shorter test scratch parent")
            runtime = parent / ("r" * padding)
            runtime.mkdir(mode=0o700)
            endpoint = prepare_endpoint(runtime)
            self.assertEqual(len(os.fsencode(endpoint.socket_name)), 100)
            server = DesktopControlServer(endpoint, lambda *_: {"boundary": 100})
            try:
                server.start()
                self.assertEqual(int(server.server.socketOptions()), 0)
                self.assertEqual(
                    stat.S_IMODE(endpoint.descriptor_path.parent.stat().st_mode), 0o700
                )
                self.assertEqual(
                    stat.S_IMODE(endpoint.descriptor_path.stat().st_mode), 0o600
                )
                result = self.worker(
                    lambda: request(endpoint.descriptor_path, "status")
                )
                self.assertEqual(result, {"boundary": 100})
            finally:
                server.close()

    @unittest.skipIf(os.name == "nt", "Unix pathname ownership")
    def test_independent_listener_collision_preserves_live_path_and_owner(self):
        endpoint = prepare_endpoint(self.root)
        original = QLocalServer()
        self.assertTrue(original.listen(endpoint.socket_name), original.errorString())
        contender = DesktopControlServer(endpoint, lambda *_: {})
        probe = QLocalSocket()
        try:
            with self.assertRaises(ControlError) as caught:
                contender.start()
            self.assertEqual(caught.exception.code, "endpoint_unavailable")
            contender.close()
            self.assertTrue(original.isListening())
            self.assertTrue(Path(endpoint.socket_name).exists())
            self.assertTrue(endpoint.descriptor_path.exists())
            probe.connectToServer(endpoint.socket_name)
            self.assertTrue(probe.waitForConnected(1000))
            self.app.processEvents()
            self.assertTrue(original.hasPendingConnections())
            accepted = original.nextPendingConnection()
            accepted.abort()
            accepted.deleteLater()
        finally:
            probe.abort()
            contender.close()
            original.close()

    def test_multiple_frames_and_duplicate_keys_are_rejected(self):
        response = self.raw(self.frame() + self.frame(id="two"))
        self.assertEqual(response["error"]["code"], "invalid_request")
        duplicate = self.frame().replace(
            b'"id": "probe"', b'"id": "probe", "id": "second"'
        )
        self.assertEqual(self.raw(duplicate)["error"]["code"], "invalid_request")
        self.assertEqual(self.calls, [])

    def test_request_response_limits_and_partial_deadline(self):
        self.assertEqual(
            self.raw(b"x" * (MAX_REQUEST_BYTES + 1))["error"]["code"],
            "request_too_large",
        )
        self.server.dispatch = lambda *_: {"text": "x" * MAX_RESPONSE_BYTES}
        with self.assertRaises(ControlError) as caught:
            self.call()
        self.assertEqual(caught.exception.code, "response_too_large")
        self.server.timeout_ms = 60
        self.assertEqual(self.raw(b'{"partial":')["error"]["code"], "timeout")

    def test_connection_limit_preserves_admitted_client(self):
        self.server.max_clients = 1
        idle = QLocalSocket()
        idle.connectToServer(self.endpoint.socket_name)
        self.assertTrue(idle.waitForConnected(1000))
        self.app.processEvents()
        with self.assertRaises(ControlError) as caught:
            self.call()
        self.assertEqual(caught.exception.code, "busy")
        self.assertEqual(self.calls, [])
        idle.abort()
        self.app.processEvents()
        self.assertEqual(self.call()["state"], "ready")

    def test_strict_shapes_and_approval_are_checked_before_dispatch(self):
        config = {
            "max_urls": 20,
            "max_requests": 40,
            "max_seconds": 60,
            "rendering_mode": "raw",
        }
        bad = [
            ("shell", {"command": "ls"}),
            ("status", {"path": "/tmp"}),
            ("select_view", {"view_id": "../../tmp"}),
            ("stop_run", {"run_id": "owned-run", "approved": False}),
            (
                "new_scan",
                {
                    "project_uuid": "p",
                    "approved": True,
                    "config": {**config, "path": "/tmp"},
                },
            ),
            (
                "new_scan",
                {
                    "project_uuid": "p",
                    "approved": True,
                    "config": {**config, "max_urls": True},
                },
            ),
            (
                "new_scan",
                {
                    "project_uuid": "p",
                    "approved": True,
                    "config": {**config, "max_seconds": float("nan")},
                },
            ),
        ]
        for operation, arguments in bad:
            with self.subTest(operation=operation), self.assertRaises(ControlError):
                self.call(operation, arguments)
        self.assertEqual(self.calls, [])
        for operation, arguments in (
            ("select_view", {"view_id": "url", "tab_id": "tab-1"}),
            ("new_tab", {"project_uuid": "project-1", "scan_uuid": "scan-1"}),
            ("close_tab", {"tab_id": "tab-1"}),
        ):
            self.assertEqual(validate_arguments(operation, arguments), arguments)

    def test_mutation_receipt_prevents_duplicate_admission_and_id_reuse(self):
        arguments = {"run_id": "owned-run", "approved": True}
        first = self.call("stop_run", arguments, request_id="same-mutation")
        self.assertEqual(
            self.call("stop_run", arguments, request_id="same-mutation"), first
        )
        self.assertEqual(len(self.calls), 1)
        with self.assertRaises(ControlError) as caught:
            self.call("resume_run", arguments, request_id="same-mutation")
        self.assertEqual(caught.exception.code, "request_id_conflict")
        self.assertEqual(len(self.calls), 1)

    def test_external_run_refusal_survives_transport(self):
        with self.assertRaises(ControlError) as caught:
            self.call("stop_run", {"run_id": "external-run", "approved": True})
        self.assertEqual(caught.exception.code, "not_owned")

    def test_real_cli_subprocess_uses_same_endpoint(self):
        result = self.worker(
            lambda: subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "seohead_desktop.control_cli",
                    "--endpoint",
                    str(self.endpoint.descriptor_path),
                    "status",
                ],
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
            )
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["result"]["operation"], "status")
        self.assertNotIn(self.endpoint.token, result.stdout + result.stderr)

    def test_in_memory_mcp_lists_annotations_and_calls_real_local_socket(self):
        from mcp.shared.memory import create_connected_server_and_client_session

        async def run():
            server = create_mcp_server(str(self.endpoint.descriptor_path))
            async with create_connected_server_and_client_session(server) as client:
                listed = await client.list_tools()
                tools = {tool.name: tool for tool in listed.tools}
                self.assertEqual(len(tools), 11)
                self.assertTrue(tools["desktop_status"].annotations.readOnlyHint)
                self.assertFalse(tools["desktop_new_scan"].annotations.readOnlyHint)
                self.assertTrue(tools["desktop_new_scan"].annotations.openWorldHint)
                self.assertTrue(tools["desktop_stop_run"].annotations.destructiveHint)
                status = await client.call_tool("desktop_status", {})
                self.assertFalse(status.isError)
                self.assertEqual(status.structuredContent["operation"], "status")
                refused = await client.call_tool(
                    "desktop_stop_run", {"run_id": "owned-run"}
                )
                self.assertTrue(refused.isError)
                coerced = await client.call_tool(
                    "desktop_stop_run", {"run_id": "owned-run", "approved": "true"}
                )
                self.assertTrue(coerced.isError)
                accepted = await client.call_tool(
                    "desktop_new_scan",
                    {
                        "project_uuid": "project-1",
                        "approved": True,
                        "config": {
                            "max_urls": 20,
                            "max_requests": 40,
                            "max_seconds": 60,
                            "rendering_mode": "raw",
                        },
                    },
                )
                self.assertFalse(accepted.isError)
                self.assertEqual(accepted.structuredContent["state"], "queued")
                external = await client.call_tool(
                    "desktop_stop_run", {"run_id": "external-run", "approved": True}
                )
                self.assertTrue(external.isError)

        self.worker(lambda: asyncio.run(run()))
        self.assertEqual(
            [operation for operation, _ in self.calls],
            ["status", "new_scan", "stop_run"],
        )


if __name__ == "__main__":
    unittest.main()
