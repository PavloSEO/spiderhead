//! `rich.layout.Layout` and `rich.table.Table` (grid mode) for the shell.
//!
//! Region division follows `ratio_resolve` exactly; table column widths follow `_calculate_column_widths`,
//! `_measure_column`, `_collapse_widths` and the `ratio_distribute` /
//! `ratio_reduce` helpers.

use crate::render::{
    justify_lines, measure, render_lines, Justify, Measurement, Options, Renderable, Segment,
};
use crate::text::{cell_len, Overflow, Text};

/// A `rich.layout.Layout` node (named, splittable, optionally sized).
#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// Region name for `layout["name"]` access.
    pub name: String,
    /// Renderable placed in the leaf (`update`).
    pub renderable: Option<Renderable>,
    /// Fixed size along the splitter direction.
    pub size: Option<usize>,
    /// Minimum cells/lines this region may take.
    pub minimum_size: usize,
    /// Share of flexible space.
    pub ratio: usize,
    /// Hidden children are skipped (`visible`).
    pub visible: bool,
    /// Split direction for `children`.
    pub direction: Direction,
    /// Child regions.
    pub children: Vec<Layout>,
    /// `_DashboardLayout.size` — height forced on the root at render time.
    pub force_height: Option<usize>,
}

/// Split direction (rich `split_row`/`split_column`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// Children divide the region's height (stacked vertically).
    #[default]
    Column,
    /// Children divide the region's width (side by side).
    Row,
}

impl Layout {
    /// `Layout(name=name)` with no renderable.
    pub fn named(name: &str) -> Self {
        Layout {
            name: name.into(),
            minimum_size: 1,
            ratio: 1,
            visible: true,
            ..Default::default()
        }
    }

    /// `_DashboardLayout(size=height)` — a root that overrides render height.
    pub fn dashboard(height: usize) -> Self {
        Layout {
            minimum_size: 1,
            ratio: 1,
            visible: true,
            force_height: Some(height),
            ..Default::default()
        }
    }

    /// `split_column(*children)`.
    pub fn split_column(&mut self, children: Vec<Layout>) {
        self.direction = Direction::Column;
        self.children = children;
    }

    /// `split_row(*children)`.
    pub fn split_row(&mut self, children: Vec<Layout>) {
        self.direction = Direction::Row;
        self.children = children;
    }

    /// `layout.update(renderable)`.
    pub fn update(&mut self, renderable: Renderable) {
        self.renderable = Some(renderable);
    }

    /// `layout["name"]` — first child (recursively) with the name.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Layout> {
        for child in &mut self.children {
            if child.name == name {
                return Some(child);
            }
            if let Some(found) = child.get_mut(name) {
                return Some(found);
            }
        }
        None
    }
}

/// `(x, y, width, height)` region tuple (rich `Region`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Region {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

/// `Layout.__rich_console__`: map regions, render leaves, join rows.
pub fn render_layout(root: &Layout, options: &Options, out: &mut Vec<Segment>) {
    let width = options.max_width;
    let Some(height) = options.height else {
        // Rich falls back to console.height; the shell always passes one.
        return;
    };
    let regions = region_map(
        root,
        Region {
            x: 0,
            y: 0,
            width,
            height,
        },
    );
    let mut rows: Vec<Vec<Segment>> = vec![Vec::new(); height];
    for (_layout, leaf) in &regions {
        if leaf.has_children {
            continue;
        }
        let region = leaf.region;
        let lines = render_lines(
            leaf.renderable.unwrap_or(&Renderable::Text(Text::blank())),
            &Options {
                max_width: region.width,
                height: Some(region.height),
                ..*options
            },
        );
        for (row, line) in rows
            .iter_mut()
            .take(region.y + region.height)
            .skip(region.y)
            .zip(lines)
        {
            row.extend(line);
        }
    }
    for row in rows {
        out.extend(row);
        out.push(Segment {
            text: "\n".into(),
            style: String::new(),
        });
    }
}

struct LeafRegion<'a> {
    region: Region,
    has_children: bool,
    renderable: Option<&'a Renderable>,
}

