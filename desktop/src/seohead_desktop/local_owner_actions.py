"""Explicit owner-only project mutations routed through the local CLI."""

from __future__ import annotations

import json
import subprocess
import threading
import uuid

from PyQt5 import sip
from PyQt5.QtCore import QObject, QRunnable, QThreadPool, pyqtSignal, pyqtSlot


def accept_goal_arguments(directory, entry_id, expected_revision):
    if not isinstance(directory, str) or not directory:
        raise ValueError("project directory is required")
    if not isinstance(entry_id, str) or not entry_id.startswith("inbox:"):
        raise ValueError("a proposed inbox goal is required")
    if type(expected_revision) is not int or expected_revision < 0:
        raise ValueError("the current inbox revision is required")
    return [
        "project-inbox-goal",
        "--directory",
        directory,
        "--entry-id",
        entry_id,
        "--state",
        "accepted",
        "--expected-revision",
        str(expected_revision),
    ]


def manual_signoff_request(directory, item_id, expected_revision, reason):
    if not isinstance(directory, str) or not directory:
        raise ValueError("project directory is required")
    if not isinstance(item_id, str) or not item_id:
        raise ValueError("a checklist item is required")
    if type(expected_revision) is not int or expected_revision < 0:
        raise ValueError("the current checklist revision is required")
    if not isinstance(reason, str) or not reason.strip() or len(reason.strip()) > 2000:
        raise ValueError("an owner review reason of 1..2000 characters is required")
    record = {
        "status": "succeeded",
        "reason": reason.strip(),
        "reviewer": "OWNER",
        "signoff": True,
    }
    payload = {
        "directory": directory,
        "item_id": item_id,
        "expected_revision": expected_revision,
        "record": record,
    }
    # With no source flags, the CLI reads the structured request from stdin.
    return ["project-checklist-record"], json.dumps(
        payload, ensure_ascii=False, separators=(",", ":")
    ).encode("utf-8")


