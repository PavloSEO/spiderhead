"""«Картинки» (ToolImages): a folder is read locally, compression runs only on the user's «Сжать», the core answers are
mapped without invention. No sample values: the files below are real image files written into a temporary folder.
"""

import os
import shutil
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QThreadPool
from PyQt5.QtWidgets import QFileDialog

from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens import images
from seohead_desktop.ui.kit import StatePanel


def run_now(pool, function, callback, owner):
    callback(function())


class ImagesScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        qt_app()

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="seohead-images-test-"))
        self.host = SimpleNamespace(
            core_executable="/opt/seohead/bin/seohead", pool=QThreadPool.globalInstance()
        )
        self.window = images.ImagesWindow(self.host)

    def tearDown(self):
        self.window.close()
        shutil.rmtree(self.tmp, ignore_errors=True)

    def make_images(self):
        from PIL import Image

        big = Image.effect_noise((900, 700), 90).convert("RGB")
        big.save(self.tmp / "chester-01.jpg", quality=95)
        Image.new("RGB", (120, 80), (10, 120, 200)).save(self.tmp / "logo.png")
        Image.new("RGB", (120, 80), (200, 120, 10)).save(self.tmp / "logo.webp")
        (self.tmp / "sub").mkdir()
        Image.new("RGB", (60, 60), (1, 2, 3)).save(self.tmp / "sub" / "icon.jpg")
        (self.tmp / "notes.txt").write_text("not an image", encoding="utf-8")
        (self.tmp / "optimized").mkdir()
        Image.new("RGB", (60, 60), (1, 2, 3)).save(self.tmp / "optimized" / "old.jpg")

    def pick_folder(self, folder):
        with patch.object(QFileDialog, "getExistingDirectory", return_value=str(folder)):
            self.window.choose_folder()

    def test_without_a_folder_the_window_is_an_honest_empty_state_and_cannot_compress(self):
        self.assertIs(self.window.center.currentWidget(), self.window.empty_folder)
        self.window.go_step(2)
        self.assertFalse(self.window.primary.isEnabled())
        self.assertEqual(self.window.model.rowCount(), 0)

    def test_scan_reads_real_files_skips_hidden_output_and_non_images(self):
        self.make_images()
        self.pick_folder(self.tmp)
        names = sorted(row["rel"] for row in self.window.model.rows)
        self.assertEqual(names, ["chester-01.jpg", "logo.png", "logo.webp", "sub/icon.jpg"])
        self.assertEqual(self.window.out_dir, str(self.tmp / "optimized"))
        self.assertEqual(self.window.proxy.rowCount(), 4)

    def test_filters_count_only_what_is_on_disk(self):
        self.make_images()
        self.pick_folder(self.tmp)
        self.window.heavy.setChecked(True)
        self.assertEqual(self.window.proxy.rowCount(), 1)
        self.window.heavy.setChecked(False)
        self.window.not_webp.setChecked(True)
        self.assertEqual(self.window.proxy.rowCount(), 3)

    def test_command_preview_shows_the_flags_the_core_will_get(self):
        self.make_images()
        self.pick_folder(self.tmp)
        text = self.window.command.text()
        for part in (
            "images-optimize",
            "--format webp",
            "--quality 80",
            "--max-width 1600",
            "--output-dir",
        ):
            self.assertIn(part, text)

    def test_compression_is_started_by_the_button_and_maps_core_results(self):
        self.make_images()
        self.pick_folder(self.tmp)
        self.window.go_step(2)
        calls = []

        def fake_optimize(command):
            calls.append(command)
            return {
                "ok": True,
                "results": [
                    {
                        "file": str(self.tmp / "chester-01.jpg"),
                        "out": str(self.tmp / "optimized" / "chester-01.webp"),
                        "ok": True,
                        "before_bytes": 1000,
                        "after_bytes": 400,
                        "saved_pct": 60.0,
                    },
                    {"file": str(self.tmp / "logo.png"), "ok": False, "error": "boom"},
                ],
            }

        with (
            patch.object(images, "run_background", side_effect=run_now),
            patch.object(images, "run_optimize", side_effect=fake_optimize),
        ):
            self.assertEqual(calls, [])
            self.window.compress()
        self.assertEqual(len(calls), 1)
        self.assertIn("--format", calls[0])
        self.assertIn("--max-width", calls[0])
        rows = {row["rel"]: row for row in self.window.model.rows}
        self.assertEqual(rows["chester-01.jpg"]["status"], "Сжат")
        self.assertEqual(rows["chester-01.jpg"]["after"], 400)
        self.assertEqual(rows["logo.png"]["status"], "Ошибка")
        self.assertTrue(self.window.compressed)
        pairs = self.window.redirect_pairs()
        self.assertEqual(pairs, [{"old_url": "/chester-01.jpg", "new_url": "/chester-01.webp"}])

    def test_refused_core_answer_is_reported_not_shown_as_numbers(self):
        self.make_images()
        self.pick_folder(self.tmp)
        with (
            patch.object(images, "run_background", side_effect=run_now),
            patch.object(images, "run_optimize", return_value=None),
        ):
            self.window.compress()
        self.assertFalse(self.window.compressed)
        self.assertEqual(self.window.note.text(), "Ядро не вернуло результат")

    def test_redirect_rules_come_from_the_core_and_are_not_invented(self):
        rules_seen = []

        def fake_rules(core, redirects, fmt):
            rules_seen.append((core, redirects, fmt))
            return ["rewrite ^/a.jpg$ /a.webp permanent;"]

        self.window.compressed = True
        self.window.model.rows = [
            {
                "path": "/x/a.jpg",
                "rel": "a.jpg",
                "size": 1,
                "ext": ".jpg",
                "selected": True,
                "after": 1,
                "pct": 1,
                "status": "Сжат",
                "out": str(self.tmp / "optimized" / "a.webp"),
            }
        ]
        self.window.out_dir = str(self.tmp / "optimized")
        with (
            patch.object(images, "run_background", side_effect=run_now),
            patch.object(images, "run_redirects", side_effect=fake_rules),
        ):
            self.window.go_step(4)
        self.assertEqual(rules_seen[0][2], "nginx")
        self.assertEqual(rules_seen[0][1], [{"old_url": "/a.jpg", "new_url": "/a.webp"}])
        self.assertIn("rewrite ^/a.jpg$", self.window.rules.toPlainText())

    def test_task_step_waits_for_the_core_and_creates_nothing(self):
        panel = self.window.pages.widget(2).findChild(StatePanel)
        self.assertIsNotNone(panel)
        self.assertEqual(panel.kind, "waiting")
        self.window.go_step(5)
        self.assertFalse(self.window.primary.isEnabled())

    def test_core_header_line_is_skipped_before_json(self):
        text = 'seohead: images-optimize\n{\n  "ok": true,\n  "results": []\n}\n'
        self.assertEqual(images.parse_core_json(text), {"ok": True, "results": []})
        self.assertIsNone(images.parse_core_json("seohead: x\nnot json"))
        self.assertEqual(images.parse_core_json('{"rules": []}'), {"rules": []})

    def test_bytes_are_shown_in_russian_units(self):
        self.assertEqual(images.kb(2048), "2,0 КБ")
        self.assertEqual(images.kb(300 * 1024), "300 КБ")
        self.assertEqual(images.kb(1536 * 1024), "1,5 МБ")


if __name__ == "__main__":
    unittest.main()
