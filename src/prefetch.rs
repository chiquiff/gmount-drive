//! Phase-2 content prefetcher: "Smart" prefetch of the folder you're browsing.
//!
//! We watch rclone's VFS cache directory (a normal local directory on disk). When rclone caches
//! a file — because you opened it, or the file manager generated a thumbnail — we learn that the
//! file's FOLDER is "hot", and we prefetch the OTHER (small) files in that same folder by reading
//! them through the mount, so opening them is instant. No root, no log parsing: we observe
//! rclone's own cache, which is the source of truth, and it works on any file manager.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use notify::{EventKind, RecursiveMode, Watcher};

use crate::rclone;

/// Files larger than this are not auto-prefetched (don't pull a 4 GB video just because you
/// opened its folder).
const MAX_PREFETCH_BYTES: u64 = 50 * 1024 * 1024;

/// Owns the cache watcher + a background worker thread. Dropping it stops both.
pub struct Prefetcher {
    stop: Arc<AtomicBool>,
    _watcher: notify::RecommendedWatcher,
}

impl Drop for Prefetcher {
    fn drop(&mut self) {
        // Signal the worker to stop; dropping `_watcher` stops the watch and drops the sender,
        // which makes the worker's recv() return Err and the thread exit.
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Prefetcher {
    /// Starts watching the VFS cache and prefetching siblings of cached files. `mountpoint` is the
    /// Drive mount (~/GoogleDrive). Returns None if the cache dir or the watcher can't be set up.
    pub fn start(mountpoint: PathBuf) -> Option<Prefetcher> {
        let cache_root = vfs_cache_root()?;
        std::fs::create_dir_all(&cache_root).ok()?;

        let stop = Arc::new(AtomicBool::new(false));
        let handled: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
        let (tx, rx) = mpsc::channel::<PathBuf>();

        // Worker: reads enqueued files through the mount so rclone caches their content.
        let stop_w = stop.clone();
        std::thread::spawn(move || {
            while let Ok(path) = rx.recv() {
                if stop_w.load(Ordering::Relaxed) {
                    break;
                }
                prefetch_file(&path);
            }
        });

        // Watcher: react when files appear/grow in the cache.
        let stop_e = stop.clone();
        let cache_root_e = cache_root.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if stop_e.load(Ordering::Relaxed) {
                return;
            }
            let Ok(event) = res else { return };
            if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                return;
            }
            for cache_path in event.paths {
                if !cache_path.is_file() {
                    continue;
                }
                let Some(folder) = drive_folder_of(&cache_path, &cache_root_e, &mountpoint) else {
                    continue;
                };
                // Process each hot folder only once per session. This also breaks the feedback
                // loop: our own prefetch writes to the cache, which would otherwise re-trigger us.
                {
                    let mut h = handled.lock().unwrap();
                    if !h.insert(folder.clone()) {
                        continue;
                    }
                }
                enqueue_folder(&folder, &cache_path, &tx);
            }
        })
        .ok()?;

        watcher.watch(&cache_root, RecursiveMode::Recursive).ok()?;

        Some(Prefetcher {
            stop,
            _watcher: watcher,
        })
    }
}

/// rclone's VFS *data* cache root for our remote: $XDG_CACHE_HOME/rclone/vfs/<remote> (or ~/.cache).
fn vfs_cache_root() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("rclone").join("vfs").join(rclone::REMOTE))
}

/// Maps a cache file path back to the Drive folder (under the mountpoint) that contains it.
fn drive_folder_of(cache_path: &Path, cache_root: &Path, mountpoint: &Path) -> Option<PathBuf> {
    let rel = cache_path.strip_prefix(cache_root).ok()?;
    let folder_rel = rel.parent()?;
    Some(mountpoint.join(folder_rel))
}

/// Enqueues the small files of `folder` for prefetch, skipping `trigger` (already cached).
fn enqueue_folder(folder: &Path, trigger: &Path, tx: &Sender<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let name = entry.file_name();
        if meta.is_file()
            && meta.len() <= MAX_PREFETCH_BYTES
            && Some(name.as_os_str()) != trigger.file_name()
        {
            let _ = tx.send(entry.path());
        }
    }
}

/// Reads a whole file through the mount so rclone downloads it into the VFS cache.
fn prefetch_file(path: &Path) {
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = std::io::copy(&mut f, &mut std::io::sink());
    }
}
