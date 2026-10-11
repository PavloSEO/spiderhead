"""Packaging contracts use temporary files and never open a site or project."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from seohead_desktop.bundle import (
    bundled_core_cli,
    bundled_core_manifest,
    package_arguments,
    verified_bundled_core_identity,
)


def load_script(name: str):
    path = ROOT / "scripts" / name
    spec = importlib.util.spec_from_file_location(path.stem, path)
    module = importlib.util.module_from_spec(spec)
    assert spec and spec.loader
    spec.loader.exec_module(module)
    return module


MANIFEST = load_script("bundle_manifest.py")


class PackagingTests(unittest.TestCase):
    def test_macos_relative_core_resolution(self):
        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / "SEOHEAD Desktop.app"
            executable = app / "Contents" / "MacOS" / "SEOHEAD Desktop"
            resources = app / "Contents" / "Resources"
            core = resources / "core" / "seohead" / "seohead"
            executable.parent.mkdir(parents=True)
            core.parent.mkdir(parents=True)
            executable.touch()
            core.touch()
            (resources / "core-manifest.json").write_text(
                json.dumps(
                    {
                        "schema": "seohead.desktop.core-manifest.v1",
                        "core": {
                            "cli_relpath": "core/seohead/seohead",
                            "commit": "a" * 40,
                            "cli_sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                            "root_relpath": "core/seohead",
                            "inventory": [
                                {
                                    "path": "seohead",
                                    "kind": "file",
                                    "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                                }
                            ],
                        },
                    }
                ),
                encoding="utf-8",
            )
            found = bundled_core_manifest(executable)
            self.assertIsNotNone(found)
            self.assertEqual(bundled_core_cli(executable), core.resolve())
            self.assertEqual(verified_bundled_core_identity(executable)["commit"], "a" * 40)

    def test_core_hash_mismatch_is_not_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / "SEOHEAD Desktop.app"
            executable = app / "Contents" / "MacOS" / "SEOHEAD Desktop"
            resources = app / "Contents" / "Resources"
            core = resources / "core" / "seohead" / "seohead"
            executable.parent.mkdir(parents=True)
            core.parent.mkdir(parents=True)
            executable.touch()
            core.write_text("changed", encoding="utf-8")
            (resources / "core-manifest.json").write_text(
                json.dumps(
                    {
                        "schema": "seohead.desktop.core-manifest.v1",
                        "core": {
                            "cli_relpath": "core/seohead/seohead",
                            "commit": "b" * 40,
                            "cli_sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                            "root_relpath": "core/seohead",
                            "inventory": [],
                        },
                    }
                ),
                encoding="utf-8",
            )
            self.assertIsNone(verified_bundled_core_identity(executable))

    def test_internal_payload_tampering_is_not_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / "SEOHEAD Desktop.app"
            executable = app / "Contents" / "MacOS" / "SEOHEAD Desktop"
            resources = app / "Contents" / "Resources"
            core = resources / "core" / "seohead" / "seohead"
            internal = core.parent / "_internal" / "data.bin"
            executable.parent.mkdir(parents=True)
            internal.parent.mkdir(parents=True)
            executable.touch()
            core.write_bytes(b"launcher")
            internal.write_bytes(b"original")
            entries = [
                {"path": "_internal/data.bin", "kind": "file", "sha256": hashlib.sha256(b"original").hexdigest()},
                {"path": "seohead", "kind": "file", "sha256": hashlib.sha256(b"launcher").hexdigest()},
            ]
            (resources / "core-manifest.json").write_text(
                json.dumps(
                    {
                        "schema": "seohead.desktop.core-manifest.v1",
                        "core": {
                            "cli_relpath": "core/seohead/seohead",
                            "commit": "c" * 40,
                            "cli_sha256": hashlib.sha256(b"launcher").hexdigest(),
                            "root_relpath": "core/seohead",
                            "inventory": entries,
                        },
                    }
                ),
                encoding="utf-8",
            )
            internal.write_bytes(b"tampered")
            self.assertIsNone(verified_bundled_core_identity(executable))

    def test_non_hex_commit_is_not_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / "SEOHEAD Desktop.app"
            executable = app / "Contents" / "MacOS" / "SEOHEAD Desktop"
            resources = app / "Contents" / "Resources"
            core = resources / "core" / "seohead" / "seohead"
            executable.parent.mkdir(parents=True)
            core.parent.mkdir(parents=True)
            executable.touch()
            core.touch()
            (resources / "core-manifest.json").write_text(
                json.dumps(
                    {
                        "schema": "seohead.desktop.core-manifest.v1",
                        "core": {
                            "cli_relpath": "core/seohead/seohead",
                            "commit": "g" * 40,
                            "cli_sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                            "root_relpath": "core/seohead",
                            "inventory": [],
                        },
                    }
                ),
                encoding="utf-8",
            )
            self.assertIsNone(verified_bundled_core_identity(executable))

    def test_core_path_cannot_escape_resources(self):
        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / "SEOHEAD Desktop.app"
            executable = app / "Contents" / "MacOS" / "SEOHEAD Desktop"
            resources = app / "Contents" / "Resources"
            executable.parent.mkdir(parents=True)
            resources.mkdir(parents=True)
            executable.touch()
            (resources / "core-manifest.json").write_text(
                json.dumps(
                    {
                        "schema": "seohead.desktop.core-manifest.v1",
                        "core": {"cli_relpath": "../../outside"},
                    }
                ),
                encoding="utf-8",
            )
            self.assertIsNone(bundled_core_cli(executable))

    def test_explicit_core_override_wins_over_packaged_default(self):
        packaged = Path("/bundle/core/seohead")
        self.assertEqual(
            package_arguments(["--capture", "preview.png"], packaged),
            ["--core-cli", str(packaged), "--capture", "preview.png"],
        )
        self.assertEqual(
            package_arguments(["--core-cli", "/diagnostic/seohead"], packaged),
            ["--core-cli", "/diagnostic/seohead"],
        )

    def test_manifest_pins_clean_git_source(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "core"
            source.mkdir()
            (source / "pyproject.toml").write_text(
                '[project]\nname = "seohead-seotools"\nversion = "3.0.0"\n',
                encoding="utf-8",
            )
            subprocess.run(["git", "init"], cwd=source, check=True, capture_output=True)
            subprocess.run(
                ["git", "remote", "add", "origin", "https://github.com/PavloSEO/seohead-tools.git"],
                cwd=source,
                check=True,
            )
            subprocess.run(["git", "add", "pyproject.toml"], cwd=source, check=True)
            subprocess.run(
                [
                    "git",
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.test",
                    "commit",
                    "-m",
                    "fixture",
                ],
                cwd=source,
                check=True,
                capture_output=True,
            )
            manifest_path = Path(directory) / "core-manifest.json"
            payload = MANIFEST.create_manifest(source, manifest_path, "macos")
            self.assertEqual(payload["core"]["version"], "3.0.0")
            self.assertEqual(len(payload["core"]["commit"]), 40)
            self.assertEqual(len(payload["core"]["source_archive_sha256"]), 64)
            self.assertIn("python", payload["build_environment"])
            self.assertTrue(payload["build_environment"]["installed_distributions"])
            self.assertNotIn(str(source), manifest_path.read_text(encoding="utf-8"))

    def test_bundle_spec_requires_every_platform_layout(self):
        spec = json.loads((ROOT / "packaging" / "bundle-spec.json").read_text())
        self.assertEqual(spec["core"]["delivery"], "same-artifact")
        self.assertEqual(set(spec["platforms"]), {"macos", "linux", "windows"})
        for platform in spec["platforms"].values():
            self.assertTrue(platform["core_cli"].startswith("core/"))

    def test_gpl_text_and_runtime_notice_contract_are_present(self):
        self.assertEqual(
            hashlib.sha256((ROOT / "LICENSE").read_bytes()).hexdigest(),
            "3972dc9744f6499f0f9b2dbf76696f2ae7ad8af9b23dde66d6af86c9dfb36986",
        )
        notices = (ROOT / "scripts" / "copy_runtime_notices.py").read_text(
            encoding="utf-8"
        )
        for notice in (
            "Qt-LGPL-3.0.txt",
            "PyQt5-sip-LICENSE.txt",
            "PyInstaller-COPYING.txt",
        ):
            self.assertIn(notice, notices)

    def test_every_platform_build_includes_the_bounded_control_agent(self):
        scripts = {
            "macos": ROOT / "scripts" / "build_macos.sh",
            "linux": ROOT / "scripts" / "build_linux.sh",
            "windows": ROOT / "scripts" / "build_windows.ps1",
        }
        for platform, path in scripts.items():
            source = path.read_text(encoding="utf-8")
            self.assertIn("seohead-desktop-agent", source, platform)
            self.assertIn("control_entrypoint.py", source, platform)
            self.assertIn("--agent", source, platform)


if __name__ == "__main__":
    unittest.main()
