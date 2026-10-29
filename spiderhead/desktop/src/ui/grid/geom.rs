//! Geometry of the big table view: the zoom ladder and the column layout.
//! Pure data, no egui — every rule here is unit-tested against the numbers
//! the specification fixes (step list, wheel accumulation, freeze clipping).

use std::collections::{BTreeMap, BTreeSet};

/// Zoom of the grid view, in percent of the base sizes. Wheel input
/// accumulates in fractions of one click and fires whole steps; a step that
/// lies closer than 5% to the current value is skipped, and 100% is never
/// skipped when the way crosses it.
#[derive(Debug, Clone)]
pub struct Zoom {
    /// Current zoom in percent (one of `STEPS` after any wheel gesture).
    percent: f32,
    /// Accumulated wheel clicks since the last fired step.
    acc: f32,
}

impl Zoom {
    /// The fixed ladder, in percent (10–400).
    pub const STEPS: [f32; 22] = [
        10.0, 12.0, 15.0, 18.0, 21.0, 25.0, 30.0, 35.0, 42.0, 50.0, 60.0, 70.0, 85.0, 100.0, 120.0,
        140.0, 170.0, 200.0, 240.0, 280.0, 340.0, 400.0,
    ];

    /// Wheel clicks that must pile up before one step fires.
    const FIRE_THRESHOLD: f32 = 0.6;
    /// Accumulator clamp; a wild gesture cannot jump more than 32 steps.
    const ACC_LIMIT: f32 = 32.0;
    /// A candidate step closer than this fraction of the current value is
    /// skipped (no "zero effect" steps).
    const MIN_STEP_FRACTION: f32 = 0.05;

    /// A zoom at `percent` (snapped to the nearest ladder value).
    pub fn new(percent: f32) -> Self {
        Self {
            percent: Self::snap(percent),
            acc: 0.0,
        }
    }

    /// Current zoom in percent.
    pub fn percent(&self) -> f32 {
        self.percent
    }

    /// Scale factor for sizes (percent / 100).
    pub fn scale(&self) -> f32 {
        self.percent / 100.0
    }

    /// Set the zoom directly (menu, control command); snaps to the ladder.
    pub fn set_percent(&mut self, percent: f32) {
        self.percent = Self::snap(percent);
        self.acc = 0.0;
    }

    /// One wheel click fraction; `+` zooms in, `−` zooms out. Touchpads send
    /// small deltas that pile up here until a whole step fires.
    pub fn wheel(&mut self, clicks: f32) {
        self.acc = (self.acc + clicks).clamp(-Self::ACC_LIMIT, Self::ACC_LIMIT);
        while self.acc >= Self::FIRE_THRESHOLD {
            if self.step(1) {
                self.acc -= 1.0;
            } else {
                self.acc = 0.0;
                break;
            }
        }
        while self.acc <= -Self::FIRE_THRESHOLD {
            if self.step(-1) {
                self.acc += 1.0;
            } else {
                self.acc = 0.0;
                break;
            }
        }
    }

    /// One explicit step in `dir` (`+1` in, `−1` out); false at the ladder end.
    pub fn step(&mut self, dir: i8) -> bool {
        let Some(next) = Self::next_step(self.percent, dir) else {
            return false;
        };
        self.percent = next;
        true
    }

    /// Nearest ladder value to `percent`.
    fn snap(percent: f32) -> f32 {
        let p = percent.clamp(Self::STEPS[0], Self::STEPS[Self::STEPS.len() - 1]);
        let mut best = Self::STEPS[0];
        let mut best_d = f32::MAX;
        for s in Self::STEPS {
            let d = (s - p).abs();
            if d < best_d {
                best_d = d;
                best = s;
            }
        }
        best
    }

    /// Next ladder value strictly in `dir` at least 5% away; 100% always
    /// stops the way. `None` at the ladder ends.
    fn next_step(current: f32, dir: i8) -> Option<f32> {
        let idx = Self::STEPS
            .iter()
            .position(|s| *s == current)
            .map(|i| i as i32)
            .unwrap_or({
                // Off-ladder value (should not happen): find the first step
                // further in the direction and resume from it.
                if dir > 0 {
                    Self::STEPS
                        .iter()
                        .position(|s| *s > current)
                        .map(|i| i as i32 - 1)
                        .unwrap_or(0)
                } else {
                    Self::STEPS
                        .iter()
                        .rposition(|s| *s < current)
                        .map(|i| i as i32 + 1)
                        .unwrap_or(Self::STEPS.len() as i32 - 1)
                }
            });
        let mut i = idx;
        loop {
            i += dir as i32;
            if i < 0 || i as usize >= Self::STEPS.len() {
                return None;
            }
            let candidate = Self::STEPS[i as usize];
            let gap = (candidate - current).abs() / current;
            if candidate == 100.0 || gap >= Self::MIN_STEP_FRACTION {
                return Some(candidate);
            }
        }
    }
}

