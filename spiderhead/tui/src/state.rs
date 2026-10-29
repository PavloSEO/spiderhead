//! Pure navigation state for the interactive shell — no rendering, no I/O.
//!
//! The state machine is dependency-free so
//! behaviour (filtering, cursor movement, view switches) is unit-tested
//! without a terminal.

use serde::{Deserialize, Serialize};

/// Watch sections whose rows can be narrowed with the filter or cleared with `c`.
pub const FILTERABLE_SECTIONS: [&str; 4] = ["findings", "tasks", "methods", "schema"];

/// Watch section order.
pub const WATCH_SECTIONS: [&str; 12] = [
    "overview", "tasks", "methods", "scans", "findings", "views", "activity", "log", "sf", "inbox",
    "schema", "sites",
];

/// Commands offered next to the flat tool list. Grouped namespaces keep their
/// own subcommands in the CLI; the palette lists them as single entries whose
/// detail view points at `seohead <group> --help`.
pub const GROUP_COMMANDS: [&str; 6] = ["scan", "project", "skill", "scenario", "sf", "mcp"];

/// Identity of one rendered page: used to keep the selected row stable across
/// background refreshes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageIdentity {
    /// Active watch section.
    pub section: String,
    /// Current page offset.
    pub offset: i64,
    /// Current filter text.
    pub query: String,
    /// Row identities in page order.
    pub identities: Vec<String>,
}

/// `str.isprintable()` — every character that would echo back in
/// `repr`, i.e. not Other* or Separator* categories except ASCII space.
pub fn is_printable_py(text: &str) -> bool {
    use unicode_general_category::{get_general_category as cat, GeneralCategory as G};
    text.chars().all(|c| {
        !matches!(
            cat(c),
            G::Control
                | G::Format
                | G::Surrogate
                | G::PrivateUse
                | G::Unassigned
                | G::LineSeparator
                | G::ParagraphSeparator
                | G::SpaceSeparator
        ) || c == ' '
    })
}

/// Keyboard-navigable palette over the shared CLI command table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellState {
    /// Flat command list plus `GROUP_COMMANDS` (merged like `__post_init__`).
    pub commands: Vec<String>,
    /// Palette filter text.
    pub query: String,
    /// Palette cursor.
    pub index: i64,
    /// Active view: palette|detail|help|watch|watch_detail|watch_filter|watch_help|note.
    pub view: String,
    /// True once the user asked to leave; the run loop turns it into exit 0.
    pub quit_requested: bool,
    /// Note draft being edited.
    pub note_text: String,
    /// Set when Enter asks to save a non-empty in-limit draft.
    pub note_ready: bool,
    /// `note` or `proposed_goal`.
    pub note_kind: String,
    /// View to return to after the note editor.
    pub note_return_view: String,
    /// Save/validation error shown in the editor.
    pub note_error: String,
    /// Character limit for a saved note.
    pub note_limit: i64,
    /// Cursor offset in `note_text`; `None` means end of text.
    pub note_cursor: Option<usize>,
    /// Suspended drafts per kind.
    pub note_drafts: std::collections::BTreeMap<String, String>,
    /// Inside a bracketed paste.
    pub paste_active: bool,
    /// Spinner animation allowed.
    pub motion_enabled: bool,
    /// Active watch section.
    pub watch_section: String,
    /// Selected row on the current page.
    pub watch_index: i64,
    /// Data offset of the current page.
    pub watch_offset: i64,
    /// Applied findings/tasks filter.
    pub watch_query: String,
    /// Task/method state filter (`None` = all).
    pub watch_status: Option<String>,
    /// Filter being typed.
    pub filter_draft: String,
    /// Findings sort field.
    pub watch_sort: String,
    /// Findings sort direction.
    pub watch_descending: bool,
    /// Ordinal of the finding open in the detail view.
    pub watch_detail_ordinal: Option<i64>,
    /// Scroll offset inside the detail view.
    pub watch_detail_offset: i64,
    /// Kind of the detail view (finding|scan|view|inbox|task|site|...).
    pub watch_detail_kind: String,
    /// Scan the findings/views sections are bound to.
    pub watch_selected_scan_uuid: Option<String>,
    /// Site whose scans/findings are browsed.
    pub watch_site_uuid: Option<String>,
    /// Site open in the detail view.
    pub watch_site_detail_uuid: Option<String>,
    /// Whether the detail site has a workspace directory.
    pub watch_site_openable: bool,
    /// Saved view open in the detail view.
    pub watch_view_name: Option<String>,
    /// Saved-view result page offset.
    pub watch_view_offset: i64,
    /// Next result page offset reported by the view query.
    pub watch_view_next: Option<i64>,
    /// Saved-view page size learned from the result.
    pub watch_view_page_size: i64,
    /// Inbox entry open in the detail view.
    pub watch_inbox_entry_id: Option<String>,
    /// Checklist item id of the selected row.
    pub watch_item_id: Option<String>,
    /// Row count of the current page (learned by the render path).
    pub watch_count: Option<i64>,
    /// Next page offset (`None` on the last page).
    pub watch_next_offset: Option<i64>,
    /// Previous page offset.
    pub watch_previous_offset: Option<i64>,
    /// Watch page size (50).
    pub watch_page_size: i64,
    /// Page identity used to keep selection stable across refreshes.
    pub watch_page_identity: Option<PageIdentity>,
    /// Identity of the selected row.
    pub watch_selected_row_id: Option<String>,
    /// View `?` returns to.
    pub watch_return_view: String,
}

