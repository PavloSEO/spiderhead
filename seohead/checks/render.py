"""Compare raw server HTML with the rendered DOM seen after JavaScript executes.

A search crawler receives the server response, while a browser user may see a
different document after client-side scripts run. This check measures that gap.
Google can render JavaScript, but rendering is deferred and not guaranteed;
Yandex has more limited rendering; many AI crawlers do not render at all. A page
looking complete in a browser therefore does not prove that its source response
contains indexable content and links.

Performance values are laboratory measurements only: LCP, CLS, and timing data
from one run on one machine. They are not field Core Web Vitals from the Chrome
UX Report and are explicitly returned under ``metrics_lab``.

``render_document`` is the engine behind selective rendering escalation across
a whole crawl (#18, ``seohead.crawl.render_escalation``): unlike
``render_check``'s single fixed desktop/mobile comparison, it honours every
setting that changes what the rendered DOM contains -- script timeout,
viewport, resize-to-content, shadow-DOM and iframe flattening, device pixel
ratio, mobile/touch emulation, page-load strategy -- because those settings
are exactly what makes two render runs on the same site not comparable
unless both are recorded (see ``seohead.crawl.settings`` for where that
recording happens).
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import re
import tempfile
from collections.abc import Callable
from pathlib import Path
from typing import Any
from urllib.parse import parse_qsl, urlencode, urljoin, urlparse, urlunsplit

from bs4 import BeautifulSoup

from seohead.checks import dualcrawl
from seohead.checks.browser_transport import (
    BrowserTransportError,
    open_browser,
    open_context,
    prepare,
)
from seohead.checks.navigation import NavigationCapture
from seohead.recon.net import UA, client_network_policy, http_client, normalize_url, validate_url
from seohead.recon.remote_policy import RemoteTargetError, current_remote_policy

# Two fixed profiles rather than a free-form width/height: a responsive page
# renders a different DOM at different widths, so comparing two runs requires
# a short, named list both can point at. seohead.crawl.settings' rendering
# config reuses this exact mapping.
VIEWPORT_PRESETS: dict[str, dict[str, int]] = {
    "desktop": {"width": 1366, "height": 768},
    "mobile": {"width": 390, "height": 844},
}

# Headless engines the pinned renderer can launch, keyed by the Playwright
# browser name a crawl config may select (seohead.crawl.settings.RENDER_ENGINES
# validates against the same list -- keep them in step). Each entry carries
# the launch options the guarded path may pass and whether Playwright can
# emulate a mobile viewport (``is_mobile``) with that engine -- it documents
# the option as unsupported on Firefox, so asking for it there is a
# capability error, not a silently different render. ``has_touch`` is a
# general context option every engine accepts, so touch emulation alone is
# never gated by this map.
BROWSER_ENGINES: dict[str, dict[str, Any]] = {
    "chromium": {"launch_options": {"chromium_sandbox": True}, "mobile_emulation": True},
    "firefox": {"launch_options": {}, "mobile_emulation": False},
    "webkit": {"launch_options": {}, "mobile_emulation": True},
}


def _engine_capability_error(engine: str, browser_cfg: dict[str, Any]) -> str:
    """Name why ``engine`` cannot serve ``browser_cfg``, or ``""`` when it can.

    Unknown names and unsupported emulation combinations are distinct errors
    from a missing Playwright or a missing browser binary: each tells the
    operator a different corrective action, and none silently substitutes
    another engine.
    """
    if engine not in BROWSER_ENGINES:
        supported = ", ".join(sorted(BROWSER_ENGINES))
        return f"unsupported rendering engine {engine!r}; supported engines: {supported}"
    if not BROWSER_ENGINES[engine]["mobile_emulation"] and browser_cfg.get("mobile_emulation"):
        return (
            f"rendering engine {engine!r} cannot emulate a mobile viewport "
            "(Playwright does not implement is_mobile for it); unset "
            "mobile_emulation or pick another engine"
        )
    return ""


def _missing_browser_binary(exc: BaseException) -> bool:
    """Whether ``exc`` is Playwright's "browser executable not installed" error."""
    message = str(exc).lower()
    return "executable doesn't exist" in message or "browser is not installed" in message


def resolve_viewport(browser_cfg: dict[str, Any]) -> dict[str, int]:
    """Effective pixel dimensions a render context must be created with.

    The named preset supplies dimensions unless ``viewport_width`` and
    ``viewport_height`` carry a complete custom pair; the preset keeps deciding
    the profile either way, so old configs behave exactly as before. A
    malformed pair raises instead of guessing -- crawl settings validation
    rejects the same shapes before a run reaches here.
    """
    from seohead.crawl.settings import MAX_VIEWPORT_DIMENSION_PX

    preset = VIEWPORT_PRESETS.get(
        str(browser_cfg.get("viewport", "desktop")), VIEWPORT_PRESETS["desktop"]
    )
    width = browser_cfg.get("viewport_width")
    height = browser_cfg.get("viewport_height")
    if width in (None, 0) and height in (None, 0):
        return dict(preset)
    if (
        type(width) is not int
        or type(height) is not int
        or width < 1
        or height < 1
        or width > MAX_VIEWPORT_DIMENSION_PX
        or height > MAX_VIEWPORT_DIMENSION_PX
    ):
        raise ValueError(
            "viewport_width and viewport_height must be a complete pair of "
            f"integers between 1 and {MAX_VIEWPORT_DIMENSION_PX}"
        )
    return {"width": width, "height": height}


# A stable diagnostic representation, not a crawler identity or fingerprint-evasion
# profile. ``render_check(..., viewport="mobile")`` compares what a typical
# smartphone request receives; callers who need a site's exact variant can supply
# one explicit user_agent, which both raw and browser requests then share.
MOBILE_USER_AGENT = (
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) "
    "AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 "
    "Mobile/15E148 Safari/604.1"
)

# Common single-page application shells. An empty mount container means the raw
# response exposes no application content to a crawler that does not render.
_SHELL_IDS = ("root", "app", "__next", "__nuxt", "q-app", "main-app")

# Below this word threshold, the raw response is effectively empty without
# JavaScript and warrants a dedicated finding.
EMPTY_BODY_WORDS = 50

# The sole all-clear message also determines ``js_dependent``. Keeping it in one
# constant prevents the summary and findings from drifting apart.
ALL_CLEAR = (
    "Raw HTML and rendered DOM are materially equivalent; JavaScript "
    "rendering does not determine SEO-visible content"
)

# A render that did not finish returns a document with none of the landmarks the
# raw response carries, at a small fraction of its size. Compared naively, that
# emptiness reads as the site deleting its own title and canonical (#623). The
# ratio is deliberately generous: a page whose rendered DOM is at least half the
# raw response's size was captured, whatever else it did.
INCOMPLETE_RENDER_BYTE_RATIO = 0.5

# Named reason and machine-readable code for that state. It is neither a clean
# result nor a finding: it says this run did not measure the page.
INCOMPLETE_RENDER_CODE = "incomplete_render"
RENDER_UNAVAILABLE = (
    "The rendered DOM was not captured, so the raw-versus-rendered comparison "
    "is unavailable: {reason}. Nothing about this page's JavaScript dependence "
    "follows from this run -- re-run it, if needed with --wait domcontentloaded "
    "or a longer --timeout"
)

# A fallback capture read the DOM at an earlier milestone than the one that was
# requested (see ``_capture_dom``). ``wait_reached`` records that on the result,
# but ``seohead.audit.site`` carries findings *text* into a report and nothing
# else -- so unless the findings list says the milestone was missed, a page
# whose scripts had not run yet arrives in the report as an affirmative "raw and
# rendered are equivalent", graded a notice (#642).
MILESTONE_MISSED = (
    "The requested {wait} milestone was never reached; the DOM was read at "
    "{reached} instead, so scripts may not have finished running before the "
    "snapshot was taken. What follows describes this run, not the page -- "
    "re-run it with a longer --timeout, or with --wait {reached} to request "
    "that milestone deliberately"
)

_SCRIPT_STYLE_RE = re.compile(
    r"<(script|style|noscript)\b[^>]*>.*?</\1>", re.IGNORECASE | re.DOTALL
)
_TAG_RE = re.compile(r"<[^>]+>")
_BROWSER_RESPONSE_BYTES = 5 * 1024 * 1024
_BROWSER_WEIGHT_RESPONSES = 2_000
_RENDER_CANCELLED = "browser rendering cancelled"


class RenderCancelled(RuntimeError):
    """A caller stopped a render at a pinned request boundary."""


MAX_CONSOLE_ERRORS = 100
MAX_CONSOLE_ERROR_CHARS = 1_000
_BROWSER_METHODS = frozenset({"GET", "HEAD", "OPTIONS", "POST"})
_BLOCKED_WEBSOCKET_LIMITATION = "browser WebSocket requests are unsupported by pinned rendering"
_HOP_BY_HOP_HEADERS = frozenset(
    {
        "connection",
        "content-length",
        "host",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    }
)

# ``route.fulfill`` writes the body it is handed straight into the renderer: it
# never applies a declared ``Content-Encoding``. Forwarding the origin's
# compression header alongside a body this route has already decoded would hand
# Chromium gzip bytes labelled ``text/html`` and produce a DOM made of the
# compressed stream (#650). The header describes a transfer coding that ends
# here, so it is dropped exactly like the hop-by-hop set above; ``content-length``
# is already in that set, which leaves Playwright to state the decoded length.
_TRANSFER_CODING_HEADERS = frozenset({"content-encoding"})

# Fallback advertised when the client cannot state its own decodable set. httpx
# decodes these two without any optional dependency.
_BASELINE_ENCODINGS = "gzip, deflate"


def _decodable_encodings(client: Any) -> str:
    """Return the content codings this HTTP client can actually decode.

    httpx builds its own ``Accept-Encoding`` from the decoders it has -- which
    grows to ``br`` and ``zstd`` when brotli/zstandard are installed and shrinks
    when they are not. Reading it back is what keeps the coding this route asks
    for and the coding it can decode from ever disagreeing.
    """
    headers = getattr(client, "headers", None)
    getter = getattr(headers, "get", None)
    advertised = getter("accept-encoding") if getter is not None else None
    return str(advertised) if advertised else _BASELINE_ENCODINGS


def _undecoded_coding(content_encoding: str, decodable: str) -> str:
    """Name a coding the origin applied that this client did not decode.

    httpx passes an unrecognised coding through untouched rather than failing,
    so a non-compliant origin can still answer in a coding nobody asked for.
    Naming it aborts the request instead of rendering the compressed stream.
    """
    supported = {item.strip().lower().split(";")[0] for item in decodable.split(",")}
    for item in (content_encoding or "").split(","):
        coding = item.strip().lower()
        if coding and coding != "identity" and coding not in supported:
            return coding
    return ""


