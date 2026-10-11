"""Paged per-IP bot claims and explicit cached DNS evidence from local log artifacts."""

from __future__ import annotations

import ipaddress
import json
import sqlite3
import time
from contextlib import closing
from pathlib import Path
from typing import Any

from seohead.checks.logs import DNS_CACHE_TTL, _verify_rows
from seohead.checks.logstore import open_log_source


def _cache(payload: str | None, checked_at: float | None, now: float) -> dict[str, Any]:
    if (
        payload
        and len(payload) <= 65536
        and type(checked_at) in (int, float)
        and 0 <= now - checked_at < DNS_CACHE_TTL
    ):
        try:
            result = json.loads(payload)
            if isinstance(result, dict) and (
                type(result.get("verified")) is bool or result.get("verified") is None
            ):
                verified = result.get("verified")
                return {
                    **{
                        key: result.get(key)
                        for key in (
                            "verified",
                            "hostname",
                            "forward_addresses",
                            "forward_match",
                            "reason",
                        )
                    },
                    "cached": True,
                    "checked_at": checked_at,
                    "verdict": "verified"
                    if verified is True
                    else "fake"
                    if verified is False
                    else "unchecked",
                }
        except (ValueError, TypeError):
            pass
    return {
        "verified": None,
        "hostname": None,
        "forward_addresses": [],
        "forward_match": None,
        "verdict": "unchecked",
        "cached": False,
        "checked_at": None,
        "reason": "No fresh DNS evidence; explicit verification is required",
    }


def bot_verification(
    source: str,
    *,
    verify_bots: bool = False,
    limit: int = 100,
    offset: int = 0,
    bot: str = "",
    family: str = "",
    ip: str = "",
    verdict: str = "",
    max_dns: int = 20,
) -> dict[str, Any]:
    """Read exact paged IP records; network and local cache writes require verify_bots."""
    if type(verify_bots) is not bool:
        raise ValueError("verify_bots must be a boolean")
    for name, value, minimum, maximum in (
        ("limit", limit, 1, 500),
        ("offset", offset, 0, 100_000_000),
        ("max_dns", max_dns, 0, 100),
    ):
        if type(value) is not int or not minimum <= value <= maximum:
            raise ValueError(f"{name} must be an integer in {minimum}..{maximum}")
    if verdict not in ("", "verified", "fake", "unchecked"):
        raise ValueError("verdict must be verified, fake or unchecked")
    if verify_bots and verdict:
        raise ValueError(
            "DNS verification with a cached verdict filter is not supported; verify an unfiltered page first"
        )
    network = ipaddress.ip_network(ip, strict=False) if ip else None
    now = time.time()
    rows = []
    total = unique_ips = 0
    params = []
    clauses = []
    for column, value in (("b.bot", bot), ("b.family", family)):
        if value:
            clauses.append(f"{column}=?")
            params.append(value)
    if network:
        clauses.append("ip_in_subnet(b.ip)=1")
    where = " WHERE " + " AND ".join(clauses) if clauses else ""
    try:
        with closing(open_log_source(source)) as con:
            if network:

                def in_subnet(value):
                    try:
                        return int(ipaddress.ip_address(value) in network)
                    except ValueError:
                        return 0

                con.create_function("ip_in_subnet", 1, in_subnet, deterministic=True)
            joined = " FROM bot_ips b LEFT JOIN dns_cache c ON b.ip=c.ip AND b.bot=c.bot"
            payload = "CASE WHEN typeof(c.payload)='text' AND length(CAST(c.payload AS BLOB))<=65536 THEN c.payload ELSE NULL END"
            if verdict:
                con.create_function(
                    "cached_verdict",
                    2,
                    lambda payload, stamp: _cache(payload, stamp, now)["verdict"],
                )
                where += (
                    " AND " if where else " WHERE "
                ) + f"cached_verdict({payload},c.checked_at)=?"
                params.append(verdict)
            unique_ips = con.execute(
                "SELECT count(DISTINCT b.ip)" + joined + where, params
            ).fetchone()[0]
            query = (
                f"SELECT b.*, {payload} AS payload, c.checked_at"
                + joined
                + where
                + " ORDER BY b.bot,b.ip"
            )
            for record in con.execute(query, params):
                cached = _cache(record["payload"], record["checked_at"], now)
                if verdict and cached["verdict"] != verdict:
                    continue
                if offset <= total < offset + limit:
                    rows.append(
                        {
                            **{
                                key: record[key]
                                for key in ("bot", "family", "ip", "visits", "last_visit")
                            },
                            **cached,
                        }
                    )
                total += 1
        verification = {"checked": False, "dns_checks": 0, "canary_requests": 0}
        if verify_bots:
            pending = [row for row in rows if not row["cached"]]
            verification = _verify_rows(pending, True, max_dns)
            refreshed = {(row["bot"], row["ip"]): row for row in verification["checks"]}
            rows = [refreshed.get((row["bot"], row["ip"]), row) for row in rows]
            updates = [row for row in verification["checks"] if row.get("checked_at") is not None]
            if updates:
                # Validate the artifact again and refuse creating any destination.
                with closing(open_log_source(source)):
                    pass
                uri = Path(source).resolve().as_uri() + "?mode=rw"
                with closing(sqlite3.connect(uri, uri=True)) as writable, writable:
                    writable.executemany(
                        "INSERT INTO dns_cache VALUES(?,?,?,?) ON CONFLICT(ip,bot) DO UPDATE SET payload=excluded.payload,checked_at=excluded.checked_at",
                        [
                            (
                                row["ip"],
                                row["bot"],
                                json.dumps(
                                    {
                                        key: value
                                        for key, value in row.items()
                                        if key
                                        not in ("visits", "last_visit", "bot", "family", "ip")
                                    }
                                ),
                                row["checked_at"],
                            )
                            for row in updates
                        ],
                    )
        return {
            "ok": True,
            "source": source,
            "rows": rows,
            "total": total,
            "unique_ips": unique_ips,
            "offset": offset,
            "limit": limit,
            "has_more": total > offset + len(rows),
            "next_offset": offset + len(rows) if total > offset + len(rows) else None,
            "verification": {key: value for key, value in verification.items() if key != "checks"},
            "identity_note": "Bot names are user-agent claims; unchecked is not an authenticity verdict",
        }
    except (OSError, sqlite3.Error, ValueError) as exc:
        return {"ok": False, "source": source, "error": f"Log bot evidence unavailable: {exc}"}
