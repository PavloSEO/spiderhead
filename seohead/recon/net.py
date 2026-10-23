"""Shared network layer: URL guardrails, DNS-over-HTTPS, and RDAP.

Every user-controlled HTTP request should use :func:`http_client`. Its transport
(see ``_PinningTransport`` below) resolves and validates every hop — the first
request and every redirect — and then connects to the literal address it just
validated, never to a second, independent resolution of the hostname. Before
this, only ``request``/``response`` event hooks ran the *check*; the *socket*
was opened by httpx's own DNS lookup, a separate call a hostile resolver could
answer differently (issue #142). Private, loopback, link-local, multicast,
reserved, and otherwise non-global addresses are blocked by default. Authorized
staging and intranet work requires an explicit ``SEOHEAD_ALLOW_PRIVATE_NETWORKS=1``
opt-in — that opens every private range, for a run that genuinely needs it.
``SEOHEAD_ALLOW_PRIVATE_HOSTS`` is the scoped alternative: a comma-separated list
of exact hostnames (e.g. one staging box) allowed to resolve privately without
opening the rest of RFC 1918 space.

DNS and registration checks use HTTP APIs because ``dig`` and ``whois`` are not
available in every container. A system ``whois`` binary remains an optional ccTLD
fallback. Network failures are returned as unavailable data rather than escaping
as fatal exceptions.
"""

from __future__ import annotations

import ipaddress
import os
import re
import shutil
import socket
import subprocess
from contextvars import ContextVar
from dataclasses import dataclass, field
from typing import Any
from urllib.parse import unquote, urljoin, urlsplit, urlunsplit

from seohead.recon.remote_policy import RemoteTargetError, current_remote_policy

UA = "Mozilla/5.0 (compatible; SEOHEAD-Tools/3.0; +https://seohead.tech/seotools)"
PRIVATE_NETWORK_ENV = "SEOHEAD_ALLOW_PRIVATE_NETWORKS"
PRIVATE_HOST_ALLOWLIST_ENV = "SEOHEAD_ALLOW_PRIVATE_HOSTS"
_PROXY_ENV_REF = re.compile(r"^env:([A-Za-z_][A-Za-z0-9_]*)$")
_PROXY_TLS_IDENTITY: ContextVar[tuple[str, str] | None] = ContextVar(
    "seohead_proxy_tls_identity", default=None
)

DOH_ENDPOINTS = (
    "https://cloudflare-dns.com/dns-query",
    "https://dns.google/resolve",
)
RDAP_BOOTSTRAP = "https://rdap.org"

# Result state a fetch carries when the edge refused the client instead of the
# site answering (issue #1363). Detection and honest reporting only: this
# toolkit never spoofs identifiers, proxies requests, or solves challenges to
# get past bot protection. The operator's paths are an allowlist for the
# crawler or auditing a supplied export.
BLOCKED_BY_WAF = "blocked_by_waf"

# ``cf-mitigated`` values that mean Cloudflare itself answered with a block.
_CF_MITIGATED_STATES = frozenset({"challenge", "block", "managed"})

# Stable literals a challenge body carries and a normal error page does not.
_CHALLENGE_BODY_MARKERS = (
    "cdn-cgi/challenge-platform",
    "cf_chl_",  # challenge bootstrap options (cf_chl_opt and friends)
    "cf-browser-verification",
    "attention required! | cloudflare",
    "just a moment...",
    "checking your browser before accessing",
    "enable javascript and cookies to continue",
)

# Only this much of a body is inspected: every marker sits near the top of a
# challenge document, and reading megabytes to classify a block is waste.
_CHALLENGE_SNIFF_BYTES = 8192


def waf_block_reason(response: Any) -> str | None:
    """Classify a 403/429 response as a bot-protection block, or return ``None``.

    Returns a short reason string (``"cf-mitigated: challenge"``,
    ``"cloudflare edge"``, ``"challenge body: just a moment..."``) when the
    status is 403/429 and the response carries WAF markers — the
    ``cf-mitigated`` header, a Cloudflare edge ``server`` header, or a known
    challenge body. A plain 403 from the origin stays a regular error, and a
    2xx response is never a block. Callers record the reason on the fetch
    result as ``blocked_by_waf`` and report it instead of auditing the block
    page body; see :data:`BLOCKED_BY_WAF`.
    """
    status = getattr(response, "status_code", None)
    if status not in (403, 429):
        return None
    headers = getattr(response, "headers", None) or {}
    mitigated = str(headers.get("cf-mitigated", "")).strip().lower()
    if mitigated in _CF_MITIGATED_STATES:
        return f"cf-mitigated: {mitigated}"
    if str(headers.get("server", "")).strip().lower() == "cloudflare":
        return "cloudflare edge"
    try:
        # A streamed response that was never read has no body to inspect; that
        # is "no body evidence", not "not blocked" — but the two header checks
        # above already covered every case where headers prove the block.
        snippet = str(response.text or "")[:_CHALLENGE_SNIFF_BYTES].lower()
    except Exception:
        snippet = ""
    for marker in _CHALLENGE_BODY_MARKERS:
        if marker in snippet:
            return f"challenge body: {marker}"
    return None


_DOMAIN_RE = re.compile(
    r"^(?=.{1,253}$)(?!-)[a-z0-9-]{1,63}(?<!-)(\.(?!-)[a-z0-9-]{1,63}(?<!-))*"
    r"\.(?!-)[a-z0-9-]*[a-z][a-z0-9-]*(?<!-)$"
)


