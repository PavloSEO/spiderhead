//! Where control-channel methods end up.

use serde_json::Value;

/// Something that answers control-channel methods (`commands.list`, `window.list`,
/// `ui.inspect`, `scan.open`, …).
pub trait Backend {
    /// Call one method. `Ok` carries the `result`, `Err` the error message.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String>;
    /// True when a real UI is attached (`ui.screenshot` and dialogs work).
    fn has_ui(&self) -> bool;
    /// Short human description ("headless", "connected to 127.0.0.1:7847").
    fn describe(&self) -> String;
}
