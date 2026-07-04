#!/usr/bin/env python3
from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


@dataclass(frozen=True, slots=True)
class CheckResult:
    name: str
    ok: bool
    detail: str


def read_text(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def check(name: str, condition: bool, detail: str) -> CheckResult:
    return CheckResult(name=name, ok=condition, detail=detail)


def main() -> int:
    cargo = read_text("Cargo.toml")
    release = read_text(".github/workflows/release.yml")
    ci = read_text(".github/workflows/ci.yml")
    readme = read_text("README.md")
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
