//! GTK4 + libadwaita interface, state-driven, with system tray and autostart.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::tray::{DriveTray, TrayAction};
use crate::{appconfig, autostart, mount, prefetch, rclone, stats};

/// Entry point called on `activate`.
pub fn build_ui(app: &adw::Application) {
    // Re-activation (second launch or from the tray): show the existing window.
    if let Some(win) = app.windows().first() {
        win.present();
        return;
    }

    // Make windows use the brand icon (resolved by name from hicolor; see install.sh).
    gtk::Window::set_default_icon_name(crate::APP_ID);

    mount::cleanup_stale();
    let background = std::env::args().any(|a| a == "--background");

    // Channel tray -> main thread.
    let (tx, rx) = async_channel::unbounded::<TrayAction>();

    // Tray on its own thread.
    let service = ksni::TrayService::new(DriveTray::new(tx));
    let tray_handle = service.handle();
    service.spawn();

    let ui = Ui::new(app, tray_handle);

    // Closing the window = hide it to the tray (the app stays alive).
    ui.window.connect_close_request(|win| {
        win.set_visible(false);
        glib::Propagation::Stop
    });

    // Preferences button.
    let ui_menu = ui.clone();
    ui.menu_btn
        .connect_clicked(move |_| ui_menu.clone().open_preferences());

    ui.refresh();
    ui.start_stats_polling();

    // If we launched while the Drive was already mounted, kick off the post-mount work.
    if mount::is_mounted() {
        ui.after_mount();
    }

    // Tray "syncing" animation: while indexing or transferring, cycle the spinner frames.
    let ui_anim = ui.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(130), move || {
        let working = ui_anim.indexing.get() || ui_anim.transferring.get();
        if working {
            let f = (ui_anim.sync_frame.get() + 1) % N_SYNC_FRAMES;
            ui_anim.sync_frame.set(f);
            ui_anim.was_working.set(true);
            ui_anim.tray_handle.update(move |t: &mut DriveTray| {
                t.syncing = true;
                t.frame = f;
            });
        } else if ui_anim.was_working.replace(false) {
            // Just stopped working: switch the tray back to the static icon once.
            ui_anim.tray_handle.update(|t: &mut DriveTray| t.syncing = false);
        }
        glib::ControlFlow::Continue
    });

    // Loop that receives tray actions.
    let ui_rx = ui.clone();
    glib::spawn_future_local(async move {
        while let Ok(action) = rx.recv().await {
            ui_rx.clone().handle_tray_action(action);
        }
    });

    if background {
        // Start hidden and, if there is an account, mount automatically.
        let ui_bg = ui.clone();
        glib::spawn_future_local(async move {
            if rclone::has_remote() && !mount::is_mounted() {
                // Background start has no visible window, so report failures via a notification.
                match gio::spawn_blocking(mount::mount).await {
                    Ok(Err(e)) => ui_bg.notify("GMount Drive", &format!("Couldn't mount your Drive: {e}")),
                    Err(_) => ui_bg.notify("GMount Drive", "Internal error while mounting"),
                    Ok(Ok(())) => ui_bg.after_mount(),
                }
                ui_bg.refresh();
            }
        });
    } else {
        ui.window.present();
        ui.clear_notification();
    }
}

/// Refs to the widgets that change with state + tray handle.
struct Ui {
    window: adw::ApplicationWindow,
    toast_label: gtk::Label,
    toast_revealer: gtk::Revealer,
    account_row: adw::ActionRow,
    mount_row: adw::ActionRow,
    space_row: adw::ActionRow,
    space_bar: gtk::LevelBar,
    action_box: gtk::Box,
    activity_group: adw::PreferencesGroup,
    speed_row: adw::ActionRow,
    cache_row: adw::ActionRow,
    menu_btn: gtk::Button,
    tray_handle: ksni::Handle<DriveTray>,
    /// Last known mount state (to refresh only when it changed externally).
    last_mounted: Cell<bool>,
    /// Monotonic counter so an old toast's hide timer doesn't hide a newer toast.
    toast_gen: Cell<u64>,
    /// Background content prefetcher (watches the VFS cache); present while it's running.
    prefetcher: Cell<Option<prefetch::Prefetcher>>,
    /// "Working" signals that drive the tray's syncing animation.
    indexing: Cell<bool>,
    transferring: Cell<bool>,
    sync_frame: Cell<usize>,
    was_working: Cell<bool>,
    /// Flag to cancel an in-progress account setup (kills the rclone process).
    cancel: Arc<AtomicBool>,
}