class NetworkUnavailable(RuntimeError):
    """Raised internally when the base HTTP client is unavailable."""


@dataclass(frozen=True)
class ProxyRoute:
    """One vetted proxy socket and a shareable identity without credentials."""

    proxy: Any = field(repr=False)
    identity: str
    authenticated: bool


class _ProxyTLSStream:
    """Keep CONNECT's vetted IP while verifying TLS against the original host."""

    def __init__(self, stream: Any) -> None:
        self._stream = stream

    def read(self, max_bytes: int, timeout: float | None = None) -> bytes:
        return self._stream.read(max_bytes, timeout=timeout)

    def write(self, buffer: bytes, timeout: float | None = None) -> None:
        self._stream.write(buffer, timeout=timeout)

    def close(self) -> None:
        self._stream.close()

    def get_extra_info(self, info: str) -> Any:
        return self._stream.get_extra_info(info)

    def start_tls(self, ssl_context: Any, server_hostname: str | None = None, timeout=None):
        identity = _PROXY_TLS_IDENTITY.get()
        if identity is None or server_hostname != identity[0]:
            raise NetworkUnavailable("proxied TLS identity is unavailable")
        return self._stream.start_tls(
            ssl_context=ssl_context, server_hostname=identity[1], timeout=timeout
        )


class _ProxyTLSBackend:
    """Wrap only an explicit HTTP proxy's tunnel stream; leave direct clients alone."""

    def __init__(self, backend: Any) -> None:
        self._backend = backend

    def connect_tcp(self, *args: Any, **kwargs: Any) -> _ProxyTLSStream:
        return _ProxyTLSStream(self._backend.connect_tcp(*args, **kwargs))

    def connect_unix_socket(self, *args: Any, **kwargs: Any):
        return self._backend.connect_unix_socket(*args, **kwargs)

    def sleep(self, seconds: float) -> None:
        self._backend.sleep(seconds)


def resolve_proxy_route(value: str, *, allow_private: bool = False) -> ProxyRoute | None:
    """Resolve and pin an explicitly configured HTTP forward proxy.

    The proxy's private-address opt-in is independent of the target allowlist.
    Target URLs are still validated and pinned by ``_PinningTransport``.
    """
    if value == "":
        return None
    if not isinstance(value, str):
        raise ValueError("http.proxy must be a URL or env:VARIABLE reference")
    reference = _PROXY_ENV_REF.fullmatch(value)
    from_env = reference is not None
    if value.startswith("env:") and not from_env:
        raise ValueError("http.proxy needs a valid env:VARIABLE reference")
    raw = os.environ.get(reference.group(1), "") if reference else value
    if not raw:
        raise ValueError("http.proxy environment reference is missing or empty")
    if any(ord(char) < 33 or ord(char) == 127 for char in raw):
        raise ValueError("http.proxy URL contains whitespace or control characters")
    try:
        parts = urlsplit(raw)
        host = parts.hostname
        port = parts.port
    except ValueError:
        raise ValueError("http.proxy URL is malformed") from None
    if parts.scheme != "http":
        raise ValueError("http.proxy supports only an http:// forward proxy")
    if not host or port is None or not 1 <= port <= 65535:
        raise ValueError("http.proxy requires a host and explicit port")
    if parts.path not in ("", "/") or parts.query or parts.fragment:
        raise ValueError("http.proxy URL cannot have a path, query, or fragment")
    if parts.username is not None or parts.password is not None:
        if not from_env:
            raise ValueError("http.proxy inline authentication is forbidden; use env:VARIABLE")
        if not parts.username or not parts.password:
            raise ValueError("http.proxy authentication needs both username and password")
        if re.search(r"%(?![0-9A-Fa-f]{2})", parts.username + parts.password):
            raise ValueError("http.proxy authentication has invalid percent encoding")
        auth = (unquote(parts.username), unquote(parts.password))
        if any(ord(char) < 33 or ord(char) == 127 for item in auth for char in item):
            raise ValueError("http.proxy authentication contains control characters")
        if ":" in auth[0]:
            raise ValueError("http.proxy username cannot contain a colon")
    else:
        auth = None
    try:
        records = socket.getaddrinfo(host, port, type=socket.SOCK_STREAM)
    except socket.gaierror:
        raise ValueError("http.proxy host could not be resolved safely") from None
    if not records:
        raise ValueError("http.proxy host could not be resolved safely")
    if not allow_private and any(not _is_public_address(item[4][0]) for item in records):
        raise ValueError(
            "http.proxy resolves to a private address; set http.proxy_allow_private=true"
        )
    address = records[0][4][0].split("%", 1)[0]
    literal = f"[{address}]" if ":" in address else address
    import httpx

    proxy = httpx.Proxy(f"http://{literal}:{port}", auth=auth)
    return ProxyRoute(
        proxy=proxy, identity=f"http://{host.lower()}:{port}", authenticated=bool(auth)
    )


