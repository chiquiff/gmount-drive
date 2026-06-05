//! GMount Drive — mount your Google Drive as a disk on Linux.
//! A free, open-source alternative to Insync. Engine: rclone. GUI: GTK4 + libadwaita.

mod appconfig;
mod autostart;
mod mount;
mod oauth;
mod rclone;
mod stats;
mod tray;
mod ui;
mod wizard;

use adw::prelude::*;
use gtk::glib;

/// App ID (must match the .desktop name and the icon installed in hicolor so the dock/taskbar
/// show the brand icon). See install.sh.
pub const APP_ID: &str = "io.github.gmountdrive.App";
/// Visible brand name.
pub const APP_NAME: &str = "GMount Drive";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .build();

    app.connect_activate(ui::build_ui);
    app.run()
}
