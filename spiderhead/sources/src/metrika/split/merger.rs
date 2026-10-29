//! Streaming merger that adds up report slices under one row cap.

use super::*;

/// Sums additive metrics per dimension key under one row cap.
pub(super) struct SliceMerger {
    pub(super) cap: i64,
    pub(super) n: usize,
    pub(super) merged: HashMap<String, usize>,
    pub(super) values: Vec<Value>,
    pub(super) order: Vec<String>,
    /// `totals` is `None` until a slice contributes a list.
    pub(super) totals: Option<Vec<Value>>,
    pub(super) totals_missing: bool,
    pub(super) dropped_rows: i64,
    pub(super) overflowed: bool,
    pub(super) incomplete: bool,
}

impl SliceMerger {
    pub(super) fn new(cap: i64, n_metrics: usize) -> Self {
        SliceMerger {
            cap,
            n: n_metrics,
            merged: HashMap::new(),
            values: Vec::new(),
            order: Vec::new(),
            totals: None,
            totals_missing: false,
            dropped_rows: 0,
            overflowed: false,
            incomplete: false,
        }
    }

    /// Validate a metric vector against the requested count.
    pub(super) fn padded(&self, values: Option<&Value>) -> (Vec<Value>, bool) {
        let empty: &[Value] = &[];
        let items: &[Value] = values
            .and_then(Value::as_array)
            .map(|v| &v[..])
            .unwrap_or(empty);
        let bad = items.len() != self.n || !items.iter().all(is_number);
        let padded: Vec<Value> = (0..self.n)
            .map(|i| items.get(i).cloned().unwrap_or(Value::Null))
            .collect();
        (padded, bad)
    }

    /// `add(body)` — merge one slice body; returns rows dropped by the cap.
    pub(super) fn add(&mut self, body: &Value) -> Result<i64, MetrikaFail> {
        let mut dropped = 0i64;
        let data = py_get(body, "data")?;
        let rows = match data {
            Some(data) if pystr::truthy(Some(data)) => py_iter(data)?,
            _ => Vec::new(),
        };
        for row in &rows {
            let (values, bad) = self.padded(py_get(row, "metrics")?);
            self.incomplete = self.incomplete || bad;
            let dimensions = py_get(row, "dimensions")?;
            let key = pyjson::dumps_opts(&dimensions.cloned().unwrap_or(Value::Null), true, false);
            match self.merged.get(&key) {
                None => {
                    if self.merged.len() as i64 >= self.cap {
                        dropped += 1;
                        self.overflowed = true;
                        continue;
                    }
                    let mut copy = py_dict_from(row.clone())?;
                    copy.insert("metrics".to_owned(), Value::Array(values));
                    self.merged.insert(key.clone(), self.values.len());
                    self.values.push(Value::Object(copy));
                    self.order.push(key);
                }
                Some(&index) => {
                    let current = self.values[index]
                        .get("metrics")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let (summed, bad) = sum_metrics(&current, &values);
                    if let Some(object) = self.values[index].as_object_mut() {
                        object.insert("metrics".to_owned(), Value::Array(summed));
                    }
                    self.incomplete = self.incomplete || bad;
                }
            }
        }
        self.dropped_rows += dropped;
        let slice_totals = py_get(body, "totals")?;
        if let Some(totals @ Value::Array(_)) = slice_totals {
            let (padded, bad) = self.padded(Some(totals));
            self.incomplete = self.incomplete || bad;
            match self.totals.take() {
                None => self.totals = Some(padded),
                Some(current) => {
                    let (summed, bad) = sum_metrics(&current, &padded);
                    self.totals = Some(summed);
                    self.incomplete = self.incomplete || bad;
                }
            }
        } else {
            // A slice without totals cannot contribute to a whole-period total.
            self.totals_missing = true;
            self.incomplete = true;
        }
        Ok(dropped)
    }

    /// `rows()` — merged rows in first-seen order.
    pub(super) fn rows(&self) -> Vec<Value> {
        self.order
            .iter()
            .map(|key| self.values[self.merged[key]].clone())
            .collect()
    }
}
