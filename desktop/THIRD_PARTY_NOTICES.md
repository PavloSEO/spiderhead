# Third-party notices

## PyQt5 and Qt

The pinned build dependency is PyQt5 5.15.11, copyright Riverbank Computing
Limited. Its installed metadata states GPL v3 and also describes a separate
commercial license. The source distribution includes the full GPLv3 text as
`LICENSE`; packaged artifacts copy it as
`licenses/SEOHEAD-Desktop-GPL-3.0-or-later.txt`.

PyQt5-Qt5 5.15.19 provides the Qt subset required by the binding. Its installed
metadata states LGPL v3. The build copies the exact wheel license to
`licenses/Qt-LGPL-3.0.txt`. `PyQt5-sip` and PyInstaller notices are copied from
the exact build environment as `PyQt5-sip-LICENSE.txt` and
`PyInstaller-COPYING.txt`. No claim is made that these preparation files are a
substitute for release review of the final build environment.

## SEOHEAD Tools core

The co-shipped `seohead-seotools` core is MIT-licensed. The build copies its
`LICENSE` and `THIRD_PARTY_NOTICES.md` from the exact, recorded source commit
into the application bundle.

## Assets

Roboto is copyright 2011 The Roboto Project Authors and is licensed under SIL
Open Font License 1.1. The exact `OFL.txt` is copied as
`licenses/Roboto-OFL-1.1.txt`.

Roboto Mono (`RobotoMono.ttf`, variable weight, Regular/Medium used) is copyright 2015 The Roboto
Mono Project Authors and is licensed under SIL Open Font License 1.1; source
`github.com/google/fonts/ofl/robotomono` (SHA-256 `a66a80e79d17e4c7cabd162e2916578a4cc08fd19eef6e2a643305eae9c567b2`).
Its `OFL-RobotoMono.txt` is copied as `licenses/RobotoMono-OFL-1.1.txt`.

The bundled Material Symbols SVG files come from Google Material Design Icons
under Apache License 2.0. The exact source URLs and SHA-256 hashes are retained
in `src/seohead_desktop/assets/asset-manifest.json`; the upstream Apache 2.0
text is copied as `licenses/Material-Design-Icons-Apache-2.0.txt`.

The SEOHEAD application mark (spider web in a magnifier with a black widow)
is the project's own brand logo, used with the project owner's authorization.
Its SVG sources (`app/seohead.svg`, full logo; `app/seohead-small.svg`,
simplified mark for 16–48 px) and their SHA-256 are recorded in the same asset
manifest. PNG, ICO and ICNS variants are generated from them by
`scripts/render_app_icon.py`.