def run_owner_action(executable, arguments, *, stdin_data=None, popen=subprocess.Popen):
    if not isinstance(executable, str) or not executable:
        raise ValueError("local core CLI is unavailable")
    if not isinstance(arguments, list) or not arguments or arguments[0] not in {
        "project-inbox-goal",
        "project-checklist-record",
    }:
        raise ValueError("local owner-action command is not declared")
    if stdin_data is not None and (
        arguments[0] != "project-checklist-record"
        or not isinstance(stdin_data, bytes)
        or len(stdin_data) > 65536
    ):
        raise ValueError("local owner-action input is invalid or too large")
    process = popen(
        [executable, *arguments],
        stdin=subprocess.PIPE if stdin_data is not None else subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    output = bytearray()
    too_large = threading.Event()

    def read_output():
        while True:
            chunk = process.stdout.read(min(65536, 1_048_577 - len(output)))
            if not chunk:
                return
            if len(output) + len(chunk) > 1_048_576:
                output.extend(chunk[: 1_048_577 - len(output)])
                too_large.set()
                try:
                    process.kill()
                except OSError:
                    pass
                return
            output.extend(chunk)

    reader = threading.Thread(target=read_output, daemon=True)
    reader.start()
    try:
        if stdin_data is not None:
            process.stdin.write(stdin_data)
            process.stdin.close()
        process.wait(timeout=45)
    except subprocess.TimeoutExpired as exc:
        process.kill()
        process.wait()
        reader.join()
        process.stdout.close()
        raise ValueError("The local owner action timed out.") from exc
    reader.join()
    process.stdout.close()
    if too_large.is_set() or process.returncode != 0:
        raise ValueError("The owner action was not accepted by the core.")
    try:
        result = json.loads(output.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as exc:
        raise ValueError("The core did not return a valid owner-action receipt.") from exc
    if not isinstance(result, dict) or result.get("ok") is not True:
        raise ValueError("The owner action was not accepted by the core.")
    return {"ok": True}


def competitor_scan_arguments(
    directory,
    task_id,
    expected_revision,
    run_id,
    *,
    approve_large_crawl=False,
    producer_build=None,
):
    if not isinstance(directory, str) or not directory:
        raise ValueError("project directory is required")
    if not isinstance(task_id, str) or not task_id.startswith("custom:"):
        raise ValueError("a current custom task is required")
    if type(expected_revision) is not int or expected_revision < 0:
        raise ValueError("the current checklist revision is required")
    if not isinstance(run_id, str) or str(uuid.UUID(run_id)) != run_id:
        raise ValueError("a valid scan run ID is required")
    if type(approve_large_crawl) is not bool:
        raise ValueError("large-crawl approval must be explicit")
    if producer_build is not None and (
        not isinstance(producer_build, str) or not producer_build.strip()
    ):
        raise ValueError("producer build must be a nonempty string")
    arguments = [
        "project-competitors-scan",
        "--directory",
        directory,
        "--task-id",
        task_id,
        "--expected-revision",
        str(expected_revision),
        "--confirm",
        "--concurrency",
        "3",
        "--run-id",
        run_id,
    ]
    if approve_large_crawl:
        arguments.append("--approve-large-crawl")
    if producer_build is not None:
        arguments.extend(("--producer-build", producer_build.strip()))
    return arguments


def competitor_scan_status_arguments(directory, run_id):
    return _competitor_scan_read_arguments(
        "project-competitors-scan-status", directory, run_id
    )


def competitor_scan_cancel_arguments(directory, run_id):
    return _competitor_scan_read_arguments(
        "project-competitors-scan-cancel", directory, run_id
    )


def _competitor_scan_read_arguments(command, directory, run_id):
    if command not in {
        "project-competitors-scan-status",
        "project-competitors-scan-cancel",
    }:
        raise ValueError("competitor scan command is not declared")
    if not isinstance(directory, str) or not directory:
        raise ValueError("project directory is required")
    if not isinstance(run_id, str) or str(uuid.UUID(run_id)) != run_id:
        raise ValueError("a valid scan run ID is required")
    return [command, "--directory", directory, "--run-id", run_id]


def run_competitor_scan_command(
    executable, arguments, *, timeout=30, popen=subprocess.Popen
):
    """Run only a declared competitor-run CLI command with bounded output and no GUI-thread wait."""
    if not isinstance(executable, str) or not executable:
        raise ValueError("local core CLI is unavailable")
    if (
        not isinstance(arguments, list)
        or not arguments
        or arguments[0]
        not in {
            "project-competitors-scan",
            "project-competitors-scan-status",
            "project-competitors-scan-cancel",
        }
    ):
        raise ValueError("competitor scan command is not declared")
    process = popen(
        [executable, *arguments],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    output = bytearray()
    too_large = threading.Event()

    def read_output():
        while True:
            chunk = process.stdout.read(min(65536, 1_048_577 - len(output)))
            if not chunk:
                return
            if len(output) + len(chunk) > 1_048_576:
                output.extend(chunk[: 1_048_577 - len(output)])
                too_large.set()
                try:
                    process.kill()
                except OSError:
                    pass
                return
            output.extend(chunk)

    reader = threading.Thread(target=read_output, daemon=True)
    reader.start()
    try:
        process.wait(timeout=timeout)
    except subprocess.TimeoutExpired as exc:
        process.kill()
        process.wait()
        reader.join()
        process.stdout.close()
        raise ValueError("The competitor scan command timed out.") from exc
    reader.join()
    process.stdout.close()
    if too_large.is_set() or process.returncode != 0:
        raise ValueError("The core did not complete the competitor scan command.")
    try:
        result = json.loads(output.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as exc:
        raise ValueError("The core did not return a valid competitor scan response.") from exc
    if not isinstance(result, dict):
        raise ValueError("The core competitor scan response was not an object.")
    return result


class _Signals(QObject):
    done = pyqtSignal(object)
    failed = pyqtSignal(str)


class _Job(QRunnable):
    def __init__(self, executable, arguments, stdin_data):
        super().__init__()
        self.executable = executable
        self.arguments = arguments
        self.stdin_data = stdin_data
        self.signals = _Signals()

    def run(self):
        try:
            self.signals.done.emit(
                run_owner_action(
                    self.executable, self.arguments, stdin_data=self.stdin_data
                )
            )
        except (OSError, ValueError, subprocess.SubprocessError):
            self.signals.failed.emit(
                "The local owner action failed. Refresh the project and try again."
            )


class LocalOwnerActions(QObject):
    """Run explicit confirmation routes outside the GUI thread and drop stale owners."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self._pending = {}
        self._serial = 0

    def submit(self, executable, arguments, on_success, on_error, owner, *, stdin_data=None):
        self._serial += 1
        serial = self._serial
        job = _Job(executable, arguments, stdin_data)
        delivery = _OwnerActionDelivery(self, serial, on_success, on_error, owner)
        self._pending[serial] = (job, delivery)
        job.signals.done.connect(delivery.succeeded)
        job.signals.failed.connect(delivery.failed)
        QThreadPool.globalInstance().start(job)

    def _deliver(self, serial, handler, value, owner):
        self._pending.pop(serial, None)
        if not sip.isdeleted(owner):
            handler(value)


class _OwnerActionDelivery(QObject):
    """Bound Qt slots return owner CLI results to the GUI thread."""

    def __init__(self, service, serial, on_success, on_error, owner):
        super().__init__(service)
        self.service = service
        self.serial = serial
        self.on_success = on_success
        self.on_error = on_error
        self.owner = owner

    @pyqtSlot(object)
    def succeeded(self, value):
        self.service._deliver(self.serial, self.on_success, value, self.owner)

    @pyqtSlot(str)
    def failed(self, value):
        self.service._deliver(self.serial, self.on_error, value, self.owner)
