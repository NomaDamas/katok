#!/usr/bin/env bash
set -euo pipefail

KATOK_BIN="${KATOK_BIN:-katok}"

if ! command -v "$KATOK_BIN" >/dev/null 2>&1; then
  if [ -x "target/debug/katok" ]; then
    KATOK_BIN="target/debug/katok"
  else
    echo "katok binary not found. Run brew tap NomaDamas/katok https://github.com/NomaDamas/katok.git && brew install katok, cargo install katok, or set KATOK_BIN=/path/to/katok." >&2
    exit 127
  fi
fi

echo "Opening macOS permission settings..."
PERMISSION_ARGS=(macos)
if [ "${KATOK_ENABLE_ACCESSIBILITY:-0}" = "1" ]; then
  PERMISSION_ARGS+=(--accessibility)
fi
"$KATOK_BIN" permissions "${PERMISSION_ARGS[@]}"
echo "Enable only the app that invokes katok for Full Disk Access, then press Enter. Accessibility is not needed by the default read-only build."
read -r _

echo "Checking KakaoTalk readiness..."
"$KATOK_BIN" doctor --macos-probe --json

echo "Syncing live macOS KakaoTalk archive..."
"$KATOK_BIN" sync --source macos --json

echo "Building local semantic index with EmbeddingGemma..."
"$KATOK_BIN" index --json

echo "Running semantic smoke search..."
"$KATOK_BIN" search semantic "최근 대화" --json
