//! Tests for the action validator and runner, using an in-memory page fake.
//! The page scripts are compared against the constants in `scripts.rs`.

use super::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// In-memory page: records calls and answers the three scripts the runner sends.
struct FakePage {
    clock: Option<Rc<Cell<f64>>>,
    click_safe: bool,
    clicks: RefCell<Vec<String>>,
    scrolls: RefCell<Vec<i64>>,
    waits: RefCell<Vec<u64>>,
    extract_budgets: RefCell<Vec<u64>>,
    extract_responses: RefCell<Vec<Value>>,
}

impl FakePage {
    fn new() -> Self {
        Self {
            clock: None,
            click_safe: true,
            clicks: RefCell::new(Vec::new()),
            scrolls: RefCell::new(Vec::new()),
            waits: RefCell::new(Vec::new()),
            extract_budgets: RefCell::new(Vec::new()),
            extract_responses: RefCell::new(Vec::new()),
        }
    }
}

impl ActionPage for FakePage {
    fn click_first(&mut self, selector: &str, timeout_ms: u64) -> Result<(), DriverError> {
        // Asserts the 5000 ms click budget.
        assert_eq!(timeout_ms, 5000);
        self.clicks.borrow_mut().push(selector.to_owned());
        Ok(())
    }

    fn wait_for_selector(&mut self, _selector: &str, timeout_ms: u64) -> Result<(), DriverError> {
        self.waits.borrow_mut().push(timeout_ms);
        Ok(())
    }

    fn wait_for_timeout(&mut self, ms: u64) -> Result<(), DriverError> {
        self.waits.borrow_mut().push(ms);
        if let Some(clock) = &self.clock {
            clock.set(clock.get() + ms as f64 / 1000.0);
        }
        Ok(())
    }

    fn evaluate(
        &mut self,
        script: &str,
        argument: Value,
        timeout_ms: u64,
    ) -> Result<Value, DriverError> {
        assert!(timeout_ms > 0);
        if script == CLICK_PROBE_JS {
            return Ok(json!({"count": 1, "safe": self.click_safe}));
        }
        if script == EXTRACT_JS {
            self.extract_budgets
                .borrow_mut()
                .push(argument["max_total_chars"].as_u64().unwrap_or_default());
            if let Some(response) = self.extract_responses.borrow_mut().pop() {
                return Ok(response);
            }
            return Ok(json!({"values": ["/one", "/two"], "omitted": 0}));
        }
        assert_eq!(script, SCROLL_JS);
        self.scrolls
            .borrow_mut()
            .push(argument.as_i64().unwrap_or_default());
        Ok(Value::Null)
    }
}

#[test]
fn closed_set_is_canonicalized_and_budgeted() {
    let input = json!([
        {"action": "click", "selector": "button.more"},
        {"action": "scroll", "pixels": 800},
        {"action": "wait_for_selector", "selector": ".item", "timeout_ms": 2000},
        {"action": "wait_ms", "ms": 500},
    ]);
    let (steps, budget) = validate_actions(&input).expect("valid list");
    assert_eq!(
        steps[0].to_value(),
        json!({"action": "click", "selector": "button.more"})
    );
    assert_eq!(steps[3].to_value(), json!({"action": "wait_ms", "ms": 500}));
    assert_eq!(budget, CLICK_BUDGET_MS + SCROLL_BUDGET_MS + 2000 + 500);
}

#[test]
fn rejects_out_of_contract_input() {
    let bad_cases: Vec<Value> = vec![
        json!("click"),
        json!([{"action": "evaluate", "script": "1"}]),
        json!([{"action": "click", "selector": "a", "extra": true}]),
        json!([{"action": "click", "selector": "a:::bad"}]),
        json!([{"action": "click", "selector": ""}]),
        json!([{"action": "scroll", "pixels": 0}]),
        json!([{"action": "scroll", "pixels": true}]),
        json!([{"action": "wait_ms", "ms": 10_001}]),
        json!([{"action": "wait_for_selector", "selector": "a", "timeout_ms": 30_001}]),
        json!([{"action": "click", "selector": "a", "regex": "x"}]),
        json!([{"action": "click"}]),
        json!([{"selector": "a"}]),
        Value::Array(vec![
            json!({"action": "click", "selector": "a"});
            MAX_STEPS + 1
        ]),
    ];
    for bad in bad_cases {
        assert!(validate_actions(&bad).is_err(), "accepted {bad}");
    }
}

