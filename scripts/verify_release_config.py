#!/usr/bin/env python3
from __future__ import annotations

import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
UUID_LITERAL = re.compile(
    r"\b[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-"
    r"[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\b"
)
PERSONAL_HOME = re.compile(r"/(?:Users|home)/[A-Za-z0-9._-]+/")
SAFE_SYNTHETIC_UUIDS = {"00000000-1111-2222-3333-444444444444"}
SAFE_SYNTHETIC_DB_NAMES = {
    "de345a8eb68ff0db3c1f8b94817936a00471d335162afc05cdfc758f638a33d427ea7742d4d420"
}
IDENTITY_FIXTURE_PATHS = {
    "src/kakao/auth.rs",
    "src/kakao/derive.rs",
    "src/kakao/reader.rs",
    "tests/reader_synthetic_db.rs",
}
INTEGER_LITERAL = re.compile(r"(?<![0-9A-Za-z_])\d[\d_]*\d(?![0-9A-Za-z_])")
DB_NAME_LITERAL = re.compile(r"(?<![0-9A-Fa-f])[0-9a-f]{78}(?![0-9A-Fa-f])")


@dataclass(frozen=True, slots=True)
class CheckResult:
    name: str
    ok: bool
    detail: str


def read_text(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def check(name: str, condition: bool, detail: str) -> CheckResult:
    return CheckResult(name=name, ok=condition, detail=detail)


def tracked_text() -> dict[str, str]:
    result = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=ROOT,
        check=True,
        capture_output=True,
    )
    texts: dict[str, str] = {}
    for raw_path in result.stdout.split(b"\0"):
        if not raw_path:
            continue
        relative = raw_path.decode("utf-8")
        try:
            texts[relative] = (ROOT / relative).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
    return texts


