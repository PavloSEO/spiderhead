"""Configuration resolution, validation, and the run manifest.

The manifest is the point: two thirds of these settings change what an audit
finds, so a report that does not record them is not comparable to any other.
"""

import json

import pytest

from seohead.crawl import settings as cfg


def test_defaults_load_and_validate():
    resolved = cfg.load()
    assert resolved["limits"]["max_urls"] == cfg.DEFAULTS["limits"]["max_urls"]
    assert resolved["robots"]["policy"] == "respect"
    # 1 is the sequential crawler: concurrency must be opt-in, not a surprise.
    assert resolved["speed"]["concurrency"] == 1


def test_every_setting_is_classified_as_results_affecting_or_not():
    """A new setting cannot be added without deciding whether it changes findings.

    Silence here is how a report stops being reproducible: a setting that moves
    the results but is absent from the manifest makes two audits differ for no
    recorded reason.
    """
    cost_only = {
        "http.timeout_seconds",  # also results-affecting; listed below
        "limits.max_response_bytes",
        "output.dir",
        "output.write_pages_jsonl",
        "output.write_decisions_jsonl",
        "speed.max_delay_seconds",
        # Changes only how fast a crawl runs, never what it finds: the spider
        # sorts batched results back into queue order before anything is
        # written, so pages.jsonl is the same at any concurrency.
        "speed.concurrency",
        # The backlog is a second rendering of findings already made, so it can
        # add a file but never a finding.
        "output.write_tasks",
        # Optional artefacts (off by default): they add files on disk, never
        # change a finding. Screenshot mode only changes the saved image framing.
        "rendering.artifacts.screenshots",
        "rendering.artifacts.console_errors",
        "rendering.artifacts.screenshot_mode",
        # The profile's identity, not whether one is used (that part is
        # rendering.browser.persistent_profile, which is results-affecting):
        # same rationale as excluding a credential's value from the manifest.
        "rendering.browser.persistent_profile_dir",
    }
    every = set(cfg._flatten(cfg.DEFAULTS))
    unclassified = every - cfg.RESULTS_AFFECTING - cost_only
    assert not unclassified, f"unclassified settings: {sorted(unclassified)}"


def test_results_affecting_names_only_real_settings():
    every = set(cfg._flatten(cfg.DEFAULTS))
    assert every >= cfg.RESULTS_AFFECTING, sorted(cfg.RESULTS_AFFECTING - every)


def test_every_discovery_key_is_store_or_crawl_and_nothing_else():
    """Two different questions: keep it in the report, versus request it. A link type carries
    only the half that changes an outcome — a redirect has no LinkEdge of its own to withhold,
    and an off-host link is never fetched by a single-host crawler (#91)."""
    for link_type, pair in cfg.DEFAULTS["discovery"].items():
        if isinstance(pair, dict):
            assert pair, link_type
            assert set(pair) <= {"store", "crawl"}, link_type


# ── precedence ──────────────────────────────────────────────────────────────


def test_file_overrides_defaults(tmp_path):
    path = tmp_path / "crawl.json"
    path.write_text(json.dumps({"limits": {"max_urls": 42}}))
    assert cfg.load(str(path))["limits"]["max_urls"] == 42


def test_environment_overrides_the_file(tmp_path, monkeypatch):
    path = tmp_path / "crawl.json"
    path.write_text(json.dumps({"limits": {"max_urls": 42}}))
    monkeypatch.setenv("SEOHEAD_CRAWL_MAX_URLS", "7")
    assert cfg.load(str(path))["limits"]["max_urls"] == 7


def test_explicit_arguments_override_the_environment(tmp_path, monkeypatch):
    monkeypatch.setenv("SEOHEAD_CRAWL_MAX_URLS", "7")
    resolved = cfg.load(overrides={"limits.max_urls": 3})
    assert resolved["limits"]["max_urls"] == 3