/// Column layout of one grid: the visible order, hidden names, per-column
/// widths (points at 100% zoom) and the number of frozen leading columns.
/// Persisted as JSON; unknown names and broken shapes fall back to defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColLayout {
    /// Explicit order of moved columns; columns not listed keep registry order.
    order: Vec<String>,
    /// Columns the user hid.
    hidden: BTreeSet<String>,
    /// Widths in points at 100%, for columns the user resized.
    widths: BTreeMap<String, f32>,
    /// Leading visible columns pinned to the left edge.
    frozen: usize,
}

impl ColLayout {
    /// The visible column names in display order: explicit moves first where
    /// they apply, then the registry order; hidden names are excluded.
    pub fn visible(&self, all: &[&str]) -> Vec<String> {
        let mut seen = std::collections::BTreeSet::<&str>::new();
        let mut out = Vec::with_capacity(all.len());
        for name in &self.order {
            if all.contains(&name.as_str()) && !self.hidden.contains(name) && seen.insert(name) {
                out.push(name.clone());
            }
        }
        for name in all {
            if !self.hidden.contains(*name) && seen.insert(name) {
                out.push((*name).to_string());
            }
        }
        out
    }

    /// Hide a column; the frozen count clips so it never covers the whole set.
    pub fn hide(&mut self, name: &str, all: &[&str]) {
        let visible = self.visible(all);
        if visible.len() <= 1 {
            return;
        }
        self.hidden.insert(name.to_string());
        let vis = self.visible(all).len();
        self.frozen = self.frozen.min(vis.saturating_sub(1));
    }

    /// Bring a hidden column back at its registry place.
    pub fn show(&mut self, name: &str) {
        self.hidden.remove(name);
    }

    /// True when the column is currently hidden.
    pub fn is_hidden(&self, name: &str) -> bool {
        self.hidden.contains(name)
    }

    /// Move `name` so it sits right before `before`; `before = None` moves it
    /// to the last visible place. No-op for unknown or hidden columns.
    pub fn move_before(&mut self, name: &str, before: Option<&str>, all: &[&str]) {
        let current = self.visible(all);
        if !current.iter().any(|c| c == name) {
            return;
        }
        if let Some(target) = before {
            if !current.iter().any(|c| c == target) || target == name {
                return;
            }
        }
        let pos = current.iter().position(|c| c == name).unwrap_or(0);
        let mut next = current;
        let moved = next.remove(pos);
        let insert_at = match before {
            Some(target) => next.iter().position(|c| c == target).unwrap_or(next.len()),
            None => next.len(),
        };
        next.insert(insert_at, moved);
        self.order = next;
    }

    /// Store a user width (screen points at 100% zoom, min 32).
    pub fn set_width(&mut self, name: &str, width: f32) {
        self.widths.insert(name.to_string(), width.max(32.0));
    }

    /// User width of a column, if resized.
    pub fn width(&self, name: &str) -> Option<f32> {
        self.widths.get(name).copied()
    }

    /// Number of frozen leading columns; never the whole visible set.
    pub fn frozen(&self) -> usize {
        self.frozen
    }

    /// Freeze the first `count` visible columns (0 = none).
    pub fn freeze(&mut self, count: usize, all: &[&str]) {
        let vis = self.visible(all).len();
        self.frozen = count.min(vis.saturating_sub(1));
    }

    /// Restore the defaults: order, widths, hidden and freeze state.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Serialize for the settings store.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "order": self.order,
            "hidden": self.hidden.iter().collect::<Vec<_>>(),
            "widths": self.widths,
            "frozen": self.frozen,
        })
    }

    /// Read back a stored layout; a wrong shape yields the default layout.
    pub fn from_json(v: &serde_json::Value) -> Self {
        let mut out = Self::default();
        let Some(obj) = v.as_object() else {
            return out;
        };
        if let Some(order) = obj.get("order").and_then(|x| x.as_array()) {
            out.order = order
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect();
        }
        if let Some(hidden) = obj.get("hidden").and_then(|x| x.as_array()) {
            out.hidden = hidden
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect();
        }
        if let Some(widths) = obj.get("widths").and_then(|x| x.as_object()) {
            out.widths = widths
                .iter()
                .filter_map(|(k, v)| v.as_f64().map(|w| (k.clone(), w as f32)))
                .collect();
        }
        out.frozen = obj
            .get("frozen")
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
            .clamp(0, 64) as usize;
        out
    }
}

