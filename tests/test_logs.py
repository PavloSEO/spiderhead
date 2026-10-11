"""Offline log-analysis tests that do not perform reverse-DNS lookups."""

from __future__ import annotations

import socket
from datetime import timezone

import pytest

from seohead.checks.logs import (
    GOOGLE,
    _findings,
    _section,
    analyze_log,
    detect_bot,
    detect_format,
    parse_apache_timestamp,
    verify_bot_rdns,
)

COMBINED = (
    '66.249.66.1 - - [18/Mar/2024:00:02:09 +0000] "GET /catalog/pumps HTTP/1.1" 200 5120 '
    '"-" "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"\n'
    '66.249.66.1 - - [18/Mar/2024:00:03:10 +0000] "GET /old-page HTTP/1.1" 404 0 '
    '"-" "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"\n'
    '20.15.1.2 - - [18/Mar/2024:01:00:00 +0000] "GET /blog/post HTTP/1.1" 200 8000 '
    '"-" "Mozilla/5.0 AppleWebKit (KHTML, like Gecko) Chrome/120 Safari/537.36"\n'
    '52.70.1.1 - - [19/Mar/2024:02:00:00 +0000] "GET /pricing HTTP/1.1" 200 3000 '
    '"-" "Mozilla/5.0 (compatible; GPTBot/1.0; +https://openai.com/gptbot)"\n'
    '1.2.3.4 - - [19/Mar/2024:03:00:00 +0000] "GET /catalog/x HTTP/1.1" 503 0 '
    '"-" "Mozilla/5.0 (compatible; AhrefsBot/7.0; +http://ahrefs.com/robot/)"\n'
)

COMMON = '24.5.66.10 - - [18/Mar/2024:00:02:09 +0000] "GET /old-pricing HTTP/1.1" 301 0\n'

IIS = (
    "#Software: Microsoft Internet Information Services 10.0\n"
    "#Fields: date time cs-uri-stem cs-uri-query cs-method c-ip cs(User-Agent) sc-status sc-bytes\n"
    "2024-03-18 00:02:09 /catalog/pumps - GET 66.249.66.1 "
    "Mozilla/5.0+(compatible;+Googlebot/2.1) 200 5120\n"
)


# ── Bot detection ────────────────────────────────────────────────────────────


def test_specific_signature_wins_over_generic():
    """Googlebot-Image must match before the generic Googlebot signature."""
    assert detect_bot("Googlebot-Image/1.0")["name"] == "Googlebot Image"
    assert detect_bot("Mozilla/5.0 (compatible; Googlebot/2.1)")["name"] == "Googlebot"


def test_human_is_not_a_bot():
    assert detect_bot("Mozilla/5.0 (Macintosh) Chrome/120 Safari/537.36") is None
    assert detect_bot(None) is None
    assert detect_bot("-") is None


def test_ai_crawlers_are_recognised_and_not_verifiable():
    """AI crawlers without official PTR ranges must not be marked as verifiable."""
    for ua, name in [
        ("GPTBot/1.0", "GPTBot (OpenAI)"),
        ("ClaudeBot/1.0", "ClaudeBot (Anthropic)"),
        ("PerplexityBot/1.0", "PerplexityBot"),
    ]:
        bot = detect_bot(ua)
        assert bot["name"] == name
        assert bot["family"] == "ai"
        assert bot["verifiable"] is False


def test_search_engines_are_verifiable():
    for ua in ("Googlebot/2.1", "bingbot/2.0", "YandexBot/3.0"):
        assert detect_bot(ua)["verifiable"] is True


# ── Timestamps ───────────────────────────────────────────────────────────────


def test_timezone_is_applied_in_the_right_direction():
    """A -0700 offset adds hours to UTC, while +0300 subtracts them."""
    west = parse_apache_timestamp("10/Oct/2000:13:55:36 -0700")
    assert (west.hour, west.tzinfo) == (20, timezone.utc)
    east = parse_apache_timestamp("10/Oct/2000:13:55:36 +0300")
    assert east.hour == 10


def test_broken_timestamp_returns_none_not_crash():
    assert parse_apache_timestamp("not a date") is None
    assert parse_apache_timestamp("32/Xxx/2024:00:00:00 +0000") is None


# ── Log formats ──────────────────────────────────────────────────────────────


def test_format_detection():
    assert detect_format(COMBINED.splitlines()) == "combined"
    assert detect_format(COMMON.splitlines()) == "common"
    assert detect_format(IIS.splitlines()) == "iis"
    assert detect_format(["garbage", "more garbage"]) is None


def test_sections_are_first_path_segment():
    assert _section("/catalog/pumps/cdm") == "/catalog"
    assert _section("/") == "/"
    assert _section("/pricing?utm=1") == "/pricing"


