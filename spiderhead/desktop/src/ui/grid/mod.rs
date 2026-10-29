//! The big virtualized table view: frozen leading columns, one scrollable
//! strip, zoom on the ladder, row selection by stable keys, column resize /
//! reorder, context-menu hooks, keyboard navigation and copy support.
//! Drawing only ever touches the visible rows: the source fetches rows
//! lazily by index, so a million-row table scrolls as smoothly as a
//! hundred-row one.
//!
//! The widget owns no data. It mutates the [`GridUi`] state it is given and
//! reports the actions it cannot resolve itself — sorting, opening details,
//! copying, menus — as [`Event`]s.

mod geom;
mod state;

pub use geom::{ColLayout, FrameGeom, Zoom};
pub use state::{Drag, GridUi, Popover, Selection, Transients};

use eframe::egui::{self, Align, Align2, FontId, Layout, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use serde_json::Value;

use crate::theme::{radius, space, ty, Palette};

/// Width of the vertical scrollbar strip, in points.
const BAR_W: f32 = 14.0;
/// Base row height at 100% zoom and «compact» density (the spec's 22 px
/// dense row; the comfortable density scales it to 26).
const ROW_BASE: f32 = 22.0;
/// Minimum thumb length of the scrollbar.
const THUMB_MIN: f32 = 24.0;
/// Grab distance around a column border that starts a resize.
const RESIZE_GRAB: f32 = 4.0;
/// Width of the header filter-button hit zone.
const FILTER_BTN_W: f32 = 18.0;
/// Pointer distance from an edge that triggers drag-selection autoscroll.
const AUTOSCROLL_EDGE: f32 = 24.0;
/// Autoscroll speed at an edge, in points per frame.
const AUTOSCROLL_SPEED: f32 = 24.0;
/// Wheel delta of one mouse click (egui point units), for the zoom ladder.
const WHEEL_CLICK: f32 = 40.0;
/// Resize toast: fixed width for the "N px" caption.
const TOAST_W: f32 = 84.0;
/// Resize floor, in base points.
const COL_MIN_W: f32 = 32.0;

/// Base row height at 100% zoom (the control channel computes scroll
/// offsets with it).
pub const fn row_base() -> f32 {
    ROW_BASE
}

/// One visible column of the grid.
#[derive(Debug, Clone)]
pub struct GridColumn {
    /// Stable column id (the data key).
    pub id: String,
    /// Header caption, already localized.
    pub header: String,
    /// Base width at 100% zoom; `None` = the column stretches to the rest.
    pub width: Option<f32>,
    /// Floor of a stretching column at 100% zoom: it never shrinks below its
    /// content while fixed columns could scroll instead (0 = no floor).
    pub min_width: f32,
    /// Numbers align right.
    pub numeric: bool,
}

/// Actions the widget resolved but cannot perform: the caller turns them into
/// data queries or shell effects.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The header was clicked: sort by the column, or reset (`None`).
    Sort {
        /// Column to sort by.
        column: String,
        /// `Some(true)` ascending, `Some(false)` descending, `None` reset.
        dir: Option<bool>,
    },
    /// The header's filter button was clicked.
    FilterMenu {
        /// The column whose filter was requested.
        column: String,
        /// Screen position for the menu.
        pos: Pos2,
    },
    /// Rows were opened (Enter): show their details.
    Activate,
    /// Copy the selection; the caller builds the TSV from live data.
    Copy,
    /// Right-click on a row; the caller opens its context menu.
    ContextMenu {
        /// The clicked row's key.
        key: String,
        /// Screen position for the menu.
        pos: Pos2,
    },
    /// Ctrl+F while the grid holds focus.
    FocusFind,
    /// The visible column order changed (a drag finished).
    ColumnsChanged,
    /// Double-click on a cell: show its full text in a popover.
    Popover {
        /// The row's key.
        key: String,
        /// The column's id.
        column: String,
        /// Screen position of the cell (the popover anchors near it).
        pos: Pos2,
    },
}

/// What one frame produced: the events plus the grid surface response.
pub struct GridOut {
    /// Actions to perform.
    pub events: Vec<Event>,
    /// The grid surface response (focus handling lives inside `show`).
    pub response: egui::Response,
}

/// Row access the widget renders from. `rows` is the filtered row count; a
/// `row` of `None` means "not fetched yet" and paints as a placeholder.
pub trait RowSource {
    /// Number of rows after the active filter.
    fn rows(&mut self) -> i64;
    /// One row by its filtered index.
    fn row(&mut self, index: i64) -> Option<Value>;
}

/// One cell's draw closure.
pub type CellFn<'a> = dyn FnMut(&mut Ui, &Palette, &GridColumn, Option<&Value>) + 'a;

/// Per-call arguments that are not part of the grid's own state.
pub struct GridArgs<'a, S: RowSource> {
    /// Stable id of this grid instance (the focus key).
    pub id: &'a str,
    /// Visible columns in display order.
    pub columns: &'a [GridColumn],
    /// Current sort: the sorted column id (if any) and its direction.
    pub sort: (Option<String>, bool),
    /// Row-density scale from the view settings (1.0 = «compact», the
    /// default; 1.15 «standard», 1.3 «comfortable»).
    pub density: f32,
    /// Row source (the filtered, sorted table).
    pub source: &'a mut S,
    /// Draws one cell into a prepared cell `Ui`.
    pub cell: &'a mut CellFn<'a>,
}

