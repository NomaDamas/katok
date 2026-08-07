#!/bin/bash
# Build a throwaway katok index from the synthetic demo fixture.
#
# The demo index lives in its own --data-dir, so it never touches the real
# archive. Every name and message in it is invented.
#
#   ./scripts/demo_index.sh            # build, then print the demo commands
#   ./scripts/demo_index.sh --reset    # discard the demo archive and rebuild
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture="$repo_root/fixtures/demo/demo_messages.jsonl"
demo_dir="${KATOK_DEMO_DIR:-$HOME/Library/Application Support/katok-demo}"

if [ -z "${demo_dir:-}" ]; then
  echo "demo_dir resolved empty; refusing to continue" >&2
  exit 1
fi

if [ "${1:-}" = "--reset" ]; then
  for stale in "$demo_dir/archive.sqlite3" "$demo_dir/archive.sqlite3-journal" "$demo_dir/status.json"; do
    [ -f "$stale" ] && /bin/rm -f -- "$stale"
  done
  echo "reset: cleared demo archive in $demo_dir"
fi

mkdir -p "$demo_dir"
python3 "$repo_root/scripts/generate_demo_fixture.py" "$fixture"
katok --data-dir "$demo_dir" sync "$fixture" --source fixture --json

cat <<EOF

demo index ready at: $demo_dir
run the demo with --data-dir so the real archive is never opened:

  katok --data-dir "$demo_dir" source chats --json
  katok --data-dir "$demo_dir" search keyword "드리겠" --limit 1000 --json
  katok --data-dir "$demo_dir" search keyword "게요" --limit 1000 --json
  katok --data-dir "$demo_dir" chunk get <chunk-id> --json

owner persona is 김하늘; "this month" is 2026-08 (fixture also carries 2026-07
commitments so the date filter has something to reject).
EOF