def test_an_unset_override_does_not_erase_a_configured_value(tmp_path):
    path = tmp_path / "crawl.json"
    path.write_text(json.dumps({"limits": {"max_urls": 42}}))
    resolved = cfg.load(str(path), overrides={"limits.max_urls": None})
    assert resolved["limits"]["max_urls"] == 42


def test_environment_values_take_the_type_of_the_default(monkeypatch):
    monkeypatch.setenv("SEOHEAD_CRAWL_MIN_DELAY", "1.5")
    monkeypatch.setenv("SEOHEAD_CRAWL_MAX_DEPTH", "2")
    resolved = cfg.load()
    assert resolved["speed"]["min_delay_seconds"] == 1.5
    assert resolved["limits"]["max_depth"] == 2


def test_a_malformed_environment_value_is_refused_by_name(monkeypatch):
    monkeypatch.setenv("SEOHEAD_CRAWL_MAX_URLS", "lots")
    with pytest.raises(cfg.ConfigError, match="SEOHEAD_CRAWL_MAX_URLS"):
        cfg.load()


# ── validation ──────────────────────────────────────────────────────────────


def test_an_unknown_setting_is_refused_with_its_path(tmp_path):
    path = tmp_path / "crawl.json"
    path.write_text(json.dumps({"scope": {"exclude_pattern": ["typo"]}}))
    with pytest.raises(cfg.ConfigError, match=r"scope\.exclude_pattern"):
        cfg.load(str(path))


def test_free_form_headers_are_a_leaf_not_a_branch(tmp_path):
    """Arbitrary header names must not be mistaken for unknown settings."""
    path = tmp_path / "crawl.json"
    path.write_text(json.dumps({"http": {"headers": {"Accept-Language": "de"}}}))
    assert cfg.load(str(path))["http"]["headers"]["Accept-Language"] == "de"


@pytest.mark.parametrize("name", ["Authorization", "cookie", "X-API-Key", "X-Auth-Token"])
def test_generic_headers_refuse_credentials_without_echoing_their_values(name):
    with pytest.raises(cfg.ConfigError, match="credential_headers") as exc_info:
        cfg.load(overrides={"http.headers": {name: "dummy-inline-value"}})
    assert "dummy-inline-value" not in str(exc_info.value)


