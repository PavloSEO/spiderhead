//! The egui shell: navigation, profile menu, status bar and the screens the
//! control commands drive. Every widget action goes through
//! `AppModel::dispatch` so the control channel and the window never diverge.

pub mod checks;
pub mod grid;
pub mod kit;
pub mod menubar;
pub mod modes;
pub mod settings;
pub mod shell;
pub mod shot;

use std::sync::{Arc, Mutex};

use eframe::egui;
use serde_json::json;

use crate::app::{AppModel, EditAction, Effect};
use crate::theme::Theme;

/// Pending shell work produced by dispatches (effects the model cannot run).
#[derive(Default)]
pub struct Effects {
    /// Viewport screenshot requests, FIFO (paths to write PNGs into).
    pub screenshots: std::collections::VecDeque<std::path::PathBuf>,
    /// Quit requested by `app.quit`.
    pub quit: bool,
    /// Minimize requested by `window.minimize`.
    pub minimize: bool,
    /// Fullscreen toggle requested by `window.fullscreen`.
    pub fullscreen: bool,
    /// Inner size requested by `window.resize`.
    pub resize: Option<[f32; 2]>,
    /// Editing operations on the focused field, FIFO (`edit.clipboard`).
    pub edits: std::collections::VecDeque<EditAction>,
    /// Native open-file dialog requested by `scan.open {"dialog":true}`.
    pub pick_scan: bool,
}

/// The eframe application: model + shared effects queue.
pub struct DesktopApp {
    /// The command surface and all state.
    pub model: AppModel,
    /// Effects the last dispatch produced.
    pub effects: Arc<Mutex<Effects>>,
    /// Filter text box content.
    pub filter_text: String,
    /// Widget-side view state (details tab, caches).
    pub view: modes::ViewState,
    /// Control requests the listener thread queues for the UI thread.
    pub requests: Option<std::sync::mpsc::Receiver<spiderhead_control::server::ControlRequest>>,
    /// The last screenshot request awaiting a viewport reply.
    shot_pending: Option<std::path::PathBuf>,
    /// The native menu goes in on the first frame (see `menubar::install`).
    menu_installed: bool,
    /// Theme whose visuals egui currently holds (`None` before the first frame).
    applied: Option<crate::theme::Theme>,
    /// Fonts were registered (first frame draws nothing).
    fonts_installed: bool,
}

impl DesktopApp {
    /// Create the app; `model` may already hold an opened scan (autostart).
    pub fn new(model: AppModel, effects: Arc<Mutex<Effects>>) -> Self {
        Self {
            model,
            effects,
            filter_text: String::new(),
            view: modes::ViewState::default(),
            requests: None,
            shot_pending: None,
            menu_installed: false,
            applied: None,
            fonts_installed: false,
        }
    }