def crawl_transport_options(proxy_route: ProxyRoute | None = None) -> dict[str, Any]:
    """Ignore ambient proxies while using system trust plus an explicit CA, if any.

    ``httpx.HTTPTransport`` otherwise constructs its own certifi-backed context.
    That bypasses the Windows certificate store used by a TLS-inspecting
    corporate proxy. Building the standard-library context explicitly keeps
    hostname verification and platform roots. An explicit CA augments those
    roots instead of replacing them.
    """
    import ssl

    options: dict[str, Any] = {"trust_env": False}
    cafile = os.environ.get("SSL_CERT_FILE")
    capath = os.environ.get("SSL_CERT_DIR")
    context = ssl.create_default_context()
    if cafile or capath:
        context.load_verify_locations(cafile=cafile, capath=capath)
    options["verify"] = context
    if proxy_route is not None:
        options["proxy_route"] = proxy_route
    return options


def private_networks_enabled() -> bool:
    """Return whether private-network access was explicitly enabled."""
    return os.getenv(PRIVATE_NETWORK_ENV, "").strip().lower() in {
        "1",
        "true",
        "yes",
        "on",
    }


def allowed_private_hosts() -> frozenset[str]:
    """Hostnames explicitly permitted to resolve to a non-public address.

    A scoped alternative to :data:`PRIVATE_NETWORK_ENV`: it authorizes one
    named staging host without opening every private range. Matching is exact
    on the lowercased, dot-stripped hostname — an entry does not extend to a
    subdomain, to a different host a redirect points at, or to any other
    address the same staging host might expose under a different name.
    """
    raw = os.getenv(PRIVATE_HOST_ALLOWLIST_ENV, "")
    return frozenset(host.strip().rstrip(".").lower() for host in raw.split(",") if host.strip())


def _remote_policy(policy: Any = None) -> Any:
    if policy is not None:
        return policy
    return current_remote_policy()


def client_network_policy(client: Any) -> Any:
    """Return a policy retained by a guarded client across worker threads."""
    return getattr(getattr(client, "_transport", None), "_seohead_remote_policy", None)


def _private_target_allowed(host: str, policy: Any = None) -> bool:
    """Whether ``host`` may resolve to a private or otherwise non-public address."""
    policy = _remote_policy(policy)
    if policy is not None:
        return policy.allows_private_host(host)
    return private_networks_enabled() or (host or "").rstrip(".").lower() in allowed_private_hosts()


# Ranges that carry a non-public address inside a globally-scoped one. Python's
# ``is_global`` answers a question about the address family, not about where the
# packet ends up: 64:ff9b::7f00:1 is 127.0.0.1 wrapped in the well-known NAT64
# prefix and reports is_global=True, so on any NAT64 host — common in CI and in
# mobile and cloud networks — the guard would pass a request to loopback.
_TRANSLATED_PREFIXES = tuple(
    ipaddress.ip_network(cidr)
    for cidr in (
        "64:ff9b::/96",  # RFC 6052 well-known NAT64 prefix
        "64:ff9b:1::/48",  # RFC 8215 local-use NAT64
        "2002::/16",  # 6to4, embeds an IPv4 address
        "::ffff:0:0/96",  # IPv4-mapped
        "::/96",  # IPv4-compatible, deprecated but still parsed
    )
)


def _embedded_ipv4(address: ipaddress.IPv6Address) -> ipaddress.IPv4Address | None:
    """The IPv4 address a translated IPv6 address actually reaches."""
    packed = address.packed
    if address in _TRANSLATED_PREFIXES[2]:  # 6to4 carries it in bytes 2..6
        return ipaddress.IPv4Address(packed[2:6])
    return ipaddress.IPv4Address(packed[-4:])


def _is_public_address(value: str) -> bool:
    try:
        address = ipaddress.ip_address(value.split("%", 1)[0])
    except ValueError:
        return False
    # Translated forms are checked first: the wrapper's own scope says nothing
    # about the destination, and Python scores some of them non-global and
    # others global regardless of what they carry.
    if isinstance(address, ipaddress.IPv6Address) and any(
        address in prefix for prefix in _TRANSLATED_PREFIXES
    ):
        try:
            return _embedded_ipv4(address).is_global
        except (ipaddress.AddressValueError, ValueError):
            return False
    return address.is_global


def pinned_target(url: str, *, policy: Any = None) -> tuple[str, dict[str, str], dict[str, str]]:
    """Rewrite a URL to connect to a vetted address, keeping the hostname.

    ``validate_url`` resolved DNS and then threw the answer away, so the HTTP
    client resolved a second time and connected to whatever came back. That is a
    time-of-check-to-time-of-use gap: a hostile resolver can answer the check
    with a public address and the connection with a loopback one. Since the guard
    also runs per redirect hop, it was one window per hop rather than one.

    Returns the URL to request, headers carrying the original ``Host``, and the
    request extensions carrying the hostname for SNI — so certificate
    verification still happens against the name, not the address.

    This is the primitive ``_PinningTransport`` below builds on for every other
    ``http_client()`` caller. It stays a free function too because
    ``collect.py``'s list-mode fetch already called it directly before the
    transport existed, on every retry attempt; the transport recognizes a
    request pinned this way (by its ``sni_hostname`` extension) and passes it
    through rather than pinning it a second time, which would re-resolve the
    literal address as if it were a hostname and lose the real one.
    """
    policy = _remote_policy(policy)
    parts = urlsplit(url)
    host = parts.hostname
    if not host:
        raise ValueError("no host to pin" if policy is not None else f"no host to pin in {url!r}")
    port = parts.port or (443 if parts.scheme == "https" else 80)

    addresses = resolve_socket_addresses(host, port, policy=policy)
    # Every answer above is already vetted. Prefer IPv4 when both families are
    # usable: local fixtures and IPv4-only staging servers commonly publish an
    # IPv6 loopback answer too, but do not listen on it. This is selection among
    # pinned answers, never a second DNS lookup or an unvetted fallback.
    selected = next((item for item in addresses if item[0] == socket.AF_INET), addresses[0])
    address = selected[3][0].split("%", 1)[0]
    literal = f"[{address}]" if ":" in address else address
    netloc = f"{literal}:{parts.port}" if parts.port else literal
    pinned = urlunsplit((parts.scheme, netloc, parts.path or "/", parts.query, ""))

    authority = f"{host}:{parts.port}" if parts.port else host
    return pinned, {"Host": authority}, {"sni_hostname": host}