@pytest.mark.parametrize(
    "override,message",
    [
        ({"robots.policy": "maybe"}, "robots.policy"),
        ({"scope.internal": "everything"}, "scope.internal"),
        ({"limits.max_urls": -1}, "max_urls"),
        ({"limits.max_depth": -2}, "max_depth"),
        ({"limits.max_query_variants_per_path": -1}, "max_query_variants_per_path"),
        ({"speed.min_delay_seconds": -1}, "min_delay_seconds"),
        ({"speed.concurrency": 0}, "concurrency"),
        ({"cache.mode": "always"}, "cache.mode"),
        ({"rendering.mode": "always"}, "rendering.mode"),
        ({"rendering.browser.viewport": "tablet"}, "rendering.browser.viewport"),
        ({"rendering.browser.wait_until": "instant"}, "rendering.browser.wait_until"),
        ({"rendering.browser.script_timeout_seconds": -1}, "script_timeout_seconds"),
        (
            {"rendering.browser.resize_to_content_max_height_px": 0},
            "resize_to_content_max_height_px",
        ),
        ({"rendering.browser.device_pixel_ratio": 0}, "device_pixel_ratio"),
        ({"rendering.escalation.policy": "always"}, "rendering.escalation.policy"),
        ({"rendering.escalation.sample_per_pattern": 0}, "sample_per_pattern"),
        ({"rendering.escalation.max_render_urls": -1}, "max_render_urls"),
        ({"rendering.escalation.max_render_seconds": -1}, "max_render_seconds"),
        ({"scope.segments": "not-a-list"}, "scope.segments must be a list"),
        ({"scope.segments": [{"prefix": "/en/"}]}, "non-empty 'name'"),
        ({"scope.segments": [{"name": "default", "prefix": "/en/"}]}, "reserved"),
        (
            {
                "scope.segments": [
                    {"name": "en", "prefix": "/en/"},
                    {"name": "en", "host": "en.example.com"},
                ]
            },
            "duplicate name",
        ),
        ({"scope.segments": [{"name": "en"}]}, "at least one of"),
        ({"scope.segments": [{"name": "en", "pattern": "["}]}, "not a valid regex"),
        (
            {"scope.segments": [{"name": "en", "prefix": "/en/", "bogus": 1}]},
            "unknown keys",
        ),
        ({"scope.segments_only": ["fr"]}, "scope.segments_only"),
        ({"rendering.browser.engine": "safari"}, "rendering.browser.engine"),
        (
            {"rendering.browser.engine": "firefox", "rendering.browser.mobile_emulation": True},
            "cannot emulate",
        ),
        # A partial pair: setting one side of a custom viewport asks for a
        # dimensionless render -- refused here rather than guessed at launch.
        ({"rendering.browser.viewport_width": 800}, "must be set together"),
        ({"rendering.browser.viewport_height": 600}, "must be set together"),
        (
            {"rendering.browser.viewport_width": True, "rendering.browser.viewport_height": 600},
            "must be an integer",
        ),
        (
            {"rendering.browser.viewport_height": False, "rendering.browser.viewport_width": 800},
            "must be an integer",
        ),
        (
            {"rendering.browser.viewport_width": -5, "rendering.browser.viewport_height": 600},
            "must be an integer",
        ),
        (
            {"rendering.browser.viewport_width": 640, "rendering.browser.viewport_height": 0},
            "must be set together",
        ),
        (
            {"rendering.browser.viewport_width": 20000, "rendering.browser.viewport_height": 600},
            "16384",
        ),
        ({"rendering.browser.page_concurrency": 0}, "between 1 and"),
        ({"rendering.browser.page_concurrency": 17}, "between 1 and"),
        ({"rendering.browser.page_concurrency": True}, "must be an integer between 1 and"),
        ({"rendering.browser.page_concurrency": 2.5}, "must be an integer between 1 and"),
    ],
)
def test_invalid_values_are_refused(override, message):
    with pytest.raises(cfg.ConfigError, match=message):
        cfg.load(overrides=override)


def test_a_segment_config_loads_and_validates():
    config = cfg.load(
        overrides={
            "scope.segments": [
                {"name": "en", "prefix": "/en/"},
                {"name": "shop", "host": "shop.example.com"},
                {"name": "legacy", "pattern": r"^https://example\.com/old-"},
            ],
            "scope.segments_only": ["en", "default"],
        }
    )
    assert [s["name"] for s in config["scope"]["segments"]] == ["en", "shop", "legacy"]
    assert config["scope"]["segments_only"] == ["en", "default"]
    cfg.validate(config)


def test_segments_only_may_reference_only_the_default_segment():
    # No declared segments at all -- "default" is still a legal name to scope to.
    config = cfg.load(overrides={"scope.segments_only": ["default"]})
    cfg.validate(config)


def test_scope_segments_and_segments_only_are_results_affecting():
    assert "scope.segments" in cfg.RESULTS_AFFECTING
    assert "scope.segments_only" in cfg.RESULTS_AFFECTING


def test_cache_defaults_to_off():
    """No side effect (a cache directory written outside any explicit output directory) may
    appear behind a default; caching is opt-in."""
    resolved = cfg.load()
    assert resolved["cache"]["mode"] == "off"
    assert resolved["cache"]["invalidate"] is False


def test_cache_mode_and_invalidate_are_results_affecting():
    assert "cache.mode" in cfg.RESULTS_AFFECTING
    assert "cache.invalidate" in cfg.RESULTS_AFFECTING


