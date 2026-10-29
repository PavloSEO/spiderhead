//! HTTP fetch layer of the crawler: client, robots.txt engine, per-host pacing.
//!
//! Recorded strings (notes, reasons) are part of the scan contract; their wording is fixed.

pub mod client;
pub mod clock;
pub mod robots;
pub mod throttle;
