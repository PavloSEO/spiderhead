import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from seohead_desktop import keep_awake, schedule_core, tray


def settings(**changes):
    value = {"schedule": dict(schedule_core.DEFAULT_SETTINGS["schedule"]), "crawl": dict(schedule_core.DEFAULT_SETTINGS["crawl"])}
    for section, fields in changes.items():
        value[section].update(fields)
    return value


class ValidateTests(unittest.TestCase):
    def test_defaults_are_valid_and_normalised(self):
        checked = schedule_core.validate(settings())
        self.assertEqual(checked["schedule"]["window_start"], "01:00")
        self.assertFalse(checked["schedule"]["enabled"])
        self.assertEqual(checked["crawl"]["requests_per_second"], 1.0)

    def test_weekdays_and_days_of_month_are_sorted_and_unique(self):
        checked = schedule_core.validate(settings(schedule={"weekdays": [4, 0, 4], "days_of_month": [15, 1, 15]}))
        self.assertEqual(checked["schedule"]["weekdays"], [0, 4])
        self.assertEqual(checked["schedule"]["days_of_month"], [1, 15])

    def test_rejects_bad_values_with_one_error_each(self):
        bad = [
            settings(schedule={"weekdays": [7]}),
            settings(schedule={"days_of_month": [0]}),
            settings(schedule={"window_start": "24:00"}),
            settings(schedule={"time_zone": "Mars/Olympus"}),
            settings(schedule={"enabled": "yes"}),
            settings(crawl={"mode": "everything"}),
            settings(crawl={"user_agent_preset": "screamingfrog"}),
            settings(crawl={"user_agent_preset": "custom", "user_agent": "   "}),
            settings(crawl={"user_agent_preset": "custom", "user_agent": "bot\nX: 1"}),
            settings(crawl={"requests_per_second": 0}),
            settings(crawl={"requests_per_second": 50}),
            settings(crawl={"url_budget": 0}),
            settings(crawl={"url_budget": 1.5}),
        ]
        for value in bad:
            with self.subTest(value=value):
                with self.assertRaises(ValueError):
                    schedule_core.validate(value)

    def test_custom_user_agent_is_trimmed_and_kept_only_for_custom(self):
        checked = schedule_core.validate(settings(crawl={"user_agent_preset": "custom", "user_agent": "  MyAudit/1.0  "}))
        self.assertEqual(checked["crawl"]["user_agent"], "MyAudit/1.0")
        self.assertEqual(schedule_core.user_agent_text(checked["crawl"]), "MyAudit/1.0")
        other = schedule_core.validate(settings(crawl={"user_agent_preset": "bingbot", "user_agent": "ignored"}))
        self.assertEqual(other["crawl"]["user_agent"], "")


class UserAgentTests(unittest.TestCase):
    def test_default_identifies_spiderhead_with_a_contact_url(self):
        agent = schedule_core.user_agent_text(schedule_core.DEFAULT_SETTINGS["crawl"])
        self.assertTrue(agent.startswith("SpiderHeadBot/"))
        self.assertIn("(+https://github.com/PavloSEO/spiderhead)", agent)

    def test_presets_contain_no_third_party_crawler_identity(self):
        for key, (_label, agent) in schedule_core.USER_AGENT_PRESETS.items():
            with self.subTest(preset=key):
                self.assertNotIn("Screaming", agent)
                self.assertNotIn("Frog", agent)

    def test_every_preset_has_a_label_and_custom_is_the_only_empty_one(self):
        empty = [k for k, (label, agent) in schedule_core.USER_AGENT_PRESETS.items() if not agent]
        self.assertEqual(empty, ["custom"])
        self.assertTrue(all(label.strip() for label, _ in schedule_core.USER_AGENT_PRESETS.values()))


