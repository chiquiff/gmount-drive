//! Guided "BYO credential" wizard: helps the user create their own Google Cloud client_id
//! in ~5 minutes (one time) and connects the Drive with those own credentials.
//!
//! Why it exists: rclone's shared client is rate-limited (~2 files/sec for small files) and
//! capped at 100 users. Your own credential removes that and, by publishing the app to
//! "Production", the refresh token never expires (in "Testing" it expires after 7 days →
//! weekly re-login). That's why step 4 ("Publish app") is NON-NEGOTIABLE.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};

/// An informational step: title, body (with markup), and a deep link to the exact page.
struct Info {
    title: &'static str,
    body: &'static str,
    link_label: &'static str,
    link_url: &'static str,
}

/// The 5 steps before pasting. Each deep link opens the exact Google Cloud page.
const INFO: [Info; 5] = [
    Info {
        title: "1. Create a project",
        body: "You'll create a free <b>project</b> in Google Cloud (it's just a container \
               for your credentials).\n\n\
               • Click the button below.\n\
               • Give it any name (e.g. <i>my-drive</i>) and create it.\n\
               • When it's done, make sure it is <b>selected at the top</b>.",
        link_label: "Open: create project",
        link_url: "https://console.cloud.google.com/projectcreate",
    },
    Info {
        title: "2. Enable the Drive API",
        body: "You need to turn on the <b>Google Drive API</b> for your project.\n\n\
               • Check that your new project is selected at the top.\n\
               • Click <b>Enable</b>.",
        link_label: "Open: enable Drive API",
        link_url: "https://console.cloud.google.com/apis/library/drive.googleapis.com",
    },
    Info {
        title: "3. Consent screen",
        body: "Configure the <b>consent screen</b> (what you'll see when signing in).\n\n\
               • User type: <b>External</b>.\n\
               • Fill in the app name and your contact email.\n\
               • If it asks for <b>scopes</b>, add the Drive one: \
               <tt>.../auth/drive</tt>.",
        link_label: "Open: consent screen",
        link_url: "https://console.cloud.google.com/auth/overview",
    },
    Info {
        title: "4. Publish the app (important!)",
        body: "<b>This step keeps your session from expiring every 7 days.</b>\n\n\
               • Go to the <b>Audience</b> section.\n\
               • Click <b>Publish app</b> to move it to <b>Production</b>.\n\
               • Confirm if asked. (You don't need Google verification for personal use.)",
        link_label: "Open: publish app",
        link_url: "https://console.cloud.google.com/auth/audience",
    },
    Info {
        title: "5. Create the OAuth credential",
        body: "Now you generate the <b>client_id</b> and <b>client_secret</b>.\n\n\
               • Click <b>Create credentials → OAuth client ID</b>.\n\
               • Application type: <b>Desktop app</b>.\n\
               • Copy the <b>client ID</b> and the <b>secret</b> it shows you.\n\n\
               You'll paste them in the next step.",
        link_label: "Open: create credential",
        link_url: "https://console.cloud.google.com/apis/credentials",
    },
];

/// Total pages = informational steps + the paste/validation page.
const N_PAGES: usize = INFO.len() + 1;

struct Wizard {
    window: adw::Window,
    stack: gtk::Stack,
    back_btn: gtk::Button,
    next_btn: gtk::Button,
    progress: gtk::Label,
    id_entry: gtk::Entry,
    secret_entry: gtk::Entry,
    upload_btn: gtk::Button,
    /// First-page shortcut: upload the JSON and jump straight to connecting.
    upload_btn_intro: gtk::Button,
    error_label: gtk::Label,
    idx: Cell<usize>,
    /// True while an OAuth flow is in progress ("Back" button becomes "Cancel").
    connecting: Cell<bool>,
    /// Flag to cancel the in-progress OAuth (closing the window or clicking "Cancel").
    cancel: Arc<AtomicBool>,
    on_success: Box<dyn Fn()>,
}

