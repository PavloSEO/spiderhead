#!/usr/bin/env bash
# Thin wrapper around the Export Analyzer CLI for the skill.
# Usage:
#   run_audit.sh exports   <exports_dir>       [out_dir] [profile]
#   run_audit.sh seospider <file.seospider>    [out_dir] [profile]
#   run_audit.sh crawl     <https://example.com> [out_dir] [profile]
set -euo pipefail

MODE="${1:?mode: exports|seospider|crawl}"
INPUT="${2:?input path or url}"
OUT="${3:-report}"
PROFILE="${4:-full}"

case "$MODE" in
  exports)   FLAG="--exports-dir" ;;
  seospider) FLAG="--load-crawl" ;;
  crawl)     FLAG="--crawl" ;;
  *) echo "unknown mode: $MODE" >&2; exit 1 ;;
esac

seohead sf run "$FLAG" "$INPUT" --profile "$PROFILE" --out "$OUT"
echo "report: $OUT/audit.json  $OUT/audit.md"