def test_cache_invalidate_is_refused_with_replay_mode():
    """Issue #137: replay's own guarantee ("never touch the network for an entry
    already on disk") and invalidate's ("force every lookup to miss") cannot both
    hold, so the combination is refused here rather than one silently winning."""
    with pytest.raises(cfg.ConfigError, match=r"cache\.invalidate"):
        cfg.load(overrides={"cache.mode": "replay", "cache.invalidate": True})


def test_cache_mode_is_configurable_through_a_file(tmp_path):
    path = tmp_path / "crawl.json"
    path.write_text(json.dumps({"cache": {"mode": "replay"}}))
    assert cfg.load(str(path))["cache"]["mode"] == "replay"


def test_a_missing_file_is_refused_rather_than_ignored():
    with pytest.raises(cfg.ConfigError, match="cannot read config"):
        cfg.load("/nonexistent/crawl.json")


def test_malformed_json_is_refused_by_name(tmp_path):
    path = tmp_path / "crawl.json"
    path.write_text("{not json")
    with pytest.raises(cfg.ConfigError, match="not valid JSON"):
        cfg.load(str(path))


# ── manifest ────────────────────────────────────────────────────────────────


def test_the_manifest_records_resolved_values_not_their_source():
    manifest = cfg.manifest(cfg.load(overrides={"limits.max_urls": 11}))
    assert manifest["limits.max_urls"] == 11


def test_two_runs_differing_in_a_results_affecting_setting_differ_in_the_manifest():
    first = cfg.manifest(cfg.load(overrides={"robots.policy": "respect"}))
    second = cfg.manifest(cfg.load(overrides={"robots.policy": "report_only"}))
    assert first != second
    assert [k for k in first if first[k] != second[k]] == ["robots.policy"]


def test_a_cost_only_setting_does_not_change_the_manifest():
    first = cfg.manifest(cfg.load(overrides={"output.dir": "/tmp/a"}))
    second = cfg.manifest(cfg.load(overrides={"output.dir": "/tmp/b"}))
    assert first == second


def test_the_manifest_is_json_serialisable():
    json.dumps(cfg.manifest(cfg.load()))


# ── browser engine, viewport dimensions, render-page concurrency (#744) ─────


def test_render_browser_defaults_stay_chromium_preset_and_sequential():
    """Old configs keep meaning the same thing: the historical Chromium
    renderer, the named presets, and one page at a time."""
    browser = cfg.load()["rendering"]["browser"]
    assert browser["engine"] == "chromium"
    assert browser["viewport_width"] == 0
    assert browser["viewport_height"] == 0
    assert browser["page_concurrency"] == 1


@pytest.mark.parametrize("engine", ["chromium", "firefox", "webkit"])
def test_every_supported_engine_loads(engine):
    resolved = cfg.load(overrides={"rendering.browser.engine": engine})
    assert resolved["rendering"]["browser"]["engine"] == engine


def test_firefox_touch_emulation_loads_without_mobile_emulation():
    """has_touch is a general context option every engine accepts; only
    is_mobile is gated, so touch alone is legal on Firefox."""
    resolved = cfg.load(
        overrides={
            "rendering.browser.engine": "firefox",
            "rendering.browser.touch_emulation": True,
        }
    )
    assert resolved["rendering"]["browser"]["touch_emulation"] is True


def test_a_complete_custom_viewport_pair_loads():
    resolved = cfg.load(
        overrides={
            "rendering.browser.viewport_width": 800,
            "rendering.browser.viewport_height": 600,
        }
    )
    assert resolved["rendering"]["browser"]["viewport_width"] == 800
    assert resolved["rendering"]["browser"]["viewport_height"] == 600


def test_a_bounded_page_concurrency_loads():
    resolved = cfg.load(overrides={"rendering.browser.page_concurrency": 4})
    assert resolved["rendering"]["browser"]["page_concurrency"] == 4


