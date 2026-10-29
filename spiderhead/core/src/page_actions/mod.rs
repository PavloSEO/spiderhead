//! Validation and bounded execution of rendered-crawl page actions.
//!
//! A page action is data, never code: the closed set is `click`, `scroll`, `wait_for_selector`,
//! `wait_ms` and selector-based `extract`. Each step carries only its declared fields, selectors must
//! compile as CSS, and the static time budget of a whole action list is bounded up front. The renderer
//! consumes canonical steps and stores bounded results per URL.
//!
//! The browser is reached only through [`ActionPage`]. CSS validation uses `scraper` (servo selectors).

mod scripts;

#[cfg(test)]
mod tests;

use scraper::Selector;
use serde_json::{json, Map, Value};
use thiserror::Error;

use scripts::{CLICK_PROBE_JS, DATALAYER_JS, EXTRACT_JS, SCROLL_JS};

/// Evidence schema tag.
pub const VERSION: &str = "page_actions.v1";
/// Most custom steps one action list may carry.
pub const MAX_STEPS: usize = 20;
/// Longest selector, in characters.
pub const MAX_SELECTOR_CHARS: usize = 512;
/// Largest per-step timeout, in milliseconds.
pub const MAX_STEP_TIMEOUT_MS: i64 = 30_000;
/// Largest `wait_ms` value, in milliseconds.
pub const MAX_WAIT_MS: i64 = 10_000;
/// Largest scroll distance, in pixels.
pub const MAX_SCROLL_PIXELS: i64 = 20_000;
/// Static time budget of a whole action list, in milliseconds.
pub const MAX_TOTAL_BUDGET_MS: u64 = 60_000;
/// Named presets a list may use, each at most once.
pub const PRESETS: &[&str] = &["expand_tabs", "load_more", "read_datalayer"];
/// Clicks a click-preset expands at most.
pub const MAX_PRESET_CLICKS: u64 = 5;
/// Budget charged to one scroll.
pub const SCROLL_BUDGET_MS: u64 = 2_000;
/// Budget charged to one extract.
pub const EXTRACT_TIMEOUT_MS: u64 = 10_000;
/// Budget charged to one `read_datalayer`.
pub const READ_DATALAYER_TIMEOUT_MS: u64 = 5_000;
/// Output cap for one `read_datalayer` payload, in characters.
pub const MAX_DATALAYER_CHARS: u64 = 16_000;
/// Node cap for one `read_datalayer` payload.
pub const MAX_DATALAYER_NODES: u64 = 1_000;
/// Longest single extracted value, in characters.
pub const MAX_EXTRACT_VALUE_CHARS: u64 = 2_000;
/// Total characters one run may extract.
pub const MAX_EXTRACT_TOTAL_CHARS: u64 = 16_000;
/// A click has no declared wait, so it is charged this fixed budget.
pub const CLICK_BUDGET_MS: u64 = 5_000;

const CLICK_SELECTOR_TAB_BUTTON: &str =
    r#"[role="tab"][aria-selected="false"],button[aria-expanded="false"]"#;
const CLICK_SELECTOR_LOAD_MORE: &str =
    r#"button[data-load-more],button[aria-label*="load more" i]"#;

/// A rejected action list or preset list, with the reason as its message.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{0}")]
pub struct PageActionError(pub String);

/// A failure raised by the browser driver, carrying the exception class name the evidence
/// records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverError {
    /// Exception class name, for example `TimeoutError`.
    pub kind: String,
}

impl DriverError {
    /// A driver failure with the given class name.
    pub fn new(kind: &str) -> Self {
        Self {
            kind: kind.to_owned(),
        }
    }
}

/// The browser surface a run needs. The renderer implements it over CDP; tests use fakes.
pub trait ActionPage {
    /// Click the first element matching `selector`, bounded by `timeout_ms`.
    fn click_first(&mut self, selector: &str, timeout_ms: u64) -> Result<(), DriverError>;
    /// Wait until `selector` matches, bounded by `timeout_ms`.
    fn wait_for_selector(&mut self, selector: &str, timeout_ms: u64) -> Result<(), DriverError>;
    /// Wait a fixed number of milliseconds.
    fn wait_for_timeout(&mut self, ms: u64) -> Result<(), DriverError>;
    /// Run a fixed script with one JSON argument and return its JSON result.
    fn evaluate(
        &mut self,
        script: &str,
        argument: Value,
        timeout_ms: u64,
    ) -> Result<Value, DriverError>;
}

