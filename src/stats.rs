//! Live mount status, read from rclone's RC API (`rclone rc ...`).

use std::process::Command;

use serde_json::Value;

use crate::{mount, rclone};

#[derive(Default, Clone)]
pub struct Stats {
    /// Average transfer speed (bytes/sec).
    pub speed_bps: f64,
    /// Number of files currently transferring.
    pub transferring: usize,
    /// Name of the first file in transfer (to show "downloading: …").
    pub current_file: Option<String>,
    /// Bytes used by the VFS cache on disk.
    pub cache_bytes: u64,
}

/// Calls an RC API command and returns the parsed JSON.
/// The `rclone rc` client connects with `--url` (NOT `--rc-addr`, which is the server's).
fn rc_call(cmd: &str) -> Option<Value> {
    let url = format!("http://{}/", mount::current_rc_addr());
    let out = Command::new(rclone::rclone_bin())
        .args(["rc", "--url", &url, cmd])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// Reads core/stats + vfs/stats. Returns zeroed values if the RC doesn't respond.
pub fn fetch() -> Stats {
    let mut s = Stats::default();

    if let Some(core) = rc_call("core/stats") {
        s.speed_bps = core.get("speed").and_then(Value::as_f64).unwrap_or(0.0);
        if let Some(arr) = core.get("transferring").and_then(Value::as_array) {
            s.transferring = arr.len();
            s.current_file = arr
                .first()
                .and_then(|t| t.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }

    if let Some(vfs) = rc_call("vfs/stats") {
        s.cache_bytes = vfs
            .get("diskCache")
            .and_then(|d| d.get("bytesUsed"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
    }

    s
}

/// Formats bytes into something readable (B/KB/MB/GB/TB).
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut f = n as f64;
    let mut i = 0;
    while f >= 1024.0 && i < UNITS.len() - 1 {
        f /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{f:.1} {}", UNITS[i])
    }
}