/// The whole grid widget: header strip, frozen and scrollable quadrants,
/// scrollbar, selection paint and input handling.
pub fn show<S: RowSource>(
    ui: &mut Ui,
    p: &Palette,
    g: &mut GridUi,
    args: GridArgs<'_, S>,
) -> GridOut {
    let GridArgs {
        id,
        columns,
        sort,
        density,
        source,
        cell,
    } = args;

    let rows = source.rows();
    let z = g.zoom.scale();
    let row_h = ROW_BASE * density * z;
    let header_h = 30.0 * density.max(1.0) * z.max(0.75);
    let full = Rect::from_min_size(ui.cursor().min, ui.available_size());
    let canvas_w = (full.width() - BAR_W).max(0.0);
    let view_h = (full.height() - header_h).max(0.0);
    let content_h = rows.max(0) as f32 * row_h;

    // Screen widths of the visible columns; the one stretching column eats
    // the leftover space when it fits, else the strip scrolls horizontally.
    let vis: Vec<&GridColumn> = columns
        .iter()
        .filter(|c| !g.layout.is_hidden(&c.id))
        .collect();
    let mut widths: Vec<f32> = vis
        .iter()
        .map(|c| c.width.or_else(|| g.layout.width(&c.id)).unwrap_or(140.0) * z)
        .collect();
    let frozen = g.layout.frozen().min(vis.len().saturating_sub(1));
    let stretched = vis.iter().position(|c| c.width.is_none());
    if let Some(s) = stretched {
        let fixed: f32 = widths
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != s)
            .map(|(_, w)| *w)
            .sum();
        let min = widths[s].max(vis[s].min_width * z);
        widths[s] = if canvas_w > fixed + min {
            canvas_w - fixed
        } else {
            min
        };
    }

    // The scroll position is the top row number in f64 (the fraction is the
    // pixel offset of the partially visible row). Pixels drift past 16.7M at
    // f32 — a million dense rows are 22M px — so the position stays counted
    // in rows and only the paint math converts to pixels. No own inertia:
    // the OS already smooths touchpad deltas.
    let rows_view = (view_h / row_h) as f64;
    let max_scroll_row = (rows.max(0) as f64 - rows_view).max(0.0);
    g.scroll_target_y = g.scroll_target_y.clamp(0.0, max_scroll_row);
    g.scroll_y = g.scroll_target_y;
    let scroll_px = (g.scroll_y * row_h as f64) as f32;
    let geom = FrameGeom::new(
        &widths,
        frozen,
        g.scroll_x * z,
        rows,
        row_h,
        header_h,
        canvas_w,
    );

    let response = ui.allocate_rect(full, Sense::click_and_drag());
    if response.clicked() {
        response.request_focus();
    }
    let painter = ui.painter().clone();
    let origin = full.min;
    let mut events = Vec::new();

    // Visible row range; the fraction shifts the body up so the partial top
    // row is drawn under the header.
    let first_row = g.scroll_y.floor().max(0.0) as i64;
    let frac_px = ((g.scroll_y - g.scroll_y.floor()) * row_h as f64) as f32;
    let body_top = origin.y + header_h - frac_px;
    let last_row = (first_row + (view_h / row_h).ceil() as i64 + 2).min(rows);

    // Row underlays: hover first, then selection (the active row stronger).
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let hover_row = pointer
        .filter(|m| full.contains(*m))
        .and_then(|m| row_at(m.y, body_top, row_h, first_row, last_row));
    if let Some(r) = hover_row {
        painter.rect_filled(
            row_rect(origin, canvas_w, body_top, row_h, r, first_row),
            0.0,
            p.hover,
        );
    }
    let mut row_keys: Vec<String> = Vec::with_capacity((last_row - first_row).max(0) as usize);
    for r in first_row..last_row {
        let rect = row_rect(origin, canvas_w, body_top, row_h, r, first_row);
        let key = source.row(r).and_then(|v| row_key(&v));
        if let Some(key) = &key {
            if g.selection.contains(key) {
                let active = g.selection.active.as_deref() == Some(key.as_str());
                let fill = if active {
                    p.accent_soft
                } else {
                    p.accent_soft.gamma_multiply(0.45)
                };
                painter.rect_filled(rect, 0.0, fill);
                painter.line_segment(
                    [rect.min, Pos2::new(rect.min.x, rect.max.y)],
                    Stroke::new(2.0_f32, p.accent),
                );
            }
        }
        row_keys.push(key.unwrap_or_default());
    }

    // Cells: frozen columns keep their place, the strip shifts by the scroll.
    for r in first_row..last_row {
        let value = source.row(r);
        let top_y = body_top + (r - first_row) as f32 * row_h;
        let grid_clip = Rect::from_min_size(origin, Vec2::new(canvas_w, header_h + view_h));
        draw_row_cells(
            ui,
            p,
            &vis,
            &widths,
            &geom,
            frozen,
            origin,
            top_y,
            row_h,
            grid_clip,
            value.as_ref(),
            cell,
        );
    }

    // Grid lines over the cells, crisp one-pixel.
    for r in first_row..=last_row {
        let y = body_top + (r - first_row) as f32 * row_h;
        if y <= full.max.y + 0.5 {
            painter.line_segment(
                [Pos2::new(origin.x, y), Pos2::new(origin.x + canvas_w, y)],
                Stroke::new(0.5_f32, p.border_soft),
            );
        }
    }
    let mut x = origin.x;
    for (i, w) in widths.iter().enumerate() {
        if i >= frozen {
            x -= geom.scroll_x;
        }
        x += *w;
        let px = x.round() - 0.5;
        if px >= origin.x && px <= origin.x + canvas_w {
            painter.line_segment(
                [
                    Pos2::new(px, origin.y),
                    Pos2::new(px, origin.y + full.height()),
                ],
                Stroke::new(0.5_f32, p.border),
            );
        }
    }
    if frozen > 0 {
        let bx = origin.x + widths.iter().take(frozen).sum::<f32>();
        painter.line_segment(
            [
                Pos2::new(bx, origin.y),
                Pos2::new(bx, origin.y + full.height()),
            ],
            Stroke::new(1.0_f32, p.outline),
        );
    }

    // Header strip over the cell area.
    let header_rect = Rect::from_min_size(origin, Vec2::new(canvas_w, header_h));
    painter.rect_filled(header_rect, 0.0, p.surface_hi);
    draw_header(
        ui, p, &vis, &widths, &geom, frozen, origin, header_h, z, &sort,
    );

    // Vertical scrollbar on the right.
    let track = Rect::from_min_size(
        Pos2::new(origin.x + canvas_w, origin.y),
        Vec2::new(BAR_W, full.height()),
    );
    draw_vbar(
        &painter,
        p,
        track,
        scroll_px,
        content_h,
        full.height(),
        pointer,
    );

    // ---- input ------------------------------------------------------------
    handle_wheel(
        ui,
        pointer,
        full,
        max_scroll_row,
        z,
        &mut g.scroll_target_y,
        &mut g.scroll_x,
    );
    handle_pointer(
        ui,
        p,
        pointer,
        &response,
        full,
        canvas_w,
        track,
        header_h,
        row_h,
        first_row,
        last_row,
        content_h,
        max_scroll_row,
        &widths,
        &vis,
        frozen,
        origin,
        scroll_px,
        z,
        &sort,
        &mut g.scroll_y,
        &mut g.scroll_target_y,
        &mut g.scroll_x,
        &mut g.layout,
        &mut g.selection,
        &mut g.transients,
        &mut g.drag,
        &mut g.active_index,
        &row_keys,
        source,
        &mut events,
    );
    handle_keys(
        ui,
        id,
        &response,
        row_h,
        view_h,
        rows,
        first_row,
        last_row,
        max_scroll_row,
        &mut g.scroll_y,
        &mut g.scroll_target_y,
        &mut g.selection,
        &mut g.transients,
        &mut g.active_index,
        &row_keys,
        &mut events,
    );

    // ---- transients -------------------------------------------------------
    draw_transients(
        ui,
        p,
        &g.transients,
        pointer,
        origin,
        full,
        header_h,
        row_h,
        first_row,
        last_row,
        &g.selection,
        source,
    );

    GridOut { events, response }
}