/// Opens the wizard as a modal window over the main one. Calls `on_success` when the account
/// is successfully connected (so the main UI can refresh).
pub fn launch(parent: &adw::ApplicationWindow, on_success: impl Fn() + 'static) {
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .vexpand(true)
        .build();

    // Informational pages. The first one also carries the "upload JSON and connect" shortcut.
    let upload_btn_intro = gtk::Button::new();
    for (i, info) in INFO.iter().enumerate() {
        let page = if i == 0 {
            first_page(info, &upload_btn_intro)
        } else {
            info_page(info)
        };
        stack.add_named(&page, Some(&format!("p{i}")));
    }

    // Final page: paste credentials.
    let id_entry = gtk::Entry::builder()
        .placeholder_text("Paste your client_id (ends in .apps.googleusercontent.com)")
        .hexpand(true)
        .build();
    let secret_entry = gtk::Entry::builder()
        .placeholder_text("Paste your client_secret")
        .hexpand(true)
        .build();
    let error_label = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    error_label.add_css_class("error");
    let upload_btn = gtk::Button::new();
    stack.add_named(
        &paste_page(&id_entry, &secret_entry, &upload_btn, &error_label),
        Some(&format!("p{}", INFO.len())),
    );

    // Bottom navigation bar.
    let back_btn = gtk::Button::with_label("← Back");
    let next_btn = gtk::Button::with_label("Next →");
    next_btn.add_css_class("suggested-action");
    let progress = gtk::Label::new(None);
    progress.add_css_class("dim-label");

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let navbar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(8)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    navbar.append(&back_btn);
    navbar.append(&progress);
    navbar.append(&spacer);
    navbar.append(&next_btn);

    let header = adw::HeaderBar::new();
    // Each step's content goes inside a ScrolledWindow so that, if it's taller than the window,
    // it scrolls — and the navigation bar (with "Validate and connect") stays ALWAYS pinned and
    // visible at the bottom, regardless of the window size.
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&stack)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    content.append(&scroller);
    content.append(&navbar);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));

    let window = adw::Window::builder()
        .title("Use my own credential")
        .modal(true)
        .default_width(560)
        .default_height(560)
        .content(&toolbar)
        .build();
    window.set_transient_for(Some(parent));

    let w = Rc::new(Wizard {
        window,
        stack,
        back_btn,
        next_btn,
        progress,
        id_entry,
        secret_entry,
        upload_btn,
        upload_btn_intro,
        error_label,
        idx: Cell::new(0),
        connecting: Cell::new(false),
        cancel: Arc::new(AtomicBool::new(false)),
        on_success: Box::new(on_success),
    });

    // Closing the window cancels any in-progress OAuth, so no thread is left hanging waiting
    // for the browser (and the app returns to a working state).
    let cancel_on_close = w.cancel.clone();
    w.window.connect_close_request(move |_| {
        cancel_on_close.store(true, Ordering::Relaxed);
        glib::Propagation::Proceed
    });

    let wu = w.clone();
    w.upload_btn.connect_clicked(move |_| wu.clone().pick_json());
    let wi = w.clone();
    w.upload_btn_intro.connect_clicked(move |_| wi.clone().pick_json());

    let wb = w.clone();
    w.back_btn.connect_clicked(move |_| {
        if wb.connecting.get() {
            // Cancel the in-progress OAuth: the thread detects it and returns right away.
            wb.cancel.store(true, Ordering::Relaxed);
            wb.back_btn.set_label("Cancelling…");
            wb.back_btn.set_sensitive(false);
        } else {
            wb.clone().go(-1);
        }
    });
    let wn = w.clone();
    w.next_btn.connect_clicked(move |_| {
        if wn.idx.get() + 1 < N_PAGES {
            wn.clone().go(1);
        } else {
            wn.clone().validate();
        }
    });

    w.update_nav();
    w.window.present();
}

impl Wizard {
    /// Moves forward or back a page (clamped) and updates the bar.
    fn go(self: Rc<Self>, delta: i32) {
        let cur = self.idx.get() as i32;
        let new = (cur + delta).clamp(0, N_PAGES as i32 - 1) as usize;
        self.goto(new);
    }

    /// Jumps to a specific page (clamped) and updates the bar.
    fn goto(&self, idx: usize) {
        let idx = idx.min(N_PAGES - 1);
        self.idx.set(idx);
        self.stack.set_visible_child_name(&format!("p{idx}"));
        self.update_nav();
    }

    /// Adjusts the button labels/sensitivity for the current page (normal state).
    fn update_nav(&self) {
        let i = self.idx.get();
        self.progress.set_text(&format!("Step {}/{}", i + 1, N_PAGES));
        self.back_btn.set_label("← Back");
        self.back_btn.set_sensitive(i > 0);
        if i + 1 < N_PAGES {
            self.next_btn.set_label("Next →");
        } else {
            self.next_btn.set_label("Validate and connect");
        }
        self.next_btn.set_sensitive(true);
    }

