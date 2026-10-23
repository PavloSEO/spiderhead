"""Unit tests for the pure (network-free) recon functions."""

import datetime as dt

import pytest

from seohead.recon import backlinks, cdn, domain, security, tech
from seohead.recon.net import http_client, normalize_domain, normalize_url, registrable_domain

# ── Input normalization ──────────────────────────────────────────────────────


def test_normalize_domain_strips_scheme_www_port_and_path():
    for raw in (
        "https://www.Example.com/path?q=1",
        "www.example.com:443",
        "http://example.com",
        "example.com.",
    ):
        assert normalize_domain(raw) == "example.com"


def test_normalize_domain_converts_idn_to_punycode():
    # The Cyrillic IDN is intentional: this fixture verifies IDNA conversion.
    assert normalize_domain("https://пример.рф/") == "xn--e1afmkfd.xn--p1ai"


def test_normalize_domain_rejects_non_domains():
    for raw in ("", "   ", "localhost", "just-a-string", "http://[::1]/", "1.2.3"):
        assert normalize_domain(raw) == ""


# ── registrable_domain: multi-tenant hosting suffixes (issue #144) ──────────


def test_registrable_domain_keeps_multi_tenant_platform_customers_apart():
    """github.io, pages.dev, herokuapp.com, vercel.app and myshopify.com are shared
    suffixes: the label directly under them is a customer, not a subdomain of one
    site, so two different customers must never collapse to the same value."""
    pairs = [
        ("alice.github.io", "bob.github.io"),
        ("myproject.pages.dev", "attacker.pages.dev"),
        ("shop.herokuapp.com", "phishing.herokuapp.com"),
        ("client.vercel.app", "other-tenant.vercel.app"),
        ("mystore.myshopify.com", "someone-elses-store.myshopify.com"),
    ]
    for start_host, other_host in pairs:
        assert registrable_domain(start_host) != registrable_domain(other_host)
        assert registrable_domain(start_host) == start_host
        assert registrable_domain(other_host) == other_host


def test_registrable_domain_still_folds_a_real_subdomain_of_a_platform_customer():
    # A deeper subdomain of alice's own site is still alice's site, not a new tenant.
    assert registrable_domain("www.alice.github.io") == "alice.github.io"


def test_registrable_domain_handles_a_three_label_multi_tenant_suffix():
    assert registrable_domain("mybucket.s3.amazonaws.com") == "mybucket.s3.amazonaws.com"


def test_registrable_domain_is_unaffected_for_ordinary_domains():
    # No regression on the cases the pre-existing compound-suffix table already covered.
    assert registrable_domain("www.example.com") == "example.com"
    assert registrable_domain("www.example.co.uk") == "example.co.uk"


def test_normalize_url_adds_scheme_and_rejects_other_protocols():
    assert normalize_url("example.com/x") == "https://example.com/x"
    assert normalize_url("http://example.com") == "http://example.com"
    assert normalize_url("ftp://example.com") == ""
    assert normalize_url("mailto:a@b.c") == ""


# ── Domain dates, WHOIS parsing, and risk flags ──────────────────────────────


def test_parse_date_handles_rdap_and_local_formats():
    assert domain._parse_date("2020-03-01T10:00:00Z") == dt.date(2020, 3, 1)
    assert domain._parse_date("2020-03-01") == dt.date(2020, 3, 1)
    assert domain._parse_date("01.03.2020") == dt.date(2020, 3, 1)
    assert domain._parse_date("not a date") is None
    assert domain._parse_date("") is None


def test_parse_cert_date_reads_openssl_format():
    assert domain._parse_cert_date("Jun  1 12:00:00 2027 GMT") == dt.date(2027, 6, 1)
    assert domain._parse_cert_date("") is None


