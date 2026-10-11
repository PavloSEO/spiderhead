"""One append-only journal of every command and MCP call this toolkit runs.

Two problems this solves, and the second is the reason it is worth building.

**Answering "what did the agent actually do".** A CLI and a stdio MCP server both
leave nothing behind once the process exits. When an audit produced a surprising
number, the question is which tools ran, against what, with which arguments and
how long they took — and today that is unanswerable after the fact.

**Making repeated work visible before it is repeated.** Every entry carries a
fingerprint of the call and where its output landed, so a later run can see that
the same tool ran against the same target minutes ago.

**Reusing that work, but only where staleness is nobody's problem.** Whether a stale answer is
acceptable is a property of the *question*, not of the tool: a parse of a page a client just
fixed must not come back from five minutes ago, but a domain registration lookup almost
certainly may. So reuse here is opt-in per tool (``reuse_policy`` / ``SEOHEAD_REUSE_POLICY``),
defaults to never, and is never silent — a reused answer is marked ``reused: true`` in both the
returned result and the journal entry that answered it. See ``journaled``.

Format is JSONL, one line per call, appended after the call completes so an
interrupted process cannot corrupt earlier entries. Default path is
``~/.config/seohead/runs.jsonl``; override with ``SEOHEAD_RUN_LOG``, or set
``log.path`` in the config file. Set ``SEOHEAD_RUN_LOG=off`` to disable.
"""

from __future__ import annotations

import calendar
import copy
import hashlib
import json
import os
import time
from contextlib import contextmanager
from pathlib import Path
from typing import Any

DEFAULT_PATH = "~/.config/seohead/runs.jsonl"

# Size ceiling for the live journal. When an append would cross it, the journal is moved to
# ``<name>.1`` (replacing the previous backup), so the on-disk total stays at most about twice
# this value. Override with ``SEOHEAD_RUN_LOG_MAX_BYTES``; a non-positive or malformed value
# falls back to the default.
MAX_BYTES_ENV = "SEOHEAD_RUN_LOG_MAX_BYTES"
DEFAULT_MAX_BYTES = 5 * 1024 * 1024

# Argument names whose values must never reach the journal. Matched as
# substrings, lowercased, because provider clients spell them differently.
SECRET_HINTS = ("token", "key", "secret", "password", "passwd", "auth", "credential")

MAX_VALUE_CHARS = 300

# Per-tool reuse policy: a JSON object mapping tool name to a maximum age in seconds, e.g.
# '{"domain_profile": 86400}'. Absent or unparsable means an empty policy, which means what the
# issue this implements asked for by name: with no policy configured, nothing is ever reused.
# Whether a stale answer is acceptable is a property of the *question*, not of the tool, so this
# is deliberately opt-in per tool rather than a single global switch.
REUSE_POLICY_ENV = "SEOHEAD_REUSE_POLICY"

# A stored result larger than this is not written back into the journal for reuse: the journal
# is meant to stay a thin, greppable log of calls, not a second copy of every large payload a
# tool ever returned. A tool whose answers are this large is also a poor fit for journal-driven
# reuse in the first place (that budget is for a domain profile or a WHOIS lookup, not a parsed
# page body).
MAX_REUSABLE_RESULT_BYTES = 20_000

# Which face of the toolkit is running. Set once at process start so a single
# journaling point can name the caller without every call site passing it.
_interface = "library"


def set_interface(name: str) -> None:
    global _interface
    _interface = name


def current_interface() -> str:
    return _interface


def log_path() -> Path | None:
    """Where the journal lives, or ``None`` when logging is switched off."""
    override = os.environ.get("SEOHEAD_RUN_LOG")
    try:
        if override:
            if override.strip().lower() in ("off", "0", "none", "false"):
                return None
            return Path(override).expanduser()
        return Path(DEFAULT_PATH).expanduser()
    except (OSError, ValueError):
        # A malformed override must silence the journal, not stop the tool.
        return None


def _redact(value: Any) -> Any:
    """Shorten a value for the journal without changing its meaning."""
    if isinstance(value, str):
        return value if len(value) <= MAX_VALUE_CHARS else value[:MAX_VALUE_CHARS] + "…"
    if isinstance(value, (list, tuple)):
        shown = [_redact(v) for v in list(value)[:10]]
        if len(value) > 10:
            shown.append(f"…+{len(value) - 10} more")
        return shown
    if isinstance(value, dict):
        return {
            k: "[redacted]"
            if isinstance(k, str) and k.lower() in {"http.proxy", "proxy", "proxy_url"}
            else _redact(v)
            for k, v in value.items()
        }
    if isinstance(value, (bool, int, float, type(None))):
        return value
    if isinstance(value, os.PathLike):
        return _redact(os.fspath(value))
    # Anything else is not JSON: a callable an interface handed the handler (a
    # progress line's callback, #619), a file handle, an object. Its type name,
    # not str(value), because most objects' str carries a memory address, and an
    # address would give the same call a different fingerprint every run. Naming
    # the type keeps the entry honest -- the argument was given, and this is
    # what it was -- where returning the object unchanged makes json.dumps raise
    # inside fingerprint(), which fails the whole run rather than degrading one
    # journal entry.
    return f"<{type(value).__name__}>"