/// `_make_region_map` — DFS region division.
fn region_map<'a>(root: &'a Layout, region: Region) -> Vec<(&'a Layout, LeafRegion<'a>)> {
    let mut out: Vec<(&'a Layout, LeafRegion<'a>)> = Vec::new();
    let mut stack: Vec<(&'a Layout, Region)> = vec![(root, region)];
    while let Some((layout, region)) = stack.pop() {
        let children: Vec<&Layout> = layout.children.iter().filter(|c| c.visible).collect();
        let has_children = !children.is_empty();
        out.push((
            layout,
            LeafRegion {
                region,
                has_children,
                renderable: layout.renderable.as_ref(),
            },
        ));
        if has_children {
            for (child, child_region) in divide(&children, region, layout.direction) {
                stack.push((child, child_region));
            }
        }
    }
    // `sorted(layout_regions, key=region)` — (x, y, w, h) ordering.
    out.sort_by_key(|(_, leaf)| leaf.region);
    out
}

/// `Splitter.divide` — allocate each child's region in the parent.
fn divide<'a>(
    children: &[&'a Layout],
    region: Region,
    direction: Direction,
) -> Vec<(&'a Layout, Region)> {
    let sizes = ratio_resolve(
        match direction {
            Direction::Row => region.width,
            Direction::Column => region.height,
        },
        children,
    );
    let mut out = Vec::with_capacity(children.len());
    let mut offset = 0usize;
    for (child, size) in children.iter().copied().zip(sizes) {
        let child_region = match direction {
            Direction::Row => Region {
                x: region.x + offset,
                y: region.y,
                width: size,
                height: region.height,
            },
            Direction::Column => Region {
                x: region.x,
                y: region.y + offset,
                width: region.width,
                height: size,
            },
        };
        out.push((child, child_region));
        offset += size;
    }
    out
}

/// `ratio_resolve` — exact `Fraction` arithmetic over (size, ratio, min).
fn ratio_resolve(total: usize, edges: &[&Layout]) -> Vec<usize> {
    let total = total as i64;
    let mut sizes: Vec<Option<i64>> = edges.iter().map(|e| e.size.map(|s| s as i64)).collect();
    while sizes.iter().any(Option::is_none) {
        let flexible: Vec<(usize, &Layout)> = edges
            .iter()
            .enumerate()
            .filter(|(i, _)| sizes[*i].is_none())
            .map(|(i, e)| (i, *e))
            .collect();
        let remaining = total - sizes.iter().map(|s| s.unwrap_or(0)).sum::<i64>();
        if remaining <= 0 {
            return edges
                .iter()
                .zip(&sizes)
                .map(|(e, s)| s.unwrap_or(e.minimum_size.max(1) as i64).max(0) as usize)
                .collect();
        }
        let sum_ratios: i64 = flexible.iter().map(|(_, e)| (e.ratio.max(1)) as i64).sum();
        // portion = remaining / sum_ratios (Fraction); test
        // portion * ratio <= minimum_size  →  remaining * ratio <= min * sum_ratios.
        let mut fixed_one = false;
        for (index, edge) in &flexible {
            let ratio = (edge.ratio.max(1)) as i64;
            let minimum = edge.minimum_size as i64;
            if remaining * ratio <= minimum * sum_ratios {
                sizes[*index] = Some(minimum);
                fixed_one = true;
                break;
            }
        }
        if fixed_one {
            continue;
        }
        // Distribute: size = floor(portion * ratio + remainder) with the
        // remainder carried forward as a Fraction.
        let mut rem_num: i64 = 0;
        let mut rem_den: i64 = 1;
        for (index, edge) in &flexible {
            let ratio = (edge.ratio.max(1)) as i64;
            // value = remaining*ratio/sum_ratios + rem_num/rem_den
            let num = remaining * ratio * rem_den + rem_num * sum_ratios;
            let den = sum_ratios * rem_den;
            let size = num.div_euclid(den);
            rem_num = num.rem_euclid(den);
            rem_den = den;
            sizes[*index] = Some(size);
        }
        break;
    }
    sizes
        .into_iter()
        .map(|s| s.unwrap_or(0).max(0) as usize)
        .collect()
}

/// One `rich.table.Column` in a grid.
#[derive(Debug, Clone, Default)]
pub struct Column {
    /// Ratio for flexible columns (`None` = fixed to measured width).
    pub ratio: Option<usize>,
    /// Fixed width override.
    pub width: Option<usize>,
    /// Minimum width.
    pub min_width: Option<usize>,
    /// Maximum width.
    pub max_width: Option<usize>,
    /// Cell justify method.
    pub justify: Option<Justify>,
    /// Column `no_wrap`.
    pub no_wrap: bool,
    /// Column `overflow` method name.
    pub overflow: Option<Overflow>,
}

/// `rich.table.Table` in grid mode: no box, no header/footer/edges.
#[derive(Debug, Clone, Default)]
pub struct Grid {
    /// `expand` — fill the console width.
    pub expand: bool,
    /// Columns in order.
    pub columns: Vec<Column>,
    /// Rows of cell renderables.
    pub rows: Vec<Vec<Renderable>>,
    /// Padding (top, right, bottom, left).
    pub padding: (usize, usize, usize, usize),
    /// `collapse_padding` (grid default True).
    pub collapse_padding: bool,
    /// `pad_edge` (grid default False).
    pub pad_edge: bool,
}

impl Grid {
    /// `Table.grid(expand=expand, padding=padding)`.
    pub fn new(expand: bool, padding: (usize, usize, usize, usize)) -> Self {
        Grid {
            expand,
            collapse_padding: true,
            pad_edge: false,
            padding,
            ..Default::default()
        }
    }

    /// `grid.add_column(...)`.
    pub fn add_column(&mut self, column: Column) {
        self.columns.push(column);
    }

    /// `grid.add_row(*cells)`.
    pub fn add_row(&mut self, cells: Vec<Renderable>) {
        self.rows.push(cells);
    }

    /// `_get_padding_width(index)` — horizontal padding inside a column.
    fn padding_width(&self, index: usize) -> usize {
        let (_, mut pad_right, _, mut pad_left) = self.padding;
        if self.collapse_padding {
            pad_left = 0;
            pad_right = pad_left.abs_diff(pad_right);
        }
        if !self.pad_edge {
            if index == 0 {
                pad_left = 0;
            }
            if index == self.columns.len().saturating_sub(1) {
                pad_right = 0;
            }
        }
        pad_left + pad_right
    }

    /// `_get_cells` padding (top, right, bottom, left) per cell position.
    fn cell_padding(
        &self,
        column: usize,
        first_row: bool,
        last_row: bool,
    ) -> (usize, usize, usize, usize) {
        let (top, right, bottom, left) = self.padding;
        let (mut top, mut right, mut bottom, mut left) = (top, right, bottom, left);
        let first_column = column == 0;
        let last_column = column == self.columns.len().saturating_sub(1);
        if self.collapse_padding {
            if !first_column {
                left = left.saturating_sub(right);
            }
            if !last_row {
                bottom = top.saturating_sub(bottom);
            }
        }
        if !self.pad_edge {
            if first_column {
                left = 0;
            }
            if last_column {
                right = 0;
            }
            if first_row {
                top = 0;
            }
            if last_row {
                bottom = 0;
            }
        }
        (top, right, bottom, left)
    }

    /// `_measure_column` — min/max over the column's padded cells.
    fn measure_column(&self, options: &Options, index: usize) -> Measurement {
        if options.max_width < 1 {
            return Measurement {
                minimum: 0,
                maximum: 0,
            };
        }
        let column = &self.columns[index];
        let padding_width = self.padding_width(index);
        if let Some(width) = column.width {
            return Measurement {
                minimum: width + padding_width,
                maximum: width + padding_width,
            }
            .with_maximum_c(options.max_width);
        }
        let mut min_widths: Vec<usize> = Vec::new();
        let mut max_widths: Vec<usize> = Vec::new();
        let row_count = self.rows.len();
        for (row_index, row) in self.rows.iter().enumerate() {
            let Some(cell) = row.get(index) else {
                continue;
            };
            let padding = self.cell_padding(index, row_index == 0, row_index + 1 == row_count);
            let renderable = Renderable::Padding(Box::new(cell.clone()), padding);
            let m = measure(&renderable, options);
            min_widths.push(m.minimum);
            max_widths.push(m.maximum);
        }
        let mut measurement = Measurement {
            minimum: min_widths.into_iter().max().unwrap_or(1),
            maximum: max_widths.into_iter().max().unwrap_or(options.max_width),
        }
        .with_maximum_c(options.max_width);
        measurement = measurement.clamp(
            column.min_width.map(|w| w + padding_width),
            column.max_width.map(|w| w + padding_width),
        );
        measurement
    }

    /// `_calculate_column_widths`.
    fn column_widths(&self, options: &Options) -> Vec<usize> {
        let max_width = options.max_width;
        let width_ranges: Vec<Measurement> = (0..self.columns.len())
            .map(|i| self.measure_column(options, i))
            .collect();
        let mut widths: Vec<usize> = width_ranges.iter().map(|r| r.maximum.max(1)).collect();
        if self.expand {
            let flexible: Vec<bool> = self.columns.iter().map(|c| c.ratio.is_some()).collect();
            let ratios: Vec<usize> = self
                .columns
                .iter()
                .zip(&flexible)
                .filter(|(_, f)| **f)
                .map(|(c, _)| c.ratio.unwrap_or(0))
                .collect();
            if ratios.iter().any(|r| *r > 0) {
                let fixed_widths: Vec<usize> = width_ranges
                    .iter()
                    .zip(&flexible)
                    .map(|(range, flex)| if *flex { 0 } else { range.maximum })
                    .collect();
                let flex_minimum: Vec<usize> = self
                    .columns
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.ratio.is_some())
                    .map(|(i, c)| c.width.unwrap_or(1) + self.padding_width(i))
                    .collect();
                let flexible_width = max_width as i64 - fixed_widths.iter().sum::<usize>() as i64;
                let flex_widths = ratio_distribute(flexible_width, &ratios, Some(&flex_minimum));
                let mut flex_iter = flex_widths.into_iter();
                for (index, flex) in flexible.iter().enumerate() {
                    if *flex {
                        widths[index] = fixed_widths[index] + flex_iter.next().unwrap_or(0);
                    }
                }
            }
        }
        let mut table_width: usize = widths.iter().sum();
        if table_width > max_width {
            let wrapable: Vec<bool> = self
                .columns
                .iter()
                .map(|c| c.width.is_none() && !c.no_wrap)
                .collect();
            widths = collapse_widths(&widths, &wrapable, max_width);
            table_width = widths.iter().sum();
            if table_width > max_width {
                let excess = table_width - max_width;
                widths = ratio_reduce(excess as i64, &vec![1usize; widths.len()], &widths, &widths);
                table_width = widths.iter().sum();
            }
            let _ = table_width;
            // Re-measure columns at their final widths.
            let width_ranges: Vec<Measurement> = (0..self.columns.len())
                .map(|i| self.measure_column(&options.update_width(widths[i]), i))
                .collect();
            widths = width_ranges.iter().map(|r| r.maximum).collect();
        }
        table_width = widths.iter().sum();
        if table_width < max_width && self.expand {
            let pads = ratio_distribute((max_width - table_width) as i64, &widths, None);
            widths = widths.iter().zip(pads).map(|(w, p)| w + p).collect();
        }
        widths
    }
}

