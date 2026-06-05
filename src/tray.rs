//! System tray icon (StatusNotifierItem) with ksni. Runs on its own thread and sends
//! actions to GTK's main thread over an async channel.

use async_channel::Sender;
use ksni::menu::{MenuItem, StandardItem};
use ksni::Tray;

#[derive(Debug, Clone, Copy)]
pub enum TrayAction {
    ShowWindow,
    ToggleMount,
    OpenFolder,
    Quit,
}

pub struct DriveTray {
    pub connected: bool,
    pub mounted: bool,
    tx: Sender<TrayAction>,
    /// Brand icon already decoded to an ARGB pixmap (empty if decoding failed).
    icons: Vec<ksni::Icon>,
}

impl DriveTray {
    pub fn new(tx: Sender<TrayAction>) -> Self {
        let icons = load_icon(include_bytes!("../assets/brand/gmount-drive-app-icon-256.png"))
            .into_iter()
            .collect();
        Self {
            connected: false,
            mounted: false,
            tx,
            icons,
        }
    }
}

/// Decodes an embedded PNG to a `ksni::Icon` (ARGB32 in network byte order, as StatusNotifier wants).
fn load_icon(bytes: &[u8]) -> Option<ksni::Icon> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for p in img.pixels() {
        let [r, g, b, a] = p.0;
        data.extend_from_slice(&[a, r, g, b]);
    }
    Some(ksni::Icon {
        width: w as i32,
        height: h as i32,
        data,
    })
}

impl Tray for DriveTray {
    fn id(&self) -> String {
        "gmount-drive".into()
    }

    fn title(&self) -> String {
        "GMount Drive".into()
    }

    /// Brand icon as an embedded pixmap (doesn't depend on the installed icon theme).
    /// If decoding failed, we return empty and fall back to the themed `icon_name` below.
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn icon_name(&self) -> String {
        // NOTE: if we return a name here, the host (AppIndicator) prioritizes it and IGNORES the
        // pixmap. So, if we have the brand pixmap, we return empty so the host uses it.
        if self.icons.is_empty() {
            if self.mounted {
                "folder-remote".into()
            } else {
                "drive-harddisk".into()
            }
        } else {
            String::new()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            item("Show window", true, &self.tx, TrayAction::ShowWindow),
            item(
                if self.mounted { "Unmount" } else { "Mount" },
                self.connected,
                &self.tx,
                TrayAction::ToggleMount,
            ),
            item("Open folder", self.mounted, &self.tx, TrayAction::OpenFolder),
            MenuItem::Separator,
            item("Quit", true, &self.tx, TrayAction::Quit),
        ]
    }
}

fn item(
    label: &str,
    enabled: bool,
    tx: &Sender<TrayAction>,
    action: TrayAction,
) -> MenuItem<DriveTray> {
    let tx = tx.clone();
    StandardItem {
        label: label.into(),
        enabled,
        activate: Box::new(move |_: &mut DriveTray| {
            let _ = tx.try_send(action);
        }),
        ..Default::default()
    }
    .into()
}
