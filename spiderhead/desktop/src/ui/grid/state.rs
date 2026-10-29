//! Widget-side state of the big table: the row selection (by stable row keys,
//! so sorting and filtering keep the same rows highlighted), the scroll
//! position, transient paint state (resize toast, copied-range marching
//! border) and the double-click popover. Everything but the layout lives in
//! the per-window view state, never persisted.

use super::geom::{ColLayout, Zoom};
use std::collections::BTreeSet;
use std::time::Instant;

/// Selection of the grid: a set of row keys plus the anchor and the active
/// key. Keys are the rows' stable ids (the URL table uses `url_id` as text),
/// so a sort or filter keeps the selection attached to the same rows.
///
/// Ctrl+A on a huge set cannot materialize every key, so `all` marks "every
/// row of the current filtered set"; then `keys` holds the *exceptions*
/// (rows toggled off). `len_total(total)` reports the real count.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// Every row of the current set is selected (`keys` = exceptions).
    pub all: bool,
    /// Selected keys — or, with `all`, the unselected exceptions.
    pub keys: BTreeSet<String>,
    /// Where Shift-click extends from.
    pub anchor: Option<String>,
    /// The row the details panel shows (last clicked).
    pub active: Option<String>,
}

impl Selection {
    /// True when no row is selected.
    pub fn is_empty(&self) -> bool {
        !self.all && self.keys.is_empty()
    }

    /// Number of selected rows for a set of `total` rows.
    pub fn len_total(&self, total: usize) -> usize {
        if self.all {
            total.saturating_sub(self.keys.len())
        } else {
            self.keys.len()
        }
    }

    /// True when the row key is selected.
    pub fn contains(&self, key: &str) -> bool {
        if self.all {
            !self.keys.contains(key)
        } else {
            self.keys.contains(key)
        }
    }

    /// Click without modifiers: only this row.
    pub fn click(&mut self, key: &str) {
        self.all = false;
        self.keys.clear();
        self.keys.insert(key.to_string());
        self.anchor = Some(key.to_string());
        self.active = Some(key.to_string());
    }

    /// Ctrl-click: add the row, or remove it when it was selected already.
    /// With `all` on, the removal goes to the exception set; adding clears
    /// the mode (its keys would be exceptions, and they are unknown here).
    pub fn ctrl_click(&mut self, key: &str) {
        if self.all {
            if !self.keys.remove(key) {
                self.keys.insert(key.to_string());
            }
            self.active = Some(key.to_string());
            self.anchor = Some(key.to_string());
            return;
        }
        if !self.keys.remove(key) {
            self.keys.insert(key.to_string());
        }
        self.anchor = Some(key.to_string());
        self.active = Some(key.to_string());
    }

    /// Shift-click: a contiguous run from the anchor to this row. The run is
    /// over the row order the caller walks in (`run` of the visible range),
    /// so the caller resolves the anchor index itself.
    pub fn shift_click(&mut self, key: &str, run: &[String]) {
        let Some(anchor) = self.anchor.clone() else {
            self.click(key);
            return;
        };
        let (Some(a), Some(b)) = (
            run.iter().position(|k| *k == anchor),
            run.iter().position(|k| k == key),
        ) else {
            self.click(key);
            return;
        };
        self.all = false;
        self.keys.clear();
        self.keys.extend(run[a.min(b)..=a.max(b)].iter().cloned());
        self.active = Some(key.to_string());
    }

    /// Select the whole current set (Ctrl+A): the flag mode with no keys.
    pub fn select_all(&mut self) {
        self.all = true;
        self.keys.clear();
    }

    /// Replace the whole selection with explicit keys (control channel).
    pub fn set_all(&mut self, keys: impl IntoIterator<Item = String>) {
        self.all = false;
        self.keys = keys.into_iter().collect();
        self.active = self.keys.iter().next_back().cloned();
        self.anchor = self.active.clone();
    }

    /// Keep only the keys still present in `visible` (a filter dropped the
    /// rest); the active row follows when it left the set. The `all` mode
    /// survives: the set itself is what shrank.
    pub fn retain_visible(&mut self, visible: &BTreeSet<String>) {
        if !self.all {
            self.keys.retain(|k| visible.contains(k));
        }
        if let Some(a) = &self.active {
            if !self.keys.contains(a) {
                self.active = None;
            }
        }
        if let Some(a) = &self.anchor {
            if !self.keys.contains(a) {
                self.anchor = self.keys.iter().next().cloned();
            }
        }
    }

