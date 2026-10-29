//! Shared confirmation gate for paid provider calls.
//!
//! A paid adapter calls [`confirmation_required`] before it builds a client or sends a request; without
//! an explicit `confirm_paid == true` the gate returns a refusal envelope and no network call happens.

use serde_json::{json, Value};

/// Return a refusal envelope unless the caller explicitly confirmed the paid call.
pub fn confirmation_required(
    confirm_paid: bool,
    operation: &str,
    estimate: Option<&Value>,
) -> Option<Value> {
    if confirm_paid {
        return None;
    }
    Some(json!({
        "ok": false,
        "state": "confirmation_required",
        "operation": operation,
        "error": format!("{operation} is paid and requires confirm_paid=true"),
        "estimate": estimate.cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn gate_refuses_without_confirmation() {
        let result = confirmation_required(false, "op", Some(&json!({"cost_usd": 0.02})))
            .expect("refusal envelope");
        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["state"], json!("confirmation_required"));
        assert_eq!(result["operation"], json!("op"));
        assert_eq!(result["estimate"], json!({"cost_usd": 0.02}));
    }

    #[test]
    fn gate_passes_only_on_explicit_true() {
        assert!(confirmation_required(true, "op", None).is_none());
        // The bool type rejects a non-bool `confirm_paid` at compile time, so only the refusal path is tested.
        let refused = confirmation_required(false, "op", None).expect("refusal");
        assert_eq!(refused["state"], json!("confirmation_required"));
    }
}