def test_tls_probe_requires_tls_1_2(monkeypatch):
    class Context:
        minimum_version = None

        def wrap_socket(self, _raw, *, server_hostname):
            self.server_hostname = server_hostname
            return Wrapped()

    class Wrapped:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

        def getpeercert(self):
            return {
                "issuer": ((("organizationName", "Example CA"),),),
                "notAfter": "Jun  1 12:00:00 2027 GMT",
                "subjectAltName": (("DNS", "example.com"),),
            }

        def version(self):
            return "TLSv1.3"

    class RawSocket:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

        def settimeout(self, value):
            self.timeout = value

        def connect(self, value):
            self.connected = value

    context = Context()
    monkeypatch.setattr(domain.ssl, "create_default_context", lambda: context)
    monkeypatch.setattr(
        domain,
        "resolve_socket_addresses",
        lambda _host, _port: [
            (domain.socket.AF_INET, domain.socket.SOCK_STREAM, 6, ("93.184.216.34", 443))
        ],
    )
    raw = RawSocket()
    monkeypatch.setattr(domain.socket, "socket", lambda *_args: raw)

    result = domain._tls("example.com", timeout=3)
    assert result["ok"] is True
    assert result["protocol"] == "TLSv1.3"
    assert context.minimum_version is domain.ssl.TLSVersion.TLSv1_2
    assert context.server_hostname == "example.com"
    assert raw.connected == ("93.184.216.34", 443)
    assert raw.timeout == 3


def test_from_whois_text_picks_key_fields_for_cctld():
    parsed = domain._from_whois_text(
        "% comment line\n"
        "domain: EXAMPLE.BY\n"
        "nserver: ns1.hoster.by\n"
        "nserver: NS2.HOSTER.BY.\n"
        "registrar: Reliable\n"
        "created: 2015-06-01\n"
        "expires: 2027-06-01\n"
    )
    assert parsed["registrar"] == "Reliable"
    assert parsed["created"] == "2015-06-01"
    assert parsed["nameservers"] == ["ns1.hoster.by", "ns2.hoster.by"]


def _profile(**over):
    base = {
        "registration": {"age_days": 4000, "expires_in_days": 300, "status": [], "source": "rdap"},
        "dns": {"a": ["1.2.3.4"], "aaaa": [], "spf": "v=spf1 ~all", "dmarc": "v=DMARC1"},
        "tls": {"valid": True, "days_left": 90},
    }
    base.update(over)
    return base


def test_flags_are_empty_for_a_healthy_domain():
    assert domain._flags(_profile()) == []


def test_flags_catch_young_domain_and_expiring_cert():
    flags = domain._flags(
        _profile(
            registration={
                "age_days": 10,
                "expires_in_days": 5,
                "status": ["clientHold"],
                "source": "rdap",
            },
            tls={"valid": True, "days_left": 3},
        )
    )
    joined = " ".join(flags)
    assert "domain" in joined and "10 days old" in joined
    assert "expires in 5" in joined
    assert "hold" in joined
    assert "certificate expires in 3" in joined


def test_flags_report_missing_spf_dmarc_and_dead_dns():
    flags = domain._flags(_profile(dns={"a": [], "aaaa": [], "spf": None, "dmarc": None}))
    joined = " ".join(flags)
    assert "does not resolve" in joined and "SPF" in joined and "DMARC" in joined


# ── CDN detection and cache analysis ─────────────────────────────────────────


def test_detect_cdn_by_header_server_and_via():
    assert cdn._detect_cdn({"cf-ray": "abc"}) == "Cloudflare"
    assert cdn._detect_cdn({"server": "AkamaiGHost"}) == "Akamai"
    assert cdn._detect_cdn({"x-amz-cf-id": "z"}) == "Amazon CloudFront"
    assert cdn._detect_cdn({"via": "1.1 google"}) == "Google Cloud CDN"
    assert cdn._detect_cdn({"server": "nginx"}) is None


def test_cache_status_and_classification():
    assert cdn._classify(cdn._cache_status({"cf-cache-status": "HIT"})) == "hit"
    assert cdn._classify(cdn._cache_status({"cf-cache-status": "DYNAMIC"})) == "miss"
    assert cdn._classify(cdn._cache_status({"x-cache": "Hit from cloudfront"})) == "hit"
    assert cdn._cache_status({"server": "nginx"}) is None
    assert cdn._classify(None) is None


