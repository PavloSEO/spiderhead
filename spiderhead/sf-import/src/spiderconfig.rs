//! Apply a crawl-rate limit to a serialized crawler config in headless mode.
//!
//! option; it only accepts `--config <file>`, and the config itself is a Java
//! serialization stream created in the GUI. The only safe headless approach is
//! to start from an existing config and update the two fields in
//! `seo.spider.config.SpiderPerformanceConfig` precisely:
//!
//! ```text
//! Z  mLimitPerformance        (boolean, 1 byte)
//! D  mUrlRequestsPerSecond    (double,  8 bytes, big-endian)
//! ```
//!
//! Their values immediately follow `TC_ENDBLOCKDATA TC_NULL` (`b"xp"`), which
//! terminates the class descriptor. The patch changes exactly nine bytes and
//! nothing else, preserving the stream structure.
//!
//! Interoperability only: this module edits a config file the user created with
//! their own licensed copy and never ships one.
//!
//! An explicit limitation: this code does not modify `mMaxThreads`. That
//! integer is one of roughly ten primitive fields in another class, and its
//! offset cannot be determined without parsing that descriptor fully. Guessing
//! an offset could silently corrupt the config. The requests-per-second limit
//! is already the more restrictive control over crawl traffic.

use std::path::{Path, PathBuf};
use thiserror::Error;

/// Field name marking the speed block inside `SpiderPerformanceConfig`.
pub const PERF_FIELD: &[u8] = b"mUrlRequestsPerSecond";
/// Serialized class name carrying the speed fields.
pub const PERF_CLASS: &[u8] = b"seo.spider.config.SpiderPerformanceConfig";
/// `TC_ENDBLOCKDATA + TC_NULL` — terminates the class descriptor.
pub const FIELDS_END: &[u8] = b"xp";

/// Match the practical range exposed by the vendor GUI: 0.1 to 1,000 URLs/s.
pub const MIN_RATE: f64 = 0.1;
/// Upper bound of the same GUI range.
pub const MAX_RATE: f64 = 1000.0;

const TC_CLASSDESC: u8 = 0x72;
const TC_ENDBLOCKDATA: u8 = 0x78;
const TC_NULL: u8 = 0x70;
const TC_STRING: u8 = 0x74;
const TC_REFERENCE: u8 = 0x71;

fn primitive_size(typecode: u8) -> Option<usize> {
    match typecode {
        b'B' | b'Z' => Some(1),
        b'C' | b'S' => Some(2),
        b'I' | b'F' => Some(4),
        b'J' | b'D' => Some(8),
        _ => None,
    }
}

/// Module label -> (class name, boolean field). Every entry was read out of a
/// real config; a name that stops matching makes the flag unknown rather than
/// wrong. Order matches the oracle's `MODULE_FLAG_FIELDS` dict.
pub const MODULE_FLAG_FIELDS: &[(&str, (&str, &str))] = &[
    (
        "structured_data_json_ld",
        (
            "seo.spider.config.SpiderStructuredDataConfig",
            "mExtractJsonLd",
        ),
    ),
    (
        "structured_data_microdata",
        (
            "seo.spider.config.SpiderStructuredDataConfig",
            "mExtractMicrodata",
        ),
    ),
    (
        "structured_data_rdfa",
        (
            "seo.spider.config.SpiderStructuredDataConfig",
            "mExtractRdfa",
        ),
    ),
    (
        "structured_data_google_validation",
        (
            "seo.spider.config.SpiderStructuredDataConfig",
            "mGoogleValidation",
        ),
    ),
    (
        "structured_data_schema_org_validation",
        (
            "seo.spider.config.SpiderStructuredDataConfig",
            "mSchemaDotOrgValidation",
        ),
    ),
    (
        "spelling",
        ("seo.spider.config.LanguageToolConfig", "mSpellCheckEnabled"),
    ),
    (
        "grammar",
        (
            "seo.spider.config.LanguageToolConfig",
            "mGrammarCheckEnabled",
        ),
    ),
    (
        "store_html",
        ("seo.spider.config.SpiderCrawlConfig", "mStoreOriginalHtml"),
    ),
    (
        "store_rendered_html",
        ("seo.spider.config.SpiderCrawlConfig", "mStoreRenderedHtml"),
    ),
    (
        "crawl_linked_xml_sitemaps",
        ("seo.spider.config.SpiderCrawlConfig", "mCrawlSitemaps"),
    ),
    (
        "auto_discover_sitemaps",
        (
            "seo.spider.config.SpiderCrawlConfig",
            "mAutoDiscoverSitemaps",
        ),
    ),
    (
        "near_duplicates",
        (
            "seo.spider.config.DuplicateConfig",
            "mNearDuplicateChecking",
        ),
    ),
    (
        "auto_crawl_analysis",
        ("seo.spider.config.CrawlAnalysisConfig", "mAutoAnalyse"),
    ),
];

