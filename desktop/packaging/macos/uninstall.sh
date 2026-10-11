#!/bin/sh
# Explicit manual removal of this package only; user data and agent configs stay local.
set -eu
[ "${1:-}" = "--yes" ] || { echo "Usage: sudo $0 --yes [DESTINATION_VOLUME]" >&2; exit 64; }
volume=${2:-/}
wrapper="$volume/usr/local/bin/seohead"
app="$volume/Applications/SEOHEAD Desktop.app"
[ ! -e "$wrapper" ] || {
    grep -q 'SEOHEAD_BUNDLED_CLI_V1' "$wrapper" || { echo "Foreign CLI wrapper; refusing removal" >&2; exit 73; }
    rm "$wrapper"
}
[ ! -e "$app" ] || {
    [ -f "$app/Contents/Resources/core-manifest.json" ] || { echo "Unrecognized app; refusing removal" >&2; exit 73; }
    # Archive instead of deleting the application permanently.
    mv "$app" "$app.uninstalled-$(date -u +%Y%m%dT%H%M%SZ)"
}
[ "$volume" != / ] || pkgutil --forget tech.seohead.desktop >/dev/null