    /// Opens a file chooser to pick the Google credentials JSON, parses it and fills the
    /// client_id / client_secret fields.
    fn pick_json(self: Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Credentials JSON"));
        filter.add_mime_type("application/json");
        filter.add_pattern("*.json");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);

        let dialog = gtk::FileDialog::builder()
            .title("Choose the credentials JSON file")
            .filters(&filters)
            .modal(true)
            .build();

        let this = self.clone();
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |res| {
            let Ok(file) = res else { return }; // the user cancelled
            let Some(path) = file.path() else {
                this.show_error("Couldn't access that file.");
                return;
            };
            match parse_creds_json(&path) {
                Ok((id, secret)) => {
                    this.id_entry.set_text(&id);
                    this.secret_entry.set_text(&secret);
                    this.error_label.set_visible(false);
                    // Jump straight to the final page to connect (works whether it was uploaded
                    // from the page-1 shortcut or from the paste page).
                    this.goto(N_PAGES - 1);
                }
                Err(e) => this.show_error(&format!("Couldn't read that JSON: {e}")),
            }
        });
    }

    fn show_error(&self, msg: &str) {
        self.error_label.set_text(msg);
        self.error_label.set_visible(true);
    }

    /// Takes what was pasted, runs a test login with those credentials and, if the token comes
    /// back, the account is created → closes the wizard and refreshes the main UI.
    fn validate(self: Rc<Self>) {
        let id = self.id_entry.text().trim().to_string();
        let secret = self.secret_entry.text().trim().to_string();
        if id.is_empty() || secret.is_empty() {
            self.show_error("Paste the client_id and client_secret before continuing.");
            return;
        }
        self.error_label.set_visible(false);

        // "Connecting" state: the browser opens; the left button becomes "Cancel" and stays
        // ALWAYS enabled, so if you close the browser or change your mind you come back instantly.
        self.connecting.set(true);
        self.cancel.store(false, Ordering::Relaxed);
        self.back_btn.set_label("Cancel");
        self.back_btn.set_sensitive(true);
        self.next_btn.set_sensitive(false);
        self.next_btn.set_label("Connecting… (authorize in the browser)");

        let cancel = self.cancel.clone();
        let this = self.clone();
        glib::spawn_future_local(async move {
            let res = gio::spawn_blocking(move || {
                crate::oauth::connect_with_creds(&id, &secret, &cancel)
            })
            .await;
            this.connecting.set(false);
            match res {
                Ok(Ok(true)) => {
                    (this.on_success)();
                    this.window.close();
                }
                // Cancelled (Cancel or closing the window): no error, just re-enable.
                Ok(Ok(false)) => this.update_nav(),
                Ok(Err(e)) => {
                    this.show_error(&format!("It didn't work: {e}"));
                    this.update_nav();
                }
                Err(_) => {
                    this.show_error("Internal error while validating.");
                    this.update_nav();
                }
            }
        });
    }
}

/// First page: a shortcut at the top ("I already have the JSON → upload and connect"), a
/// separator, and below it the normal step-1 content.
fn first_page(info: &Info, upload_btn: &gtk::Button) -> gtk::Widget {
    let shortcut_title = gtk::Label::builder()
        .label("Already have your credentials file?")
        .xalign(0.0)
        .build();
    shortcut_title.add_css_class("title-4");

    let shortcut_hint = gtk::Label::builder()
        .label("If you already created your client_id in Google and downloaded the JSON, \
                upload it here and connect directly, without going through the steps.")
        .wrap(true)
        .xalign(0.0)
        .build();
    shortcut_hint.add_css_class("dim-label");

    let upload_content = adw::ButtonContent::builder()
        .icon_name("document-open-symbolic")
        .label("Upload JSON and connect")
        .build();
    upload_btn.set_child(Some(&upload_content));
    upload_btn.set_halign(gtk::Align::Start);
    upload_btn.add_css_class("suggested-action");
    upload_btn.add_css_class("pill");

    let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
    let or_label = gtk::Label::builder()
        .label("— or follow the steps to create it —")
        .xalign(0.0)
        .build();
    or_label.add_css_class("dim-label");

    // Normal step content.
    let title = gtk::Label::builder().label(info.title).xalign(0.0).build();
    title.add_css_class("title-2");
    let body = gtk::Label::builder()
        .label(info.body)
        .use_markup(true)
        .wrap(true)
        .xalign(0.0)
        .build();
    let link = deep_link_button(info);

    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    b.append(&shortcut_title);
    b.append(&shortcut_hint);
    b.append(upload_btn);
    b.append(&sep);
    b.append(&or_label);
    b.append(&title);
    b.append(&body);
    b.append(&link);

    let clamp = adw::Clamp::builder().maximum_size(480).child(&b).build();
    clamp.upcast()
}

