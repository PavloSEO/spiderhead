"""Read external-provider secrets from ``~/.config``, with environment overrides.

The providers intentionally do not share one variable or a proprietary configuration format.
Existing secrets already have stable locations, including ``~/.config/arsenkin/token`` and
``~/.config/yandex-wordstat/{api_key,folder_id}``. Moving them would break working scripts and
could leave forgotten copies behind. Therefore:

* the canonical persistent source is a user-readable file on disk;
* environment variables take precedence for CI, containers, and one-off runs with another key;
* **a secret value is never printed or included in an exception.** Messages identify only the
  missing path or variable. This is more important than debugging convenience because logs may
  be copied into session transcripts.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

from seohead.core.filesystem import stage_bytes

CONFIG_ROOT = Path(os.path.expanduser("~/.config"))


class MissingCredential(RuntimeError):
    """A credential is missing; the message names sources but never exposes a value."""


def is_private_mode(mode: int) -> bool:
    """True when a secret file is not readable by group or others.

    POSIX permission bits carry this. Windows has no such bits: ``os.stat`` always reports
    ``0o666``/``0o444`` and ``os.chmod`` only toggles read-only, so the check could never pass
    there. On Windows privacy comes from the ACL of the user profile (``~/.config`` lives in it),
    and the mode check is skipped.
    """
    if os.name == "nt":
        return True
    return not mode & 0o077


def read(path: str, env_var: str, *, hint: str = "") -> str:
    """Read a secret from ``$env_var`` or ``~/.config/<path>``.

    ``path`` is relative to ``~/.config``, for example ``arsenkin/token``.
    """
    from_env = os.environ.get(env_var)
    if from_env and from_env.strip():
        return from_env.strip()

    file_path = CONFIG_ROOT / path
    if not file_path.exists():
        raise MissingCredential(
            f"credential not found: store it in {file_path} or set ${env_var}"
            + (f". {hint}" if hint else "")
        )
    value = file_path.read_text(encoding="utf-8").strip()
    if not value:
        raise MissingCredential(f"{file_path} is empty; store a value there or set ${env_var}")
    return value


def _private_target(path: str) -> Path:
    """Return ``~/.config/<path>`` after refusing symlinks on the file and its directory."""
    target = CONFIG_ROOT / path
    if target.parent.is_symlink() or target.is_symlink():
        raise ValueError(f"credential storage must not use symlinks: {path}")
    return target


def write_secret(path: str, value: str, *, replace: bool = False) -> None:
    """Store ``value`` at ``~/.config/<path>`` with mode 0600, atomically.

    ``replace=False`` fails if the file exists; ``replace=True`` fails if it does not. A failed
    write leaves the previous file untouched. Errors name only the relative path, never the value.
    """
    if not value:
        raise ValueError(f"empty credential value rejected for {path}")
    target = _private_target(path)
    target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(target.parent, 0o700)
    if replace and not target.is_file():
        raise FileNotFoundError(f"no existing credential to replace at {path}")
    if not replace and target.exists():
        raise FileExistsError(f"credential already exists at {path}; use replace")
    staged = stage_bytes(target.parent, ".credential-", value.encode("utf-8"), mode=0o600)
    try:
        if replace:
            os.replace(staged, target)
        else:
            # os.link refuses an existing target, so a concurrent writer cannot be overwritten.
            os.link(staged, target)
    finally:
        Path(staged).unlink(missing_ok=True)


def revoke_secret(path: str) -> bool:
    """Delete ``~/.config/<path>``; return whether a file was removed."""
    target = _private_target(path)
    try:
        target.unlink()
    except FileNotFoundError:
        return False
    return True


def available(path: str, env_var: str) -> bool:
    """Return whether a credential exists, for diagnostics and graceful provider skips."""
    try:
        read(path, env_var)
    except MissingCredential:
        return False
    return True


def _invalid_source(path: str, accepted: list[str], reason: str) -> dict[str, Any]:
    return {
        "state": "invalid",
        "source_reference": f"config:{path}",
        "accepted_source_references": accepted,
        "reason": reason,
    }


def source_status(path: str, env_var: str, *, file_env_var: str | None = None) -> dict[str, Any]:
    """Describe a credential source without returning its value or an absolute path."""
    accepted = [f"env:{env_var}", f"config:{path}"]
    custom_file = (os.environ.get(file_env_var) or "").strip() if file_env_var else ""
    if file_env_var:
        accepted.append(f"env-path:{file_env_var}")
    from_env = os.environ.get(env_var)
    if from_env and from_env.strip():
        return {
            "state": "configured_unverified",
            "source_reference": f"env:{env_var}",
            "accepted_source_references": accepted,
        }

    if custom_file:
        candidate = Path(custom_file).expanduser()
        source_reference = f"env-path:{file_env_var}"
        if candidate.is_symlink() or candidate.parent.is_symlink():
            return {
                "state": "invalid",
                "source_reference": source_reference,
                "accepted_source_references": accepted,
                "reason": "configured credential file uses a symlink",
            }
        try:
            info = candidate.stat()
        except OSError:
            return {
                "state": "invalid",
                "source_reference": source_reference,
                "accepted_source_references": accepted,
                "reason": "configured credential file is missing or unreadable",
            }
        if not candidate.is_file() or not is_private_mode(info.st_mode):
            return {
                "state": "invalid",
                "source_reference": source_reference,
                "accepted_source_references": accepted,
                "reason": "configured credential file is not a private regular file",
            }
        if info.st_size > 1024 * 1024:
            return {
                "state": "invalid",
                "source_reference": source_reference,
                "accepted_source_references": accepted,
                "reason": "configured credential file exceeds 1 MiB",
            }
        try:
            configured = candidate.read_text(encoding="utf-8").strip()
        except (OSError, UnicodeError):
            configured = ""
        if not configured:
            return {
                "state": "invalid",
                "source_reference": source_reference,
                "accepted_source_references": accepted,
                "reason": "configured credential file is unreadable or empty",
            }
        return {
            "state": "configured_unverified",
            "source_reference": source_reference,
            "accepted_source_references": accepted,
        }

    candidate = CONFIG_ROOT / path
    if candidate.is_symlink() or candidate.parent.is_symlink():
        return _invalid_source(path, accepted, "configured credential path uses a symlink")
    try:
        info = candidate.stat()
    except FileNotFoundError:
        return {
            "state": "missing",
            "source_reference": None,
            "accepted_source_references": accepted,
        }
    except OSError:
        return _invalid_source(path, accepted, "configured credential file is unreadable")
    if not candidate.is_file() or not is_private_mode(info.st_mode):
        return _invalid_source(
            path, accepted, "configured credential path is not a private regular file"
        )
    if info.st_size > 1024 * 1024:
        return _invalid_source(path, accepted, "configured credential file exceeds 1 MiB")
    try:
        value = candidate.read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        return _invalid_source(
            path, accepted, "configured credential file is unreadable or invalid text"
        )
    if not value.strip():
        return _invalid_source(path, accepted, "configured credential file is empty")
    return {
        "state": "configured_unverified",
        "source_reference": f"config:{path}",
        "accepted_source_references": accepted,
    }


# --- provider-specific credentials -----------------------------------------


def arsenkin_token() -> str:
    return read(
        "arsenkin/token", "ARSENKIN_TOKEN", hint="Create the token in your arsenkin.ru account."
    )


def yandex_cloud_api_key() -> str:
    return read(
        "yandex-wordstat/api_key",
        "YANDEX_CLOUD_API_KEY",
        hint="Create it in Yandex AI Studio with the search-api.webSearch.user role.",
    )


def yandex_cloud_folder_id() -> str:
    return read(
        "yandex-wordstat/folder_id",
        "YANDEX_CLOUD_FOLDER_ID",
        hint="Use the folder ID shown in the Yandex Cloud console.",
    )


def metrika_token() -> str:
    return read(
        "yandex-metrika/token",
        "YANDEX_METRIKA_TOKEN",
        hint="Create a Yandex Metrica OAuth token at oauth.yandex.ru.",
    )


def miratext_api_key() -> str:
    return read("miratext/api_key", "MIRATEXT_API_KEY")


def dataforseo_login() -> str:
    return read("dataforseo/login", "DATAFORSEO_LOGIN")


def dataforseo_password() -> str:
    return read("dataforseo/password", "DATAFORSEO_PASSWORD")


def dataforseo_ready() -> tuple[bool, dict[str, bool]]:
    """Return DataForSEO readiness and its per-component status, without exposing secret values.

    DataForSEO authenticates with HTTP Basic auth over login *and* password (see
    ``DataForSEOClient._auth``); either one alone cannot authenticate a request. This is the
    single readiness definition shared by provider wrappers and ``sources_doctor`` so the two
    never drift back out of sync with what the client actually requires.
    """
    components = {
        "login": available("dataforseo/login", "DATAFORSEO_LOGIN"),
        "password": available("dataforseo/password", "DATAFORSEO_PASSWORD"),
    }
    return all(components.values()), components


def gsc_access_token() -> str:
    """Search Console has no long-lived API key; this reads a short-lived OAuth2 bearer token.

    Generate one at https://developers.google.com/oauthplayground (authorize the
    ``webmasters.readonly`` scope) or with ``gcloud auth application-default print-access-token``
    after ``gcloud auth application-default login --scopes=...webmasters.readonly``. A token
    expires roughly hourly; this client does not refresh one, so callers re-supply it per session.
    """
    return read(
        "gsc/access_token",
        "GSC_ACCESS_TOKEN",
        hint="See docs/SETUP.md for how to obtain a Search Console OAuth token.",
    )


GOOGLE_TOKEN_URI = "https://oauth2.googleapis.com/token"
GSC_SERVICE_ACCOUNT_MAX_BYTES = 65536
_GSC_SERVICE_ACCOUNT_REQUIRED_FIELDS = ("client_email", "private_key", "token_uri")


def _gsc_service_account_candidate_path() -> Path:
    configured = os.environ.get("GSC_SERVICE_ACCOUNT_FILE")
    if configured:
        return Path(configured).expanduser()
    return CONFIG_ROOT / "gsc/service-account.json"


def _gsc_service_account_file_state(path: Path) -> str:
    """Coarse filesystem state of the candidate path; contents are never inspected here."""
    if path.is_symlink():
        return "unsafe_file"
    try:
        info = path.stat()
    except OSError:
        return "missing"
    if not path.is_file() or not is_private_mode(info.st_mode):
        return "unsafe_file"
    return "present"


def gsc_service_account_path() -> Path:
    """Return a restricted local service-account JSON path without reading or printing its key."""
    path = _gsc_service_account_candidate_path()
    state = _gsc_service_account_file_state(path)
    if state == "missing":
        raise MissingCredential("GSC service-account JSON file is not configured or readable")
    if state == "unsafe_file":
        raise MissingCredential(
            "GSC service-account JSON must be a private regular file (mode 0600)"
        )
    return path


def gsc_service_account_document() -> tuple[str, dict[str, Any] | None]:
    """Validate the configured service-account JSON and return ``(status, document)``.

    ``document`` is populated only for ``"configured_unverified"`` and is intended for the
    runtime auth path. Diagnostics report the status alone — ``"missing"``,
    ``"unsafe_file"``, ``"too_large"``, ``"unreadable"``, ``"malformed_json"``, or
    ``"unsupported_shape"`` — which never carries key material, the account email, or
    document fragments. A structurally valid document is configuration only; authenticated
    access is established separately by ``provider-verify``.
    """
    path = _gsc_service_account_candidate_path()
    state = _gsc_service_account_file_state(path)
    if state != "present":
        return state, None
    try:
        size = path.stat().st_size
    except OSError:
        return "missing", None
    if size > GSC_SERVICE_ACCOUNT_MAX_BYTES:
        return "too_large", None
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except OSError:
        return "unreadable", None
    except ValueError:
        return "malformed_json", None
    if not isinstance(document, dict):
        return "unsupported_shape", None
    if (
        document.get("type") != "service_account"
        or document.get("token_uri") != GOOGLE_TOKEN_URI
        or any(
            not isinstance(document.get(field), str) or not document[field]
            for field in _GSC_SERVICE_ACCOUNT_REQUIRED_FIELDS
        )
    ):
        return "unsupported_shape", None
    return "configured_unverified", document


def gsc_service_account_status() -> str:
    """Coarse credential state for diagnostics; never a verification claim."""
    status, _document = gsc_service_account_document()
    return status


def gsc_service_account_available() -> bool:
    return gsc_service_account_status() == "configured_unverified"


def ga4_access_token() -> str:
    """Read a read-only Google Analytics Data API bearer token."""
    return read(
        "ga4/access_token",
        "GA4_ACCESS_TOKEN",
        hint="Authorize only analytics.readonly for the selected GA4 property.",
    )


def yandex_webmaster_token() -> str:
    return read(
        "yandex-webmaster/access_token",
        "YANDEX_WEBMASTER_TOKEN",
        hint="Authorize a read-only Yandex Webmaster application before collecting evidence.",
    )


def bing_webmaster_key() -> str:
    return read(
        "bing-webmaster/api_key",
        "BING_WEBMASTER_API_KEY",
        hint="Create a Bing Webmaster API key for the verified site.",
    )


def crux_api_key() -> str:
    return read(
        "crux/api_key",
        "CRUX_API_KEY",
        hint="Create an API key in Google Cloud Console and enable the Chrome UX Report API.",
    )


def indexnow_key() -> str:
    """IndexNow needs a self-generated key, not a provider-issued secret.

    Any random string works (for example ``openssl rand -hex 16``); publish it unmodified at
    ``https://<your-domain>/<key>.txt`` before submitting, so a receiving search engine can
    verify the submitter controls the host.
    """
    return read(
        "indexnow/key",
        "INDEXNOW_KEY",
        hint="Generate one yourself and publish it at https://<host>/<key>.txt first.",
    )
