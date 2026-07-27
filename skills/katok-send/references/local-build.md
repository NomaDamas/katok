# Binary and build

## Which binary runs

- **`~/.local/bin/katok`**, built locally, is the only intended target.
- The Homebrew build from `NomaDamas/katok` has been uninstalled deliberately. `/opt/homebrew/bin` precedes `~/.local/bin` on PATH, so reinstalling it puts an older binary without `send` in front without any warning. Do not reinstall it.
- Source: this repository, branch `feat/ax-send`.

```bash
cargo build --release
cp target/release/katok ~/.local/bin/katok
katok send --room x --list-windows --json   # confirm the new binary is in front
```

## Relationship to upstream

This repository is `aldegad/katok`, a fork of `NomaDamas/katok`. Sending does not exist upstream; it is local to the fork.

Upstream aims at a read-only tool, so whether sending is ever proposed upstream is undecided. The implementation is kept deliberately shallow so that decision stays cheap:

- All Accessibility code lives in one file, `src/kakao/ax_send.rs`.
- Every part of it sits behind `#[cfg(target_os = "macos")]`, so other targets are unaffected.
- macOS-only crates (`core-foundation`, `core-graphics`) are declared under `[target.'cfg(target_os = "macos")'.dependencies]`.
- Only three existing files change: `cli.rs` (the `Send` variant), `commands.rs` (routing plus `run_send`), and `kakao/mod.rs` (module registration).

Those same three files are the likely conflict points on `git pull upstream main`. `ax_send.rs` is a new file and will not conflict.

## Why this is implemented directly instead of wrapping a third-party tool

The first version wrapped `channprj/kmsg`, a Swift tool. It worked, but it left a third-party binary dependency, and sending required installing and building a separate tool. Only one path out of kmsg was actually needed — put text in an open window and press Enter — while most of its size went to the automatic room-opening path, which is too slow to use. AX is a C API callable directly over FFI, and the clipboard is reachable through the already-linked Carbon Pasteboard Manager, so no AppKit binding is needed either.
