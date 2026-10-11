"""Offline per-IP DNS, retained-cache and interface evidence contracts."""

from __future__ import annotations

import io
import json
import sqlite3
import sys
import time
from collections import OrderedDict

import pytest

from seohead.checks import logs
from seohead.checks.logbots import bot_verification
from seohead.checks.logstore import import_logs
from seohead.cli import main
from seohead.projects.workspace import create_project


@pytest.fixture
def imported(tmp_path):
    project = tmp_path / "project"
    create_project(str(project), target="https://example.test")
    source = tmp_path / "synthetic.log"
    source.write_text(
        "".join(
            f'{ip} - - [11/Oct/2026:00:00:0{n} +0000] "GET /shop HTTP/1.1" 200 10 "-" "{bot}"\n'
            for n, (ip, bot) in enumerate(
                [
                    ("192.0.2.1", "Googlebot"),
                    ("192.0.2.2", "Googlebot"),
                    ("192.0.2.1", "Googlebot"),
                    ("2001:db8::1", "GPTBot"),
                ],
                1,
            )
        )
    )
    result = import_logs(str(source), str(project))
    assert result["ok"], result
    return result["source"]


@pytest.fixture
def dns(monkeypatch):
    calls = []
    monkeypatch.setattr(logs, "_DNS_CACHE", OrderedDict())
    monkeypatch.setattr(logs, "_dns_available", lambda: True)

    def verify(ip, suffixes):
        calls.append(ip)
        return {
            "verified": ip.endswith("1"),
            "hostname": "synthetic.googlebot.com",
            "forward_addresses": [ip],
            "forward_match": ip.endswith("1"),
            "reason": "Mocked DNS evidence",
        }

    monkeypatch.setattr(logs, "verify_bot_rdns", verify)
    return calls


def test_default_reads_are_offline_exact_and_paged(imported, monkeypatch):
    def unexpected(*args):
        pytest.fail("DNS ran without explicit opt-in")

    monkeypatch.setattr(logs, "_dns_available", unexpected)
    monkeypatch.setattr(logs, "verify_bot_rdns", unexpected)
    result = bot_verification(imported, limit=1)
    assert result["total"] == 3
    assert result["unique_ips"] == 3
    assert result["has_more"] and result["next_offset"] == 1
    assert result["rows"][0]["verdict"] == "unchecked"
    assert bot_verification(imported, ip="192.0.2.0/24")["total"] == 2
    assert bot_verification(imported, family="googlebot")["rows"][0]["visits"] == 2


def test_per_ip_verdicts_persist_and_filter_without_dns(imported, dns, monkeypatch):
    result = bot_verification(imported, verify_bots=True, family="googlebot", max_dns=1)
    assert dns == ["192.0.2.1"]
    assert [row["verdict"] for row in result["rows"]] == ["verified", "unchecked"]
    result = bot_verification(imported, verify_bots=True, family="googlebot", max_dns=1)
    assert dns == ["192.0.2.1", "192.0.2.2"]
    assert result["rows"][0]["cached"]
    monkeypatch.setattr(logs, "_DNS_CACHE", OrderedDict())
    monkeypatch.setattr(
        logs, "_dns_available", lambda: pytest.fail("Fresh retained cache invoked DNS")
    )
    result = bot_verification(imported, verify_bots=True, family="googlebot")
    assert all(row["cached"] for row in result["rows"])
    assert result["verification"]["dns_checks"] == 0
    fake = bot_verification(imported, verdict="fake")
    assert fake["total"] == fake["unique_ips"] == 1 and fake["rows"][0]["ip"] == "192.0.2.2"
    assert bot_verification(imported, verdict="verified")["rows"][0]["ip"] == "192.0.2.1"


def test_expired_cache_is_unchecked_and_unavailable_dns_is_not_fake(imported, dns, monkeypatch):
    bot_verification(imported, verify_bots=True, family="googlebot")
    with sqlite3.connect(imported) as con:
        con.execute("UPDATE dns_cache SET checked_at=?", (time.time() - 7200,))
    monkeypatch.setattr(logs, "_DNS_CACHE", OrderedDict())
    monkeypatch.setattr(logs, "_dns_available", lambda: False)
    result = bot_verification(imported, verify_bots=True, family="googlebot")
    assert all(row["verdict"] == "unchecked" for row in result["rows"])
    assert result["verification"]["dns_available"] is False
    assert len(dns) == 2


@pytest.mark.parametrize(
    "kwargs",
    [
        {"verify_bots": "yes"},
        {"limit": 501},
        {"offset": -1},
        {"max_dns": -1},
        {"verdict": "clean"},
        {"verify_bots": True, "verdict": "fake"},
        {"ip": "garbage"},
    ],
)
def test_invalid_inputs_are_rejected(imported, kwargs):
    with pytest.raises(ValueError):
        bot_verification(imported, **kwargs)


def test_missing_source_does_not_create_file(tmp_path):
    source = tmp_path / "missing.sqlite"
    assert bot_verification(str(source))["ok"] is False
    assert not source.exists()


def test_nested_cli_reads_same_evidence(imported, capsys, monkeypatch):
    monkeypatch.setattr(sys, "stdin", io.StringIO(""))
    assert main(["logs", "bots", "--source", imported, "--ip", "192.0.2.2"]) == 0
    result = json.loads(capsys.readouterr().out)
    assert result["total"] == 1
    assert result["rows"][0]["ip"] == "192.0.2.2"
    assert (
        main(
            ["logs", "summary", "--source", imported, "--view", "bot_ips", "--family", "googlebot"]
        )
        == 0
    )
    assert json.loads(capsys.readouterr().out)["total"] == 2


def test_cache_cannot_replace_observed_ip_visits_or_bot(imported):
    with sqlite3.connect(imported) as con:
        con.execute(
            "INSERT INTO dns_cache VALUES(?,?,?,?)",
            (
                "192.0.2.1",
                "Googlebot",
                json.dumps({"verified": True, "ip": "forged", "visits": 999, "bot": "Forged"}),
                time.time(),
            ),
        )
    row = bot_verification(imported, ip="192.0.2.1")["rows"][0]
    assert row["ip"] == "192.0.2.1" and row["visits"] == 2 and row["bot"] == "Googlebot"


def test_oversized_or_wrong_type_cache_payload_remains_unchecked(imported):
    with sqlite3.connect(imported) as con:
        con.execute(
            "INSERT INTO dns_cache VALUES(?,?,?,?)",
            ("192.0.2.1", "Googlebot", "x" * 1000000, time.time()),
        )
        con.execute(
            "INSERT INTO dns_cache VALUES(?,?,?,?)",
            ("192.0.2.2", "Googlebot", sqlite3.Binary(b"fake"), time.time()),
        )
    result = bot_verification(imported, verdict="unchecked", family="googlebot")
    assert result["total"] == 2
    assert all(row["verdict"] == "unchecked" for row in result["rows"])
