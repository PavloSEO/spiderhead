"""Project routing is shared across CLI, MCP and direct handlers."""

from pathlib import Path

import pytest

from seohead import cli
from seohead.mcp import handlers
from seohead.mcp.mcp_server import build_server
from seohead.projects.run_observation import status as run_status
from seohead.projects.workspace import create_project


def test_mcp_project_creation_preserves_custom_references(tmp_path):
    tool = build_server()._tool_manager.get_tool("seo_project_new")
    result = tool.fn(
        directory=str(tmp_path / "shop"),
        target="https://example.test/",
        template_references=["shop/product-card"],
        profile_references=["shop/basic"],
        facts=[{"name": "cms", "value": "fixture", "provenance": "operator", "observed_at": None}],
    )
    assert result["project"]["template_references"] == ["shop/product-card"]
    assert result["project"]["facts"][0]["value"] == "fixture"


def test_cli_only_forwards_project_and_explicit_scan_arguments(tmp_path):
    project = tmp_path / "shop"
    create_project(project, "https://example.test/")
    args = cli.build_parser().parse_args(
        [
            "crawl-site",
            "--project",
            str(project),
            "--resume",
            "saved.sqlite",
        ]
    )
    _, kwargs = cli._build_kwargs("crawl-site", args)
    assert kwargs["project"] == str(project)
    assert kwargs["resume"] == "saved.sqlite"
    assert "scan_out" not in kwargs and "url" not in kwargs


def test_project_crawl_defaults_and_explicit_output_precedence(tmp_path, monkeypatch):
    project = tmp_path / "shop"
    create_project(project, "https://example.test/")
    captured = {}

    def capture(url, **kwargs):
        captured.update(url=url, **kwargs)
        return {"ok": True, "scan": kwargs["scan_out"]}

    monkeypatch.setattr("seohead.mcp.scan_handlers.crawl_site_scan", capture)
    tool = build_server()._tool_manager.get_tool("seo_crawl_site")
    result = tool.fn(project=str(project), producer_build="a" * 40, approve_large_crawl=True)
    assert Path(result["scan"]).parent == project / "scans"
    assert captured["url"] == "https://example.test/"
    explicit = str(tmp_path / "competitor.sqlite")
    tool.fn(
        project=str(project),
        url="https://competitor.test/",
        scan_out=explicit,
        approve_large_crawl=True,
    )
    assert captured["url"] == "https://competitor.test/"
    assert captured["scan_out"] == explicit


def test_project_default_crawl_profile_layers_project_overrides(tmp_path, monkeypatch):
    from seohead.crawl import profiles
    from seohead.projects.runtime import project_policy

    project = tmp_path / "shop"
    create_project(project, "https://example.test/")
    profile_dir = tmp_path / "profiles"
    monkeypatch.setenv(profiles.DIR_ENV, str(profile_dir))
    config = tmp_path / "crawl.json"
    config.write_text('{"speed":{"concurrency":2}}', encoding="utf-8")
    profiles.save("catalog", str(config))
    policy = project_policy(str(project))["policy"]
    policy["crawl_profile"] = "catalog"
    policy["crawl_overrides"] = {"speed.concurrency": 3}
    project_policy(str(project), policy=policy, apply=True, expected_revision=0)
    captured = {}

    def capture(_url, **kwargs):
        captured.update(kwargs)
        return {"finish_reason": "finished"}

    monkeypatch.setattr("seohead.mcp.scan_handlers.crawl_site_scan", capture)
    monkeypatch.setattr(handlers, "_record_scoped_evidence", lambda *_args: {})
    result = handlers.crawl_site(
        project=str(project), producer_build="a" * 40, approve_large_crawl=True
    )
    assert captured["settings"]["speed"]["concurrency"] == 3
    assert result["finish_reason"] == "finished"


def test_crawl_control_is_registered_for_cli_and_mcp(tmp_path, monkeypatch):
    project = tmp_path / "shop"
    create_project(project, "https://example.test/")
    args = cli.build_parser().parse_args(
        [
            "crawl-control",
            "--project",
            str(project),
            "--run-id",
            "a" * 32,
            "--action",
            "pause",
        ]
    )
    handler_name, kwargs = cli._build_kwargs("crawl-control", args)
    assert handler_name == "crawl_control"
    assert kwargs == {"project": str(project), "run_id": "a" * 32, "action": "pause"}
    monkeypatch.setattr(
        handlers,
        "crawl_control",
        lambda **values: {"ok": True, **values},
    )
    tool = build_server()._tool_manager.get_tool("seo_crawl_control")
    assert tool.fn(project=str(project), run_id="a" * 32, action="stop")["action"] == "stop"