def main() -> int:
    tracked = tracked_text()
    cargo = read_text("Cargo.toml")
    release = read_text(".github/workflows/release.yml")
    ci = read_text(".github/workflows/ci.yml")
    readme = read_text("README.md")
    gitignore = read_text(".gitignore")
    formula = read_text("Formula/katok.rb")
    setup_script = read_text("scripts/katok-macos-setup.sh")
    has_dependency_path = re.search(r"\{[^}\n]*path\s*=", cargo) is not None
    commit_formula_match = re.search(
        r"- name: Commit formula(?P<body>.*?)(?:\n\s+- name:|\Z)",
        release,
        re.DOTALL,
    )
    commit_formula_body = (
        commit_formula_match.group("body") if commit_formula_match is not None else ""
    )
    private_tool_paths = sorted(
        path for path in tracked if path == ".omo" or path.startswith(".omo/")
    )
    personal_home_paths = sorted(
        path for path, text in tracked.items() if PERSONAL_HOME.search(text)
    )
    non_synthetic_uuid_paths = sorted(
        path
        for path, text in tracked.items()
        if any(match.group(0) not in SAFE_SYNTHETIC_UUIDS for match in UUID_LITERAL.finditer(text))
    )
    plausible_real_user_id_paths = sorted(
        path
        for path, text in tracked.items()
        if path in IDENTITY_FIXTURE_PATHS
        and any(
            100_000_000 <= int(match.group(0).replace("_", "")) <= 999_999_999
            for match in INTEGER_LITERAL.finditer(text)
        )
    )
    non_synthetic_db_name_paths = sorted(
        path
        for path, text in tracked.items()
        if path in IDENTITY_FIXTURE_PATHS
        and any(
            match.group(0) not in SAFE_SYNTHETIC_DB_NAMES
            for match in DB_NAME_LITERAL.finditer(text)
        )
    )
    live_oracle_paths = sorted(
        path
        for path, text in tracked.items()
        if path.startswith(("src/", "tests/", "skills/"))
        and re.search(
            r"\b(?:reference machine|empirically verified|measured on|live archive|live install)\b",
            text,
            re.IGNORECASE,
        )
    )

    checks = [
        check("package-name", 'name = "katok"' in cargo, "Cargo package is named katok"),
        check(
            "repository",
            'repository = "https://github.com/NomaDamas/katok"' in cargo,
            "Cargo metadata points at the release repository",
        ),
        check(
            "no-workspace-path-deps",
            "[workspace]" not in cargo
            and not has_dependency_path
            and "katok-core" not in cargo
            and "katok-adapters" not in cargo
            and "katok-kakao" not in cargo,
            "Cargo manifest has no internal workspace/path dependency",
        ),
        check(
            "release-tag-trigger",
            re.search(r"tags:\s*\n\s+- \"v\*\"", release) is not None
            and "workflow_dispatch" not in release,
            "Release workflow runs only on v* tags",
        ),
        check(
            "crates-token",
            "CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}" in release,
            "Release workflow uses Cargo's crates.io token environment variable",
        ),
        check(
            "homebrew-tap",
            "repository: NomaDamas/homebrew-katok" not in release
            and "HOMEBREW_TAP_TOKEN" not in release
            and "ref: main" in release
            and "path: tap" in release,
            "Release workflow updates Formula/katok.rb in the same repository",
        ),
        check(
            "macos-artifacts",
            "aarch64-apple-darwin" in release
            and "dist/*.tar.gz" in release,
            "Release workflow builds the supported Apple Silicon macOS archive",
        ),
        check(
            "formula-contract",
            "class Katok < Formula" in release
            and 'system "cargo", "install", *std_cargo_args' in release
            and "katok doctor --json" in release,
            "Generated Homebrew formula installs via cargo and documents macOS permission check",
        ),
        check(
            "homebrew-https-url",
            "git@github.com:NomaDamas/katok.git" not in "\n".join(
                [readme, formula, release, setup_script],
            )
            and "https://github.com/NomaDamas/katok.git" in readme
            and 'url "https://github.com/NomaDamas/katok.git"' in formula
            and 'url "https://github.com/NomaDamas/katok.git"' in release,
            "Homebrew installation docs and formula URLs use HTTPS instead of SSH",
        ),
        check(
            "formula-commit-tag-env",
            "git commit -m \"feat(katok): update to ${TAG}\"" in commit_formula_body
            and "TAG: ${{ needs.validate.outputs.tag }}" in commit_formula_body,
            "Homebrew formula commit step has TAG in its own environment",
        ),
        check(
            "formula-revision-from-trigger-sha",
            'revision="${GITHUB_SHA}"' in release
            and 'git rev-list -n 1 "${TAG}"' not in release,
            "Homebrew formula revision uses the tag-triggering commit without requiring fetched tags",
        ),
        check(
            "ci-preflight",
            "cargo publish --dry-run" in ci
            and "python3 scripts/verify_release_config.py" in ci
            and "cargo clippy --all-targets -- -D warnings" in ci,
            "CI runs lint, package, and release-config preflights",
        ),
        check(
            "release-preflight",
            "cargo publish --dry-run" in release
            and "python3 scripts/verify_release_config.py" in release
            and "cargo clippy --all-targets -- -D warnings" in release,
            "Release validation runs lint, package, and release-config preflights",
        ),
        check(
            "no-private-tool-state",
            not private_tool_paths,
            "Tracked release tree excludes private tool state such as .omo/",
        ),
        check(
            "private-tool-state-ignored",
            "/.omo/" in gitignore and "!/.omo/" not in gitignore,
            "Git ignore rules block the complete private .omo tree without exceptions",
        ),
        check(
            "no-personal-home-paths",
            not personal_home_paths,
            "Tracked text excludes absolute personal home-directory paths",
        ),
        check(
            "synthetic-uuid-fixtures",
            not non_synthetic_uuid_paths,
            "UUID literals are limited to the explicit synthetic fixture value",
        ),
        check(
            "synthetic-user-id-fixtures",
            not plausible_real_user_id_paths,
            "Identity fixtures exclude plausible real Kakao user-id literals",
        ),
        check(
            "synthetic-db-name-fixtures",
            not non_synthetic_db_name_paths,
            "Derived DB-name oracles are limited to the synthetic fixture value",
        ),
        check(
            "no-live-derived-oracles",
            not live_oracle_paths,
            "Source, tests, and skills state contracts without private live-observation provenance",
        ),
    ]

    failed = [result for result in checks if not result.ok]
    for result in checks:
        status = "ok" if result.ok else "fail"
        print(f"{status}: {result.name}: {result.detail}")

    if failed:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
