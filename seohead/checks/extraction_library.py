"""A library of ready-made extraction rule sets: named JSON presets shipped with the package.

Writing CSS selectors by hand is fine for one site-specific field, but the common SEO
questions — every heading, every image alt, the DOM inventory, inline styles — are the
same on every site. Each preset is a plain ``extraction_rules.v1`` list validated by the
same closed grammar as user rules: data-only, no callables, no regex, bounded matches.
Load one by name (``scan-extract --preset headings``) instead of copy-pasting selectors.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from seohead.checks.extraction_rules import validate_rules

PRESET_DIR = Path(__file__).parent / "extraction_presets"


def available() -> list[dict[str, Any]]:
    """Every shipped preset as ``{"name", "description", "rules"}``, sorted by name."""
    presets = []
    for path in sorted(PRESET_DIR.glob("*.json")):
        payload = json.loads(path.read_text(encoding="utf-8"))
        presets.append(
            {
                "name": payload["name"],
                "description": payload["description"],
                "rules": len(payload["rules"]),
            }
        )
    return presets


def preset(name: str) -> list[dict[str, Any]]:
    """The validated rule list of one preset (case-insensitive name match)."""
    if not isinstance(name, str) or not name:
        raise ValueError("preset name must be a nonempty string")
    wanted = name.strip().lower()
    for path in PRESET_DIR.glob("*.json"):
        if path.stem.lower() != wanted:
            continue
        payload = json.loads(path.read_text(encoding="utf-8"))
        rules = validate_rules(payload["rules"])
        return rules
    known = ", ".join(item["name"] for item in available())
    raise ValueError(f"unknown extraction preset {name!r}; shipped presets: {known}")
