"""Offline public contracts for robots retrieval and group boundaries."""

from __future__ import annotations

import pytest

from seohead.checks import robots
from seohead.crawl.spider import crawl_site


class Response:
    def __init__(self, text: str = "", status_code: int = 200, content_type: str = "text/plain"):
        self.text = text
        self.status_code = status_code
        self.headers = {"content-type": content_type}


class Client:
    def __init__(self, response: Response):
        self.response = response

    def __enter__(self) -> Client:
        return self

    def __exit__(self, *_args: object) -> bool:
        return False

    def get(self, _url: str) -> Response:
        return self.response


@pytest.mark.parametrize("status_code", [429, 503])
def test_unavailable_robots_status_stops_the_spider_and_fails_the_tool(monkeypatch, status_code):
    response = Response(status_code=status_code)
    monkeypatch.setattr(robots, "http_client", lambda *_args, **_kwargs: (Client(response), False))

    checked = robots.check_robots("https://example.com/")
    crawled = crawl_site(
        "https://example.com/",
        min_delay=0,
        sleeper=lambda _delay: None,
        fetcher=lambda url: (
            response if url.endswith("/robots.txt") else Response("<title>Home</title>")
        ),
    )

    assert checked["ok"] is False
    assert checked["status_code"] == status_code
    assert "rules could not be read" in checked["error"]
    assert crawled.finish_reason == "robots_unavailable"
    assert str(status_code) in crawled.stopped_reason


def test_404_robots_remains_a_missing_permissive_file(monkeypatch):
    response = Response(status_code=404)
    monkeypatch.setattr(robots, "http_client", lambda *_args, **_kwargs: (Client(response), False))

    checked = robots.check_robots("https://example.com/")
    crawled = crawl_site(
        "https://example.com/",
        min_delay=0,
        sleeper=lambda _delay: None,
        fetcher=lambda url: (
            response if url.endswith("/robots.txt") else Response("<title>Home</title>")
        ),
    )

    assert checked["ok"] is True
    assert checked["exists"] is False
    assert checked["note"] == "no robots.txt (crawl allowed)"
    assert {page.url for page in crawled.pages} == {"https://example.com/"}


def test_crawl_delay_closes_an_agent_run_before_the_next_agent():
    parsed = robots.parse_robots(
        "User-agent: FirstBot\nCrawl-delay: 10,5\n"
        "User-agent: SecondBot\nCrawl-delay: 2\nDisallow: /second-only/\n"
    )

    assert parsed["groups"] == [
        {
            "user_agents": ["FirstBot"],
            "allow": [],
            "disallow": [],
            "crawl_delay": 10.5,
            "request_rate_delay": None,
        },
        {
            "user_agents": ["SecondBot"],
            "allow": [],
            "disallow": ["/second-only/"],
            "crawl_delay": 2.0,
            "request_rate_delay": None,
        },
    ]
    assert robots.crawl_delay(parsed, "FirstBot") == 10.5
    assert robots.crawl_delay(parsed, "SecondBot") == 2.0
    assert robots.is_allowed(parsed, "/second-only/", "FirstBot") is True
    assert robots.is_allowed(parsed, "/second-only/", "SecondBot") is False


@pytest.mark.parametrize("delay", ["soon", "Infinity", "NaN", "-1"])
def test_malformed_crawl_delay_still_closes_an_agent_run(delay):
    parsed = robots.parse_robots(
        f"User-agent: FirstBot\nCrawl-delay: {delay}\n"
        "User-agent: SecondBot\nDisallow: /second-only/\n"
    )

    assert robots.crawl_delay(parsed, "FirstBot") is None
    assert robots.is_allowed(parsed, "/second-only/", "FirstBot") is True
    assert robots.is_allowed(parsed, "/second-only/", "SecondBot") is False


def test_repeated_agent_groups_after_crawl_delay_still_combine():
    parsed = robots.parse_robots(
        "User-agent: ExampleBot\nCrawl-delay: 1\nUser-agent: ExampleBot\nDisallow: /private/\n"
    )

    assert len(parsed["groups"]) == 2
    assert robots.crawl_delay(parsed, "ExampleBot") == 1.0
    assert robots.is_allowed(parsed, "/private/", "ExampleBot") is False


