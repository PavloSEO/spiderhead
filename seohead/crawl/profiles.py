"""Named crawl profiles: saved ``crawl-site --config`` files selected by name.

A profile is the same JSON a ``--config`` file is, kept in one directory, so no
second settings format exists. Saving runs the file through the loader a crawl uses,
and refuses credential headers: a profile is a file on disk and must not hold a secret.
Overrides (``--set``) still apply on top of a selected profile, as they do on a file.
"""

from __future__ import annotations

import json
import os
import pathlib
import re

from seohead.core.filesystem import atomic_write_bytes
from seohead.crawl import settings as crawl_settings

NAME_RE = re.compile(r"[a-z0-9][a-z0-9_-]{0,63}")
DIR_ENV = "SEOHEAD_CRAWL_PROFILES"


class ProfileError(ValueError):
    """A profile name, file or stored content that cannot be used."""


def profiles_dir() -> pathlib.Path:
    raw = os.environ.get(DIR_ENV)
    if raw:
        return pathlib.Path(raw)
    return pathlib.Path.home() / ".config" / "seohead" / "crawl-profiles"


def _file_for(name: str) -> pathlib.Path:
    if not NAME_RE.fullmatch(name or ""):
        raise ProfileError(
            f"profile name {name!r}: use a-z, 0-9, '-' or '_', starting with a letter or digit, up to 64 characters"
        )
    return profiles_dir() / f"{name}.json"


def saved_names() -> list[str]:
    directory = profiles_dir()
    if not directory.is_dir():
        return []
    return sorted(p.stem for p in directory.glob("*.json") if NAME_RE.fullmatch(p.stem))


def path_for(name: str) -> str:
    """Path of a saved profile, for ``crawl-site --config``."""
    target = _file_for(name)
    if target.is_symlink():
        raise ProfileError(f"crawl profile {name!r} must not be a symlink")
    if not target.is_file():
        known = ", ".join(saved_names()) or "none"
        raise ProfileError(f"no crawl profile {name!r}; saved profiles: {known}")
    return str(target)


def show(name: str) -> dict[str, object]:
    """Return the validated, credential-free JSON stored for ``name``."""
    target = _file_for(name)
    if target.is_symlink():
        raise ProfileError(f"crawl profile {name!r} must not be a symlink")
    if not target.is_file():
        known = ", ".join(saved_names()) or "none"
        raise ProfileError(f"no crawl profile {name!r}; saved profiles: {known}")
    try:
        with target.open(encoding="utf-8") as handle:
            body = json.load(handle)
    except OSError as exc:
        raise ProfileError(f"cannot read crawl profile {name!r}: {exc}") from exc
    except ValueError as exc:
        raise ProfileError(f"crawl profile {name!r} is not valid JSON: {exc}") from exc
    if not isinstance(body, dict):
        raise ProfileError(f"crawl profile {name!r} must contain an object")
    try:
        validated = crawl_settings.load(str(target))
    except crawl_settings.ConfigError as exc:
        raise ProfileError(str(exc)) from exc
    if validated["http"]["credential_headers"]:
        raise ProfileError("a crawl profile cannot contain http.credential_headers")
    return body


def delete(name: str) -> None:
    """Remove the stored profile ``name``."""
    target = _file_for(name)
    if not target.is_file():
        known = ", ".join(saved_names()) or "none"
        raise ProfileError(f"no crawl profile {name!r}; saved profiles: {known}")
    target.unlink()


def save(name: str, config_file: str) -> str:
    """Validate ``config_file`` and store it as profile ``name``; returns the stored path."""
    target = _file_for(name)
    try:
        with open(config_file, encoding="utf-8") as handle:
            body = json.load(handle)
    except OSError as exc:
        raise ProfileError(f"cannot read config {config_file!r}: {exc}") from exc
    except ValueError as exc:
        raise ProfileError(f"config {config_file!r} is not valid JSON: {exc}") from exc
    if not isinstance(body, dict):
        raise ProfileError(f"config {config_file!r} must contain an object")
    try:
        validated = crawl_settings.load(config_file)
    except crawl_settings.ConfigError as exc:
        raise ProfileError(str(exc)) from exc
    if validated["http"]["credential_headers"]:
        raise ProfileError(
            "a profile cannot store http.credential_headers; pass credentials at crawl time"
        )
    target.parent.mkdir(parents=True, exist_ok=True)
    atomic_write_bytes(
        target,
        (json.dumps(body, ensure_ascii=False, indent=2) + "\n").encode("utf-8"),
        prefix=".profile-",
    )
    return str(target)