def _guard_browser_route(route) -> None:
    """Fail closed if a pinned HTTP fulfiller was not installed."""
    route.abort("blockedbyclient")


class _BrowserWeight:
    """Count delivered responses and bounded image dimensions; retain no bodies."""

    def __init__(self, target: str) -> None:
        self.host = urlparse(target).hostname
        self.totals = dict.fromkeys(
            ("bytes", "html_bytes", "image_bytes", "font_bytes", "third_party_bytes"), 0
        )
        self.observed = 0
        self.unmeasured = 0
        self.reason = ""
        self.documents = 0
        self.pending = 0
        self.image_dimensions: dict[str, dict[str, int]] = {}

    def started(self, request: Any) -> None:
        if urlparse(str(getattr(request, "url", ""))).scheme in {"http", "https"}:
            self.pending += 1

    def omit(self, reason: str, *, completed_request: bool = False) -> None:
        if completed_request:
            self.pending = max(0, self.pending - 1)
        self.unmeasured += 1
        if not self.reason:
            self.reason = reason

    def delivered(
        self,
        request: Any,
        mime: str,
        size: int,
        body: bytes | bytearray | None = None,
        status: int | None = None,
    ) -> None:
        self.pending = max(0, self.pending - 1)
        if self.observed >= _BROWSER_WEIGHT_RESPONSES:
            self.omit("pinned-browser response observation budget exhausted")
            return
        self.observed += 1
        self.totals["bytes"] += size
        kind = getattr(request, "resource_type", "")
        mime = mime.partition(";")[0].strip().lower()
        if kind == "document":
            self.documents += 1
            self.totals["html_bytes"] += size
        if kind == "image" or mime.startswith("image/"):
            self.totals["image_bytes"] += size
            url = str(request.url)
            if body is not None and len(url) <= 2048:
                from seohead.storage.resource_graph import _image_dimensions

                dimensions: dict[str, int] = {}
                # Inspect metadata, never a pixel buffer; unsupported or oversized
                # image declarations must not break delivery or imply a size.
                with contextlib.suppress(Exception):
                    width, height = _image_dimensions(body)
                    if width is not None and height is not None:
                        dimensions = {
                            "intrinsic_width": width,
                            "intrinsic_height": height,
                        }
                        if type(status) is int:
                            dimensions["status_code"] = status
                previous = self.image_dimensions.get(url)
                # One URL can answer differently on repeated actual requests.
                # Conflicting metadata cannot identify the body each element used.
                self.image_dimensions[url] = (
                    dimensions if previous is None or previous == dimensions else {}
                )
        if (
            kind == "font"
            or mime.startswith("font/")
            or mime
            in {"application/font-woff", "application/x-font-ttf", "application/vnd.ms-fontobject"}
        ):
            self.totals["font_bytes"] += size
        if urlparse(str(request.url)).hostname != self.host:
            self.totals["third_party_bytes"] += size

    def finish(self) -> dict[str, Any]:
        missing_document = not self.documents
        state = (
            "unavailable"
            if not self.observed
            else "partial"
            if self.unmeasured or self.pending or missing_document
            else "complete"
        )
        return {
            "state": state,
            "reason": self.reason
            or (
                "pinned-browser requests were unfinished at capture"
                if self.pending
                else "no document response delivered by pinned-browser navigation"
                if missing_document
                else ""
            ),
            **{key: value if self.observed else None for key, value in self.totals.items()},
            "responses_observed": self.observed,
            "responses_unmeasured": self.unmeasured + self.pending,
            "unit": "content-decoded bytes",
            "scope": "current pinned-browser navigation window",
        }


class _BrowserNetworkEvidence:
    """Bounded network-log facts that explain resources rejected by the guard."""

    CAP = 100

    def __init__(self) -> None:
        self.resources: list[dict[str, Any]] = []
        self.omitted = 0
        self.active = False

    def begin(self) -> None:
        self.active = True

    def blocked(self, request: Any, reason: str) -> None:
        if len(self.resources) >= self.CAP:
            self.omitted += 1
            return
        parts = urlsplit(str(getattr(request, "url", "")))
        # Query strings and fragments can carry credentials or personal data.
        safe_url = f"{parts.scheme}://{parts.netloc}{parts.path}" if parts.hostname else ""
        self.resources.append(
            {
                "url": safe_url[:2048],
                "resource_type": str(getattr(request, "resource_type", "unknown"))[:32],
                "reason": reason[:160],
            }
        )

    def http_failure(self, request: Any, status_code: int) -> None:
        before = len(self.resources)
        self.blocked(request, f"HTTP response status {status_code}")
        if len(self.resources) > before:
            self.resources[-1]["status_code"] = status_code

    def finish(self) -> dict[str, Any]:
        return {
            "state": "unavailable" if not self.active else "partial" if self.omitted else "complete",
            "reason": (
                "browser network log was not started"
                if not self.active
                else "blocked resource log reached its 100-entry limit"
                if self.omitted
                else ""
            ),
            "resources": self.resources,
            "omitted": self.omitted,
            "policy": "pinned_http",
        }


def _pinned_browser_route(
    client: Any,
    *,
    request_gate: Callable[[], None] | None = None,
    max_response_bytes: int = _BROWSER_RESPONSE_BYTES,
    weight_observer: _BrowserWeight | None = None,
    network_observer: _BrowserNetworkEvidence | None = None,
    action_phase: Callable[[], bool] | None = None,
) -> tuple[Callable[[Any], None], list[str]]:
    """Build a Playwright fulfiller backed by the shared pinned HTTP transport."""
    if type(max_response_bytes) is not int or max_response_bytes < 1:
        raise ValueError("browser response limit must be a positive integer")
    limitations: list[str] = []
    policy = client_network_policy(client)

    def abort(route: Any, reason: str) -> None:
        if network_observer is not None:
            network_observer.blocked(route.request, reason)
        if weight_observer is not None:
            weight_observer.omit(
                "pinned-browser request failed or was aborted", completed_request=True
            )
        if reason not in limitations:
            limitations.append(reason)
        route.abort("blockedbyclient")

    def handler(route: Any) -> None:
        request = route.request
        url = str(request.url)
        method = str(request.method).upper()
        in_action_phase = bool(action_phase and action_phase())
        action_side_effect = in_action_phase and (
            method == "POST"
            or (method == "GET" and getattr(request, "resource_type", "") == "document")
        )
        if action_side_effect:
            reason = (
                "page actions may not send POST requests"
                if method == "POST"
                else "page actions may not navigate to another document"
            )
            if network_observer is not None:
                network_observer.blocked(request, reason)
            if weight_observer is not None:
                weight_observer.omit(reason, completed_request=True)
            route.abort("blockedbyclient")
            return
        if method not in _BROWSER_METHODS:
            reason = (
                "browser method is unsupported by pinned rendering"
                if policy is not None
                else f"browser method {method} is unsupported by pinned rendering"
            )
            abort(route, reason)
            return
        try:
            if policy is None:
                validate_url(url)
            else:
                validate_url(url, policy=policy)
            if request_gate is not None:
                request_gate()
            headers = {
                name: value
                for name, value in request.all_headers().items()
                if name.lower() not in _HOP_BY_HOP_HEADERS and name.lower() != "accept-encoding"
            }
            # Chromium invites codings this transport may not own a decoder for
            # (it asks for br and zstd), and the reply has to be decoded here
            # before Playwright sees it. Asking only for what this client can
            # decode keeps the origin from answering in a coding that would
            # reach the renderer unparsed.
            decodable = _decodable_encodings(client)
            headers["accept-encoding"] = decodable
            cookies = getattr(client, "cookies", None)
            if cookies is not None:
                cookies.clear()
            body = request.post_data_buffer if method == "POST" else None
            if body is not None and not isinstance(body, bytes):
                raise TypeError("browser POST body is unavailable")
            with client.stream(method, url, headers=headers, content=body) as response:
                response_headers: dict[str, str] = {}
                response_header_names: dict[str, str] = {}
                cookie_headers: list[str] = []
                has_cors_header = False
                for name, value in response.headers.multi_items():
                    lowered = name.lower()
                    if lowered == "set-cookie":
                        cookie_headers.append(value)
                        continue
                    if lowered == "access-control-allow-origin":
                        has_cors_header = True
                    if (
                        lowered not in _HOP_BY_HOP_HEADERS
                        and lowered not in _TRANSFER_CODING_HEADERS
                    ):
                        response_name = response_header_names.setdefault(lowered, name)
                        if response_name in response_headers:
                            response_headers[response_name] += f", {value}"
                        else:
                            response_headers[response_name] = value
                if cookie_headers:
                    response_headers["set-cookie"] = "\n".join(cookie_headers)
                origin = headers.get("origin")
                request_parts = urlparse(url)
                request_origin = f"{request_parts.scheme}://{request_parts.netloc}"
                if origin and origin != request_origin and not has_cors_header:
                    response_headers["access-control-allow-origin"] = ""
                undecoded = _undecoded_coding(
                    response.headers.get("content-encoding", ""), decodable
                )
                if undecoded:
                    abort(
                        route,
                        (
                            "browser response content coding is undecodable by pinned rendering"
                            if policy is not None
                            else f"browser response content coding {undecoded} is undecodable "
                            "by pinned rendering"
                        ),
                    )
                    return
                # Decoded bytes, not transferred ones: this is the body Chromium
                # is handed, so it is also the body the cap has to measure -- a
                # compression bomb would otherwise pass the cap compressed.
                body = bytearray()
                for chunk in response.iter_bytes():
                    if len(body) + len(chunk) > max_response_bytes:
                        abort(route, "browser response exceeds pinned rendering byte limit")
                        return
                    body.extend(chunk)
                route.fulfill(
                    status=response.status_code, headers=response_headers, body=bytes(body)
                )
                if network_observer is not None and response.status_code >= 400:
                    network_observer.http_failure(request, response.status_code)
                if weight_observer is not None:
                    weight_observer.delivered(
                        request,
                        response.headers.get("content-type", ""),
                        len(body),
                        body,
                        response.status_code,
                    )
        except RenderCancelled:
            abort(route, _RENDER_CANCELLED)
        except Exception as exc:
            abort(route, f"pinned browser request failed: {_network_error_summary(exc, policy)}")

    return handler, limitations


