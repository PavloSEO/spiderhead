"""Offline boundaries for descriptor-backed crawl drafts; no GUI or scan launch."""

import os
import unittest
from unittest.mock import patch

from seohead_desktop.crawl_configuration import (
    describe_controls,
    preview_configuration,
    validate_overrides,
)


def descriptor():
    # Representative real describe-settings rows. Values do not come from the
    # implementation's allowlist, so schema/type changes cannot validate themselves.
    defaults = {
        "scope.internal": "host",
        "scope.include_patterns": [],
        "scope.exclude_patterns": [],
        "scope.include_extensions": [],
        "scope.exclude_extensions": [],
        "scope.include_media_types": [],
        "scope.exclude_media_types": [],
        "limits.max_urls": 200,
        "limits.max_requests": 20000,
        "limits.max_crawl_seconds": 0,
        "limits.max_depth": 5,
        "storage.min_free_bytes": 1024**3,
        "speed.concurrency": 1,
        "speed.adaptive": True,
        "speed.min_delay_seconds": 0.5,
        "speed.max_delay_seconds": 60.0,
        "cache.mode": "off",
        "cache.invalidate": False,
        "rendering.mode": "raw",
        "rendering.browser.engine": "chromium",
        "rendering.browser.mobile_emulation": False,
        "rendering.browser.viewport_width": 0,
        "rendering.browser.viewport_height": 0,
        "rendering.browser.page_concurrency": 1,
        "rendering.rendered_links.store": False,
        "rendering.rendered_links.crawl": False,
        "discovery.resolve_redirect_destination": False,
        "discovery.resolve_canonical_destination": False,
        "resources.fetch": False,
        "storage.body_mode": "captured_entity_bytes",
        "http.user_agent": "",
        "http.headers": {},
        "http.proxy": "",
        "http.credential_headers": [],
        "rendering.browser.persistent_profile": False,
        "rendering.browser.remote_endpoint_env": "",
        "future.unknown_control": False,
    }
    return {
        "settings": [
            {
                "path": key,
                "type": type(value).__name__,
                "default": value,
                "description": "Core descriptor fixture",
                "results_affecting": True,
            }
            for key, value in defaults.items()
        ]
    }


