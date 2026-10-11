#!/usr/bin/env python3
"""Install or launch Desktop and its compatible core in one local environment."""

from __future__ import annotations

import argparse
import datetime
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

if sys.version_info < (3, 11):
    raise SystemExit(
        "Source bootstrap requires Python 3.11 or newer (tested with 3.12); no global packages are installed."
    )

from bundle_manifest import _core_identity, desktop_identity

ROOT = Path(__file__).resolve().parents[1]
MARKER = "seohead-source-install.json"


def executable(environment: Path, name: str) -> Path:
    return (
        environment
        / ("Scripts" if os.name == "nt" else "bin")
        / (name + (".exe" if os.name == "nt" else ""))
    )


def installation_plan(core_source: Path, environment: Path, *, update: bool = False) -> dict:
    contract = json.loads((ROOT / "packaging" / "source-install.json").read_text(encoding="utf-8"))
    core = _core_identity(core_source)
    if (
        core["commit"] != contract["core_commit"]
        or core["repository"] != contract["core_repository"]
    ):
        raise ValueError(
            "core source must match the tested commit and repository in packaging/source-install.json"
        )
    marker = environment / MARKER
    if environment.exists() and not (
        update and marker.is_file() and (environment / "pyvenv.cfg").is_file()
    ):
        raise ValueError(
            "environment already exists; choose a new --venv, or use --update for an environment created by this script"
        )
    if update and not marker.is_file():
        raise ValueError("--update requires an environment created by this script")
    if update:
        saved = json.loads(marker.read_text(encoding="utf-8"))
        if saved.get("desktop_source") != str(ROOT):
            raise ValueError("the environment belongs to a different Desktop checkout")
    uv = shutil.which("uv")
    if not uv:
        raise ValueError(
            "uv is required; install it using https://docs.astral.sh/uv/getting-started/installation/"
        )
    commands = []
    if not update:
        commands.append([uv, "venv", "--python", sys.executable, str(environment)])
    commands.append(
        [
            uv,
            "pip",
            "install",
            "--python",
            str(executable(environment, "python")),
            "--editable",
            f"{ROOT}[build]",
            "--editable",
            f"{core_source}[{contract['core_extras']}]",
        ]
    )
    return {
        "core": core,
        "desktop": desktop_identity(ROOT, require_clean=False),
        "commands": commands,
    }


def install(args) -> int:
    environment = args.venv.resolve()
    plan = installation_plan(args.core_source.resolve(), environment, update=args.update)
    if args.dry_run:
        print(
            json.dumps(
                {
                    "venv": str(environment),
                    "core_commit": plan["core"]["commit"],
                    "commands": plan["commands"],
                },
                indent=2,
            )
        )
        return 0
    child_environment = dict(os.environ, UV_CACHE_DIR=str(ROOT / ".build" / "uv-cache"))
    marker = {
        "desktop_source": str(ROOT),
        "core_source": str(args.core_source.resolve()),
        "state": "installing",
        "core": plan["core"],
        "desktop": plan["desktop"],
    }
    for index, command in enumerate(plan["commands"]):
        subprocess.run(command, cwd=ROOT, env=child_environment, check=True)
        if index == 0:
            (environment / MARKER).write_text(json.dumps(marker, indent=2) + "\n", encoding="utf-8")
    python = executable(environment, "python")
    subprocess.run([str(executable(environment, "seohead")), "--version"], check=True)
    subprocess.run(
        [str(executable(environment, "seohead-desktop-agent")), "--help"],
        check=True,
        stdout=subprocess.DEVNULL,
    )
    subprocess.run(
        [str(executable(environment, "seohead-desktop")), "--help"],
        check=True,
        stdout=subprocess.DEVNULL,
    )
    subprocess.run(
        [
            str(python),
            "-c",
            "from seohead.mcp.mcp_server import main; from seohead_desktop.control_cli import create_mcp_server; from PyQt5.QtSvg import QSvgRenderer",
        ],
        check=True,
    )
    if _core_identity(args.core_source.resolve()) != plan["core"]:
        raise ValueError("core source changed while installing; installation was not accepted")
    marker["state"] = "ready"
    marker["installed_distributions"] = json.loads(
        subprocess.check_output(
            [
                str(python),
                "-c",
                "import importlib.metadata as m,json; print(json.dumps({d.metadata['Name']:d.version for d in m.distributions() if d.metadata.get('Name')}))",
            ],
            text=True,
        )
    )
    (environment / MARKER).write_text(json.dumps(marker, indent=2) + "\n", encoding="utf-8")
    print(
        f"Installed. Launch: {sys.executable} {ROOT / 'scripts' / 'source.py'} run --venv {environment}"
    )
    return 0


def launch_command(args) -> list[str]:
    environment = args.venv.resolve()
    desktop = executable(environment, "seohead-desktop")
    core = executable(environment, "seohead")
    if not desktop.is_file() or not core.is_file():
        raise ValueError("Desktop and core must be installed in the selected --venv")
    command = [str(desktop), "--core-cli", str(core)]
    for option in ("project", "agent_control"):
        path = getattr(args, option)
        if path is not None:
            if not path.is_dir() or path.is_symlink():
                raise ValueError(
                    f"--{option.replace('_', '-')} requires an existing real directory"
                )
            command.extend(["--" + option.replace("_", "-"), str(path.resolve())])
    if args.no_settings:
        command.append("--no-settings")
    return command


def run(args) -> int:
    command = launch_command(args)
    if args.dry_run:
        print(
            json.dumps(
                {
                    "command": command,
                    "log": str(args.log.resolve())
                    if args.log
                    else "automatic private log under .build/logs",
                },
                indent=2,
            )
        )
        return 0
    log = args.log
    if log is None:
        logs = ROOT / ".build" / "logs"
        logs.mkdir(parents=True, exist_ok=True)
        log = logs / ("desktop-" + datetime.datetime.now().strftime("%Y%m%d-%H%M%S-%f") + ".log")
    descriptor = os.open(log, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as output:
        output.write(
            json.dumps(
                {
                    "command": command,
                    "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                }
            )
            + "\n"
        )
        output.flush()
        print(f"Desktop log: {log.resolve()}", flush=True)
        result = subprocess.run(
            command,
            cwd=ROOT,
            stdout=output,
            stderr=subprocess.STDOUT,
            env=dict(os.environ, PYTHONUNBUFFERED="1", SEOHEAD_DESKTOP_LOG=str(log.resolve())),
        )
        output.write(json.dumps({"exit_code": result.returncode}) + "\n")
    return result.returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    setup = commands.add_parser(
        "install",
        help="Install both packages; never replace an existing unmanaged environment",
    )
    setup.add_argument("--core-source", type=Path, required=True)
    setup.add_argument(
        "--update",
        action="store_true",
        help="Update only an environment previously created by this script",
    )
    launch = commands.add_parser("run", help="Fast source preview without a frozen build")
    launch.add_argument(
        "--project",
        type=Path,
        help="Existing project; use . to open the current project",
    )
    launch.add_argument(
        "--agent-control",
        type=Path,
        help="Existing owned runtime directory; enables local control explicitly",
    )
    launch.add_argument(
        "--log", type=Path, help="New log file; an existing file is never overwritten"
    )
    launch.add_argument("--no-settings", action="store_true")
    for command in (setup, launch):
        command.add_argument("--venv", type=Path, default=ROOT / ".venv")
        command.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    try:
        return install(args) if args.command == "install" else run(args)
    except (OSError, ValueError, subprocess.CalledProcessError) as exc:
        print(f"source: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