def safe_arguments(arguments: dict[str, Any] | None) -> dict[str, Any]:
    """Drop credentials, shorten the rest.

    A journal that leaks an API key is worse than no journal, and the leak would
    be silent: nothing about a log file suggests it holds secrets.
    """
    out: dict[str, Any] = {}
    for name, value in (arguments or {}).items():
        if name.lower() in {"http.proxy", "proxy", "proxy_url"} or any(
            hint in name.lower() for hint in SECRET_HINTS
        ):
            out[name] = "[redacted]"
        else:
            out[name] = _redact(value)
    return out


# Arguments an interface adds for display only. They change what the operator sees
# during a run and nothing about what the run does, so they must not change its
# identity: a crawl and the same crawl with -q are the same call, and a journal
# that says otherwise cannot answer "have I already done this" (#619).
DISPLAY_ONLY_ARGUMENTS = frozenset({"progress"})


def fingerprint(tool: str, arguments: dict[str, Any] | None) -> str:
    """Stable identity for "the same call again", for later reuse decisions."""
    identity = {
        name: value
        for name, value in safe_arguments(arguments).items()
        if name not in DISPLAY_ONLY_ARGUMENTS
    }
    payload = json.dumps({"tool": tool, "arguments": identity}, sort_keys=True, ensure_ascii=False)
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()[:16]


def max_bytes() -> int:
    """The live journal's size ceiling in bytes, read fresh like ``reuse_policy``."""
    try:
        value = int(os.environ.get(MAX_BYTES_ENV, DEFAULT_MAX_BYTES))
    except ValueError:
        return DEFAULT_MAX_BYTES
    return value if value > 0 else DEFAULT_MAX_BYTES


def backup_path(path: Path) -> Path:
    """Where the previous generation of the journal is kept after rotation."""
    return path.with_name(path.name + ".1")


def record(entry: dict[str, Any]) -> dict[str, Any]:
    """Append one entry. Never raises: journaling must not break a run."""
    path = log_path()
    if path is None:
        return entry
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        line = json.dumps(entry, ensure_ascii=False, default=str) + "\n"
        if path.exists() and path.stat().st_size + len(line.encode("utf-8")) > max_bytes():
            os.replace(path, backup_path(path))
        with open(path, "a", encoding="utf-8") as handle:
            handle.write(line)
    except (OSError, ValueError):
        # An unwritable journal is a degraded observation, not a failed audit.
        # ValueError as well as OSError: an invalid path (a null byte from a bad
        # environment variable) raises before the filesystem is ever touched.
        pass
    return entry


@contextmanager
def journal(
    interface: str,
    tool: str,
    arguments: dict[str, Any] | None = None,
    *,
    redact_error: bool = False,
):
    """Record one call, whether it succeeds or raises.

    ``yield``s a mutable dict; a caller may add result facts to it (URLs crawled,
    output directory) and they are written alongside the call.
    """
    started = time.time()
    monotonic = time.monotonic()
    facts: dict[str, Any] = {}
    error: str | None = None
    try:
        yield facts
    except BaseException as exc:  # recorded, then re-raised unchanged
        error = (
            f"{type(exc).__name__}: sensitive operation failed"
            if redact_error
            else f"{type(exc).__name__}: {exc}"
        )
        raise
    finally:
        record(
            {
                "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(started)),
                "interface": interface,
                "tool": tool,
                "arguments": safe_arguments(arguments),
                "fingerprint": fingerprint(tool, arguments),
                "duration_s": round(time.monotonic() - monotonic, 3),
                "ok": error is None,
                "error": error,
                **facts,
            }
        )


def reuse_policy() -> dict[str, float]:
    """The configured per-tool maximum reuse age, in seconds. Empty means never reuse.

    Read fresh on every call rather than cached at import time, so a test (or an operator) can
    change ``SEOHEAD_REUSE_POLICY`` between calls in the same process.
    """
    raw = os.environ.get(REUSE_POLICY_ENV)
    if not raw:
        return {}
    try:
        parsed = json.loads(raw)
    except ValueError:
        return {}  # a malformed policy must disable reuse, not crash the tool
    if not isinstance(parsed, dict):
        return {}
    out: dict[str, float] = {}
    for name, value in parsed.items():
        try:
            age = float(value)
        except (TypeError, ValueError):
            continue
        if age > 0:
            out[str(name)] = age
    return out


def _parse_ts(ts: str) -> float | None:
    try:
        return calendar.timegm(time.strptime(ts, "%Y-%m-%dT%H:%M:%SZ"))
    except (ValueError, TypeError):
        return None


