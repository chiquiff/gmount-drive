//! App preferences, persisted to ~/.config/gmount-drive/config.json.
//! "Empty"/0 values mean "no limit / default".

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    /// Folder where the Drive is mounted.
    pub mountpoint: String,
    /// VFS cache limit in GB (0 = no limit). Maps to --vfs-cache-max-size.
    pub cache_max_gb: u32,
    /// Evict from the cache anything unused after N days (0 = no limit). Maps to --vfs-cache-max-age.
    pub cache_max_age_days: u32,
    /// Bandwidth limit in MB/s (0 = no limit). Maps to --bwlimit.
    pub bwlimit_mbps: u32,
    /// Open the folder automatically after mounting.
    pub open_after_mount: bool,
    /// Mount in read-only mode (--read-only).
    pub read_only: bool,
    /// Show Google Docs/Sheets/Slides as .docx/.xlsx/.pptx (--drive-export-formats).
    pub gdocs_as_office: bool,
    /// Build the folder skeleton on mount (breadth-first) for instant navigation.
    pub fast_browsing: bool,
    /// Prefetch the content of folders you browse (so files open instantly).
    pub prefetch_content: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mountpoint: default_mountpoint(),
            cache_max_gb: 0,
            cache_max_age_days: 0,
            bwlimit_mbps: 0,
            open_after_mount: false,
            read_only: false,
            gdocs_as_office: false,
            fast_browsing: true,
            prefetch_content: true,
        }
    }
}

/// Default mount folder: ~/GoogleDrive.
pub fn default_mountpoint() -> String {
    home()
        .join("GoogleDrive")
        .to_string_lossy()
        .into_owned()
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"));
    base.join("gmount-drive").join("config.json")
}

impl Config {
    /// Reads the config from disk; if it doesn't exist or is corrupt, returns the defaults.
    pub fn load() -> Config {
        let path = config_path();
        let Ok(data) = std::fs::read_to_string(&path) else {
            return Config::default();
        };
        serde_json::from_str(&data).unwrap_or_default()
    }

    /// Saves the config to disk (creates the folder if needed).
    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }

    /// Mount folder as a PathBuf (falls back to the default if it's empty).
    pub fn mountpoint_path(&self) -> PathBuf {
        if self.mountpoint.trim().is_empty() {
            PathBuf::from(default_mountpoint())
        } else {
            PathBuf::from(&self.mountpoint)
        }
    }
}
