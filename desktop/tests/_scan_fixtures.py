"""Run and scan records in the shape project-observe / project-scans return (keys taken from a real core answer on the
loopback QA site; the live values — 5 of 18 URLs, 1 request/s over a 5 s window — are from a real run observed mid-crawl)."""

from datetime import datetime, timedelta, timezone

NOW = datetime(2026, 10, 9, 9, 10, 0, tzinfo=timezone.utc)
HOST = "crawl.localhost"


def stamp(seconds_ago):
    return (NOW - timedelta(seconds=seconds_ago)).isoformat().replace("+00:00", "Z")


def events(*codes, start=60):
    return [{"at": stamp(start - 2 * index), "code": code, "phase": phase} for index, (code, phase) in enumerate(codes)]


def run(identity="22d43d8238fd4594966239154703455c", state="finished", started=600, artifact="scans/20261009T082104Z_crawl.localhost_0609ac70.sqlite",
        counters=None, telemetry=None, kind="native", finish_reason="finished", event_list=None, finished=590):
    base_counters = {"excluded": 0, "fetched": 20, "inflight": 0, "queued": 0, "rate_per_second": 2.000844128854619}
    base_telemetry = {"queue_semantics": "separate", "rate_window_seconds": 5.497679624997545, "sampled_at": stamp(finished or 0),
                      "source": "native_frontier", "unit": "pages", "age_seconds": 2095.0, "state": "retained", "current_rate_per_second": None}
    return {
        "artifact": artifact,
        "collector": {"aggregate_max_requests_per_second": 2.0, "config_fingerprint": "6d02e4b29c259e3d", "max_crawl_seconds": 0, "max_requests": 20000,
                      "max_requests_per_second": 2.0, "max_urls": 60, "mode": "spider", "origin": HOST, "resumed": False},
        "collector_pid": None, "controller_pid": 62049, "counters": {**base_counters, **(counters or {})},
        "events": event_list if event_list is not None else events(("started", "admission"), ("entered", "collection"), ("progress", "collection"), ("finished", "finalizing")),
        "finish_reason": finish_reason, "finished_at": stamp(finished) if finished is not None else None, "id": identity, "kind": kind, "pid": 62049,
        "source_metadata": None, "started_at": stamp(started), "state": state, "telemetry": {**base_telemetry, **(telemetry or {})},
        "source_kind": kind, "pid_state": "retained" if state != "running" else "alive",
    }


def live_run(identity="5157e4864aa14a2981dd9b96047ae5e0", **extra):
    """A native run observed mid-crawl: 5 fetched, 13 queued, 0 in flight, 1 request/s over the 5 s window."""
    counters = {"fetched": 5, "queued": 13, "inflight": 0, "excluded": 0, "rate_per_second": 1.0, **extra.pop("counters", {})}
    return run(identity, "running", started=8, artifact=None, finished=None, finish_reason=None, counters=counters,
               telemetry={"state": "fresh", "age_seconds": 0.4, "sampled_at": stamp(0.4), "current_rate_per_second": 1.0, "rate_window_seconds": 5.0, **extra.pop("telemetry", {})},
               event_list=events(("started", "admission"), ("entered", "collection"), ("progress", "collection"), ("progress", "collection")), **extra)


def stale_run(identity="9e2f0d4cf5e84f5aa1d7a10b8cc64a11"):
    return live_run(identity, telemetry={"state": "stale", "age_seconds": 95.3, "current_rate_per_second": None})


def scan(uuid="d49c2811-695d-4174-92d4-e1714a0ef566", artifact="scans/20261009T082104Z_crawl.localhost_0609ac70.sqlite", lifecycle="finished",
         crawl_partial=False, corpus_partial=True, done=20, created=598, finished=590, finish_reason="finished"):
    return {
        "uuid": uuid, "path": "/project/qa/" + artifact, "bytes": 258048, "disk_bytes": 827392, "lifecycle": lifecycle, "finish_reason": finish_reason,
        "crawl_partial": crawl_partial, "corpus_partial": corpus_partial, "start_url": f"http://{HOST}:54517/", "host": HOST, "source_kind": "native",
        "created_at": stamp(created), "finished_at": stamp(finished),
        "evidence": {"frontier": {"state": "available", "reason": "", "counts": {"queued": 0, "inflight": 0, "done": done, "excluded": 0}}},
    }


def owned(identity="a1f00bf2e04b4d4a8f7c3d4b5c6d7e8f", state="running", core="5157e4864aa14a2981dd9b96047ae5e0", **extra):
    return {"id": identity, "project": "/project/qa", "project_uuid": "p", "kind": "crawl", "state": state, "core_run_id": core, "observer_run_id": core,
            "artifact": None, "core_state": None, "max_urls": 60, "rendering_mode": "raw", "resume_path": None, "sitemap_url": None, "input_mode": "spider",
            "max_urls_per_second": None, "owned": True, "status_reason": None, **extra}
