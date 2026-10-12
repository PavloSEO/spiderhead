"""Shared pytest fixtures."""

from __future__ import annotations

import os
import shutil

import pytest

from seohead.sf.core.audit import run_audit

FIXTURES = os.path.join(os.path.dirname(__file__), "fixtures")


def pytest_configure(config):
    """Switch the run journal off for the whole session, before collection.

    The journal ignores SEOHEAD_CONFIG_DIR, so without this any CLI call made by a test (or at
    import time) appends to the real ~/.config/seohead/runs.jsonl. Tests that need the journal
    set SEOHEAD_RUN_LOG themselves.
    """
    os.environ["SEOHEAD_RUN_LOG"] = "off"


@pytest.fixture
def exports_dir(tmp_path):
    """A temp exports dir holding top-level export fixture files."""
    dst = tmp_path / "exports"
    dst.mkdir()
    for entry in os.scandir(FIXTURES):
        if entry.is_file():
            shutil.copy(entry.path, dst / entry.name)
    return str(dst)


@pytest.fixture
def internal_only_dir(tmp_path):
    """A temp exports dir with only Internal:All (no inlinks)."""
    dst = tmp_path / "exports_internal"
    dst.mkdir()
    shutil.copy(os.path.join(FIXTURES, "internal_all.csv"), dst / "internal_all.csv")
    return str(dst)


@pytest.fixture
def result(exports_dir):
    return run_audit(input_mode="parse-exports", exports_dir=exports_dir, log=lambda m: None)


def checks_in(result) -> set[str]:
    return {i.check for i in result.issues}


def issues_of(result, check):
    return [i for i in result.issues if i.check == check]


@pytest.fixture(autouse=True)
def isolated_user_configuration(tmp_path, monkeypatch):
    """Isolate shared MCP state; client tests additionally provide a temporary HOME."""
    home = tmp_path / "isolated-home"
    monkeypatch.setenv("SEOHEAD_CONFIG_DIR", str(home / ".config/seohead"))
    # The run journal ignores SEOHEAD_CONFIG_DIR; without this every test that calls a
    # journaled handler would append to the real ~/.config/seohead/runs.jsonl.
    monkeypatch.setenv("SEOHEAD_RUN_LOG", "off")


@pytest.fixture
def semantics_offline(monkeypatch, tmp_path):
    """Semantic-core tests never spend quota: sockets fail and the spend journal is private."""
    import socket

    def forbidden(*_args, **_kwargs):
        raise AssertionError("network access is forbidden in the semantics test suite")

    monkeypatch.setattr(socket, "create_connection", forbidden)
    monkeypatch.setattr(socket.socket, "connect", forbidden)
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))


@pytest.fixture
def sem_store(tmp_path):
    from seohead.semantics.store import Store

    result = Store(tmp_path / "sya.db")
    try:
        yield result
    finally:
        result.close()


@pytest.fixture
def sem_cfg(tmp_path):
    return {
        "_project": "fixture-project",
        "_dir": str(tmp_path),
        "preset": "seo",
        "regions": ["225"],
        "min_base": 20,
        "exact_cut_base": 20,
        "exact_cut_exact": 10,
        "budget_arsenkin_gate": 2_000,
        "reexpand_floor": 40,
        "max_depth": 0,
        "num": 10,
        "rps": 100,
    }
