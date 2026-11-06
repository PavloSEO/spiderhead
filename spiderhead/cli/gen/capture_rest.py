#!/usr/bin/env python3
"""Capture Python oracle outputs for the rest-b command group.

Group: ``log-scan``, ``gsc-archive``, ``metrika-traffic-pdf`` and
``cloudflare-logpush-import``. Cases live in ``tests/fixtures/rest/cases.json``;
each case names optional ``setup`` steps (run first, their outputs recorded),
``files_in`` seeds, the command ``args`` shared by both surfaces (the CLI reads
them through ``--input``), follow-up ``then`` steps, ``collect`` files, optional
``env`` credentials and an optional ``routes`` script for the provider loopback
fake (``SPIDERHEAD_PROVIDER_LOOPBACK``). Every case and surface runs in a fresh
work directory so cases never depend on order.

    capture_rest.py [--case ID ...]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE / "fake_net"))
import capture_handlers as ch  # noqa: E402  (shared oracle runners and normalisation)
from fake_server import FakeProviders  # noqa: E402

CRATE = HERE.parent
FIXTURES = CRATE / "tests" / "fixtures" / "rest"
CASES = FIXTURES / "cases.json"
CLI = ch.CLI

COMPACT_TS_RE = re.compile(r"\d{8}T\d{6}Z")
DIGEST_RE = re.compile(r'("(?:digest|sha256)": ")[0-9a-f]{64}')
IDENTITY_RE = re.compile(r'("(?:device|inode|mtime_ns)": )-?[0-9]+')
PORT_RE = re.compile(r"127\.0\.0\.1:\d+")
HEX32_RE = re.compile(r'("session_id": ")[0-9a-f]{32}')

# Credential names a case may explicitly seed through ``env``; cleared for every run.
CREDENTIAL_VARS = (
    "GSC_ACCESS_TOKEN",
    "GSC_SERVICE_ACCOUNT_FILE",
    "YANDEX_METRIKA_TOKEN",
    "CLOUDFLARE_API_TOKEN",
    "CLOUDFLARE_TOKEN_FILE",
    "SEOHEAD_CLOUDFLARE_TOKEN_FILE",
    "SEOHEAD_CHROME",
)


def norm_text(norm, text: str) -> str:
    text = PORT_RE.sub("127.0.0.1:{PORT}", text)
    text = IDENTITY_RE.sub(lambda m: f"{m.group(1)}<DURATION>", ch.norm_text(norm, text))
    text = COMPACT_TS_RE.sub("<TS>", DIGEST_RE.sub(r"\1<DIGEST>", text))
    return HEX32_RE.sub(r"\1<HEX>", text)


def fill(value, tokens: dict):
    """Substitute `{TOKEN}` placeholders in every string of a JSON value."""
    if isinstance(value, str):
        for key, text in tokens.items():
            value = value.replace("{" + key + "}", text)
        return value
    if isinstance(value, list):
        return [fill(item, tokens) for item in value]
    if isinstance(value, dict):
        return {key: fill(item, tokens) for key, item in value.items()}
    return value


def seed_files(case: dict, work: Path, tokens: dict) -> None:
    for site in case.get("scan_seeds", []):
        source = ch.ORACLE_ROOT / "raw-scans" / site
        for candidate in source.iterdir():
            if candidate.is_file():
                shutil.copy(candidate, work / candidate.name)
    for name, text in case.get("files_in", {}).items():
        target = work / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(fill(text, tokens), encoding="utf-8")


def collect_files(work: Path, names: list[str]) -> dict:
    files: dict[str, str] = {}
    for name in names:
        target = work / name
        paths = (
            sorted(p for p in target.rglob("*") if p.is_file())
            if target.is_dir()
            else [target]
        )
        for path in paths:
            if not path.exists():
                continue
            raw = path.read_bytes()
            key = str(path.relative_to(work))
            try:
                files[key] = raw.decode("utf-8")
            except UnicodeDecodeError:
                files[key] = "sha256:" + hashlib.sha256(raw).hexdigest()
    return files


def case_env(case: dict, work: Path, origin: str) -> dict:
    env = dict(os.environ)
    env.update(ch.ENV_BASE)
    env["HOME"] = str(work)
    env["NO_PROXY"] = "127.0.0.1"
    env["no_proxy"] = "127.0.0.1"
    for name in CREDENTIAL_VARS:
        env.pop(name, None)
    if case.get("routes"):
        env["SPIDERHEAD_PROVIDER_LOOPBACK"] = origin
        # sitecustomize.py re-aims the oracle's urllib calls at the fake server.
        env["PYTHONPATH"] = str(HERE / "fake_net")
    else:
        env.pop("SPIDERHEAD_PROVIDER_LOOPBACK", None)
        env.pop("PYTHONPATH", None)
    for name, value in (case.get("env") or {}).items():
        env[name] = value
    return env


def run_cli_steps(steps, work, tokens, env, norm):
    outputs = []
    for step in steps:
        argv = [step["command"], "--input", json.dumps(fill(step["args"], tokens))]
        if step.get("flags") is not None:
            argv = [step["command"], *fill(step["flags"], tokens)]
        proc = subprocess.run(
            [str(CLI), *argv],
            capture_output=True,
            text=True,
            env=env,
            cwd=work,
            timeout=300,
        )
        outputs.append(
            {
                "exit": proc.returncode,
                "stdout": norm_text(norm, proc.stdout),
                "stderr": norm_text(norm, proc.stderr),
            }
        )
    return outputs


class McpSession:
    """Oracle MCP server over stdio with the case environment."""

    def __init__(self, work: Path, env: dict) -> None:
        self.config_dir = Path(tempfile.mkdtemp(prefix="sh-rest-cfg-"))
        env["SPIDERHEAD_CONFIG_DIR"] = str(self.config_dir)
        for var in ("SPIDERHEAD_MCP_CONSUMER_ID", "SPIDERHEAD_MCP_PROJECT_ALLOWLIST"):
            env.pop(var, None)
        self.proc = subprocess.Popen(
            [str(CLI), "mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            cwd=work,
            text=True,
            bufsize=1,
        )
        self.next_id = 1
        self.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "capture", "version": "0"},
            },
        )
        self.notify("notifications/initialized", {})

    def notify(self, method, params):
        self.proc.stdin.write(
            json.dumps({"jsonrpc": "2.0", "method": method, "params": params}) + "\n"
        )
        self.proc.stdin.flush()

    def request(self, method, params):
        rid = self.next_id
        self.next_id += 1
        self.proc.stdin.write(
            json.dumps(
                {"jsonrpc": "2.0", "id": rid, "method": method, "params": params}
            )
            + "\n"
        )
        self.proc.stdin.flush()
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise SystemExit(
                    f"oracle mcp server closed: {self.proc.stderr.read()[-1500:]}"
                )
            message = json.loads(line)
            if message.get("id") == rid:
                return message["result"]

    def call(self, tool, arguments):
        result = self.request("tools/call", {"name": tool, "arguments": arguments})
        return {
            "tool": tool,
            "is_error": bool(result.get("isError")),
            "text": result["content"][0]["text"] if result.get("content") else "",
        }

    def stop(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=30)
        shutil.rmtree(self.config_dir, ignore_errors=True)


def run_case(case: dict, probe_origin: str) -> dict:
    record: dict = {}
    steps = case.get("setup", []) + [case] + case.get("then", [])
    for surface in ("cli", "mcp"):
        work = Path(tempfile.mkdtemp(prefix=f"sh-rest-{surface}-")).resolve()
        try:
            tokens = {"WORK": str(work), "ORIGIN": probe_origin}
            with FakeProviders([]) as server:
                server.routes = [
                    dict(route, _served=0)
                    for route in fill(case.get("routes", []), tokens)
                ]
                origin = server.origin
                tokens["ORIGIN"] = origin
                seed_files(case, work, tokens)
                env = case_env(case, work, origin)
                norm = ch.Norm("http://base.invalid", work)
                if surface == "cli":
                    outputs = run_cli_steps(steps, work, tokens, env, norm)
                else:
                    session = McpSession(work, env)
                    outputs = []
                    for step in steps:
                        got = session.call(
                            step["tool"], fill(step["args"], tokens)
                        )
                        outputs.append(
                            {
                                "is_error": got["is_error"],
                                "text": norm_text(norm, got["text"]),
                            }
                        )
                    session.stop()
                files = {
                    k: norm_text(norm, v) if not v.startswith("sha256:") else v
                    for k, v in collect_files(
                        work, case.get("collect", [])
                    ).items()
                }
                record[surface] = {
                    "steps": outputs,
                    "files": files,
                    "requests": norm.walk(server.requests),
                }
        finally:
            shutil.rmtree(work, ignore_errors=True)
    return record


def capture_all(out: Path, only: set[str]) -> int:
    cases = json.loads(CASES.read_text(encoding="utf-8"))
    count = 0
    for case in cases:
        if only and case["id"] not in only:
            continue
        record = run_case(case, "")
        target = out / f"{case['id']}.json"
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(
            json.dumps(record, ensure_ascii=False, indent=1, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        count += 1
    return count


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--case", action="append", default=[])
    args = parser.parse_args()
    return 0 if capture_all(FIXTURES, set(args.case)) else 1


if __name__ == "__main__":
    raise SystemExit(main())