/// Y → row index, when the point is on a body row.
fn row_at(y: f32, body_top: f32, row_h: f32, first: i64, last: i64) -> Option<i64> {
    if y < body_top {
        return None;
    }
    let r = first + ((y - body_top) / row_h).floor() as i64;
    (r < last).then_some(r)
}

/// Screen rect of one visible row; `body_top` is the absolute screen y of
/// row `first` (the header bottom minus the partial-row scroll offset).
fn row_rect(origin: Pos2, width: f32, body_top: f32, row_h: f32, r: i64, first: i64) -> Rect {
    Rect::from_min_size(
        Pos2::new(origin.x, body_top + (r - first) as f32 * row_h),
        Vec2::new(width, row_h),
    )
}

/// Screen X where visible column `i` starts, relative to the grid origin.
fn col_left(widths: &[f32], i: usize, frozen: usize, scroll_x: f32) -> f32 {
    let mut acc = 0.0;
    for (j, w) in widths.iter().enumerate() {
        if j == i {
            return acc - if j >= frozen { scroll_x } else { 0.0 };
        }
        acc += *w;
    }
    acc
}

/// Visible-column index at grid-local `x`; the last column wins past the end.
/// For the header pass `scroll_x = 0` (the header only scrolls virtually).
fn col_at(widths: &[f32], frozen: usize, scroll_x: f32, x: f32) -> usize {
    let last = widths.len().saturating_sub(1);
    for (i, w) in widths.iter().enumerate() {
        let left = col_left(widths, i, frozen, scroll_x);
        if x >= left && x < left + *w {
            return i;
        }
    }
    last
}

/// Where the pointer is, in grid terms.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Hit {
    /// Visible-column index under the pointer.
    col: usize,
    /// On a column border of the header strip (resize zone).
    border: bool,
    /// On the header strip.
    header: bool,
    /// On the header's filter button.
    filter_btn: bool,
    /// On the vertical scrollbar.
    vbar: bool,
}

