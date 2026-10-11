"""Canvas component values from the application's canonical theme tokens."""

from .. import theming


def metric(name):
    return theming.metrics()["canvas"][name]


def color(name):
    return theming.roles()[name]