impl ShellState {
    /// `ShellState(commands=...)` — merges `GROUP_COMMANDS` like `__post_init__`.
    pub fn new(commands: Vec<String>) -> Self {
        let mut all = commands.clone();
        for c in GROUP_COMMANDS {
            if !all.iter().any(|existing| existing == c) {
                all.push(c.to_string());
            }
        }
        ShellState {
            commands: all,
            query: String::new(),
            index: 0,
            view: "palette".into(),
            quit_requested: false,
            note_text: String::new(),
            note_ready: false,
            note_kind: "note".into(),
            note_return_view: "watch".into(),
            note_error: String::new(),
            note_limit: 8000,
            note_cursor: None,
            note_drafts: Default::default(),
            paste_active: false,
            motion_enabled: true,
            watch_section: "overview".into(),
            watch_index: 0,
            watch_offset: 0,
            watch_query: String::new(),
            watch_status: None,
            filter_draft: String::new(),
            watch_sort: "severity".into(),
            watch_descending: false,
            watch_detail_ordinal: None,
            watch_detail_offset: 0,
            watch_detail_kind: "finding".into(),
            watch_selected_scan_uuid: None,
            watch_site_uuid: None,
            watch_site_detail_uuid: None,
            watch_site_openable: false,
            watch_view_name: None,
            watch_view_offset: 0,
            watch_view_next: None,
            watch_view_page_size: 50,
            watch_inbox_entry_id: None,
            watch_item_id: None,
            watch_count: None,
            watch_next_offset: None,
            watch_previous_offset: None,
            watch_page_size: 50,
            watch_page_identity: None,
            watch_selected_row_id: None,
            watch_return_view: "watch".into(),
        }
    }

    /// `filtered` property — commands containing the query, case-insensitive.
    pub fn filtered(&self) -> Vec<&str> {
        if self.query.is_empty() {
            return self.commands.iter().map(String::as_str).collect();
        }
        let needle = self.query.to_lowercase();
        self.commands
            .iter()
            .map(String::as_str)
            .filter(|c| c.contains(&needle))
            .collect()
    }

    /// `selected` property — the highlighted command or `None`.
    pub fn selected(&self) -> Option<&str> {
        let items = self.filtered();
        if items.is_empty() {
            None
        } else {
            Some(items[(self.index as usize).min(items.len() - 1)])
        }
    }

    /// `move(delta)` — clamped cursor move.
    pub fn move_by(&mut self, delta: i64) {
        let items = self.filtered();
        if items.is_empty() {
            self.index = 0;
            return;
        }
        self.index = (self.index + delta).clamp(0, items.len() as i64 - 1);
    }

    /// `jump(index)` — clamped absolute cursor move.
    pub fn jump(&mut self, index: i64) {
        let items = self.filtered();
        self.index = index.clamp(0, (items.len() as i64 - 1).max(0));
    }

    /// `type_char` — append to the palette filter and reset the cursor.
    pub fn type_char(&mut self, c: &str) {
        self.query.push_str(c);
        self.index = 0;
    }

    /// `backspace` — drop the last filter character.
    pub fn backspace(&mut self) {
        self.query.pop();
        self.index = 0;
    }

    /// `open_selected` — open the detail view for the highlighted command.
    pub fn open_selected(&mut self) {
        if self.selected().is_some() {
            self.watch_detail_offset = 0;
            self.view = "detail".into();
        }
    }