fn hit_test(
    m: Pos2,
    origin: Pos2,
    canvas_w: f32,
    header_h: f32,
    widths: &[f32],
    frozen: usize,
    scroll_x: f32,
) -> Hit {
    if m.x >= origin.x + canvas_w {
        return Hit {
            col: 0,
            border: false,
            header: false,
            filter_btn: false,
            vbar: true,
        };
    }
    let header = m.y < origin.y + header_h;
    let sx = if header { 0.0 } else { scroll_x };
    let lx = m.x - origin.x;
    let col = col_at(widths, frozen, sx, lx);
    let border = header
        && (0..widths.len())
            .any(|i| (lx - col_left(widths, i, frozen, sx) - widths[i]).abs() <= RESIZE_GRAB);
    let left = col_left(widths, col, frozen, sx);
    let w = widths.get(col).copied().unwrap_or(0.0);
    let btn_left = left + w - FILTER_BTN_W - 4.0;
    let filter_btn = header && !border && lx >= btn_left && lx <= left + w - 2.0;
    Hit {
        col,
        border,
        header,
        filter_btn,
        vbar: false,
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_row_cells(
    ui: &mut Ui,
    p: &Palette,
    vis: &[&GridColumn],
    widths: &[f32],
    geom: &FrameGeom,
    frozen: usize,
    origin: Pos2,
    top_y: f32,
    row_h: f32,
    grid_clip: Rect,
    value: Option<&Value>,
    cell: &mut CellFn<'_>,
) {
    let strip_left = origin.x + widths.iter().take(frozen).sum::<f32>();
    let right_edge = origin.x + widths.iter().sum::<f32>() - geom.scroll_x;
    let mut x = origin.x;
    for (i, col) in vis.iter().enumerate() {
        let w = widths[i];
        let left = if i < frozen { x } else { x - geom.scroll_x };
        x += w;
        if left + w <= origin.x || left >= right_edge {
            continue;
        }
        let clip_right = if i < frozen {
            strip_left.max(left)
        } else {
            right_edge.max(left)
        };
        let clip = Rect::from_min_max(
            Pos2::new(left.max(origin.x), top_y),
            Pos2::new(clip_right, top_y + row_h),
        )
        .intersect(grid_clip);
        let rect = Rect::from_min_size(Pos2::new(left, top_y), Vec2::new(w, row_h));
        let value_cell = value.and_then(|v| v.get(col.id.as_str()));
        let mut cell_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(if col.numeric {
                    Layout::right_to_left(Align::Center)
                } else {
                    Layout::left_to_right(Align::Center)
                })
                .sense(Sense::hover()),
        );
        cell_ui.set_clip_rect(clip);
        cell_ui.add_space(space::SM);
        cell(&mut cell_ui, p, col, value_cell);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_header(
    ui: &mut Ui,
    p: &Palette,
    vis: &[&GridColumn],
    widths: &[f32],
    geom: &FrameGeom,
    frozen: usize,
    origin: Pos2,
    header_h: f32,
    z: f32,
    sort: &(Option<String>, bool),
) {
    let mut x = origin.x;
    for (i, col) in vis.iter().enumerate() {
        let w = widths[i];
        let left = if i < frozen { x } else { x - geom.scroll_x };
        x += w;
        if left + w <= origin.x || left >= origin.x + geom.body_x + geom.body_w {
            continue;
        }
        let rect = Rect::from_min_size(Pos2::new(left, origin.y), Vec2::new(w, header_h));
        let mut hui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        hui.set_clip_rect(rect);
        hui.add_space(space::SM);
        let sorted = sort.0.as_deref() == Some(col.id.as_str());
        let ink = if sorted { p.text } else { p.weak };
        hui.label(
            egui::RichText::new(&col.header)
                .size(ty::CAPTION * z.max(0.85))
                .color(ink),
        );
        if sorted {
            let arrow = if sort.1 {
                crate::ui::kit::icons::ARROW_UPWARD
            } else {
                crate::ui::kit::icons::ARROW_DOWNWARD
            };
            hui.label(
                egui::RichText::new(arrow)
                    .size(ty::OVERLINE)
                    .color(p.accent),
            );
        }
        // Filter-button slot on the right edge of the header cell.
        let btn = Rect::from_min_size(
            Pos2::new(rect.right() - FILTER_BTN_W - 4.0, rect.center().y - 8.0),
            Vec2::new(FILTER_BTN_W, 16.0),
        );
        ui.painter().text(
            btn.center(),
            Align2::CENTER_CENTER,
            crate::ui::kit::icons::EXPAND_MORE,
            FontId::proportional(10.0),
            p.weak,
        );
    }
}

fn thumb_h(track: Rect, view_h: f32, content_h: f32) -> f32 {
    let span = track.height() - 6.0;
    (span * view_h / (content_h + view_h)).clamp(THUMB_MIN, span)
}

fn draw_vbar(
    painter: &egui::Painter,
    p: &Palette,
    track: Rect,
    scroll_px: f32,
    content_h: f32,
    view_h: f32,
    pointer: Option<Pos2>,
) {
    painter.rect_filled(track, 0.0, p.nav);
    if content_h <= view_h {
        return;
    }
    let span = track.height() - 6.0;
    let th = thumb_h(track, view_h, content_h);
    let max_scroll = content_h - view_h;
    let t = (scroll_px / max_scroll).clamp(0.0, 1.0);
    let thumb_y = track.min.y + 3.0 + t * (span - th);
    let thumb = Rect::from_min_size(
        Pos2::new(track.min.x + 3.0, thumb_y),
        Vec2::new(BAR_W - 6.0, th),
    );
    let active = pointer.is_some_and(|m| thumb.contains(m) || track.contains(m));
    painter.rect_filled(thumb, 4.0, if active { p.weak } else { p.disabled });
}

#[allow(clippy::too_many_arguments)]
fn handle_wheel(
    ui: &Ui,
    pointer: Option<Pos2>,
    full: Rect,
    max_scroll_row: f64,
    z: f32,
    scroll_target_y: &mut f64,
    scroll_x: &mut f32,
) {
    let Some(m) = pointer else { return };
    if !full.contains(m) {
        return;
    }
    let mut dy = 0.0_f64;
    let mut dx = 0.0;
    ui.input(|i| {
        for e in &i.events {
            if let egui::Event::MouseWheel {
                delta,
                unit,
                modifiers,
            } = e
            {
                let (ux, uy) = match unit {
                    egui::MouseWheelUnit::Point => (delta.x, delta.y),
                    _ => (delta.x * 20.0, delta.y * 20.0),
                };
                if modifiers.command || modifiers.ctrl {
                    // Ctrl/⌘+wheel does not scale a data table; the view
                    // zoom lives on ⌘+/⌘− and the status strip.
                    continue;
                }
                if modifiers.shift {
                    dx += uy;
                    continue;
                }
                // A discrete mouse click steps three rows; touchpad deltas
                // arrive pre-smoothed and go through as they come.
                if *unit == egui::MouseWheelUnit::Point && uy.abs() >= WHEEL_CLICK {
                    dy += uy.signum() as f64 * 3.0;
                } else {
                    dy += (uy / WHEEL_CLICK * 3.0) as f64;
                }
                dx += ux;
            }
        }
    });
    let new_target = (*scroll_target_y + dy).clamp(0.0, max_scroll_row);
    let moved = (new_target - *scroll_target_y).abs() > f64::EPSILON || dx.abs() > f32::EPSILON;
    *scroll_target_y = new_target;
    *scroll_x = (*scroll_x + dx / z).clamp(0.0, 1_000_000.0);
    if moved {
        ui.ctx().request_repaint();
    }
}

/// Column index whose left edge is nearest the pointer during a move drag
/// (the insertion point); `None` when the drop is a no-op (same place, or
/// across the frozen/scrollable boundary).
fn move_target(
    widths: &[f32],
    frozen: usize,
    scroll_x: f32,
    col: usize,
    local_x: f32,
) -> Option<usize> {
    // Nearest left edge among visible columns.
    let mut best = 0;
    let mut best_d = f32::MAX;
    for i in 0..widths.len() {
        let d = (local_x - col_left(widths, i, frozen, scroll_x)).abs();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    // Inserting right before the next column is the same place.
    if best == col || best == col + 1 {
        return None;
    }
    if (col < frozen) != (best < frozen) {
        return None;
    }
    Some(best)
}

#[allow(clippy::too_many_arguments)]
fn handle_pointer<S: RowSource>(
    ui: &Ui,
    p: &Palette,
    pointer: Option<Pos2>,
    response: &egui::Response,
    full: Rect,
    canvas_w: f32,
    track: Rect,
    header_h: f32,
    row_h: f32,
    first_row: i64,
    last_row: i64,
    content_h: f32,
    max_scroll_row: f64,
    widths: &[f32],
    vis: &[&GridColumn],
    frozen: usize,
    origin: Pos2,
    scroll_px: f32,
    z: f32,
    sort: &(Option<String>, bool),
    scroll_y: &mut f64,
    scroll_target_y: &mut f64,
    scroll_x: &mut f32,
    layout: &mut ColLayout,
    selection: &mut Selection,
    transients: &mut Transients,
    drag: &mut Option<Drag>,
    active_index: &mut Option<i64>,
    row_keys: &[String],
    source: &mut S,
    events: &mut Vec<Event>,
) {
    let mods = ui.input(|i| i.modifiers);
    let scroll_x_px = *scroll_x * z;

    // Live drag first: it owns the pointer while held.
    if let Some(d) = *drag {
        let down = ui.input(|i| i.pointer.primary_down());
        if !down {
            if let Drag::Move { col } = d {
                if let Some(target) = pointer
                    .and_then(|m| move_target(widths, frozen, scroll_x_px, col, m.x - origin.x))
                {
                    let name = vis[col].id.clone();
                    let before = vis.get(target).map(|c| c.id.clone());
                    layout.move_before(&name, before.as_deref(), &[]);
                    events.push(Event::ColumnsChanged);
                }
            }
            *drag = None;
            return;
        }
        let Some(m) = pointer else { return };
        match d {
            Drag::Resize {
                col,
                start_x,
                start_w,
            } => {
                let name = vis[col].id.clone();
                let w = (start_w + (m.x - start_x) / z).max(COL_MIN_W);
                layout.set_width(&name, w);
                transients.toast(format!("{} px", w.round() as i64));
                ui.ctx().request_repaint();
            }
            Drag::Move { col } => {
                if let Some(target) = move_target(widths, frozen, scroll_x_px, col, m.x - origin.x)
                {
                    let px = origin.x + col_left(widths, target, frozen, scroll_x_px);
                    ui.painter().line_segment(
                        [Pos2::new(px, full.min.y), Pos2::new(px, full.max.y)],
                        Stroke::new(3.0_f32, p.accent),
                    );
                }
                ui.ctx().request_repaint();
            }
            Drag::VBar { grab } => {
                let span = (track.height() - 6.0).max(1.0);
                let th = thumb_h(track, full.height(), content_h);
                let t = ((m.y - track.min.y - 3.0 - grab) / (span - th).max(1.0)).clamp(0.0, 1.0);
                *scroll_y = t as f64 * max_scroll_row;
                *scroll_target_y = *scroll_y;
                ui.ctx().request_repaint();
            }
            Drag::Select { anchor } => {
                // Extend the range to the row under the pointer.
                let last_visible = (m.y - origin.y - header_h) / row_h;
                let r = if last_visible < 0.0 {
                    first_row
                } else {
                    (first_row + last_visible as i64).clamp(first_row, last_row - 1)
                };
                let (lo, hi) = (anchor.min(r), anchor.max(r));
                let keys: Vec<String> = (lo..=hi)
                    .filter_map(|i| source.row(i).and_then(|v| row_key(&v)))
                    .collect();
                selection.set_all(keys);
                if let Some(k) = source.row(r).and_then(|v| row_key(&v)) {
                    selection.active = Some(k.clone());
                    selection.anchor = Some(k);
                }
                *active_index = Some(r);
                // Autoscroll at the edges keeps long drags going.
                let mut d_y = 0.0;
                if m.y > full.max.y - AUTOSCROLL_EDGE {
                    d_y = AUTOSCROLL_SPEED;
                } else if m.y < origin.y + header_h + AUTOSCROLL_EDGE {
                    d_y = -AUTOSCROLL_SPEED;
                }
                if d_y != 0.0 {
                    *scroll_target_y = (*scroll_target_y + (d_y / AUTOSCROLL_SPEED) as f64)
                        .clamp(0.0, max_scroll_row);
                    *scroll_y = *scroll_target_y;
                }
                ui.ctx().request_repaint();
            }
        }
        return;
    }

    let Some(m) = pointer else { return };
    let hit = hit_test(m, origin, canvas_w, header_h, widths, frozen, scroll_x_px);

    // Cursor shape over the grid.
    if hit.header && hit.border {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    } else if hit.header && !hit.vbar {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // Scrollbar: press on the thumb drags, a click on the track pages.
    if hit.vbar && content_h > full.height() {
        let span = (track.height() - 6.0).max(1.0);
        let th = thumb_h(track, full.height(), content_h);
        let max_scroll = content_h - full.height();
        let t = (scroll_px / max_scroll).clamp(0.0, 1.0);
        let thumb_y = track.min.y + 3.0 + t * (span - th);
        if response.drag_started() {
            let grab = (m.y - thumb_y).clamp(0.0, th);
            *drag = Some(Drag::VBar { grab });
        } else if response.clicked() {
            let dir = if m.y < thumb_y { -1.0 } else { 1.0 };
            let next =
                (*scroll_y + dir * full.height() as f64 / row_h as f64).clamp(0.0, max_scroll_row);
            *scroll_y = next;
            *scroll_target_y = next;
        }
        return;
    }

    // Header: resize, reorder, sort, filter button.
    if hit.header {
        if response.drag_started() && hit.border {
            let base = vis[hit.col]
                .width
                .or_else(|| layout.width(&vis[hit.col].id))
                .unwrap_or(140.0);
            *drag = Some(Drag::Resize {
                col: hit.col,
                start_x: m.x,
                start_w: base,
            });
        } else if response.drag_started() && !hit.filter_btn && !hit.border {
            *drag = Some(Drag::Move { col: hit.col });
        } else if response.clicked() && !hit.border {
            let id = vis[hit.col].id.clone();
            if hit.filter_btn {
                events.push(Event::FilterMenu { column: id, pos: m });
            } else {
                let dir = match sort {
                    (Some(c), false) if *c == id => Some(false),
                    (Some(c), _) if *c == id => None,
                    _ => Some(true),
                };
                events.push(Event::Sort { column: id, dir });
            }
        }
        return;
    }

    // Body rows: selection, popover, context menu.
    let row = row_at(m.y, origin.y + header_h, row_h, first_row, last_row);
    let key_at = |r: i64| -> String {
        row_keys
            .get((r - first_row) as usize)
            .cloned()
            .unwrap_or_default()
    };
    if response.drag_started() {
        if let Some(r) = row {
            let key = key_at(r);
            if !key.is_empty() {
                if mods.command {
                    selection.ctrl_click(&key);
                } else if mods.shift {
                    selection.shift_click(&key, row_keys);
                } else {
                    selection.click(&key);
                }
                *active_index = Some(r);
                *drag = Some(Drag::Select { anchor: r });
            }
        } else {
            // Empty space below the rows: clear the selection.
            selection.clear();
            transients.dismiss();
        }
        return;
    }
    if response.clicked() {
        if let Some(r) = row {
            let key = key_at(r);
            if !key.is_empty() {
                if mods.command {
                    selection.ctrl_click(&key);
                } else if mods.shift {
                    selection.shift_click(&key, row_keys);
                } else {
                    selection.click(&key);
                }
                *active_index = Some(r);
                transients.dismiss();
            }
        } else {
            selection.clear();
        }
    }
    if response.double_clicked() {
        if let Some(r) = row {
            let key = key_at(r);
            if !key.is_empty() {
                events.push(Event::Popover {
                    key,
                    column: vis.get(hit.col).map(|c| c.id.clone()).unwrap_or_default(),
                    pos: m,
                });
            }
        }
    }
    if response.secondary_clicked() {
        if let Some(r) = row {
            let key = key_at(r);
            if !key.is_empty() {
                if !selection.contains(&key) {
                    selection.click(&key);
                    *active_index = Some(r);
                }
                events.push(Event::ContextMenu { key, pos: m });
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_keys(
    ui: &Ui,
    id: &str,
    response: &egui::Response,
    row_h: f32,
    view_h: f32,
    rows: i64,
    first_row: i64,
    last_row: i64,
    max_scroll_row: f64,
    scroll_y: &mut f64,
    scroll_target_y: &mut f64,
    selection: &mut Selection,
    transients: &mut Transients,
    active_index: &mut Option<i64>,
    row_keys: &[String],
    events: &mut Vec<Event>,
) {
    let _ = id;
    if !response.has_focus() {
        return;
    }
    let last_index = rows.max(1) - 1;
    let per_page = (view_h / row_h).max(1.0) as i64;
    let mut move_sel: i64 = 0;
    let mut page: f32 = 0.0;
    let mut extend = false;
    let mut fire: Option<Event> = None;
    ui.input(|i| {
        for e in &i.events {
            match e {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let cmd = modifiers.command;
                    match key {
                        egui::Key::ArrowDown => {
                            move_sel = 1;
                            extend = modifiers.shift;
                        }
                        egui::Key::ArrowUp => {
                            move_sel = -1;
                            extend = modifiers.shift;
                        }
                        egui::Key::PageDown => page = per_page as f32,
                        egui::Key::PageUp => page = -(per_page as f32),
                        egui::Key::Home => move_sel = -rows,
                        egui::Key::End => move_sel = rows,
                        egui::Key::Enter => fire = Some(Event::Activate),
                        egui::Key::Escape => {
                            if transients.popover.is_some() {
                                transients.dismiss();
                            } else {
                                selection.clear();
                            }
                        }
                        egui::Key::F if cmd => fire = Some(Event::FocusFind),
                        egui::Key::A if cmd => selection.select_all(),
                        _ => {}
                    }
                }
                egui::Event::Copy => fire = Some(Event::Copy),
                _ => {}
            }
        }
    });
    if page != 0.0 {
        // One row of overlap keeps the reading line in view.
        *scroll_target_y =
            (*scroll_target_y + (page - page.signum()) as f64).clamp(0.0, max_scroll_row);
        *scroll_y = *scroll_target_y;
        // Paging moves the selection with the view (KB-04).
        if move_sel == 0 {
            move_sel = page as i64;
        }
    }
    if move_sel != 0 {
        let cur = active_index.unwrap_or(first_row);
        let next = (cur + move_sel).clamp(0, last_index);
        *active_index = Some(next);
        if next >= first_row && next < last_row {
            if let Some(key) = row_keys
                .get((next - first_row) as usize)
                .filter(|k| !k.is_empty())
            {
                if extend {
                    selection.shift_click(key, row_keys);
                } else {
                    selection.click(key);
                }
            }
        }
        // Keep the active row in view with a small margin.
        let top = next as f64;
        let bottom = top + 1.0;
        let view_top = *scroll_y;
        let view_bottom = view_top + (view_h / row_h) as f64;
        let jump = if top < view_top {
            Some(next as f64 - 1.0)
        } else if bottom > view_bottom {
            Some(next as f64 - (view_h / row_h) as f64 + 2.0)
        } else {
            None
        };
        if let Some(next) = jump {
            let next = next.max(0.0);
            *scroll_y = next;
            *scroll_target_y = next;
        }
    }
    if let Some(ev) = fire {
        events.push(ev);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_transients<S: RowSource>(
    ui: &Ui,
    p: &Palette,
    transients: &Transients,
    pointer: Option<Pos2>,
    origin: Pos2,
    full: Rect,
    header_h: f32,
    row_h: f32,
    first_row: i64,
    last_row: i64,
    selection: &Selection,
    source: &mut S,
) {
    let painter = ui.painter();
    if transients.toast_alive() {
        if let Some((text, _)) = &transients.toast {
            let rect = Rect::from_min_size(
                pointer.unwrap_or(full.center()) + Vec2::new(12.0, 18.0),
                Vec2::new(TOAST_W, 22.0),
            );
            painter.rect_filled(rect, radius::SM as f32, p.tertiary_container);
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(ty::CAPTION),
                p.on_tertiary_container,
            );
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(50));
    }
    if transients.copied_alive() {
        for r in first_row..last_row {
            let key = source.row(r).and_then(|v| row_key(&v));
            if key.as_deref().is_some_and(|k| selection.contains(k)) {
                let rect = row_rect(
                    origin,
                    full.width(),
                    origin.y + header_h,
                    row_h,
                    r,
                    first_row,
                );
                let pts = [
                    rect.min,
                    rect.right_top(),
                    rect.right_bottom(),
                    rect.left_bottom(),
                    rect.min,
                ];
                let dashed = egui::Shape::dashed_line(
                    &pts,
                    Stroke::new(1.5_f32, p.accent),
                    4.0_f32,
                    3.0_f32,
                );
                painter.extend(dashed);
            }
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    }
}

/// Stable row key of a row value (`url_id` as text for the URL table).
fn row_key(v: &Value) -> Option<String> {
    v.get("url_id")
        .and_then(Value::as_i64)
        .map(|i| i.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: [f32; 3] = [60.0, 200.0, 200.0];
    const O: Pos2 = Pos2::new(0.0, 0.0);

    #[test]
    fn hit_test_names_the_vbar_zone() {
        let h = hit_test(Pos2::new(470.0, 100.0), O, 460.0, 34.0, &W, 1, 0.0);
        assert!(h.vbar);
    }

    #[test]
    fn hit_test_finds_columns_in_header_and_body() {
        let h = hit_test(Pos2::new(100.0, 10.0), O, 460.0, 34.0, &W, 1, 0.0);
        assert!(h.header && h.col == 1);
        // The strip scrolled 250 px: column 2 sits under the pointer now.
        let h = hit_test(Pos2::new(100.0, 100.0), O, 460.0, 34.0, &W, 1, 250.0);
        assert!(!h.header);
        assert_eq!(h.col, 2);
    }

    #[test]
    fn hit_test_spots_borders_and_the_filter_button() {
        // Border between columns 1 and 2 at x = 60 + 200.
        let h = hit_test(Pos2::new(260.0, 10.0), O, 460.0, 34.0, &W, 1, 0.0);
        assert!(h.border);
        let h = hit_test(Pos2::new(242.0, 10.0), O, 460.0, 34.0, &W, 1, 0.0);
        assert!(h.filter_btn, "the filter button is the rightmost strip");
        let h = hit_test(Pos2::new(30.0, 10.0), O, 460.0, 34.0, &W, 1, 0.0);
        assert!(!h.filter_btn);
    }

    #[test]
    fn col_at_picks_the_last_column_past_the_end() {
        assert_eq!(col_at(&W, 1, 0.0, 30.0), 0);
        assert_eq!(col_at(&W, 1, 0.0, 100.0), 1);
        assert_eq!(col_at(&W, 1, 0.0, 500.0), 2);
    }

    #[test]
    fn row_at_clips_to_the_visible_range() {
        assert_eq!(row_at(40.0, 34.0, 30.0, 5, 20), Some(5));
        assert_eq!(row_at(70.0, 34.0, 30.0, 5, 20), Some(6));
        assert_eq!(row_at(20.0, 34.0, 30.0, 5, 20), None, "above the body");
        assert_eq!(row_at(100.0, 34.0, 30.0, 5, 6), None, "past the last row");
    }

    #[test]
    fn move_target_names_the_nearest_edge() {
        // Columns at 0, 60, 260 (frozen 1): pointer at 130 → before col 2.
        assert_eq!(move_target(&W, 1, 0.0, 2, 100.0), Some(1));
        assert_eq!(
            move_target(&W, 1, 0.0, 1, 260.0),
            None,
            "next edge = same place"
        );
        assert_eq!(
            move_target(&W, 1, 0.0, 2, 30.0),
            None,
            "into the frozen strip"
        );
    }

    /// Frame-time benchmark over one million rows, headless (egui computes
    /// and tessellates the frame, nothing is rasterized). Not a unit test:
    /// run it in release with
    /// `cargo test -p spiderhead-desktop --release frame_time -- --ignored --nocapture`
    /// and read the printed average/p95; the release-only assertion holds the
    /// 60 fps line.
    #[test]
    #[ignore = "frame-time benchmark; run in release with --ignored"]
    fn frame_time_headless_million_rows() {
        use crate::theme::Theme;
        use std::time::Instant;

        struct Million;
        impl RowSource for Million {
            fn rows(&mut self) -> i64 {
                1_000_000
            }
            fn row(&mut self, i: i64) -> Option<Value> {
                Some(serde_json::json!({
                    "url_id": i,
                    "url": format!("http://example.com/catalog/page-{}.html", i),
                    "status_code": 200,
                    "content_type": "text/html",
                    "title": format!("Catalogue page {} — lorem ipsum dolor sit amet", i),
                    "size_bytes": 12_345,
                    "response_time": 0.12,
                    "indexable": true,
                }))
            }
        }

        let mk = |id: &str, w: Option<f32>, num: bool| GridColumn {
            id: id.to_string(),
            header: id.to_string(),
            width: w,
            min_width: 0.0,
            numeric: num,
        };
        let columns = vec![
            mk("url", None, false),
            mk("status_code", Some(72.0), true),
            mk("content_type", Some(110.0), false),
            mk("title", Some(260.0), false),
            mk("size_bytes", Some(96.0), true),
            mk("response_time", Some(96.0), true),
            mk("indexable", Some(96.0), false),
        ];
        let mut cell = |ui: &mut Ui, _p: &Palette, col: &GridColumn, v: Option<&Value>| {
            let Some(v) = v else { return };
            if col.numeric {
                ui.label(v.as_i64().unwrap_or(0).to_string());
            } else {
                ui.add(egui::Label::new(v.as_str().unwrap_or("")).truncate());
            }
        };

        let ctx = egui::Context::default();
        let mut g = GridUi::default();
        let palette = Theme::Light.palette();
        let frame_ms = |times: &[std::time::Duration]| -> (f64, f64) {
            let avg = times.iter().map(|t| t.as_secs_f64()).sum::<f64>() / times.len() as f64;
            let mut sorted = times.to_vec();
            sorted.sort();
            let p95 = sorted[sorted.len() * 95 / 100].as_secs_f64();
            (avg * 1000.0, p95 * 1000.0)
        };

        // Warmup: fonts, caches.
        let mut warm = Vec::new();
        for frame in 0..30 {
            let input = egui::RawInput {
                time: Some(frame as f64 * 0.016),
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 900.0))),
                ..egui::RawInput::default()
            };
            let t0 = Instant::now();
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut src = Million;
                    let _ = show(
                        ui,
                        &palette,
                        &mut g,
                        GridArgs {
                            id: "bench",
                            columns: &columns,
                            sort: (None, false),
                            density: 1.0,
                            source: &mut src,
                            cell: &mut cell,
                        },
                    );
                });
            });
            warm.push(t0.elapsed());
        }

        // Measured run: steady scrolling (the easing keeps asking for frames
        // while the offset catches up) plus idle frames that must repaint
        // only on input.
        let mut times = Vec::new();
        for frame in 0..300 {
            let mut input = egui::RawInput {
                time: Some((30 + frame) as f64 * 0.016),
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 900.0))),
                ..egui::RawInput::default()
            };
            if frame % 5 == 0 {
                input.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0.0, -60.0),
                    modifiers: egui::Modifiers::default(),
                });
            }
            let t0 = Instant::now();
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut src = Million;
                    let _ = show(
                        ui,
                        &palette,
                        &mut g,
                        GridArgs {
                            id: "bench",
                            columns: &columns,
                            sort: (None, false),
                            density: 1.0,
                            source: &mut src,
                            cell: &mut cell,
                        },
                    );
                });
            });
            times.push(t0.elapsed());
        }
        let (warm_avg, _) = frame_ms(&warm);
        let (avg, p95) = frame_ms(&times);
        println!(
            "grid frame time, 1M rows, 1600×900: warmup avg {warm_avg:.2} ms;              scroll avg {avg:.2} ms, p95 {p95:.2} ms"
        );
        if !cfg!(debug_assertions) {
            assert!(p95 < 16.6, "p95 frame {p95:.2} ms misses 60 fps on 1M rows");
        }
    }

    #[test]
    fn thumb_h_stays_above_the_floor() {
        let track = Rect::from_min_size(Pos2::ZERO, Vec2::new(14.0, 300.0));
        assert!(thumb_h(track, 100.0, 1_000_000.0) >= THUMB_MIN);
        // A nearly-fitting content keeps the thumb large.
        assert!(thumb_h(track, 300.0, 301.0) > 140.0);
    }
}