impl Measurement {
    fn with_maximum_c(self, width: usize) -> Self {
        Measurement {
            minimum: self.minimum.min(width),
            maximum: self.maximum.min(width),
        }
    }
}

/// `Table.__rich_console__` for a grid: render each row of padded cells.
pub fn render_grid(grid: &Grid, options: &Options, out: &mut Vec<Segment>) {
    if grid.columns.is_empty() {
        out.push(Segment {
            text: "\n".into(),
            style: String::new(),
        });
        return;
    }
    let widths = grid.column_widths(options);
    let row_count = grid.rows.len();
    for (row_index, row) in grid.rows.iter().enumerate() {
        let mut cells: Vec<Vec<Vec<Segment>>> = Vec::with_capacity(grid.columns.len());
        let mut max_height = 1usize;
        for (index, column) in grid.columns.iter().enumerate() {
            let width = widths[index];
            let padding = grid.cell_padding(index, row_index == 0, row_index + 1 == row_count);
            let cell: Renderable = match row.get(index) {
                Some(cell) if padding != (0, 0, 0, 0) => {
                    Renderable::Padding(Box::new(cell.clone()), padding)
                }
                Some(cell) => cell.clone(),
                None => Renderable::Text(Text::blank()),
            };
            let cell_options = Options {
                max_width: width,
                height: None,
                justify: column.justify,
                overflow: column.overflow,
                no_wrap: column.no_wrap || options.no_wrap,
            };
            let lines = render_lines(&cell, &cell_options);
            max_height = max_height.max(lines.len());
            cells.push(lines);
        }
        let shaped: Vec<Vec<Vec<Segment>>> = cells
            .into_iter()
            .zip(&widths)
            .map(|(cell, w)| crate::render::set_shape(&cell, *w, Some(max_height)))
            .collect();
        for line_no in 0..max_height {
            for cell in &shaped {
                out.extend(cell[line_no].iter().cloned());
            }
            out.push(Segment {
                text: "\n".into(),
                style: String::new(),
            });
        }
    }
}

