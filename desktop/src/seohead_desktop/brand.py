"""Packaged brand assets (assets/brand): spider for 12-40 px marks, logo for 48-512 px, lockup = mark + name.

On the dark and high-contrast themes the white variants are used (spider, lockup); the logo tile is the same on all themes.
"""

from PyQt5.QtCore import QRectF, Qt
from PyQt5.QtGui import QIcon, QPainter, QPixmap
from PyQt5.QtSvg import QSvgRenderer

from . import theming
from .common import ROOT

BRAND = ROOT / "assets/brand"


def _white():
    return theming.active_theme() != "light"


def asset(name):
    return str(BRAND / f"{name}.svg")


def logo_path():
    return asset("seohead-logo")


def spider_path():
    return asset("seohead-spider-white" if _white() else "seohead-spider")


def lockup_path():
    return asset("seohead-lockup-white" if _white() else "seohead-lockup")


def render(path, width, height=None, ratio=2):
    """A crisp pixmap of one SVG, fitted into width x height (device pixel ratio ``ratio``)."""
    renderer = QSvgRenderer(path)
    box = renderer.defaultSize()
    height = height or round(width * box.height() / max(1, box.width()))
    pixmap = QPixmap(width * ratio, height * ratio)
    pixmap.fill(Qt.transparent)
    painter = QPainter(pixmap)
    painter.setRenderHint(QPainter.Antialiasing)
    renderer.render(painter, QRectF(0, 0, width * ratio, height * ratio))
    painter.end()
    pixmap.setDevicePixelRatio(ratio)
    return pixmap


def spider_icon(size=16):
    return QIcon(render(spider_path(), size, size))


def app_icon():
    return QIcon(logo_path())


def set_application_identity(app):
    """Apply the native SpiderHead app name and bundled icon to the current process."""
    app.setApplicationName("SpiderHead")
    app.setApplicationDisplayName("SpiderHead")
    icon = app_icon()
    app.setWindowIcon(icon)

    # Cocoa uses NSApp for the Dock tile on source launches; the frozen bundle also
    # declares SpiderHead in Info.plist so the Switcher has the same identity.
    try:
        from AppKit import NSApplication, NSImage
    except ImportError:
        return
    image = NSImage.alloc().initWithContentsOfFile_(str(ROOT / "assets/app/seohead-1024.png"))
    if image is not None:
        NSApplication.sharedApplication().setApplicationIconImage_(image)