def _bounded_action_evaluator(context: Any, page: Any):
    """Return an isolated Chromium evaluator with a CDP execution deadline."""
    session = context.new_cdp_session(page)
    session.send("Runtime.enable")
    session.send("Page.enable")
    frame_tree = session.send("Page.getFrameTree")
    frame_id = frame_tree["frameTree"]["frame"]["id"]
    world = session.send(
        "Page.createIsolatedWorld",
        {"frameId": frame_id, "worldName": "SpiderHeadPageActions"},
    )
    execution_context_id = world["executionContextId"]

    def evaluate(script: str, argument: Any, timeout_ms: int) -> Any:
        if type(timeout_ms) is not int or timeout_ms < 1:
            raise TimeoutError("page action time budget exhausted")
        expression = (
            f"({script})()"
            if argument is None
            else f"({script})({json.dumps(argument, ensure_ascii=True)})"
        )
        response = session.send(
            "Runtime.evaluate",
            {
                "expression": expression,
                "contextId": execution_context_id,
                "returnByValue": True,
                "awaitPromise": False,
                "timeout": timeout_ms,
            },
        )
        if response.get("exceptionDetails"):
            raise RuntimeError("bounded page action evaluation failed")
        return (response.get("result") or {}).get("value")

    return evaluate, session


def _guard_websocket_route(
    ws_route: Any,
    limitations: list[str] | None = None,
    weight_observer: _BrowserWeight | None = None,
) -> None:
    """Fail closed because this renderer has no pinned WebSocket transport."""
    if limitations is not None and _BLOCKED_WEBSOCKET_LIMITATION not in limitations:
        limitations.append(_BLOCKED_WEBSOCKET_LIMITATION)
    if weight_observer is not None:
        weight_observer.omit(_BLOCKED_WEBSOCKET_LIMITATION)
    ws_route.close()


def _refuse_if_root() -> None:
    """Refuse to launch the rendering browser as root, rather than disable its sandbox.

    Chromium's sandbox will not start as root unless it is told to run
    without one (``--no-sandbox``). This toolkit never passes that flag --
    rendering executes whatever code the audited site serves, and a browser
    with no sandbox removes the one barrier between that code and the host
    running the crawl. Refusing outright is the documented alternative.
    """
    geteuid = getattr(os, "geteuid", None)
    if geteuid is not None and geteuid() == 0:
        raise RuntimeError(
            "refusing to launch the rendering browser as root: that would require "
            "disabling the sandbox (--no-sandbox), which this toolkit never does -- "
            "run the crawl as a non-root user instead"
        )


def _artifact_filename(url: str) -> str:
    """A filesystem-safe, collision-resistant name for one URL's artifacts."""
    return hashlib.sha256(url.encode("utf-8")).hexdigest()[:24]


def _redact_console(value: Any) -> str:
    text = str(value or "")[:MAX_CONSOLE_ERROR_CHARS]
    text = re.sub(
        r"(?i)(?:authorization|token|secret|password|cookie)\s*[:=]\s*[^\s,;]+", "[redacted]", text
    )
    return re.sub(r"https?://[^\s'\"]+", "[url]", text)


def _error_summary(exc: Exception, policy: Any = None) -> str:
    """Keep remote operational failures free of URLs, headers and browser data."""
    if policy is not None or current_remote_policy() is not None:
        if isinstance(exc, RemoteTargetError):
            return exc.code
        return "remote browser request failed"
    return f"{type(exc).__name__}: {exc}"


_TLS_VERIFICATION_FAILURE = (
    "TLS certificate verification failed; trust the intercepting CA in the platform store "
    "or set SSL_CERT_FILE/SSL_CERT_DIR"
)


def _network_error_summary(exc: Exception, policy: Any = None) -> str:
    """Name certificate failures without exposing transport detail or credentials."""
    if any(
        marker in str(exc).lower()
        for marker in ("certificate_verify_failed", "proxied origin tls verification failed")
    ):
        return _TLS_VERIFICATION_FAILURE
    return _error_summary(exc, policy)


def _staged_screenshot_path(artifacts_dir: str, url: str) -> str:
    directory = os.path.abspath(artifacts_dir)
    if os.path.islink(directory) or os.path.islink(os.path.dirname(directory)):
        raise ValueError("browser artifact staging directory is unsafe")
    os.makedirs(directory, mode=0o700, exist_ok=True)
    if os.path.islink(directory) or not os.path.isdir(directory):
        raise ValueError("browser artifact staging directory is unsafe")
    descriptor, path = tempfile.mkstemp(
        prefix=_artifact_filename(url) + "-", suffix=".png", dir=directory
    )
    os.close(descriptor)
    return path


_FRAGMENT_META_RE = re.compile(
    r'<meta[^>]+name=["\']fragment["\'][^>]+content=["\']!["\']', re.IGNORECASE
)


def legacy_fragment_target(url: str, html: str) -> str | None:
    """Return the ``_escaped_fragment_`` URL for a page opting into the legacy
    AJAX-crawling scheme, or ``None`` when the page does not declare it.

    Google's now-deprecated scheme let a site announce, via a ``#!`` hash
    fragment in its own URL or a page-wide
    ``<meta name="fragment" content="!">``, that a fully rendered snapshot is
    available at a companion URL built from ``?_escaped_fragment_=``. Some
    legacy single-page applications still implement only this, not real
    server-side rendering or a modern render pipeline, so honouring it
    recovers real content without needing a browser at all.
    """
    parts = urlparse(url)
    fragment = parts.fragment
    if fragment.startswith("!"):
        escaped = fragment[1:]
    elif html and _FRAGMENT_META_RE.search(html):
        escaped = ""
    else:
        return None
    query = dict(parse_qsl(parts.query))
    query["_escaped_fragment_"] = escaped
    return urlunsplit((parts.scheme, parts.netloc, parts.path, urlencode(query), ""))


# This script reads laboratory metrics after load. LCP and CLS are captured by
# PerformanceObserver; the remaining values come from Navigation Timing.
_METRICS_JS = """() => {
  const nav = performance.getEntriesByType('navigation')[0] || {};
  const lcpEntries = performance.getEntriesByType('largest-contentful-paint') || [];
  const paints = {};
  for (const p of performance.getEntriesByType('paint')) paints[p.name] = Math.round(p.startTime);
  return {
    ttfb_ms: Math.round(nav.responseStart || 0),
    dom_content_loaded_ms: Math.round(nav.domContentLoadedEventEnd || 0),
    load_ms: Math.round(nav.loadEventEnd || 0),
    first_contentful_paint_ms: paints['first-contentful-paint'] ?? null,
    largest_contentful_paint_ms: window.__seohead_lcp
      ? Math.round(window.__seohead_lcp)
      : (lcpEntries.length ? Math.round(lcpEntries[lcpEntries.length - 1].startTime) : null),
    cumulative_layout_shift: window.__seohead_cls != null
      ? Math.round(window.__seohead_cls * 1000) / 1000 : null,
    transfer_size_kb: nav.transferSize ? Math.round(nav.transferSize / 1024) : null,
  };
}"""

# CLS and LCP accumulate from navigation start, so observers must be installed
# before navigation. Installing them afterward misses the earliest and often
# largest shifts. LCP also requires a buffered observer in practice because
# ``getEntriesByType('largest-contentful-paint')`` is commonly empty; relying on
# that API alone produced null LCP values in real-site runs.
_CLS_INIT_JS = """
window.__seohead_cls = 0;
window.__seohead_lcp = 0;
try {
  new PerformanceObserver((list) => {
    for (const entry of list.getEntries()) {
      if (!entry.hadRecentInput) window.__seohead_cls += entry.value;
    }
  }).observe({type: 'layout-shift', buffered: true});
} catch (e) {}
try {
  new PerformanceObserver((list) => {
    const entries = list.getEntries();
    if (entries.length) window.__seohead_lcp = entries[entries.length - 1].startTime;
  }).observe({type: 'largest-contentful-paint', buffered: true});
} catch (e) {}
"""

# getComputedStyle resolves background-image wherever the declaring CSS rule
# lives -- inline style, a <style> block, or an external stylesheet -- so it
# is the only way to see a background image an external stylesheet declares:
# that CSS text never appears in either the raw or the rendered HTML string.
_BACKGROUND_IMAGES_JS = """() => {
  const found = new Set();
  const urlRe = /url\\((['"]?)(.*?)\\1\\)/g;
  document.querySelectorAll('*').forEach((el) => {
    const bg = getComputedStyle(el).backgroundImage;
    if (!bg || bg === 'none') return;
    let m;
    while ((m = urlRe.exec(bg))) {
      const src = m[2];
      if (src && !src.startsWith('data:')) found.add(new URL(src, document.baseURI).href);
    }
  });
  return Array.from(found);
}"""

# Browser natural sizes are density-corrected CSS pixels, not file pixels.
# Physical image dimensions come from the already-delivered pinned body.
_IMAGE_BOXES_JS = """() => {
  const all = Array.from(document.querySelectorAll('img'));
  const images = all.slice(0, 2000).map((img) => {
    const rect = img.getBoundingClientRect();
    const style = getComputedStyle(img);
    const spacing = ['paddingLeft', 'paddingRight', 'paddingTop', 'paddingBottom',
      'borderLeftWidth', 'borderRightWidth', 'borderTopWidth', 'borderBottomWidth']
      .some((key) => parseFloat(style[key]) > 0);
    const transformed = style.transform !== 'none'
      || Math.abs(rect.width - img.offsetWidth) > 0.5
      || Math.abs(rect.height - img.offsetHeight) > 0.5;
    const geometryReason = style.objectFit !== 'fill' ? 'object-fit image geometry is not measured'
      : spacing ? 'image padding or border geometry is not measured'
      : transformed ? 'transformed image geometry is not measured' : '';
    const candidate = img.currentSrc || img.src || '';
    const src = candidate.length <= 2048 && !/^(data|blob):/.test(candidate) ? candidate : '';
    const entry = src ? performance.getEntriesByName(src)[0] : null;
    return {
      src: src,
      natural_width: img.naturalWidth,
      natural_height: img.naturalHeight,
      rendered_width: Math.round(rect.width),
      rendered_height: Math.round(rect.height),
      encoded_bytes: entry ? (entry.encodedBodySize || 0) : 0,
      geometry_state: geometryReason ? 'unavailable' : 'complete',
      geometry_reason: geometryReason,
    };
  });
  return {dpr: window.devicePixelRatio || 1, images: images,
          images_omitted: Math.max(0, all.length - images.length)};
}"""

# An image counts as oversized only when it is at least this heavy on the wire;
# a 2x-too-large 2 KB icon wastes nothing worth a finding.
IMAGE_OVERSIZED_RATIO = 2
IMAGE_OVERSIZED_MIN_BYTES = 10 * 1024


