"""Bounded, user-local IPC for an existing Desktop window, never a core client."""

from __future__ import annotations

import json
import math
import os
import re
import secrets
import stat
import time
import uuid
from collections import OrderedDict
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

from PyQt5.QtCore import QCoreApplication, QObject, QThread, QTimer
from PyQt5.QtNetwork import QLocalServer, QLocalSocket

PROTOCOL = "seohead.desktop.control.v1"
MAX_REQUEST_BYTES = 64 * 1024
MAX_RESPONSE_BYTES = 1024 * 1024
TIMEOUT_MS = 5000
MAX_DESCRIPTOR_BYTES = 16 * 1024
_ACTIVE_ENDPOINTS = set()
READ_OPERATIONS = frozenset({"status", "tabs", "project_scans"})
_FIELDS = {
    "status": (set(), set()),
    "tabs": (set(), set()),
    "project_scans": ({"project_uuid"}, {"tab_id"}),
    "select_scan": ({"project_uuid", "scan_uuid"}, {"tab_id"}),
    "select_view": ({"view_id"}, {"tab_id"}),
    "select_tab": ({"tab_id"}, set()),
    "close_tab": ({"tab_id"}, set()),
    "new_tab": ({"project_uuid"}, {"scan_uuid", "view_id"}),
    "new_scan": ({"project_uuid", "config", "approved"}, {"tab_id"}),
    "stop_run": ({"run_id", "approved"}, set()),
    "resume_run": ({"run_id", "approved"}, set()),
}
_IDENTIFIER = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}\Z")


class ControlError(ValueError):
    """A safe, user-facing refusal from transport or the Desktop callback."""

    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


@dataclass(frozen=True)
class ControlEndpoint:
    """Private owner handle. Publish only descriptor_path, never serialize this."""

    descriptor_path: Path
    socket_name: str
    instance_id: str
    token: str = field(repr=False)
    file_identity: tuple[int, int] = field(repr=False)
    created_here: bool = field(default=False, repr=False)


def _checked_path(path, *, directory=False, private=False):
    path = Path(path)
    try:
        info = path.lstat()
        correct_type = (
            stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode)
        )
        if (
            not path.is_absolute()
            or not correct_type
            or path.resolve(strict=True) != path
        ):
            raise ValueError
        if os.name != "nt" and (
            info.st_uid != os.geteuid()
            or (private
            and stat.S_IMODE(info.st_mode) != (0o700 if directory else 0o600))
        ):
            raise ValueError
    except (OSError, ValueError):
        raise ControlError(
            "unsafe_endpoint",
            "Endpoint paths must be owned, real paths with private directory/file permissions",
        ) from None
    return path, info


def prepare_endpoint(runtime_directory) -> ControlEndpoint:
    """Create only a fresh private instance directory under an existing owned root.

    Does not change permissions of existing paths. Unix path traversal permissions
    and the token are required because macOS ignores QLocalServer socket options.
    """
    root, _ = _checked_path(runtime_directory, directory=True)
    instance_id = uuid.uuid4().hex
    private = root / ("d-" + instance_id[:16])
    socket_name = (
        "seohead-desktop-" + instance_id if os.name == "nt" else str(private / "ipc")
    )
    if os.name != "nt" and len(os.fsencode(socket_name)) > 100:
        raise ControlError(
            "endpoint_path_too_long",
            "Choose a shorter existing runtime directory for the local socket",
        )
    private.mkdir(mode=0o700)
    metadata = private / "control.json"
    try:
        _checked_path(private, directory=True, private=True)
        token = secrets.token_hex(32)
        payload = {
            "protocol": PROTOCOL,
            "instance_id": instance_id,
            "endpoint": socket_name,
            "token": token,
        }
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(metadata, flags, 0o600)
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(_encode(payload))
            stream.flush()
            os.fsync(stream.fileno())
        _, info = _checked_path(metadata, private=True)
        return ControlEndpoint(
            metadata, socket_name, instance_id, token, (info.st_dev, info.st_ino), True
        )
    except Exception:
        # This new directory contains only this failed attempt's metadata.
        if metadata.exists():
            metadata.unlink()
        private.rmdir()
        raise