/// A canonical, validated step. Only the declared fields exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Click the first matching element when it is safe to click.
    Click {
        /// CSS selector.
        selector: String,
    },
    /// Scroll the window by `pixels`.
    Scroll {
        /// Distance in pixels.
        pixels: i64,
    },
    /// Wait for a selector to appear.
    WaitForSelector {
        /// CSS selector.
        selector: String,
        /// Per-step timeout in milliseconds.
        timeout_ms: i64,
    },
    /// Wait a fixed time.
    WaitMs {
        /// Wait length in milliseconds.
        ms: i64,
    },
    /// Read `text`, `href` or `value` from every match.
    Extract {
        /// CSS selector.
        selector: String,
        /// One of `text`, `href`, `value`.
        attribute: String,
    },
    /// Read the bounded tail of `window.dataLayer` (preset only).
    ReadDatalayer,
}

impl Step {
    /// The action name as recorded in evidence.
    pub fn kind(&self) -> &'static str {
        match self {
            Step::Click { .. } => "click",
            Step::Scroll { .. } => "scroll",
            Step::WaitForSelector { .. } => "wait_for_selector",
            Step::WaitMs { .. } => "wait_ms",
            Step::Extract { .. } => "extract",
            Step::ReadDatalayer => "read_datalayer",
        }
    }

    /// The canonical JSON object for this step.
    pub fn to_value(&self) -> Value {
        match self {
            Step::Click { selector } => json!({"action": "click", "selector": selector}),
            Step::Scroll { pixels } => json!({"action": "scroll", "pixels": pixels}),
            Step::WaitForSelector {
                selector,
                timeout_ms,
            } => {
                json!({"action": "wait_for_selector", "selector": selector, "timeout_ms": timeout_ms})
            }
            Step::WaitMs { ms } => json!({"action": "wait_ms", "ms": ms}),
            Step::Extract {
                selector,
                attribute,
            } => json!({"action": "extract", "selector": selector, "attribute": attribute}),
            Step::ReadDatalayer => json!({"action": "read_datalayer"}),
        }
    }

    /// Maximum wall time for this step, in milliseconds.
    fn budget_ms(&self) -> u64 {
        match self {
            Step::Click { .. } => CLICK_BUDGET_MS,
            Step::Scroll { .. } => SCROLL_BUDGET_MS,
            Step::WaitForSelector { timeout_ms, .. } => to_u64(*timeout_ms),
            Step::WaitMs { ms } => to_u64(*ms),
            Step::Extract { .. } => EXTRACT_TIMEOUT_MS,
            Step::ReadDatalayer => READ_DATALAYER_TIMEOUT_MS,
        }
    }
}

fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn err(message: &str) -> PageActionError {
    PageActionError(message.to_owned())
}

/// The field names each action accepts, exactly.
fn fields_of(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "click" => Some(&["action", "selector"]),
        "scroll" => Some(&["action", "pixels"]),
        "wait_for_selector" => Some(&["action", "selector", "timeout_ms"]),
        "wait_ms" => Some(&["action", "ms"]),
        "extract" => Some(&["action", "selector", "attribute"]),
        _ => None,
    }
}

fn selector(value: Option<&Value>) -> Result<String, PageActionError> {
    let text = match value.and_then(Value::as_str) {
        Some(text) if (1..=MAX_SELECTOR_CHARS).contains(&text.chars().count()) => text,
        _ => return Err(err("page action selector is invalid")),
    };
    if Selector::parse(text).is_err() {
        return Err(err("page action CSS selector is invalid"));
    }
    Ok(text.to_owned())
}

/// A JSON integer within `low..=high`; floats and booleans are refused.
fn integer(
    value: Option<&Value>,
    low: i64,
    high: i64,
    message: &str,
) -> Result<i64, PageActionError> {
    match value.and_then(Value::as_i64) {
        Some(number) if (low..=high).contains(&number) => Ok(number),
        _ => Err(err(message)),
    }
}