    /// `back` — clear a filter first, leave a sub-view, else quit.
    pub fn back(&mut self) -> bool {
        if self.view != "palette" {
            self.view = "palette".into();
            return true;
        }
        if !self.query.is_empty() {
            self.query.clear();
            self.index = 0;
            return true;
        }
        false
    }

    /// `handle_key` — apply one symbolic key name (see `keys`).
    pub fn handle_key(&mut self, key: &str) {
        if key == "timeout" {
            return;
        }
        if key == "ctrl_d" || key == "ctrl_c" {
            self.quit_requested = true;
            return;
        }
        if (self.view == "watch" || self.view == "watch_detail")
            && (key == "char:n" || key == "char:g")
        {
            self.note_return_view = self.view.clone();
            self.note_drafts
                .insert(self.note_kind.clone(), self.note_text.clone());
            self.note_kind = if key == "char:n" {
                "note".into()
            } else {
                "proposed_goal".into()
            };
            self.note_text = self
                .note_drafts
                .get(&self.note_kind)
                .cloned()
                .unwrap_or_default();
            self.note_cursor = Some(self.note_text.chars().count());
            self.note_error.clear();
            self.view = "note".into();
            return;
        }
        match self.view.as_str() {
            "watch_help" => self.key_watch_help(key),
            "note" => self.key_note(key),
            "watch_filter" => self.key_watch_filter(key),
            "watch_detail" => self.key_watch_detail(key),
            "watch" => self.key_watch(key),
            "detail" | "help" => self.key_detail(key),
            _ => self.key_palette(key),
        }
    }

    fn key_watch_help(&mut self, key: &str) {
        if matches!(key, "escape" | "enter" | "char:?") {
            self.view = self.watch_return_view.clone();
        }
    }

    fn key_note(&mut self, key: &str) {
        let text_len = self.note_text.chars().count();
        let cursor = self.note_cursor.unwrap_or(text_len).min(text_len);
        if key == "paste_start" {
            self.paste_active = true;
        } else if key == "paste_end" {
            self.paste_active = false;
        } else if key == "enter" && self.paste_active {
            splice_chars(&mut self.note_text, cursor, cursor, "\n");
            self.note_cursor = Some(cursor + 1);
        } else if key == "escape" {
            self.note_drafts
                .insert(self.note_kind.clone(), self.note_text.clone());
            self.paste_active = false;
            self.view = self.note_return_view.clone();
        } else if key == "backspace" {
            splice_chars(&mut self.note_text, cursor.saturating_sub(1), cursor, "");
            self.note_cursor = Some(cursor.saturating_sub(1));
            self.note_error.clear();
        } else if key == "delete" {
            splice_chars(&mut self.note_text, cursor, cursor + 1, "");
        } else if key == "left" {
            self.note_cursor = Some(cursor.saturating_sub(1));
        } else if key == "right" {
            self.note_cursor = Some((cursor + 1).min(text_len));
        } else if key == "up" || key == "down" {
            let line_start = self.note_text[..byte_of(&self.note_text, cursor)]
                .rfind('\n')
                .map(|b| self.note_text[..=b].chars().count())
                .unwrap_or(0);
            let column = cursor - line_start;
            if key == "up" && line_start > 0 {
                let prev = self.note_text[..byte_of(&self.note_text, line_start - 1)]
                    .rfind('\n')
                    .map(|b| self.note_text[..=b].chars().count())
                    .unwrap_or(0);
                self.note_cursor = Some((prev + column).min(line_start - 1));
            } else if key == "down" {
                let after = &self.note_text[byte_of(&self.note_text, cursor)..];
                if let Some(nl) = after.find('\n') {
                    let next_line = cursor + after[..nl].chars().count();
                    let rest = &self.note_text[byte_of(&self.note_text, next_line + 1)..];
                    let end = rest
                        .find('\n')
                        .map(|i| next_line + 1 + rest[..i].chars().count())
                        .unwrap_or(text_len);
                    self.note_cursor = Some((next_line + 1 + column).min(end));
                }
            }
        } else if key == "home" {
            self.note_cursor = Some(0);
        } else if key == "end" {
            self.note_cursor = Some(text_len);
        } else if key == "ctrl_x" {
            self.note_text.clear();
            self.note_drafts.remove(&self.note_kind);
            self.note_cursor = Some(0);
            self.note_error = "Draft discarded.".into();
        } else if key == "enter" && !self.note_text.trim().is_empty() {
            if self.note_text.chars().count() as i64 > self.note_limit {
                self.note_error = format!(
                    "Draft exceeds {} characters; shorten it before saving.",
                    crate::localization::number(self.note_limit)
                );
            } else {
                self.note_ready = true;
                self.view = self.note_return_view.clone();
            }
        } else if let Some(c) = char_of(key) {
            if is_printable_py(c) {
                splice_chars(&mut self.note_text, cursor, cursor, c);
                self.note_cursor = Some(cursor + c.chars().count());
                if self.note_text.chars().count() as i64 > self.note_limit {
                    self.note_error = format!(
                        "Draft kept in full; shorten to {} characters before saving.",
                        crate::localization::number(self.note_limit)
                    );
                }
            }
        }
    }

