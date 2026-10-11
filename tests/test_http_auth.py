"""Host-bound Basic/Digest auth stays out of unrelated requests and gates retries."""

import base64

import httpx

from seohead.crawl.auth import host_bound_auth


def _entry(scheme="basic"):
    return {
        "host": "example.test",
        "scheme": scheme,
        "username_env": "env:SEOHEAD_TEST_USER",
        "password_env": "env:SEOHEAD_TEST_PASSWORD",
    }


def test_basic_auth_is_host_bound_and_uses_environment_values(monkeypatch):
    monkeypatch.setenv("SEOHEAD_TEST_USER", "crawler")
    monkeypatch.setenv("SEOHEAD_TEST_PASSWORD", "fixture-secret")
    seen = []

    def transport(request):
        seen.append((request.url.host, request.headers.get("Authorization")))
        return httpx.Response(200, request=request)

    with httpx.Client(auth=host_bound_auth([_entry()]), transport=httpx.MockTransport(transport)) as client:
        client.get("https://example.test/")
        client.get("https://other.test/")

    assert seen[0] == (
        "example.test",
        "Basic " + base64.b64encode(b"crawler:fixture-secret").decode("ascii"),
    )
    assert seen[1] == ("other.test", None)


def test_digest_challenge_retry_reserves_an_extra_request_turn(monkeypatch):
    monkeypatch.setenv("SEOHEAD_TEST_USER", "crawler")
    monkeypatch.setenv("SEOHEAD_TEST_PASSWORD", "fixture-secret")
    seen = []
    turns = []

    def transport(request):
        seen.append(request.headers.get("Authorization"))
        if len(seen) == 1:
            return httpx.Response(
                401,
                headers={
                    "WWW-Authenticate": 'Digest realm="fixture", nonce="n", '
                    'qop="auth", opaque="o", algorithm=MD5'
                },
                request=request,
            )
        return httpx.Response(200, request=request)

    auth = host_bound_auth([_entry("digest")], lambda: turns.append("retry"))
    with httpx.Client(auth=auth, transport=httpx.MockTransport(transport)) as client:
        assert client.get("https://example.test/").status_code == 200

    assert seen[0] is None
    assert seen[1].startswith("Digest username=\"crawler\"")
    assert turns == ["retry"]


def test_http_auth_refuses_to_send_credentials_over_plain_http(monkeypatch):
    monkeypatch.setenv("SEOHEAD_TEST_USER", "crawler")
    monkeypatch.setenv("SEOHEAD_TEST_PASSWORD", "fixture-secret")
    transport = httpx.MockTransport(lambda request: httpx.Response(200, request=request))
    with httpx.Client(auth=host_bound_auth([_entry()]), transport=transport) as client:
        try:
            client.get("http://example.test/")
        except ValueError as exc:
            assert "requires HTTPS" in str(exc)
        else:
            raise AssertionError("HTTP auth reached a plain HTTP origin")


def test_redirect_to_another_host_does_not_receive_the_original_auth(monkeypatch):
    monkeypatch.setenv("SEOHEAD_TEST_USER", "crawler")
    monkeypatch.setenv("SEOHEAD_TEST_PASSWORD", "fixture-secret")
    seen = []

    def transport(request):
        seen.append((request.url.host, request.headers.get("Authorization")))
        if request.url.host == "example.test":
            return httpx.Response(302, headers={"Location": "https://other.test/"}, request=request)
        return httpx.Response(200, request=request)

    with httpx.Client(
        auth=host_bound_auth([_entry()]),
        transport=httpx.MockTransport(transport),
        follow_redirects=True,
    ) as client:
        assert client.get("https://example.test/").status_code == 200
    assert seen[0][1] is not None
    assert seen[1] == ("other.test", None)