# ── End-to-end parsing ───────────────────────────────────────────────────────


def _write(tmp_path, text, name="access.log"):
    p = tmp_path / name
    p.write_text(text, encoding="utf-8")
    return str(p)


def test_combined_log_is_parsed_and_grouped(tmp_path):
    r = analyze_log(_write(tmp_path, COMBINED))
    assert r["ok"] and r["format"] == "combined"
    assert r["lines"]["parsed"] == 5 and r["lines"]["skipped"] == 0
    assert "googlebot" in r["by_family"] and "ai" in r["by_family"]
    assert "human" in r["by_family"] and "seo-tool" in r["by_family"]
    # Googlebot requested both the /catalog section and /old-page.
    assert "/catalog" in r["sections_by_family"]["googlebot"]
    # Bot response codes are aggregated separately from human requests.
    assert r["status_by_family"]["googlebot"][404] == 1


def test_iis_reads_field_positions_from_directive(tmp_path):
    r = analyze_log(_write(tmp_path, IIS, "u_ex.log"))
    assert r["ok"] and r["format"] == "iis"
    assert r["lines"]["parsed"] == 1
    assert "googlebot" in r["by_family"]


def test_verification_is_off_by_default(tmp_path):
    """Network verification must remain disabled without explicit permission."""
    r = analyze_log(_write(tmp_path, COMBINED))
    assert r["verification"]["checked"] is False


def test_missing_file_is_data_not_a_crash():
    r = analyze_log("/nope/does-not-exist.log")
    assert r["ok"] is False and "error" in r


def test_unparsable_file_says_so(tmp_path):
    r = analyze_log(_write(tmp_path, "definitely not a log\nsecond line\n"))
    assert r["ok"] is False
    assert "Apache Common/Combined" in r["error"]
    assert "IIS W3C" in r["error"]


def test_findings_flag_error_rate_for_bots(tmp_path):
    """The fixture gives AhrefsBot one 503 response, producing a 100% error rate."""
    r = analyze_log(_write(tmp_path, COMBINED))
    assert any(finding.startswith("seo-tool:") and "(100%)" in finding for finding in r["findings"])


def test_findings_mention_ai_crawlers(tmp_path):
    r = analyze_log(_write(tmp_path, COMBINED))
    assert any("GPTBot (OpenAI)" in finding for finding in r["findings"])


# ── High-cardinality accumulator caps ────────────────────────────────────────


def test_top_paths_keep_incrementing_after_the_path_cap_fills(tmp_path, monkeypatch):
    """#251: once the tracked-path set is full, a repeat of an already-tracked path must
    still increment -- only a brand-new path is refused. The bug capped every increment
    on the cap, so a path counted before the cap filled silently stopped growing while
    the family total it should sum to kept climbing, making top_paths_by_family false."""
    from seohead.checks import logs

    monkeypatch.setattr(logs, "MAX_TRACKED_PATHS", 2)
    lines = "\n".join(
        [
            '192.0.2.1 - - [18/Mar/2024:00:00:01 +0000] "GET /first HTTP/1.1" 200 1 "-" '
            '"Googlebot/2.1"',
            '192.0.2.1 - - [18/Mar/2024:00:00:02 +0000] "GET /second HTTP/1.1" 200 1 "-" '
            '"Googlebot/2.1"',
            '192.0.2.1 - - [18/Mar/2024:00:00:03 +0000] "GET /third HTTP/1.1" 200 1 "-" '
            '"Googlebot/2.1"',
            '192.0.2.1 - - [18/Mar/2024:00:00:04 +0000] "GET /first HTTP/1.1" 200 1 "-" '
            '"Googlebot/2.1"',
        ]
    )
    r = analyze_log(_write(tmp_path, lines + "\n"))
    assert r["by_family"]["googlebot"]["Googlebot"] == 4  # every hit counted in the family total
    assert r["top_paths_by_family"]["googlebot"]["/first"] == 2  # both /first hits, not just one
    assert r["paths_truncated"] == ["googlebot"]  # /third was refused once the cap was full


def _row(ip: str, n: int) -> str:
    return (
        f'{ip} - - [18/Mar/2024:00:00:{n % 60:02d} +0000] "GET / HTTP/1.1" 200 1 "-" '
        '"Googlebot/2.1"\n'
    )


def test_unique_ips_are_exact_beyond_the_old_cap(tmp_path):
    lines = "".join(_row(f"192.0.{i // 256}.{i % 256}", i) for i in range(2100))
    r = analyze_log(_write(tmp_path, lines), offset=2098, limit=2)
    assert r["bots"][0]["unique_ips"] == 2100
    assert r["unique_bot_ips"] == 2100
    assert r["bot_ips"]["total"] == 2100
    assert len(r["bot_ips"]["rows"]) == 2
    assert r["ips_truncated"] == []
    assert not r["bot_ips"]["has_more"]


