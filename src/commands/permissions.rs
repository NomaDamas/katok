use crate::support::print_payload;
use anyhow::{Context, Result};
use serde::Serialize;

const FULL_DISK_ACCESS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";
const ACCESSIBILITY_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

#[derive(Serialize)]
struct PermissionOpenReport {
    platform: &'static str,
    dry_run: bool,
    opened: Vec<PermissionPaneReport>,
    note: &'static str,
}

#[derive(Serialize)]
struct PermissionPaneReport {
    pane: &'static str,
    url: &'static str,
    opened: bool,
}

pub(crate) fn open_macos(accessibility: bool, dry_run: bool, json: bool) -> Result<()> {
    let mut opened = vec![open_pane(
        "full_disk_access",
        FULL_DISK_ACCESS_URL,
        dry_run,
    )?];
    if accessibility {
        opened.push(open_pane("accessibility", ACCESSIBILITY_URL, dry_run)?);
    }

    print_payload(
        json,
        &PermissionOpenReport {
            platform: std::env::consts::OS,
            dry_run,
            opened,
            note: "macOS requires the user to grant Full Disk Access in System Settings; katok can open the pane but cannot self-grant TCC permissions",
        },
    )
}

#[cfg(target_os = "macos")]
fn open_pane(pane: &'static str, url: &'static str, dry_run: bool) -> Result<PermissionPaneReport> {
    if !dry_run {
        let status = std::process::Command::new("open")
            .arg(url)
            .status()
            .with_context(|| format!("open macOS permission pane: {pane}"))?;
        anyhow::ensure!(
            status.success(),
            "open macOS permission pane failed for {pane}"
        );
    }

    Ok(PermissionPaneReport {
        pane,
        url,
        opened: !dry_run,
    })
}

#[cfg(not(target_os = "macos"))]
fn open_pane(
    pane: &'static str,
    url: &'static str,
    _dry_run: bool,
) -> Result<PermissionPaneReport> {
    Ok(PermissionPaneReport {
        pane,
        url,
        opened: false,
    })
}