def read_endpoint(descriptor_path) -> ControlEndpoint:
    """Read bounded metadata only after owner, type and permission verification."""
    path, info = _checked_path(descriptor_path, private=True)
    _checked_path(path.parent, directory=True, private=True)
    if not 0 < info.st_size <= MAX_DESCRIPTOR_BYTES:
        raise ControlError("unsafe_endpoint", "Endpoint descriptor size is invalid")
    try:
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        with os.fdopen(descriptor, "rb") as stream:
            opened = os.fstat(stream.fileno())
            if (opened.st_dev, opened.st_ino) != (info.st_dev, info.st_ino):
                raise ControlError(
                    "unsafe_endpoint", "Endpoint descriptor changed while opening"
                )
            raw = stream.read(MAX_DESCRIPTOR_BYTES + 1)
        if len(raw) > MAX_DESCRIPTOR_BYTES:
            raise ControlError(
                "unsafe_endpoint", "Endpoint descriptor exceeds its size limit"
            )
        payload = _decode(raw)
        _object(payload, {"protocol", "instance_id", "endpoint", "token"})
        instance = payload["instance_id"]
        token = payload["token"]
        if (
            payload["protocol"] != PROTOCOL
            or not isinstance(instance, str)
            or not re.fullmatch(r"[a-f0-9]{32}", instance)
            or not isinstance(token, str)
            or not re.fullmatch(r"[a-f0-9]{64}", token)
        ):
            raise ControlError(
                "unsafe_endpoint", "Invalid endpoint descriptor identity"
            )
        expected = (
            "seohead-desktop-" + instance
            if os.name == "nt"
            else str(path.parent / "ipc")
        )
        if payload["endpoint"] != expected:
            raise ControlError(
                "unsafe_endpoint",
                "Endpoint must belong to its private instance directory",
            )
        return ControlEndpoint(
            path, expected, instance, token, (info.st_dev, info.st_ino)
        )
    except OSError:
        raise ControlError(
            "unavailable", "Cannot read the Desktop endpoint descriptor"
        ) from None


def _identifier(value, label):
    if not isinstance(value, str) or not _IDENTIFIER.fullmatch(value):
        raise ControlError(
            "invalid_request", f"{label} must be a bounded identifier, not a path"
        )
    return value


def _object(value, required, optional=()):
    if (
        type(value) is not dict
        or not required <= value.keys()
        or value.keys() - (required | set(optional))
    ):
        raise ControlError("invalid_request", "Missing or unsupported object fields")


def _json_tree(value, depth=0):
    if depth > 8:
        raise ControlError("invalid_request", "JSON nesting exceeds the control limit")
    if value is None or type(value) is bool:
        return
    if type(value) in {int, float}:
        if abs(value) > 2**63 - 1 or (type(value) is float and not math.isfinite(value)):
            raise ControlError(
                "invalid_request", "JSON numbers must be finite and bounded"
            )
    elif type(value) is str:
        if len(value) > 4096 or "\x00" in value:
            raise ControlError("invalid_request", "JSON text exceeds the control limit")
    elif type(value) is list and len(value) <= 256:
        for item in value:
            _json_tree(item, depth + 1)
    elif type(value) is dict and len(value) <= 128:
        for key, item in value.items():
            if type(key) is not str or len(key) > 128:
                raise ControlError("invalid_request", "Invalid JSON object key")
            _json_tree(item, depth + 1)
    else:
        raise ControlError("invalid_request", "Unsupported or oversized JSON value")