def test_engine_viewport_pair_and_concurrency_are_results_affecting():
    """A webkit run and a chromium run can produce different DOMs, and an
    800px DOM differs from a 1366px one -- both must show up in the manifest,
    or two audits differ for no recorded reason."""
    for path in (
        "rendering.browser.engine",
        "rendering.browser.viewport_width",
        "rendering.browser.viewport_height",
        "rendering.browser.page_concurrency",
    ):
        assert path in cfg.RESULTS_AFFECTING


def test_the_manifest_records_engine_viewport_and_concurrency():
    manifest = cfg.manifest(
        cfg.load(
            overrides={
                "rendering.browser.engine": "webkit",
                "rendering.browser.viewport_width": 800,
                "rendering.browser.viewport_height": 600,
                "rendering.browser.page_concurrency": 4,
            }
        )
    )
    assert manifest["rendering.browser.engine"] == "webkit"
    assert manifest["rendering.browser.viewport_width"] == 800
    assert manifest["rendering.browser.viewport_height"] == 600
    assert manifest["rendering.browser.page_concurrency"] == 4


def test_the_new_render_settings_reach_describe_settings():
    """The CLI's --config-help and the MCP describe-settings surface share
    this listing, so an entry here is what exposes the settings to an agent."""
    rows = {row["path"]: row for row in cfg.describe_settings()}
    for path in (
        "rendering.browser.engine",
        "rendering.browser.viewport_width",
        "rendering.browser.viewport_height",
        "rendering.browser.page_concurrency",
    ):
        assert rows[path]["results_affecting"] is True
        assert rows[path]["description"]


# ── describe_settings (CLI --config-help / MCP #23 share this) ──────────────


def test_every_setting_has_a_description():
    """A config key without a reachable description is exactly what #27 forbids.

    ``describe_settings`` looks up ``DESCRIPTIONS`` by the same paths as
    ``DEFAULTS``; a missing entry would raise ``KeyError`` here.
    """
    described = {row["path"] for row in cfg.describe_settings()}
    assert described == set(cfg._flatten(cfg.DEFAULTS))
    for row in cfg.describe_settings():
        assert row["description"], row["path"]


def test_describe_settings_reports_type_default_and_results_affecting():
    by_path = {row["path"]: row for row in cfg.describe_settings()}
    max_urls = by_path["limits.max_urls"]
    assert max_urls["type"] == "int"
    assert max_urls["default"] == cfg.DEFAULTS["limits"]["max_urls"]
    assert max_urls["results_affecting"] is True

    out_dir = by_path["output.dir"]
    assert out_dir["results_affecting"] is False


# ── fingerprint (resumable crawl checkpoint invalidation, #16) ──────────────


def test_fingerprint_is_stable_for_the_same_manifest():
    assert cfg.fingerprint(cfg.load()) == cfg.fingerprint(cfg.load())


def test_fingerprint_changes_with_a_results_affecting_setting():
    first = cfg.fingerprint(cfg.load(overrides={"limits.max_depth": 3}))
    second = cfg.fingerprint(cfg.load(overrides={"limits.max_depth": 4}))
    assert first != second


def test_fingerprint_ignores_a_cost_only_setting():
    first = cfg.fingerprint(cfg.load(overrides={"output.dir": "/tmp/a"}))
    second = cfg.fingerprint(cfg.load(overrides={"output.dir": "/tmp/b"}))
    assert first == second


# ── politeness ──────────────────────────────────────────────────────────────


def test_the_effective_rate_is_derived_from_the_combination_not_one_knob():
    assert cfg.effective_request_rate(cfg.load(overrides={"speed.min_delay_seconds": 0.5})) == 2.0
    assert cfg.effective_request_rate(cfg.load(overrides={"speed.min_delay_seconds": 2.0})) == 0.5


def test_no_delay_reports_an_unbounded_rate():
    rate = cfg.effective_request_rate(cfg.load(overrides={"speed.min_delay_seconds": 0}))
    assert rate == float("inf")