def image_sizing_findings(
    images: list[dict[str, Any]], dpr: float
) -> dict[str, list[dict[str, Any]]]:
    """Compare each rendered image's intrinsic pixels with the device pixels its box needs.

    ``oversized``: the file has more than ``IMAGE_OVERSIZED_RATIO`` times the pixels
    the box can show, and weighs at least ``IMAGE_OVERSIZED_MIN_BYTES``.
    ``upscaled``: the file is narrower than the box in device pixels, so it blurs.
    Requires physical ``intrinsic_width``/``intrinsic_height`` from the delivered
    file. Browser naturalWidth is density-corrected and cannot substitute.
    Unproved dimensions and zero display boxes are skipped, not measured as zero.
    """
    seen: set[tuple] = set()
    oversized: list[dict[str, Any]] = []
    upscaled: list[dict[str, Any]] = []
    for img in images:
        src = str(img.get("src") or "")
        natural_w = int(img.get("intrinsic_width") or 0)
        natural_h = int(img.get("intrinsic_height") or 0)
        rendered_w = int(img.get("rendered_width") or 0)
        rendered_h = int(img.get("rendered_height") or 0)
        key = (src, natural_w, rendered_w, rendered_h)
        if (
            not src
            or img.get("geometry_state") == "unavailable"
            or key in seen
            or natural_w <= 0
            or natural_h <= 0
            or rendered_w <= 0
            or rendered_h <= 0
        ):
            continue
        seen.add(key)
        needed_w = rendered_w * dpr
        entry = {
            "src": src,
            "natural": [natural_w, natural_h],
            "rendered": [rendered_w, rendered_h],
            "dpr": dpr,
            **({"status_code": img["status_code"]} if type(img.get("status_code")) is int else {}),
        }
        if (
            natural_w > IMAGE_OVERSIZED_RATIO * needed_w
            and int(img.get("encoded_bytes") or 0) >= IMAGE_OVERSIZED_MIN_BYTES
        ):
            oversized.append(entry)
        elif natural_w < needed_w:
            upscaled.append(entry)
    return {"oversized": oversized, "upscaled": upscaled}


def _render_asset_evidence(page: Any, coverage: Any, weight: _BrowserWeight) -> dict[str, Any]:
    """Capture bounded layout evidence separately from HTML and field metrics."""
    try:
        boxes = page.evaluate(_IMAGE_BOXES_JS)
        if not isinstance(boxes, dict) or not isinstance(boxes.get("images"), list):
            raise ValueError("image boxes are unavailable")
        images = [
            {
                **img,
                **(
                    weight.image_dimensions.get(img.get("src"), {})
                    if img.get("natural_width")
                    and img.get("natural_height")
                    and img.get("geometry_state") != "unavailable"
                    else {}
                ),
            }
            for img in boxes["images"]
        ]
        sizing = image_sizing_findings(images, float(boxes.get("dpr") or 1))
        unmeasured = sum(
            not img.get("src")
            or not img.get("intrinsic_width")
            or not img.get("intrinsic_height")
            or not img.get("rendered_width")
            or not img.get("rendered_height")
            or not img.get("encoded_bytes")
            for img in images
        )
        layout = {
            "state": "unavailable"
            if images and unmeasured == len(images)
            else "partial"
            if boxes.get("images_omitted") or unmeasured
            else "complete",
            "reason": "physical image dimensions, delivered body size or visible layout unavailable"
            if unmeasured
            else "image observation budget exceeded"
            if boxes.get("images_omitted")
            else "",
            "images_observed": len(images),
            "images_unmeasured": unmeasured,
            "images_omitted": boxes.get("images_omitted", 0),
            "image_sizing": sizing,
            "min_encoded_bytes": IMAGE_OVERSIZED_MIN_BYTES,
            "oversized_ratio": IMAGE_OVERSIZED_RATIO,
        }
        geometry_reasons = sorted(
            {img["geometry_reason"] for img in images if img.get("geometry_reason")}
        )
        if geometry_reasons:
            layout["reason"] = "; ".join(geometry_reasons)
    except Exception:
        layout = {
            "state": "unavailable",
            "reason": "native render did not capture image display sizes",
        }
    result = {
        "source": "native-render",
        "layout": layout,
        "coverage": coverage.finish(),
        "weight": weight.finish(),
    }
    if len(json.dumps(result, ensure_ascii=True)) > 512 * 1024:
        reason = "native render asset summary exceeds the 512 KiB retention bound"
        return {
            "source": "native-render",
            "layout": {"state": "unavailable", "reason": reason},
            "coverage": {"state": "unavailable", "reason": reason},
            "weight": weight.finish(),
        }
    return result


def _visible_text(html: str) -> str:
    """Return candidate content text after removing scripts, styles, and tags."""
    return _TAG_RE.sub(" ", _SCRIPT_STYLE_RE.sub(" ", html or ""))


def _words(html: str) -> int:
    return len([w for w in _visible_text(html).split() if len(w) > 1])


def _links(html: str, base_url: str) -> set[str]:
    """Return internal links that can participate in crawling the site."""
    if not html:
        return set()
    from seohead.checks.parser import document_base_url

    # Host comes from the page URL; links resolve against the document base.
    host = urlparse(normalize_url(base_url)).hostname or ""
    soup = BeautifulSoup(html, features="lxml")
    resolve_from = document_base_url(soup, base_url)
    out: set[str] = set()
    for tag in soup.find_all("a", href=True):
        href = str(tag["href"]).strip()
        if not href or href.startswith(("#", "javascript:", "mailto:", "tel:")):
            continue
        absolute = urljoin(resolve_from, href).split("#")[0]
        if (urlparse(absolute).hostname or "") == host:
            out.add(absolute)
    return out


def _jsonld_types(html: str) -> list[str]:
    """Extract Schema.org JSON-LD types.

    Markup injected only after JavaScript does not exist for non-rendering
    crawlers, so raw and rendered type sets are measured separately.
    """
    types: list[str] = []
    for block in re.findall(
        r'<script[^>]+type=["\']application/ld\+json["\'][^>]*>(.*?)</script>',
        html or "",
        re.IGNORECASE | re.DOTALL,
    ):
        try:
            data = json.loads(block.strip())
        except (ValueError, TypeError):
            continue
        stack = [data]
        while stack:
            node = stack.pop()
            if isinstance(node, dict):
                t = node.get("@type")
                if isinstance(t, str):
                    types.append(t)
                elif isinstance(t, list):
                    types += [x for x in t if isinstance(x, str)]
                stack += [v for v in node.values() if isinstance(v, (dict, list))]
            elif isinstance(node, list):
                stack += node
    return sorted(set(types))


def detect_empty_shell(html: str) -> str | None:
    """Return the ID of an empty SPA mount container, or ``None``.

    A raw-HTML regex match, not a rendering result -- so this is also what
    the crawl-level gate in ``seohead.crawl.render_escalation`` calls on the
    start page's raw HTML, before any browser is ever launched (#18).
    """
    for shell_id in _SHELL_IDS:
        m = re.search(
            rf'<div[^>]+id=["\']{shell_id}["\'][^>]*>(.*?)</div>',
            html or "",
            re.IGNORECASE | re.DOTALL,
        )
        if m and not m.group(1).strip():
            return shell_id
        if re.search(
            rf'<div[^>]+id=["\']{shell_id}["\'][^>]*/?>\s*</div>', html or "", re.IGNORECASE
        ):
            return shell_id
    return None


def _snapshot(html: str, url: str, representation: str = "unknown") -> dict[str, Any]:
    """Build an identical, comparable snapshot for raw HTML and rendered DOM."""
    from seohead.checks.page_facts import extract

    facts = extract(html, url) if html else {}
    # Images: <img>/<source> plus CSS url() backgrounds declared inline or in a
    # <style> block. A background declared only in an external stylesheet is
    # not visible here -- render_check() merges that in from computed styles.
    images = sorted(dualcrawl.build_page_evidence(html, url)["images"])
    return {
        "words": _words(html),
        "links": len(_links(html, url)),
        "images": images,
        "title": facts.get("title") or "",
        "h1": facts.get("h1") or "",
        "description": facts.get("description") or "",
        "canonical": facts.get("canonical") or "",
        "meta_robots": facts.get("meta_robots") or "",
        "jsonld_types": _jsonld_types(html),
        "html_bytes": len(html or ""),
        "dom_bytes": len((html or "").encode("utf-8")),
        "line_count": (html or "").count("\n") + (1 if html else 0),
        "representation": representation,
    }


def incomplete_render_reason(raw: dict[str, Any], rendered: dict[str, Any]) -> str | None:
    """Name why a rendered snapshot cannot be compared, or return ``None``.

    A browser that fails mid-navigation still hands back a document, and that
    document is nearly empty. Comparing it against a full raw response produces
    confident nonsense -- "the title changes after JavaScript" from a title the
    render never read, "the canonical is injected by JavaScript" from a
    canonical the render never saw (#623, four such findings in a row on one
    live site). The pair is therefore judged before it is compared.

    The evidence for "this did not capture the page" is the *conjunction*: the
    raw response carries landmarks (a title, or internal links), the rendered
    document carries none of them at all -- no title, no h1, no canonical, no
    internal link -- and it is a fraction of the raw response's size. A page
    that genuinely renders to nothing keeps at least one of those, and a page
    with no title and no links on either side is measured and merely empty, not
    unmeasured. Returning ``None`` therefore means "compare these", never "this
    page is fine".
    """
    if not isinstance(raw, dict) or not isinstance(rendered, dict):
        return None
    if rendered.get("title") or rendered.get("h1") or rendered.get("canonical"):
        return None
    if int(rendered.get("links") or 0) > 0:
        return None
    # Nothing was lost if the raw response had nothing to lose.
    if not raw.get("title") and int(raw.get("links") or 0) == 0:
        return None
    raw_bytes = int(raw.get("html_bytes") or 0)
    rendered_bytes = int(rendered.get("html_bytes") or 0)
    if raw_bytes <= 0 or rendered_bytes >= raw_bytes * INCOMPLETE_RENDER_BYTE_RATIO:
        return None
    return (
        "the rendered document has no title, no h1, no canonical and no internal links, "
        f"at {rendered_bytes} bytes against the raw response's {raw_bytes} "
        f"({rendered_bytes / raw_bytes:.0%})"
    )


def _shell_finding(shell: str) -> str:
    """State the empty-mount-point fact, which reads the raw HTML and nothing else."""
    return (
        f'Raw HTML contains an empty <div id="{shell}"> mount point; the '
        "page is assembled entirely by JavaScript, so a non-rendering "
        "crawler receives an empty page"
    )


def little_text(snapshot: dict[str, Any]) -> bool:
    """Whether a raw snapshot is too thin to stand on its own without JavaScript.

    Feeds ``rendering.escalation.policy: "auto"`` (the desktop Auto mode): a
    pattern whose sampled raw page is below ``EMPTY_BODY_WORDS`` is escalated even
    when raw and rendered agree, because the raw page carries too little text to
    trust as the whole picture.
    """
    return int(snapshot.get("words", 0) or 0) < EMPTY_BODY_WORDS


