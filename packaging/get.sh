#!/bin/sh
# SpiderHead one-line installer: the `spiderhead` command line tool for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/PavloSEO/spiderhead/main/packaging/get.sh | sh
#   ... | sh -s -- --version 0.10.0 --prefix "$HOME/.local"
#
# Downloads the binary for this machine from the GitHub release, checks it against
# SHA256SUMS and, when the release is signed and OpenSSL can verify Ed25519, against
# SHA256SUMS.sig. Nothing is installed when a check fails.
#
# Test hooks: SPIDERHEAD_BASE overrides the releases URL.
set -eu

BASE=${SPIDERHEAD_BASE:-https://github.com/PavloSEO/spiderhead/releases}
VERSION=""
PREFIX="${HOME:-}/.local"

# Release signing public key (Ed25519, PEM). Empty until the first signed release; then the
# installer refuses an unsigned or wrongly signed SHA256SUMS where OpenSSL can check it.
PUBKEY=''

while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION=${2:?}; shift 2 ;;
    --prefix) PREFIX=${2:?}; shift 2 ;;
    -h|--help) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

fail() { echo "error: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "$1 is required"; }
need curl

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64) TARGET=x86_64-apple-darwin ;;
  Linux-x86_64) TARGET=x86_64-unknown-linux-gnu ;;
  Linux-aarch64|Linux-arm64) TARGET=aarch64-unknown-linux-gnu ;;
  *) fail "no SpiderHead build for $(uname -s) $(uname -m)" ;;
esac

if [ -n "$VERSION" ]; then URL="$BASE/download/v${VERSION#v}"; else URL="$BASE/latest/download"; fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM
cd "$TMP"

echo "Fetching checksums ($TARGET)..."
curl -fsSL -o SHA256SUMS "$URL/SHA256SUMS" || fail "cannot download $URL/SHA256SUMS"

# Signature: verified when possible, required once a public key is baked in.
if [ -n "$PUBKEY" ]; then
  curl -fsSL -o SHA256SUMS.sig "$URL/SHA256SUMS.sig" || fail "release is not signed (no SHA256SUMS.sig)"
  printf '%s\n' "$PUBKEY" > key.pub
  if command -v openssl >/dev/null 2>&1 \
     && openssl pkeyutl -verify -pubin -inkey key.pub -rawin -in SHA256SUMS -sigfile SHA256SUMS.sig >/dev/null 2>&1; then
    echo "Signature OK."
  else
    fail "signature check failed (or this OpenSSL cannot verify Ed25519: use OpenSSL 3)"
  fi
fi

NAME=$(awk -v t="-$TARGET" '{ n=$2; sub(/^\*/, "", n); if (n ~ /^spiderhead-[0-9]/ && substr(n, length(n)-length(t)+1) == t) print n }' SHA256SUMS | head -n 1)
[ -n "$NAME" ] || fail "this release has no build for $TARGET"

echo "Downloading $NAME..."
curl -fsSL -o "$NAME" "$URL/$NAME" || fail "cannot download $URL/$NAME"

if command -v sha256sum >/dev/null 2>&1; then SUM="sha256sum -c -"; else need shasum; SUM="shasum -a 256 -c -"; fi
grep -F " $NAME" SHA256SUMS | $SUM >/dev/null || fail "checksum mismatch for $NAME"

mkdir -p "$PREFIX/bin"
install -m 755 "$NAME" "$PREFIX/bin/spiderhead"
echo "Installed $("$PREFIX/bin/spiderhead" --version) to $PREFIX/bin/spiderhead"
case ":$PATH:" in *":$PREFIX/bin:"*) ;; *) echo "Add $PREFIX/bin to your PATH." ;; esac
