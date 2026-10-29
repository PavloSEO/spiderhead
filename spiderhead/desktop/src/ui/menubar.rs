//! The native menu bar. On macOS the menu is a real `NSMenu` built with muda;
//! every item resolves to a control command (`crate::menus::lookup`), so the
//! menu, the widgets and `spiderhead-desktop-ctl` share one dispatch surface.
//! Elsewhere this module is a no-op — the egui hotkeys cover those builds.

/// Install the application menu. Call from the first frame: the windowing
/// layer installs its own default menu when the app finishes launching, which
/// happens after the creation callback and would replace an earlier menu.
pub fn install() {
    #[cfg(target_os = "macos")]
    imp::install();
}

/// Drain pending menu events into `(method, params)` dispatches.
pub fn drain() -> Vec<(&'static str, serde_json::Value)> {
    #[cfg(target_os = "macos")]
    return imp::drain();
    #[cfg(not(target_os = "macos"))]
    Vec::new()
}

#[cfg(target_os = "macos")]
mod imp {
    use muda::accelerator::Accelerator;
    use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};

    use crate::menus::{ItemSpec, MenuSpec};

    thread_local! {
        /// The installed menu: NSApp keeps only a weak hold on the items'
        /// handlers, so the tree must outlive `install`.
        static MENU: std::cell::RefCell<Option<Menu>> = const { std::cell::RefCell::new(None) };
    }

    /// Build the menu tree from the shared spec and hand it to NSApp.
    pub fn install() {
        let menu = Menu::new();
        for spec in crate::menus::menus() {
            let _ = menu.append(&build_menu(spec));
        }
        menu.init_for_nsapp();
        MENU.with(|m| *m.borrow_mut() = Some(menu));
    }

    /// One top-level submenu (title + rows).
    fn build_menu(spec: &MenuSpec) -> Submenu {
        let sub = Submenu::new(spec.title, true);
        for item in spec.items {
            append_item(&sub, item);
        }
        sub
    }

    /// Append one spec row to a submenu.
    fn append_item(menu: &Submenu, item: &ItemSpec) {
        match item {
            ItemSpec::Sep => {
                let _ = menu.append(&PredefinedMenuItem::separator());
            }
            ItemSpec::Item {
                id, label, accel, ..
            } => {
                let accelerator = accel.parse::<Accelerator>().ok();
                let _ = menu.append(&MenuItem::with_id(*id, *label, true, accelerator));
            }
            ItemSpec::Sub { label, items } => {
                let sub = Submenu::new(*label, true);
                for item in *items {
                    append_item(&sub, item);
                }
                let _ = menu.append(&sub);
            }
        }
    }

    /// Events accumulated since the last frame → control dispatches.
    pub fn drain() -> Vec<(&'static str, serde_json::Value)> {
        let mut out = Vec::new();
        for event in MenuEvent::receiver().try_iter() {
            if let Some(call) = crate::menus::lookup(event.id.0.as_str()) {
                out.push(call);
            }
        }
        out
    }
}
