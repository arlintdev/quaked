//! Sign-in tokens in this Mac's login Keychain (never in config.json, which is shared with
//! paired Macs). Uses macOS's `security` tool.

use std::process::Command;

const SECURITY: &str = "/usr/bin/security";
const SERVICE: &str = "dev.arlint.quaked";

/// The stored secret for `account` (e.g. "digin:https://digin.work"), if any.
pub fn get(account: &str) -> Option<String> {
    let out = Command::new(SECURITY).args(["find-generic-password", "-s", SERVICE, "-a", account, "-w"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string();
    (!s.is_empty()).then_some(s)
}

/// Stores (or replaces) the secret for `account`.
pub fn set(account: &str, secret: &str) -> Result<(), String> {
    let out = Command::new(SECURITY)
        .args(["add-generic-password", "-U", "-s", SERVICE, "-a", account, "-l", &format!("quaked: {account}"), "-w", secret])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Forgets the secret for `account`.
pub fn delete(account: &str) {
    let _ = Command::new(SECURITY).args(["delete-generic-password", "-s", SERVICE, "-a", account]).output();
}
