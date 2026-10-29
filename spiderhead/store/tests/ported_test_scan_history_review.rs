//!
//! Fixture: the oracle's finished `basic` scan (with its audit-v2 sidecar) is copied into a
//! temp directory, and its `finished_at` is moved back in time, as the reference `_finished` helper
//! does with `UPDATE scan SET finished_at=?`.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use spiderhead_store::history::{prune_apply, prune_preview, snapshot_scan};
use spiderhead_store::scan_artifact::open_scan;

/// Scratch directory under the system temp dir, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("sh-history-review-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn oracle_basic() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../oracle/raw-scans/basic")
        .canonicalize()
        .expect("oracle basic fixture")
}

/// Copy the oracle scan (and its audit sidecar) to `dest` and set `finished_at` `age_days` ago.
fn finished_copy(dest: &Path, age_days: i64) {
    let src = oracle_basic();
    fs::copy(src.join("scan.sqlite"), dest).expect("copy scan");
    fs::copy(
        src.join("scan.sqlite.audit-v2.sqlite"),
        PathBuf::from(format!("{}.audit-v2.sqlite", dest.display())),
    )
    .expect("copy audit sidecar");
    let finished = chrono_like_days_ago(age_days);
    let con = Connection::open(dest).expect("open copy");
    con.execute("UPDATE scan SET finished_at=?1", [finished])
        .expect("set finished_at");
}

/// ISO-8601 UTC timestamp `days` before now, in the same shape as the scan writer.
fn chrono_like_days_ago(days: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    let t = now - days * 86_400;
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    let secs = t.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000000Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// (fixture-only probe: the oracle basic scan is complete; the partial variant is left for a
// follow-up once a partial-corpus fixture exists.)
#[test]
fn probe_preview_on_finished_copy() {
    let dir = Scratch::new("probe-preview");
    finished_copy(&dir.path().join("old.sqlite"), 40);
    let plan = prune_preview(dir.path(), 30, 0).expect("preview");
    let candidates = plan["candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 1);
}

#[test]
fn probe_snapshot_and_open() {
    let dir = Scratch::new("probe-snapshot");
    let source = dir.path().join("source.sqlite");
    finished_copy(&source, 0);
    let history = dir.path().join("history");
    fs::create_dir(&history).expect("mkdir");
    let result = snapshot_scan(&source, &history).expect("snapshot");
    let _con = open_scan(Path::new(&result), false, None).expect("open");
    let _ = prune_apply;
}