# ── credential headers ──────────────────────────────────────────────────────


def _cred_overrides(**entry):
    return {
        "http.credential_headers": [entry],
        "http.credentials_acknowledged": True,
    }


def test_an_unbound_credential_entry_is_refused(monkeypatch):
    """An entry without a host binding would be sent on every request."""
    monkeypatch.setenv("SEOHEAD_TEST_TOKEN", "x")
    overrides = _cred_overrides(headers={"Authorization": "env:SEOHEAD_TEST_TOKEN"})
    with pytest.raises(cfg.ConfigError, match="host binding"):
        cfg.load(overrides=overrides)


def test_credentials_without_acknowledgement_are_refused(monkeypatch):
    monkeypatch.setenv("SEOHEAD_TEST_TOKEN", "x")
    overrides = {
        "http.credential_headers": [
            {"host": "example.com", "headers": {"Authorization": "env:SEOHEAD_TEST_TOKEN"}}
        ]
    }
    with pytest.raises(cfg.ConfigError, match="credentials_acknowledged"):
        cfg.load(overrides=overrides)


def test_an_inline_credential_value_is_refused():
    """Config files carry a reference to the environment, never the secret itself."""
    overrides = _cred_overrides(host="example.com", headers={"Authorization": "Bearer abc123"})
    with pytest.raises(cfg.ConfigError, match="environment variable"):
        cfg.load(overrides=overrides)


def test_a_credential_referencing_an_unset_variable_is_refused():
    overrides = _cred_overrides(host="example.com", headers={"Authorization": "env:SEOHEAD_NOPE"})
    with pytest.raises(cfg.ConfigError, match="SEOHEAD_NOPE"):
        cfg.load(overrides=overrides)


def test_a_bound_credential_with_a_set_variable_loads(monkeypatch):
    monkeypatch.setenv("SEOHEAD_TEST_TOKEN", "s3cr3t")
    overrides = _cred_overrides(
        host="example.com", headers={"Authorization": "env:SEOHEAD_TEST_TOKEN"}
    )
    resolved = cfg.load(overrides=overrides)
    assert resolved["http"]["credential_headers"][0]["host"] == "example.com"


def test_credentials_apply_only_to_their_own_host(monkeypatch):
    """The mechanism that keeps a credential off a cross-host redirect target."""
    monkeypatch.setenv("SEOHEAD_TEST_TOKEN", "s3cr3t")
    entries = [{"host": "example.com", "headers": {"Authorization": "env:SEOHEAD_TEST_TOKEN"}}]
    assert cfg.resolve_credential_headers(entries, "example.com") == {"Authorization": "s3cr3t"}
    assert cfg.resolve_credential_headers(entries, "EXAMPLE.COM") == {"Authorization": "s3cr3t"}
    assert cfg.resolve_credential_headers(entries, "other-host.com") == {}


def test_configuring_credentials_adds_the_default_destructive_path_exclusions(monkeypatch):
    monkeypatch.setenv("SEOHEAD_TEST_TOKEN", "s3cr3t")
    overrides = _cred_overrides(
        host="example.com", headers={"Authorization": "env:SEOHEAD_TEST_TOKEN"}
    )
    resolved = cfg.load(overrides=overrides)
    for pattern in cfg.DESTRUCTIVE_PATH_PATTERNS:
        assert pattern in resolved["scope"]["exclude_patterns"]


def test_credential_values_are_redacted_from_the_manifest(monkeypatch):
    monkeypatch.setenv("SEOHEAD_TEST_TOKEN", "s3cr3t")
    overrides = _cred_overrides(
        host="example.com", headers={"Authorization": "env:SEOHEAD_TEST_TOKEN"}
    )
    manifest = cfg.manifest(cfg.load(overrides=overrides))
    entry = manifest["http.credential_headers"][0]
    assert entry["host"] == "example.com"
    assert entry["headers"]["Authorization"] == "REDACTED"
    assert "s3cr3t" not in json.dumps(manifest)


