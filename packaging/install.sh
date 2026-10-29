#!/bin/sh
# SpiderHead installer for macOS and Linux, run from a dist folder.
#
#   ./install.sh                       interactive menu
#   ./install.sh --profile cli         lite | cli | desktop | all
#   ./install.sh --profile all --prefix "$HOME/.local" [--from DIR] [--uninstall]
#
# Layout under PREFIX: lib/spiderhead/bin/* (real files), bin/* (symlinks, except for
# "lite"), share/spiderhead/profile.toml (default desktop mode: simple or agent).
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
PROFILE=""
PREFIX=""
FROM="$HERE"
UNINSTALL=0

usage() { sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --profile) PROFILE=${2:?}; shift 2 ;;
    --prefix) PREFIX=${2:?}; shift 2 ;;
    --from) FROM=${2:?}; shift 2 ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help) usage 0 ;;
    *) echo "unknown argument: $1" >&2; usage 2 ;;
  esac
done

if [ -z "$PREFIX" ]; then
  if [ "$(id -u)" = 0 ]; then PREFIX=/usr/local; else PREFIX="$HOME/.local"; fi
fi

if [ -z "$PROFILE" ]; then
  echo "SpiderHead installer. What to install?"
  echo "  1) Лёгкая  - desktop window, simple mode, no CLI on PATH"
  echo "  2) CLI     - spiderhead + spiderhead-mcp on PATH, no window"
  echo "  3) Desktop - window (agent mode) + control tool + CLI"
  echo "  4) Всё     - everything, with docs"
  printf "Choice [3]: "
  read -r ans || ans=""
  case "${ans:-3}" in
    1) PROFILE=lite ;; 2) PROFILE=cli ;; 3) PROFILE=desktop ;; 4) PROFILE=all ;;
    *) echo "invalid choice" >&2; exit 2 ;;
  esac
fi
case "$PROFILE" in lite|cli|desktop|all) ;; *) echo "unknown profile: $PROFILE" >&2; exit 2 ;; esac

# FROM is either a profile folder (has profile.toml) or a dist folder holding profile folders.
if [ -f "$FROM/profile.toml" ]; then SRC="$FROM"; else SRC="$FROM/$PROFILE"; fi
[ -f "$SRC/profile.toml" ] || { echo "no $PROFILE profile in $FROM (run: cargo xtask dist)" >&2; exit 1; }

LIB="$PREFIX/lib/spiderhead/bin"
BIN="$PREFIX/bin"
SHARE="$PREFIX/share/spiderhead"
names=$(ls "$SRC/bin")

if [ "$UNINSTALL" = 1 ]; then
  for n in $names; do rm -f "$BIN/$n" "$LIB/$n"; done
  rm -f "$SHARE/profile.toml"
  echo "removed $PROFILE binaries from $PREFIX"
  exit 0
fi

# Verify checksums before copying anything.
if [ -f "$SRC/SHA256SUMS" ]; then
  if command -v sha256sum >/dev/null 2>&1; then SUM="sha256sum -c"; else SUM="shasum -a 256 -c"; fi
  (cd "$SRC" && $SUM SHA256SUMS >/dev/null) || { echo "checksum mismatch in $SRC" >&2; exit 1; }
fi

mkdir -p "$LIB" "$SHARE"
for n in $names; do install -m 755 "$SRC/bin/$n" "$LIB/$n"; done
install -m 644 "$SRC/profile.toml" "$SHARE/profile.toml"
if grep -q '^path_cli = true' "$SRC/profile.toml"; then
  mkdir -p "$BIN"
  for n in $names; do ln -sf "$LIB/$n" "$BIN/$n"; done
fi
if [ "$(uname -s)" = Linux ] && [ -f "$LIB/spiderhead-desktop" ]; then
  mkdir -p "$PREFIX/share/applications"
  printf '[Desktop Entry]\nType=Application\nName=SpiderHead\nExec=%s/spiderhead-desktop\nCategories=Development;\n' "$LIB" \
    > "$PREFIX/share/applications/spiderhead.desktop"
fi
echo "installed profile '$PROFILE' into $PREFIX"
case ":$PATH:" in *":$BIN:"*) ;; *) grep -q '^path_cli = true' "$SRC/profile.toml" && echo "add $BIN to PATH" ;; esac