/// Config parsing/patching failure surface (`ValueError` in the oracle).
#[derive(Debug, Error)]
pub enum SpiderConfigError {
    /// The stream layout is not the exact structure this implementation can
    /// patch, or the value read cannot be trusted.
    #[error("{0}")]
    Value(String),
    /// No base config exists to start from.
    #[error("{0}")]
    Runtime(String),
    /// Filesystem failure while reading or writing a config.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// reference `repr()` of a single-character string — the `!r` formatting the
/// oracle uses in its error text (`'Z'`, `'"'"'`, `'\\x07'`, `'\u00e9'`).
fn py_repr_char(c: u8) -> String {
    match c {
        b'\'' => "\"'\"".to_owned(),
        b'\\' => "'\\\\'".to_owned(),
        b'\n' => "'\\n'".to_owned(),
        b'\r' => "'\\r'".to_owned(),
        b'\t' => "'\\t'".to_owned(),
        0x20..=0x7e => format!("'{}'", c as char),
        0xa1..=0xff if c != 0xad => format!("'{}'", c as char),
        _ => format!("'\\x{c:02x}'"),
    }
}

fn value_err(msg: impl Into<String>) -> SpiderConfigError {
    SpiderConfigError::Value(msg.into())
}

/// `_perf_offsets`: the flag and double offsets in SpiderPerformanceConfig.
///
/// Err when the stream layout is not the exact structure this implementation
/// can patch. Never guess offsets in an unknown config.
fn perf_offsets(blob: &[u8]) -> Result<(usize, usize), SpiderConfigError> {
    let field = blob
        .windows(PERF_FIELD.len())
        .position(|w| w == PERF_FIELD)
        .ok_or_else(|| {
            value_err(format!(
                "config does not contain {} — \
                 it is not a serialized Screaming Frog config or uses another format version",
                String::from_utf8_lossy(PERF_FIELD)
            ))
        })?;
    let end = blob[field..]
        .windows(FIELDS_END.len())
        .position(|w| w == FIELDS_END)
        .map(|p| field + p)
        .ok_or_else(|| {
            value_err("SpiderPerformanceConfig descriptor is unterminated — config is corrupt")
        })?;
    let flag_at = end + FIELDS_END.len();
    let double_at = flag_at + 1;
    if double_at + 8 > blob.len() {
        return Err(value_err(
            "config ends inside the speed block — file is corrupt",
        ));
    }
    if blob[flag_at] > 1 {
        return Err(value_err(format!(
            "mLimitPerformance contains {:#x}, expected 0 or 1 — \
             the config layout does not match and cannot be patched safely",
            blob[flag_at]
        )));
    }
    Ok((flag_at, double_at))
}

/// `read_speed`: `(limit_enabled, URLs_per_second)` from a config blob.
pub fn read_speed(blob: &[u8]) -> Result<(bool, f64), SpiderConfigError> {
    let (flag_at, double_at) = perf_offsets(blob)?;
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&blob[double_at..double_at + 8]);
    Ok((blob[flag_at] != 0, f64::from_be_bytes(buf)))
}

/// `patch_speed`: enable and set the crawl-rate limit without performing I/O.
pub fn patch_speed(blob: &[u8], urls_per_second: f64) -> Result<Vec<u8>, SpiderConfigError> {
    let rate = urls_per_second;
    if !(MIN_RATE..=MAX_RATE).contains(&rate) {
        return Err(value_err(format!(
            "rate {} is outside the allowed {}..{} URLs/s",
            crate::pyvalue::float_repr(rate),
            crate::pyvalue::float_repr(MIN_RATE),
            crate::pyvalue::float_repr(MAX_RATE),
        )));
    }
    let (flag_at, double_at) = perf_offsets(blob)?;
    let mut out = blob.to_vec();
    out[flag_at] = 1;
    out[double_at..double_at + 8].copy_from_slice(&rate.to_be_bytes());
    // Trust the patch only after a round-trip through the same strict parser.
    let (enabled, written) = read_speed(&out)?;
    if !enabled || (written - rate).abs() > 1e-9 {
        return Err(value_err(
            "speed patch failed round-trip validation — file was not written",
        ));
    }
    Ok(out)
}

