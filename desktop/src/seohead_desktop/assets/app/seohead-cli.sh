#!/bin/sh
# SEOHEAD_BUNDLED_CLI_V1 — installed with the matching Desktop app.
set -eu
app=${SEOHEAD_APP_PATH:-"/Applications/SEOHEAD Desktop.app"}
core="$app/Contents/Resources/core/seohead/seohead"
[ -x "$core" ] || { echo "SEOHEAD bundled core not found: $core" >&2; exit 69; }
exec "$core" "$@"
