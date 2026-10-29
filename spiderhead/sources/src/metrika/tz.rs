//! A minimal TZif reader for the production zone path: it answers for one unix timestamp
//! from the system tz database.

use std::fs;
use std::path::PathBuf;

/// UTC offset in whole seconds for one instant, read from the system tz database.
/// `None` when the key is invalid or no readable TZif file exists under the search directories.
///
/// Before the first recorded transition the type of that first transition is used, and after
/// the last transition the last type applies. The POSIX footer rule is not expanded, which only
/// matters past the file's explicit transition table (~2038 for zones with recurring rules).
pub(super) fn tz_offset_at(name: &str, unix_seconds: i64) -> Option<i64> {
    let data = fs::read(tzpath_file(name)?).ok()?;
    let body = parse_body(&data)?;
    offset_at(&body, unix_seconds)
}

/// Search directories for TZif files: the environment override first, then the platform directories.
fn tzpath_file(name: &str) -> Option<PathBuf> {
    if crate::ga4_content::zoneinfo_key_check(name).is_some() {
        return None;
    }
    let dirs: Vec<PathBuf> = match std::env::var_os("PYTHONTZPATH") {
        Some(value) if !value.is_empty() => std::env::split_paths(&value).collect(),
        _ => [
            "/usr/share/zoneinfo",
            "/usr/lib/zoneinfo",
            "/usr/share/lib/zoneinfo",
            "/etc/zoneinfo",
        ]
        .iter()
        .map(PathBuf::from)
        .collect(),
    };
    dirs.into_iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

/// A parsed TZif body: transition instants, per-transition type indices, and
/// the `utoff` of each `ttinfo` entry.
struct Body {
    transitions: Vec<i64>,
    type_indices: Vec<u8>,
    offsets: Vec<i32>,
}

/// Read the newest body — the 64-bit v2/v3/v4 table when present, else v1.
fn parse_body(data: &[u8]) -> Option<Body> {
    if data.get(..4) != Some(b"TZif") {
        return None;
    }
    match *data.get(4)? {
        // Version '\0' means the file has only the 32-bit table.
        0 => read_body(data, false),
        _ => {
            let v1_len = body_len(data.get(20..44)?, false)?;
            read_body(data.get(44 + v1_len..)?, true)
        }
    }
}

/// Byte length of one body section given its header counts.
fn body_len(counts: &[u8], wide: bool) -> Option<usize> {
    let six = |i: usize| -> Option<usize> {
        Some(u32::from_be_bytes(counts.get(i * 4..i * 4 + 4)?.try_into().ok()?) as usize)
    };
    let timecnt = six(3)?;
    let typecnt = six(4)?;
    let charcnt = six(5)?;
    let leapcnt = six(2)?;
    let ttisstdcnt = six(1)?;
    let ttisgmtcnt = six(0)?;
    let ts_size = if wide { 8 } else { 4 };
    let leap_size = if wide { 12 } else { 8 };
    Some(
        timecnt * (ts_size + 1)
            + typecnt * 6
            + charcnt
            + leapcnt * leap_size
            + ttisstdcnt
            + ttisgmtcnt,
    )
}

/// Parse one 44-byte header plus its body starting at `data`.
fn read_body(data: &[u8], wide: bool) -> Option<Body> {
    let counts = data.get(20..44)?;
    let u32_at = |counts: &[u8], i: usize| -> Option<usize> {
        Some(u32::from_be_bytes(counts.get(i * 4..i * 4 + 4)?.try_into().ok()?) as usize)
    };
    let timecnt = u32_at(counts, 3)?;
    let typecnt = u32_at(counts, 4)?;
    let mut cursor = 44usize;
    let ts_size = if wide { 8 } else { 4 };
    let mut transitions = Vec::with_capacity(timecnt);
    for _ in 0..timecnt {
        let chunk = data.get(cursor..cursor + ts_size)?;
        transitions.push(if wide {
            i64::from_be_bytes(chunk.try_into().ok()?)
        } else {
            i64::from(i32::from_be_bytes(chunk.try_into().ok()?))
        });
        cursor += ts_size;
    }
    let type_indices = data.get(cursor..cursor + timecnt)?.to_vec();
    cursor += timecnt;
    let mut offsets = Vec::with_capacity(typecnt);
    for _ in 0..typecnt {
        let chunk = data.get(cursor..cursor + 6)?;
        offsets.push(i32::from_be_bytes(chunk[..4].try_into().ok()?));
        cursor += 6;
    }
    Some(Body {
        transitions,
        type_indices,
        offsets,
    })
}

/// `utcoffset` for one instant — last transition at or before it.
fn offset_at(body: &Body, unix_seconds: i64) -> Option<i64> {
    if body.offsets.is_empty() {
        return None;
    }
    let idx = match body.transitions.partition_point(|&ts| ts <= unix_seconds) {
        0 => body.type_indices.first().copied().unwrap_or(0) as usize,
        i => body.type_indices.get(i - 1).copied().unwrap_or(0) as usize,
    };
    body.offsets.get(idx).map(|&off| i64::from(off))
}