    /// Dispatch a command from a widget and queue its effects.
    pub fn act(&mut self, method: &str, params: serde_json::Value) -> Option<serde_json::Value> {
        let outcome = self.model.dispatch(method, &params);
        self.queue_effects(outcome.effects);
        match outcome.answer {
            Ok(v) => Some(v),
            Err(e) => {
                // Control answers carry {"error","reason_code"} JSON — the
                // status strip wants the human line, the channel keeps the
                // structured shape.
                self.model.status = serde_json::from_str::<serde_json::Value>(&e)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|m| m.as_str()).map(str::to_string))
                    .unwrap_or(e);
                None
            }
        }
    }

    /// Queue shell work a dispatch produced (widget or control path alike).
    fn queue_effects(&mut self, effects: Vec<Effect>) {
        let mut queue = self.effects.lock().unwrap_or_else(|p| p.into_inner());
        for effect in effects {
            match effect {
                Effect::SetTheme(_) => {}
                Effect::Quit => queue.quit = true,
                Effect::Screenshot(path) => queue.screenshots.push_back(path),
                Effect::Minimize => queue.minimize = true,
                Effect::Fullscreen => queue.fullscreen = true,
                Effect::Resize(size) => queue.resize = Some(size),
                Effect::Edit(op) => queue.edits.push_back(op),
                Effect::PickScan => queue.pick_scan = true,
            }
        }
    }

    /// Drain queued control requests; answers go straight back to clients.
    fn serve_requests(&mut self) {
        let Some(rx) = self.requests.take() else {
            return;
        };
        while let Ok(req) = rx.try_recv() {
            let outcome = self.model.dispatch(&req.method, &req.params);
            self.queue_effects(outcome.effects);
            let _ = req.reply.send(outcome.answer);
        }
        self.requests = Some(rx);
    }

    /// Drain queued native-menu events through the same dispatch surface.
    /// A menu item carrying a TSV payload (`table.copy`) lands on the
    /// clipboard, the same place its widget counterpart puts it.
    fn serve_menu(&mut self, ctx: &egui::Context) {
        for (method, params) in menubar::drain() {
            if let Some(v) = self.act(method, params) {
                if let Some(tsv) = v.get("tsv").and_then(|t| t.as_str()) {
                    ctx.copy_text(tsv.to_string());
                    self.model.status = crate::i18n::TSV_COPIED.into();
                }
                if let Some(text) = v.get("clipboard").and_then(|t| t.as_str()) {
                    ctx.copy_text(text.to_string());
                }
            }
        }
    }

    /// Perform the queued shell work the model cannot do itself.
    fn run_effects(&mut self, ctx: &egui::Context) {
        let mut effects = self.effects.lock().unwrap_or_else(|p| p.into_inner());
        // Dequeue pending screenshots → ask the viewport for a capture, one
        // at a time and FIFO: a second request must wait for the reply, not
        // overwrite `shot_pending` and lose the first path silently.
        if self.shot_pending.is_none() {
            if let Some(path) = effects.screenshots.pop_front() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                self.shot_pending = Some(path);
            }
        }
        if effects.minimize {
            effects.minimize = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if let Some([w, h]) = effects.resize.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
        }
        if effects.fullscreen {
            effects.fullscreen = false;
            let on = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!on));
        }
        while let Some(op) = effects.edits.pop_front() {
            match op {
                EditAction::Cut => ctx.send_viewport_cmd(egui::ViewportCommand::RequestCut),
                EditAction::Copy => ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy),
                EditAction::Paste => ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste),
                // Undo/select-all reach the focused field as ordinary key
                // events, the same way winit reports ⌘Z / ⌘A.
                EditAction::Undo | EditAction::SelectAll => {
                    let key = if op == EditAction::Undo {
                        egui::Key::Z
                    } else {
                        egui::Key::A
                    };
                    ctx.input_mut(|i| {
                        i.events.push(egui::Event::Key {
                            key,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::COMMAND,
                        });
                    });
                }
            }
        }
        if effects.pick_scan {
            effects.pick_scan = false;
            drop(effects);
            let file = rfd::FileDialog::new()
                .add_filter("scan", &["sqlite"])
                .pick_file();
            if let Some(path) = file {
                self.act("scan.open", json!({"path": path.display().to_string()}));
                self.act("ui.navigate", json!({"id": "urls"}));
            }
            return;
        }
        if effects.quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for DesktopApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.menu_installed {
            self.menu_installed = true;
            menubar::install();
        }
        self.serve_requests();
        self.serve_menu(ctx);
        if !self.fonts_installed {
            // Fonts take effect next frame; styles naming them must wait.
            crate::theme::install_fonts(ctx);
            self.fonts_installed = true;
            ctx.request_repaint();
            return;
        }
        self.model.system_dark = ctx.system_theme() == Some(egui::Theme::Dark);
        if self.model.settings.get_str("view.theme") == "system" {
            self.model.theme = if self.model.system_dark {
                Theme::Dark
            } else {
                Theme::Light
            };
        }
        if self.applied != Some(self.model.theme) {
            crate::theme::apply(ctx, self.model.theme);
            // Native chrome follows the app theme: NSAppearance on macOS,
            // the title-bar dark mode elsewhere.
            let system = match self.model.theme {
                crate::theme::Theme::Dark => egui::SystemTheme::Dark,
                _ => egui::SystemTheme::Light,
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(system));
            self.applied = Some(self.model.theme);
        }

        // Shell hotkeys: ⌘K palette, F1 help, ⌘, settings, ⌘B nav rail. On
        // macOS the menu bar owns these accelerators; the egui fallback
        // covers other OSes.
        let (palette, help, settings, nav_fold) = ctx.input(|i| {
            (
                i.modifiers.command && i.key_pressed(egui::Key::K),
                i.key_pressed(egui::Key::F1),
                i.modifiers.command && i.key_pressed(egui::Key::Comma),
                i.modifiers.command && i.key_pressed(egui::Key::B),
            )
        });
        if palette {
            let open = self.model.windows.contains("palette");
            self.act("ui.window", json!({"id": "palette", "open": !open}));
        }
        if help {
            self.act("ui.window", json!({"id": "help"}));
        }
        if settings {
            self.act("ui.window", json!({"id": "settings"}));
        }
        if nav_fold {
            self.act(
                "ui.sidebar",
                json!({"collapsed": !self.model.nav_collapsed}),
            );
        }

        // F5 reloads the table — in any product mode, never on the chooser.
        if self.model.mode.is_some() && ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            self.act("table.filter", json!({"reload": true}));
        }

        self.run_effects(ctx);
        if let Some(image) = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        }) {
            if let Some(path) = self.shot_pending.take() {
                if let Err(e) = shot::encode_png(&image).and_then(|png| std::fs::write(&path, png))
                {
                    self.model.status = format!("screenshot failed: {e}");
                }
            }
        }

        match self.model.mode {
            None => modes::start::show(ctx, self),
            Some(_) => {
                shell::top_bar(ctx, self);
                shell::status_bar(ctx, self);
                shell::navigation(ctx, self);
                shell::windows(ctx, self);
                let p = self.model.theme.palette();
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .fill(p.bg)
                            .inner_margin(egui::Margin::same(crate::theme::space::LG as i8)),
                    )
                    .show(ctx, |ui| modes::show(ui, self));
            }
        }
    }
}