def resolve_socket_addresses(
    host: str, port: int, *, policy: Any = None
) -> list[tuple[int, int, int, Any]]:
    """Resolve once and return vetted socket addresses for a direct connection."""
    policy = _remote_policy(policy)
    try:
        records = socket.getaddrinfo(host, port, type=socket.SOCK_STREAM)
    except socket.gaierror as exc:
        if policy is not None:
            raise ValueError("hostname could not be resolved safely") from None
        raise ValueError(f"hostname could not be resolved safely: {host}") from exc
    if not records:
        raise ValueError(
            "hostname could not be resolved safely"
            if policy is not None
            else f"hostname could not be resolved safely: {host}"
        )
    if policy is not None and len({_is_public_address(record[4][0]) for record in records}) > 1:
        raise ValueError("mixed public and private DNS answers blocked")
    if policy is not None and any(
        not _is_public_address(record[4][0])
        and not policy.allows_private_address(host, record[4][0])
        for record in records
    ):
        raise ValueError("private or non-public network target blocked")
    if (
        policy is None
        and not _private_target_allowed(host)
        and any(not _is_public_address(record[4][0]) for record in records)
    ):
        raise ValueError(
            f"private or non-public network target blocked; set {PRIVATE_NETWORK_ENV}=1 "
            f"to authorize every private target, or add {host!r} to "
            f"{PRIVATE_HOST_ALLOWLIST_ENV} to authorize only this one"
        )

    unique: list[tuple[int, int, int, Any]] = []
    seen: set[tuple[int, int, int, Any]] = set()
    for family, socktype, proto, _canonname, sockaddr in records:
        item = (family, socktype, proto, sockaddr)
        if item not in seen:
            seen.add(item)
            unique.append(item)
    return unique


def validate_url(url: str, *, policy: Any = None) -> str:
    """Validate an HTTP(S) URL and block private networks by default.

    Embedded credentials are rejected because they are easily copied into logs and
    transcripts. Hostnames are resolved before a request and every resolved address
    must be globally routable. This is a guardrail, not a network sandbox; callers
    handling hostile DNS should additionally isolate the process or container.
    """
    policy = _remote_policy(policy)
    value = str(url or "").strip()
    try:
        parsed = urlsplit(value)
    except ValueError as exc:
        if policy is not None:
            raise ValueError("invalid URL") from None
        raise ValueError(f"invalid URL: {exc}") from exc
    if parsed.scheme.lower() not in {"http", "https"}:
        raise ValueError("only http:// and https:// URLs are supported")
    if not parsed.hostname:
        raise ValueError("URL must include a hostname")
    if parsed.username is not None or parsed.password is not None:
        raise ValueError("embedded URL credentials are not supported")

    if policy is None and private_networks_enabled():
        return value

    host = parsed.hostname.rstrip(".").lower()
    if (host == "localhost" or host.endswith(".localhost")) and not _private_target_allowed(
        host, policy
    ):
        if policy is not None:
            raise ValueError("private-network target blocked")
        raise ValueError(
            f"private-network target blocked; set {PRIVATE_NETWORK_ENV}=1 to authorize "
            f"every private target, or add {host!r} to {PRIVATE_HOST_ALLOWLIST_ENV} to "
            "authorize only this one"
        )

    resolve_socket_addresses(
        host,
        parsed.port or (443 if parsed.scheme.lower() == "https" else 80),
        policy=policy,
    )
    return value


class BlockedRedirectError(ValueError):
    """A redirect's ``Location`` pointed at a target :func:`validate_url` refuses.

    Raised instead of a bare ``ValueError`` so a caller built around a broad
    ``except Exception`` — ``fetch_one`` (issue #175) and ``check_soft404`` alike
    — can tell "we refused to follow this redirect" apart from a transport
    failure and record it as the classified outcome it is: the response the
    server sent us was real, only the next hop is refused. ``status_code`` and
    ``location`` carry that response's status and the absolute target that was
    refused, since raising from inside the event hook is the only place either
    is still available — the caller never receives the response object itself.
    """

    def __init__(self, message: str, *, status_code: int, location: str):
        super().__init__(message)
        self.status_code = status_code
        self.location = location


