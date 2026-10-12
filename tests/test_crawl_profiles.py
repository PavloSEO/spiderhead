"""Named crawl profiles (issue #941): a saved --config file selected by name.

The store is a directory of JSON files, so these tests point it at tmp_path and
never touch the operator's home directory.
"""

import json

import pytest

from seohead import cli
from seohead.crawl import profiles


@pytest.fixture
def store(tmp_path, monkeypatch):
    directory = tmp_path / "profiles"
    monkeypatch.setenv(profiles.DIR_ENV, str(directory))
    return directory


def _write(path, body):
    path.write_text(json.dumps(body), encoding="utf-8")
    return str(path)


def test_save_then_select_round_trips_the_validated_file(store, tmp_path):
    source = _write(tmp_path / "in.json", {"speed": {"concurrency": 2}})
    stored = profiles.save("catalog", source)
    assert stored == str(store / "catalog.json")
    assert profiles.saved_names() == ["catalog"]
    assert json.loads((store / "catalog.json").read_text(encoding="utf-8")) == {
        "speed": {"concurrency": 2}
    }
    assert profiles.path_for("catalog") == stored
    assert profiles.name_for_path(stored) == "catalog"
    assert profiles.name_for_path(str(tmp_path / "other.json")) is None


def test_selecting_an_unknown_profile_names_the_saved_ones(store, tmp_path):
    profiles.save("alpha", _write(tmp_path / "a.json", {}))
    with pytest.raises(profiles.ProfileError, match="saved profiles: alpha"):
        profiles.path_for("beta")


def test_empty_store_says_none(store):
    with pytest.raises(profiles.ProfileError, match="saved profiles: none"):
        profiles.path_for("anything")


def test_show_returns_validated_profile_json(store, tmp_path):
    source = _write(tmp_path / "a.json", {"speed": {"concurrency": 3}})
    profiles.save("fast", source)
    assert profiles.show("fast") == {"speed": {"concurrency": 3}}


def test_show_refuses_credentials_in_a_manually_edited_profile(store, tmp_path):
    store.mkdir()
    (store / "private.json").write_text(
        json.dumps(
            {
                "http": {
                    "credentials_acknowledged": True,
                    "credential_headers": [
                        {"host": "example.com", "headers": {"Authorization": "env:SEOHEAD_TOKEN"}}
                    ],
                }
            }
        ),
        encoding="utf-8",
    )
    with pytest.raises(profiles.ProfileError, match="cannot contain HTTP authentication"):
        profiles.show("private")


def test_show_refuses_basic_digest_auth_before_resolving_environment(store):
    store.mkdir()
    (store / "auth.json").write_text(
        json.dumps(
            {
                "http": {
                    "auth_acknowledged": True,
                    "auth": [
                        {
                            "host": "example.com",
                            "scheme": "digest",
                            "username_env": "env:SEOHEAD_MISSING_USER",
                            "password_env": "env:SEOHEAD_MISSING_PASSWORD",
                        }
                    ],
                }
            }
        ),
        encoding="utf-8",
    )
    with pytest.raises(profiles.ProfileError, match="cannot contain HTTP authentication"):
        profiles.show("auth")


def test_show_and_select_refuse_profile_symlinks(store, tmp_path):
    source = _write(tmp_path / "outside.json", {})
    store.mkdir()
    (store / "linked.json").symlink_to(source)
    with pytest.raises(profiles.ProfileError, match="must not be a symlink"):
        profiles.show("linked")
    with pytest.raises(profiles.ProfileError, match="must not be a symlink"):
        profiles.path_for("linked")


@pytest.mark.parametrize("name", ["", "Has Caps", "../escape", "a" * 65, "-leading"])
def test_names_outside_the_safe_pattern_are_refused(store, tmp_path, name):
    with pytest.raises(profiles.ProfileError, match="profile name"):
        profiles.save(name, _write(tmp_path / "a.json", {}))


def test_invalid_settings_are_refused_before_anything_is_written(store, tmp_path):
    bad = _write(tmp_path / "bad.json", {"no_such_setting": 1})
    with pytest.raises(profiles.ProfileError):
        profiles.save("broken", bad)
    assert not (store / "broken.json").exists()