    fn key_watch_filter(&mut self, key: &str) {
        if key == "escape" {
            self.view = "watch".into();
        } else if key == "backspace" {
            self.filter_draft.pop();
        } else if key == "enter" {
            self.watch_query = self.filter_draft.clone();
            self.watch_offset = 0;
            self.watch_index = 0;
            self.view = "watch".into();
        } else if let Some(c) = char_of(key) {
            if is_printable_py(c) && self.filter_draft.chars().count() < 256 {
                self.filter_draft.push_str(c);
            }
        }
    }

    fn key_watch_detail(&mut self, key: &str) {
        if self.watch_detail_kind == "site" && key == "char:s" && self.watch_site_openable {
            self.watch_site_uuid = self.watch_site_detail_uuid.clone();
            self.watch_selected_scan_uuid = None;
            self.watch_section = "scans".into();
            self.watch_index = 0;
            self.watch_offset = 0;
            self.watch_count = None;
            self.view = "watch".into();
            return;
        }
        if self.watch_detail_kind == "view" && (key == "char:[" || key == "char:]") {
            if key == "char:[" {
                self.watch_view_offset =
                    (self.watch_view_offset - self.watch_view_page_size).max(0);
            } else if let Some(next) = self.watch_view_next {
                self.watch_view_offset = next;
            }
            self.watch_detail_offset = 0;
            return;
        }
        self.scroll_detail(key, "watch");
    }

