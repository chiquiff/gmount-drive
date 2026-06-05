//! rclone integration layer (via subprocesses).
//! Detects the binary, queries version/remotes and creates/deletes the Drive account.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Name of the rclone remote the app manages (MVP: a single account).
pub const REMOTE: &str = "gdrive";

/// Returns the rclone binary to use: first ~/.local/bin/rclone, otherwise the one on PATH.
pub fn rclone_bin() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let local = PathBuf::from(&home).join(".local/bin/rclone");
        if local.exists() {
            return local;
        }
    }
    PathBuf::from("rclone")
}

/// First line of `rclone version` (e.g. "rclone v1.74.2").
pub fn version() -> Result<String, String> {
    let out = Command::new(rclone_bin())
        .arg("version")
        .output()
        .map_err(|e| format!("couldn't run rclone: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("rclone")
        .trim()
        .to_string())
}

/// Lists the configured remotes (without the trailing ':').
pub fn list_remotes() -> Vec<String> {
    let out = match Command::new(rclone_bin()).arg("listremotes").output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().trim_end_matches(':').to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Is our Drive account set up?
pub fn has_remote() -> bool {
    list_remotes().iter().any(|r| r == REMOTE)
}

/// Creates the Drive remote with OAuth (the "quick" path, rclone's shared client).
/// **BLOCKS**: rclone opens the browser and waits for the login. It is **cancellable**: if
/// `cancel` is set to `true` (e.g. you click "Cancel"), we kill the rclone process and return
/// `Ok(false)`. `Ok(true)` = account connected.
pub fn create_drive_remote(cancel: &AtomicBool) -> Result<bool, String> {
    let mut child = Command::new(rclone_bin())
        .args([
            "config",
            "create",
            REMOTE,
            "drive",
            "scope=drive",
            "config_is_local=true",
        ])
        // Silence stdout: rclone dumps the created config there (including the token). stderr is
        // inherited so the user can see the OAuth link if the browser doesn't open by itself.
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't run rclone: {e}"))?;

    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(false);
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(true),
            Ok(Some(_)) => return Err("account setup didn't complete".to_string()),
            Ok(None) => std::thread::sleep(Duration::from_millis(150)),
            Err(e) => return Err(format!("error waiting for rclone: {e}")),
        }
    }
}

/// Creates the Drive remote with the user's own credentials (BYO) and an ALREADY-obtained OAuth
/// token (we got it ourselves in `crate::oauth`, so rclone doesn't open the browser or show its
/// own page). `--non-interactive` guarantees it won't attempt any prompt.
pub fn create_drive_remote_with_token(
    client_id: &str,
    client_secret: &str,
    token_json: &str,
) -> Result<(), String> {
    let cid = format!("client_id={client_id}");
    let csec = format!("client_secret={client_secret}");
    let tok = format!("token={token_json}");
    let status = Command::new(rclone_bin())
        .args(["config", "create", REMOTE, "drive"])
        .arg(&cid)
        .arg(&csec)
        .args(["scope=drive", "config_is_local=true"])
        .arg(&tok)
        .arg("--non-interactive")
        // Silence stdout so the token isn't dumped to the terminal.
        .stdout(Stdio::null())
        .status()
        .map_err(|e| format!("couldn't run rclone: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err("couldn't save the account in rclone".to_string())
    }
}

/// Drive space (in bytes). Some fields may come back as zero if the API doesn't provide them.
pub struct About {
    pub total: u64,
    pub used: u64,
    pub free: u64,
}

/// Queries the Drive used/free space (`rclone about gdrive: --json`).
/// **BLOCKS** (makes a network call): run in spawn_blocking.
pub fn about() -> Option<About> {
    let out = Command::new(rclone_bin())
        .args(["about", &format!("{REMOTE}:"), "--json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let g = |k: &str| v.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
    Some(About {
        total: g("total"),
        used: g("used"),
        free: g("free"),
    })
}

/// Deletes the Drive account from rclone's config.
pub fn delete_remote() -> Result<(), String> {
    let status = Command::new(rclone_bin())
        .args(["config", "delete", REMOTE])
        .status()
        .map_err(|e| format!("couldn't run rclone: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("couldn't delete the account".to_string())
    }
}
