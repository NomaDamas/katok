#!/usr/bin/env bash
set -euo pipefail

KATOK_BIN="${KATOK_BIN:-katok}"

if ! command -v "$KATOK_BIN" >/dev/null 2>&1; then
  if [ -x "target/debug/katok" ]; then
    KATOK_BIN="target/debug/katok"
  else
    echo "katok binary not found. Run brew tap NomaDamas/katok git@github.com:NomaDamas/katok.git && brew install katok, cargo install katok, or set KATOK_BIN=/path/to/katok." >&2
    exit 127
  fi
fi

echo "Opening macOS permission settings..."
"$KATOK_BIN" permissions macos --accessibility
echo "Enable your terminal app for Full Disk Access. Enable Accessibility too if you plan to use KakaoTalk UI automation, then press Enter."
read -r _

echo "Checking KakaoTalk readiness..."
"$KATOK_BIN" doctor --macos-probe --json

echo "Syncing live macOS KakaoTalk archive..."
"$KATOK_BIN" sync --source macos --json

echo "Building local semantic index with EmbeddingGemma..."
"$KATOK_BIN" index --json

echo "Running semantic smoke search..."
"$KATOK_BIN" search semantic "최근 대화" --json