/// `find_base_config`: an explicit base config, or the latest crawl config.
pub fn find_base_config(explicit: Option<&str>) -> Option<PathBuf> {
    find_base_config_in(explicit, &crawl_config_globs())
}

/// `find_base_config` over a caller-supplied glob list — the oracle
/// monkeypatches `CRAWL_CONFIG_GLOBS` in tests.
pub fn find_base_config_in(explicit: Option<&str>, patterns: &[String]) -> Option<PathBuf> {
    if let Some(path) = explicit {
        if !path.is_empty() && Path::new(path).is_file() {
            return Some(PathBuf::from(path));
        }
    }
    let mut found: Vec<PathBuf> = Vec::new();
    for pattern in patterns {
        found.extend(glob_files(pattern));
    }
    if found.is_empty() {
        return None;
    }
    // `max(found, key=os.path.getmtime)`: the FIRST maximum wins.
    let mtime = |p: &PathBuf| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    };
    let mut best = &found[0];
    for candidate in &found[1..] {
        if mtime(candidate) > mtime(best) {
            best = candidate;
        }
    }
    Some(best.clone())
}

/// `CRAWL_CONFIG_GLOBS` — the two vendor locations as raw glob strings.
fn crawl_config_globs() -> Vec<String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("~"));
    vec![
        home.join(".ScreamingFrogSEOSpider/ProjectInstanceData/*/serialised_data/spiderconfig")
            .to_string_lossy()
            .into_owned(),
        home.join(".ScreamingFrogSEOSpider/spiderconfig")
            .to_string_lossy()
            .into_owned(),
    ]
}

/// `glob.glob(pattern)` restricted to `*` inside path segments, the only
/// wildcard the oracle's patterns use. Yields existing files only.
fn glob_files(pattern: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let pat = Path::new(pattern);
    let anchored = pat.is_absolute();
    let segments: Vec<String> = pat
        .iter()
        .map(|c| c.to_string_lossy().into_owned())
        .filter(|c| c != "/")
        .collect();
    fn walk(dir: &Path, segments: &[String], out: &mut Vec<PathBuf>) {
        let Some((head, tail)) = segments.split_first() else {
            if dir.is_file() {
                out.push(dir.to_path_buf());
            }
            return;
        };
        if head.contains('*') || head.contains('?') || head.contains('[') {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if fnmatch(head, &name) {
                    walk(&entry.path(), tail, out);
                }
            }
        } else {
            walk(&dir.join(head), tail, out);
        }
    }
    let start = if anchored {
        PathBuf::from("/")
    } else {
        PathBuf::from(".")
    };
    walk(&start, &segments, &mut out);
    out
}

/// `fnmatch` for a single path segment: `*` any run, `?` one char,
/// `[...]`/`[!...]` character classes.
fn fnmatch(pattern: &str, name: &str) -> bool {
    fn inner(p: &[u8], n: &[u8]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some(b'*') => (0..=n.len()).any(|i| inner(&p[1..], &n[i..])),
            Some(b'?') => !n.is_empty() && inner(&p[1..], &n[1..]),
            Some(b'[') => {
                if n.is_empty() {
                    return false;
                }
                let (neg, mut i) = if p.get(1) == Some(&b'!') {
                    (true, 2)
                } else {
                    (false, 1)
                };
                let mut matched = false;
                let mut prev: Option<u8> = None;
                while i < p.len() && p[i] != b']' {
                    if p[i] == b'-' && prev.is_some() && i + 1 < p.len() && p[i + 1] != b']' {
                        i += 1;
                        if prev.unwrap() <= n[0] && n[0] <= p[i] {
                            matched = true;
                        }
                        prev = None;
                    } else {
                        if p[i] == n[0] {
                            matched = true;
                        }
                        prev = Some(p[i]);
                    }
                    i += 1;
                }
                matched != neg && i < p.len() && inner(&p[i + 1..], &n[1..])
            }
            Some(&c) => !n.is_empty() && n[0] == c && inner(&p[1..], &n[1..]),
        }
    }
    inner(pattern.as_bytes(), name.as_bytes())
}