def test_parse_cache_control_reads_directives():
    parsed = cdn._parse_cache_control("public, max-age=3600, immutable")
    assert parsed["max_age"] == 3600 and parsed["public"] and parsed["immutable"]
    assert cdn._parse_cache_control("no-store")["no_store"] is True
    assert cdn._parse_cache_control("")["raw"] is None
    # A malformed max-age value must not crash the parser.
    assert cdn._parse_cache_control("max-age=abc")["max_age"] is None


def _cdn_result(**over):
    result = {
        "cdn": "Cloudflare",
        "transport": {
            "http_version": "HTTP/2",
            "http_version_measurable": True,
            "http3_advertised": True,
            "content_encoding": "br",
            "brotli_supported": True,
            "ttfb_first_ms": 120.0,
        },
        "cache": {
            "cache_control": cdn._parse_cache_control("public, max-age=600"),
            "etag": 'W/"x"',
            "last_modified": None,
            "hit_first": "miss",
            "hit_second": "hit",
            "revalidation": {"supported": True},
        },
    }
    for key, value in over.items():
        result[key].update(value) if isinstance(value, dict) else result.update({key: value})
    return result


def test_cdn_findings_stay_quiet_when_everything_is_fine():
    assert cdn._findings(_cdn_result()) == []


def test_cdn_findings_do_not_blame_the_server_when_h2_is_missing():
    """A client without h2 support must not blame the origin for using HTTP/1.1."""
    found = cdn._findings(
        _cdn_result(transport={"http_version": "HTTP/1.1", "http_version_measurable": False})
    )
    joined = " ".join(found)
    assert "h2" in joined
    assert "HTTP/2 would" not in joined


def test_cdn_findings_report_probe_failure_not_brotli_disabled():
    """#482: brotli_supported=None (probe failed) must not read as 'not enabled'."""
    found = cdn._findings(
        _cdn_result(transport={"content_encoding": "gzip", "brotli_supported": None})
    )
    joined = " ".join(found)
    assert "Brotli is not enabled" not in joined
    assert "could not be probed" in joined


def test_cdn_findings_still_report_confirmed_brotli_absence():
    """Negative control: a probe that succeeded and returned non-br still fires."""
    found = cdn._findings(
        _cdn_result(transport={"content_encoding": "gzip", "brotli_supported": False})
    )
    assert any("Brotli is not enabled" in f for f in found)


def test_cdn_findings_stay_quiet_when_brotli_confirmed():
    """Negative control: a probe confirming br must not fire any brotli finding."""
    found = cdn._findings(
        _cdn_result(transport={"content_encoding": "br", "brotli_supported": True})
    )
    assert not any("Brotli" in f for f in found)


def test_cdn_findings_report_real_http1_and_dead_cache():
    found = cdn._findings(
        _cdn_result(
            transport={"http_version": "HTTP/1.1"},
            cache={"hit_second": "miss", "cache_control": cdn._parse_cache_control("")},
        )
    )
    joined = " ".join(found)
    assert "HTTP/2 would" in joined
    assert "MISS" in joined and "CDN did not retain the page" in joined
    assert "Cache-Control is missing" in joined


# ── Technology signatures and heuristics ─────────────────────────────────────


def _match(kind, marker, *, html="", headers=None, cookies=None, scripts=""):
    return tech._match(
        kind,
        marker,
        html_low=html.lower(),
        headers=headers or {},
        cookies=cookies or {},
        scripts_low=scripts.lower(),
    )


def test_match_finds_signatures_in_every_source():
    assert _match("html", "/wp-content/", html="<img src='/WP-CONTENT/a.png'>")
    assert _match("header", "x-vercel-id", headers={"x-vercel-id": "1"})
    assert _match("value", "nginx", headers={"server": "nginx/1.21"})
    assert _match("cookie", "BITRIX_SM", cookies={"BITRIX_SM_GUEST_ID": "7"})
    assert _match("script", "jquery", scripts="https://cdn/jQuery.min.js")
    assert _match("html", "/wp-content/", html="<p>unrelated content</p>") is None


def test_match_returns_the_marker_it_fired_on():
    assert "x-vercel-id" in _match("header", "x-vercel-id", headers={"x-vercel-id": "1"})