def compare(
    raw: dict[str, Any], rendered: dict[str, Any], raw_html: str = "", shell: str | None = None
) -> list[str]:
    """Generate findings for a raw-HTML and rendered-DOM snapshot pair.

    This pure function uses neither the network nor a browser, allowing complete
    offline tests while the Playwright layer remains a thin adapter.

    A pair whose rendered half never captured the page yields the
    ``RENDER_UNAVAILABLE`` statement and no finding that draws on the rendered
    half: an unfinished measurement is not evidence about the site. Findings
    that read the raw response alone still hold, because nothing about them
    depended on the browser -- ``shell`` comes from ``detect_empty_shell()``,
    which never looks at the rendered DOM, and an empty single-page-application
    shell is exactly the page whose render times out, so dropping it with the
    render lost a genuine defect precisely where it mattered most (#642).
    """
    unavailable = incomplete_render_reason(raw, rendered)
    if unavailable:
        out: list[str] = [RENDER_UNAVAILABLE.format(reason=unavailable)]
        if shell:
            out.append(_shell_finding(shell))
        return out

    out = []

    if shell:
        out.append(_shell_finding(shell))
    elif raw.get("words", 0) < EMPTY_BODY_WORDS < rendered.get("words", 0):
        out.append(
            f"Raw HTML contains {raw['words']} words versus "
            f"{rendered['words']} after rendering; the server response "
            "contains effectively no page copy"
        )

    words_gain = rendered.get("words", 0) - raw.get("words", 0)
    if raw.get("words", 0) >= EMPTY_BODY_WORDS and words_gain > 0:
        share = words_gain / max(rendered.get("words", 1), 1)
        if share >= 0.3:
            out.append(
                f"{share:.0%} of page copy appears only after JavaScript "
                f"(+{words_gain} words); this content is unavailable to "
                "non-rendering crawlers"
            )

    links_gain = rendered.get("links", 0) - raw.get("links", 0)
    if links_gain > 0 and rendered.get("links", 0):
        share = links_gain / rendered["links"]
        if share >= 0.3 or raw.get("links", 0) == 0:
            out.append(
                f"{links_gain} of {rendered['links']} internal links appear "
                "only after JavaScript, reducing or preventing crawl discovery"
            )

    if raw.get("title") != rendered.get("title"):
        out.append(
            f"The title changes after JavaScript: raw {raw.get('title')!r}, "
            f"rendered {rendered.get('title')!r}; crawlers may index "
            "different title values"
        )
    if raw.get("h1") != rendered.get("h1") and rendered.get("h1"):
        out.append(
            f"H1 differs between raw HTML {raw.get('h1') or '—'!r} and rendered DOM "
            f"{rendered.get('h1')!r}"
        )
    if raw.get("canonical") != rendered.get("canonical"):
        out.append(
            "The canonical URL is injected or changed by JavaScript; this "
            "indexing directive should not depend on rendering"
        )

    new_types = set(rendered.get("jsonld_types", [])) - set(raw.get("jsonld_types", []))
    if new_types:
        out.append("Schema.org types appear only after JavaScript: " + ", ".join(sorted(new_types)))

    new_images = set(rendered.get("images", [])) - set(raw.get("images", []))
    if new_images:
        out.append(
            f"{len(new_images)} image(s) are visible only after rendering, most often a CSS "
            "background-image resolved from an external stylesheet; a non-rendering crawler, "
            "and every alt-text or image-weight check built on <img> alone, sees none of them"
        )

    if not out:
        out.append(ALL_CLEAR)
    return out


class _NeverRaised(Exception):
    """Stands in for Playwright's ``TimeoutError`` when it cannot be imported."""


# Deferred and lazily-hydrating scripts write the DOM after the navigation
# milestone resolves. Half a second is enough for that on the pages this check
# was misreading, and short enough not to change the cost of a render.
SETTLE_MS = 500


def _capture_dom(
    page: Any,
    target: str,
    wait: str,
    timeout: float,
    settle_ms: int,
    navigation_timeout: type[BaseException],
) -> str:
    """Navigate, settle, and report the load milestone actually reached.

    A site with long-polling third-party scripts -- analytics, chat, ads -- may
    never go network-idle and may not fire ``load`` either. That is ordinary,
    and losing the whole check to it is worse than capturing the DOM slightly
    earlier. ``DOMContentLoaded`` has normally fired long before the timeout, so
    ``wait_for_load_state`` returns at once and the page is read without being
    fetched a second time. When it genuinely never fired there is no document to
    read, and the timeout propagates unchanged.
    """
    try:
        page.goto(target, wait_until=wait, timeout=timeout * 1000)
        reached = wait
    except navigation_timeout:
        if wait == "domcontentloaded":
            raise
        page.wait_for_load_state("domcontentloaded", timeout=timeout * 1000)
        reached = "domcontentloaded"
    if settle_ms > 0:
        page.wait_for_timeout(settle_ms)
    return reached


