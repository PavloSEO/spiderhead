//! Rendering-section validation.

use super::*;

/// Validates the `rendering` section: mode, page actions, screenshot mode and link flags.
pub(super) fn validate_rendering(rendering: &Value) -> Result<(), ConfigError> {
    let mode = at(rendering, "mode").as_str().unwrap_or_default();
    if !RENDER_MODES.contains(&mode) {
        return err(format!(
            "rendering.mode must be one of {RENDER_MODES:?}, got {mode:?}"
        ));
    }
    let actions = rendering
        .get("actions")
        .cloned()
        .unwrap_or_else(|| Value::Array(vec![]));
    let presets = rendering
        .get("presets")
        .cloned()
        .unwrap_or_else(|| Value::Array(vec![]));
    let (steps, action_budget) = page_actions::validate_actions(&actions)
        .map_err(|e| ConfigError(format!("rendering page actions: {}", e.0)))?;
    let (preset_steps, preset_budget) = page_actions::preset_actions(&presets)
        .map_err(|e| ConfigError(format!("rendering page actions: {}", e.0)))?;
    if steps.len() + preset_steps.len() > 20 || action_budget + preset_budget > 60_000 {
        return err("rendering page actions: page actions exceed the combined step or time budget");
    }

    let escalation = at(rendering, "escalation");
    let screenshot = rendering
        .pointer("/artifacts/screenshot_mode")
        .and_then(Value::as_str)
        .unwrap_or("full_page");
    if screenshot != "full_page" && screenshot != "first_screen" {
        return err("rendering.artifacts.screenshot_mode must be full_page or first_screen");
    }
    if !at(rendering, "rendered_links.store").is_boolean() {
        return err("rendering.rendered_links.store must be a boolean");
    }
    if !at(rendering, "rendered_links.crawl").is_boolean() {
        return err("rendering.rendered_links.crawl must be a boolean");
    }
    let policy = escalation
        .get("policy")
        .and_then(Value::as_str)
        .unwrap_or("sampled");
    if !["sampled", "full", "auto"].contains(&policy) {
        return err("rendering.escalation.policy must be sampled, full or auto");
    }
    if at(escalation, "sample_per_pattern").as_i64().unwrap_or(0) < 1 {
        return err("rendering.escalation.sample_per_pattern must be at least 1");
    }
    if at(escalation, "max_render_urls").as_i64().unwrap_or(0) < 0 {
        return err("rendering.escalation.max_render_urls cannot be negative");
    }
    if at(escalation, "max_render_seconds").as_f64().unwrap_or(0.0) < 0.0 {
        return err("rendering.escalation.max_render_seconds cannot be negative");
    }

    let browser = at(rendering, "browser");
    let Some(browser_map) = browser.as_object() else {
        return err("rendering.browser must be an object");
    };
    crate::browser_transport::validate_config(browser_map, true)
        .map_err(|e| ConfigError(format!("rendering.browser: {}", e.message)))?;
    let viewport = at(browser, "viewport").as_str().unwrap_or_default();
    if !RENDER_VIEWPORTS.contains(&viewport) {
        return err(format!(
            "rendering.browser.viewport must be one of {RENDER_VIEWPORTS:?}, got {viewport:?}"
        ));
    }
    let engine = at(browser, "engine").as_str().unwrap_or_default();
    if !RENDER_ENGINES.contains(&engine) {
        return err(format!(
            "rendering.browser.engine must be one of {RENDER_ENGINES:?}, got {engine:?}"
        ));
    }
    if !RENDER_MOBILE_EMULATION_ENGINES.contains(&engine)
        && at(browser, "mobile_emulation").as_bool() == Some(true)
    {
        return err(format!(
            "rendering.browser.engine={engine:?} cannot emulate a mobile viewport (Playwright does not implement is_mobile for it); unset mobile_emulation or pick another engine"
        ));
    }
    for name in ["viewport_width", "viewport_height"] {
        match at(browser, name).as_i64() {
            Some(n) if (0..=MAX_VIEWPORT_DIMENSION_PX).contains(&n) => {}
            _ => {
                return err(format!(
                    "rendering.browser.{name} must be an integer between 0 and {MAX_VIEWPORT_DIMENSION_PX}, got {}",
                    at(browser, name)
                ))
            }
        }
    }
    let width = at(browser, "viewport_width").as_i64().unwrap_or(0);
    let height = at(browser, "viewport_height").as_i64().unwrap_or(0);
    if (width > 0) != (height > 0) {
        return err(
            "rendering.browser.viewport_width and rendering.browser.viewport_height must be set together (both >0 or both 0)",
        );
    }
    let concurrency = at(browser, "page_concurrency").as_i64();
    match concurrency {
        Some(n) if (1..=MAX_RENDER_PAGE_CONCURRENCY).contains(&n) => {}
        _ => {
            return err(format!(
                "rendering.browser.page_concurrency must be an integer between 1 and {MAX_RENDER_PAGE_CONCURRENCY}, got {}",
                at(browser, "page_concurrency")
            ))
        }
    }
    let wait = at(browser, "wait_until").as_str().unwrap_or_default();
    if !RENDER_WAIT_UNTIL.contains(&wait) {
        return err(format!(
            "rendering.browser.wait_until must be one of {RENDER_WAIT_UNTIL:?}, got {wait:?}"
        ));
    }
    if at(browser, "script_timeout_seconds")
        .as_f64()
        .unwrap_or(0.0)
        < 0.0
    {
        return err("rendering.browser.script_timeout_seconds cannot be negative");
    }
    if at(browser, "resize_to_content_max_height_px")
        .as_i64()
        .unwrap_or(0)
        < 1
    {
        return err("rendering.browser.resize_to_content_max_height_px must be at least 1");
    }
    if at(browser, "device_pixel_ratio").as_f64().unwrap_or(0.0) <= 0.0 {
        return err("rendering.browser.device_pixel_ratio must be positive");
    }
    let persistent = at(browser, "persistent_profile").as_bool() == Some(true);
    let has_dir = at(browser, "persistent_profile_dir")
        .as_str()
        .is_some_and(|d| !d.is_empty());
    if persistent && !has_dir {
        return err(
            "rendering.browser.persistent_profile is true but persistent_profile_dir is empty; a persistent profile must name a directory explicitly",
        );
    }
    Ok(())
}
