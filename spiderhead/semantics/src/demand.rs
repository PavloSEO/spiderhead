//! Observed-demand helper.

/// `has_observed_demand` — any of `impr`/`pos_y`/`pos_g` present and > 0.
pub fn has_observed_demand(impr: Option<i64>, pos_y: Option<i64>, pos_g: Option<i64>) -> bool {
    [impr, pos_y, pos_g]
        .iter()
        .any(|v| v.is_some_and(|x| x > 0))
}