def _validate_locally_pinned_request(request: Any) -> None:
    """Recheck a pre-pinned local request against its named host.

    List-mode collection pins before calling ``httpx.Client.get``. Its URL is
    necessarily a literal address, so treating it as a new user URL in the
    request hook loses the explicit named-host private allowlist. Do not trust
    the extension alone: the literal must still match the original Host header
    and a current vetted resolution of that exact original hostname.
    """
    original_host = request.extensions.get("sni_hostname")
    if not isinstance(original_host, str) or not original_host:
        raise ValueError("invalid pinned target")
    try:
        literal = ipaddress.ip_address(str(request.url.host)).compressed
        authority = urlsplit("//" + request.headers.get("host", ""))
        host_matches = (
            authority.hostname is not None
            and authority.hostname.rstrip(".").lower() == original_host.rstrip(".").lower()
            and authority.port == request.url.port
            and authority.username is None
            and authority.password is None
            and not authority.path
            and not authority.query
            and not authority.fragment
        )
    except ValueError:
        host_matches = False
    if not host_matches:
        raise ValueError("request host does not match pinned target")
    addresses = {
        ipaddress.ip_address(record[3][0].split("%", 1)[0]).compressed
        for record in resolve_socket_addresses(original_host, request.url.port or 80)
    }
    if literal not in addresses:
        raise ValueError("pinned target does not match hostname resolution")


def _guard_request(request: Any, policy: Any = None) -> None:
    if "sni_hostname" in request.extensions:
        if policy is not None:
            # The remote transport validates the literal against the named host
            # under its request budget and remote egress policy.
            return
        _validate_locally_pinned_request(request)
        return
    validate_url(str(request.url), policy=policy)


def _guard_redirect(response: Any, policy: Any = None) -> None:
    if not getattr(response, "is_redirect", False):
        return
    location = response.headers.get("location")
    if not location:
        return
    target = urljoin(str(response.request.url), location)
    try:
        validate_url(target, policy=policy)
    except ValueError as exc:
        raise BlockedRedirectError(
            str(exc),
            status_code=response.status_code,
            location="" if policy is not None else target,
        ) from exc


def network_event_hooks(policy: Any = None) -> dict[str, list[Any]]:
    """Return httpx hooks that validate every request and redirect.

    This is a second, independent check ahead of the transport's own pinning
    below — it runs first and rejects a bad scheme or embedded credentials
    before a connection is even attempted. It does not do the pinning itself:
    its resolution is discarded, exactly like ``validate_url``'s always was.
    The property that closes issue #142 is enforced downstream, in
    ``_PinningTransport.handle_request``, which connects to the address it
    just resolved rather than to a second, later resolution of the hostname.
    """
    if policy is None:
        return {"request": [_guard_request], "response": [_guard_redirect]}
    return {
        "request": [lambda request: _guard_request(request, policy)],
        "response": [lambda response: _guard_redirect(response, policy)],
    }


# Transport-construction kwargs a caller may forward through http_client() —
# distinct from httpx.Client's own kwargs (headers, follow_redirects, ...),
# which pass through untouched. Client() would silently ignore verify/cert/
# trust_env/http1/limits/proxy once a custom transport is supplied instead of
# building its own default one, so these have to be lifted out and given to
# the pinning transport directly; uds/local_address/retries/socket_options
# aren't Client() parameters at all and must be lifted out either way.
_TRANSPORT_KWARGS = (
    "verify",
    "cert",
    "trust_env",
    "http1",
    "limits",
    "proxy",
    "uds",
    "local_address",
    "retries",
    "socket_options",
)
_TRANSPORT_ONLY_KWARGS = ("uds", "local_address", "retries", "socket_options")

_pinning_transport_cls: type | None = None