/// Validate an action list and return `(canonical_steps, static_budget_ms)`.
pub fn validate_actions(value: &Value) -> Result<(Vec<Step>, u64), PageActionError> {
    let list = match value.as_array() {
        Some(list) if list.len() <= MAX_STEPS => list,
        _ => return Err(err("page actions must be a bounded list")),
    };
    let mut steps = Vec::with_capacity(list.len());
    let mut budget: u64 = 0;
    for raw in list {
        let object = raw
            .as_object()
            .filter(|o| o.get("action").and_then(Value::as_str).is_some())
            .ok_or_else(|| err("page action must be an object with an action name"))?;
        let kind = object
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let fields = fields_of(kind).ok_or_else(|| err("page action is not in the closed set"))?;
        if object.len() != fields.len() || !fields.iter().all(|f| object.contains_key(*f)) {
            return Err(err("page action has unsupported fields"));
        }
        let step = parse_step(kind, object, &mut budget)?;
        steps.push(step);
    }
    if budget > MAX_TOTAL_BUDGET_MS {
        return Err(err("page actions exceed the static time budget"));
    }
    Ok((steps, budget))
}

fn parse_step(
    kind: &str,
    object: &Map<String, Value>,
    budget: &mut u64,
) -> Result<Step, PageActionError> {
    match kind {
        "click" => {
            let selector = selector(object.get("selector"))?;
            *budget += CLICK_BUDGET_MS;
            Ok(Step::Click { selector })
        }
        "scroll" => {
            let pixels = integer(
                object.get("pixels"),
                1,
                MAX_SCROLL_PIXELS,
                "scroll pixels must be 1..20000",
            )?;
            *budget += SCROLL_BUDGET_MS;
            Ok(Step::Scroll { pixels })
        }
        "wait_for_selector" => {
            let timeout_ms = integer(
                object.get("timeout_ms"),
                1,
                MAX_STEP_TIMEOUT_MS,
                "step timeout is out of range",
            )?;
            let selector = selector(object.get("selector"))?;
            *budget += to_u64(timeout_ms);
            Ok(Step::WaitForSelector {
                selector,
                timeout_ms,
            })
        }
        "wait_ms" => {
            let ms = integer(object.get("ms"), 0, MAX_WAIT_MS, "wait_ms must be 0..10000")?;
            *budget += to_u64(ms);
            Ok(Step::WaitMs { ms })
        }
        _ => {
            let attribute = object
                .get("attribute")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !matches!(attribute, "text" | "href" | "value") {
                return Err(err("extract attribute must be text, href, or value"));
            }
            let selector = selector(object.get("selector"))?;
            *budget += EXTRACT_TIMEOUT_MS;
            Ok(Step::Extract {
                selector,
                attribute: attribute.to_owned(),
            })
        }
    }
}

/// Expand the named presets into the closed, validated action set.
pub fn preset_actions(value: &Value) -> Result<(Vec<Step>, u64), PageActionError> {
    let list = match value.as_array() {
        Some(list) if list.len() <= PRESETS.len() => list,
        _ => return Err(err("page action presets must be a bounded list")),
    };
    let mut names: Vec<&str> = Vec::with_capacity(list.len());
    for item in list {
        match item.as_str() {
            Some(name) if PRESETS.contains(&name) => names.push(name),
            _ => return Err(err("unknown page action preset")),
        }
    }
    let mut unique = names.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != names.len() {
        return Err(err("page action presets must be unique"));
    }
    let mut steps = Vec::new();
    let mut budget: u64 = 0;
    for name in names {
        if name == "expand_tabs" {
            steps.extend((0..MAX_PRESET_CLICKS).map(|_| Step::Click {
                selector: CLICK_SELECTOR_TAB_BUTTON.to_owned(),
            }));
        } else if name == "load_more" {
            steps.extend((0..MAX_PRESET_CLICKS).map(|_| Step::Click {
                selector: CLICK_SELECTOR_LOAD_MORE.to_owned(),
            }));
        } else {
            steps.push(Step::ReadDatalayer);
            budget += READ_DATALAYER_TIMEOUT_MS;
        }
    }
    // Presets have their own strict small cap; custom actions use MAX_STEPS.
    let click_count = steps
        .iter()
        .filter(|s| matches!(s, Step::Click { .. }))
        .count() as u64;
    budget += click_count * CLICK_BUDGET_MS;
    if budget > MAX_TOTAL_BUDGET_MS {
        return Err(err("page action presets exceed the static time budget"));
    }
    Ok((steps, budget))
}

mod run;
pub use run::run_actions;
