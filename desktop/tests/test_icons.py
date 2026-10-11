"""Vector resolution, state color and licensed-source preservation contracts."""

import hashlib
import unittest

from PyQt5.QtCore import QRect, QSize, Qt
from PyQt5.QtGui import QIcon, QPainter, QPixmap
from PyQt5.QtTest import QTest
from PyQt5.QtWidgets import QPushButton

from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.icons import (
    ASSET_ROOT,
    MaterialIconLabel,
    _SvgEngine,
    material_icon,
)
from seohead_desktop.ui.presentation import theme_tokens


class IconTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def test_all_assets_are_painted_at_each_physical_resolution_without_mutation(self):
        paths = sorted(ASSET_ROOT.glob("*.svg"))
        self.assertGreaterEqual(len(paths), 53)
        before = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}
        for path in paths:
            engine = _SvgEngine(path.read_bytes(), "#49454F")
            self.assertTrue(
                all(r.isValid() for r in engine.renderers.values()), path.name
            )
            for scale in (1, 1.5, 2, 3):
                pixmap = engine.scaledPixmap(
                    QSize(24, 24), QIcon.Normal, QIcon.Off, scale
                )
                self.assertEqual(pixmap.width(), round(24 * scale))
                self.assertEqual(pixmap.devicePixelRatio(), scale)
                image = pixmap.toImage()
                self.assertTrue(
                    any(
                        image.pixelColor(x, y).alpha()
                        for x in range(image.width())
                        for y in range(image.height())
                    ),
                    path.name,
                )
        self.assertEqual(
            before, {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}
        )

    def test_vector_paint_uses_the_actual_device_and_hover_tint(self):
        colors = theme_tokens()["colors"]
        symbol = material_icon("check_circle")
        rendered = []
        for mode, expected in (
            (QIcon.Normal, colors["on_surface_variant"]),
            (QIcon.Active, colors["primary"]),
            (QIcon.Disabled, colors["outline"]),
        ):
            canvas = QPixmap(54, 54)
            canvas.setDevicePixelRatio(3)
            canvas.fill(Qt.transparent)
            painter = QPainter(canvas)
            symbol.paint(painter, QRect(0, 0, 18, 18), Qt.AlignCenter, mode)
            painter.end()
            image = canvas.toImage()
            opaque = [
                image.pixelColor(x, y).name()
                for x in range(54)
                for y in range(54)
                if image.pixelColor(x, y).alpha() == 255
            ]
            self.assertTrue(opaque)
            self.assertEqual(set(opaque), {expected.lower()})
            rendered.append(image)
        self.assertNotEqual(rendered[0], rendered[1])

    def test_explicit_contrast_color_survives_active_state(self):
        engine = _SvgEngine((ASSET_ROOT / "play_arrow.svg").read_bytes(), "#ffffff")
        image = engine.scaledPixmap(QSize(24, 24), QIcon.Active, QIcon.Off, 2).toImage()
        opaque = {
            image.pixelColor(x, y).name()
            for x in range(48)
            for y in range(48)
            if image.pixelColor(x, y).alpha() == 255
        }
        self.assertEqual(opaque, {"#ffffff"})

    def test_label_hover_repaints_vector_and_invalid_paths_are_refused(self):
        button = QPushButton("Icon host")
        button.resize(120, 60)
        label = MaterialIconLabel("check_circle", parent=button)
        label.move(10, 10)
        button.show()
        QTest.qWait(10)
        self.assertEqual(label.sizeHint(), QSize(18, 18))
        self.assertFalse(label.grab().isNull())
        QTest.mouseMove(button, button.rect().center())
        self.app.processEvents()
        self.assertFalse(label.grab().isNull())
        label.set_material_icon("help")
        self.assertEqual(label.name, "help")
        self.assertTrue(material_icon("../outside").isNull())
        self.assertTrue(material_icon("nonexistent").isNull())
        button.close()
        button.deleteLater()
        self.app.processEvents()


if __name__ == "__main__":
    unittest.main()
