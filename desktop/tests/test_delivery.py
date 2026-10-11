"""Delivery regressions exercise source identity, icon containers and launch boundaries."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import struct
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from PyQt5.QtGui import QImage

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import bundle_manifest as manifest  # noqa: E402
import smoke_bundle as smoke  # noqa: E402
import source  # noqa: E402


def repository(root: Path) -> None:
    root.mkdir()
    (root / "pyproject.toml").write_text(
        '[project]\nname="seohead-seotools"\nversion="3.0.0"\n'
    )
    subprocess.run(["git", "init", "--quiet", str(root)], check=True)
    subprocess.run(["git", "add", "."], cwd=root, check=True)
    subprocess.run(
        [
            "git",
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
        cwd=root,
        check=True,
    )
    subprocess.run(
        [
            "git",
            "remote",
            "add",
            "origin",
            "https://github.com/PavloSEO/seohead-tools.git",
        ],
        cwd=root,
        check=True,
    )


class DeliveryTests(unittest.TestCase):
    def test_build_refuses_package_imports_from_another_checkout(self):
        core, desktop = Path("/selected/core"), Path("/selected/desktop")
        with patch.object(
            manifest.importlib.util,
            "find_spec",
            return_value=SimpleNamespace(origin="/different/seohead/__init__.py"),
        ), self.assertRaisesRegex(ValueError, "different source"):
            manifest.verify_import_sources(core, desktop)
        paths = [
            core / "seohead" / "__init__.py",
            desktop / "src" / "seohead_desktop" / "__init__.py",
        ]
        with patch.object(
            manifest.importlib.util,
            "find_spec",
            side_effect=[SimpleNamespace(origin=str(path)) for path in paths],
        ):
            manifest.verify_import_sources(core, desktop)

    def test_app_icon_manifest_and_native_containers(self):
        assets = ROOT / "src" / "seohead_desktop" / "assets"
        for entry in json.loads((assets / "asset-manifest.json").read_text()):
            self.assertEqual(
                hashlib.sha256((assets / entry["path"]).read_bytes()).hexdigest(),
                entry["sha256"],
                entry["path"],
            )
        for size in (16, 24, 32, 48, 64, 128, 256, 512, 1024):
            image = QImage(str(assets / "app" / f"seohead-{size}.png"))
            self.assertEqual((image.width(), image.height()), (size, size))
            self.assertEqual(image.pixelColor(0, 0).alpha(), 0)
            self.assertEqual(image.pixelColor(size // 2, size // 2).alpha(), 255)
            step = max(1, size // 64)
            if size > 32:  # the logo tile is bright blue; 16-32 px use the dark spider (brandbook)
                self.assertGreater(
                    max(
                        image.pixelColor(x, y).blue()
                        for x in range(0, size, step)
                        for y in range(0, size, step)
                    ),
                    200,
                )
        data = (assets / "app" / "seohead.ico").read_bytes()
        self.assertEqual(struct.unpack_from("<HHH", data), (0, 1, 7))
        for index, size in enumerate((16, 24, 32, 48, 64, 128, 256)):
            width, height, _, _, _, _, length, offset = struct.unpack_from(
                "<BBBBHHII", data, 6 + 16 * index
            )
            self.assertEqual((width or 256, height or 256), (size, size))
            self.assertEqual(
                data[offset : offset + length],
                (assets / "app" / f"seohead-{size}.png").read_bytes(),
            )
        icns = (assets / "app" / "seohead.icns").read_bytes()
        self.assertEqual(icns[:4], b"icns")
        self.assertEqual(struct.unpack_from(">I", icns, 4)[0], len(icns))

    def test_preview_identity_records_dirty_bytes_and_release_rejects_them(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "repo"
            repository(root)
            before = manifest.desktop_identity(root)
            (root / "untracked.py").write_text("value=1\n")
            with self.assertRaisesRegex(ValueError, "clean Git"):
                manifest.desktop_identity(root)
            after = manifest.desktop_identity(root, require_clean=False)
            self.assertTrue(after["dirty"])
            self.assertNotEqual(
                before["source_tree_sha256"], after["source_tree_sha256"]
            )
            self.assertIn(
                "untracked.py", {entry["path"] for entry in after["inventory"]}
            )

    def test_build_source_changes_are_rejected_and_cache_changes_with_dependencies(
        self,
    ):
        with tempfile.TemporaryDirectory() as directory:
            core, desktop = (Path(directory) / name for name in ("core", "desktop"))
            repository(core)
            repository(desktop)
            path = Path(directory) / "manifest.json"
            payload = manifest.create_manifest(
                core, path, "macos", desktop, preview=True
            )
            manifest.verify_sources(path, core, desktop)
            key = manifest.cache_key(path, "core")
            payload["build_environment"]["python"] = "different"
            path.write_text(json.dumps(payload))
            self.assertNotEqual(key, manifest.cache_key(path, "core"))
            (desktop / "new.py").write_text("changed=True\n")
            with self.assertRaisesRegex(ValueError, "changed during"):
                manifest.verify_sources(path, core, desktop)

    def test_smoke_never_executes_an_agent_outside_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory) / "bundle"
            resources = bundle / "resources"
            core = resources / "core" / "seohead"
            external = Path(directory) / "external" / "agent"
            core.parent.mkdir(parents=True)
            external.parent.mkdir()
            core.write_bytes(b"core")
            external.write_bytes(b"agent")
            payload = {
                "schema": "seohead.desktop.core-manifest.v1",
                "core": {
                    "cli_relpath": "core/seohead",
                    "cli_sha256": smoke.sha256_file(core),
                    "root_relpath": "core",
                    "inventory": smoke.inventory(core.parent),
                },
                "agent": {
                    "executable_relpath": "../../external/agent",
                    "root_relpath": "../../external",
                    "executable_sha256": smoke.sha256_file(external),
                    "inventory": smoke.inventory(external.parent),
                },
            }
            (resources / "core-manifest.json").write_text(json.dumps(payload))
            with patch.object(smoke.subprocess, "run") as run:
                with self.assertRaises(ValueError):
                    smoke.check_bundle(bundle)
                run.assert_not_called()

    def test_install_refuses_unmanaged_environment_and_mismatched_core(self):
        contract = json.loads((ROOT / "packaging" / "source-install.json").read_text())
        identity = {
            "repository": contract["core_repository"],
            "commit": contract["core_commit"],
        }
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(source, "_core_identity", return_value=identity),
        ):
            environment = Path(directory) / "existing"
            environment.mkdir()
            sentinel = environment / "user-file"
            sentinel.write_text("preserve")
            with self.assertRaisesRegex(ValueError, "already exists"):
                source.installation_plan(Path(directory), environment)
            with self.assertRaisesRegex(ValueError, "already exists"):
                source.installation_plan(Path(directory), environment, update=True)
            self.assertEqual(sentinel.read_text(), "preserve")
            identity["commit"] = "0" * 40
            with self.assertRaisesRegex(ValueError, "tested commit"):
                source.installation_plan(Path(directory), Path(directory) / "new")

    def test_install_plan_uses_one_environment_without_modifying_disk(self):
        contract = json.loads((ROOT / "packaging" / "source-install.json").read_text())
        identity = {
            "repository": contract["core_repository"],
            "commit": contract["core_commit"],
        }
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(source, "_core_identity", return_value=identity),
            patch.object(source.shutil, "which", return_value="/tools/uv"),
        ):
            environment = Path(directory) / "new"
            plan = source.installation_plan(Path(directory) / "core", environment)
            self.assertFalse(environment.exists())
            self.assertEqual(plan["commands"][0][-1], str(environment))
            self.assertEqual(
                plan["commands"][1][4], str(source.executable(environment, "python"))
            )
            self.assertEqual(plan["commands"][1].count("--editable"), 2)

    def test_source_launch_logs_output_exit_and_never_overwrites_log(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / "desktop.log"
            args = argparse.Namespace(log=log, dry_run=False)
            command = [
                sys.executable,
                "-c",
                "import sys; print('desktop fixture'); sys.exit(7)",
            ]
            with patch.object(source, "launch_command", return_value=command):
                self.assertEqual(source.run(args), 7)
                self.assertIn("desktop fixture", log.read_text())
                self.assertIn('"exit_code": 7', log.read_text())
                if os.name != "nt":
                    self.assertEqual(log.stat().st_mode & 0o777, 0o600)
                with self.assertRaises(FileExistsError):
                    source.run(args)


if __name__ == "__main__":
    unittest.main()