    /// Drop the whole selection (Esc, click on empty space).
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Transient feedback of the grid, reset by time or by the next action.
#[derive(Debug, Clone, Default)]
pub struct Transients {
    /// Resize toast: the text to show near the pointer, with its timestamp.
    pub toast: Option<(String, Instant)>,
    /// The selection was just copied: draw the marching border until this
    /// instant (the marchers themselves phase with the frame time).
    pub copied_until: Option<Instant>,
    /// Double-click popover: the cell (row key, column) and its full text.
    pub popover: Option<Popover>,
}

/// One open full-text popover (double click on a cell).
#[derive(Debug, Clone, PartialEq)]
pub struct Popover {
    /// Row key the popover belongs to (closes when the row goes away).
    pub row: String,
    /// Column id the popover shows.
    pub col: String,
    /// Full text of the cell.
    pub text: String,
    /// Screen point the popover anchors to (the cell's corner).
    pub pos: [f32; 2],
}

/// One pointer session on the grid (resize, column move, range select,
/// scrollbar drag). Lives in [`GridUi`], set on drag start, cleared on stop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Drag {
    /// Resizing visible column `i`; pointer screen X at drag start.
    Resize {
        /// Column index being resized.
        col: usize,
        /// Pointer X when the drag began.
        start_x: f32,
        /// Column base width when the drag began.
        start_w: f32,
    },
    /// Moving column `i` to a new place.
    Move {
        /// Column index being moved.
        col: usize,
    },
    /// Selecting a row range; the anchor row index.
    Select {
        /// Row index the drag started on.
        anchor: i64,
    },
    /// Dragging the vertical scrollbar; grab offset inside the thumb.
    VBar {
        /// Pointer offset inside the thumb when the drag began.
        grab: f32,
    },
}

/// Widget-side state of one grid: zoom, column layout, selection, scroll,
/// transients and the live drag. One instance per grid lives in the app's
/// view state; the widget mutates it, persistence snapshots `layout`/`zoom`.
#[derive(Debug, Clone)]
pub struct GridUi {
    /// Zoom of the grid (the fixed ladder).
    pub zoom: Zoom,
    /// Column layout: order, widths, hidden set, frozen count.
    pub layout: ColLayout,
    /// Row selection by stable keys.
    pub selection: Selection,
    /// Transient paint state.
    pub transients: Transients,
    /// Scroll position as the top row number (f64; the fraction is the
    /// pixel offset into the row). Pixels go stale past ~16.7M at f32 — a
    /// million dense rows are 22M px, so the position stays row-counted.
    pub scroll_y: f64,
    /// Where the scroll is heading (same units); wheel deltas accumulate
    /// here and the drawn position follows instantly.
    pub scroll_target_y: f64,
    /// Horizontal scroll in base points.
    pub scroll_x: f32,
    /// Live pointer session, if any.
    pub(crate) drag: Option<Drag>,
    /// Index of the active row (keyboard navigation starts here).
    pub(crate) active_index: Option<i64>,
}

impl Default for GridUi {
    fn default() -> Self {
        Self {
            zoom: Zoom::new(100.0),
            layout: ColLayout::default(),
            selection: Selection::default(),
            transients: Transients::default(),
            scroll_y: 0.0,
            scroll_target_y: 0.0,
            scroll_x: 0.0,
            drag: None,
            active_index: None,
        }
    }
}

/// How long the resize toast stays up.
pub const TOAST_SECS: f32 = 1.2;
/// How long the copied-range border marches.
pub const COPIED_SECS: f32 = 1.2;

impl Transients {
    /// Show a toast; a second toast replaces the first (never queues).
    pub fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    /// True while the toast is alive.
    pub fn toast_alive(&self) -> bool {
        self.toast
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed().as_secs_f32() < TOAST_SECS)
    }

    /// Mark the selection as copied; the border marches for a short while.
    pub fn mark_copied(&mut self) -> bool {
        self.copied_until = Some(Instant::now());
        true
    }

    /// True while the copied border should march.
    pub fn copied_alive(&self) -> bool {
        self.copied_until
            .is_some_and(|t| t.elapsed().as_secs_f32() < COPIED_SECS)
    }

    /// Close the popover, the toast and the copied border (Esc, click
    /// elsewhere).
    pub fn dismiss(&mut self) {
        self.popover = None;
        self.toast = None;
        self.copied_until = None;
    }
}

#[cfg(test)]
mod state_tests {
    use super::*;

