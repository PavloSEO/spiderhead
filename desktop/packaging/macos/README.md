# Personal macOS installer

Build only, never run the installer as part of validation:

```bash
python packaging/macos/build.py --core-source /path/to/seohead-tools --python /path/to/build-venv/bin/python
pkgutil --payload-files dist/SEOHEAD-Desktop.pkg
```

PyInstaller is used because the existing, verified `scripts/build_macos.sh` already
co-ships a frozen core, native PyQt5 runtime, control agent, approved app icon and
source/runtime SHA-256 manifest. Reusing it keeps one package identity and smoke
contract. Source checkouts must be clean, with the selected core/desktop on the
build interpreter's import path. The build extra pins PyInstaller; core extras
must be installed in that environment. Generated files stay in ignored `dist/`.
No paid certificate, Developer ID signature or notarization is used. PyInstaller
and the existing bundler apply local ad-hoc signatures required by macOS runtime.

The pkg payload places the app in `/Applications/SEOHEAD Desktop.app` and its
same-version CLI wrapper in `/usr/local/bin/seohead`. The preinstall refuses symlinks and a foreign CLI, and verifies timestamped
backups of an existing SEOHEAD wrapper/app before replacement. The postinstall only checks
the installed payload and wrapper mode, respects the destination volume, and
starts no processes. It writes no agent configuration. Inspect the scripts before
manual installation. `SEOHEAD_APP_PATH` lets wrapper smoke tests point at the app
in the build directory; it is unnecessary after installation.

Pavel installs the package manually, with macOS administrator authorization:

```bash
sudo installer -pkg dist/SEOHEAD-Desktop.pkg -target /
```

For a DMG, add `--dmg`. The first-run screen offers **Install command line tool**:
it asks macOS for administrator authorization only after an explicit click.
Cancelling authorization makes no change. The app checks its verified co-shipped
core before offering this action. A regular source launch never installs a wrapper.

Manual uninstall: `sudo sh packaging/macos/uninstall.sh --yes`. It refuses a
foreign wrapper or unrecognized app, removes only this wrapper, archives the app
alongside its original location, and forgets this pkg receipt. Projects, settings,
agent entries and backups stay in place. `--yes DESTINATION_VOLUME` supports
isolated script validation. No system install/uninstall is performed by tests. Use `--output-dir dist/BUILD_ID`
for another build without replacing earlier artifacts.
