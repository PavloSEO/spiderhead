"""Official service marks on a white tile (sheets ProjSources / SetSources): 40 px tile, 1 px border, radius 10.

Single-colour Simple Icons SVGs are recoloured in memory to the brand colour; multi-colour official marks and PNGs
are drawn as they are. A service without a shipped mark gets a neutral Material glyph, never an invented logo.
"""

from __future__ import annotations

from pathlib import Path

from PyQt5.QtCore import QRectF, QSize, Qt
from PyQt5.QtGui import QColor, QImage, QPainter, QPainterPath, QPen
from PyQt5.QtSvg import QSvgRenderer
from PyQt5.QtWidgets import QWidget


from .icons import material_icon

ASSET_ROOT = Path(__file__).resolve().parents[1] / "assets" / "brands"
# key -> (file, brand colour for single-colour SVGs or None, mark size in px inside the 40 px tile)
LOGOS = {
    "gsc": ("gsc.png", None, 28),
    "ga4": ("ga4.svg", None, 26),
    "gtm": ("googletagmanager.svg", "#246FDB", 26),
    "metrika": ("metrika.svg", None, 28),
    "webmaster": ("webmaster.svg", None, 28),
    "bing": ("bing.png", None, 26),
    "topvisor": ("topvisor.png", None, 28),
    "psi": ("pagespeedinsights.svg", "#4285F4", 28),
    "arsenkin": ("arsenkin.png", None, 28),
    "dataforseo": ("dataforseo.png", None, 28),
    "yandex_cloud": ("yandexcloud-color.svg", None, 26),
}
TILE = 40
WHITE = "#FFFFFF"
BORDER = "#DDE1E7"  # the tile stays white with the same border in both themes (BrandBook)


def logo_path(key):
    """Absolute path of the shipped mark of ``key`` (None when the app has none)."""
    entry = LOGOS.get(key)
    path = ASSET_ROOT / entry[0] if entry else None
    return path if path is not None and path.is_file() else None


def _renderer(path, colour):
    raw = path.read_bytes()
    if colour:  # single-colour glyph: tint the in-memory root only, the licensed file stays intact
        raw = raw.replace(b"<svg ", f'<svg fill="{colour}" '.encode(), 1)
    return QSvgRenderer(raw)


class BrandTile(QWidget):
    """White rounded tile with the service mark; painted at the real device scale."""

    def __init__(self, key, size=TILE, parent=None):
        super().__init__(parent)
        self.key = key
        self.setFixedSize(size, size)
        self.setAttribute(Qt.WA_TransparentForMouseEvents)
        self._image = None
        self._renderer = None
        entry = LOGOS.get(key)
        path = logo_path(key)
        self.has_logo = path is not None
        if path is not None:
            if path.suffix == ".svg":
                self._renderer = _renderer(path, entry[1])
            else:
                self._image = QImage(str(path))
            self._mark = entry[2]

    def sizeHint(self):
        return QSize(self.width(), self.height())

    def paintEvent(self, _event):
        painter = QPainter(self)
        painter.setRenderHints(QPainter.Antialiasing | QPainter.SmoothPixmapTransform)
        tile = QRectF(self.rect()).adjusted(0.5, 0.5, -0.5, -0.5)
        shape = QPainterPath()
        shape.addRoundedRect(tile, 10, 10)
        painter.fillPath(shape, QColor(WHITE))
        painter.setPen(QPen(QColor(BORDER), 1))
        painter.drawPath(shape)
        if not self.has_logo:
            material_icon("sell", "role:text_muted").paint(painter, self.rect(), Qt.AlignCenter)
            return
        side = self._mark
        box = QRectF((self.width() - side) / 2, (self.height() - side) / 2, side, side)
        if self._renderer is not None:
            view = self._renderer.viewBoxF()
            scale = min(box.width() / view.width(), box.height() / view.height())
            fitted = QRectF(0, 0, view.width() * scale, view.height() * scale)
            fitted.moveCenter(box.center())
            self._renderer.render(painter, fitted)
        else:
            painter.drawImage(box, self._image)