class CommandTests(unittest.TestCase):
    def test_commands_carry_the_project_directory_and_the_revision(self):
        self.assertEqual(schedule_core.read_args("/p"), ["project-schedule", "--directory", "/p"])
        args = schedule_core.update_args("/p", 3, {"a": 1})
        self.assertEqual(args[:1], ["project-schedule-update"])
        self.assertEqual(args[args.index("--expected-revision") + 1], "3")
        self.assertEqual(schedule_core.run_due_args("/p"), ["schedule-run-due", "--directory", "/p"])

    def test_read_parses_revision_facts_and_defaults_missing_fields(self):
        state = schedule_core.parse_state({
            "revision": 4,
            "schedule": {"enabled": True, "weekdays": [0], "days_of_month": [1, 15], "window_start": "01:00",
                         "window_end": "06:00", "time_zone": "Europe/Minsk", "competitors": False},
            "next_run": "2026-10-12T01:00:00+03:00",
            "last_run": {"at": "2026-10-05T01:00:00+03:00", "status": "finished"},
        })
        self.assertEqual(state["revision"], 4)
        self.assertEqual(state["settings"]["crawl"]["user_agent_preset"], "spiderhead")
        self.assertEqual(state["next_run"], "12.10.2026 01:00")
        self.assertEqual(state["last_run"], {"at": "05.10.2026 01:00", "status": "finished"})

    def test_unreadable_stamps_are_none_not_an_error(self):
        state = schedule_core.parse_state({"next_run": "soon", "last_run": None})
        self.assertIsNone(state["next_run"])
        self.assertIsNone(state["last_run"])

    def test_save_validates_before_calling_the_core(self):
        calls = []
        with self.assertRaises(ValueError):
            schedule_core.save_schedule("seohead", "/p", 1, settings(crawl={"url_budget": 0}), call=lambda *a: calls.append(a))
        self.assertEqual(calls, [])

    def test_save_sends_the_checked_settings_with_the_revision(self):
        calls = []

        def fake(executable, args):
            calls.append((executable, args))
            return {"ok": True, "revision": 2, "schedule": settings()["schedule"], "crawl": settings()["crawl"]}

        state = schedule_core.save_schedule("seohead", "/p", 1, settings(crawl={"requests_per_second": 2}), call=fake)
        self.assertEqual(state["revision"], 2)
        executable, args = calls[0]
        self.assertEqual(executable, "seohead")
        self.assertEqual(args[args.index("--expected-revision") + 1], "1")
        self.assertIn('"requests_per_second": 2.0', args[args.index("--input") + 1])


class RunPassTests(unittest.TestCase):
    def test_running_scans_are_summed_and_unavailable_projects_counted(self):
        answers = {"/a": {"ok": True, "running": 1}, "/b": {"ok": False}, "/c": {"ok": True, "running": 2}}

        def fake(executable, args):
            directory = args[args.index("--directory") + 1]
            if not answers[directory].get("ok"):
                raise ValueError("Core command unavailable")
            return answers[directory]

        result = tray.run_pass("seohead", ["/a", "/b", "/c"], call=fake)
        self.assertEqual(result, {"running": 3, "unavailable": 1, "projects": 3})

    def test_negative_or_missing_running_counts_as_none(self):
        self.assertEqual(schedule_core.run_due("seohead", "/a", call=lambda e, a: {"running": -4})["running"], 0)
        self.assertEqual(schedule_core.run_due("seohead", "/a", call=lambda e, a: {})["running"], 0)


class FakeProcess:
    def __init__(self, args):
        self.args = args
        self.alive = True
        self.terminated = False

    def poll(self):
        return None if self.alive else 0

    def terminate(self):
        self.terminated = True
        self.alive = False


class KeepAwakeTests(unittest.TestCase):
    def make(self, binary="/usr/bin/caffeinate"):
        started = []

        def popen(args, **kwargs):
            process = FakeProcess(args)
            started.append(process)
            return process

        return keep_awake.KeepAwake(which=lambda name: binary, popen=popen), started

    def test_runs_caffeinate_only_while_a_scan_is_wanted(self):
        awake, started = self.make()
        awake.set_active(False)
        self.assertEqual(started, [])
        awake.set_active(True)
        awake.set_active(True)
        self.assertEqual(len(started), 1)
        self.assertEqual(started[0].args, ["/usr/bin/caffeinate", "-i"])
        awake.set_active(False)
        self.assertTrue(started[0].terminated)
        self.assertFalse(awake.active)

    def test_restarts_when_the_previous_process_has_gone(self):
        awake, started = self.make()
        awake.set_active(True)
        started[0].alive = False
        awake.set_active(True)
        self.assertEqual(len(started), 2)

    def test_no_caffeinate_means_no_process(self):
        awake, started = self.make(binary=None)
        awake.set_active(True)
        self.assertEqual(started, [])
        self.assertFalse(awake.active)


if __name__ == "__main__":
    unittest.main()
