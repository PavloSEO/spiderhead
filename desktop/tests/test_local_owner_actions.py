import io
import json
import subprocess
import unittest

from seohead_desktop.local_owner_actions import (
    accept_goal_arguments,
    competitor_scan_arguments,
    competitor_scan_cancel_arguments,
    competitor_scan_status_arguments,
    manual_signoff_request,
    run_owner_action,
)


class LocalOwnerActionsTests(unittest.TestCase):
    def test_competitor_scan_commands_share_a_caller_run_id_and_resume_settings(self):
        run_id = "f8b826c6-c622-4000-8d41-090f683ea2bc"
        args = competitor_scan_arguments(
            "project",
            "custom:competitor-audit",
            7,
            run_id,
            approve_large_crawl=True,
            producer_build="a" * 40,
        )
        self.assertEqual(args[:1], ["project-competitors-scan"])
        self.assertEqual(args[args.index("--run-id") + 1], run_id)
        self.assertIn("--approve-large-crawl", args)
        self.assertEqual(args[args.index("--producer-build") + 1], "a" * 40)
        self.assertEqual(
            competitor_scan_status_arguments("project", run_id),
            ["project-competitors-scan-status", "--directory", "project", "--run-id", run_id],
        )
        self.assertEqual(
            competitor_scan_cancel_arguments("project", run_id),
            ["project-competitors-scan-cancel", "--directory", "project", "--run-id", run_id],
        )
        with self.assertRaises(ValueError):
            competitor_scan_arguments("project", "custom:task", 7, run_id.upper())

    def test_goal_acceptance_uses_revisioned_local_cli_route(self):
        self.assertEqual(
            accept_goal_arguments("project", "inbox:goal-1", 7),
            [
                "project-inbox-goal",
                "--directory",
                "project",
                "--entry-id",
                "inbox:goal-1",
                "--state",
                "accepted",
                "--expected-revision",
                "7",
            ],
        )
        with self.assertRaises(ValueError):
            accept_goal_arguments("project", "inbox:goal-1", None)

    def test_manual_signoff_records_explicit_owner_review(self):
        arguments, input_data = manual_signoff_request(
            "project", "custom:review", 9, "Reviewed the result."
        )
        self.assertEqual(arguments, ["project-checklist-record"])
        self.assertNotIn("Reviewed the result", arguments)
        payload = json.loads(input_data)
        self.assertEqual(
            payload,
            {
                "directory": "project",
                "item_id": "custom:review",
                "expected_revision": 9,
                "record": {
                    "status": "succeeded",
                    "reason": "Reviewed the result.",
                    "reviewer": "OWNER",
                    "signoff": True,
                },
            },
        )
        with self.assertRaises(ValueError):
            manual_signoff_request("project", "custom:review", 9, "  ")

    def test_local_action_requires_a_successful_json_receipt(self):
        class InputPipe:
            def __init__(self):
                self.data = bytearray()

            def write(self, value):
                self.data.extend(value)

            def close(self):
                pass

        class Process:
            def __init__(self, _args, *, stdin, stdout, stderr):
                self.stdout = io.BytesIO(b'{"ok": true}')
                self.stdin = InputPipe() if stdin == subprocess.PIPE else None
                self.returncode = 0

            def wait(self, timeout):
                return self.returncode

            def kill(self):
                self.returncode = -9

        created = []

        def popen(*args, **kwargs):
            process = Process(*args, **kwargs)
            created.append(process)
            return process

        self.assertEqual(
            run_owner_action(
                "seohead",
                ["project-checklist-record"],
                stdin_data=b'{"record":{}}',
                popen=popen,
            ),
            {"ok": True},
        )
        self.assertEqual(bytes(created[0].stdin.data), b'{"record":{}}')

        def refused(*args, **kwargs):
            process = Process(*args, **kwargs)
            process.returncode = 1
            return process

        with self.assertRaises(ValueError):
            run_owner_action("seohead", ["project-inbox-goal"], popen=refused)


if __name__ == "__main__":
    unittest.main()
