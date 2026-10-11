"""Saved real core answers (tests/core_fixtures) behind the same redacting adapter the app uses; no CLI, no network."""

from seohead_desktop.source_service import load_sources
from tests._screens_core import fixture

PROJECT = "<project>"
AUTH = {"ok": True, "configured": False, "access_verified": False}  # real `provider-auth --provider gsc --action status` on a machine without a grant


def core_call(overrides=None):
    """call(executable, args) -> saved answer of that command; ``overrides`` maps a command name to a dict or an exception."""
    answers = {"provider-registry": "provider_registry.json", "provider-readiness": "provider_readiness.json", "spend-report": "spend_report.json",
               "sources-doctor": "sources_doctor.json", "sources-status": "sources_status.json"}
    calls = []

    def call(_executable, args):
        calls.append(list(args))
        command = args[0]
        if overrides and command in overrides:
            value = overrides[command]
            if isinstance(value, Exception):
                raise value
            return value
        if command == "provider-auth":
            return dict(AUTH)
        return fixture(answers[command])

    call.calls = calls
    return call


def sources_hook(overrides=None, pending=None):
    """SettingsContext action ``sources``; ``pending`` (a list) collects requests instead of answering them (loading state)."""
    call = core_call(overrides)

    def request(operation, callback, on_error, owner, project=None, provider=None, since=None):
        if pending is not None:
            pending.append((operation, callback, on_error))
            return
        try:
            callback(load_sources("core", operation, project, provider, since, call=call))
        except ValueError as exc:
            on_error(str(exc))

    request.call = call
    return request