def _get_pinning_transport_cls() -> type:
    """Build (once) the ``httpx.HTTPTransport`` subclass that pins every hop.

    Defined lazily, like the ``import httpx`` in ``http_client()`` below, so
    this module stays importable in the rare environment without httpx.
    """
    global _pinning_transport_cls
    if _pinning_transport_cls is not None:
        return _pinning_transport_cls

    import httpx

    class _PinningTransport(httpx.HTTPTransport):
        """Resolve, validate, and connect to the same address — every hop.

        ``http_client()`` used to validate a URL via request hooks and let
        httpx resolve DNS again to open the socket: two independent
        ``getaddrinfo()`` calls, free to disagree (issue #142). This is
        httpx's own connection path — ``handle_request`` runs once per hop,
        including every redirect the client follows internally — so pinning
        it here, rather than at each of fifteen call sites, is structural:
        a caller of ``http_client()`` gets the guard without asking for it.

        A request that already carries an ``sni_hostname`` extension was
        pinned by its caller before reaching the transport (``collect.py``'s
        list-mode fetch does this via ``pinned_target`` directly — see its
        docstring). Re-pinning it here would resolve the already-literal
        address as if it were a hostname, and the ``sni_hostname`` this
        transport would then set from *that* would replace the real one,
        breaking SNI and certificate verification. So an already-pinned
        request passes straight through to the real connection instead.
        """

        def handle_request(self, request: httpx.Request) -> httpx.Response:
            policy = getattr(self, "_seohead_remote_policy", None)
            if policy is not None:
                parsed_request = urlsplit(str(request.url))
                if (
                    request.url.scheme not in {"http", "https"}
                    or parsed_request.username is not None
                    or parsed_request.password is not None
                ):
                    raise RemoteTargetError("unsafe_target", "invalid remote request")
                # A caller may have pre-pinned the target before reaching this
                # transport. Recheck the literal against this job's policy even
                # when that caller ran in a different thread/context.
                original_host = request.extensions.get("sni_hostname")
                if "sni_hostname" in request.extensions and not original_host:
                    raise RemoteTargetError("unsafe_target", "invalid pinned target")
                expected_host = str(original_host or request.url.host)
                try:
                    authority = urlsplit("//" + request.headers.get("host", ""))
                    host_matches = (
                        authority.hostname is not None
                        and authority.hostname.rstrip(".").lower()
                        == expected_host.rstrip(".").lower()
                        and (authority.port is None or authority.port == request.url.port)
                        and authority.username is None
                        and authority.password is None
                        and not authority.path
                        and not authority.query
                        and not authority.fragment
                    )
                except ValueError:
                    host_matches = False
                if not host_matches:
                    raise RemoteTargetError("unsafe_target", "request host does not match target")
                if original_host:
                    literal = request.url.host
                    try:
                        ipaddress.ip_address(literal)
                    except ValueError:
                        raise RemoteTargetError("unsafe_target", "invalid pinned address") from None
                    if not _is_public_address(literal) and not policy.allows_private_address(
                        str(original_host), literal
                    ):
                        raise RemoteTargetError("unsafe_target", "private target blocked")
            if "sni_hostname" not in request.extensions:
                pinned_url, _headers, pin_extensions = pinned_target(
                    str(request.url), policy=policy
                )
                # Headers (including Host) are left untouched: httpx already
                # set them from the real hostname when it built this request,
                # and that is exactly what a pinned connection must keep.
                request = httpx.Request(
                    method=request.method,
                    url=pinned_url,
                    headers=request.headers,
                    stream=request.stream,
                    extensions={**request.extensions, **pin_extensions},
                )
            if policy is not None:
                policy.reserve_request(
                    str(request.url), original_host=request.extensions.get("sni_hostname")
                )
            proxy_route = getattr(self, "_seohead_proxy_route", None)
            tls_token = None
            if proxy_route is not None and request.url.scheme == "https":
                original_host = request.extensions.get("sni_hostname")
                if not isinstance(original_host, str):
                    raise NetworkUnavailable("proxied TLS identity is unavailable")
                tls_token = _PROXY_TLS_IDENTITY.set((request.url.host, original_host))
            try:
                response = super().handle_request(request)
            except httpx.ProxyError as exc:
                if policy is not None:
                    raise httpx.ConnectError("remote connection failed") from None
                if getattr(self, "_seohead_proxy_route", None) is None:
                    raise
                reason = (
                    "proxy authentication failed (HTTP 407)"
                    if "407" in str(exc)
                    else "proxy CONNECT failed"
                )
                raise NetworkUnavailable(reason) from None
            except (httpx.ConnectError, httpx.ConnectTimeout) as exc:
                if policy is not None:
                    if isinstance(exc, httpx.TimeoutException):
                        raise httpx.ConnectTimeout("remote request timed out") from None
                    raise httpx.ConnectError("remote connection failed") from None
                if getattr(self, "_seohead_proxy_route", None) is None:
                    raise
                reason = (
                    "proxied origin TLS verification failed"
                    if "CERTIFICATE_VERIFY_FAILED" in str(exc)
                    else "proxy connection failed"
                )
                raise NetworkUnavailable(reason) from None
            except httpx.TimeoutException:
                if policy is not None:
                    raise httpx.ReadTimeout("remote request timed out") from None
                if proxy_route is not None:
                    raise NetworkUnavailable("proxied request failed") from None
                raise
            except httpx.RequestError:
                if policy is not None:
                    raise httpx.ConnectError("remote connection failed") from None
                if getattr(self, "_seohead_proxy_route", None) is None:
                    raise
                raise NetworkUnavailable("proxied request failed") from None
            except Exception:
                if policy is not None:
                    raise RemoteTargetError("transport_failure", "remote request failed") from None
                raise
            finally:
                if tls_token is not None:
                    _PROXY_TLS_IDENTITY.reset(tls_token)
            if (
                getattr(self, "_seohead_proxy_route", None) is not None
                and response.status_code == 407
            ):
                response.close()
                raise NetworkUnavailable("proxy authentication failed (HTTP 407)")
            return response

    _pinning_transport_cls = _PinningTransport
    return _pinning_transport_cls


def _install_proxy_tls_backend(transport: Any) -> None:
    """Fail closed if the installed HTTPX/httpcore proxy path cannot preserve SNI."""
    import httpcore

    pool = getattr(transport, "_pool", None)
    backend = getattr(pool, "_network_backend", None)
    if not isinstance(pool, httpcore.HTTPProxy) or backend is None:
        transport.close()
        raise NetworkUnavailable(
            "installed HTTP proxy transport cannot preserve origin TLS identity"
        )
    pool._network_backend = _ProxyTLSBackend(backend)