    fn key_watch(&mut self, key: &str) {
        if key == "char:m" {
            self.motion_enabled = !self.motion_enabled;
        } else if key == "char:?" {
            self.watch_return_view = self.view.clone();
            self.view = "watch_help".into();
        } else if let Some(&section) = [
            ("char:1", "overview"),
            ("char:2", "tasks"),
            ("char:3", "methods"),
            ("char:4", "scans"),
            ("char:5", "findings"),
            ("char:6", "views"),
            ("char:7", "activity"),
            ("char:8", "log"),
            ("char:9", "sf"),
            ("char:0", "inbox"),
            ("char:a", "schema"),
            ("char:p", "sites"),
        ]
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, s)| s)
        {
            self.watch_section = section.into();
            self.watch_index = 0;
            self.watch_offset = 0;
            self.watch_detail_ordinal = None;
            self.watch_detail_kind = "finding".into();
            self.watch_query.clear();
            self.watch_status = None;
            self.watch_count = None;
            self.watch_item_id = None;
        } else if key == "up" {
            self.watch_index = (self.watch_index - 1).max(0);
        } else if key == "down" {
            self.watch_index = match self.watch_count {
                Some(count) => (self.watch_index + 1).min((count - 1).max(0)),
                None => self.watch_index + 1,
            };
        } else if key == "home" {
            self.watch_index = 0;
        } else if key == "end" {
            self.watch_index = ((self.watch_count.unwrap_or(1)) - 1).max(0);
        } else if key == "page_up" {
            self.watch_offset = if self.watch_count.is_some() {
                self.watch_previous_offset.unwrap_or(self.watch_offset)
            } else {
                (self.watch_offset - self.watch_page_size).max(0)
            };
            self.watch_index = 0;
        } else if key == "page_down" {
            if self.watch_count.is_none() || self.watch_next_offset.is_some() {
                self.watch_offset = self
                    .watch_next_offset
                    .unwrap_or(self.watch_offset + self.watch_page_size);
                self.watch_index = 0;
            }
        } else if key == "char:f" && FILTERABLE_SECTIONS.contains(&self.watch_section.as_str()) {
            self.filter_draft = self.watch_query.clone();
            self.view = "watch_filter".into();
        } else if key == "char:c" && FILTERABLE_SECTIONS.contains(&self.watch_section.as_str()) {
            self.watch_query.clear();
            self.watch_offset = 0;
            self.watch_index = 0;
        } else if key == "char:s" && self.watch_section == "findings" {
            const CHOICES: [&str; 4] = ["severity", "check", "target_url", "id"];
            let pos = CHOICES
                .iter()
                .position(|c| *c == self.watch_sort)
                .unwrap_or(0);
            self.watch_sort = CHOICES[(pos + 1) % CHOICES.len()].into();
            self.watch_offset = 0;
            self.watch_index = 0;
        } else if key == "char:t"
            && ["tasks", "methods", "schema"].contains(&self.watch_section.as_str())
        {
            let choices: [Option<&str>; 7] = [
                None,
                Some("running"),
                Some("blocked"),
                Some("review"),
                Some("remaining"),
                Some("stale"),
                Some("completed"),
            ];
            let pos = choices
                .iter()
                .position(|c| *c == self.watch_status.as_deref())
                .unwrap_or(0);
            self.watch_status = choices[(pos + 1) % choices.len()].map(str::to_string);
            self.watch_offset = 0;
            self.watch_index = 0;
        } else if key == "char:r" && self.watch_section == "findings" {
            self.watch_descending = !self.watch_descending;
            self.watch_offset = 0;
            self.watch_index = 0;
        } else if key == "enter" && self.watch_section != "overview" {
            self.watch_detail_offset = 0;
            self.watch_view_offset = 0;
            self.watch_detail_kind = match self.watch_section.as_str() {
                "findings" => "finding",
                "scans" => "scan",
                "views" => "view",
                "inbox" => "inbox",
                "tasks" | "methods" | "schema" => "task",
                "sites" => "site",
                other => other,
            }
            .into();
            self.view = "watch_detail".into();
        } else if matches!(key, "escape" | "char:q" | "ctrl_c") {
            self.quit_requested = true;
        }
    }

    fn key_detail(&mut self, key: &str) {
        self.scroll_detail(key, "palette");
    }

    fn key_palette(&mut self, key: &str) {
        if key == "up" {
            self.move_by(-1);
        } else if key == "down" {
            self.move_by(1);
        } else if key == "page_up" {
            self.move_by(-10);
        } else if key == "page_down" {
            self.move_by(10);
        } else if key == "home" {
            self.jump(0);
        } else if key == "end" {
            self.jump(self.filtered().len() as i64 - 1);
        } else if key == "enter" {
            self.open_selected();
        } else if key == "escape" {
            self.quit_requested = !self.back();
        } else if key == "backspace" {
            self.backspace();
        } else if key == "ctrl_c" {
            self.quit_requested = true;
        } else if key == "char:?" {
            self.view = "help".into();
        } else if key == "char:q" && self.query.is_empty() {
            self.quit_requested = true;
        } else if let Some(c) = char_of(key) {
            if is_printable_py(c) {
                self.type_char(c);
            }
        }
    }

    /// `_scroll_detail` — scroll a read-only detail page; escape/enter/ctrl_c
    /// leave it for `exit_view`.
    fn scroll_detail(&mut self, key: &str, exit_view: &str) {
        match key {
            "down" | "page_down" => self.watch_detail_offset += if key == "down" { 1 } else { 10 },
            "up" | "page_up" => {
                self.watch_detail_offset =
                    (self.watch_detail_offset - if key == "up" { 1 } else { 10 }).max(0)
            }
            "home" => self.watch_detail_offset = 0,
            "end" => self.watch_detail_offset = 1_000_000_000,
            _ => {}
        }
        if matches!(key, "escape" | "enter" | "ctrl_c") {
            self.view = exit_view.into();
            self.quit_requested = key == "ctrl_c";
        }
    }
}

/// `key[5:]` when `key.startswith("char:")`.
fn char_of(key: &str) -> Option<&str> {
    key.strip_prefix("char:")
}

/// Splice `with` into `text` replacing `text[start..end]` (character offsets).
fn splice_chars(text: &mut String, start: usize, end: usize, with: &str) {
    let b0 = byte_of(text, start);
    let b1 = byte_of(text, end);
    text.replace_range(b0..b1, with);
}

/// Byte offset of character index `n` (end of string when `n >= len`).
fn byte_of(text: &str, n: usize) -> usize {
    text.char_indices()
        .nth(n)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}