    fn keys(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn click_selects_one_row_and_sets_anchor() {
        let mut s = Selection::default();
        s.click("7");
        assert_eq!(s.len_total(100), 1);
        assert!(s.contains("7"));
        assert_eq!(s.anchor.as_deref(), Some("7"));
        assert_eq!(s.active.as_deref(), Some("7"));
    }

    #[test]
    fn ctrl_click_toggles_one_row() {
        let mut s = Selection::default();
        s.click("1");
        s.ctrl_click("2");
        assert_eq!(s.len_total(100), 2);
        s.ctrl_click("1");
        assert_eq!(s.len_total(100), 1);
        assert!(!s.contains("1"));
        assert_eq!(
            s.active.as_deref(),
            Some("1"),
            "ctrl keeps the last row active"
        );
    }

    #[test]
    fn select_all_is_flag_mode_with_exceptions() {
        let mut s = Selection::default();
        s.select_all();
        assert_eq!(s.len_total(50_000), 50_000);
        assert!(s.contains("anything"));
        s.ctrl_click("7");
        assert_eq!(s.len_total(50_000), 49_999);
        assert!(!s.contains("7"));
        assert!(s.contains("8"));
        // A plain click leaves the flag mode.
        s.click("9");
        assert!(!s.all);
        assert_eq!(s.len_total(50_000), 1);
    }

    #[test]
    fn shift_click_selects_the_run_from_the_anchor() {
        let mut s = Selection::default();
        s.click("b");
        let run = keys(&["a", "b", "c", "d"]);
        s.shift_click("d", &run);
        assert_eq!(s.len_total(100), 3);
        assert!(s.contains("b") && s.contains("c") && s.contains("d"));
        // Reverse direction keeps the same set.
        s.click("d");
        s.shift_click("a", &run);
        assert_eq!(s.len_total(100), 4);
        // A missing anchor falls back to a plain click.
        s.anchor = None;
        s.shift_click("c", &run);
        assert_eq!(s.len_total(100), 1);
    }

    #[test]
    fn ctrl_a_replaces_and_sets_active() {
        let mut s = Selection::default();
        s.set_all(keys(&["3", "1", "2"]));
        assert_eq!(s.len_total(3), 3);
        assert_eq!(s.active.as_deref(), Some("3"));
    }

    #[test]
    fn retain_visible_drops_gone_rows_and_fixes_active() {
        let mut s = Selection::default();
        s.set_all(keys(&["1", "2", "3"]));
        s.active = Some("2".into());
        let visible: BTreeSet<String> = keys(&["2", "3"]).into_iter().collect();
        s.retain_visible(&visible);
        assert_eq!(s.len_total(100), 2);
        assert_eq!(s.active.as_deref(), Some("2"));
        let visible: BTreeSet<String> = keys(&["3"]).into_iter().collect();
        s.retain_visible(&visible);
        assert_eq!(s.active, None, "an active row that left the set clears");
        assert_eq!(s.anchor.as_deref(), Some("3"), "anchor follows a survivor");
    }

    #[test]
    fn retain_visible_keeps_the_all_mode() {
        let mut s = Selection::default();
        s.select_all();
        let visible: BTreeSet<String> = keys(&["1"]).into_iter().collect();
        s.retain_visible(&visible);
        assert!(s.all, "the set shrank, the mode did not");
    }

    #[test]
    fn clear_resets_everything() {
        let mut s = Selection::default();
        s.set_all(keys(&["1"]));
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.active, None);
    }

    #[test]
    fn transients_toast_replaces_and_expires() {
        let mut t = Transients::default();
        t.toast("180 px");
        t.toast("220 px");
        assert!(t.toast_alive());
        assert_eq!(
            t.toast.as_ref().map(|(text, _)| text.as_str()),
            Some("220 px")
        );
        // An aged toast reports dead; dismiss closes everything.
        t.dismiss();
        assert!(!t.toast_alive());
        assert!(!t.copied_alive());
        assert!(t.popover.is_none());
    }

    #[test]
    fn transients_copied_border_lives_briefly() {
        let mut t = Transients::default();
        assert!(t.mark_copied());
        assert!(t.copied_alive());
        t.dismiss();
        assert!(!t.copied_alive());
    }

    #[test]
    fn popover_holds_row_and_text() {
        let mut t = Transients {
            popover: Some(Popover {
                row: "9".into(),
                col: "title".into(),
                text: "Полный заголовок".into(),
                pos: [10.0, 20.0],
            }),
            ..Transients::default()
        };
        assert_eq!(t.popover.clone().unwrap().text, "Полный заголовок");
        t.dismiss();
        assert!(t.popover.is_none());
    }

    #[test]
    fn grid_ui_defaults_to_100_percent_zoom() {
        let g = GridUi::default();
        assert_eq!(g.zoom.percent(), 100.0);
        assert_eq!(g.scroll_y, 0.0);
        assert_eq!(g.scroll_x, 0.0);
        assert!(g.drag.is_none());
        assert!(g.selection.is_empty());
        assert_eq!(g.layout, ColLayout::default());
    }
}