def test_a_profile_cannot_store_credential_headers(store, tmp_path, monkeypatch):
    monkeypatch.setenv("SEOHEAD_TEST_TOKEN", "x")
    secret = _write(
        tmp_path / "auth.json",
        {
            "http": {
                "credentials_acknowledged": True,
                "credential_headers": [
                    {"host": "example.com", "headers": {"Authorization": "env:SEOHEAD_TEST_TOKEN"}}
                ],
            }
        },
    )
    with pytest.raises(profiles.ProfileError, match="cannot store HTTP authentication"):
        profiles.save("login", secret)
    assert not (store / "login.json").exists()


def test_cli_save_profile_stores_and_does_not_crawl(store, tmp_path, capsys):
    source = _write(tmp_path / "in.json", {"speed": {"concurrency": 3}})
    rc = cli.main(["crawl-site", "--save-profile", "fast", "--config", source])
    assert rc == 0
    assert json.loads(capsys.readouterr().out)["profile"] == "fast"
    assert (store / "fast.json").is_file()


def test_cli_save_profile_needs_a_config_file(store, capsys):
    rc = cli.main(["crawl-site", "--save-profile", "fast"])
    assert rc == 1
    assert "--save-profile needs --config" in capsys.readouterr().err


def test_cli_profile_and_config_are_mutually_exclusive(store, tmp_path, capsys):
    source = _write(tmp_path / "in.json", {})
    profiles.save("fast", source)
    rc = cli.main(
        ["crawl-site", "--url", "https://example.com", "--profile", "fast", "--config", source]
    )
    assert rc == 1
    assert "not both" in capsys.readouterr().err


def test_cli_profile_selects_the_stored_file_as_the_config(store, tmp_path, monkeypatch):
    source = _write(tmp_path / "in.json", {"speed": {"concurrency": 3}})
    profiles.save("fast", source)
    seen = {}

    def fake_crawl(**kwargs):
        seen.update(kwargs)
        return {"finish_reason": "complete"}

    monkeypatch.setitem(cli.handlers.HANDLERS, "crawl_site", fake_crawl)
    rc = cli.main(["crawl-site", "--url", "https://example.com", "--profile", "fast", "-q"])
    assert rc == 0
    assert seen["config"] == str(store / "fast.json")


def test_delete_removes_the_stored_profile(store, tmp_path):
    profiles.save("old", _write(tmp_path / "a.json", {}))
    profiles.delete("old")
    assert profiles.saved_names() == []


def test_deleting_an_unknown_profile_names_the_saved_ones(store, tmp_path):
    profiles.save("alpha", _write(tmp_path / "a.json", {}))
    with pytest.raises(profiles.ProfileError, match="saved profiles: alpha"):
        profiles.delete("beta")
    assert profiles.saved_names() == ["alpha"]


def test_cli_lists_and_deletes_profiles(store, tmp_path, capsys):
    profiles.save("beta", _write(tmp_path / "b.json", {}))
    profiles.save("alpha", _write(tmp_path / "a.json", {}))
    assert cli.main(["crawl-profile", "list"]) == 0
    assert json.loads(capsys.readouterr().out) == {"profiles": ["alpha", "beta"]}
    assert cli.main(["crawl-profile", "delete", "alpha"]) == 0
    assert json.loads(capsys.readouterr().out) == {"profile": "alpha", "deleted": True}
    assert profiles.saved_names() == ["beta"]


def test_cli_shows_profile_as_json(store, tmp_path, capsys):
    profiles.save("alpha", _write(tmp_path / "a.json", {"speed": {"concurrency": 3}}))
    assert cli.main(["crawl-profile", "show", "alpha"]) == 0
    assert json.loads(capsys.readouterr().out) == {"speed": {"concurrency": 3}}


def test_cli_delete_of_an_unknown_profile_fails(store, capsys):
    rc = cli.main(["crawl-profile", "delete", "ghost"])
    assert rc == 1
    assert "no crawl profile 'ghost'" in capsys.readouterr().err