def find_reusable(
    tool: str, arguments: dict[str, Any] | None, max_age_seconds: float
) -> dict[str, Any] | None:
    """The most recent still-fresh, successful, storable answer to this exact call, if any.

    "Exact call" is the journal's own fingerprint — the same tool against arguments that are
    identical once order and secrets are normalised away, which is what makes this reuse rather
    than a guess.
    """
    if max_age_seconds <= 0:
        return None
    target = fingerprint(tool, arguments)
    now = time.time()
    for entry in read_entries(limit=2000):
        if entry.get("tool") != tool or entry.get("fingerprint") != target:
            continue
        if not entry.get("ok") or "result" not in entry:
            continue
        stored_at = _parse_ts(str(entry.get("ts", "")))
        if stored_at is None or (now - stored_at) > max_age_seconds:
            continue
        return {"ts": entry["ts"], "result": entry["result"]}
    return None


def _redact_secret_keys(value: Any) -> Any:
    """Secret-named keys replaced, everything else untouched (no length truncation).

    Unlike ``safe_arguments``, this must preserve a reusable result byte-for-byte wherever it is
    not a secret: a truncated string played back as if it were the real answer would be a second,
    quieter version of the exact staleness bug this module exists to prevent.
    """
    if isinstance(value, dict):
        return {
            key: (
                "[redacted]"
                if any(hint in key.lower() for hint in SECRET_HINTS)
                else _redact_secret_keys(v)
            )
            for key, v in value.items()
        }
    if isinstance(value, list):
        return [_redact_secret_keys(v) for v in value]
    return value


def _reusable_payload(result: Any) -> Any | None:
    """The result, if it is a plain-JSON dict small enough to journal for later reuse."""
    if not isinstance(result, dict):
        return None
    redacted = _redact_secret_keys(result)
    try:
        encoded = json.dumps(redacted, ensure_ascii=False, default=str)
    except (TypeError, ValueError):
        return None
    if len(encoded) > MAX_REUSABLE_RESULT_BYTES:
        return None
    return json.loads(encoded)


def journaled(tool: str, function, *, sensitive_arguments: set[str] | frozenset[str] = frozenset()):
    """Wrap a handler so every call through any interface is recorded once.

    When this tool has an opt-in reuse policy (see ``reuse_policy``) and the journal holds a
    still-fresh, successful answer to the exact same call, that answer is returned instead of
    calling ``function`` again — the underlying request (a fetch, a paid lookup) never happens a
    second time. The reuse is never silent: the returned dict carries ``reused: true`` and
    ``reused_from_ts``, and a new journal entry records that this call was answered from reuse.
    """
    import functools

    @functools.wraps(function)
    def wrapper(**kwargs):
        sensitive_present = any(name in kwargs for name in sensitive_arguments)
        journal_arguments = {
            name: "[redacted]" if name in sensitive_arguments else value
            for name, value in kwargs.items()
        }
        max_age = 0 if sensitive_present else reuse_policy().get(tool, 0)
        if max_age:
            reusable = find_reusable(tool, journal_arguments, max_age)
            if reusable is not None:
                with journal(current_interface(), tool, journal_arguments) as facts:
                    facts["reused"] = True
                    facts["reused_from_ts"] = reusable["ts"]
                    result = copy.deepcopy(reusable["result"])
                    if isinstance(result, dict):
                        result["reused"] = True
                        result["reused_from_ts"] = reusable["ts"]
                    return result

        with journal(
            current_interface(), tool, journal_arguments, redact_error=sensitive_present
        ) as facts:
            result = function(**kwargs)
            if isinstance(result, dict):
                # Enough to answer "what did that run do" without opening the
                # report. The tool's own "ok" is kept under a separate name: the
                # journal's "ok" means the call did not raise, which is a
                # different claim from the tool reporting a usable result.
                for key in ("urls_collected", "out_dir", "scan", "count"):
                    if key in result:
                        facts[key] = result[key]
                if "ok" in result:
                    facts["result_ok"] = result["ok"]
            if max_age:
                payload = _reusable_payload(result)
                if payload is not None:
                    facts["result"] = payload
            return result

    return wrapper


def read_entries(limit: int = 100) -> list[dict[str, Any]]:
    """Most recent entries first. A missing journal is empty, not an error."""
    path = log_path()
    if path is None:
        return []
    # The rotated generation is older, so it goes first; reuse lookups must still see it.
    lines: list[str] = []
    for source in (backup_path(path), path):
        try:
            if source.exists():
                lines.extend(source.read_text(encoding="utf-8").splitlines())
        except (OSError, ValueError):
            continue
    entries: list[dict[str, Any]] = []
    for line in reversed(lines):
        if not line.strip():
            continue
        try:
            entries.append(json.loads(line))
        except ValueError:
            continue  # a truncated final line must not hide the rest
        if len(entries) >= limit:
            break
    return entries
