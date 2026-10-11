import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import copy
import threading

from PyQt5.QtCore import QCoreApplication, QEvent, QEventLoop, QObject, QTimer

from seohead_desktop import keep_awake, schedule_core, tray


def defaults(**changes):
    value = copy.deepcopy(schedule_core.DEFAULT_SETTINGS)
    for key, fields in changes.items():
        if isinstance(fields, dict) and isinstance(value.get(key), dict):
            value[key].update(fields)
        else:
            value[key] = fields
    return value


def show_answer(**changes):
    answer = {
        "ok": True,
        "revision": 3,
        "settings": {**defaults(), "format": "seohead.schedule.v1", "project_uuid": "u"},
        "state": {"last_window": None, "last_run_at": None, "last_result": None},
        "resume_artifact": None,
        "due": {"due": False, "reason": "disabled", "time_zone": "Europe/Minsk"},
        "next_due_at": None,
    }
    answer.update(changes)
    return answer


class ValidateTests(unittest.TestCase):
    def test_defaults_pass(self):
        self.assertEqual(schedule_core.validate(defaults())["time_zone"], "auto")

    def test_rejects_bad_values(self):
        bad = [
            defaults(weekdays=["xyz"]),
            defaults(month_days=[0]),
            defaults(window={"start": "24:00", "end": "06:00"}),
            defaults(window={"start": "01:00", "end": "01:00"}),
            defaults(time_zone="Mars/Olympus"),
            defaults(crawl={"mode": "sitemap", "sitemap_url": None}),
            defaults(crawl={"mode": "list", "urls": []}),
            defaults(crawl={"urls": ["ftp://x"]}),
            defaults(crawl={"min_delay_seconds": 0.1}),
            defaults(crawl={"max_urls": 0}),
            defaults(user_agent={"preset": "custom", "custom": None}),
            defaults(user_agent={"preset": "spiderhead", "custom": "bot\nX: 1"}),
        ]
        for value in bad:
            with self.subTest(value=value), self.assertRaises(ValueError):
                schedule_core.validate(value)


class UserAgentTests(unittest.TestCase):
    def test_presets_match_the_core_and_no_third_party_identity(self):
        self.assertIn("spiderhead", schedule_core.USER_AGENT_PRESETS)
        self.assertEqual(schedule_core.DEFAULT_PRESET, "spiderhead")
        self.assertNotIn("screaming", " ".join(schedule_core.USER_AGENT_PRESETS).lower())

    def test_custom_text_wins_over_the_preset_label(self):
        self.assertEqual(
            schedule_core.user_agent_text({"preset": "bingbot", "custom": "MyAudit/1.0"}),
            "MyAudit/1.0",
        )
        self.assertEqual(
            schedule_core.user_agent_text({"preset": "bingbot", "custom": None}), "Bingbot"
        )


class CommandTests(unittest.TestCase):
    def test_arguments_follow_the_core_cli(self):
        self.assertEqual(schedule_core.read_args("/p"), ["schedule-show", "--directory", "/p"])
        args = schedule_core.update_args("/p", 3, {"enabled": True})
        self.assertEqual(args[:1], ["schedule-set"])
        self.assertEqual(args[args.index("--expected-revision") + 1], "3")
        self.assertEqual(args[args.index("--input") + 1], '{"settings": {"enabled": true}}')
        self.assertEqual(
            schedule_core.run_due_args("/p"), ["schedule-run-due", "--directory", "/p"]
        )

    def test_state_is_read_from_show_output(self):
        answer = show_answer(
            revision=4,
            next_due_at="2026-10-12T01:00:00Z",
            competitors=[
                {
                    "directory": "competitors/example",
                    "url": "https://example.test/",
                    "schedule_configured": False,
                }
            ],
            state={
                "last_window": "2026-10-05",
                "last_run_at": "2026-10-05T01:00:00Z",
                "last_result": [{"ok": True}, {"ok": False}],
            },
        )
        state = schedule_core.parse_state(answer)
        self.assertEqual(state["revision"], 4)
        self.assertEqual(state["settings"]["crawl"]["mode"], "spider")
        self.assertTrue(state["next_run"])
        self.assertEqual(state["last_run"]["status"], "failed")
        self.assertFalse(state["due_now"])
        self.assertEqual(state["zone"], "Europe/Minsk")
        self.assertEqual(state["competitors"][0]["directory"], "competitors/example")

    def test_save_validates_then_reads_back(self):
        calls = []

        def fake(executable, args, timeout=45):
            calls.append(args)
            if args[0] == "schedule-set":
                return {"ok": True, "revision": 4}
            return show_answer(revision=4)

        with self.assertRaises(ValueError):
            schedule_core.save_schedule(
                "seohead", "/p", 3, defaults(crawl={"max_urls": 0}), call=fake
            )
        self.assertEqual(calls, [])
        state = schedule_core.save_schedule("seohead", "/p", 3, defaults(enabled=True), call=fake)
        self.assertEqual(calls[0][0], "schedule-set")
        self.assertEqual(state["revision"], 4)

    def test_refusal_reports_the_core_line(self):
        import tempfile

        with tempfile.NamedTemporaryFile("w", suffix=".sh", delete=False) as script:
            self.addCleanup(os.unlink, script.name)
            script.write("#!/bin/sh\necho 'error: schedule revision conflict' >&2\n")
        os.chmod(script.name, 0o755)
        with self.assertRaises(ValueError) as caught:
            schedule_core._call(script.name, ["schedule-set"])
        self.assertEqual(str(caught.exception), "schedule revision conflict")

    def test_run_due_keeps_a_failed_run_as_an_answer(self):
        result = schedule_core.run_due(
            "seohead", "/p", call=lambda e, a, timeout=45: {"ok": False, "ran": True, "reason": ""}
        )
        self.assertEqual(result, {"ran": True, "ok": False, "reason": ""})