/// Builds an informational page (title + markup body + deep-link button).
fn info_page(info: &Info) -> gtk::Widget {
    let title = gtk::Label::builder().label(info.title).xalign(0.0).build();
    title.add_css_class("title-2");

    let body = gtk::Label::builder()
        .label(info.body)
        .use_markup(true)
        .wrap(true)
        .xalign(0.0)
        .build();

    let link = deep_link_button(info);

    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    b.append(&title);
    b.append(&body);
    b.append(&link);

    let clamp = adw::Clamp::builder().maximum_size(480).child(&b).build();
    clamp.upcast()
}

/// "Pill" button that opens the step's deep link in the browser.
fn deep_link_button(info: &Info) -> gtk::Button {
    let link = gtk::Button::builder().halign(gtk::Align::Start).build();
    let content = adw::ButtonContent::builder()
        .icon_name("web-browser-symbolic")
        .label(info.link_label)
        .build();
    link.set_child(Some(&content));
    link.add_css_class("pill");
    let url = info.link_url;
    link.connect_clicked(move |_| open_url(url));
    link
}

/// Builds the last page: upload the JSON (or paste by hand) client_id + client_secret and validate.
fn paste_page(
    id_entry: &gtk::Entry,
    secret_entry: &gtk::Entry,
    upload_btn: &gtk::Button,
    error_label: &gtk::Label,
) -> gtk::Widget {
    let title = gtk::Label::builder()
        .label("6. Load your credentials")
        .xalign(0.0)
        .build();
    title.add_css_class("title-2");

    let hint = gtk::Label::builder()
        .label("Easiest: <b>upload the JSON file</b> you downloaded when creating the credential \
                (Google offers it with the <i>Download JSON</i> button). If you prefer, you can \
                also paste the values by hand below.")
        .use_markup(true)
        .wrap(true)
        .xalign(0.0)
        .build();

    // Prominent button to upload the JSON.
    let upload_content = adw::ButtonContent::builder()
        .icon_name("document-open-symbolic")
        .label("Upload JSON file")
        .build();
    upload_btn.set_child(Some(&upload_content));
    upload_btn.set_halign(gtk::Align::Start);
    upload_btn.add_css_class("suggested-action");
    upload_btn.add_css_class("pill");

    let or_label = gtk::Label::builder()
        .label("— or paste by hand —")
        .xalign(0.0)
        .build();
    or_label.add_css_class("dim-label");

    let group = adw::PreferencesGroup::new();
    let id_row = adw::ActionRow::builder().title("client_id").build();
    id_row.add_suffix(id_entry);
    let secret_row = adw::ActionRow::builder().title("client_secret").build();
    secret_row.add_suffix(secret_entry);
    group.add(&id_row);
    group.add(&secret_row);

    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    b.append(&title);
    b.append(&hint);
    b.append(upload_btn);
    b.append(&or_label);
    b.append(&group);
    b.append(error_label);

    let clamp = adw::Clamp::builder().maximum_size(480).child(&b).build();
    clamp.upcast()
}

/// Parses the credentials JSON that Google Cloud downloads and returns (client_id, client_secret).
/// The JSON wraps the keys in `"installed"` (Desktop client) or `"web"` (web client).
fn parse_creds_json(path: &std::path::Path) -> Result<(String, String), String> {
    let data = std::fs::read_to_string(path).map_err(|e| format!("couldn't open: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&data).map_err(|e| format!("not a valid JSON: {e}"))?;
    let inner = v.get("installed").or_else(|| v.get("web")).unwrap_or(&v);

    let id = inner
        .get("client_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "couldn't find 'client_id' in the file".to_string())?;
    let secret = inner
        .get("client_secret")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "couldn't find 'client_secret' in the file".to_string())?;

    Ok((id.to_string(), secret.to_string()))
}

/// Opens a URL in the default browser.
fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}