class CrawlConfigurationTests(unittest.TestCase):
    def test_catalogue_exposes_every_known_path_without_enabling_unknown_controls(self):
        payload = descriptor()
        rows = describe_controls(payload)
        self.assertEqual(len(rows), len(payload["settings"]))
        fields = {row["path"]: row for row in rows}
        self.assertTrue(fields["scope.include_patterns"]["editable"])
        self.assertFalse(fields["future.unknown_control"]["editable"])
        self.assertFalse(fields["http.proxy"]["editable"])
        self.assertIsNone(fields["http.proxy"]["default"])

    def test_descriptor_type_drift_and_duplicate_paths_fail_closed(self):
        payload = descriptor()
        row = next(
            row for row in payload["settings"] if row["path"] == "limits.max_urls"
        )
        row["type"] = "str"
        with self.assertRaisesRegex(ValueError, "type changed"):
            validate_overrides(payload, {"limits.max_urls": 100})
        payload["settings"].append(dict(row))
        with self.assertRaisesRegex(ValueError, "Duplicate"):
            describe_controls(payload)

    def test_unknown_or_secret_fields_are_never_admitted_or_echoed(self):
        for path in (
            "unknown.path",
            "http.headers",
            "http.proxy",
            "http.credential_headers",
            "rendering.browser.remote_endpoint_env",
            "rendering.browser.persistent_profile",
        ):
            with self.subTest(path=path), self.assertRaises(ValueError) as caught:
                validate_overrides(descriptor(), {path: "synthetic-secret-marker"})
            self.assertNotIn("synthetic-secret-marker", str(caught.exception))

    def test_typed_budget_values_preserve_zero_vs_missing_and_reject_bool(self):
        self.assertEqual(
            validate_overrides(descriptor(), {"limits.max_depth": 0}),
            {"limits.max_depth": 0},
        )
        for value in (True, "10", 1.5, None, 1000001, -1):
            with self.subTest(value=value), self.assertRaises(ValueError):
                validate_overrides(descriptor(), {"limits.max_urls": value})

    def test_desktop_preserves_disabled_native_population_and_time_limits(self):
        for path in ("limits.max_urls", "limits.max_requests", "limits.max_crawl_seconds"):
            with self.subTest(path=path):
                self.assertEqual(validate_overrides(descriptor(), {path: 0}), {path: 0})
        self.assertEqual(validate_overrides(descriptor(), {}), {})

    def test_delay_admission_rejects_fast_nonfinite_and_overflow_values(self):
        for value in (-0.1, True, float("inf"), float("nan"), 10**1000):
            with (
                self.subTest(value=type(value).__name__),
                self.assertRaises(ValueError),
            ):
                validate_overrides(descriptor(), {"speed.min_delay_seconds": value})
        self.assertEqual(
            validate_overrides(descriptor(), {"speed.min_delay_seconds": 2}),
            {"speed.min_delay_seconds": 2.0},
        )
        self.assertEqual(validate_overrides(descriptor(), {"speed.min_delay_seconds": 0.1}), {"speed.min_delay_seconds": 0.1})

    def test_invalid_enum_regex_suffix_and_media_types_are_rejected(self):
        for path, value in (
            ("rendering.mode", "pretend-js"),
            ("scope.include_patterns", ["["]),
            ("scope.include_patterns", [123]),
            ("scope.include_extensions", ["tar.gz"]),
            ("scope.include_extensions", ["HTML", ".html"]),
            ("scope.include_media_types", ["*/html"]),
            ("scope.include_media_types", ["TEXT/HTML", "text/html"]),
        ):
            with self.subTest(path=path), self.assertRaises(ValueError):
                validate_overrides(descriptor(), {path: value})
        accepted = {
            "scope.include_patterns": [r"/products/"],
            "scope.exclude_extensions": [".pdf"],
            "scope.include_media_types": ["text/html", "image/*"],
        }
        self.assertEqual(validate_overrides(descriptor(), accepted), accepted)

    def test_lists_are_bounded_and_do_not_alias_input(self):
        values = {"scope.exclude_patterns": ["/private/"]}
        result = validate_overrides(descriptor(), values)
        values["scope.exclude_patterns"].append("/other/")
        self.assertEqual(result, {"scope.exclude_patterns": ["/private/"]})
        with self.assertRaises(ValueError):
            validate_overrides(descriptor(), {"scope.exclude_patterns": ["x"] * 257})

    def test_viewport_pairs_and_engine_constraints_use_effective_base(self):
        with self.assertRaisesRegex(ValueError, "together"):
            validate_overrides(descriptor(), {"rendering.browser.viewport_width": 1440})
        base = {
            "rendering.browser.viewport_width": 390,
            "rendering.browser.viewport_height": 844,
        }
        self.assertEqual(
            validate_overrides(
                descriptor(),
                {"rendering.browser.viewport_width": 412},
                base_values=base,
            ),
            {"rendering.browser.viewport_width": 412},
        )
        with self.assertRaisesRegex(ValueError, "firefox"):
            validate_overrides(
                descriptor(),
                {"rendering.browser.engine": "firefox"},
                base_values={"rendering.browser.mobile_emulation": True},
            )

    def test_cache_replay_and_rendered_link_capture_are_independent_gates(self):
        with self.assertRaisesRegex(ValueError, "replay"):
            validate_overrides(
                descriptor(),
                {"cache.invalidate": True},
                base_values={"cache.mode": "replay"},
            )
        with self.assertRaisesRegex(ValueError, "route evidence"):
            validate_overrides(descriptor(), {"rendering.rendered_links.crawl": True})
        result = validate_overrides(
            descriptor(),
            {
                "rendering.rendered_links.store": True,
                "rendering.rendered_links.crawl": False,
            },
        )
        self.assertFalse(result["rendering.rendered_links.crawl"])

    def test_list_destination_resolution_cannot_silently_change_site_crawl(self):
        values = {
            "discovery.resolve_redirect_destination": True,
            "discovery.resolve_canonical_destination": True,
        }
        with self.assertRaisesRegex(ValueError, "URL list"):
            validate_overrides(descriptor(), values)
        self.assertEqual(
            validate_overrides(descriptor(), values, input_mode="list"), values
        )

    def test_resource_fetch_requires_sqlite_but_metadata_retention_is_separate(self):
        with self.assertRaisesRegex(ValueError, "SQLite"):
            validate_overrides(
                descriptor(), {"resources.fetch": True}, storage_backend="json"
            )
        values = {"resources.fetch": True, "storage.body_mode": "off"}
        self.assertEqual(validate_overrides(descriptor(), values), values)

    def test_preview_keeps_explicit_overrides_and_labels_core_validation(self):
        values = {"limits.max_urls": 40, "scope.include_patterns": ["/catalog/"]}
        with patch(
            "builtins.open", side_effect=AssertionError("No I/O in draft module")
        ):
            preview = preview_configuration(descriptor(), values)
        self.assertEqual(preview["overrides"], values)
        self.assertEqual(preview["state"], "draft")
        self.assertTrue(preview["requires_core_validation"])
        self.assertEqual(len(preview["changes"]), 2)
        self.assertNotIn("http.proxy", preview["overrides"])

    def test_current_core_descriptor_and_loader_roundtrip_when_available(self):
        try:
            from seohead.crawl.settings import describe_settings, load
        except ImportError:
            self.skipTest(
                "Run this pure suite in the existing core environment for schema roundtrip"
            )
        live = {"settings": describe_settings()}
        self.assertEqual(
            {row["path"] for row in describe_controls(live)},
            {row["path"] for row in live["settings"]},
        )
        overrides = {
            "limits.max_urls": 40,
            "limits.max_depth": 2,
            "limits.max_requests": 100,
            "limits.max_crawl_seconds": 60,
            "speed.min_delay_seconds": 2.0,
            "sitemaps.auto_discover": True,
            "scope.include_patterns": ["/catalog/"],
            "scope.exclude_extensions": [".pdf"],
            "scope.include_media_types": ["text/html"],
            "rendering.mode": "js",
            "rendering.browser.viewport_width": 390,
            "rendering.browser.viewport_height": 844,
            "rendering.artifacts.console_errors": True,
            "resources.fetch": True,
            "storage.body_mode": "captured_entity_bytes",
        }
        validated = validate_overrides(live, overrides)
        with patch.dict(os.environ, {}, clear=True):
            effective = load(overrides=validated)
        for path, value in validated.items():
            actual = effective
            for part in path.split("."):
                actual = actual[part]
            self.assertEqual(actual, value, path)


if __name__ == "__main__":
    unittest.main()