/// Frame geometry of the grid: pixel positions of the visible rows and
/// columns for one paint pass. Everything derives from the layout, the zoom
/// and the viewport size — nothing persists between frames.
#[derive(Debug, Clone)]
pub struct FrameGeom {
    /// Row height in screen points this frame.
    pub row_h: f32,
    /// Header height in screen points.
    pub header_h: f32,
    /// Screen X where the scrollable columns start (after the frozen strip).
    pub body_x: f32,
    /// Screen width of the scrollable column strip.
    pub body_w: f32,
    /// Screen width of the frozen strip.
    pub frozen_w: f32,
    /// First visible scrollable column index (into the visible list).
    pub first_col: usize,
    /// Horizontal scroll offset in screen points, clipped to the content.
    pub scroll_x: f32,
    /// Total content width of the scrollable strip in screen points.
    pub content_w: f32,
    /// Total content height in screen points (`rows × row_h`).
    pub content_h: f32,
}

impl FrameGeom {
    /// Compute one frame's geometry.
    ///
    /// `widths` are screen widths of the *visible* columns (already zoomed);
    /// `viewport` is the cell-area size; `scroll_x` is in screen points.
    pub fn new(
        widths: &[f32],
        frozen: usize,
        scroll_x: f32,
        rows: i64,
        row_h: f32,
        header_h: f32,
        viewport_w: f32,
    ) -> Self {
        let frozen_w: f32 = widths.iter().take(frozen).sum();
        let content_w: f32 = widths.iter().skip(frozen).sum();
        let body_x = frozen_w;
        let body_w = (viewport_w - frozen_w).max(0.0);
        let scroll_x = scroll_x.clamp(0.0, (content_w - body_w).max(0.0));
        // First visible scrollable column: skip columns fully scrolled past.
        let mut first_col = frozen;
        let mut acc = 0.0;
        for (i, w) in widths.iter().enumerate().skip(frozen) {
            if acc + *w <= scroll_x {
                acc += *w;
                first_col = i + 1;
            } else {
                break;
            }
        }
        Self {
            row_h,
            header_h,
            body_x,
            body_w,
            frozen_w,
            first_col,
            scroll_x,
            content_w,
            content_h: (rows.max(0) as f32) * row_h,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Ladder facts the specification fixes as numbers.

    #[test]
    fn zoom_ladder_is_the_fixed_22_steps() {
        assert_eq!(Zoom::STEPS.len(), 22);
        assert_eq!(Zoom::STEPS[0], 10.0);
        assert_eq!(Zoom::STEPS[13], 100.0);
        assert_eq!(Zoom::STEPS[21], 400.0);
    }

    #[test]
    fn zoom_snaps_off_ladder_values() {
        assert_eq!(Zoom::new(121.0).percent(), 120.0);
        assert_eq!(Zoom::new(0.0).percent(), 10.0);
        assert_eq!(Zoom::new(9999.0).percent(), 400.0);
    }

    #[test]
    fn wheel_accumulates_fractions_and_fires_whole_steps() {
        let mut z = Zoom::new(100.0);
        // Half clicks: nothing fires below the 0.6 threshold.
        z.wheel(0.3);
        z.wheel(0.2);
        assert_eq!(z.percent(), 100.0);
        // Crossing the threshold fires exactly one step.
        z.wheel(0.2);
        assert_eq!(z.percent(), 120.0);
        // The remainder was consumed; a small reverse turn does not undo it.
        z.wheel(-0.2);
        assert_eq!(z.percent(), 120.0);
    }

    #[test]
    fn wheel_steps_are_geometric_and_never_skip_100() {
        let mut z = Zoom::new(120.0);
        z.wheel(-1.0);
        assert_eq!(z.percent(), 100.0, "the way down stops at 100");
        let mut z = Zoom::new(100.0);
        z.wheel(-1.0);
        assert_eq!(z.percent(), 85.0);
        let mut z = Zoom::new(85.0);
        z.wheel(1.0);
        assert_eq!(z.percent(), 100.0);
    }

    #[test]
    fn near_steps_are_skipped_but_ladder_ends_stop() {
        // 85 → 100: gap 17.6% — fine; from 400 nothing further exists.
        let mut z = Zoom::new(400.0);
        assert!(!z.step(1));
        let mut z = Zoom::new(10.0);
        assert!(!z.step(-1));
        // Direct set + step: off-ladder current resumes at the next step.
        let mut z = Zoom::new(100.0);
        z.set_percent(119.0);
        assert!(z.step(1));
        assert_eq!(z.percent(), 140.0, "120 is within 5% of 119, skipped");
    }

    #[test]
    fn zoom_set_resets_the_accumulator() {
        let mut z = Zoom::new(100.0);
        z.wheel(0.5);
        z.set_percent(200.0);
        z.wheel(-0.2);
        assert_eq!(z.percent(), 200.0, "stale fractions must not fire");
    }

    const ALL: [&str; 4] = ["url", "status_code", "title", "depth"];

    #[test]
    fn layout_visible_order_and_hidden() {
        let mut l = ColLayout::default();
        assert_eq!(
            l.visible(&ALL),
            ALL.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
        l.hide("status_code", &ALL);
        assert!(!l.visible(&ALL).contains(&"status_code".to_string()));
        l.show("status_code");
        assert!(l.visible(&ALL).contains(&"status_code".to_string()));
    }

    #[test]
    fn layout_never_hides_the_last_visible_column() {
        let mut l = ColLayout::default();
        for name in ALL {
            l.hide(name, &ALL);
        }
        assert_eq!(l.visible(&ALL).len(), 1);
    }

    #[test]
    fn layout_move_before_and_to_end() {
        let mut l = ColLayout::default();
        l.move_before("depth", Some("url"), &ALL);
        assert_eq!(l.visible(&ALL)[0], "depth");
        l.move_before("depth", None, &ALL);
        let vis = l.visible(&ALL);
        assert_eq!(vis.last().unwrap(), "depth");
        // Unknown target and self-target are no-ops.
        l.move_before("url", Some("depth"), &ALL);
        assert_eq!(l.visible(&ALL), ["status_code", "title", "url", "depth"]);
        l.move_before("url", Some("url"), &ALL);
        assert_eq!(l.visible(&ALL), ["status_code", "title", "url", "depth"]);
    }

    #[test]
    fn layout_freeze_clips_to_visible_set() {
        let mut l = ColLayout::default();
        l.freeze(99, &ALL);
        assert_eq!(l.frozen(), ALL.len() - 1);
        l.hide("url", &ALL);
        l.freeze(99, &ALL);
        assert!(l.frozen() < l.visible(&ALL).len());
    }

    #[test]
    fn layout_json_round_trip_and_junk() {
        let mut l = ColLayout::default();
        l.move_before("title", Some("status_code"), &ALL);
        l.hide("depth", &ALL);
        l.set_width("url", 380.0);
        l.freeze(1, &ALL);
        let back = ColLayout::from_json(&l.to_json());
        assert_eq!(back, l);
        // Junk shapes fall back to defaults, never panic.
        assert_eq!(ColLayout::from_json(&json!("x")), ColLayout::default());
        assert_eq!(
            ColLayout::from_json(&json!({"widths": {"url": "NaN"}})).width("url"),
            None
        );
    }

    #[test]
    fn frame_geom_clips_scroll_and_finds_first_column() {
        // Columns: frozen 60 + 100 + 200 + 200; viewport 300.
        let g = FrameGeom::new(&[60.0, 100.0, 200.0, 200.0], 1, 0.0, 10, 30.0, 34.0, 300.0);
        assert_eq!(g.frozen_w, 60.0);
        assert_eq!(g.content_w, 500.0);
        assert_eq!(g.scroll_x, 0.0);
        assert_eq!(g.first_col, 1);
        // Scrolling deep clamps at content − body (260 here).
        let g = FrameGeom::new(
            &[60.0, 100.0, 200.0, 200.0],
            1,
            500.0,
            10,
            30.0,
            34.0,
            300.0,
        );
        assert_eq!(g.scroll_x, 260.0);
        assert_eq!(g.first_col, 2, "the 100-px column scrolled out fully");
        // Frozen strip wider than the viewport: nothing scrollable left.
        let g = FrameGeom::new(&[200.0, 200.0], 1, 0.0, 0, 30.0, 34.0, 120.0);
        assert_eq!(g.body_w, 0.0);
    }

    #[test]
    fn frame_geom_content_height_counts_rows() {
        let g = FrameGeom::new(&[100.0], 0, 0.0, 7, 30.0, 34.0, 100.0);
        assert_eq!(g.content_h, 210.0);
        let g = FrameGeom::new(&[100.0], 0, 0.0, -3, 30.0, 34.0, 100.0);
        assert_eq!(g.content_h, 0.0);
    }
}