def render_check(
    url: str,
    timeout: float = 30.0,
    wait: str = "load",
    viewport: str = "desktop",
    user_agent: str | None = None,
    *,
    settle_ms: int = SETTLE_MS,
    request_gate: Callable[[], None] | None = None,
    engine: str = "chromium",
    viewport_size: dict[str, int] | None = None,
    mobile_emulation: bool | None = None,
    touch_emulation: bool | None = None,
    transport_config: dict[str, str] | None = None,
    proxy_route=None,
) -> dict[str, Any]:
    """Compare a server response with the DOM produced after JavaScript executes.

    Playwright is optional. When unavailable, the tool returns ``ok: False`` and
    an installation command instead of misrepresenting an unperformed check.

    ``wait="load"`` is deliberate. ``networkidle`` may never occur on commercial
    sites because analytics, chat, and advertising keep connections open, turning
    a useful render check into a timeout. Search-engine rendering does not require
    complete network silence either. Callers may still request ``networkidle``
    when a particular application genuinely needs it -- and when that milestone
    times out the DOM is still read at ``domcontentloaded`` rather than the whole
    check being lost, with ``wait_reached`` recording which milestone the
    snapshot actually came from -- and with the findings list saying so too, so
    that a snapshot taken before the scripts ran can never report an all-clear
    (#642). ``settle_ms`` is a short pause after that milestone for deferred
    scripts to write the DOM.

    When the browser hands back a document that never captured the page, the
    result is ``ok: False`` with a named reason (``reason:
    "incomplete_render"``) and both snapshots for inspection -- never findings
    about the site, which is what an unfinished render used to be reported as
    (#623). ``empty_shell`` rides along with it: that answer comes from the raw
    response and does not depend on the browser having finished.
    """
    if viewport not in VIEWPORT_PRESETS:
        return {"ok": False, "error": f"unknown viewport {viewport!r}"}
    if engine not in BROWSER_ENGINES:
        supported = ", ".join(sorted(BROWSER_ENGINES))
        return {
            "ok": False,
            "error": f"unsupported rendering engine {engine!r}; supported engines: {supported}",
        }
    # ``None`` means the caller left the choice to the preset, exactly as
    # before; an explicit flag lets crawl escalation match what its own
    # settings say the render will do.
    is_mobile = (viewport == "mobile") if mobile_emulation is None else bool(mobile_emulation)
    has_touch = (viewport == "mobile") if touch_emulation is None else bool(touch_emulation)
    # Only is_mobile is gated: Playwright documents it as unsupported on
    # Firefox, while has_touch is a general context option every engine
    # accepts, so a touch-only request is passed through to the launcher.
    if not BROWSER_ENGINES[engine]["mobile_emulation"] and is_mobile:
        return {
            "ok": False,
            "error": (
                f"rendering engine {engine!r} cannot emulate a mobile viewport "
                "(Playwright does not implement is_mobile for it); pick another "
                "engine or a non-mobile viewport"
            ),
        }
    if viewport_size is not None:
        width = viewport_size.get("width")
        height = viewport_size.get("height")
        if type(width) is not int or type(height) is not int or width < 1 or height < 1:
            return {
                "ok": False,
                "error": "viewport_size must carry positive integer width and height",
            }
        size = {"width": width, "height": height}
    else:
        size = dict(VIEWPORT_PRESETS[viewport])
    invalid_identity = {
        "ok": False,
        "error": "user_agent must be a single header line",
    }
    if user_agent is not None and not isinstance(user_agent, str):
        return invalid_identity
    selected_user_agent = user_agent or (MOBILE_USER_AGENT if viewport == "mobile" else UA)
    if not selected_user_agent.strip() or any(
        ord(character) < 32 or ord(character) > 126 for character in selected_user_agent
    ):
        return invalid_identity
    if not url or not str(url).strip():
        return {"ok": False, "error": "URL is required"}
    target = normalize_url(str(url).strip())

    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        remote = (transport_config or {}).get("transport") == "remote"
        return {
            "ok": False,
            "error": "Playwright is required",
            "install": (
                "pip install 'seohead[render]'"
                if remote
                else f"pip install 'seohead[render]' && python -m playwright install {engine}"
            ),
        }
    try:
        from playwright.sync_api import TimeoutError as navigation_timeout
    except ImportError:  # a harness may expose only sync_playwright
        navigation_timeout = _NeverRaised
    try:
        validate_url(target)
    except ValueError as exc:
        return {"ok": False, "url": target, "error": str(exc)}
    try:
        endpoint, transport_facts = prepare(transport_config)
        transport_info = {"browser_transport": transport_facts} if endpoint is not None else {}
        if endpoint is None:
            _refuse_if_root()
    except BrowserTransportError as exc:
        return {"ok": False, "url": target, "reason": exc.code, "error": str(exc)}
    except RuntimeError as exc:
        return {"ok": False, "url": target, "error": str(exc)}

    navigation = NavigationCapture(target, wait, timeout)
    transport_info["navigation"] = navigation.data
    weight = _BrowserWeight(target)
    asset_evidence = {"source": "native-render", "weight": weight.finish()}
    transport_info["asset_evidence"] = asset_evidence
    network_evidence = _BrowserNetworkEvidence()
    transport_info["network_evidence"] = network_evidence.finish()
    coverage = None
    render_completed = False

    # Fetch raw HTML with the regular client: this is what a non-rendering crawler receives.
    from seohead.recon.net import crawl_transport_options

    transport_options = crawl_transport_options(proxy_route)
    if request_gate is None:
        client, _ = http_client(
            timeout, headers={"User-Agent": selected_user_agent}, **transport_options
        )
    else:
        client, _ = http_client(
            timeout,
            headers={"User-Agent": selected_user_agent},
            event_hooks={"request": [lambda _request: request_gate()]},
            **transport_options,
        )
    try:
        resp = client.get(target)
        raw_html = resp.text
        final_url = str(resp.url)
        status = resp.status_code
    except Exception as exc:
        return {
            "ok": False,
            "error": f"Raw HTML fetch failed: {_network_error_summary(exc)}",
            "url": target,
            "viewport": viewport,
            "viewport_size": size,
            "user_agent": selected_user_agent,
        }
    finally:
        client.close()

    browser_client = None
    try:
        browser_client, _http2 = http_client(
            timeout,
            follow_redirects=False,
            headers={"User-Agent": selected_user_agent},
            **transport_options,
        )
        with sync_playwright() as pw:
            try:
                browser = open_browser(
                    getattr(pw, engine),
                    endpoint,
                    timeout_seconds=timeout,
                    local_launch_options=(
                        {
                            **dict(BROWSER_ENGINES[engine]["launch_options"]),
                            **_local_chromium_launch_options(),
                        }
                        if endpoint is None and engine == "chromium"
                        else dict(BROWSER_ENGINES[engine]["launch_options"])
                    ),
                )
            except Exception as exc:
                if endpoint is None and _missing_browser_binary(exc):
                    return {
                        "ok": False,
                        "url": target,
                        "error": (
                            f"the {engine!r} browser binary is not installed for this "
                            f"Playwright ({type(exc).__name__})"
                        ),
                        "install": f"python -m playwright install {engine}",
                    }
                raise
            context = None
            try:
                # service_workers="block": a default-configuration service
                # worker can serve requests the page.route() guard below never
                # sees, the exact bypass #18's security section names.
                # user_agent=UA: the raw fetch above used this same identity. Without it,
                # Chromium's own default UA reaches the origin instead, and a server that
                # varies its response by User-Agent (legitimate, common, and unrelated to
                # JavaScript) looks indistinguishable from a page that genuinely needs a
                # renderer -- issue #199. Matching identity removes that confound rather
                # than trying to detect it after the fact.
                context_options: dict[str, Any] = {
                    "viewport": size,
                    "service_workers": "block",
                    "user_agent": selected_user_agent,
                }
                # Passed only when set: engines without device emulation
                # (Firefox) reject the key outright rather than ignore it.
                if is_mobile:
                    context_options["is_mobile"] = True
                if has_touch:
                    context_options["has_touch"] = True
                context = open_context(browser, endpoint, context_options)
                context.add_init_script(_CLS_INIT_JS)
                route_handler, limitations = _pinned_browser_route(
                    browser_client,
                    request_gate=request_gate,
                    weight_observer=weight,
                    network_observer=network_evidence,
                )
                network_evidence.begin()
                context.route("**/*", route_handler)
                # WebSockets are not HTTP requests and page.route() never sees
                # them either; route_web_socket is the separate interception
                # point that covers them.
                context.route_web_socket(
                    "**/*", lambda ws_route: _guard_websocket_route(ws_route, limitations, weight)
                )
                page = context.new_page()
                page.on("request", weight.started)
                navigation.attach(context, page)
                from seohead.checks.browser_coverage import BrowserCoverage

                coverage = BrowserCoverage(page, engine)
                wait_reached = _capture_dom(
                    page, target, wait, timeout, settle_ms, navigation_timeout
                )
                rendered_html = page.content()
                rendered_url = page.url
                navigation.data["final_url"] = rendered_url
                metrics = page.evaluate(_METRICS_JS)
                computed_backgrounds = page.evaluate(_BACKGROUND_IMAGES_JS)
                if limitations:
                    if _RENDER_CANCELLED in limitations:
                        return {
                            "ok": False,
                            "url": target,
                            "reason": "render_cancelled",
                            "error": _RENDER_CANCELLED,
                            **transport_info,
                        }
                    raise RuntimeError("; ".join(limitations))
                if size["width"] <= 600 or is_mobile:
                    from seohead.checks.mobile_layout import measure_mobile_layout

                    transport_info["mobile_layout"] = measure_mobile_layout(page)
                    if wait_reached != wait:
                        transport_info["mobile_layout"] = {
                            "state": "unavailable",
                            "reason": "requested navigation milestone was not reached",
                        }
                asset_evidence.update(_render_asset_evidence(page, coverage, weight))
                render_completed = wait_reached == wait
            finally:
                asset_evidence["weight"] = weight.finish()
                if coverage is not None:
                    coverage.close()
                if context is not None:
                    try:
                        context.close()
                    finally:
                        browser.close()
    except BrowserTransportError as exc:
        return {
            "ok": False,
            "reason": exc.code,
            "error": str(exc),
            "url": target,
            **transport_info,
        }
    except Exception as exc:
        return {
            "ok": False,
            **({"reason": "remote_render_failed"} if endpoint is not None else {}),
            "error": (
                "Remote browser rendering failed after connection"
                if endpoint is not None
                else f"Browser rendering failed: {_error_summary(exc)}"
            ),
            "url": target,
            "viewport": viewport,
            "viewport_size": size,
            "user_agent": selected_user_agent,
            "raw": _snapshot(raw_html, final_url, "raw"),
            **transport_info,
        }
    finally:
        navigation.finish(success=render_completed)
        transport_info["network_evidence"] = network_evidence.finish()
        if browser_client is not None:
            browser_client.close()

    raw = _snapshot(raw_html, final_url, "raw")
    rendered = _snapshot(rendered_html, rendered_url, "rendered")
    # Merge in what only getComputedStyle can see: a background-image an
    # external stylesheet declares, absent from both HTML strings above.
    rendered["images"] = sorted(set(rendered["images"]) | set(computed_backgrounds))
    # Read from the raw response, before anything is decided about the render:
    # this answer holds whether or not the browser finished, which is why the
    # incomplete return below carries the key rather than omitting it (#642).
    shell = detect_empty_shell(raw_html)
    # An unfinished render is an unmeasured page, not a broken site: report it
    # the way every other unavailable measurement here is reported -- ok: False
    # with a named reason -- and emit no findings from it at all (#623). Both
    # snapshots ride along so the reason can be checked rather than believed.
    incomplete = incomplete_render_reason(raw, rendered)
    if incomplete:
        return {
            "ok": False,
            "url": target,
            "final_url": final_url,
            "status": status,
            "viewport": viewport,
            "viewport_size": size,
            "user_agent": selected_user_agent,
            "reason": INCOMPLETE_RENDER_CODE,
            "error": RENDER_UNAVAILABLE.format(reason=incomplete),
            "raw": raw,
            "rendered": rendered,
            "element_diff_state": "unavailable",
            "element_diff_reason": incomplete,
            "element_diffs": [],
            "empty_shell": shell,
            "wait": wait,
            "wait_reached": wait_reached,
            "settle_ms": settle_ms,
            # Neither True nor False: this run does not know.
            "js_dependent": None,
            "metrics_lab": metrics,
            **transport_info,
        }
    findings = compare(raw, rendered, raw_html, shell)
    from seohead.checks.element_diff import classify_diffs, diff_elements

    element_diffs = diff_elements(raw, rendered, rendered_ok=wait_reached == wait)
    element_rules = [
        {"rule": rule, "diff": dict(item)} for rule, item in classify_diffs(element_diffs)
    ]
    # Keep the summary aligned with findings: five widget words do not make a
    # page JavaScript-dependent, while findings use a 30% materiality threshold.
    js_dependent: bool | None = findings != [ALL_CLEAR]
    if wait_reached != wait:
        # _capture_dom() fell back to an earlier milestone. When scripts had not
        # run by then the rendered DOM equals the raw HTML, compare() fires on
        # nothing and ALL_CLEAR asserts that JavaScript does not determine this
        # page's content -- an affirmative verdict on a page nobody rendered.
        # The miss goes into the findings list because that list is what the
        # audit consumes, and it replaces the all-clear rather than joining it.
        findings = [MILESTONE_MISSED.format(wait=wait, reached=wait_reached)] + [
            f for f in findings if f != ALL_CLEAR
        ]
        # A difference that was found is still a difference; the absence of one
        # is not, so it stops being False and becomes "this run does not know".
        js_dependent = True if js_dependent else None
    # Its own report section, not merged into "findings": #21's compare()
    # assumes the site changed between two runs, this assumes the site is the
    # same and the method differs, so it gets its own schema/keys (dualcrawl.v1).
    dual_crawl = dualcrawl.compare_evidence(
        {final_url: {"images": set(raw["images"]), "links": _links(raw_html, final_url)}},
        {
            final_url: {
                "images": set(rendered["images"]),
                "links": _links(rendered_html, rendered_url),
            }
        },
        method_a="static",
        method_b="rendered",
    )
    return {
        "ok": True,
        "url": target,
        "final_url": final_url,
        "status": status,
        "viewport": viewport,
        "viewport_size": size,
        # Both snapshots were requested under this identity (#199) -- recorded so a report
        # can show its comparison is not confounded by a server that varies its response by
        # User-Agent, rather than leaving that an unstated assumption.
        "user_agent": selected_user_agent,
        "raw": raw,
        "rendered": rendered,
        "element_diff_state": "complete" if wait_reached == wait else "unavailable",
        "element_diff_reason": "" if wait_reached == wait else "requested navigation milestone was not reached",
        "element_diffs": element_diffs,
        "element_rules": element_rules,
        "empty_shell": shell,
        # Which milestone the rendered snapshot actually came from: a site with
        # long-polling scripts never reaches networkidle, and a report should be
        # able to say the DOM was read at domcontentloaded instead of assuming
        # the requested milestone was the one that happened.
        "wait": wait,
        "wait_reached": wait_reached,
        "settle_ms": settle_ms,
        "js_dependent": js_dependent,
        # Laboratory, not field data: one run from one machine. Field Core Web
        # Vitals come from CrUX and must not be inferred from this measurement.
        "metrics_lab": metrics,
        "asset_evidence": asset_evidence,
        "image_sizing": (asset_evidence.get("layout") or {}).get(
            "image_sizing", {"oversized": [], "upscaled": []}
        ),
        "findings": findings,
        "dual_crawl": dual_crawl,
        **transport_info,
    }


