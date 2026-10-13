import os
import unittest

from tests._qt import ui

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QLabel, QPushButton

from seohead_desktop import i18n
from seohead_desktop.qt import app as qt_app
from seohead_desktop.settings_store import AppSettings
from seohead_desktop.source_service import load_sources, redacted_providers
from seohead_desktop.ui.brand_logos import BORDER, BrandTile
from seohead_desktop.ui.settings import SECTION_IDS, full_schema, sources
from seohead_desktop.ui.settings.context import SettingsContext
from seohead_desktop.ui.settings.dialog import SettingsDialog
from seohead_desktop.ui.settings.source_layout import SourceRow
from tests._sources_support import PROJECT, core_call, sources_hook


def texts(widget):
    return [label.text() for label in widget.findChildren(QLabel)]


class SourcesSectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        i18n.set_language("ru")

    def page(self, hook, project=PROJECT):
        actions = {"sources": hook} if hook else {}
        return sources.build_page(
            AppSettings(schema=full_schema()),
            SettingsContext(project_directory=project, actions=actions),
        )

    def test_section_is_twelfth_and_follows_mcp(self):
        self.assertEqual(len(SECTION_IDS), 12)
        self.assertEqual(SECTION_IDS[SECTION_IDS.index("mcp") + 1], "sources")

    def test_without_a_core_hook_nothing_is_invented(self):
        self.assertIn(ui("Нет данных"), texts(self.page(None)))

    def test_loading_then_every_provider_of_the_core_with_its_state(self):
        pending = []
        page = self.page(sources_hook(pending=pending))
        self.assertTrue(any("Читаю" in t for t in texts(page)))
        operation, callback, _err = pending[0]
        self.assertEqual(operation, "snapshot")
        callback(load_sources("core", "snapshot", call=core_call()))
        rows = page.findChildren(SourceRow)
        self.assertEqual(len(rows), 14)
        joined = " | ".join(texts(page))
        for expected in (
            "Google Search Console",
            ui("ключ задан · не проверен"),
            ui("нужен ключ"),
            ui("без ключа"),
        ):
            self.assertIn(expected, joined)

    def test_configured_is_never_reported_as_connected(self):
        page = self.page(sources_hook())
        states = {row.pid: row.state.text() for row in page.findChildren(SourceRow)}
        self.assertEqual(states["gsc"], ui("ключ задан · не проверен"))
        self.assertNotIn(ui("подключено"), states.values())

    def test_check_all_runs_only_the_local_doctor(self):
        hook = sources_hook()
        page = self.page(hook)
        page.check_all()
        commands = [args[0] for args in hook.call.calls]
        self.assertIn("sources-doctor", commands)
        self.assertFalse({"sources-sync", "provider-verify"} & set(commands))
        self.assertTrue(any("проверено" in t for t in texts(page)))

    def test_core_failure_is_shown_in_place(self):
        page = self.page(
            sources_hook(
                {"provider-readiness": ValueError("x"), "provider-registry": ValueError("x")}
            )
        )
        self.assertTrue(any("не вернуло список источников" in t for t in texts(page)))

    def test_secret_values_never_cross_the_adapter(self):
        leaking = {
            "gsc": {
                "readiness_state": "configured_unverified",
                "api_key": "SECRET-123",
                "credential_sources": {
                    "service_account": {"source_reference": "/redacted/SECRET.json"}
                },
            }
        }
        self.assertNotIn("SECRET", repr(redacted_providers(leaking)))

    def test_details_open_for_every_provider(self):
        page = self.page(sources_hook())
        for pid in page.provider_ids():
            page.show_detail(pid)
            self.assertTrue(texts(page), pid)
        page.show_list()

    def test_logos_are_white_tiles_with_the_brandbook_border(self):
        page = self.page(sources_hook())
        tiles = {tile.key for tile in page.findChildren(BrandTile)}
        self.assertTrue({"gsc", "metrika", "arsenkin", "dataforseo"} <= tiles, tiles)
        self.assertEqual(BORDER, "#DDE1E7")

    def test_dialog_lists_the_section_with_its_title(self):
        dialog = SettingsDialog(
            AppSettings(schema=full_schema()), SettingsContext(), section="sources"
        )
        self.assertEqual(dialog.section_title.text(), ui("Источники данных"))

    def test_spend_view_shows_core_units_and_waiting_limits_never_roubles(self):
        page = self.page(sources_hook())
        page._snapshot(load_sources("core", "snapshot", call=core_call()))
        page._open(("spend",))
        joined = " | ".join(texts(page))
        self.assertIn(ui("ЛИМИТЫ И ПОДТВЕРЖДЕНИЯ"), joined)
        self.assertNotIn("₽", joined)
        export = [b for b in page.findChildren(QPushButton) if b.text() == ui("Экспорт CSV")]
        self.assertEqual(len(export), 1)
        self.assertFalse(export[0].isEnabled())

    def test_core_missing_is_reported(self):
        from seohead_desktop.app import MainWindow

        window = MainWindow(persistent=False)
        window.core_executable = None
        seen = []
        window.request_sources("snapshot", seen.append, seen.append, window)
        # Source-service callbacks carry the original error, before UI rendering.
        self.assertEqual(seen, ["CLI ядра seohead не найден"])
        window.close()


if __name__ == "__main__":
    unittest.main()