/// `build_throttled_config`: build a rate-limited config and return its
/// absolute path.
pub fn build_throttled_config(
    dest: &Path,
    urls_per_second: f64,
    base: Option<&str>,
) -> Result<PathBuf, SpiderConfigError> {
    build_throttled_config_in(dest, urls_per_second, base, &crawl_config_globs())
}

/// `build_throttled_config` over a caller-supplied glob list — the oracle
/// monkeypatches `CRAWL_CONFIG_GLOBS` in tests.
pub fn build_throttled_config_in(
    dest: &Path,
    urls_per_second: f64,
    base: Option<&str>,
    patterns: &[String],
) -> Result<PathBuf, SpiderConfigError> {
    let source = find_base_config_in(base, patterns).ok_or_else(|| {
        SpiderConfigError::Runtime(
            "cannot apply a crawl-rate limit because no base Screaming Frog config was found. \
             Save any config from the GUI (Config → File → Config → Save As) and set its \
             path as sf_cli.seospiderconfig in config.json, or run one crawl in the GUI so \
             SF creates a reusable config automatically. Without a base file, the \
             limit cannot be set; the toolkit will not crawl a third-party site at full \
             speed after a rate limit was explicitly requested."
                .to_owned(),
        )
    })?;
    let blob = std::fs::read(&source)?;
    let patched = patch_speed(&blob, urls_per_second)?;
    // os.path.abspath(dest): normpath against the cwd, no symlink resolution.
    let abs = if dest.is_absolute() {
        normpath(dest)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        normpath(&cwd.join(dest))
    };
    let dir = abs.parent().map(Path::to_path_buf).unwrap_or_default();
    let dir = if dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        dir
    };
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&abs, patched)?;
    Ok(abs)
}

/// POSIX `normpath`: collapse `.`/`..`/duplicate slashes textually, keep the
/// root. Differs from `canonicalize` — symlinks are not resolved.
fn normpath(path: &Path) -> PathBuf {
    let mut out: Vec<String> = Vec::new();
    for part in path.to_string_lossy().split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other.to_owned()),
        }
    }
    let joined = out.join("/");
    if path.is_absolute() {
        PathBuf::from(format!("/{joined}"))
    } else {
        PathBuf::from(joined)
    }
}

// ── module flags ────────────────────────────────────────────────────────────
//
// Which registry checks a crawl can satisfy is decided by module switches
// inside the same serialized config. The stream is standard Java serialization,
// so the layout is parsed, never guessed: a class descriptor carries its fields
// in order (primitives first, alphabetically, then object fields), the class
// data follows the descriptor, and a primitive's offset is the sum of the sizes
// of the primitives before it. Anything that does not match exactly is reported
// as unknown, never as "off".

/// `struct.unpack(">H", blob[i:i+2])` — `struct.error` text on a short buffer.
fn read_u16(blob: &[u8], at: usize) -> Result<u16, SpiderConfigError> {
    let bytes = blob.get(at..at + 2).ok_or_else(|| {
        value_err("class descriptor is malformed: unpack requires a buffer of 2 bytes")
    })?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// `blob[i]` — `IndexError` text when past the end.
fn read_byte(blob: &[u8], at: usize) -> Result<u8, SpiderConfigError> {
    blob.get(at)
        .copied()
        .ok_or_else(|| value_err("class descriptor is malformed: index out of range"))
}

/// `blob[i:i+len]` — a slice never raises; a short tail just comes back short.
fn slice(blob: &[u8], at: usize, len: usize) -> &[u8] {
    let start = at.min(blob.len());
    &blob[start..(at + len).min(blob.len())]
}

/// `.decode("utf-8")` — `UnicodeDecodeError` text on bad input.
fn decode_utf8(bytes: &[u8]) -> Result<String, SpiderConfigError> {
    String::from_utf8(bytes.to_vec()).map_err(|e| {
        let err = e.utf8_error();
        let pos = err.valid_up_to();
        let byte = bytes.get(pos).copied().unwrap_or(0);
        let reason = if err.error_len().is_none() {
            "unexpected end of data"
        } else if (0x80..0xc2).contains(&byte) && pos > 0 {
            "invalid continuation byte"
        } else {
            "invalid start byte"
        };
        value_err(format!(
            "class descriptor is malformed: 'utf-8' codec can't decode byte              0x{byte:02x} in position {pos}: {reason}"
        ))
    })
}

/// `_read_class_descriptor`: parse one `TC_CLASSDESC` and return
/// `(name, fields, end)`. `fields` are `(typecode, name)` in stream order.
type ClassDescriptor = (String, Vec<(u8, String)>, usize);

fn read_class_descriptor(blob: &[u8], at: usize) -> Result<ClassDescriptor, SpiderConfigError> {
    if at >= blob.len() || blob[at] != TC_CLASSDESC {
        return Err(value_err("not a class descriptor at the given offset"));
    }
    let mut i = at + 1;
    let name_len = read_u16(blob, i)? as usize;
    i += 2;
    let name = decode_utf8(slice(blob, i, name_len))?;
    i += name_len;
    i += 8; // serialVersionUID
    i += 1; // class flags
    let count = read_u16(blob, i)? as usize;
    i += 2;
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let typecode = read_byte(blob, i)?;
        i += 1;
        let field_len = read_u16(blob, i)? as usize;
        i += 2;
        let field_name = decode_utf8(slice(blob, i, field_len))?;
        i += field_len;
        if typecode == b'L' || typecode == b'[' {
            // Object fields carry a type signature: a string or a back-reference.
            match read_byte(blob, i)? {
                TC_STRING => {
                    i += 1;
                    let sig_len = read_u16(blob, i)? as usize;
                    i += 2 + sig_len;
                }
                TC_REFERENCE => i += 5,
                other => {
                    return Err(value_err(format!(
                        "unexpected type signature marker {other:#x}"
                    )))
                }
            }
        } else if primitive_size(typecode).is_none() {
            return Err(value_err(format!(
                "unknown field typecode {}",
                py_repr_char(typecode)
            )));
        }
        fields.push((typecode, field_name));
    }
    Ok((name, fields, i))
}