def test_sitemap_does_not_close_an_agent_run():
    parsed = robots.parse_robots(
        "User-agent: FirstBot\nSitemap: https://example.com/sitemap.xml\n"
        "User-agent: SecondBot\nDisallow: /private/\n"
    )

    assert parsed["groups"] == [
        {
            "user_agents": ["FirstBot", "SecondBot"],
            "allow": [],
            "disallow": ["/private/"],
            "crawl_delay": None,
            "request_rate_delay": None,
        }
    ]


def test_blank_user_agent_group_never_overrides_wildcard_or_absorbs_its_rules():
    parsed = robots.parse_robots(
        "User-agent: # retired bot\nDisallow: /old-secret/\n\nUser-agent: *\nDisallow: /\n"
    )

    assert len(parsed["groups"]) == 2
    assert parsed["groups"][0]["user_agents"] == [""]
    assert parsed["groups"][0]["disallow"] == ["/old-secret/"]
    assert robots.is_allowed(parsed, "/", "GPTBot") is False
    assert robots.is_allowed(parsed, "/old-secret/", "GPTBot") is False


def test_spider_uses_its_selected_agent_crawl_delay():
    robots_text = (
        "User-agent: SpiderHeadBot\nCrawl-delay: 10\nUser-agent: OtherBot\nCrawl-delay: 2\n"
    )

    result = crawl_site(
        "https://example.com/",
        min_delay=0,
        sleeper=lambda _delay: None,
        fetcher=lambda url: (
            Response(robots_text)
            if url.endswith("/robots.txt")
            else Response("<title>Home</title>")
        ),
    )

    assert result.crawl_delay_applied == 10.0
    assert result.effective_delay >= 10.0


def test_a_blank_user_agent_does_not_displace_the_wildcard_group():
    """#566: a name lost to a comment must not void the site's default policy."""
    parsed = robots.parse_robots(
        "User-agent: # old bot rule, no longer needed\nDisallow: /old-secret/\n\n"
        "User-agent: *\nDisallow: /\n"
    )

    # The blank group names nobody, so the wildcard group still rules GPTBot.
    assert robots.is_allowed(parsed, "/", "GPTBot") is False
    assert robots.is_allowed(parsed, "/anything", "GPTBot") is False


def test_a_bare_user_agent_line_names_no_crawler():
    parsed = robots.parse_robots(
        "User-agent:\nDisallow: /old-secret/\n\nUser-agent: *\nAllow: /\nDisallow: /private/\n"
    )

    # Rules under the blank group apply to nobody; the wildcard group applies.
    assert robots.is_allowed(parsed, "/old-secret/", "GPTBot") is True
    assert robots.is_allowed(parsed, "/private/", "GPTBot") is False


def test_a_blank_token_beside_a_real_name_leaves_that_name_matching():
    parsed = robots.parse_robots(
        "User-agent:\nUser-agent: GPTBot\nDisallow: /named/\n\nUser-agent: *\nDisallow: /\n"
    )

    assert robots.is_allowed(parsed, "/named/", "GPTBot") is False
    # The named group is the specific match, so the wildcard Disallow: / is not applied.
    assert robots.is_allowed(parsed, "/other/", "GPTBot") is True
    assert robots.is_allowed(parsed, "/other/", "OtherBot") is False


def test_the_most_specific_group_still_wins_over_the_wildcard():
    parsed = robots.parse_robots(
        "User-agent: *\nDisallow: /\n\nUser-agent: Googlebot\nAllow: /\n\n"
        "User-agent: Googlebot-Image\nDisallow: /photos/\n"
    )

    assert robots.is_allowed(parsed, "/page", "Googlebot") is True
    assert robots.is_allowed(parsed, "/photos/a.jpg", "Googlebot-Image") is False
    # Googlebot-Image is the longer matching token, so the shorter Googlebot
    # group does not also allow /photos/.
    assert robots.is_allowed(parsed, "/page", "OtherBot") is False


@pytest.mark.parametrize("text", ["", "\n\n", "# just a comment\n", "User-agent:\n"])
def test_a_robots_file_naming_nobody_leaves_everything_allowed(text):
    assert robots.is_allowed(robots.parse_robots(text), "/", "GPTBot") is True