/// `Lines.justify` for one grid column's lines (rich renders cells through
/// `render_lines` with `justify` in options — Text handles it internally;
/// this helper exists for completeness of the port).
#[allow(dead_code)]
fn justify_cell(lines: &mut [Text], width: usize, justify: Justify) {
    justify_lines(lines, width, justify, Overflow::Fold);
}

/// `_collapse_widths` — shrink wrapable columns to fit `max_width`.
fn collapse_widths(widths: &[usize], wrapable: &[bool], max_width: usize) -> Vec<usize> {
    let mut widths = widths.to_vec();
    let mut total_width: usize = widths.iter().sum();
    let mut excess = total_width as i64 - max_width as i64;
    if wrapable.iter().any(|w| *w) {
        while total_width > 0 && excess > 0 {
            let max_column = widths
                .iter()
                .zip(wrapable)
                .filter(|(_, w)| **w)
                .map(|(w, _)| *w)
                .max()
                .unwrap_or(0);
            let second_max = widths
                .iter()
                .zip(wrapable)
                .filter(|(w, allow)| **allow && **w != max_column)
                .map(|(w, _)| *w)
                .max()
                .unwrap_or(0);
            let column_difference = max_column.saturating_sub(second_max);
            let ratios: Vec<usize> = widths
                .iter()
                .zip(wrapable)
                .map(|(w, allow)| usize::from(*allow && *w == max_column))
                .collect();
            if !ratios.iter().any(|r| *r > 0) || column_difference == 0 {
                break;
            }
            let max_reduce: Vec<usize> =
                vec![(excess as usize).min(column_difference); widths.len()];
            widths = ratio_reduce(excess, &ratios, &max_reduce, &widths);
            total_width = widths.iter().sum();
            excess = total_width as i64 - max_width as i64;
        }
    }
    widths
}

