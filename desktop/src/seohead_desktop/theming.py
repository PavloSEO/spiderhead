"""Design-system v2 theming: tokens.json (three themes) -> QSS and a legacy colour view.

One canonical source (``theme/tokens.json``). The QSS is generated from it by plain
string substitution; changing the theme rebuilds the stylesheet once.
"""

from __future__ import annotations

import json
import subprocess
import sys
from functools import lru_cache
from pathlib import Path
from string import Template

from PyQt5.QtCore import QObject, pyqtSignal
from PyQt5.QtGui import QFontDatabase, QGuiApplication

THEME_DIR = Path(__file__).resolve().parent / "theme"
THEMES = ("light", "dark", "hc")

# Old colour names still read by widgets that predate design v2 -> v2 role (or badge/note pair).
# Removed together with theme/legacy.qss once the screens are rebuilt from the canvas.
_LEGACY_ROLES = {
    "primary": "primary",
    "on_primary": "on_primary",
    "primary_container": "selected",
    "on_primary_container": "on_selected",
    "secondary": "text_2",
    "on_secondary": "on_primary",
    "secondary_container": "selected",
    "on_secondary_container": "on_selected",
    "error": "error",
    "on_error": "on_primary",
    "surface": "surface",
    "on_surface": "text",
    "on_surface_variant": "text_2",
    "surface_container_lowest": "base",
    "surface_container_low": "raised",
    "surface_container": "hover",
    "surface_container_high": "disabled_bg",
    "surface_container_highest": "divider",
    "background": "surface",
    "on_background": "text",
    "outline": "disabled_fg",
    "outline_variant": "outline",
    "inverse_surface": "text",
    "inverse_on_surface": "base",
    "inverse_primary": "primary",
    "success": "success",
    "on_success": "base",
    "warning": "warning",
    "on_warning": "base",
}
_LEGACY_BADGES = {
    "tertiary": ("goal", 1),
    "tertiary_container": ("goal", 0),
    "on_tertiary": ("goal", 0),
    "on_tertiary_container": ("goal", 1),
    "error_container": ("err", 0),
    "on_error_container": ("err", 1),
    "success_container": ("ok", 0),
    "on_success_container": ("ok", 1),
    "warning_container": ("warn", 0),
    "on_warning_container": ("warn", 1),
}


class ThemeSignals(QObject):
    changed = pyqtSignal(str)


signals = ThemeSignals()
_active = "light"


@lru_cache(maxsize=1)
def raw_tokens():
    return json.loads((THEME_DIR / "tokens.json").read_text())


def active_theme():
    return _active


def set_active_theme(name):
    global _active
    if name not in THEMES:
        raise ValueError(f"unknown theme: {name}")
    if name != _active:
        _active = name
        signals.changed.emit(name)


def theme(name=None):
    return raw_tokens()["themes"][name or _active]


def roles(name=None):
    return theme(name)["roles"]


def metrics():
    return raw_tokens()["metrics"]


def legacy_colors(name=None):
    t = theme(name)
    r = t["roles"]
    out = {key: r[role] for key, role in _LEGACY_ROLES.items()}
    out["scrim"] = "#000000"
    out.update({key: t["badges"][kind][index] for key, (kind, index) in _LEGACY_BADGES.items()})
    out.update(
        {
            "rating_star": "#F5B100",
            "dark_border": "rgba(255,255,255,.15)",
            "dark_border_hover": "rgba(255,255,255,.3)",
            "dark_surface_hover": "rgba(255,255,255,.1)",
        }
    )
    return out


