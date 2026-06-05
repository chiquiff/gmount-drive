//! Autostart at login via XDG autostart (~/.config/autostart/*.desktop).
//! Launches the app with --background so it appears in the tray and mounts automatically.

use std::path::PathBuf;

fn autostart_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".config")
        });
    base.join("autostart")
        .join(format!("{}.desktop", crate::APP_ID))
}

pub fn is_enabled() -> bool {
    autostart_path().exists()
}

pub fn enable() -> Result<(), String> {
    let path = autostart_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         Comment=Mount your Google Drive as a disk\n\
         Exec={exe} --background\n\
         Icon={icon}\n\
         StartupWMClass={icon}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        name = crate::APP_NAME,
        exe = exe.display(),
        icon = crate::APP_ID,
    );
    std::fs::write(&path, content).map_err(|e| e.to_string())
}

pub fn disable() -> Result<(), String> {
    let path = autostart_path();
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}