def validate_arguments(operation: str, arguments: dict) -> dict:
    """Validate IPC shape; owner callback additionally admits IDs and core settings."""
    if not isinstance(operation, str) or operation not in _FIELDS:
        raise ControlError(
            "unsupported_operation", "This Desktop control operation is not allowed"
        )
    _object(arguments, *_FIELDS[operation])
    _json_tree(arguments)
    for key, value in arguments.items():
        if key.endswith(("_id", "_uuid")):
            _identifier(value, key)
    if "approved" in arguments and arguments["approved"] is not True:
        raise ControlError(
            "approval_required", "This run operation requires explicit approved=true"
        )
    if "config" in arguments:
        config = arguments["config"]
        _object(
            config,
            {"max_urls", "rendering_mode", "max_requests", "max_seconds"},
            {"approve_large_crawl", "configuration_overrides", "sitemap_url"},
        )
        for key, maximum in (
            ("max_urls", 1_000_000),
            ("max_requests", 2_000_000),
            ("max_seconds", 2**31 - 1),
        ):
            if type(config[key]) is not int or not 0 <= config[key] <= maximum:
                raise ControlError(
                    "invalid_request", f"config.{key} is outside Desktop bounds"
                )
        if not isinstance(config["rendering_mode"], str) or config[
            "rendering_mode"
        ] not in {"raw", "js"}:
            raise ControlError(
                "invalid_request", "config.rendering_mode must be raw or js"
            )
        if (
            "approve_large_crawl" in config
            and type(config["approve_large_crawl"]) is not bool
        ):
            raise ControlError(
                "invalid_request", "config.approve_large_crawl must be boolean"
            )
        if "configuration_overrides" in config:
            overrides = config["configuration_overrides"]
            if type(overrides) is not dict or any(
                not re.fullmatch(r"[a-z][a-z0-9_.]*", key) for key in overrides
            ):
                raise ControlError(
                    "invalid_request",
                    "configuration_overrides must contain dotted setting names",
                )
        if "sitemap_url" in config:
            from urllib.parse import urlsplit

            try:
                url = config["sitemap_url"]
                parsed = urlsplit(url) if isinstance(url, str) else None
                if (
                    not parsed
                    or parsed.scheme not in {"http", "https"}
                    or not parsed.hostname
                    or parsed.username
                    or parsed.password
                    or parsed.fragment
                    or any(c in url for c in "\r\n\x00")
                ):
                    raise ValueError
                _ = parsed.port  # Access validates malformed port syntax.
            except ValueError:
                raise ControlError(
                    "invalid_request",
                    "sitemap_url must be an absolute HTTP(S) URL without credentials or fragment",
                ) from None
    return arguments


def _pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate JSON key")
        result[key] = value
    return result


def _decode(raw):
    try:
        return json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=_pairs,
            parse_constant=lambda value: (_ for _ in ()).throw(ValueError(value)),
        )
    except (UnicodeError, ValueError, RecursionError):
        raise ControlError(
            "invalid_request", "Expected one UTF-8 JSON object"
        ) from None


def _encode(payload):
    return (
        json.dumps(
            payload, ensure_ascii=False, allow_nan=False, separators=(",", ":")
        ).encode("utf-8")
        + b"\n"
    )


def _error(request_id, code, message):
    return {
        "protocol": PROTOCOL,
        "id": request_id,
        "ok": False,
        "error": {"code": code, "message": str(message)[:512]},
    }


