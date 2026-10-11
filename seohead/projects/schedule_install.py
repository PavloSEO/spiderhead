"""OS trigger text for ``schedule run-due``: launchd plist, cron line or Task Scheduler command.

Nothing here runs on import or during ``configure``. Each function writes or returns
text only when an operator calls it explicitly. The launchd plist is written to a
path the caller gives (default: the user's LaunchAgents folder) and is not loaded;
loading it is left to the operator with ``launchctl bootstrap``.
"""

from __future__ import annotations

import hashlib
import os
import plistlib
import sys
from pathlib import Path
from typing import Any

KINDS = ("launchd", "cron", "windows")
INTERVAL_SECONDS = 3600


def _label(directory: str) -> str:
    digest = hashlib.sha256(os.path.abspath(directory).encode("utf-8")).hexdigest()[:12]
    return f"tech.seohead.schedule.{digest}"


def _command(directory: str, python: str) -> list[str]:
    return [
        python,
        "-m",
        "seohead.cli",
        "schedule-run-due",
        "--directory",
        os.path.abspath(directory),
    ]


def default_launchd_path(directory: str) -> Path:
    return Path.home() / "Library" / "LaunchAgents" / f"{_label(directory)}.plist"


def install(
    directory: str,
    *,
    kind: str,
    dest: str | None = None,
    python: str | None = None,
) -> dict[str, Any]:
    if kind not in KINDS:
        raise ValueError("kind must be launchd, cron or windows")
    interpreter = python or sys.executable
    argv = _command(directory, interpreter)
    if kind == "launchd":
        path = Path(dest) if dest else default_launchd_path(directory)
        plist = {
            "Label": _label(directory),
            "ProgramArguments": argv,
            "StartInterval": INTERVAL_SECONDS,
            "RunAtLoad": False,
        }
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("wb") as stream:
            plistlib.dump(plist, stream)
        return {
            "ok": True,
            "kind": kind,
            "written": str(path),
            "loaded": False,
            "next_step": f"launchctl bootstrap gui/{os.getuid()} {path}",
        }
    if kind == "cron":
        line = "0 * * * * " + " ".join(_shell_quote(part) for part in argv)
        written: str | None = None
        if dest:
            Path(dest).write_text(line + "\n", encoding="utf-8")
            written = dest
        return {
            "ok": True,
            "kind": kind,
            "line": line,
            "written": written,
            "installed": False,
            "next_step": "add the line with crontab -e",
        }
    command = (
        "schtasks /Create /SC HOURLY /TN "
        + _quote_windows(_label(directory))
        + " /TR "
        + _quote_windows(" ".join(_quote_windows(part) for part in argv))
        + " /F"
    )
    written = None
    if dest:
        Path(dest).write_text(command + "\r\n", encoding="utf-8")
        written = dest
    return {
        "ok": True,
        "kind": kind,
        "command": command,
        "written": written,
        "installed": False,
        "next_step": "run the command in an elevated or user Command Prompt",
    }


def uninstall(directory: str, *, kind: str, dest: str | None = None) -> dict[str, Any]:
    if kind not in KINDS:
        raise ValueError("kind must be launchd, cron or windows")
    if kind == "launchd":
        path = Path(dest) if dest else default_launchd_path(directory)
        removed = path.exists()
        path.unlink(missing_ok=True)
        return {
            "ok": True,
            "kind": kind,
            "removed": str(path) if removed else None,
            "next_step": f"launchctl bootout gui/{os.getuid()} {path} if it was loaded",
        }
    removed_file: str | None = None
    if dest and Path(dest).exists():
        Path(dest).unlink()
        removed_file = dest
    hint = (
        "delete the line from crontab -e"
        if kind == "cron"
        else f"schtasks /Delete /TN {_quote_windows(_label(directory))} /F"
    )
    return {"ok": True, "kind": kind, "removed": removed_file, "next_step": hint}


def _shell_quote(value: str) -> str:
    if value and all(ch.isalnum() or ch in "/._-=" for ch in value):
        return value
    return "'" + value.replace("'", "'\"'\"'") + "'"


def _quote_windows(value: str) -> str:
    return '"' + value.replace('"', '\\"') + '"'