def test_basic_and_digest_auth_require_acknowledged_host_bound_environment_refs(monkeypatch):
    monkeypatch.setenv("SEOHEAD_TEST_USER", "crawler")
    monkeypatch.setenv("SEOHEAD_TEST_PASSWORD", "s3cr3t")
    auth = [
        {
            "host": "example.com",
            "scheme": "digest",
            "username_env": "env:SEOHEAD_TEST_USER",
            "password_env": "env:SEOHEAD_TEST_PASSWORD",
        }
    ]
    with pytest.raises(cfg.ConfigError, match="auth_acknowledged"):
        cfg.load(overrides={"http.auth": auth})
    resolved = cfg.load(overrides={"http.auth": auth, "http.auth_acknowledged": True})
    assert resolved["http"]["auth"][0]["scheme"] == "digest"
    manifest = cfg.manifest(resolved)
    assert manifest["http.auth"] == [{"host": "example.com", "scheme": "digest"}]
    serialized = json.dumps(manifest)
    assert "SEOHEAD_TEST_PASSWORD" not in serialized
    assert "s3cr3t" not in serialized


def test_http_auth_rejects_inline_credentials_and_ambiguous_host_setup(monkeypatch):
    monkeypatch.setenv("SEOHEAD_TEST_USER", "crawler")
    monkeypatch.setenv("SEOHEAD_TEST_PASSWORD", "s3cr3t")
    inline = [
        {
            "host": "example.com",
            "scheme": "basic",
            "username_env": "crawler",
            "password_env": "env:SEOHEAD_TEST_PASSWORD",
        }
    ]
    with pytest.raises(cfg.ConfigError, match="username_env"):
        cfg.load(overrides={"http.auth": inline, "http.auth_acknowledged": True})
    conflict = _cred_overrides(
        host="example.com", headers={"Authorization": "env:SEOHEAD_TEST_PASSWORD"}
    )
    conflict.update(
        {
            "http.auth": [
                {
                    "host": "example.com",
                    "scheme": "basic",
                    "username_env": "env:SEOHEAD_TEST_USER",
                    "password_env": "env:SEOHEAD_TEST_PASSWORD",
                }
            ],
            "http.auth_acknowledged": True,
        }
    )
    with pytest.raises(cfg.ConfigError, match="cannot both configure"):
        cfg.load(overrides=conflict)


def test_configuring_http_auth_adds_default_destructive_path_exclusions(monkeypatch):
    monkeypatch.setenv("SEOHEAD_TEST_USER", "crawler")
    monkeypatch.setenv("SEOHEAD_TEST_PASSWORD", "s3cr3t")
    auth = [
        {
            "host": "example.com",
            "scheme": "basic",
            "username_env": "env:SEOHEAD_TEST_USER",
            "password_env": "env:SEOHEAD_TEST_PASSWORD",
        }
    ]
    resolved = cfg.load(overrides={"http.auth": auth, "http.auth_acknowledged": True})
    for pattern in cfg.DESTRUCTIVE_PATH_PATTERNS:
        assert pattern in resolved["scope"]["exclude_patterns"]


def test_zstd_negotiation_is_explicit_and_requires_a_bounded_decoder(monkeypatch):
    if not cfg.zstd_decoder_available():
        with pytest.raises(
            cfg.ConfigError, match=r"compression.zstd or the optional zstandard package"
        ):
            cfg.load(overrides={"http.zstd": True})
        return
    resolved = cfg.load(overrides={"http.zstd": True})
    assert "zstd" in cfg.request_headers(resolved)["Accept-Encoding"]
    with pytest.raises(cfg.ConfigError, match=r"cache.mode=off"):
        cfg.load(
            overrides={
                "http.zstd": True,
                "cache.mode": "live",
            }
        )