/// Number of frames in the tray "syncing" animation (see assets/brand/gmount-drive-sync-*.png).
const N_SYNC_FRAMES: usize = 8;

impl Ui {
    fn new(app: &adw::Application, tray_handle: ksni::Handle<DriveTray>) -> Rc<Self> {
        let header = adw::HeaderBar::new();
        // Preferences button, to the left of the title so it's easy to spot.
        // Wired up in build_ui, once the Rc<Ui> exists.
        let menu_btn = gtk::Button::from_icon_name("view-more-symbolic");
        menu_btn.set_tooltip_text(Some("Preferences"));
        header.pack_start(&menu_btn);

        // --- Status group ---
        let group = adw::PreferencesGroup::builder().title("Status").build();

        let rclone_row = adw::ActionRow::builder().title("rclone engine").build();
        rclone_row.add_prefix(&icon("drive-harddisk-symbolic"));
        rclone_row.set_subtitle(&match rclone::version() {
            Ok(v) => v,
            Err(e) => format!("not found: {e}"),
        });

        let account_row = adw::ActionRow::builder().title("Google account").build();
        account_row.add_prefix(&icon("avatar-default-symbolic"));

        let mount_row = adw::ActionRow::builder().title("Mount").build();
        mount_row.add_prefix(&icon("folder-symbolic"));

        // Drive used/free space (filled in on connect, via `rclone about`).
        let space_row = adw::ActionRow::builder()
            .title("Drive space")
            .subtitle("—")
            .build();
        space_row.add_prefix(&icon("drive-harddisk-symbolic"));
        let space_bar = gtk::LevelBar::builder()
            .min_value(0.0)
            .max_value(1.0)
            .valign(gtk::Align::Center)
            .build();
        space_bar.set_size_request(120, -1);
        space_bar.set_visible(false);
        space_row.add_suffix(&space_bar);

        group.add(&rclone_row);
        group.add(&account_row);
        group.add(&mount_row);
        group.add(&space_row);

        // --- Activity group (hidden when not mounted) ---
        let activity_group = adw::PreferencesGroup::builder().title("Activity").build();
        let speed_row = adw::ActionRow::builder().title("Speed").subtitle("—").build();
        speed_row.add_prefix(&icon("network-transmit-receive-symbolic"));
        let cache_row = adw::ActionRow::builder()
            .title("Disk cache")
            .subtitle("—")
            .build();
        cache_row.add_prefix(&icon("drive-multidisk-symbolic"));
        activity_group.add(&speed_row);
        activity_group.add(&cache_row);
        activity_group.set_visible(false);

        // --- Action buttons (dynamic) ---
        let action_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(8)
            .build();

        // --- Layout --- (options now live in the Preferences window)
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();
        content.append(&brand_header());
        content.append(&group);
        content.append(&activity_group);
        content.append(&action_box);

        let clamp = adw::Clamp::builder().maximum_size(500).child(&content).build();

        // FLOATING toast at top center: an Overlay layers it without moving/shrinking the content.
        let toast_label = gtk::Label::builder()
            .wrap(true)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(16)
            .margin_end(16)
            .build();
        let toast_pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        toast_pill.append(&toast_label);
        toast_pill.add_css_class("osd"); // dark rounded "pill" floating background
        let toast_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .valign(gtk::Align::Start)
            .halign(gtk::Align::Center)
            .margin_top(10)
            .reveal_child(false)
            .child(&toast_pill)
            .build();

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&clamp));
        overlay.add_overlay(&toast_revealer);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&overlay));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(crate::APP_NAME)
            .default_width(480)
            .default_height(520)
            .content(&toolbar)
            .build();

        Rc::new(Ui {
            window,
            toast_label,
            toast_revealer,
            account_row,
            mount_row,
            space_row,
            space_bar,
            action_box,
            activity_group,
            speed_row,
            cache_row,
            menu_btn,
            tray_handle,
            last_mounted: Cell::new(mount::is_mounted()),
            toast_gen: Cell::new(0),
            prefetcher: Cell::new(None),
            indexing: Cell::new(false),
            transferring: Cell::new(false),
            sync_frame: Cell::new(0),
            was_working: Cell::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Shows a floating toast at top center and hides it on its own after ~4s.
    /// A generation counter ensures an older toast's timer doesn't hide a newer message.
    fn toast(self: &Rc<Self>, msg: &str) {
        self.toast_label.set_text(msg);
        self.toast_revealer.set_reveal_child(true);
        let generation = self.toast_gen.get().wrapping_add(1);
        self.toast_gen.set(generation);
        let ui = self.clone();
        glib::timeout_add_seconds_local_once(4, move || {
            if ui.toast_gen.get() == generation {
                ui.toast_revealer.set_reveal_child(false);
            }
        });
    }

    /// Re-reads the real state and rebuilds buttons + tray icon.
    fn refresh(self: &Rc<Self>) {
        let has_remote = rclone::has_remote();
        let mounted = mount::is_mounted();
        self.last_mounted.set(mounted);

        self.account_row
            .set_subtitle(if has_remote { "Connected ✓" } else { "Not connected" });
        let mp = mount::mountpoint();
        self.mount_row.set_subtitle(&if mounted {
            // Escape the path: row subtitles are parsed as Pango markup.
            format!("Mounted at {}", glib::markup_escape_text(&mp.to_string_lossy()))
        } else if has_remote {
            "Unmounted".to_string()
        } else {
            "—".to_string()
        });

        // Drive space: only when there is an account.
        self.space_row.set_visible(has_remote);
        if has_remote {
            self.clone().update_space();
        }

        // Update the tray.
        self.tray_handle.update(move |t: &mut DriveTray| {
            t.connected = has_remote;
            t.mounted = mounted;
        });

        // Rebuild buttons.
        self.action_box.set_sensitive(true);
        while let Some(child) = self.action_box.first_child() {
            self.action_box.remove(&child);
        }

        if !has_remote {
            let btn = primary_button("Connect Google Drive");
            let ui = self.clone();
            btn.connect_clicked(move |_| ui.clone().on_connect());
            self.action_box.append(&btn);

            let byo = gtk::Button::with_label("Use my own credential (no speed limit)");
            byo.add_css_class("pill");
            let ui = self.clone();
            byo.connect_clicked(move |_| ui.clone().on_byo());
            self.action_box.append(&byo);
        } else if !mounted {
            let btn = primary_button("Mount my Drive");
            let ui = self.clone();
            btn.connect_clicked(move |_| ui.clone().on_mount());
            self.action_box.append(&btn);

            let disc = gtk::Button::with_label("Disconnect account");
            disc.add_css_class("destructive-action");
            let ui = self.clone();
            disc.connect_clicked(move |_| ui.clone().on_disconnect());
            self.action_box.append(&disc);
        } else {
            let open = primary_button("Open folder");
            open.connect_clicked(|_| mount::open_folder());
            self.action_box.append(&open);

            let um = gtk::Button::with_label("Unmount");
            let ui = self.clone();
            um.connect_clicked(move |_| ui.clone().on_unmount());
            self.action_box.append(&um);
        }
    }

    /// Starts a timer that updates the live status every 3s (when mounted).
    fn start_stats_polling(self: &Rc<Self>) {
        let ui = self.clone();
        glib::timeout_add_seconds_local(3, move || {
            let ui = ui.clone();
            glib::spawn_future_local(async move {
                // Auto-heal stale mounts (rclone died leaving the endpoint behind).
                let _ = gio::spawn_blocking(mount::cleanup_stale).await;

                let mounted = mount::is_mounted();
                // If the state changed externally (stale mount cleaned up, etc.), refresh.
                if ui.last_mounted.replace(mounted) != mounted {
                    ui.refresh();
                }

                if mounted {
                    let s = gio::spawn_blocking(stats::fetch).await.unwrap_or_default();
                    ui.transferring.set(s.transferring > 0);
                    ui.show_stats(Some(s));
                } else {
                    ui.transferring.set(false);
                    ui.show_stats(None);
                }
            });
            glib::ControlFlow::Continue
        });
    }

    fn show_stats(&self, s: Option<stats::Stats>) {
        match s {
            Some(s) => {
                self.activity_group.set_visible(true);
                let speed = format!("{}/s", stats::human_bytes(s.speed_bps as u64));
                // Escape the filename: row subtitles are parsed as Pango markup.
                self.speed_row.set_subtitle(&match s.current_file {
                    Some(f) if s.transferring > 0 => {
                        format!("{speed} — downloading: {}", glib::markup_escape_text(&f))
                    }
                    _ => speed,
                });
                self.cache_row.set_subtitle(&stats::human_bytes(s.cache_bytes));
            }
            None => self.activity_group.set_visible(false),
        }
    }

    /// System notification (useful when running in the background without a window). We only show
    /// it when there's NO visible window — otherwise the toast already covers it, and a lingering
    /// notification shows up as a confusing badge on the dock icon.
    fn notify(&self, title: &str, body: &str) {
        if self.window.is_visible() {
            return;
        }
        let n = gio::Notification::new(title);
        n.set_body(Some(body));
        if let Some(app) = self.window.application() {
            app.send_notification(Some("gmount-drive"), &n);
        }
    }

    /// Clears any pending system notification (and its dock badge). Called when the window opens.
    fn clear_notification(&self) {
        if let Some(app) = self.window.application() {
            app.withdraw_notification("gmount-drive");
        }
    }

    /// Queries the Drive space (on a thread) and updates the row + the bar.
    fn update_space(self: Rc<Self>) {
        let ui = self.clone();
        glib::spawn_future_local(async move {
            let about = gio::spawn_blocking(rclone::about).await.ok().flatten();
            match about {
                Some(a) if a.total > 0 => {
                    ui.space_row.set_subtitle(&format!(
                        "{} of {} used · {} free",
                        stats::human_bytes(a.used),
                        stats::human_bytes(a.total),
                        stats::human_bytes(a.free)
                    ));
                    ui.space_bar.set_value(a.used as f64 / a.total as f64);
                    ui.space_bar.set_visible(true);
                }
                _ => {
                    ui.space_row.set_subtitle("—");
                    ui.space_bar.set_visible(false);
                }
            }
        });
    }

    /// Opens the Preferences window (mount folder, cache, bandwidth, etc.).
    fn open_preferences(self: Rc<Self>) {
        let cfg = appconfig::Config::load();
        let win = adw::PreferencesWindow::builder()
            .title("Preferences")
            .modal(true)
            .default_width(480)
            .default_height(680)
            .build();
        win.set_transient_for(Some(&self.window));

        let page = adw::PreferencesPage::new();

        // --- Mount ---
        let g_mount = adw::PreferencesGroup::builder().title("Mount").build();
        let folder_row = adw::ActionRow::builder()
            .title("Mount folder")
            .subtitle(glib::markup_escape_text(&cfg.mountpoint))
            .build();
        let pick = gtk::Button::from_icon_name("folder-open-symbolic");
        pick.set_valign(gtk::Align::Center);
        pick.set_tooltip_text(Some("Choose folder"));
        let ui_pick = self.clone();
        let row_for_cb = folder_row.clone();
        let win_for_cb = win.clone();
        pick.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::builder()
                .title("Choose the mount folder")
                .modal(true)
                .build();
            let row = row_for_cb.clone();
            let ui2 = ui_pick.clone();
            dialog.select_folder(Some(&win_for_cb), gio::Cancellable::NONE, move |res| {
                if let Ok(folder) = res {
                    if let Some(path) = folder.path() {
                        let p = path.to_string_lossy().into_owned();
                        let mut c = appconfig::Config::load();
                        c.mountpoint = p.clone();
                        let _ = c.save();
                        row.set_subtitle(&glib::markup_escape_text(&p));
                        if mount::is_mounted() {
                            ui2.toast("The new folder will be used the next time you mount.");
                        }
                    }
                }
            });
        });
        folder_row.add_suffix(&pick);
        g_mount.add(&folder_row);

        let readonly_row = adw::SwitchRow::builder()
            .title("Read-only mount")
            .subtitle("Browse your files without modifying or deleting them")
            .active(cfg.read_only)
            .build();
        readonly_row.connect_active_notify(|s| {
            let mut c = appconfig::Config::load();
            c.read_only = s.is_active();
            let _ = c.save();
        });
        g_mount.add(&readonly_row);

        page.add(&g_mount);

        // --- Google Docs ---
        let g_docs = adw::PreferencesGroup::builder().title("Google Docs").build();
        let gdocs_row = adw::SwitchRow::builder()
            .title("Show as Office files")
            .subtitle("Google Docs/Sheets/Slides as .docx/.xlsx/.pptx (read-only)")
            .active(cfg.gdocs_as_office)
            .build();
        gdocs_row.connect_active_notify(|s| {
            let mut c = appconfig::Config::load();
            c.gdocs_as_office = s.is_active();
            let _ = c.save();
        });
        g_docs.add(&gdocs_row);
        page.add(&g_docs);

        // --- Cache & network ---
        let g_cache = adw::PreferencesGroup::builder()
            .title("Cache and network")
            .description("0 = no limit")
            .build();

        let cache_size = spin_row("Max cache (GB)", cfg.cache_max_gb as f64, 0.0, 4096.0);
        cache_size.connect_changed_save(|c, v| c.cache_max_gb = v as u32);
        g_cache.add(&cache_size.row);

        let cache_age = spin_row("Delete cache after (days)", cfg.cache_max_age_days as f64, 0.0, 365.0);
        cache_age.connect_changed_save(|c, v| c.cache_max_age_days = v as u32);
        g_cache.add(&cache_age.row);

        let bw = spin_row("Bandwidth (MB/s)", cfg.bwlimit_mbps as f64, 0.0, 1000.0);
        bw.connect_changed_save(|c, v| c.bwlimit_mbps = v as u32);
        g_cache.add(&bw.row);

        let fast_row = adw::SwitchRow::builder()
            .title("Faster browsing")
            .subtitle("Preload the folder list on mount (like Google Drive)")
            .active(cfg.fast_browsing)
            .build();
        fast_row.connect_active_notify(|s| {
            let mut c = appconfig::Config::load();
            c.fast_browsing = s.is_active();
            let _ = c.save();
        });
        g_cache.add(&fast_row);

        let prefetch_row = adw::SwitchRow::builder()
            .title("Preload files you're browsing")
            .subtitle("Download folder contents in the background so files open instantly")
            .active(cfg.prefetch_content)
            .build();
        prefetch_row.connect_active_notify(|s| {
            let mut c = appconfig::Config::load();
            c.prefetch_content = s.is_active();
            let _ = c.save();
        });
        g_cache.add(&prefetch_row);

        // Action: clear the cache now.
        let clear_row = adw::ActionRow::builder()
            .title("Clear cache now")
            .subtitle("Frees downloaded data (best with the Drive unmounted)")
            .build();
        let clear_btn = gtk::Button::with_label("Clear");
        clear_btn.set_valign(gtk::Align::Center);
        let ui_clear = self.clone();
        clear_btn.connect_clicked(move |_| {
            match mount::clear_cache() {
                Ok(()) => ui_clear.toast("Cache cleared"),
                Err(e) => ui_clear.toast(&format!("Couldn't clear: {e}")),
            }
        });
        clear_row.add_suffix(&clear_btn);
        g_cache.add(&clear_row);
        page.add(&g_cache);

        // --- Behavior ---
        let g_behavior = adw::PreferencesGroup::builder().title("Behavior").build();

        let open_row = adw::SwitchRow::builder()
            .title("Open folder on mount")
            .active(cfg.open_after_mount)
            .build();
        open_row.connect_active_notify(|s| {
            let mut c = appconfig::Config::load();
            c.open_after_mount = s.is_active();
            let _ = c.save();
        });
        g_behavior.add(&open_row);

        let autostart_row = adw::SwitchRow::builder()
            .title("Start at login")
            .subtitle("Mount your Drive when you log in")
            .active(autostart::is_enabled())
            .build();
        autostart_row.connect_active_notify(|s| {
            let r = if s.is_active() {
                autostart::enable()
            } else {
                autostart::disable()
            };
            if let Err(e) = r {
                eprintln!("autostart: {e}");
            }
        });
        g_behavior.add(&autostart_row);

        // Action: open Drive in the browser.
        let drive_row = adw::ActionRow::builder()
            .title("Open Drive in the browser")
            .subtitle("Go to drive.google.com")
            .build();
        let drive_btn = gtk::Button::from_icon_name("web-browser-symbolic");
        drive_btn.set_valign(gtk::Align::Center);
        drive_btn.set_tooltip_text(Some("Open in browser"));
        drive_row.add_suffix(&drive_btn);
        drive_row.set_activatable_widget(Some(&drive_btn));
        drive_btn.connect_clicked(|_| {
            let _ = std::process::Command::new("xdg-open")
                .arg("https://drive.google.com")
                .spawn();
        });
        g_behavior.add(&drive_row);
        page.add(&g_behavior);

        win.add(&page);
        win.present();
    }

    fn handle_tray_action(self: Rc<Self>, action: TrayAction) {
        match action {
            TrayAction::ShowWindow => {
                self.clear_notification();
                self.window.set_visible(true);
                self.window.present();
            }
            TrayAction::OpenFolder => mount::open_folder(),
            TrayAction::ToggleMount => {
                if mount::is_mounted() {
                    self.on_unmount();
                } else {
                    self.on_mount();
                }
            }
            TrayAction::Quit => {
                self.stop_prefetch();
                let app = self.window.application();
                glib::spawn_future_local(async move {
                    let _ = gio::spawn_blocking(mount::unmount).await;
                    if let Some(app) = app {
                        app.quit();
                    }
                });
            }
        }
    }

    fn on_connect(self: Rc<Self>) {
        self.toast("Opening the browser to sign in…");
        // "Connecting" state: a Cancel button stays enabled in case you close the browser or change your mind.
        self.cancel.store(false, Ordering::Relaxed);
        self.show_connecting();

        let cancel = self.cancel.clone();
        let ui = self.clone();
        glib::spawn_future_local(async move {
            let res = gio::spawn_blocking(move || rclone::create_drive_remote(&cancel)).await;
            match res {
                Ok(Ok(true)) => ui.toast("Account connected! 🎉"),
                Ok(Ok(false)) => ui.toast("Connection cancelled"),
                Ok(Err(e)) => ui.toast(&format!("Error: {e}")),
                Err(_) => ui.toast("Internal error while connecting"),
            }
            ui.refresh();
        });
    }

    /// Replaces the buttons with a "Connecting…" state plus an enabled Cancel button
    /// (which kills the rclone process). `refresh()` restores the normal buttons when done.
    fn show_connecting(self: &Rc<Self>) {
        while let Some(child) = self.action_box.first_child() {
            self.action_box.remove(&child);
        }
        let info = gtk::Label::new(Some("Connecting… authorize in the browser"));
        info.add_css_class("dim-label");
        self.action_box.append(&info);

        let cancel_btn = gtk::Button::with_label("Cancel");
        let ui = self.clone();
        cancel_btn.connect_clicked(move |b| {
            ui.cancel.store(true, Ordering::Relaxed);
            b.set_label("Cancelling…");
            b.set_sensitive(false);
        });
        self.action_box.append(&cancel_btn);
    }

    /// Launches the guided wizard so the user can use their own Google credential.
    fn on_byo(self: Rc<Self>) {
        let ui = self.clone();
        crate::wizard::launch(&self.window, move || ui.refresh());
    }

    fn on_mount(self: Rc<Self>) {
        self.action_box.set_sensitive(false);
        self.toast("Mounting your Drive…");
        let ui = self.clone();
        glib::spawn_future_local(async move {
            let res = gio::spawn_blocking(mount::mount).await;
            match res {
                Ok(Ok(())) => {
                    let mp = mount::mountpoint();
                    ui.toast(&format!("Mounted! Your Drive is at {}", mp.display()));
                    ui.notify("Google Drive mounted", &format!("Available at {}", mp.display()));
                    if appconfig::Config::load().open_after_mount {
                        mount::open_folder();
                    }
                    ui.after_mount();
                }
                Ok(Err(e)) => ui.toast(&format!("Mount error: {e}")),
                Err(_) => ui.toast("Internal error while mounting"),
            }
            ui.refresh();
        });
    }

    /// Post-mount background work: build the folder skeleton (instant navigation) and start the
    /// content prefetcher (instant opens). Called from every path that ends up mounted.
    fn after_mount(self: &Rc<Self>) {
        let cfg = appconfig::Config::load();

        // 1. Skeleton: one bulk recursive refresh (fast-list) so EVERY folder lists instantly
        //    afterwards — no blank/loading folders.
        if cfg.fast_browsing {
            let ui_warm = self.clone();
            glib::spawn_future_local(async move {
                ui_warm.indexing.set(true);
                ui_warm
                    .mount_row
                    .set_subtitle("Mounted — indexing your Drive…");
                let ok = gio::spawn_blocking(crate::skeleton::build)
                    .await
                    .unwrap_or(false);
                ui_warm.indexing.set(false);
                if mount::is_mounted() {
                    ui_warm.toast(if ok {
                        "Fast browsing ready ✓"
                    } else {
                        "Indexing finished"
                    });
                }
                ui_warm.refresh();
            });
        }

        // 2. Content prefetch: watch the VFS cache and warm the folders you browse.
        if cfg.prefetch_content {
            self.prefetcher
                .set(prefetch::Prefetcher::start(mount::mountpoint()));
        } else {
            self.prefetcher.set(None);
        }
    }

    /// Stops the content prefetcher (on unmount/disconnect/quit). Dropping it stops its threads.
    fn stop_prefetch(&self) {
        self.prefetcher.set(None);
    }

    fn on_unmount(self: Rc<Self>) {
        self.action_box.set_sensitive(false);
        self.stop_prefetch();
        let ui = self.clone();
        glib::spawn_future_local(async move {
            let _ = gio::spawn_blocking(mount::unmount).await;
            ui.toast("Unmounted");
            ui.show_stats(None);
            ui.refresh();
        });
    }

    fn on_disconnect(self: Rc<Self>) {
        self.action_box.set_sensitive(false);
        self.stop_prefetch();
        let ui = self.clone();
        glib::spawn_future_local(async move {
            let res = gio::spawn_blocking(|| {
                let _ = mount::unmount();
                rclone::delete_remote()
            })
            .await;
            match res {
                Ok(Ok(())) => ui.toast("Account disconnected"),
                Ok(Err(e)) => ui.toast(&format!("Error: {e}")),
                Err(_) => ui.toast("Internal error"),
            }
            ui.refresh();
        });
    }
}

