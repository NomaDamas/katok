# macOS first-run setup

`katok` can check KakaoTalk readiness automatically, but macOS does not allow a CLI to grant Full Disk Access or Accessibility by itself. The setup flow therefore does the next best thing:

1. Open the exact System Settings panes.
2. Ask the user to enable the current terminal app.
3. Run `katok doctor --json` to confirm KakaoTalk app/container/DB visibility.
4. Run `katok sync --source macos --json`.
5. Run `katok index --json`.
6. Run a semantic smoke search.

Use the helper:

```bash
scripts/katok-macos-setup.sh
```

With a custom binary:

```bash
KATOK_BIN=target/debug/katok scripts/katok-macos-setup.sh
```

## Why this cannot be fully automatic

Apple's TCC permission system requires a user gesture in System Settings for Full Disk Access and Accessibility. A local app can open the pane and verify the result, but it cannot silently grant itself permission.

## Expected result

After the permission toggles are enabled, `katok doctor --json` should report:

```json
{
  "source_adapter": {
    "macos": {
      "app_installed": true,
      "container_present": true,
      "db_file_count": 1
    }
  }
}
```

The exact `db_file_count` can vary by KakaoTalk account state.