@lru_cache(maxsize=8)
def legacy_tokens(name):
    """Shape of the pre-v2 tokens.json, filled from v2 values, for not-yet-rebuilt widgets."""
    m = metrics()
    return {
        "colors": legacy_colors(name),
        "font_family": m["font_family"],
        "font_size": m["font"]["body"],
        "font_small": m["font"]["meta"],
        "font_title": m["font"]["title"],
        "font_rail": m["font"]["rail"],
        "table_row_height": m["row"]["standard"],
        "density": {k: m["row"][k] for k in ("compact", "standard", "comfortable")},
        "layout": {
            "navigation_width": m["layout"]["navigation"],
            "navigation_rail": m["layout"]["rail"],
            "compact_breakpoint": m["layout"]["compact_breakpoint"],
            "content_margin": m["layout"]["margin_screen"],
            "content_margin_narrow": m["layout"]["margin_panel"],
            "section_spacing": 24,
            "section_spacing_narrow": 16,
        },
        "motion": m["motion"],
        "scrollbar_extent": m["scrollbar"]["extent"],
        "scrollbar_thumb_minimum": m["scrollbar"]["thumb_min"],
        "splitter_width": m["layout"]["splitter"],
        "radius_control": m["radius"]["control"],
        "radius_panel": m["radius"]["card"],
        "radius_popup": m["radius"]["card"],
    }


def _mono_family(stack):
    """First installed family of the comma-separated stack (Qt QSS takes a single family)."""
    families = [f.strip() for f in stack.split(",")]
    if QGuiApplication.instance() is None:
        return families[0]
    installed = set(QFontDatabase().families())
    return next((f for f in families if f in installed), families[-1])


def substitution(name=None):
    """Flat name -> value mapping used by the QSS templates."""
    t = theme(name)
    m = metrics()
    values = dict(t["roles"])
    for kind, (bg, fg) in t["badges"].items():
        values[f"badge_{kind}_bg"], values[f"badge_{kind}_fg"] = bg, fg
    for kind, (bg, border, fg) in t["notes"].items():
        values[f"note_{kind}_bg"], values[f"note_{kind}_border"], values[f"note_{kind}_fg"] = (
            bg,
            border,
            fg,
        )
    for key, value in t["data"].items():
        values[f"data_{key}"] = value
    for group in ("font", "radius", "row", "control", "layout", "canvas"):
        for key, value in m[group].items():
            values[f"{group}_{key}"] = value
    values.update(
        font_family=m["font_family"],
        mono_family=_mono_family(m["font_mono"]),
        scroll=m["scrollbar"]["extent"],
        thumb_min=m["scrollbar"]["thumb_min"],
        icon_root=(Path(__file__).resolve().parent / "assets/icons").as_posix(),
    )
    return values


def stylesheet(name=None):
    """Generated QSS for one theme (legacy rules first, v2 rules override them)."""
    legacy = {
        **legacy_colors(name),
        **{k: v for k, v in legacy_tokens(name or _active).items() if isinstance(v, (str, int))},
        "icon_root": substitution(name)["icon_root"],
    }
    return "\n".join(
        [
            Template((THEME_DIR / "legacy.qss").read_text()).substitute(legacy),
            Template((THEME_DIR / "theme.qss").read_text()).substitute(substitution(name)),
            *(
                Template(part.read_text()).substitute(substitution(name))
                for part in sorted(
                    p for p in (THEME_DIR / "parts").glob("*.qss") if p.name != "canvas.qss"
                )
            ),
            Template((THEME_DIR / "parts/canvas.qss").read_text()).substitute(substitution(name)),
        ]
    )


def system_prefers_high_contrast():
    """Best-effort OS high-contrast flag (checked once at start-up, never in a paint path)."""
    try:
        if sys.platform == "darwin":
            out = subprocess.run(
                ["defaults", "read", "com.apple.universalaccess", "increaseContrast"],
                capture_output=True,
                text=True,
                timeout=2,
            )
            return out.stdout.strip() == "1"
        if sys.platform == "win32":
            import ctypes

            class HighContrast(ctypes.Structure):
                _fields_ = [
                    ("cbSize", ctypes.c_uint),
                    ("dwFlags", ctypes.c_uint),
                    ("lpszDefaultScheme", ctypes.c_wchar_p),
                ]

            info = HighContrast(ctypes.sizeof(HighContrast), 0, None)
            ctypes.windll.user32.SystemParametersInfoW(
                0x42, info.cbSize, ctypes.byref(info), 0
            )  # SPI_GETHIGHCONTRAST
            return bool(info.dwFlags & 1)  # HCF_HIGHCONTRASTON
    except (OSError, subprocess.SubprocessError, AttributeError):
        pass
    return False