def test_tech_findings_do_not_call_jquery_a_headless_stack():
    """jQuery commonly accompanies a CMS and does not imply a headless stack."""
    found = tech._findings(
        {
            "cms": [{"name": "OpenCart"}],
            "library": [{"name": "jQuery"}],
            "analytics": [{"name": "Yandex Metrica"}],
        },
        [],
    )
    assert not any("headless" in f for f in found)


def test_tech_findings_flag_a_real_headless_stack():
    found = tech._findings(
        {
            "cms": [{"name": "WordPress"}],
            "framework": [{"name": "Next.js"}],
            "analytics": [{"name": "GA4"}],
        },
        [],
    )
    assert any("headless" in f for f in found)


def test_tech_findings_notice_missing_analytics_and_pixel_bloat():
    found = tech._findings(
        {"cms": [{"name": "WordPress"}], "pixel": [{"name": "a"}, {"name": "b"}, {"name": "c"}]},
        [f"host{i}.tld" for i in range(12)],
    )
    joined = " ".join(found)
    assert "analytics" in joined and "3 advertising" in joined and "12 third-party" in joined


# ── Security scoring and findings ────────────────────────────────────────────


def test_grade_scale():
    assert security._grade(100) == "A" and security._grade(90) == "A"
    assert security._grade(75) == "B" and security._grade(60) == "C"
    assert security._grade(0) == "F"


def _sec_result(**over):
    result = {
        "headers_missing": [],
        "version_disclosure": {},
        "cookies": [],
        "https_redirect": {"checked": True, "upgrades": True},
        "exposed_paths": None,
    }
    result.update(over)
    return result


def test_security_findings_are_empty_for_a_clean_site():
    assert security._findings(_sec_result()) == []


def test_security_findings_report_disclosure_only_with_a_version():
    assert security._findings(_sec_result(version_disclosure={"server": "nginx"})) == []
    found = security._findings(_sec_result(version_disclosure={"x-powered-by": "PHP/7.4.33"}))
    assert any("x-powered-by" in f and "version" in f for f in found)


def test_security_findings_cover_http_downgrade_cookies_and_exposed_paths():
    found = security._findings(
        _sec_result(
            https_redirect={"checked": True, "upgrades": False},
            cookies=[{"name": "SID", "secure": False, "same_site": None}],
            exposed_paths=[{"path": "/.env", "bytes": 120}],
        )
    )
    joined = " ".join(found)
    assert "does not redirect to https" in joined.lower()
    assert "without the Secure flag" in joined and "without SameSite" in joined
    assert "/.env" in joined


class _FakeCookieHeaders:
    def __init__(self, cookies):
        self._cookies = cookies

    def get_list(self, name):
        return self._cookies


class _FakeCookieResp:
    def __init__(self, cookies):
        self.headers = _FakeCookieHeaders(cookies)


def test_cookie_flags_detect_secure_without_a_preceding_space():
    found = security._cookie_flags(_FakeCookieResp(["sessionid=abc123;Secure;HttpOnly;Path=/"]))
    assert found == [{"name": "sessionid", "secure": True, "http_only": True, "same_site": None}]


def test_cookie_flags_still_detect_secure_with_a_preceding_space():
    found = security._cookie_flags(_FakeCookieResp(["sessionid=abc123; Secure; HttpOnly; Path=/"]))
    assert found[0]["secure"] is True


def test_cookie_flags_report_insecure_when_secure_is_absent():
    found = security._cookie_flags(_FakeCookieResp(["sessionid=abc123;HttpOnly"]))
    assert found[0]["secure"] is False


class _FakeProbeHeaders(dict):
    def get_list(self, name):
        return []

    def get(self, name, default=""):
        return dict.get(self, name, default)


class _FakeProbeResp:
    def __init__(self, status_code, text, headers=None):
        self.status_code = status_code
        self.text = text
        self.content = text.encode()
        self.headers = _FakeProbeHeaders(headers or {"content-type": "text/plain"})


def test_probe_does_not_report_a_soft_404_for_svn_entries():
    class FakeClient:
        def get(self, url):
            if url.endswith("/.svn/entries"):
                return _FakeProbeResp(
                    200,
                    "Page not found - please check the URL",
                    {"content-type": "text/plain; charset=utf-8"},
                )
            return _FakeProbeResp(404, "not found")

    exposed = security._probe(FakeClient(), "https://example.com")
    assert not any(item["path"] == "/.svn/entries" for item in exposed)