def test_manifest_redacts_credentials_from_a_legacy_generic_header_mapping():
    legacy = cfg.load()
    legacy["http"]["headers"] = {
        "Authorization": "dummy-inline-value",
        "Accept-Language": "de",
    }
    manifest = cfg.manifest(legacy)
    assert manifest["http.headers"] == {"Authorization": "REDACTED", "Accept-Language": "de"}
    assert legacy["http"]["headers"]["Authorization"] == "dummy-inline-value"


# ── rendering (#18) ──────────────────────────────────────────────────────────


def test_rendering_defaults_to_raw_with_no_browser_needed():
    resolved = cfg.load()
    assert resolved["rendering"]["mode"] == "raw"


def test_a_persistent_profile_without_a_directory_is_refused():
    with pytest.raises(cfg.ConfigError, match="persistent_profile_dir"):
        cfg.load(overrides={"rendering.browser.persistent_profile": True})


def test_a_persistent_profile_with_a_directory_loads():
    resolved = cfg.load(
        overrides={
            "rendering.browser.persistent_profile": True,
            "rendering.browser.persistent_profile_dir": "/tmp/some-profile",
        }
    )
    assert resolved["rendering"]["browser"]["persistent_profile_dir"] == "/tmp/some-profile"


def test_the_persistent_profile_directory_is_not_in_the_manifest():
    """Same rationale as a credential's value: a shareable manifest should not carry a path."""
    manifest = cfg.manifest(
        cfg.load(
            overrides={
                "rendering.browser.persistent_profile": True,
                "rendering.browser.persistent_profile_dir": "/tmp/some-profile",
            }
        )
    )
    assert "rendering.browser.persistent_profile_dir" not in manifest
    assert manifest["rendering.browser.persistent_profile"] is True


def test_two_viewports_differ_at_exactly_that_manifest_key():
    """Acceptance criterion: two runs at different viewport widths produce
    manifests differing at exactly that key."""
    desktop = cfg.manifest(cfg.load(overrides={"rendering.browser.viewport": "desktop"}))
    mobile = cfg.manifest(cfg.load(overrides={"rendering.browser.viewport": "mobile"}))
    assert [k for k in desktop if desktop[k] != mobile[k]] == ["rendering.browser.viewport"]


def test_the_render_extras_are_cost_only_and_off_the_manifest():
    first = cfg.manifest(cfg.load(overrides={"rendering.artifacts.screenshots": True}))
    second = cfg.manifest(cfg.load(overrides={"rendering.artifacts.screenshots": False}))
    assert first == second


# ── rate warnings (#925) ────────────────────────────────────────────────────


@pytest.mark.parametrize(
    ("delay", "warns"),
    [(0.5, False), (1.0, False), (0.4, True), (0.25, True), (0, True)],
)
def test_a_rate_above_two_requests_per_second_warns_and_two_does_not(delay, warns):
    warnings = cfg.rate_warnings(cfg.load(overrides={"speed.min_delay_seconds": delay}))
    assert bool(warnings) is warns


def test_the_rate_fields_report_the_number_or_unbounded_with_its_warning():
    fast = cfg.rate_fields(cfg.load(overrides={"speed.min_delay_seconds": 0.25}))
    assert fast["effective_max_requests_per_second"] == pytest.approx(4.0)
    assert "above 2 req/s" in fast["warnings"][0]
    unbounded = cfg.rate_fields(cfg.load(overrides={"speed.min_delay_seconds": 0}))
    assert unbounded["effective_max_requests_per_second"] == "unbounded"
    default = cfg.rate_fields(cfg.load())
    assert default["effective_max_requests_per_second"] == 2.0
    assert default["warnings"] == []
    assert default["load_estimate"]["format"] == "crawl-load-estimate.v1"
    assert default["load_estimate"]["duration_floor_seconds"] == 100.0
    assert default["load_estimate"]["disk_estimate_bytes"] is None
