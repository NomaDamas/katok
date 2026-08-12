#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# ///
# ─── How to run ───
# uv run scripts/verify_history_privacy.py

from __future__ import annotations

import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Final


ROOT: Final = Path(__file__).resolve().parents[1]
UUID_LITERAL: Final = re.compile(
    rb"\b[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-"
    rb"[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\b"
)
PERSONAL_HOME: Final = re.compile(rb"/(?:Users|home)/[A-Za-z0-9._-]+/")
INTEGER_LITERAL: Final = re.compile(rb"(?<![0-9A-Za-z_])\d[\d_]*\d(?![0-9A-Za-z_])")
DB_NAME_LITERAL: Final = re.compile(rb"(?<![0-9A-Fa-f])[0-9a-f]{78}(?![0-9A-Fa-f])")
SAFE_SYNTHETIC_UUIDS: Final = {b"00000000-1111-2222-3333-444444444444"}
SAFE_SYNTHETIC_DB_NAMES: Final = {
    b"de345a8eb68ff0db3c1f8b94817936a00471d335162afc05cdfc758f638a33d427ea7742d4d420"
}
IDENTITY_PATH_SUFFIXES: Final = (
    "src/kakao/auth.rs",
    "src/kakao/derive.rs",
    "src/kakao/reader.rs",
    "tests/reader_synthetic_db.rs",
)


@dataclass(frozen=True, slots=True)
class Finding:
    commit_id: str
    path: str
    rule: str


def git(*args: str, cwd: Path) -> bytes:
    return subprocess.run(
        ["git", *args],
        cwd=cwd,
        check=True,
        capture_output=True,
    ).stdout


def commits(repo: Path, revision: str) -> tuple[str, ...]:
    return tuple(git("rev-list", "--reverse", revision, cwd=repo).decode().splitlines())


def changed_paths(repo: Path, commit_id: str) -> tuple[str, ...]:
    output = git(
        "diff-tree",
        "--root",
        "--no-commit-id",
        "--name-only",
        "-r",
        "-z",
        commit_id,
        cwd=repo,
    )
    return tuple(
        raw.decode("utf-8")
        for raw in output.split(b"\0")
        if raw
    )


def rules_for(path: str, content: bytes) -> tuple[str, ...]:
    rules: list[str] = []
    if PERSONAL_HOME.search(content):
        rules.append("personal-home-path")
    if any(match.group(0) not in SAFE_SYNTHETIC_UUIDS for match in UUID_LITERAL.finditer(content)):
        rules.append("non-synthetic-uuid")

    identity_path = path.endswith(IDENTITY_PATH_SUFFIXES)
    if identity_path and any(
        100_000_000 <= int(match.group(0).replace(b"_", b"")) <= 999_999_999
        for match in INTEGER_LITERAL.finditer(content)
    ):
        rules.append("plausible-kakao-user-id")
    if identity_path and any(
        match.group(0) not in SAFE_SYNTHETIC_DB_NAMES
        for match in DB_NAME_LITERAL.finditer(content)
    ):
        rules.append("non-synthetic-db-name")
    return tuple(rules)


def findings(repo: Path, revision: str) -> tuple[Finding, ...]:
    found: list[Finding] = []
    for commit_id in commits(repo, revision):
        for path in changed_paths(repo, commit_id):
            try:
                content = git("show", f"{commit_id}:{path}", cwd=repo)
            except subprocess.CalledProcessError:
                continue
            found.extend(
                Finding(commit_id=commit_id, path=path, rule=rule)
                for rule in rules_for(path, content)
            )
    return tuple(found)


def main() -> int:
    repo = Path(sys.argv[1]).resolve() if len(sys.argv) >= 2 else ROOT
    revision = sys.argv[2] if len(sys.argv) == 3 else "HEAD"
    found = findings(repo, revision)
    if not found:
        print("ok: git-history-privacy: all reachable history uses synthetic fixtures")
        return 0

    for item in found:
        print(
            f"fail: git-history-privacy: {item.rule}: "
            f"{item.commit_id[:12]} {item.path}",
            file=sys.stderr,
        )
    return 1


if __name__ == "__main__":
    sys.exit(main())