def test_probe_still_reports_real_svn_entries_content():
    class FakeClient:
        def get(self, url):
            if url.endswith("/.svn/entries"):
                return _FakeProbeResp(
                    200,
                    "10\n\ndir\n0\nsvn://svn.example.com/repo\n",
                    {"content-type": "text/plain"},
                )
            return _FakeProbeResp(404, "not found")

    exposed = security._probe(FakeClient(), "https://example.com")
    assert any(item["path"] == "/.svn/entries" for item in exposed)


# ── Backlink domain matching ─────────────────────────────────────────────────


def test_same_site_matches_domain_and_subdomains_only():
    assert backlinks._same_site("https://example.com/a", "example.com")
    assert backlinks._same_site("https://blog.example.com/", "example.com")
    assert backlinks._same_site("https://www.example.com/", "example.com")
    assert not backlinks._same_site("https://other.example/", "example.com")
    assert not backlinks._same_site("/relative", "example.com")


# ── http_client reserved kwargs (#484) ───────────────────────────────────────


def test_http_client_rejects_reserved_transport_kwarg_clearly():
    httpx = pytest.importorskip("httpx")
    with pytest.raises(TypeError, match="transport"):
        http_client(5.0, transport=httpx.HTTPTransport())


def test_http_client_rejects_reserved_http2_kwarg_clearly():
    with pytest.raises(TypeError, match="http2"):
        http_client(5.0, http2=False)


def test_http_client_still_accepts_ordinary_kwargs():
    """Negative control: kwargs that already worked must keep working unchanged."""
    pytest.importorskip("httpx")
    client, _ = http_client(5.0, verify=False)
    client.close()
    client, _ = http_client(5.0, headers={"X-Test": "1"})
    assert client.headers.get("x-test") == "1"
    client.close()


# ── WAF block classification (#1363) ──────────────────────────────────────────


class _FakeResponse:
    """Just the two surfaces waf_block_reason reads: status and headers."""

    def __init__(self, status_code, headers=None, text=""):
        self.status_code = status_code
        self.headers = headers or {}
        self.text = text


def test_cf_mitigated_403_is_a_named_block():
    from seohead.recon.net import BLOCKED_BY_WAF, waf_block_reason

    response = _FakeResponse(403, {"cf-mitigated": "challenge"})
    assert waf_block_reason(response) == "cf-mitigated: challenge"
    assert BLOCKED_BY_WAF == "blocked_by_waf"


def test_cloudflare_edge_429_is_a_block_even_without_the_mitigation_header():
    from seohead.recon.net import waf_block_reason

    response = _FakeResponse(429, {"server": "cloudflare"})
    assert waf_block_reason(response) == "cloudflare edge"


def test_challenge_body_markers_are_detected_from_the_snippet():
    from seohead.recon.net import waf_block_reason

    body = "<html><body>Please wait... <title>Just a moment...</title>" + "x" * 9000
    assert waf_block_reason(_FakeResponse(403, {}, body)) == "challenge body: just a moment..."
    # Only the first bytes are inspected, so a marker below the sniff window
    # is not evidence the classifier promises to find.
    far = "y" * 9000 + "cdn-cgi/challenge-platform"
    assert waf_block_reason(_FakeResponse(403, {}, far)) is None


def test_a_plain_origin_403_stays_a_regular_error():
    """No markers, no block: the origin refused this URL, not this client."""
    from seohead.recon.net import waf_block_reason

    body = "<html><head><title>403 Forbidden</title></head><body>Forbidden</body></html>"
    assert waf_block_reason(_FakeResponse(403, {"server": "nginx"}, body)) is None


def test_a_2xx_is_never_a_block_and_only_403_429_can_be():
    from seohead.recon.net import waf_block_reason

    served = _FakeResponse(200, {"server": "cloudflare"}, "cdn-cgi/challenge-platform")
    assert waf_block_reason(served) is None
    assert waf_block_reason(_FakeResponse(404, {"cf-mitigated": "challenge"})) is None
    assert waf_block_reason(_FakeResponse(500, {"server": "cloudflare"})) is None
