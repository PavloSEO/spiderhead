"""CrUX request construction stays bounded and separates URL/origin scope."""

import unittest

from seohead_desktop.screens.vitals import (
    METRICS,
    crux_arguments,
    pagespeed_collect_arguments,
    pagespeed_projection,
    pagespeed_read_arguments,
)


class CruxArgumentsTests(unittest.TestCase):
    def test_single_url_and_origin_requests_name_the_field_metrics(self):
        self.assertEqual(
            crux_arguments("url", ["https://example.test/page"], "PHONE"),
            [
                "crux-report",
                "--form-factor",
                "PHONE",
                "--metrics",
                ",".join(METRICS),
                "--url",
                "https://example.test/page",
            ],
        )
        self.assertIn(
            "https://example.test",
            crux_arguments("origin", ["https://example.test"], "DESKTOP"),
        )

    def test_template_sampling_is_capped_and_validated(self):
        args = crux_arguments("samples", ["https://example.test/a", "https://example.test/b"], "TABLET")
        self.assertIn("--max-samples", args)
        self.assertEqual(args[args.index("--max-samples") + 1], "2")
        with self.assertRaises(ValueError):
            crux_arguments("samples", ["https://example.test/a,b"], "PHONE")
        with self.assertRaises(ValueError):
            crux_arguments("origin", ["https://example.test/path"], "PHONE")
        with self.assertRaises(ValueError):
            crux_arguments("url", [], "PHONE")

    def test_pagespeed_collection_is_keyed_and_artifact_read_is_local(self):
        collect = pagespeed_collect_arguments(
            ["https://example.test/a"], "mobile_samples", "/private/artifacts"
        )
        self.assertEqual(collect[:7], [
            "provider-collect",
            "--provider",
            "pagespeed",
            "--operation",
            "mobile_samples",
            "--artifact-dir",
            "/private/artifacts",
        ])
        self.assertIn('"urls":["https://example.test/a"]', collect[-1])
        read = pagespeed_read_arguments("/private/artifacts", "local-artifact:" + "a" * 64)
        self.assertEqual(read[0], "provider-artifact-read")
        self.assertEqual(read[read.index("--limit") + 1], "25")
        self.assertNotIn("--input", read)

    def test_pagespeed_projection_drops_raw_artifact_and_auth_fields(self):
        result = pagespeed_projection(
            {
                "schema_version": "pagespeed-artifact-summary.v1",
                "artifact_ref": "local-artifact:private",
                "api_key": "never-display",
                "total": 1,
                "samples": [
                    {
                        "url": "https://example.test/page",
                        "strategy": "mobile",
                        "lab_only": True,
                        "categories": {"performance": {"score": 0.8, "title": "Performance", "raw": "omit"}},
                        "audits": {"largest-contentful-paint": {"numeric_value": 2500, "display_value": "2.5 s", "raw": "omit"}},
                        "raw_response": "omit",
                    }
                ],
            }
        )
        self.assertNotIn("api_key", result)
        self.assertNotIn("artifact_ref", result)
        self.assertNotIn("raw_response", result["samples"][0])
        self.assertNotIn("raw", result["samples"][0]["categories"]["performance"])
        self.assertNotIn("raw", result["samples"][0]["audits"]["largest-contentful-paint"])


if __name__ == "__main__":
    unittest.main()
