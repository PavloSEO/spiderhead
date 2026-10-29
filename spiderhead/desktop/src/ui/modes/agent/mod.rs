//! Агентский режим: the project as the agent sees it — goal and tasks, inbox,
//! projects. Screens land here as the project surfaces are wired.

use crate::ui::kit::icons as ph;
use eframe::egui;

use crate::app::Nav;
use crate::i18n;
use crate::theme::Palette;
use crate::ui::kit;

/// Empty states of the agent sections until their screens are built.
pub fn show(ui: &mut egui::Ui, nav: Nav, p: &Palette) {
    let (icon, title, hint) = match nav {
        Nav::Inbox => (ph::INBOX, i18n::INBOX_TITLE, i18n::INBOX_HINT),
        Nav::Projects => (ph::WORKSPACES, i18n::PROJECTS_TITLE, i18n::PROJECTS_HINT),
        _ => (ph::CHECKLIST, i18n::WORK_TITLE, i18n::WORK_HINT),
    };
    kit::empty_state(ui, p, icon, title, hint, None);
}