/// Brand header: icon (embedded in the binary) + name + tagline. Theme-proof.
fn brand_header() -> gtk::Widget {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(14)
        .halign(gtk::Align::Center)
        .build();

    // Embedded icon: doesn't depend on runtime paths.
    let bytes = glib::Bytes::from_static(include_bytes!(
        "../assets/brand/gmount-drive-icon-256.png"
    ));
    if let Ok(texture) = gdk::Texture::from_bytes(&bytes) {
        let img = gtk::Image::from_paintable(Some(&texture));
        img.set_pixel_size(64);
        row.append(&img);
    }

    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .build();
    let name = gtk::Label::builder()
        .label(crate::APP_NAME)
        .xalign(0.0)
        .build();
    name.add_css_class("title-1");
    let tagline = gtk::Label::builder()
        .label("Cloud mount for desktop")
        .xalign(0.0)
        .build();
    tagline.add_css_class("dim-label");
    text.append(&name);
    text.append(&tagline);
    row.append(&text);

    row.upcast()
}

/// An adw::SpinRow wrapped with a helper to save the config when the value changes.
struct SpinSetting {
    row: adw::SpinRow,
}

impl SpinSetting {
    /// Connects the value change: applies `apply` on the freshly-loaded config and saves it.
    fn connect_changed_save<F>(&self, apply: F)
    where
        F: Fn(&mut appconfig::Config, f64) + 'static,
    {
        self.row.connect_value_notify(move |r| {
            let mut c = appconfig::Config::load();
            apply(&mut c, r.value());
            let _ = c.save();
        });
    }
}

/// Creates an integer numeric row for Preferences.
fn spin_row(title: &str, value: f64, lower: f64, upper: f64) -> SpinSetting {
    let adj = gtk::Adjustment::new(value, lower, upper, 1.0, 10.0, 0.0);
    let row = adw::SpinRow::builder()
        .title(title)
        .adjustment(&adj)
        .climb_rate(1.0)
        .digits(0)
        .build();
    SpinSetting { row }
}

fn primary_button(label: &str) -> gtk::Button {
    let b = gtk::Button::with_label(label);
    b.add_css_class("suggested-action");
    b.add_css_class("pill");
    b.set_hexpand(true);
    b
}

fn icon(name: &str) -> gtk::Image {
    gtk::Image::from_icon_name(name)
}