/// `round()` — round-half-to-even on a float.
pub fn py_round(value: f64) -> i64 {
    let floor = value.floor();
    let diff = value - floor;
    let rounded = if diff > 0.5 || (diff == 0.5 && (floor as i64) % 2 != 0) {
        floor + 1.0
    } else {
        floor
    };
    rounded as i64
}

/// `ratio_distribute` — integer shares, minimums honored, sums to `total`.
fn ratio_distribute(total: i64, ratios: &[usize], minimums: Option<&[usize]>) -> Vec<usize> {
    let ratios: Vec<i64> = match minimums {
        Some(mins) => ratios
            .iter()
            .zip(mins)
            .map(|(r, m)| if *m > 0 { *r as i64 } else { 0 })
            .collect(),
        None => ratios.iter().map(|r| *r as i64).collect(),
    };
    let mut total_ratio: i64 = ratios.iter().sum();
    debug_assert!(total_ratio > 0);
    if total_ratio <= 0 {
        return vec![0; ratios.len()];
    }
    let mut remaining = total;
    let mut out = Vec::with_capacity(ratios.len());
    for (ratio, minimum) in ratios.iter().zip(
        minimums
            .map(|m| m.to_vec())
            .unwrap_or_else(|| vec![0; ratios.len()]),
    ) {
        let distributed = if total_ratio > 0 {
            ((ratio * remaining) as f64 / total_ratio as f64).ceil() as i64
        } else {
            remaining
        }
        .max(minimum as i64);
        out.push(distributed.max(0) as usize);
        total_ratio -= ratio;
        remaining -= distributed;
    }
    out
}

/// `ratio_reduce` — subtract `total` across values proportionally.
fn ratio_reduce(total: i64, ratios: &[usize], maximums: &[usize], values: &[usize]) -> Vec<usize> {
    let ratios: Vec<i64> = ratios
        .iter()
        .zip(maximums)
        .map(|(r, m)| if *m > 0 { *r as i64 } else { 0 })
        .collect();
    let mut total_ratio: i64 = ratios.iter().sum();
    if total_ratio == 0 {
        return values.to_vec();
    }
    let mut remaining = total;
    let mut out = Vec::with_capacity(ratios.len());
    for ((ratio, maximum), value) in ratios.iter().zip(maximums).zip(values) {
        if *ratio > 0 && total_ratio > 0 {
            let distributed = py_round((*ratio * remaining) as f64 / total_ratio as f64);
            let distributed = distributed.min(*maximum as i64);
            out.push(value.saturating_sub(distributed.max(0) as usize));
            remaining -= distributed;
            total_ratio -= ratio;
        } else {
            out.push(*value);
        }
    }
    out
}

/// Cell length of a line's segments (diagnostics).
#[allow(dead_code)]
pub fn line_width(line: &[Segment]) -> usize {
    line.iter().map(|s| cell_len(&s.text)).sum()
}