class DesktopControlServer(QObject):
    """One endpoint whose short callback always runs in the Qt application thread.

    Callback receives ``(operation, arguments)`` and returns a JSON object. It must
    resolve IDs exclusively against opened projects and the shared scan manager,
    validate config through the current core descriptor, and return immediately.
    It must not open a modal dialog or wait for a scan/core operation to complete.
    """

    def __init__(
        self,
        endpoint: ControlEndpoint,
        dispatch: Callable[[str, dict], dict],
        *,
        parent=None,
        timeout_ms=TIMEOUT_MS,
        max_clients=8,
    ):
        super().__init__(parent)
        if not isinstance(endpoint, ControlEndpoint) or not endpoint.created_here:
            raise ValueError("Server requires a freshly prepared owner endpoint")
        self.endpoint = endpoint
        if (
            not callable(dispatch)
            or type(timeout_ms) is not int
            or not 1 <= timeout_ms <= TIMEOUT_MS
            or type(max_clients) is not int
            or not 1 <= max_clients <= 32
        ):
            raise ValueError("Invalid local-control callback or bounds")
        self.dispatch, self.timeout_ms, self.max_clients = (
            dispatch,
            timeout_ms,
            max_clients,
        )
        self.server = QLocalServer(self)
        # Unix access is enforced by the private directory and authenticated
        # descriptor. Qt permission flags bind through an extra temporary path,
        # which can overflow sockaddr_un even when the final path fits. Keep the
        # native user-only named-pipe ACL on Windows.
        self.server.setSocketOptions(
            QLocalServer.UserAccessOption
            if os.name == "nt"
            else QLocalServer.SocketOptions(0)
        )
        self.server.setMaxPendingConnections(max_clients)
        self.server.newConnection.connect(self._accept)
        self._clients = {}
        self._owns_endpoint = False
        # Bounded retry receipts, not a durable run store. Never automatically retry.
        self._receipts = OrderedDict()

    def start(self):
        application = QCoreApplication.instance()
        if (
            application is None
            or self.thread() != application.thread()
            or QThread.currentThread() != self.thread()
        ):
            raise RuntimeError(
                "Desktop control must start in the Qt application thread"
            )
        if self.server.isListening():
            return
        current = read_endpoint(self.endpoint.descriptor_path)
        if (
            current.instance_id != self.endpoint.instance_id
            or not secrets.compare_digest(current.token, self.endpoint.token)
        ):
            raise ControlError("unsafe_endpoint", "Prepared endpoint identity changed")
        if self.endpoint.socket_name in _ACTIVE_ENDPOINTS or (
            os.name != "nt" and os.path.lexists(self.endpoint.socket_name)
        ):
            raise ControlError(
                "endpoint_unavailable",
                "Endpoint already has an owner or an existing path; choose a new explicit instance location",
            )
        if not self.server.listen(self.endpoint.socket_name):
            reason = " ".join(self.server.errorString().replace("\x00", "").split())[
                :160
            ]
            raise ControlError(
                "endpoint_unavailable",
                f"Endpoint cannot listen (Qt {int(self.server.serverError())}: {reason or 'no reason supplied'}; "
                f"socket path {len(os.fsencode(self.endpoint.socket_name))} bytes)",
            )
        _ACTIVE_ENDPOINTS.add(self.endpoint.socket_name)
        self._owns_endpoint = True

    def close(self):
        if self.server.isListening():
            self.server.close()
        for socket in tuple(self._clients):
            socket.abort()
            self._discard(socket)
        self._receipts.clear()
        if self._owns_endpoint:
            _ACTIVE_ENDPOINTS.discard(self.endpoint.socket_name)
            self._owns_endpoint = False
            path = self.endpoint.descriptor_path
            try:
                info = path.lstat()
                if (
                    stat.S_ISREG(info.st_mode)
                    and (info.st_dev, info.st_ino) == self.endpoint.file_identity
                ):
                    path.unlink()
                # A directory with unrelated files is deliberately preserved.
                path.parent.rmdir()
            except OSError:
                pass

    def _accept(self):
        while self.server.hasPendingConnections():
            socket = self.server.nextPendingConnection()
            if len(self._clients) >= self.max_clients:
                socket.disconnected.connect(socket.deleteLater)
                socket.write(
                    _encode(
                        self._with_identity(
                            _error(
                                None, "busy", "Desktop control connection limit reached"
                            )
                        )
                    )
                )
                socket.disconnectFromServer()
                continue
            socket.setReadBufferSize(MAX_REQUEST_BYTES + 1)
            timer = QTimer(socket)
            timer.setSingleShot(True)
            self._clients[socket] = {
                "data": bytearray(),
                "timer": timer,
                "answered": False,
            }
            socket.readyRead.connect(lambda socket=socket: self._read(socket))
            socket.disconnected.connect(lambda socket=socket: self._discard(socket))
            timer.timeout.connect(
                lambda socket=socket: self._answer(
                    socket,
                    _error(None, "timeout", "Incomplete request deadline expired"),
                )
            )
            timer.start(self.timeout_ms)
            self._read(socket)

    def _discard(self, socket):
        client = self._clients.pop(socket, None)
        if client:
            client["timer"].stop()
            socket.deleteLater()

    def _read(self, socket):
        client = self._clients.get(socket)
        if not client or client["answered"]:
            return
        client["data"].extend(bytes(socket.readAll()))
        raw = bytes(client["data"])
        if len(raw) > MAX_REQUEST_BYTES:
            self._answer(
                socket,
                _error(None, "request_too_large", "Control request exceeds 64 KiB"),
            )
        elif b"\n" in raw:
            frame, tail = raw.split(b"\n", 1)
            if tail:
                self._answer(
                    socket,
                    _error(
                        None,
                        "invalid_request",
                        "Only one request is allowed per connection",
                    ),
                )
                return
            request_id = None
            try:
                request = _decode(frame)
                _object(
                    request,
                    {
                        "protocol",
                        "instance_id",
                        "token",
                        "id",
                        "operation",
                        "arguments",
                    },
                )
                request_id = _identifier(request["id"], "id")
                if request["protocol"] != PROTOCOL:
                    raise ControlError(
                        "unsupported_protocol", "Unsupported Desktop control protocol"
                    )
                if (
                    request["instance_id"] != self.endpoint.instance_id
                    or not isinstance(request["token"], str)
                    or not re.fullmatch(r"[a-f0-9]{64}", request["token"])
                    or not secrets.compare_digest(request["token"], self.endpoint.token)
                ):
                    raise ControlError(
                        "unauthorized", "Desktop endpoint authentication failed"
                    )
                operation = request["operation"]
                if not isinstance(operation, str):
                    raise ControlError("invalid_request", "operation must be text")
                arguments = validate_arguments(operation, request["arguments"])
                canonical = _encode(
                    {key: value for key, value in request.items() if key != "token"}
                )
                prior = self._receipts.get(request_id)
                if prior is not None:
                    if prior[0] != canonical:
                        raise ControlError(
                            "request_id_conflict",
                            "Request ID was already used for a different operation",
                        )
                    self._answer(socket, prior[1])
                    return
                try:
                    result = self.dispatch(operation, arguments)
                    if type(result) is not dict:
                        raise TypeError("Callback must return an object")
                    response = {
                        "protocol": PROTOCOL,
                        "id": request_id,
                        "ok": True,
                        "result": result,
                    }
                    encoded = _encode(self._with_identity(response))
                    if len(encoded) > MAX_RESPONSE_BYTES:
                        response = _error(
                            request_id,
                            "response_too_large",
                            "Desktop response exceeds 1 MiB; operation may already have been admitted",
                        )
                except ControlError as exc:
                    response = _error(request_id, exc.code, str(exc))
                except ValueError as exc:
                    response = _error(request_id, "rejected", str(exc))
                except Exception:
                    response = _error(
                        request_id,
                        "callback_failed",
                        "Desktop could not complete the control request",
                    )
                if operation not in READ_OPERATIONS:
                    self._receipts[request_id] = (canonical, response)
                    if len(self._receipts) > 16:
                        self._receipts.popitem(last=False)
                self._answer(socket, response)
            except (ControlError, TypeError, OverflowError) as exc:
                self._answer(
                    socket,
                    _error(
                        request_id, getattr(exc, "code", "invalid_request"), str(exc)
                    ),
                )

    def _answer(self, socket, response):
        client = self._clients.get(socket)
        if not client or client["answered"]:
            return
        client["answered"] = True
        client["timer"].stop()
        socket.write(_encode(self._with_identity(response)))
        socket.disconnectFromServer()
        # A peer that refuses to read must not retain an answered socket forever.
        client["timer"].timeout.disconnect()
        client["timer"].timeout.connect(socket.abort)
        client["timer"].start(self.timeout_ms)

    def _with_identity(self, response):
        return {**response, "instance_id": self.endpoint.instance_id}