def rendered_html(
    url: str,
    timeout: float = 30.0,
    wait: str = "load",
    *,
    request_gate: Callable[[], None] | None = None,
    transport_config: dict[str, str] | None = None,
    proxy_route=None,
) -> dict[str, Any]:
    """Return rendered HTML for tools that require the final DOM.

    A separate narrow function lets regional and similar audits request one HTML
    document without constructing the full raw-versus-rendered comparison report.
    """
    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        remote = (transport_config or {}).get("transport") == "remote"
        return {
            "ok": False,
            "error": "Playwright is required",
            "install": (
                "pip install 'seohead[render]'"
                if remote
                else "pip install 'seohead[render]' && python -m playwright install chromium"
            ),
        }
    target = normalize_url(str(url or "").strip())
    if not target:
        return {"ok": False, "error": "URL is required"}
    try:
        validate_url(target)
    except ValueError as exc:
        return {"ok": False, "url": target, "error": str(exc)}
    try:
        endpoint, transport_facts = prepare(transport_config)
        if endpoint is None:
            _refuse_if_root()
    except BrowserTransportError as exc:
        return {"ok": False, "url": target, "reason": exc.code, "error": str(exc)}
    except RuntimeError as exc:
        return {"ok": False, "url": target, "error": str(exc)}
    browser_client = None
    try:
        from seohead.recon.net import crawl_transport_options

        browser_client, _http2 = http_client(
            timeout,
            follow_redirects=False,
            headers={"User-Agent": UA},
            **crawl_transport_options(proxy_route),
        )
        with sync_playwright() as pw:
            browser = open_browser(
                pw.chromium,
                endpoint,
                timeout_seconds=timeout,
                local_launch_options=(
                    _local_chromium_launch_options()
                    if endpoint is None
                    else {"chromium_sandbox": True}
                ),
            )
            context = None
            try:
                context = open_context(
                    browser, endpoint, {"service_workers": "block", "user_agent": UA}
                )
                try:
                    route_handler, limitations = _pinned_browser_route(
                        browser_client, request_gate=request_gate
                    )
                    context.route("**/*", route_handler)
                    context.route_web_socket(
                        "**/*", lambda ws_route: _guard_websocket_route(ws_route, limitations)
                    )
                    page = context.new_page()
                    page.goto(target, wait_until=wait, timeout=timeout * 1000)
                    if limitations:
                        if _RENDER_CANCELLED in limitations:
                            return {
                                "ok": False,
                                "url": target,
                                "reason": "render_cancelled",
                                "error": _RENDER_CANCELLED,
                            }
                        raise RuntimeError("; ".join(limitations))
                    html = page.content()
                    result = {
                        "ok": True,
                        "url": page.url,
                        "representation": "rendered",
                        "html": html,
                        "dom_bytes": len(html.encode("utf-8")),
                        "line_count": html.count("\n") + (1 if html else 0),
                    }
                    if endpoint is not None:
                        result["browser_transport"] = transport_facts
                    return result
                finally:
                    context.close()
            finally:
                if context is not None:
                    browser.close()
    except BrowserTransportError as exc:
        return {"ok": False, "url": target, "reason": exc.code, "error": str(exc)}
    except Exception as exc:
        error = (
            "Remote browser rendering failed after connection"
            if endpoint is not None
            else _error_summary(exc)
        )
        return {
            "ok": False,
            "error": error,
            "url": target,
            **({"reason": "remote_render_failed"} if endpoint is not None else {}),
        }
    finally:
        if browser_client is not None:
            browser_client.close()


# Merges every open shadow root's light-DOM-visible children into its host
# element so page.content() -- which only ever serializes light DOM -- carries
# what a search engine's own DOM flattening would see. Closed shadow roots are
# unreachable from page script at all and are left untouched, same as for any
# renderer.
_FLATTEN_SHADOW_DOM_JS = """() => {
  let flattened = 0;
  const walk = (root) => {
    root.querySelectorAll('*').forEach((el) => {
      if (el.shadowRoot) {
        walk(el.shadowRoot);
        el.append(...Array.from(el.shadowRoot.childNodes));
        flattened += 1;
      }
    });
  };
  walk(document);
  return flattened;
}"""

# Replaces each same-origin iframe with its own document's body content,
# matching how a search engine assembles one page out of same-origin frames.
# A cross-origin frame throws on contentDocument access and is left as an
# empty frame -- exactly what a non-rendering crawler could see too, so
# nothing is invented in its place.
_FLATTEN_IFRAMES_JS = """() => {
  let flattened = 0;
  document.querySelectorAll('iframe').forEach((frame) => {
    try {
      const doc = frame.contentDocument;
      if (doc && doc.body) {
        const div = document.createElement('div');
        div.setAttribute('data-flattened-iframe', frame.src || '');
        div.innerHTML = doc.body.innerHTML;
        frame.replaceWith(div);
        flattened += 1;
      }
    } catch (e) {
      // Cross-origin: not reachable from page script, left as-is.
    }
  });
  return flattened;
}"""


def _bounded_dom_script(max_html_bytes: int | None) -> str:
    limit = "null" if max_html_bytes is None else str(max_html_bytes)
    return f"""() => {{
      const html = document.documentElement.outerHTML;
      const bytes = new TextEncoder().encode(html).byteLength;
      const limit = {limit};
      if (limit !== null && bytes > limit) return {{complete: false, bytes}};
      return {{complete: true, bytes, html}};
    }}"""


def _safe_policy_facts(policy_facts: dict[str, Any] | None) -> dict[str, bool]:
    """Keep retention policy facts without copying headers, paths, or credentials."""
    facts = policy_facts or {}
    return {
        "credentials_used": bool(facts.get("credentials_used")),
        "cache_control_no_store": bool(facts.get("cache_control_no_store")),
    }


def _local_chromium_launch_options() -> dict[str, Any]:
    """Keep Chromium sandboxed and optionally select an installed local browser.

    ``SEOHEAD_CHROME`` is an operator-controlled executable override. It applies
    only to local Chromium launches; remote transport and other browser engines
    do not consume it. An explicit but invalid path is an error rather than a
    silent fallback to a different browser binary.
    """
    options: dict[str, Any] = {"chromium_sandbox": True}
    executable = os.environ.get("SEOHEAD_CHROME")
    if executable:
        path = Path(executable).expanduser()
        if not path.is_file() or not os.access(path, os.X_OK):
            raise RuntimeError("SEOHEAD_CHROME must point to an executable local Chrome binary")
        options["executable_path"] = str(path.resolve())
    return options


