"""Restricted local storage and explicit refresh for read-only OAuth grants."""

from __future__ import annotations

import json
import os
import tempfile
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from seohead.data_sources import oauth_flow
from seohead.data_sources.credentials import CONFIG_ROOT, MissingCredential, is_private_mode
from seohead.data_sources.http import open_no_redirect

TOKEN_HOST = "https://oauth2.googleapis.com/token"
AUTH_HOST = "https://accounts.google.com/o/oauth2/v2/auth"
RefreshTransport = Callable[[dict[str, str]], dict[str, Any]]


def _path(provider: str) -> Path:
    if provider != "gsc":
        raise ValueError("unsupported OAuth provider")
    return CONFIG_ROOT / provider / "oauth.json"


def _flow_path(provider: str) -> Path:
    return _path(provider).with_name("oauth-flow.json")


def save_grant(provider: str, grant: dict[str, Any]) -> None:
    """Explicitly persist a read-only grant locally with restrictive permissions."""
    required = {"refresh_token", "client_id", "client_secret", "scopes"}
    if (
        not isinstance(grant, dict)
        or set(grant) != required
        or not all(isinstance(grant[key], str) and grant[key] for key in required - {"scopes"})
    ):
        raise ValueError("OAuth grant has an unsupported shape")
    if not isinstance(grant["scopes"], list) or grant["scopes"] != [
        "https://www.googleapis.com/auth/webmasters.readonly"
    ]:
        raise ValueError("GSC grants must have only the webmasters.readonly scope")
    path = _path(provider)
    if path.parent.is_symlink() or path.is_symlink():
        raise ValueError("OAuth storage must not use symlinks")
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    if path.exists():
        raise ValueError("OAuth grant already exists; revoke or disconnect it before reconnecting")
    descriptor, staged = tempfile.mkstemp(prefix=".oauth-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            json.dump(grant, stream, sort_keys=True)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(staged, 0o600)
        # Publish without replacing a grant created by another writer.
        os.link(staged, path)
    finally:
        Path(staged).unlink(missing_ok=True)


def _grant(provider: str) -> dict[str, Any]:
    path = _path(provider)
    if (
        not path.is_file()
        or path.is_symlink()
        or not is_private_mode(path.stat().st_mode)
        or path.stat().st_size > 65536
    ):
        raise MissingCredential("durable OAuth grant is not configured")
    try:
        value = json.loads(path.read_text())
    except (OSError, ValueError) as exc:
        raise MissingCredential("durable OAuth grant is unreadable") from exc
    if (
        not isinstance(value, dict)
        or set(value) != {"refresh_token", "client_id", "client_secret", "scopes"}
        or any(
            not isinstance(value.get(key), str) or not value[key]
            for key in ("refresh_token", "client_id", "client_secret")
        )
        or value.get("scopes") != ["https://www.googleapis.com/auth/webmasters.readonly"]
    ):
        raise MissingCredential("durable OAuth grant is invalid")
    return value


def _default_refresh(payload: dict[str, str]) -> dict[str, Any]:
    request = urllib.request.Request(
        TOKEN_HOST,
        data=urllib.parse.urlencode(payload).encode(),
        method="POST",
        headers={"Content-Type": "application/x-www-form-urlencoded"},
    )
    with open_no_redirect(request, timeout=30) as response:
        return json.loads(response.read().decode("utf-8"))


def _default_exchange(payload: dict[str, str]) -> dict[str, Any]:
    request = urllib.request.Request(
        TOKEN_HOST,
        data=urllib.parse.urlencode(payload).encode(),
        method="POST",
        headers={"Content-Type": "application/x-www-form-urlencoded"},
    )
    with open_no_redirect(request, timeout=30) as response:
        return json.loads(response.read().decode("utf-8"))


def _mark_invalid_grant(provider: str) -> None:
    oauth_flow.mark_invalid(_flow_path(provider))


def refresh_access_token(
    provider: str, *, transport: RefreshTransport | None = None
) -> dict[str, Any]:
    """Exchange a stored refresh token only when an explicit live operation needs it."""
    grant = _grant(provider)
    payload = {
        "grant_type": "refresh_token",
        "refresh_token": grant["refresh_token"],
        "client_id": grant["client_id"],
        "client_secret": grant["client_secret"],
    }
    try:
        body = (transport or _default_refresh)(payload)
    except urllib.error.HTTPError as exc:
        try:
            response = json.loads(exc.read(8193).decode("utf-8", "replace"))
        except (OSError, ValueError):
            response = {}
        if isinstance(response, dict) and response.get("error") == "invalid_grant":
            _mark_invalid_grant(provider)
            raise MissingCredential("invalid_grant") from None
        raise MissingCredential("OAuth refresh failed; provider rejected the grant") from None
    token = body.get("access_token") if isinstance(body, dict) else None
    if not isinstance(token, str) or not token:
        if isinstance(body, dict) and body.get("error") == "invalid_grant":
            _mark_invalid_grant(provider)
            raise MissingCredential("invalid_grant")
        raise MissingCredential("OAuth refresh did not return an access token")
    return {"access_token": token, "scopes": grant["scopes"], "expires_in": body.get("expires_in")}


def grant_available(provider: str = "gsc") -> bool:
    try:
        _grant(provider)
    except (MissingCredential, OSError, ValueError):
        return False
    return True


def manage_grant(
    provider: str,
    action: str = "status",
    grant_file: str | None = None,
    confirm: bool = False,
    *,
    state: str | None = None,
    code: str | None = None,
    exchange_transport: Callable[[dict[str, str]], dict[str, Any]] | None = None,
) -> dict[str, Any]:
    """Manage a local read-only grant without returning OAuth material.

    Connect imports a grant obtained through the provider's consent flow; it
    does not bypass consent or claim property access. Revoke is a separate,
    confirmed Google write; disconnect only removes the local stored grant.
    """
    path = _path(provider)
    if action == "status":
        result: dict[str, Any] = {
            "ok": True,
            "configured": grant_available(provider),
            "access_verified": False,
        }
        flow = oauth_flow.load_flow(_flow_path(provider))
        if flow is not None:
            result["flow"] = {
                "flow_id": flow["flow_id"],
                "status": oauth_flow.effective_status(flow, datetime.now(timezone.utc)),
                "expires_at": flow["expires_at"],
                "connected_at": flow["connected_at"],
                "token_expires_at": flow["token_expires_at"],
            }
        return result
    if action == "start":
        client_id = os.environ.get("GSC_OAUTH_CLIENT_ID", "").strip()
        redirect_uri = os.environ.get("GSC_OAUTH_REDIRECT_URI", "").strip()
        parsed = urllib.parse.urlsplit(redirect_uri)
        if (
            not client_id
            or parsed.scheme not in {"https", "http"}
            or not parsed.netloc
            or parsed.username is not None
            or parsed.password is not None
        ):
            raise ValueError("GSC OAuth client ID and redirect URI must be configured")
        if parsed.scheme == "http" and parsed.hostname not in {
            "localhost",
            "127.0.0.1",
            "::1",
        }:
            raise ValueError("OAuth redirect URI must use HTTPS outside loopback")
        now = datetime.now(timezone.utc)
        flow_path = _flow_path(provider)
        with oauth_flow.flow_lock(flow_path):
            if grant_available(provider):
                raise ValueError(
                    "a durable OAuth grant already exists; disconnect it before reconnecting"
                )
            flow, raw_state = oauth_flow._create_flow_unlocked(flow_path, now)
        query = urllib.parse.urlencode(
            {
                "client_id": client_id,
                "redirect_uri": redirect_uri,
                "response_type": "code",
                "scope": "https://www.googleapis.com/auth/webmasters.readonly",
                "access_type": "offline",
                "prompt": "consent",
                "state": raw_state,
            }
        )
        return {
            "ok": True,
            "flow": {
                "flow_id": flow["flow_id"],
                "status": "waiting",
                "expires_at": flow["expires_at"],
            },
            "authorization_url": f"{AUTH_HOST}?{query}",
        }
    if action == "cancel":
        # Local and reversible: only the pending flow record changes, never a grant.
        flow = oauth_flow.cancel_flow(_flow_path(provider), datetime.now(timezone.utc))
        if flow is None:
            return {"ok": False, "error": "no_active_flow"}
        if flow["status"] in {"connected", "revoked", "invalid"}:
            return {"ok": False, "error": "flow_not_waiting"}
        return {"ok": True, "flow": {"flow_id": flow["flow_id"], "status": flow["status"]}}
    if action == "connect":
        if not isinstance(grant_file, str):
            raise ValueError("connect requires a private grant_file")
        source = Path(grant_file).expanduser()
        if (
            source.is_symlink()
            or not source.is_file()
            or not is_private_mode(source.stat().st_mode)
            or source.stat().st_size > 65536
        ):
            raise ValueError("grant_file must be a private bounded regular JSON file")
        try:
            grant = json.loads(source.read_text(encoding="utf-8"))
        except (OSError, ValueError) as exc:
            raise ValueError("grant_file is unreadable or invalid JSON") from exc
        save_grant(provider, grant)
        return {"ok": True, "configured": True, "access_verified": False}
    if action == "callback":
        if (
            not isinstance(state, str)
            or not isinstance(code, str)
            or not code
            or len(code) > 4096
            or any(ord(char) < 32 for char in code)
        ):
            raise ValueError("OAuth callback requires state and authorization code")
        now = datetime.now(timezone.utc)
        flow = oauth_flow.verify_callback_state(_flow_path(provider), state, now)
        client_id = os.environ.get("GSC_OAUTH_CLIENT_ID", "").strip()
        client_secret = os.environ.get("GSC_OAUTH_CLIENT_SECRET", "")
        redirect_uri = os.environ.get("GSC_OAUTH_REDIRECT_URI", "").strip()
        if not client_id or not client_secret or not redirect_uri:
            raise ValueError("GSC OAuth client credentials and redirect URI must be configured")
        try:
            token_response = (exchange_transport or _default_exchange)(
                {
                    "grant_type": "authorization_code",
                    "code": code,
                    "client_id": client_id,
                    "client_secret": client_secret,
                    "redirect_uri": redirect_uri,
                }
            )
        except Exception as exc:
            raise ValueError("Google OAuth code exchange failed; flow remains unconnected") from exc
        if not isinstance(token_response, dict):
            raise ValueError("Google OAuth returned an invalid token response")
        expected_scope = "https://www.googleapis.com/auth/webmasters.readonly"
        scopes = str(token_response.get("scope", "")).split()
        refresh_token = token_response.get("refresh_token")
        access_token = token_response.get("access_token")
        expires_in = token_response.get("expires_in")
        if (
            not isinstance(refresh_token, str)
            or not refresh_token
            or not isinstance(access_token, str)
            or not access_token
            or scopes != [expected_scope]
            or type(expires_in) is not int
            or not 1 <= expires_in <= 86400
        ):
            raise ValueError("Google OAuth response did not grant the required read-only scope")
        connected_at = datetime.now(timezone.utc)
        flow_path = _flow_path(provider)
        with oauth_flow.flow_lock(flow_path):
            oauth_flow.verify_callback_state(flow_path, state, connected_at)
            if grant_available(provider):
                raise ValueError(
                    "a durable OAuth grant appeared during consent; disconnect it first"
                )
            save_grant(
                provider,
                {
                    "refresh_token": refresh_token,
                    "client_id": client_id,
                    "client_secret": client_secret,
                    "scopes": [expected_scope],
                },
            )
            completed = oauth_flow.complete_flow(flow_path, flow, connected_at, expires_in)
        return {
            "ok": True,
            "configured": True,
            "access_verified": False,
            "flow": {
                "flow_id": completed["flow_id"],
                "status": completed["status"],
                "connected_at": completed["connected_at"],
                "token_expires_at": completed["token_expires_at"],
            },
        }
    if action == "refresh":
        try:
            refreshed = refresh_access_token(provider)
        except MissingCredential as exc:
            if "invalid_grant" in str(exc).lower():
                return {
                    "ok": False,
                    "refreshed": False,
                    "state": "invalid_grant",
                    "error": "stored OAuth grant was rejected; reconnect through consent",
                }
            return {
                "ok": False,
                "refreshed": False,
                "state": "verification_failed",
                "error": "stored OAuth grant could not be refreshed",
            }
        return {
            "ok": True,
            "refreshed": True,
            "scopes": refreshed["scopes"],
            "expires_in": refreshed["expires_in"],
            "property_access_verified": False,
        }
    if action in {"disconnect", "revoke"}:
        if confirm is not True:
            raise ValueError("disconnect/revoke requires confirm=true")
        grant = _grant(provider)
        if action == "revoke":
            request = urllib.request.Request(
                "https://oauth2.googleapis.com/revoke",
                data=urllib.parse.urlencode({"token": grant["refresh_token"]}).encode(),
                method="POST",
                headers={"Content-Type": "application/x-www-form-urlencoded"},
            )
            try:
                with open_no_redirect(request, timeout=30) as response:
                    if response.status != 200:
                        raise ValueError("Google did not confirm revocation")
            except OSError as exc:
                raise ValueError("Google revocation failed; local grant preserved") from exc
        path.unlink()
        oauth_flow.mark_revoked(_flow_path(provider))
        return {
            "ok": True,
            "configured": False,
            "remote_revoked": action == "revoke",
            "scope": "stored OAuth grant only; environment bearers and service accounts are unchanged",
        }
    raise ValueError(
        "action must be status, start, callback, connect, refresh, cancel, disconnect, or revoke"
    )