def request(
    endpoint,
    operation: str,
    arguments: dict | None = None,
    *,
    timeout_ms=TIMEOUT_MS,
    request_id: str | None = None,
) -> dict:
    """One blocking local-socket request; safe to run in a client worker thread."""
    identity = read_endpoint(endpoint)
    if type(timeout_ms) is not int or not 1 <= timeout_ms <= TIMEOUT_MS:
        raise ValueError("Control timeout must be from 1 to 5000 ms")
    arguments = validate_arguments(operation, {} if arguments is None else arguments)
    request_id = _identifier(request_id or uuid.uuid4().hex, "id")
    payload = _encode(
        {
            "protocol": PROTOCOL,
            "instance_id": identity.instance_id,
            "token": identity.token,
            "id": request_id,
            "operation": operation,
            "arguments": arguments,
        }
    )
    if len(payload) > MAX_REQUEST_BYTES:
        raise ControlError("request_too_large", "Control request exceeds 64 KiB")
    deadline = time.monotonic() + timeout_ms / 1000
    remaining = lambda: max(0, int((deadline - time.monotonic()) * 1000))
    socket = QLocalSocket()
    socket.setReadBufferSize(MAX_RESPONSE_BYTES + 1)
    try:
        socket.connectToServer(identity.socket_name)
        if not socket.waitForConnected(remaining()):
            raise ControlError("unavailable", "Desktop endpoint is unavailable")
        if socket.write(payload) != len(payload):
            raise ControlError("unavailable", "Could not send the Desktop request")
        if socket.bytesToWrite() and not socket.waitForBytesWritten(remaining()):
            raise ControlError(
                "timeout",
                "Request write deadline expired; inspect status before retrying",
            )
        data = bytearray()
        while True:
            data.extend(bytes(socket.readAll()))
            if len(data) > MAX_RESPONSE_BYTES:
                raise ControlError(
                    "response_too_large", "Desktop response exceeds 1 MiB"
                )
            if b"\n" in data:
                frame, tail = bytes(data).split(b"\n", 1)
                if tail:
                    raise ControlError(
                        "invalid_response", "Expected one Desktop response"
                    )
                response = _decode(frame)
                if (
                    type(response) is not dict
                    or response.get("protocol") != PROTOCOL
                    or response.get("instance_id") != identity.instance_id
                    or (response.get("id") != request_id
                    and response.get("id") is not None)
                    or type(response.get("ok")) is not bool
                ):
                    raise ControlError(
                        "invalid_response", "Desktop response identity is invalid"
                    )
                if response["ok"] is not True:
                    error = response.get("error") or {}
                    if (
                        type(error) is not dict
                        or not isinstance(error.get("code"), str)
                        or not isinstance(error.get("message"), str)
                    ):
                        raise ControlError(
                            "invalid_response", "Desktop error response is invalid"
                        )
                    raise ControlError(
                        str(error.get("code", "rejected")),
                        str(error.get("message", "Desktop rejected the request")),
                    )
                if (
                    response["id"] != request_id
                    or type(response.get("result")) is not dict
                ):
                    raise ControlError(
                        "invalid_response", "Desktop response result is invalid"
                    )
                return response["result"]
            if not remaining() or not socket.waitForReadyRead(remaining()):
                raise ControlError(
                    "timeout",
                    "Desktop reply unavailable; operation outcome may be unknown. Inspect status before retrying",
                )
    finally:
        socket.abort()