def render_document(
    url: str,
    rendering_config: dict[str, Any],
    *,
    nav_timeout: float = 30.0,
    artifacts_dir: str | None = None,
    user_agent: str = "",
    max_html_bytes: int | None = None,
    policy_facts: dict[str, Any] | None = None,
    request_gate: Callable[[], None] | None = None,
    proxy_route=None,
) -> dict[str, Any]:
    """Render one URL under the full crawler rendering configuration.

    ``rendering_config`` is the resolved ``rendering`` block from
    ``seohead.crawl.settings`` (its ``browser`` and ``artifacts`` sub-dicts),
    not a browser handle -- which is exactly what lets a test replace this
    whole function with a stub for ``seohead.crawl.render_escalation``,
    never needing a real browser or the network.

    Unlike ``render_check`` (one fixed comparison for the single-page tool),
    this is what selective escalation calls for every page it decides to
    re-fetch, so every setting #18 asked for is honoured: script timeout
    (how long JavaScript may keep running after load), viewport,
    resize-to-content with its cap, shadow-DOM and iframe flattening, device
    pixel ratio, mobile/touch emulation, page-load strategy, a persistent
    profile that stays off unless a directory is explicitly named, and the
    headless engine the crawl configuration selected.
    """
    target = normalize_url(str(url or "").strip())
    if not target:
        return {"ok": False, "error": "URL is required"}
    browser_cfg = rendering_config.get("browser", {})
    artifacts_cfg = rendering_config.get("artifacts", {})
    # Settings validation rejects an unknown engine before a crawl reaches
    # here; the same check keeps a direct call honest too, and is deliberately
    # distinct from a missing Playwright or a missing browser binary below.
    engine = str(browser_cfg.get("engine") or "chromium")
    capability = _engine_capability_error(engine, browser_cfg)
    if capability:
        return {"ok": False, "url": target, "error": capability}
    try:
        viewport = resolve_viewport(browser_cfg)
    except ValueError as exc:
        return {"ok": False, "url": target, "error": str(exc)}
    if browser_cfg.get("persistent_profile"):
        return {
            "ok": False,
            "url": target,
            "error": "persistent browser profiles are unavailable with pinned rendering until cookie continuity is verified",
        }
    if max_html_bytes is not None and (type(max_html_bytes) is not int or max_html_bytes < 0):
        return {
            "ok": False,
            "url": target,
            "error": "max_html_bytes must be a non-negative integer",
        }
    try:
        validate_url(target)
    except ValueError as exc:
        return {"ok": False, "url": target, "error": str(exc)}
    try:
        endpoint, transport_facts = prepare(browser_cfg, embedded=True)
        if endpoint is None:
            _refuse_if_root()
    except BrowserTransportError as exc:
        return {"ok": False, "url": target, "reason": exc.code, "error": str(exc)}
    except RuntimeError as exc:
        return {"ok": False, "url": target, "error": str(exc)}
    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        remote = browser_cfg.get("transport") == "remote"
        return {
            "ok": False,
            "error": "Playwright is required",
            "install": (
                "pip install 'seohead[render]'"
                if remote
                else f"pip install 'seohead[render]' && python -m playwright install {engine}"
            ),
        }

    console_errors: list[str] = []
    console_errors_omitted = 0
    screenshot_path: str | None = None
    screenshot_state = "disabled"
    screenshot_error = ""
    shadow_flattened = 0
    iframe_flattened = 0
    observed_policy = _safe_policy_facts(policy_facts)
    observed_policy["credentials_used"] |= bool(browser_cfg.get("persistent_profile"))
    # Built before launch so a failure -- a timeout, an aborted navigation,
    # a missing binary -- still reports the requested engine and the effective
    # viewport it was attempted under, without pretending a render happened.
    renderer: dict[str, Any] = {
        "engine": f"playwright-{engine}",
        "engine_version": "unknown",
        "navigation": {},
        "settings": {
            "viewport": viewport,
            "user_agent": user_agent or UA,
            "device_pixel_ratio": float(browser_cfg.get("device_pixel_ratio", 1.0) or 1.0),
            "mobile_emulation": bool(browser_cfg.get("mobile_emulation")),
            "touch_emulation": bool(browser_cfg.get("touch_emulation")),
            "script_timeout_seconds": float(browser_cfg.get("script_timeout_seconds", 0) or 0),
            "resize_to_content": bool(browser_cfg.get("resize_to_content")),
            "resize_to_content_max_height_px": int(
                browser_cfg.get("resize_to_content_max_height_px", 15000)
            ),
            "persistent_profile": bool(browser_cfg.get("persistent_profile")),
            "screenshot_mode": str(artifacts_cfg.get("screenshot_mode", "full_page")),
        },
        "transforms": {
            "flatten_shadow_dom_requested": bool(browser_cfg.get("flatten_shadow_dom")),
            "flatten_shadow_dom_applied": 0,
            "flatten_iframes_requested": bool(browser_cfg.get("flatten_iframes")),
            "flatten_iframes_applied": 0,
        },
        "policy": observed_policy,
        "console_error_count": 0,
        # The run's render-page bound, recorded per page so a stored render
        # can be read against the scheduling it was produced under.
        "page_concurrency": int(browser_cfg.get("page_concurrency") or 1),
    }
    if endpoint is not None:
        renderer["transport"] = transport_facts
    browser_limitations: list[str] = []
    weight = _BrowserWeight(target)
    network_evidence = _BrowserNetworkEvidence()
    renderer["network_evidence"] = network_evidence.finish()
    renderer["settings"]["asset_evidence"] = {"source": "native-render", "weight": weight.finish()}
    coverage = None
    navigation = NavigationCapture(target, browser_cfg.get("wait_until", "load"), nav_timeout)
    renderer["navigation"] = navigation.data
    render_completed = False

    # The weight observer counts request starts without reading wire headers.
    # Reading those headers upgraded credentials_used the moment any request
    # carried Cookie:, and a browser carries back whatever the site's own
    # Set-Cookie gave it -- so a page that sets a session cookie and then asks for
    # one same-origin subresource, which is the ordinary shape of the web, had its
    # serialized DOM stored as credentialed on a run with nothing configured at all
    # (#656, the rendering lane's half of #647). What this run was configured to
    # send is already what sqlite_render._policy_facts derives and passes in as
    # policy_facts -- http.credential_headers, or a persistent browser profile --
    # and the wire has nothing to add to it: this renderer builds its own network
    # client with no credential of its own, so a sensitive header can only reach a
    # request through one of those two, or from the site itself.

    def _capture_response(response: Any) -> None:
        if max_html_bytes is None:
            return
        from seohead.crawl.cache import parse_cache_control

        if "no-store" in parse_cache_control(response.all_headers().get("cache-control", "")):
            observed_policy["cache_control_no_store"] = True

    def _on_console(msg: Any) -> None:
        nonlocal console_errors_omitted
        if artifacts_cfg.get("console_errors") and msg.type == "error":
            if len(console_errors) < MAX_CONSOLE_ERRORS:
                console_errors.append(_redact_console(msg.text))
            else:
                console_errors_omitted += 1

    browser = None
    network_client = None
    try:
        from seohead.recon.net import crawl_transport_options

        network_client, _http2 = http_client(
            nav_timeout,
            follow_redirects=False,
            headers={"User-Agent": user_agent or UA},
            **crawl_transport_options(proxy_route),
        )
        with sync_playwright() as pw:
            context_options: dict[str, Any] = {
                "viewport": viewport,
                # The same identity the static crawl presented, for the same
                # reason #199 pinned it on the single-page probe: this fetch
                # replaces a page's body-derived evidence, so it must ask the
                # origin as the client the rest of the crawl was. Left to
                # Chromium's own default it advertises HeadlessChrome, which
                # bot protection commonly challenges, and a report then mixes
                # two populations -- escalated pages described from what the
                # site serves a headless browser, every other page from what
                # it serves the toolkit.
                "user_agent": user_agent or UA,
                "device_scale_factor": float(browser_cfg.get("device_pixel_ratio", 1.0) or 1.0),
                # Blocks the default-configuration bypass named in #18's
                # security section: a service worker can otherwise answer
                # requests page.route() never sees.
                "service_workers": "block",
            }
            # Passed only when set: engines without device emulation
            # (Firefox) reject the keys outright rather than ignore them.
            if browser_cfg.get("mobile_emulation"):
                context_options["is_mobile"] = True
            if browser_cfg.get("touch_emulation"):
                context_options["has_touch"] = True
            try:
                browser = open_browser(
                    getattr(pw, engine),
                    endpoint,
                    timeout_seconds=nav_timeout,
                    local_launch_options=(
                        {
                            **dict(BROWSER_ENGINES[engine]["launch_options"]),
                            **_local_chromium_launch_options(),
                        }
                        if endpoint is None and engine == "chromium"
                        else dict(BROWSER_ENGINES[engine]["launch_options"])
                    ),
                )
            except Exception as exc:
                if endpoint is None and _missing_browser_binary(exc):
                    return {
                        "ok": False,
                        "url": target,
                        "error": (
                            f"the {engine!r} browser binary is not installed for this "
                            f"Playwright ({type(exc).__name__})"
                        ),
                        "install": f"python -m playwright install {engine}",
                        "renderer": renderer,
                    }
                raise
            context = open_context(browser, endpoint, context_options)
            actual_browser = browser if browser is not None else getattr(context, "browser", None)
            renderer["engine_version"] = str(getattr(actual_browser, "version", "unknown"))
            try:
                action_state = {"active": False}
                route_handler, browser_limitations = _pinned_browser_route(
                    network_client,
                    request_gate=request_gate,
                    weight_observer=weight,
                    network_observer=network_evidence,
                    action_phase=lambda: action_state["active"],
                )
                network_evidence.begin()
                context.route("**/*", route_handler)
                context.route_web_socket(
                    "**/*",
                    lambda ws_route: _guard_websocket_route(ws_route, browser_limitations, weight),
                )
                page = context.new_page()
                page.on("request", weight.started)
                navigation.attach(context, page)
                from seohead.checks.browser_coverage import BrowserCoverage

                coverage = BrowserCoverage(page, engine)
                if max_html_bytes is not None:
                    page.on("response", _capture_response)
                page.on("console", _on_console)
                page.goto(
                    target,
                    wait_until=browser_cfg.get("wait_until", "load"),
                    timeout=nav_timeout * 1000,
                )
                script_timeout = float(browser_cfg.get("script_timeout_seconds", 0) or 0)
                if script_timeout > 0:
                    page.wait_for_timeout(script_timeout * 1000)
                action_cfg = rendering_config.get("actions", [])
                preset_cfg = rendering_config.get("presets", [])
                if action_cfg or preset_cfg:
                    if engine != "chromium":
                        renderer["page_actions"] = {
                            "schema": "page_actions.v1",
                            "state": "unavailable",
                            "reason": "bounded page actions require Chromium CDP runtime deadlines",
                        }
                    else:
                        action_state["active"] = True
                        action_session = None
                        try:
                            from seohead.checks.page_actions import run_actions

                            evaluate_action, action_session = _bounded_action_evaluator(
                                context, page
                            )
                            renderer["page_actions"] = run_actions(
                                page,
                                action_cfg,
                                preset_cfg,
                                evaluate=evaluate_action,
                            )
                        except Exception as exc:
                            renderer["page_actions"] = {
                                "schema": "page_actions.v1",
                                "state": "unavailable",
                                "reason": type(exc).__name__,
                            }
                        finally:
                            if action_session is not None:
                                with contextlib.suppress(Exception):
                                    action_session.detach()
                if browser_cfg.get("resize_to_content"):
                    cap = int(browser_cfg.get("resize_to_content_max_height_px", 15000))
                    content_height = int(page.evaluate("document.documentElement.scrollHeight"))
                    page.set_viewport_size(
                        {"width": viewport["width"], "height": max(min(content_height, cap), 1)}
                    )
                if browser_cfg.get("flatten_shadow_dom"):
                    shadow_flattened = int(page.evaluate(_FLATTEN_SHADOW_DOM_JS) or 0)
                if browser_cfg.get("flatten_iframes"):
                    iframe_flattened = int(page.evaluate(_FLATTEN_IFRAMES_JS) or 0)
                if viewport["width"] <= 600 or browser_cfg.get("mobile_emulation"):
                    from seohead.checks.mobile_layout import measure_mobile_layout

                    renderer["mobile_layout"] = measure_mobile_layout(page)
                if max_html_bytes is None:
                    html = page.content()
                    dom = {"complete": True, "bytes": len(html.encode("utf-8")), "html": html}
                else:
                    dom = page.evaluate(_bounded_dom_script(max_html_bytes))
                final_url = page.url
                navigation.data["final_url"] = final_url
                if artifacts_cfg.get("screenshots") and artifacts_dir:
                    staged = _staged_screenshot_path(artifacts_dir, target)
                    try:
                        page.screenshot(
                            path=staged,
                            full_page=artifacts_cfg.get("screenshot_mode", "full_page")
                            == "full_page",
                        )
                        screenshot_path = staged
                        screenshot_state = "staged"
                    except Exception as exc:
                        screenshot_state = "unavailable"
                        screenshot_error = (
                            "browser screenshot failed"
                            if current_remote_policy() is not None
                            else _redact_console(f"{type(exc).__name__}: {exc}")
                        )
                        with contextlib.suppress(OSError):
                            os.unlink(staged)
                elif artifacts_cfg.get("screenshots"):
                    screenshot_state = "unavailable"
                    screenshot_error = "browser artifact staging directory was not supplied"
                if browser_limitations:
                    if _RENDER_CANCELLED in browser_limitations:
                        return {
                            "ok": False,
                            "url": target,
                            "reason": "render_cancelled",
                            "error": _RENDER_CANCELLED,
                            "renderer": renderer,
                        }
                    raise RuntimeError("; ".join(browser_limitations))
                renderer["settings"]["asset_evidence"] = _render_asset_evidence(
                    page, coverage, weight
                )
                render_completed = True
            finally:
                renderer["settings"]["asset_evidence"]["weight"] = weight.finish()
                if coverage is not None:
                    coverage.close()
                try:
                    context.close()
                finally:
                    if browser is not None:
                        browser.close()
    except BrowserTransportError as exc:
        return {
            "ok": False,
            "url": target,
            "reason": exc.code,
            "error": str(exc),
            "renderer": renderer,
        }
    except Exception as exc:
        error = (
            "Remote browser rendering failed after connection"
            if endpoint is not None
            else _error_summary(exc)
        )
        return {
            "ok": False,
            "url": target,
            "error": error,
            "renderer": renderer,
            **({"reason": "remote_render_failed"} if endpoint is not None else {}),
        }
    finally:
        navigation.finish(success=render_completed)
        renderer["network_evidence"] = network_evidence.finish()
        if network_client is not None:
            network_client.close()

    renderer["navigation"]["final_url"] = final_url
    renderer["transforms"]["flatten_shadow_dom_applied"] = shadow_flattened
    renderer["transforms"]["flatten_iframes_applied"] = iframe_flattened
    renderer["console_error_count"] = len(console_errors) + console_errors_omitted
    if not isinstance(dom, dict) or not dom.get("complete"):
        return {
            "ok": False,
            "url": target,
            "final_url": final_url,
            "dom_state": "truncated",
            "dom_bytes": (dom or {}).get("bytes") if isinstance(dom, dict) else None,
            "renderer": renderer,
            "error": "serialized DOM exceeds max_html_bytes",
        }

    return {
        "ok": True,
        "url": target,
        "final_url": final_url,
        "representation": "rendered",
        "html": dom["html"],
        "dom_bytes": dom.get("bytes"),
        "line_count": dom["html"].count("\n") + (1 if dom["html"] else 0),
        "dom_state": "complete",
        "renderer": renderer,
        "console_errors": console_errors,
        "console_errors_omitted": console_errors_omitted,
        "screenshot_path": screenshot_path,
        "screenshot_state": screenshot_state,
        "screenshot_error": screenshot_error,
    }
