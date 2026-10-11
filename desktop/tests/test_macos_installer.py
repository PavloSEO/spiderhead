"""Installer script contracts use only a disposable destination volume."""
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from seohead_desktop import cli_install

ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / "packaging/macos"
WRAPPER = ROOT / "src/seohead_desktop/assets/app/seohead-cli.sh"


@unittest.skipUnless(os.name == "posix", "POSIX installer script validation")
class InstallerTests(unittest.TestCase):
    def test_foreign_wrapper_is_refused_and_owned_wrapper_is_backed_up(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            wrapper = root / "usr/local/bin/seohead"
            wrapper.parent.mkdir(parents=True)
            wrapper.write_text("foreign wrapper")
            command = ["sh", str(SCRIPTS / "preinstall"), "package", "location", str(root)]
            result = subprocess.run(command, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(wrapper.read_text(), "foreign wrapper")
            wrapper.write_bytes(WRAPPER.read_bytes())
            result = subprocess.run(command, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            backups = list(wrapper.parent.glob("seohead.seohead-before-*"))
            self.assertEqual(len(backups), 1)
            self.assertEqual(backups[0].read_bytes(), wrapper.read_bytes())

    def test_postinstall_and_uninstall_require_only_the_fake_payload(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            app = root / "Applications/SEOHEAD Desktop.app"
            core = app / "Contents/Resources/core/seohead/seohead"
            core.parent.mkdir(parents=True)
            core.write_text("#!/bin/sh\nexit 0\n")
            core.chmod(0o755)
            (app / "Contents/Resources/core-manifest.json").write_text('{"schema":"seohead.desktop.core-manifest.v1"}')
            wrapper = root / "usr/local/bin/seohead"
            wrapper.parent.mkdir(parents=True)
            wrapper.write_bytes(WRAPPER.read_bytes())
            subprocess.run(["sh", str(SCRIPTS / "postinstall"), "package", "location", str(root)], check=True)
            self.assertTrue(os.access(wrapper, os.X_OK))
            subprocess.run(["sh", str(SCRIPTS / "uninstall.sh"), "--yes", str(root)], check=True)
            self.assertFalse(wrapper.exists())
            self.assertFalse(app.exists())
            self.assertEqual(len(list(app.parent.glob("*.uninstalled-*"))), 1)

    def test_source_launch_cannot_request_administrator_authorization(self):
        with patch.object(cli_install, "verified_bundled_core_identity", return_value=None), patch.object(cli_install.subprocess, "run") as run:
            with self.assertRaises(ValueError):
                cli_install.install_cli()
            run.assert_not_called()