def test_ip_page_counts_repeats_last_visit_and_subnet(tmp_path):
    r = analyze_log(
        _write(tmp_path, _row("192.0.2.1", 1) + _row("192.0.2.2", 2) + _row("192.0.2.1", 3)),
        ip="192.0.2.1/32",
    )
    assert r["bot_ips"]["total"] == 1
    row = r["bot_ips"]["rows"][0]
    assert row["visits"] == 2
    assert row["last_visit"].endswith("00:00:03+00:00")
    assert row["verdict"] == "unchecked"


def test_per_ip_verification_does_not_extrapolate_family(tmp_path, monkeypatch):
    from seohead.checks import logs

    calls = []

    def verify(ip, suffixes):
        calls.append(ip)
        return {
            "verified": ip.endswith("1"),
            "hostname": "crawl.googlebot.com",
            "forward_match": ip.endswith("1"),
            "forward_addresses": [ip],
            "reason": "synthetic",
        }

    monkeypatch.setattr(logs, "_dns_available", lambda: True)
    monkeypatch.setattr(logs, "verify_bot_rdns", verify)
    monkeypatch.setattr(logs, "_DNS_CACHE", logs.OrderedDict())
    path = _write(tmp_path, _row("192.0.2.1", 1) + _row("192.0.2.2", 2))
    r = analyze_log(path, verify_bots=True, max_dns=1)
    assert calls == ["192.0.2.1"]
    assert [row["verdict"] for row in r["bot_ips"]["rows"]] == ["verified", "unchecked"]
    r = analyze_log(path, verify_bots=True, max_dns=2)
    assert calls == ["192.0.2.1", "192.0.2.2"]
    assert [row["verdict"] for row in r["bot_ips"]["rows"]] == ["verified", "fake"]
    assert r["bot_ips"]["rows"][0]["cached"]


@pytest.mark.parametrize(
    "kwargs",
    [
        {"limit": 0},
        {"max_lines": True},
        {"offset": -1},
        {"max_dns": 101},
        {"verify_bots": "yes"},
        {"ip": "not-an-ip"},
    ],
)
def test_log_analysis_validates_bounds(kwargs):
    with pytest.raises(ValueError):
        analyze_log("synthetic.log", **kwargs)


def test_dns_deadline_does_not_change_global_socket_timeout(monkeypatch):
    import threading

    event = threading.Event()

    def blocked(ip):
        event.wait(1)
        return ("outside.example.test", [], [ip])

    monkeypatch.setattr(socket, "gethostbyaddr", blocked)
    old = socket.getdefaulttimeout()
    try:
        result = verify_bot_rdns("192.0.2.99", GOOGLE, timeout=0.01)
        assert result["verified"] is None
        assert "deadline" in result["reason"]
        assert socket.getdefaulttimeout() == old
    finally:
        event.set()


def test_ipv6_forward_confirmation_and_suffix_boundary(monkeypatch):
    monkeypatch.setattr(socket, "gethostbyaddr", lambda ip: ("crawl.googlebot.com.", [], [ip]))
    monkeypatch.setattr(
        socket, "getaddrinfo", lambda *args: [(None, None, None, None, ("2001:db8::1", 0, 0, 0))]
    )
    assert verify_bot_rdns("2001:db8::1", GOOGLE)["forward_match"] is True
    monkeypatch.setattr(socket, "gethostbyaddr", lambda ip: ("evilgooglebot.com", [], [ip]))
    assert verify_bot_rdns("2001:db8::1", GOOGLE)["verified"] is False


def test_max_lines_does_not_consume_the_rest_of_the_file():
    """#252: ``[*sample, *handle]`` materialized every remaining line before parsing the
    first one, so max_lines only marked excess rows skipped after the whole file (and its
    memory) was already consumed. A one-line cap must stop reading, not read everything
    and then discard most of it."""
    import builtins
    from unittest.mock import patch

    from seohead.checks import logs

    line = '192.0.2.1 - - [18/Mar/2024:00:00:01 +0000] "GET / HTTP/1.1" 200 1 "-" "Googlebot/2.1"\n'

    class SyntheticHandle:
        def __init__(self, remaining: int) -> None:
            self._sample_pending = True
            self.remaining = remaining
            self.iterated = 0

        def readline(self) -> str:
            if self._sample_pending:
                self._sample_pending = False
                return line
            return ""

        def __iter__(self):
            for _ in range(self.remaining):
                self.iterated += 1
                yield line

        def read(self, _size: int = -1) -> bytes:
            # The gzip-magic probe reads raw bytes first; a synthetic text file has none.
            return b""

        def close(self) -> None:
            return None

        def __enter__(self):
            return self

        def __exit__(self, *_args: object) -> bool:
            return False

    handle = SyntheticHandle(remaining=25)
    with patch.object(builtins, "open", return_value=handle):
        result = logs.analyze_log("synthetic-access.log", max_lines=1, sample_size=1)

    # One line must still be pulled to discover there *is* a line past the cap -- a
    # generator has no way to report "more" without being asked for its next item --
    # but the old code pulled and iterated all 25 remaining lines before parsing even
    # the first one; stopping at 1 is the whole fix.
    assert handle.iterated == 1, "a one-line cap must not pull the file's remaining lines"
    assert result["lines"] == {"total": 1, "parsed": 1, "skipped": 0, "truncated": True}