/// `read_boolean_field`: one boolean field of a config class out of the
/// serialized stream.
///
/// Err when the class or field is absent, when the class has a superclass with
/// its own data (the offset would not be computable), or when the byte read is
/// not a boolean.
pub fn read_boolean_field(
    blob: &[u8],
    class_name: &str,
    field_name: &str,
) -> Result<bool, SpiderConfigError> {
    let marker = class_name.as_bytes();
    let at = blob
        .windows(marker.len())
        .position(|w| w == marker)
        .map(|p| p as i64)
        .unwrap_or(-1);
    if at < 3 {
        return Err(value_err(format!(
            "{class_name} is not present in this config"
        )));
    }
    let (name, fields, end) = read_class_descriptor(blob, (at - 3) as usize)?;
    if name != class_name {
        return Err(value_err(format!(
            "expected {class_name} at the descriptor, found {name}"
        )));
    }

    // Class data starts after the class annotation and the superclass
    // descriptor. Only a null superclass ("xp") keeps the offset computable.
    if blob.get(end..end + 2) != Some(&[TC_ENDBLOCKDATA, TC_NULL]) {
        return Err(value_err(format!(
            "{class_name} has a superclass or annotation data — offset is not computable"
        )));
    }
    let data_at = end + 2;

    let mut offset = 0usize;
    for (typecode, name_in_stream) in &fields {
        let Some(size) = primitive_size(*typecode) else {
            // Object fields follow every primitive, so the target is not a primitive.
            break;
        };
        if name_in_stream == field_name {
            if *typecode != b'Z' {
                return Err(value_err(format!(
                    "{class_name}.{field_name} is {}, not a boolean",
                    py_repr_char(*typecode)
                )));
            }
            let position = data_at + offset;
            let value = *blob
                .get(position)
                .ok_or_else(|| value_err("config ends before the requested field"))?;
            if value > 1 {
                return Err(value_err(format!(
                    "{class_name}.{field_name} holds {value:#x}, expected 0 or 1 — \
                     the layout does not match and the value cannot be trusted"
                )));
            }
            return Ok(value != 0);
        }
        offset += size;
    }
    Err(value_err(format!(
        "{class_name} has no primitive field {field_name}"
    )))
}

/// `read_module_flags`: the module switches, `None` for anything unreadable.
///
/// A module that cannot be read is reported as unknown on purpose. Reporting it
/// as off would tell the operator to fix a config that may already be correct.
/// Order matches the oracle dict.
pub fn read_module_flags(blob: &[u8]) -> Vec<(&'static str, Option<bool>)> {
    MODULE_FLAG_FIELDS
        .iter()
        .map(|(label, (class_name, field_name))| {
            let flag = read_boolean_field(blob, class_name, field_name).ok();
            (*label, flag)
        })
        .collect()
}
