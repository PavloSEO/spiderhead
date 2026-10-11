"""Menu rows for the context menus and the menu bar (canvas Menus.dc.html).

An entry is ``(icon, label, action, shortcut, enabled, tip)``; ``None`` is a separator. The shortcut is shown on the
right as text, so it never registers a second global shortcut next to the screen's own ``QShortcut`` objects.
A disabled entry keeps its place and says in the tooltip why it is not available.
"""

from PyQt5.QtGui import QIcon
from PyQt5.QtWidgets import QMenu

from ..i18n import tr
from .icons import material_icon
from .kit import unavailable_tip


def entry(icon, label, action=None, shortcut="", enabled=True, tip=""):
    return (icon, label, action, shortcut, enabled, tip)


def unavailable(icon, label, shortcut=""):
    """A row that stays visible for the board, disabled with the core reason."""
    return entry(icon, label, None, shortcut, False, unavailable_tip(label))


def fill_menu(menu: QMenu, entries):
    for item in entries:
        if item is None:
            menu.addSeparator()
            continue
        icon, label, action, shortcut, enabled, tip = item
        text = tr(label) + ("\t" + shortcut if shortcut else "")
        act = menu.addAction(material_icon(icon) if icon else QIcon(), text)
        act.setEnabled(bool(enabled and action))
        if tip:
            act.setToolTip(tr(tip))
        if action and enabled:
            act.triggered.connect(lambda _checked=False, call=action: call())
    return menu