def test_project_native_crawl_persists_live_phases_and_final_counters(tmp_path, monkeypatch):
    project = tmp_path / "shop"
    create_project(project, "https://example.test/")

    def capture(_url, **kwargs):
        kwargs["progress"](7, 11)
        kwargs["observation"]("analysis")
        return {
            "ok": True,
            "scan": kwargs["scan_out"],
            "partial": True,
            "finish_reason": "url_limit",
        }

    monkeypatch.setattr("seohead.mcp.scan_handlers.crawl_site_scan", capture)
    result = handlers.crawl_site(
        project=str(project), producer_build="a" * 40, approve_large_crawl=True
    )

    run = run_status(project)["items"][0]
    assert result["observer_run_id"] == run["id"]
    assert run["state"] == "partial" and run["finish_reason"] == "url_limit"
    assert run["artifact"].startswith("scans/")
    assert run["collector"]["mode"] == "spider"
    assert run["collector"]["max_urls"] == 200
    assert run["collector"]["max_requests"] == 20_000
    assert run["collector"]["max_crawl_seconds"] == 0
    assert run["collector"]["max_requests_per_second"] == 2.0
    assert run["counters"] == {
        "fetched": 7,
        "queued": 11,
        "inflight": None,
        "excluded": None,
        "rate_per_second": None,
    }
    assert [event["phase"] for event in run["events"]] == [
        "admission",
        "collection",
        "analysis",
        "finalizing",
    ]


def test_project_resume_does_not_inject_new_crawl_arguments(tmp_path, monkeypatch):
    project = tmp_path / "shop"
    create_project(project, "https://example.test/")
    captured = {}

    def resume(path, **kwargs):
        captured.update(path=path, **kwargs)
        return {"ok": True}

    monkeypatch.setattr("seohead.mcp.scan_handlers.resume_scan", resume)
    monkeypatch.setattr(
        "seohead.mcp.scan_handlers.resume_inputs",
        lambda _path: {
            "settings": {
                "limits": {"max_urls": 50, "max_requests": 150, "max_crawl_seconds": 60},
                "speed": {"min_delay_seconds": 0.5},
            }
        },
    )
    assert handlers.crawl_site(project=str(project), resume="saved.sqlite")["ok"]
    assert captured["path"] == "saved.sqlite" and captured["url"] is None


@pytest.mark.parametrize("command", ["scan_list", "scan_prune"])
def test_project_history_requires_a_valid_project_and_shares_mcp_defaults(tmp_path, command):
    invalid = tmp_path / "invalid"
    (invalid / "scans").mkdir(parents=True)
    with pytest.raises(ValueError):
        getattr(handlers, command)(project=str(invalid))
    project = tmp_path / "shop"
    create_project(project, "https://example.test/")
    tool = build_server()._tool_manager.get_tool("seo_" + command)
    actual = tool.fn(project=str(project))
    expected = getattr(handlers, command)(directory=str(project / "scans"))
    assert actual == expected
    if command == "scan_prune":
        assert actual["applied"] is False and actual["plan"]["candidates"] == []
    assert (project / "project.json").exists()


@pytest.mark.parametrize("from_config", [False, True])
def test_project_preserves_explicit_legacy_output(tmp_path, monkeypatch, from_config):
    import json

    project = tmp_path / "shop"
    create_project(project, "https://example.test/")
    legacy = tmp_path / "legacy"

    class LegacySelected(Exception):
        pass

    def legacy_crawl(*args, **kwargs):
        raise LegacySelected

    monkeypatch.setattr("seohead.crawl.spider.crawl_site", legacy_crawl)
    monkeypatch.setattr(
        "seohead.mcp.scan_handlers.crawl_site_scan",
        lambda *args, **kwargs: pytest.fail("explicit legacy output must retain its route"),
    )
    if from_config:
        config = tmp_path / "config.json"
        config.write_text(json.dumps({"output": {"dir": str(legacy)}}))
        kwargs = {"config": str(config)}
    else:
        kwargs = {"out_dir": str(legacy)}
    with pytest.raises(LegacySelected):
        handlers.crawl_site(project=str(project), approve_large_crawl=True, **kwargs)
    assert list((project / "scans").iterdir()) == []
