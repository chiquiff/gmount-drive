//! Mounting/unmounting the Drive via `rclone mount` + VFS cache.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::rclone;

/// Default RC address (fallback if there's none saved from an in-progress mount).
const RC_ADDR_DEFAULT: &str = "127.0.0.1:15572";

/// Picks a free TCP port (for the RC API), so two mounts/zombies never clash.
fn pick_free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
        .unwrap_or(15572)
}

/// File where we store the current mount's RC address (so stats can read it).
fn rc_addr_file() -> Option<PathBuf> {
    let dir = std::env::var_os("HOME")
        .map(PathBuf::from)?
        .join(".cache/gmount-drive");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("rc-addr"))
}

/// Current mount's RC address (the one chosen when mounting; default if none).
pub fn current_rc_addr() -> String {
    rc_addr_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| RC_ADDR_DEFAULT.to_string())
}

/// Current mount point, taken from the preferences (default ~/GoogleDrive).
pub fn mountpoint() -> PathBuf {
    crate::appconfig::Config::load().mountpoint_path()
}

/// Is our mount point mounted? (reads /proc/mounts)
pub fn is_mounted() -> bool {
    let mp = mountpoint();
    let mp_str = mp.to_string_lossy();
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
        return false;
    };
    for line in mounts.lines() {
        // format: <dev> <target> <fstype> ...   (target octal-escapes space/tab/newline/backslash)
        if let Some(target) = line.split_whitespace().nth(1) {
            let target = target
                .replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\012", "\n")
                .replace("\\134", "\\");
            if target == mp_str {
                return true;
            }
        }
    }
    false
}

/// Creates the mount point if it doesn't exist.
pub fn ensure_mountpoint() -> Result<PathBuf, String> {
    let mp = mountpoint();
    if !mp.exists() {
        std::fs::create_dir_all(&mp)
            .map_err(|e| format!("couldn't create {}: {e}", mp.display()))?;
    }
    Ok(mp)
}

/// If the mount point got stuck (rclone died leaving FUSE behind), unmount it lazily.
/// NOTE: on a stuck endpoint `Path::exists()` fails (stat returns ENOTCONN), so we do NOT use it
/// as a guard. We detect the case by the ENOTCONN error (107 on Linux) when reading the directory.
pub fn cleanup_stale() {
    let mp = mountpoint();
    if !is_mounted() {
        return;
    }
    let errno = std::fs::read_dir(&mp).err().and_then(|e| e.raw_os_error());
    if errno == Some(107) {
        let _ = Command::new("fusermount3")
            .arg("-uz")
            .arg(&mp)
            .stderr(Stdio::null())
            .status();
    }
}

/// Path of the log where rclone mount writes its stderr (for diagnosing failures).
fn log_path() -> Option<PathBuf> {
    let dir = std::env::var_os("HOME")
        .map(PathBuf::from)?
        .join(".cache/gmount-drive");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("mount.log"))
}

/// Last lines of the rclone mount log (to show the reason for a failure).
fn log_tail() -> String {
    let Some(p) = log_path() else {
        return String::new();
    };
    let content = std::fs::read_to_string(p).unwrap_or_default();
    content
        .lines()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Mounts the Drive. Launches `rclone mount` (with --rc for stats) as a background process and
/// waits up to ~15s for the mount to appear. Does NOT use --daemon because it clashes with --rc.
pub fn mount() -> Result<(), String> {
    if is_mounted() {
        return Ok(());
    }
    let mp = ensure_mountpoint()?;
    let remote = format!("{}:", rclone::REMOTE);

    // rclone stderr -> log (if possible), otherwise discard.
    let stderr = match log_path().and_then(|p| std::fs::File::create(p).ok()) {
        Some(f) => Stdio::from(f),
        None => Stdio::null(),
    };

    // Optional flags from the preferences (cache and bandwidth).
    let cfg = crate::appconfig::Config::load();
    let mut extra: Vec<String> = Vec::new();
    if cfg.cache_max_gb > 0 {
        extra.push("--vfs-cache-max-size".into());
        extra.push(format!("{}G", cfg.cache_max_gb));
    }
    if cfg.cache_max_age_days > 0 {
        extra.push("--vfs-cache-max-age".into());
        extra.push(format!("{}d", cfg.cache_max_age_days));
    }
    if cfg.bwlimit_mbps > 0 {
        extra.push("--bwlimit".into());
        extra.push(format!("{}M", cfg.bwlimit_mbps));
    }
    if cfg.read_only {
        extra.push("--read-only".into());
    }
    if cfg.gdocs_as_office {
        // Google Docs/Sheets/Slides show up as Office files (read-only) instead of .gdoc
        // shortcuts that open the browser.
        extra.push("--drive-export-formats".into());
        extra.push("docx,xlsx,pptx".into());
    }

    // Free port for the RC API (avoids clashes with rclone zombies on a fixed port).
    let rc_addr = format!("127.0.0.1:{}", pick_free_port());
    if let Some(f) = rc_addr_file() {
        let _ = std::fs::write(f, &rc_addr);
    }

    let mut child = Command::new(rclone::rclone_bin())
        .arg("mount")
        .arg(&remote)
        .arg(&mp)
        // A long dir-cache-time = folder listings stay cached (re-opening = instant);
        // poll-interval keeps the cache fresh (new files appear within ~15s).
        // --fast-list lets the recursive skeleton refresh use Google Drive's bulk recursive
        // listing (ListR) — the whole tree in a few API calls instead of one per folder.
        .args([
            "--vfs-cache-mode",
            "full",
            "--dir-cache-time",
            "1000h",
            "--poll-interval",
            "15s",
            "--fast-list",
        ])
        // Remote-control API to read live status (localhost only, no auth).
        .args(["--rc", "--rc-addr"])
        .arg(&rc_addr)
        .arg("--rc-no-auth")
        .args(&extra)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .map_err(|e| format!("couldn't launch rclone mount: {e}"))?;

    // Wait for it to mount (up to ~15s), detecting if the process dies first.
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(500));
        if is_mounted() {
            return Ok(());
        }
        if let Ok(Some(_)) = child.try_wait() {
            let why = log_tail();
            return Err(if why.is_empty() {
                "rclone mount exited unexpectedly".to_string()
            } else {
                format!("rclone mount failed: {why}")
            });
        }
    }
    let _ = child.kill();
    Err("the mount took too long (timeout)".to_string())
}

/// Unmounts the Drive (fusermount3 -u; if that fails, -uz lazy).
pub fn unmount() -> Result<(), String> {
    let mp = mountpoint();
    let ok = Command::new("fusermount3")
        .arg("-u")
        .arg(&mp)
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        let _ = Command::new("fusermount3")
            .arg("-uz")
            .arg(&mp)
            .stderr(Stdio::null())
            .status();
    }
    Ok(())
}

/// Opens the mounted folder in the file manager.
pub fn open_folder() {
    let _ = Command::new("xdg-open").arg(mountpoint()).spawn();
}

/// Clears rclone's VFS cache for our remote. Downloaded data is freed and re-downloaded when
/// files are opened. Best done with the Drive unmounted.
pub fn clear_cache() -> Result<(), String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME not found")?;
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"))
        .join("rclone");
    for sub in ["vfs", "vfsMeta"] {
        let dir = base.join(sub).join(rclone::REMOTE);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| format!("couldn't delete {}: {e}", dir.display()))?;
        }
    }
    Ok(())
}
