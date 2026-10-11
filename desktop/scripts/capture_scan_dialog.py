"""Screens of the «Новый скан» family for scripts/capture_screens.py: the dialog (ScanDialog/ScanList), its settings window
(Sc*) and the one-line launcher (QuickScan). Each is painted over the real application window, as in the canvas.

  newscan[:site|sitemap|list|sf|rpsbad|ready]      the dialog; list leaves the URL box empty and shows its placeholder
  scanset:<speed|scope|request|robots|render|extract|storage|profiles>[:bad|ignore|js]   the settings window
  quickscan[:idle|mode|params|outside|busy|noproject]   the launcher; «busy» starts a real scan on the project's own stand

Everything shown is real: the project is opened through the core, the settings descriptor comes from the core, nothing
is invented. ``busy`` needs the project's site to be served locally (the shop stand) and stops that scan after the frame.
"""

from __future__ import annotations

import time

from PyQt5.QtCore import QPoint, Qt
from PyQt5.QtGui import QColor, QPainter, QPixmap
from PyQt5.QtWidgets import QApplication, QCheckBox, QLineEdit, QToolButton



def pump(times=10, pause=0.02):
    for _ in range(times):
        QApplication.processEvents()
        time.sleep(pause)


def type_into(edit, text):
    edit.setText(text)
    edit.textEdited.emit(text)


class Job:
    """Built by capture_screens.build(); ``render_image`` returns the finished frame."""

    def __init__(self, name, options, open_project):
        self.kind, _, self.arg = name.partition(":")
        self.options, self.open_project = options, open_project

    def close(self):
        pass

    # ---- the application window ----------------------------------------------------------------------------------
    def window(self, width, height, theme, lang, project=True):
        from seohead_desktop.app import MainWindow

        window = MainWindow(persistent=False)
        window.prefs.set("view.theme", theme)
        window.prefs.set("view.language", lang)
        window.show_startup_workspace()
        if project and self.options.get("project"):
            self.open_project(window, self.options["project"])
        window.setAttribute(Qt.WA_DontShowOnScreen, True)
        window.resize(width, height)
        window.show()
        pump(3)
        window.resize(width, height)  # the offscreen screen is 800x600 and clamps the first resize
        pump(3)
        if project and self.options.get("project"):
            if window.crawl_descriptor is None:
                window.load_crawl_descriptor()
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline and window.crawl_descriptor is None:
                pump(2, 0.05)
        return window

    def render_image(self, width, height, theme, lang):
        window = self.window(width, height, theme, lang, project=self.arg != "noproject")
        try:
            image = QPixmap(width, height)
            window.render(image)
            return {"newscan": self.dialog, "scanset": self.settings, "quickscan": self.quick}[self.kind](window, image, width, height)
        finally:
            window.close()

    @staticmethod
    def overlay(image, shot, x=None, y=None, dim=True):
        painter = QPainter(image)
        if dim:
            painter.fillRect(image.rect(), QColor(0, 0, 0, 100))
        painter.drawPixmap((image.width() - shot.width()) // 2 if x is None else x, (image.height() - shot.height()) // 2 if y is None else y, shot)
        painter.end()
        return image

    # ---- «Новый скан» ---------------------------------------------------------------------------------------------
    def dialog(self, window, image, width, height):
        from seohead_desktop.screens.new_scan import NewScanDialog

        dialog = NewScanDialog(window)
        dialog.setAttribute(Qt.WA_DontShowOnScreen, True)
        dialog.show()
        pump(8)
        arg = self.arg
        if dialog.draft is not None:
            if arg in ("sitemap", "list", "sf"):
                dialog.findChild(QToolButton, "scanSource_" + arg).click()
            if arg == "sitemap":
                dialog.sitemap_edit.setText(dialog.draft.target.rstrip("/") + "/sitemap.xml")
                dialog.sitemap_edit.textEdited.emit(dialog.sitemap_edit.text())
            if arg == "rpsbad":
                type_into(dialog.findChild(QLineEdit, "scanRequestRate"), "3")
            if arg == "ready":
                dialog.findChild(QCheckBox, "scanLargeApproval").click()
        pump(6)
        shot = dialog.grab()
        dialog.close()
        return self.overlay(image, shot)

    # ---- «Настройки скана» ----------------------------------------------------------------------------------------
    def settings(self, window, image, width, height):
        from seohead_desktop.screens.new_scan_settings import ScanSettingsDialog

        page, _, variant = self.arg.partition(":")
        draft = self.draft_of(window)
        if variant == "bad":
            type_into_draft(draft, "depth", "-3")
            type_into_draft(draft, "rps", "3")
        if variant == "ignore":
            draft.set_value("robots.policy", "ignore")
        if variant == "js":
            draft.set_value("rendering.mode", "js")
        dialog = ScanSettingsDialog(draft, window, window, page or "speed")
        dialog.setAttribute(Qt.WA_DontShowOnScreen, True)
        size = (min(1380, max(720, width - 40)), min(840, max(600, height - 40)))
        dialog.resize(*size)
        dialog.show()
        pump(8)
        shot = dialog.grab()
        dialog.close()
        return self.overlay(image, shot)

    def draft_of(self, window):
        from seohead_desktop.screens.new_scan_draft import ScanDraft

        site = ((window.project_result or {}).get("project") or {}).get("site") or {}
        descriptor = window.crawl_descriptor
        if descriptor is None:  # no project: the saved core answer for the settings descriptor (tests/core_fixtures)
            descriptor = json.loads((FIXTURES / "crawl_describe_settings.json").read_text(encoding="utf-8"))
        return ScanDraft(descriptor, target=site.get("target") or "", host=site.get("host") or "",
                         project_directory=window.project_directory or "", prefs=window.prefs)

    # ---- «Быстрый запуск» -----------------------------------------------------------------------------------------
    def quick(self, window, image, width, height):
        from seohead_desktop.screens.quick_scan import QuickScanPopup

        state = self.arg or "idle"
        popup = QuickScanPopup(window)
        popup.setAttribute(Qt.WA_DontShowOnScreen, True)
        popup.place()
        popup.show()
        pump(8)
        bar = popup.bar
        if state == "outside":
            bar.url.setText("https://example.org/")
        if state == "busy":
            run_id = bar.start()
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline and not (run_id and bar.live_row.isVisibleTo(popup)):
                pump(3)
            pump(20, 0.1)
        extra = None
        if state == "mode":
            bar.mode_menu.setAttribute(Qt.WA_DontShowOnScreen, True)
            bar.mode_menu.popup(QPoint(0, 0))
            pump(4)
            extra = bar.mode_menu
        elif state == "params":
            bar._toggle_params()
            bar.popover.setAttribute(Qt.WA_DontShowOnScreen, True)
            pump(4)
            extra = bar.popover
        popup.adjustSize()
        pump(4)
        x = (width - popup.width()) // 2
        anchor = window.project_button
        y = anchor.mapTo(window, QPoint(0, anchor.height())).y() + 8
        painter = QPainter(image)
        painter.drawPixmap(x, y, popup.grab())
        if extra is not None:
            owner = bar.mode_button if state == "mode" else bar.chip
            at = owner.mapTo(popup, QPoint(0, owner.height() + 4)) + QPoint(x, y)
            painter.drawPixmap(at, extra.grab())
        painter.end()
        if state == "busy":
            bar.stop()
            pump(10, 0.1)
        extra and extra.hide()
        popup.close()
        return image


def type_into_draft(draft, key, text):
    draft.set_text(key, text)


__all__ = ["Job"]
