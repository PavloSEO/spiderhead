import json
import os
import sys
import tempfile
import time
import unicodedata
import unittest
from pathlib import Path
from unittest.mock import patch

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")


from seohead_desktop import project_create
from seohead_desktop.project_create import (
    ProjectCreator,
    error_text,
    normalize_target,
    suggest_directory,
    validate,
)
from seohead_desktop.qt import app as qt_app

CORE = str(Path(sys.executable).with_name("seohead"))
TARGET = "http://crawl.localhost:1/"  # nothing listens there: project-new must not need the network


def wait_for(app, condition, timeout=30):
    end = time.monotonic() + timeout
    while time.monotonic() < end and not condition():
        app.processEvents()
        time.sleep(0.01)
    return condition()


class LocalValidationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)

    def test_target_is_normalised_and_host_extracted(self):
        self.assertEqual(normalize_target(" shop.example.test "), "https://shop.example.test")
        self.assertEqual(normalize_target("http://crawl.localhost:1/"), "http://crawl.localhost:1/")
        self.assertEqual(normalize_target(""), "")
        self.assertEqual(project_create.target_host("Shop.Example.test/path"), "shop.example.test")

    def test_suggested_directory_comes_from_the_host(self):
        self.assertEqual(suggest_directory(self.base, "shop.example.test"), str(self.base / "shop-example-test"))
        self.assertEqual(suggest_directory(self.base, "", ""), "")

    def test_validation_names_the_first_problem(self):
        self.assertEqual(validate("", TARGET)[0], "directory")
        self.assertEqual(validate(str(self.base / "no" / "deep"), TARGET)[0], "directory")  # parent is missing
        (self.base / "taken").mkdir()
        self.assertEqual(validate(str(self.base / "taken"), TARGET)[0], "directory")
        self.assertEqual(validate(str(self.base / "new"), "")[0], "target")
        self.assertEqual(validate(str(self.base / "new"), "ftp://shop.example.test")[0], "target")
        self.assertIsNone(validate(str(self.base / "new"), TARGET))
        self.assertIsNone(validate(str(self.base / "new"), "shop.example.test"))

    def test_core_reason_is_taken_from_stderr(self):
        self.assertEqual(error_text("seohead: project-new\nerror: target must have a valid public hostname\n"),
                         "target must have a valid public hostname")
        self.assertEqual(error_text("noise"), "Ядро не создало проект")


@unittest.skipUnless(Path(CORE).is_file(), "core CLI is not installed next to the interpreter")
class CoreRunTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.creator = ProjectCreator()
        self.created, self.failed = [], []
        self.creator.created.connect(self.created.append)
        self.creator.failed.connect(self.failed.append)
        self.addCleanup(self.creator.deleteLater)

    def run_core(self, *args):
        self.creator.start(*args)
        self.assertTrue(wait_for(self.app, lambda: self.created or self.failed))

    def test_real_core_creates_a_project_without_the_network(self):
        directory = self.base / "crawl"
        self.run_core(CORE, str(directory), TARGET, "Локальный стенд")
        self.assertEqual(self.failed, [])
        answer = self.created[0]
        self.assertEqual(Path(answer["path"]).resolve(), directory.resolve())
        written = json.loads((directory / "project.json").read_text(encoding="utf-8"))
        site = written["site"]
        self.assertEqual((site["host"], site["target"]), ("crawl.localhost", TARGET))
        self.assertEqual(unicodedata.normalize("NFC", site["label"]), "Локальный стенд")  # the core may store another Unicode form
        self.assertEqual(answer["project"]["project_uuid"], written["project_uuid"])
        self.assertTrue((directory / "scans").is_dir())

    def test_existing_folder_is_refused_by_the_core_and_left_alone(self):
        directory = self.base / "crawl"
        self.run_core(CORE, str(directory), TARGET)
        self.created.clear()
        before = (directory / "project.json").read_bytes()
        self.run_core(CORE, str(directory), TARGET)
        self.assertEqual(self.created, [])
        self.assertIn("already exists", self.failed[0])
        self.assertEqual((directory / "project.json").read_bytes(), before)

    def test_core_refusal_of_a_bad_address_is_shown_as_its_reason(self):
        self.run_core(CORE, str(self.base / "x"), "http://localhost-without-dot/")
        self.assertEqual(self.created, [])
        self.assertTrue(self.failed[0])
        self.assertFalse((self.base / "x").exists())

    def test_missing_core_and_unrunnable_core_fail_honestly(self):
        self.creator.start(None, str(self.base / "x"), TARGET)
        self.assertEqual(self.failed, ["Ядро seohead не найдено"])
        self.failed.clear()
        self.run_core(str(self.base / "no-such-core"), str(self.base / "x"), TARGET)
        self.assertEqual(self.failed, ["Не удалось запустить ядро seohead"])
        self.assertFalse((self.base / "x").exists())

    def test_unexpected_answer_is_an_error_not_a_project(self):
        script = self.base / "fake-core"
        script.write_text("#!/bin/sh\necho '{\"ok\": false}'\n")
        script.chmod(0o755)
        self.run_core(str(script), str(self.base / "x"), TARGET)
        self.assertEqual(self.failed, ["Ядро вернуло неожиданный ответ"])

    def test_a_second_start_while_running_is_ignored(self):
        script = self.base / "slow-core"
        script.write_text("#!/bin/sh\nsleep 0.3\necho '{\"ok\": true, \"path\": \"/p\"}'\n")
        script.chmod(0o755)
        self.creator.start(str(script), str(self.base / "x"), TARGET)
        self.assertTrue(self.creator.running)
        self.creator.start(str(script), str(self.base / "y"), TARGET)
        self.assertTrue(wait_for(self.app, lambda: self.created or self.failed))
        self.assertEqual(len(self.created), 1)
        with patch.object(project_create, "TIMEOUT_MS", 50):
            self.created.clear()
            self.creator.start(str(script), str(self.base / "z"), TARGET)
            self.assertTrue(wait_for(self.app, lambda: self.created or self.failed))


if __name__ == "__main__":
    unittest.main()
