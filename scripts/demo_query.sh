#!/bin/bash
# Answer the demo question against the demo index:
#   "이번 달에 내가 하기로 약속한 것들, 방 상관없이 다 모아줘"
#
# Sweeps Korean commitment endings, keeps only the owner's messages from the
# current demo month, and prints each hit in full.
set -euo pipefail

demo_dir="${KATOK_DEMO_DIR:-$HOME/Library/Application Support/katok-demo}"
owner="${KATOK_DEMO_OWNER:-김하늘}"
since="${KATOK_DEMO_SINCE:-2026-07-31T15:00:00}"  # KST 2026-08-01 00:00
work="$(mktemp -d)"
[ -n "$work" ] && [ -d "$work" ] || { echo "mktemp -d failed" >&2; exit 1; }
trap '[ -n "${work:-}" ] && /bin/rm -rf -- "$work"' EXIT

for phrase in 게요 할게 드릴게 겠습니다 께요 하기로 약속 예정 드리겠 보낼게; do
  katok --data-dir "$demo_dir" search keyword "$phrase" --limit 1000 --json \
    > "$work/$phrase.json" 2>/dev/null || echo '[]' > "$work/$phrase.json"
done

jq -s -c --arg owner "$owner" --arg since "$since" \
  '[.[][]] | unique_by(.chunk_id)
   | map(select(.started_at >= $since))
   | map(select(.sender_nickname == $owner))
   | sort_by(.started_at) | .[]' "$work"/*.json > "$work/hits.jsonl"

echo "본인 약속 후보 $(wc -l < "$work/hits.jsonl" | tr -d ' ')건 ($since 이후, 발신자=$owner)"
echo

while read -r id; do
  katok --data-dir "$demo_dir" chunk get "$id" --json
done < <(jq -r '.chunk_id' "$work/hits.jsonl") \
  | jq -s -r '.[] | "\(.started_at)  [\(.chat_name)]\n\(.text)\n---"'
