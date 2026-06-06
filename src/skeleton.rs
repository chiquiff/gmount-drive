//! Builds the folder "skeleton" by asking rclone to recursively refresh its directory cache
//! (`vfs/refresh recursive=true`).
//!
//! Why this and not a per-folder walk: with the mount's `--fast-list`, Google Drive lists the
//! WHOLE tree in a handful of bulk API calls (ListR) instead of one call per folder. After this,
//! every folder lists instantly from the cache — no blank/loading folders — and we don't hammer
//! the API (which would rate-limit and starve the user's own navigation).

use std::process::Command;

use crate::{mount, rclone};

/// Triggers a full recursive directory-cache refresh. **BLOCKS** until rclone finishes the bulk
/// listing — run in spawn_blocking. Returns true if the refresh succeeded.
pub fn build() -> bool {
    let url = format!("http://{}/", mount::current_rc_addr());
    Command::new(rclone::rclone_bin())
        .args(["rc", "--url", &url, "vfs/refresh", "recursive=true"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