def http_client(timeout: float, **kwargs: Any):
    """Return ``(client, http2_capable)`` with shared URL guardrails.

    The boolean must reach reports: without the optional HTTP/2 codec, reporting
    HTTP/1.1 as a server limitation would describe the client rather than the site.

    The client cannot be built without the pinning transport below — there is
    no kwarg that switches it off — because the fix for issue #142 is "this is
    the only client this function can hand back", not "remember to opt in".
    """
    try:
        import httpx
    except ImportError as exc:  # pragma: no cover - a base dependency
        raise NetworkUnavailable("httpx is required") from exc

    if "proxy" in kwargs:
        raise TypeError("http_client() requires a vetted proxy_route, not a raw proxy")
    reserved = kwargs.keys() & {"transport", "http2"}
    if reserved:
        raise TypeError(
            f"http_client() does not accept {', '.join(sorted(reserved))!s}: "
            "the pinning transport and its HTTP/2 negotiation are not "
            "overridable by callers"
        )

    policy = _remote_policy()
    if policy is not None:
        if kwargs.get("proxy") is not None or kwargs.get("proxy_route") is not None:
            raise RemoteTargetError("unsupported_proxy", "remote proxy routing is unsupported")
        # Ambient proxy variables are not a service tenant's authorization.
        kwargs["trust_env"] = False
    proxy_route = kwargs.pop("proxy_route", None)
    if proxy_route is not None:
        if not isinstance(proxy_route, ProxyRoute):
            raise TypeError("proxy_route must be a vetted ProxyRoute")
        kwargs["proxy"] = proxy_route.proxy
        kwargs["trust_env"] = False
        # Pooling by the rewritten IP could reuse one TLS tunnel for a different
        # hostname on the same address. Force a fresh verified tunnel per request.
        prior_limits = kwargs.get("limits")
        kwargs["limits"] = httpx.Limits(
            max_connections=prior_limits.max_connections if prior_limits else 100,
            max_keepalive_connections=0,
        )

    supplied_hooks = kwargs.pop("event_hooks", None) or {}
    hooks = network_event_hooks(policy)
    for phase, values in supplied_hooks.items():
        hooks.setdefault(phase, []).extend(values)

    transport_kwargs = {k: kwargs[k] for k in _TRANSPORT_KWARGS if k in kwargs}
    client_kwargs = {k: v for k, v in kwargs.items() if k not in (*_TRANSPORT_ONLY_KWARGS, "proxy")}
    PinningTransport = _get_pinning_transport_cls()

    options = {
        "timeout": timeout,
        "headers": {"User-Agent": UA},
        "follow_redirects": True,
        "event_hooks": hooks,
        **client_kwargs,
    }
    if proxy_route is not None:
        transport = PinningTransport(http2=False, **transport_kwargs)
        transport._seohead_proxy_route = proxy_route
        _install_proxy_tls_backend(transport)
        return httpx.Client(transport=transport, **options), False
    try:
        transport = PinningTransport(http2=True, **transport_kwargs)
        transport._seohead_remote_policy = policy
        transport._seohead_proxy_route = proxy_route
        return httpx.Client(http2=True, transport=transport, **options), True
    except ImportError:
        transport = PinningTransport(http2=False, **transport_kwargs)
        transport._seohead_remote_policy = policy
        transport._seohead_proxy_route = proxy_route
        return httpx.Client(transport=transport, **options), False


def _client(timeout: float):
    return http_client(timeout)[0]


# Compound public suffixes common enough that treating the last two labels as
# the registrable domain would merge unrelated sites. A full public suffix list
# is large and changes; this only has to tell a subdomain from a separate site.
_COMPOUND_SUFFIXES = frozenset({"com", "net", "org", "co", "gov", "edu", "ac", "spb", "msk"})

# Multi-tenant hosting suffixes where the label directly under the suffix is a
# customer, not a subdomain of one site -- the shape a full public suffix list
# would get right and this project deliberately doesn't ship (issue #144). Not
# a dependency: no PSL package was already in use, and this project's own rule
# on adding one for a single lookup is to prefer the standard library or a
# hand-kept table first. Kept short and reviewed whenever a client's audited
# site turns out to live on a shared suffix not yet listed here -- the ones
# below are the platforms actually seen in the checks this toolkit runs
# (GitHub/GitLab Pages, Vercel, Netlify, Heroku, Shopify, S3 static sites,
# Firebase/web.app, WordPress.com, Blogspot), not an attempt at completeness.
_MULTI_TENANT_SUFFIXES = frozenset(
    {
        "github.io",
        "githubusercontent.com",
        "gitlab.io",
        "pages.dev",
        "vercel.app",
        "netlify.app",
        "herokuapp.com",
        "myshopify.com",
        "wordpress.com",
        "blogspot.com",
        "s3.amazonaws.com",
        "web.app",
        "firebaseapp.com",
    }
)


def registrable_domain(host: str) -> str:
    """Approximate the registrable domain of a hostname."""
    host = (host or "").lower().strip(".")
    parts = host.split(".")
    if len(parts) <= 2:
        return host
    # Longest suffix first: a 3-label entry (s3.amazonaws.com) must be tried
    # before a 2-label slice of the same host could ever mask it.
    for suffix_len in (3, 2):
        if len(parts) > suffix_len and ".".join(parts[-suffix_len:]) in _MULTI_TENANT_SUFFIXES:
            return ".".join(parts[-(suffix_len + 1) :])
    if parts[-2] in _COMPOUND_SUFFIXES and len(parts[-1]) <= 3:
        return ".".join(parts[-3:])
    return ".".join(parts[-2:])


def normalize_domain(value: str) -> str:
    """Normalize a URL or hostname to a lowercase, non-www ASCII domain."""
    raw = (value or "").strip()
    if not raw:
        return ""
    if "//" in raw:
        raw = urlsplit(raw).netloc or urlsplit(raw).path
    raw = raw.split("/")[0].split("@")[-1].strip().rstrip(".").lower()
    if raw.startswith("[") or raw.count(":") > 1:
        return ""
    raw = raw.split(":")[0]
    raw = raw.removeprefix("www.")
    try:
        raw = raw.encode("idna").decode("ascii")
    except (UnicodeError, UnicodeDecodeError):
        return ""
    return raw if _DOMAIN_RE.match(raw) else ""


