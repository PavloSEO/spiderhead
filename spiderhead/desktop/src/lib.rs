//! Library side of the SpiderHead desktop: the application model, the read-only
//! scan data layer and the command registry that both the window and the control
//! channel drive. Nothing here paints pixels; `main.rs` adds the egui shell.

#![forbid(unsafe_code)]

pub mod app;
pub mod check_titles;
pub mod commands;
pub mod data;
pub mod i18n;
pub mod integrate;
pub mod menus;
pub mod sample;
pub mod settings;
pub mod theme;
pub mod ui;