class RunPassTests(unittest.TestCase):
    def test_only_due_projects_run_and_keep_awake_brackets_each_run(self):
        events = []

        class Awake:
            def set_active(self, wanted):
                events.append(("awake", wanted))

        due = {"/a": {"due_now": True}, "/b": {"due_now": False}, "/c": {"due_now": True}}

        def read(executable, directory):
            if directory == "/bad":
                raise ValueError("unavailable")
            return due[directory]

        def run(executable, directory):
            events.append(("run", directory))
            return {"ran": True, "ok": True, "reason": ""}

        result = tray.run_pass("seohead", ["/a", "/b", "/c"], Awake(), read=read, run=run)
        self.assertEqual(result, {"ran": 2, "unavailable": 0, "projects": 3})
        self.assertEqual([e for e in events if e[0] == "run"], [("run", "/a"), ("run", "/c")])
        self.assertEqual(events[-1], ("awake", False))

    def test_unreadable_project_is_counted_not_fatal(self):
        class Awake:
            def set_active(self, wanted):
                pass

        def read(executable, directory):
            raise ValueError("unavailable")

        result = tray.run_pass(
            "seohead", ["/x"], Awake(), read=read, run=lambda e, d: {"ran": True}
        )
        self.assertEqual(result["unavailable"], 1)


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

    def test_runs_caffeinate_only_while_wanted(self):
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

    def test_no_caffeinate_means_no_process(self):
        awake, started = self.make(binary=None)
        awake.set_active(True)
        self.assertEqual(started, [])


class ScheduleJobsDeliveryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = QCoreApplication.instance() or QCoreApplication([])

    def _wait(self, loop, milliseconds=2000):
        timer = QTimer()
        timer.setSingleShot(True)
        timer.timeout.connect(loop.quit)
        timer.start(milliseconds)
        loop.exec_()
        timer.stop()

    def test_result_callback_runs_on_the_gui_thread(self):
        jobs = schedule_core.ScheduleJobs()
        owner = QObject()
        main_thread = threading.get_ident()
        received = []
        loop = QEventLoop()

        def success(value):
            received.append((value, threading.get_ident()))
            loop.quit()

        jobs.submit(lambda: "done", success, lambda error: loop.quit(), owner)
        self._wait(loop)
        self.assertEqual(received, [("done", main_thread)])

    def test_closed_owner_drops_a_queued_result(self):
        jobs = schedule_core.ScheduleJobs()
        owner = QObject()
        started = threading.Event()
        release = threading.Event()
        self.addCleanup(release.set)
        received = []

        def work():
            started.set()
            release.wait(2)
            return "done"

        jobs.submit(work, received.append, lambda error: received.append(error), owner)
        self.assertTrue(started.wait(2))
        owner.deleteLater()
        QCoreApplication.sendPostedEvents(owner, QEvent.DeferredDelete)
        release.set()
        self._wait(QEventLoop(), 100)
        self.assertEqual(received, [])


if __name__ == "__main__":
    unittest.main()
