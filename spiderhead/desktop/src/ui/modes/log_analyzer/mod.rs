//! Анализ логов mode. The log-analysis core is not ported yet
//! (`spiderhead-log-analyzer`); every section shows the canvas empty state until it is.

use crate::ui::kit::icons as ph;
use eframe::egui;

use crate::i18n;
use crate::theme::Palette;
use crate::ui::kit;

/// Empty state shared by the logs sections.
pub fn show(ui: &mut egui::Ui, p: &Palette) {
    kit::empty_state(
        ui,
        p,
        ph::DESCRIPTION,
        i18n::LOGS_EMPTY_TITLE,
        i18n::LOGS_EMPTY_HINT,
        None,
    );
}