def normalize_url(value: str) -> str:
    """Normalize user input to an absolute HTTP(S) URL without touching DNS."""
    raw = (value or "").strip()
    if not raw:
        return ""
    scheme_prefix = raw.split(":", 1)[0].lower()
    if "//" not in raw and scheme_prefix.isalpha() and raw[len(scheme_prefix) :].startswith(":"):
        return ""
    if "//" not in raw:
        raw = "https://" + raw
    try:
        parts = urlsplit(raw)
    except ValueError:
        return ""
    if parts.scheme not in {"http", "https"} or not parts.netloc:
        return ""
    if parts.username is not None or parts.password is not None:
        return ""
    return raw


def doh(name: str, rtype: str, timeout: float = 8.0) -> list[str]:
    """Query DNS over HTTPS and return record values; failures return an empty list."""
    if not name:
        return []
    try:
        client = _client(timeout)
    except NetworkUnavailable:
        return []
    with client:
        for endpoint in DOH_ENDPOINTS:
            try:
                response = client.get(
                    endpoint,
                    params={"name": name, "type": rtype},
                    headers={"Accept": "application/dns-json"},
                )
                if response.status_code != 200:
                    continue
                answers = response.json().get("Answer") or []
            except Exception:
                continue
            records = []
            for item in answers:
                data = str(item.get("data", "")).strip()
                if data:
                    records.append(data.strip('"').rstrip("."))
            if records:
                return records
    return []


def rdap(path: str, timeout: float = 12.0) -> dict[str, Any]:
    """Query RDAP and distinguish unsupported registries from parser failures."""
    try:
        client = _client(timeout)
    except NetworkUnavailable:
        return {"supported": False, "error": "httpx is required"}
    with client:
        try:
            response = client.get(
                f"{RDAP_BOOTSTRAP}/{path}",
                headers={"Accept": "application/rdap+json"},
            )
        except Exception as exc:
            return {"supported": False, "error": str(exc)}
        if response.status_code == 404:
            return {"supported": False, "error": "not found in RDAP"}
        if response.status_code >= 400:
            return {"supported": False, "error": f"RDAP HTTP {response.status_code}"}
        try:
            return {"supported": True, "data": response.json()}
        except ValueError:
            return {"supported": False, "error": "RDAP returned non-JSON"}


# Registries the default whois resolver does not reach. Without these it answers
# with the zone record instead of the domain's own, which reads as a real
# registration: every .ru domain came back with the .RU delegation date of 1994.
WHOIS_SERVERS_BY_TLD: dict[str, str] = {
    "ru": "whois.tcinet.ru",
    "su": "whois.tcinet.ru",
    "xn--p1ai": "whois.tcinet.ru",  # the Cyrillic .rf ccTLD, in punycode
}

# Fields a registry uses to refer to the authoritative server.
_WHOIS_REFERRAL_KEYS = ("whois", "refer", "registrar whois server")

_HOSTNAME_RE = re.compile(
    r"^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$"
)


def whois_field(text: str, keys: tuple[str, ...]) -> str | None:
    """First value of the first matching key, ignoring comment lines."""
    for line in text.splitlines():
        stripped = line.lstrip()
        if ":" not in stripped or stripped.startswith(("%", "#")):
            continue
        key, _, value = stripped.partition(":")
        if key.strip().lower() in keys and value.strip():
            return value.strip()
    return None


def whois_text(domain: str, timeout: float = 15.0, server: str | None = None) -> str | None:
    """Return raw system-whois output as an optional ccTLD fallback."""
    binary = shutil.which("whois")
    if not binary or not domain:
        return None
    argv = [binary]
    if server:
        # The server may come from a registry response, i.e. untrusted input.
        # Only a syntactically valid hostname is ever passed on, and it is
        # passed as an argument vector, never through a shell.
        if not _HOSTNAME_RE.match(server.lower()):
            return None
        argv += ["-h", server]
    argv.append(domain)
    try:
        process = subprocess.run(argv, capture_output=True, text=True, timeout=timeout, check=False)
    except (subprocess.SubprocessError, OSError):
        return None
    return process.stdout or None


def whois_lookup(domain: str, timeout: float = 15.0) -> tuple[str | None, str | None]:
    """Query whois, following one registry referral. Returns ``(text, server)``.

    The referral hop is what separates a domain's own record from the record of
    its zone; a caller still has to confirm the answer is about the domain it
    asked for.
    """
    if not domain:
        return None, None
    tld = domain.rsplit(".", 1)[-1].lower()
    mapped = WHOIS_SERVERS_BY_TLD.get(tld)
    if mapped:
        text = whois_text(domain, timeout, server=mapped)
        if text:
            return text, mapped

    text = whois_text(domain, timeout)
    if not text:
        return None, None

    referral = whois_field(text, _WHOIS_REFERRAL_KEYS)
    if referral:
        referral = referral.split("//")[-1].split("/")[0].strip().lower()
        if referral and referral != (mapped or ""):
            deeper = whois_text(domain, timeout, server=referral)
            if deeper:
                return deeper, referral
    return text, None