# ── Forward-confirmed reverse DNS (#485) ─────────────────────────────────────


def test_verify_bot_rdns_forward_lookup_unavailable_is_not_a_mismatch(monkeypatch):
    """A real Googlebot must not be reported as fake when forward DNS is down."""
    monkeypatch.setattr(
        socket,
        "gethostbyaddr",
        lambda ip: ("crawl-66-249-66-1.googlebot.com", [], [ip]),
    )

    def raise_forward(_hostname):
        raise OSError("Network is unreachable")

    monkeypatch.setattr(socket, "gethostbyname_ex", raise_forward)

    result = verify_bot_rdns("66.249.66.1", GOOGLE)
    assert result["verified"] is None
    assert "forward" in result["reason"].lower()
    assert "unavailable" in result["reason"].lower()

    findings = _findings(
        {
            "lines": {"parsed": 1, "skipped": 0, "total": 1},
            "by_family": {"googlebot": {"66.249.66.1": 1}},
            "status_by_family": {},
            "verification": {
                "checked": True,
                "dns_available": True,
                "checks": [{"bot": "googlebot", "ip": "66.249.66.1", **result}],
            },
        }
    )
    assert not any("impersonating" in f for f in findings)


def test_verify_bot_rdns_real_mismatch_still_flags(monkeypatch):
    """A genuine forward/reverse mismatch (forward DNS worked, addresses differ) must
    still read as an unverified bot -- the fix must not silence real mismatches."""
    monkeypatch.setattr(
        socket,
        "gethostbyaddr",
        lambda ip: ("crawl-1-2-3-4.googlebot.com", [], [ip]),
    )
    monkeypatch.setattr(
        socket,
        "gethostbyname_ex",
        lambda hostname: (hostname, [], ["9.9.9.9"]),
    )

    result = verify_bot_rdns("1.2.3.4", GOOGLE)
    assert result["verified"] is False
    assert "does not resolve back to" in result["reason"]


def test_gzip_log_is_read_by_content_not_extension(tmp_path):
    """Rotated archives keep their .gz name only sometimes; the gzip header is the signal."""
    import gzip

    plain = tmp_path / "access.log.1"
    plain.write_bytes(gzip.compress(COMBINED.encode("utf-8")))
    renamed = tmp_path / "access.log"
    renamed.write_bytes(gzip.compress(COMBINED.encode("utf-8")))
    for path in (plain, renamed):
        r = analyze_log(str(path))
        assert r["ok"] and r["format"] == "combined"
        assert r["lines"]["parsed"] == 5
        assert "googlebot" in r["by_family"]


def test_truncated_gzip_is_an_error_not_a_partial_result(tmp_path):
    import gzip

    data = gzip.compress((COMBINED * 200).encode("utf-8"))
    path = tmp_path / "access.log.2.gz"
    path.write_bytes(data[: len(data) // 2])
    r = analyze_log(str(path))
    assert r["ok"] is False
    assert "Cannot read log" in r["error"]


def test_common_log_does_not_claim_absent_bots_from_missing_user_agents(tmp_path):
    result = analyze_log(_write(tmp_path, COMMON))
    assert result["bot_detection"]["available"] is False
    assert any("unavailable" in message for message in result["findings"])
    assert not any("Googlebot does not appear" in message for message in result["findings"])


def test_stalled_dns_workers_are_bounded_across_calls(monkeypatch):
    import threading

    from seohead.checks import logs

    event = threading.Event()
    monkeypatch.setattr(logs, "_DNS_WORKERS", threading.BoundedSemaphore(1))

    def blocked(ip):
        event.wait(1)
        return ("outside.example.test", [], [ip])

    monkeypatch.setattr(socket, "gethostbyaddr", blocked)
    try:
        first = verify_bot_rdns("192.0.2.1", GOOGLE, timeout=0.01)
        assert first["verified"] is None and "deadline" in first["reason"]
        second = verify_bot_rdns("192.0.2.2", GOOGLE, timeout=0.01)
        assert second["verified"] is None and "capacity" in second["reason"]
    finally:
        event.set()