#[test]
fn rejects_when_static_budget_exceeds_limit() {
    let steps = Value::Array(vec![json!({"action": "wait_ms", "ms": 10_000}); 7]);
    let error = validate_actions(&steps).expect_err("over budget");
    assert!(error.0.contains("static time budget"), "{error}");
}

#[test]
fn empty_list_is_valid() {
    assert_eq!(validate_actions(&json!([])), Ok((Vec::new(), 0)));
}

#[test]
fn extract_is_selector_based_and_has_no_script_field() {
    let (steps, budget) = validate_actions(
        &json!([{"action": "extract", "selector": "a.product", "attribute": "href"}]),
    )
    .expect("valid extract");
    assert_eq!(
        steps.iter().map(Step::to_value).collect::<Vec<_>>(),
        vec![json!({"action": "extract", "selector": "a.product", "attribute": "href"})]
    );
    assert!(budget > 0);
    let error = validate_actions(&json!([
        {"action": "extract", "selector": "a", "attribute": "text", "script": "1"}
    ]))
    .expect_err("script field refused");
    assert!(error.0.contains("unsupported fields"), "{error}");
}

#[test]
fn named_presets_are_explicit_and_bounded() {
    let (steps, budget) =
        preset_actions(&json!(["expand_tabs", "read_datalayer"])).expect("presets");
    assert_eq!(steps.len(), 6);
    assert_eq!(budget, 5 * 5000 + 5000);
    let error = preset_actions(&json!(["anything"])).expect_err("unknown preset");
    assert!(error.0.contains("unknown page action preset"), "{error}");
}

#[test]
fn runner_returns_per_step_results_without_executable_input() {
    let mut page = FakePage::new();
    let evidence = run_actions(
        &mut page,
        &json!([
            {"action": "click", "selector": "button.more"},
            {"action": "scroll", "pixels": 400},
            {"action": "extract", "selector": "a", "attribute": "href"},
        ]),
        None,
        &|| 0.0,
    )
    .expect("runs");
    assert_eq!(evidence["state"], "complete");
    assert_eq!(evidence["steps"][2]["value"], json!(["/one", "/two"]));
    assert_eq!(*page.scrolls.borrow(), vec![400]);
}

#[test]
fn runner_enforces_one_cumulative_deadline() {
    let clock = Rc::new(Cell::new(0.0));
    let mut page = FakePage::new();
    page.clock = Some(Rc::clone(&clock));
    let now = Rc::clone(&clock);
    let evidence = run_actions(
        &mut page,
        &Value::Array(vec![json!({"action": "wait_ms", "ms": 10_000}); 6]),
        None,
        &move || now.get(),
    )
    .expect("runs");
    assert_eq!(evidence["budget_ms"], 60_000);
    assert_eq!(evidence["state"], "partial");
    assert_eq!(evidence["steps"][5]["state"], "partial");
}

// extract_chars must tally UTF-16 code units, the unit EXTRACT_JS counts with String.length.
// Counting scalar values would halve astral text, so the cumulative cap would be exceeded.
#[test]
fn extract_budget_counts_utf16_code_units() {
    let mut page = FakePage::new();
    // Two emoji of 2 UTF-16 units each: the page just spent 16,000 units of the cap.
    let smile = "\u{1F600}".repeat(MAX_EXTRACT_TOTAL_CHARS as usize / 2);
    *page.extract_responses.borrow_mut() = vec![
        json!({"values": ["tail"], "omitted": 0}),
        json!({"values": [smile], "omitted": 0}),
    ];
    let evidence = run_actions(
        &mut page,
        &json!([
            {"action": "extract", "selector": "a", "attribute": "text"},
            {"action": "extract", "selector": "a", "attribute": "text"},
        ]),
        None,
        &|| 0.0,
    )
    .expect("runs");
    assert_eq!(evidence["state"], "complete");
    assert_eq!(
        *page.extract_budgets.borrow(),
        vec![MAX_EXTRACT_TOTAL_CHARS, 0]
    );
}

#[test]
fn runner_refuses_form_or_navigation_click_targets() {
    let mut page = FakePage::new();
    page.click_safe = false;
    let evidence = run_actions(
        &mut page,
        &json!([{"action": "click", "selector": "button.submit"}]),
        None,
        &|| 0.0,
    )
    .expect("runs");
    assert_eq!(evidence["state"], "partial");
    assert_eq!(evidence["steps"][0]["state"], "unavailable");
    assert!(page.clicks.borrow().is_empty());
}
