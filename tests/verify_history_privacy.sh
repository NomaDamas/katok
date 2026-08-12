#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo="$(mktemp -d "${TMPDIR:-/tmp}/katok-history-privacy.XXXXXX")"
trap 'rm -rf "${repo}"' EXIT

git -C "${repo}" init -q
git -C "${repo}" config user.email test@example.invalid
git -C "${repo}" config user.name Test
mkdir -p "${repo}/src/kakao"

uuid_prefix="AAAAAAAA-BBBB-CCCC"
uuid_suffix="DDDD-EEEEEEEEEEEE"
printf 'const UUID: &str = "%s-%s";\n' \
  "${uuid_prefix}" "${uuid_suffix}" > "${repo}/src/kakao/auth.rs"
git -C "${repo}" add .
git -C "${repo}" commit -qm leaked

cat > "${repo}/src/kakao/auth.rs" <<'EOF'
const UUID: &str = "00000000-1111-2222-3333-444444444444";
EOF
git -C "${repo}" commit -qam sanitized-tip

if python3 "${root}/scripts/verify_history_privacy.py" "${repo}" HEAD >/dev/null 2>&1; then
  echo "history scanner missed a sensitive value hidden in an earlier commit" >&2
  exit 1
fi

git -C "${repo}" filter-branch -f --tree-filter \
  'if test -f src/kakao/auth.rs; then
     printf "%s\n" "const UUID: &str = \"00000000-1111-2222-3333-444444444444\";" > src/kakao/auth.rs
   fi' -- --all >/dev/null 2>&1
git -C "${repo}" for-each-ref --format='delete %(refname)' refs/original/ |
  git -C "${repo}" update-ref --stdin
git -C "${repo}" reflog expire --expire=now --all
git -C "${repo}" gc --prune=now --quiet

python3 "${root}/scripts/verify_history_privacy.py" "${repo}" HEAD >/dev/null
echo "ok: history privacy scanner rejects old leaks and accepts rewritten history"
