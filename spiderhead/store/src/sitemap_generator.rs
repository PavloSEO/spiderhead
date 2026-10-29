//! Generate protocol-limited XML sitemaps from one validated, read-only scan.
//!
//! No URL is fetched. Page eligibility comes from retained response/directive/canonical facts;
//! optional image locations come from complete retained HTML. Missing `lastmod` is omitted, and
//! missing image bodies remain explicit coverage gaps.

#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use crate::error::StoreError;
use crate::exports::{unlink_owned, write_file, Owned};
use crate::link_context::cell_value;
use crate::scan_artifact::open_scan;
use rusqlite::types::Value as Sql;
use rusqlite::{params, params_from_iter, Connection};
use serde_json::{json, Map, Value};
use spiderhead_core::filesystem::{file_identity, PrivateMode};
use spiderhead_parse::dom::{parse_document, ElementRef, Selector};
use spiderhead_parse::parser::extract::{extract_url_sources, image_url_sources};
use spiderhead_parse::parser::meta::robots_directives;
use spiderhead_parse::parser::tokenize::{document_base_url, DocumentSource};
use spiderhead_parse::py_url::{urljoin, urlsplit, urlunsplit};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Format label of the result.
pub const FORMAT: &str = "spiderhead.scan-sitemaps.v1";
/// Sitemap protocol namespace.
pub const NAMESPACE: &str = "http://www.sitemaps.org/schemas/sitemap/0.9";
/// Image sitemap namespace.
pub const IMAGE_NAMESPACE: &str = "http://www.google.com/schemas/sitemap-image/1.1";
/// URLs per sitemap file.
pub const MAX_URLS: i64 = 50000;
/// Uncompressed bytes per sitemap file (50 MiB).
pub const MAX_BYTES: i64 = 52428800;
/// Images listed per page.
pub const MAX_IMAGES: i64 = 1000;
/// Largest retained document read for image discovery (5 MiB).
pub const MAX_DOCUMENT_BYTES: i64 = 5242880;
/// Regex source for a plain date (digits in any script).
pub const DATE: &str = r"\d{4}-\d{2}-\d{2}\Z";
/// Regex source for an ISO-8601 instant with a zone (digits in any script).
pub const DATETIME: &str =
    r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:\d{2})\Z";
/// Trust-signal names that establish a modification date, sorted for a stable order.
pub const MODIFIED_SIGNALS: &[&str] = &[
    "jsonld_date:datemodified",
    "meta_article_modified",
    "meta_date:datemodified",
    "meta_date:dcterms.modified",
    "meta_date:last-modified",
    "meta_og_updated",
    "microdata_date:datemodified",
];

type Row = Map<String, Value>;
/// Error of this module, carrying a message only.
type Res<T> = Result<T, String>;

fn quote(text: &str, safe: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        let c = byte as char;
        if c.is_ascii_alphanumeric() || "_.-~".contains(c) || (byte < 128 && safe.contains(c)) {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn hostname_port(netloc: &str) -> (String, Option<String>) {
    let host_port = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    if let Some(rest) = host_port.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').unwrap_or((rest, ""));
        return (host.to_string(), tail.strip_prefix(':').map(String::from));
    }
    match host_port.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), Some(p.to_string())),
        None => (host_port.to_string(), None),
    }
}

/// Normalise one sitemap URL: absolute http(s), no credentials/fragment, IDNA host, quoted path.
pub fn loc(value: &str) -> Res<String> {
    if value.chars().any(|c| (c as u32) < 32 || c as u32 == 127) {
        return Err("invalid sitemap URL".into());
    }
    let parts = urlsplit(value).map_err(|e| e.to_string())?;
    let scheme = parts.scheme.to_lowercase();
    let (host, port) = hostname_port(&parts.netloc);
    if !matches!(scheme.as_str(), "http" | "https") || host.is_empty() {
        return Err("sitemap URLs must be absolute HTTP(S) URLs".into());
    }
    if parts.netloc.contains('@') || !parts.fragment.is_empty() {
        return Err("sitemap URLs cannot contain credentials or fragments".into());
    }
    let port = match port.as_deref() {
        None | Some("") => None,
        Some(p) => Some(
            p.parse::<u32>()
                .ok()
                .filter(|n| *n <= 65535)
                .ok_or("Port could not be cast to integer value as invalid")?,
        ),
    };
    // Host encoding uses the IDNA 2003 codec, not the WHATWG `url` crate.
    let ascii = spiderhead_core::net::encode_idna(&host.to_lowercase())
        .map_err(|_| "invalid sitemap URL host")?;
    let mut host = ascii.to_lowercase();
    if host
        .chars()
        .any(|c| c.is_whitespace() || "\\/?#".contains(c))
    {
        return Err("invalid sitemap URL host".into());
    }
    if host.contains(':') && !host.starts_with('[') {
        host = format!("[{host}]");
    }
    let default = if scheme == "http" { 80 } else { 443 };
    if let Some(p) = port.filter(|p| *p != default) {
        host.push_str(&format!(":{p}"));
    }
    let path = if parts.path.is_empty() {
        "/"
    } else {
        parts.path.as_str()
    };
    let value = urlunsplit(
        &scheme,
        &host,
        &quote(path, "/%:@!$&'()*+,;=-._~"),
        &quote(&parts.query, "/%?:@!$&'()*+,;=-._~"),
        "",
    );
    if value.chars().count() >= 2048 {
        return Err("sitemap URL must contain fewer than 2048 characters".into());
    }
    Ok(value)
}

/// `(scheme, netloc)` of a URL.
pub fn origin(value: &str) -> (String, String) {
    urlsplit(value)
        .map(|p| (p.scheme, p.netloc))
        .unwrap_or_default()
}

fn valid_date(y: i64, m: i64, d: i64) -> bool {
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    y >= 1 && (1..=12).contains(&m) && d >= 1 && d <= days[(m - 1) as usize]
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn num(text: &str) -> Option<i64> {
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())).then(|| text.parse().ok())?
}

/// UTC instant `isoformat()` with a `Z` suffix; microseconds only when non-zero.
fn utc_iso(epoch: i64, micros: i64) -> String {
    let (y, m, d) = civil_from_days(epoch.div_euclid(86_400));
    let rem = epoch.rem_euclid(86_400);
    let fraction = if micros == 0 {
        String::new()
    } else {
        format!(".{micros:06}")
    };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}{fraction}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn iso_datetime(value: &str) -> Option<String> {
    let value = value.replace('Z', "+00:00");
    let (date, rest) = value.split_once('T')?;
    let dparts: Vec<&str> = date.split('-').collect();
    let (y, m, d) = (
        num(dparts.first()?)?,
        num(dparts.get(1)?)?,
        num(dparts.get(2)?)?,
    );
    if !valid_date(y, m, d) {
        return None;
    }
    let zone_at = rest.rfind(['+', '-'])?;
    let (clock, zone) = rest.split_at(zone_at);
    let (clock, fraction) = clock.split_once('.').map_or((clock, ""), |(c, f)| (c, f));
    let c: Vec<&str> = clock.split(':').collect();
    let (h, mi) = (num(c.first()?)?, num(c.get(1)?)?);
    let s = c.get(2).map_or(Some(0), |s| num(s))?;
    if h > 23 || mi > 59 || s > 59 {
        return None;
    }
    let mut micros_text: String = fraction.chars().take(6).collect();
    while micros_text.len() < 6 && !fraction.is_empty() {
        micros_text.push('0');
    }
    let micros = if fraction.is_empty() {
        0
    } else {
        num(&micros_text)?
    };
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let (zh, zm) = zone[1..].split_once(':')?;
    let offset = sign * (num(zh)? * 3600 + num(zm)? * 60);
    if num(zh)? > 23 || num(zm)? > 59 {
        return None;
    }
    Some(utc_iso(
        days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60 + s - offset,
        micros,
    ))
}

fn rfc2822(value: &str) -> Option<String> {
    let value = value.trim();
    let value = value.split_once(',').map_or(value, |(_, rest)| rest).trim();
    let tokens: Vec<&str> = value.split_whitespace().collect();
    if tokens.len() < 4 {
        return None;
    }
    let day = num(tokens[0])?;
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let month = months
        .iter()
        .position(|m| tokens[1].to_lowercase().starts_with(m))? as i64
        + 1;
    let mut year = num(tokens[2])?;
    if tokens[2].len() <= 2 {
        year += if year > 68 { 1900 } else { 2000 };
    }
    let clock: Vec<&str> = tokens[3].split(':').collect();
    let (h, mi) = (num(clock.first()?)?, num(clock.get(1)?)?);
    let s = clock.get(2).map_or(Some(0), |s| num(s))?;
    if !valid_date(year, month, day) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let zone = tokens.get(4).copied().unwrap_or("-0000");
    let offset = match zone.to_uppercase().as_str() {
        "GMT" | "UT" | "UTC" | "Z" => 0,
        "EST" => -5 * 3600,
        "EDT" => -4 * 3600,
        "CST" => -6 * 3600,
        "CDT" => -5 * 3600,
        "MST" => -7 * 3600,
        "MDT" => -6 * 3600,
        "PST" => -8 * 3600,
        "PDT" => -7 * 3600,
        "-0000" => return None, // naive: no zone information
        z if (z.starts_with('+') || z.starts_with('-')) && z.len() == 5 && z.is_ascii() => {
            let sign = if z.starts_with('-') { -1 } else { 1 };
            sign * (num(&z[1..3])? * 3600 + num(&z[3..5])? * 60)
        }
        _ => return None,
    };
    Some(utc_iso(
        days_from_civil(year, month, day) * 86_400 + h * 3600 + mi * 60 + s.min(59) - offset,
        0,
    ))
}

/// Normalise a date or zone-aware instant to sitemap form, or `None`.
/// `header` defaults to false.
pub fn date(value: &str, header: bool) -> Option<String> {
    let is_date = value.len() == 10
        && value.as_bytes()[4] == b'-'
        && value.as_bytes()[7] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit());
    if is_date {
        let (y, m, d) = (num(&value[..4])?, num(&value[5..7])?, num(&value[8..])?);
        return valid_date(y, m, d).then(|| value.to_string());
    }
    let looks_iso = value.len() >= 16
        && value.as_bytes()[10] == b'T'
        && fancy_regex::Regex::new(DATETIME).is_ok_and(|re| re.is_match(value).unwrap_or(false));
    if looks_iso {
        return iso_datetime(value);
    }
    if header {
        return rfc2822(value);
    }
    None
}

fn str_of<'a>(page: &'a Row, key: &str) -> &'a str {
    page.get(key).and_then(Value::as_str).unwrap_or("")
}

/// `lastmod` from `Last-Modified` or a high-confidence modification signal, with its source.
pub fn lastmod(page: &Row) -> Res<(Option<String>, String)> {
    let raw = match str_of(page, "headers") {
        "" => "[]",
        text => text,
    };
    let headers: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    for pair in headers.as_array().into_iter().flatten() {
        let (name, value) = (
            pair[0].as_str().unwrap_or(""),
            pair[1].as_str().unwrap_or(""),
        );
        if name.to_lowercase() == "last-modified" {
            if let Some(valid) = date(value.trim(), true) {
                return Ok((Some(valid), "last-modified".into()));
            }
        }
    }
    let signals: Value = match str_of(page, "trust_signals_json") {
        "" => json!({}),
        text => serde_json::from_str(text).map_err(|e| e.to_string())?,
    };
    for item in signals
        .get("dates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let signal = item["signal"].as_str().unwrap_or("");
        if item["confidence"] == json!("high")
            && MODIFIED_SIGNALS.contains(&signal.to_lowercase().as_str())
        {
            if let Some(valid) = date(item["value"].as_str().unwrap_or("").trim(), false) {
                return Ok((Some(valid), signal.to_string()));
            }
        }
    }
    Ok((None, "unavailable".into()))
}

/// Case-sensitive glob match (`*`, `?`, `[...]`).
/// Compiled patterns are cached process-wide so hot loops compile each pattern once.
fn compiled_fnmatch(pattern: &str) -> Option<fancy_regex::Regex> {
    static CACHE: OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Option<fancy_regex::Regex>>>,
    > = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut map = cache.lock().unwrap_or_else(|e| e.into_inner());
    if map.len() >= 256 && !map.contains_key(pattern) {
        map.clear();
    }
    map.entry(pattern.to_string())
        .or_insert_with(|| {
            let mut re = String::from("(?s)^");
            let chars: Vec<char> = pattern.chars().collect();
            let mut i = 0;
            while i < chars.len() {
                match chars[i] {
                    '*' => re.push_str(".*"),
                    '?' => re.push('.'),
                    '[' => {
                        let mut j = i + 1;
                        if j < chars.len() && chars[j] == '!' {
                            j += 1;
                        }
                        if j < chars.len() && chars[j] == ']' {
                            j += 1;
                        }
                        while j < chars.len() && chars[j] != ']' {
                            j += 1;
                        }
                        if j >= chars.len() {
                            re.push_str("\\[");
                        } else {
                            let body: String = chars[i + 1..j].iter().collect();
                            let (neg, body) = body
                                .strip_prefix('!')
                                .map_or((false, body.as_str()), |b| (true, b));
                            let escaped = body.replace('\\', "\\\\").replace('[', "\\[");
                            re.push_str(&format!("[{}{escaped}]", if neg { "^" } else { "" }));
                            i = j;
                        }
                    }
                    c => re.push_str(&fancy_regex::escape(&c.to_string())),
                }
                i += 1;
            }
            re.push('$');
            fancy_regex::Regex::new(&re).ok()
        })
        .clone()
}

/// Case-sensitive glob match (`*`, `?`, `[...]`).
pub fn fnmatchcase(name: &str, pattern: &str) -> bool {
    compiled_fnmatch(pattern).is_some_and(|r| r.is_match(name).unwrap_or(false))
}

fn truthy(page: &Row, key: &str) -> bool {
    match page.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// Why a page is left out of the sitemap (`""` when it is eligible).
pub fn exclusion(
    page: &Row,
    loc_value: &str,
    origin_value: &(String, String),
    exclude: &[String],
) -> String {
    if truthy(page, "blocked") {
        return "robots_blocked".into();
    }
    if page.get("status_code").and_then(Value::as_i64) != Some(200) {
        return "not_200".into();
    }
    if truthy(page, "error") || truthy(page, "redirect_url") {
        return "response_error_or_redirect".into();
    }
    let directives = robots_directives(&[
        page.get("meta_robots").and_then(Value::as_str),
        page.get("x_robots").and_then(Value::as_str),
    ]);
    if directives.iter().any(|d| d == "noindex") {
        return "noindex".into();
    }
    for name in ["canonical", "final_canonical", "final_url"] {
        let value = str_of(page, name);
        if !value.is_empty() {
            let resolved = urljoin(loc_value, value)
                .map_err(|e| e.to_string())
                .and_then(|j| loc(&j));
            match resolved {
                Ok(l) if l == loc_value => {}
                Ok(_) => {
                    return if name != "final_url" {
                        "non_canonical"
                    } else {
                        "redirected"
                    }
                    .into()
                }
                Err(_) => {
                    return if name != "final_url" {
                        "invalid_canonical"
                    } else {
                        "invalid_final_url"
                    }
                    .into()
                }
            }
        }
    }
    if &origin(loc_value) != origin_value {
        return "outside_origin".into();
    }
    if exclude.iter().any(|p| fnmatchcase(loc_value, p)) {
        return "user_excluded".into();
    }
    String::new()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// One XML entry (`<url>` or `<sitemap>`) followed by a newline.
/// Defaults: `lastmod` and `images` unset, `tag` is `url`.
pub fn entry(
    loc_value: &str,
    lastmod: Option<&str>,
    images: Option<&[String]>,
    tag: &str,
) -> Vec<u8> {
    let mut xml = format!("<{tag}><loc>{}</loc>", escape(loc_value));
    if let Some(value) = lastmod.filter(|v| !v.is_empty()) {
        xml.push_str(&format!("<lastmod>{}</lastmod>", escape(value)));
    }
    for image in images.unwrap_or_default() {
        xml.push_str(&format!(
            "<image:image><image:loc>{}</image:loc></image:image>",
            escape(image)
        ));
    }
    xml.push_str(&format!("</{tag}>\n"));
    xml.into_bytes()
}

fn scan_msg(message: &str) -> String {
    message.to_string()
}

/// Stream chunks, retaining only one URL entry and bounded file metadata.
pub struct Writer {
    destination: PathBuf,
    prefix: String,
    header: Vec<u8>,
    footer: Vec<u8>,
    /// Finished files: `{name, urls, bytes}`.
    pub files: Vec<Map<String, Value>>,
    stream: Option<(std::fs::File, PathBuf)>,
    count: i64,
    size: i64,
}

impl Writer {
    /// A writer of `<prefix>-NNNN.xml` parts in `destination`.
    /// `images` defaults to false.
    pub fn new(destination: &Path, prefix: &str, images: bool) -> Self {
        let header = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"{NAMESPACE}\"{}>\n",
            if images {
                format!(" xmlns:image=\"{IMAGE_NAMESPACE}\"")
            } else {
                String::new()
            }
        );
        Writer {
            destination: destination.to_path_buf(),
            prefix: prefix.to_string(),
            header: header.into_bytes(),
            footer: b"</urlset>\n".to_vec(),
            files: Vec::new(),
            stream: None,
            count: 0,
            size: 0,
        }
    }

    /// Append one entry, rolling to a new part at the URL or byte limit.
    pub fn add(&mut self, entry: &[u8], owned: &mut Owned) -> Res<()> {
        let (head, foot) = (self.header.len() as i64, self.footer.len() as i64);
        let len = entry.len() as i64;
        if head + len + foot > MAX_BYTES {
            return Err(scan_msg(
                "one sitemap entry exceeds the uncompressed byte limit",
            ));
        }
        if self.stream.is_some() && (self.count >= MAX_URLS || self.size + len + foot > MAX_BYTES) {
            self.finish()?;
        }
        if self.stream.is_none() {
            if self.files.len() as i64 >= MAX_URLS {
                return Err(scan_msg("sitemap index entry limit exceeded"));
            }
            let path =
                self.destination
                    .join(format!("{}-{:04}.xml", self.prefix, self.files.len() + 1));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .private_mode(0o600)
                .open(&path)
                .map_err(|e| e.to_string())?;
            let state = file_identity(&file).map_err(|e| e.to_string())?;
            owned.insert(
                path.to_string_lossy().into_owned(),
                (state.0 as i64, state.1 as i64),
            );
            file.write_all(&self.header).map_err(|e| e.to_string())?;
            self.size = head;
            self.count = 0;
            self.stream = Some((file, path));
        }
        if let Some((file, _)) = self.stream.as_mut() {
            file.write_all(entry).map_err(|e| e.to_string())?;
        }
        self.size += len;
        self.count += 1;
        Ok(())
    }

    /// Close the open part, recording its name, URL count and byte size.
    pub fn finish(&mut self) -> Res<()> {
        let Some((mut file, path)) = self.stream.take() else {
            return Ok(());
        };
        file.write_all(&self.footer).map_err(|e| e.to_string())?;
        file.flush().map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut item = Map::new();
        item.insert("name".into(), json!(name));
        item.insert("urls".into(), json!(self.count));
        item.insert("bytes".into(), json!(self.size + self.footer.len() as i64));
        self.files.push(item);
        Ok(())
    }

    /// Drop the open part without finishing it.
    pub fn close(&mut self) {
        self.stream = None;
    }
}

/// Seam for the pieces of image discovery that read retained documents.
pub trait PageDocuments {
    /// Read one retained complete HTML document (`bodies::read_document`).
    fn read_document(
        &self,
        con: &Connection,
        document_id: i64,
        max_decoded_bytes: i64,
    ) -> Result<String, StoreError>;
    /// Image URL sources of `html` resolved against `loc` (audio/video `<source>` excluded).
    fn image_sources(&self, html: &str, loc: &str) -> Vec<BTreeMap<String, String>>;
}

/// [`PageDocuments`] over the stored bodies and the HTML parser.
pub struct StoredDocuments;

impl PageDocuments for StoredDocuments {
    fn read_document(
        &self,
        con: &Connection,
        document_id: i64,
        max: i64,
    ) -> Result<String, StoreError> {
        crate::bodies::read_document(con, document_id, max)
    }

    fn image_sources(&self, html: &str, loc: &str) -> Vec<BTreeMap<String, String>> {
        let mut soup = parse_document(html);
        // A source element in audio/video is not image evidence.
        let selector = Selector::parse("source").expect("static selector");
        let stray: Vec<_> = soup
            .select(&selector)
            .filter(|el| {
                !el.ancestors()
                    .filter_map(ElementRef::wrap)
                    .any(|a| a.value().name() == "picture")
            })
            .map(|el| el.id())
            .collect();
        for id in stray {
            if let Some(mut node) = soup.tree.get_mut(id) {
                node.detach();
            }
        }
        let base = document_base_url(DocumentSource::Tree(&soup), loc);
        image_url_sources(&extract_url_sources(&soup, &base))
    }
}

fn bump(coverage: &mut BTreeMap<String, i64>, key: impl Into<String>) {
    *coverage.entry(key.into()).or_insert(0) += 1;
}

/// Image locations of one eligible page, with the coverage gaps found on the way.
pub fn images(
    con: &Connection,
    docs: &dyn PageDocuments,
    page: &Row,
    page_loc: &str,
    coverage: &mut BTreeMap<String, i64>,
    exclude: &[String],
) -> Res<Vec<String>> {
    let document_id = page
        .get("document_id")
        .and_then(Value::as_i64)
        .filter(|d| *d != 0);
    if document_id.is_none() || !str_of(page, "content_type").to_lowercase().contains("html") {
        bump(coverage, "body_unavailable");
        return Ok(Vec::new());
    }
    let html = match docs.read_document(con, document_id.unwrap_or(0), MAX_DOCUMENT_BYTES) {
        Ok(html) => html,
        Err(StoreError::ScanError(e)) => {
            bump(coverage, format!("body_unavailable: {}", e.message));
            return Ok(Vec::new());
        }
        Err(other) => return Err(other.to_string()),
    };
    let sources = docs.image_sources(&html, page_loc);
    bump(coverage, "bodies_read");
    let has_resource_status = con
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='resource_graph_fetches'",
            [],
            |_| Ok(()),
        )
        .is_ok();
    let (mut found, mut seen): (Vec<String>, BTreeSet<String>) = (Vec::new(), BTreeSet::new());
    for source in sources {
        if !matches!(
            source.get("tag").map(String::as_str),
            Some("img" | "source")
        ) {
            continue; // CSS URLs can name fonts and other non-image resources.
        }
        let Ok(image) = loc(source.get("url").map_or("", String::as_str)) else {
            bump(coverage, "invalid_image_url");
            continue;
        };
        if !seen.insert(image.clone()) {
            continue;
        }
        if exclude.iter().any(|p| fnmatchcase(&image, p)) {
            bump(coverage, "user_excluded_image");
            continue;
        }
        let row: Option<(Option<i64>, Option<String>, Option<String>, i64)> = con
            .query_row(
                "SELECT p.status_code,p.meta_robots,p.x_robots,\
                 EXISTS(SELECT 1 FROM context_items c WHERE c.kind='robots_blocked_url' \
                 AND c.item_key='url:'||u.url_id) AS blocked \
                 FROM urls u LEFT JOIN pages p USING(url_id) WHERE u.url=?",
                params![image],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok();
        if let Some((status, meta, x, blocked)) = &row {
            let noindex = robots_directives(&[
                Some(meta.as_deref().unwrap_or("")),
                Some(x.as_deref().unwrap_or("")),
            ])
            .iter()
            .any(|d| d == "noindex");
            if *blocked != 0 || status.is_some_and(|s| s != 200) || noindex {
                bump(coverage, "excluded_image");
                continue;
            }
        }
        let resource_status: Option<i64> = if has_resource_status {
            con.query_row(
                "SELECT status_code FROM resource_graph_fetches WHERE resolved_url=?",
                params![image],
                |r| r.get(0),
            )
            .ok()
            .flatten()
        } else {
            None
        };
        if resource_status.is_some_and(|s| s != 200) {
            bump(coverage, "excluded_image");
            bump(coverage, "resource_non_200_image");
            continue;
        }
        if found.len() as i64 >= MAX_IMAGES {
            bump(coverage, "image_limit_omitted");
            continue;
        }
        let page_status = row.as_ref().and_then(|r| r.0);
        if (row.is_none() || page_status.is_none()) && resource_status.is_none() {
            bump(coverage, "image_response_unavailable");
        }
        found.push(image);
    }
    if found.is_empty() {
        bump(coverage, "pages_without_images");
    }
    Ok(found)
}

fn sqlite_text(error: StoreError) -> String {
    match error {
        StoreError::ScanError(e) => e.message,
        other => other.to_string(),
    }
}

fn failure(message: String) -> Row {
    let mut out = Map::new();
    out.insert("ok".into(), json!(false));
    out.insert("format".into(), json!(FORMAT));
    out.insert("error".into(), json!(message));
    out
}

fn counts_json(counts: &BTreeMap<String, i64>) -> Value {
    Value::Object(counts.iter().map(|(k, v)| (k.clone(), json!(v))).collect())
}

/// Write a fresh directory of sitemap parts and an absolute-URL sitemap index.
///
/// `sitemap_base_url` is the public directory where the generated files will be deployed. It
/// must share the scan start URL's origin; deploy at the origin root to cover the full site.
/// `exclude` holds absolute-URL shell globs. Existing output directories are never overwritten.
/// Limits are uncompressed UTF-8 bytes including declarations, namespaces and closing tags.
/// Defaults: `include_images` is false and `exclude` is unset.
pub fn generate_sitemaps(
    scan: &Path,
    out_dir: &Path,
    sitemap_base_url: &str,
    include_images: bool,
    exclude: Option<&[String]>,
    docs: &dyn PageDocuments,
) -> Row {
    let destination = std::path::absolute(out_dir).unwrap_or_else(|_| out_dir.to_path_buf());
    let mut owned = Owned::new();
    let (mut created, mut published) = (false, false);
    let mut writers: Vec<Writer> = Vec::new();
    let result = run(
        scan,
        &destination,
        sitemap_base_url,
        include_images,
        exclude.unwrap_or(&[]),
        docs,
        &mut owned,
        &mut writers,
        &mut created,
        &mut published,
    );
    for writer in &mut writers {
        writer.close();
    }
    if created && !published {
        let paths: Vec<String> = owned.keys().cloned().collect();
        for path in paths.iter().rev() {
            let _ = unlink_owned(Path::new(path), &owned);
        }
        let _ = std::fs::remove_dir(&destination);
    }
    result.unwrap_or_else(failure)
}

#[allow(clippy::too_many_arguments)]
fn run(
    scan: &Path,
    destination: &Path,
    sitemap_base_url: &str,
    include_images: bool,
    exclude: &[String],
    docs: &dyn PageDocuments,
    owned: &mut Owned,
    writers: &mut Vec<Writer>,
    created: &mut bool,
    published: &mut bool,
) -> Res<Row> {
    if exclude.len() > 100
        || exclude
            .iter()
            .any(|p| p.is_empty() || p.chars().count() > 2048)
    {
        return Err("exclude must be a list of at most 100 nonempty URL globs".into());
    }
    let base = loc(sitemap_base_url)?;
    let base_parts = urlsplit(&base).map_err(|e| e.to_string())?;
    if !base_parts.query.is_empty() || !base_parts.path.ends_with('/') {
        return Err("sitemap_base_url must be an HTTP(S) directory URL ending in /".into());
    }
    let con = open_scan(scan, false, None).map_err(sqlite_text)?;
    let header: Row = con
        .query_row("SELECT * FROM scan", [], |r| {
            let mut map = Map::new();
            for (i, name) in r.as_ref().column_names().iter().enumerate() {
                map.insert((*name).to_string(), cell_value(r.get_ref(i)?));
            }
            Ok(map)
        })
        .map_err(|e| e.to_string())?;
    let start = match str_of(&header, "start_url") {
        "" => base.clone(),
        s => s.to_string(),
    };
    let scan_origin = origin(&loc(&start)?);
    if origin(&base) != scan_origin {
        return Err("sitemap_base_url must share the scan start URL origin".into());
    }
    // The main artifact remains mode=ro. Disk-backed TEMP storage deduplicates normalized
    // locations without retaining the URL population in memory.
    con.execute_batch("PRAGMA query_only=OFF")
        .map_err(|e| e.to_string())?;
    con.execute_batch("CREATE TEMP TABLE sitemap_emitted(loc TEXT PRIMARY KEY) WITHOUT ROWID")
        .map_err(|e| e.to_string())?;
    std::fs::DirBuilder::new()
        .private_mode(0o700)
        .create(destination)
        .map_err(|e| e.to_string())?;
    *created = true;
    writers.push(Writer::new(destination, "sitemap", false));
    writers.push(Writer::new(destination, "image-sitemap", true));
    let mut excluded: BTreeMap<String, i64> = BTreeMap::new();
    let mut lastmods: BTreeMap<String, i64> = BTreeMap::new();
    let mut image_coverage: BTreeMap<String, i64> = BTreeMap::new();
    let (mut total, mut emitted, mut image_urls, mut image_page_count) = (0i64, 0i64, 0i64, 0i64);
    // Joining one document-selected response avoids a second response population or an
    // in-memory page/URL list. Older scans fall back to their last page response.
    let query = "SELECT p.*,u.url,r.effective_headers_redacted_json AS headers,\
        EXISTS(SELECT 1 FROM context_items c WHERE c.kind='robots_blocked_url' \
        AND c.item_key='url:'||p.url_id) AS blocked FROM pages p JOIN urls u USING(url_id) \
        LEFT JOIN documents d ON d.document_id=p.document_id LEFT JOIN responses r \
        ON r.response_id=COALESCE(d.source_response_id,(SELECT rr.response_id FROM responses rr \
        WHERE rr.request_url_id=p.url_id AND rr.purpose='page' AND rr.method='GET' \
        ORDER BY rr.request_ordinal DESC LIMIT 1)) ORDER BY p.page_ordinal";
    {
        let mut stmt = con.prepare(query).map_err(|e| e.to_string())?;
        let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let mut rows = stmt
            .query(params_from_iter(Vec::<Sql>::new()))
            .map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            total += 1;
            let mut page = Map::new();
            for (i, name) in names.iter().enumerate() {
                page.insert(
                    name.clone(),
                    cell_value(row.get_ref(i).map_err(|e| e.to_string())?),
                );
            }
            let Ok(page_loc) = loc(str_of(&page, "url")) else {
                bump(&mut excluded, "invalid_url");
                continue;
            };
            let reason = exclusion(&page, &page_loc, &scan_origin, exclude);
            if !reason.is_empty() {
                bump(&mut excluded, reason);
                continue;
            }
            // Sitemap files have directory scope in the protocol.
            let loc_path = urlsplit(&page_loc).map(|p| p.path).unwrap_or_default();
            if !loc_path.starts_with(&base_parts.path) {
                bump(&mut excluded, "outside_sitemap_directory");
                continue;
            }
            let inserted = con
                .execute(
                    "INSERT OR IGNORE INTO temp.sitemap_emitted VALUES(?)",
                    params![page_loc],
                )
                .map_err(|e| e.to_string())?;
            if inserted == 0 {
                bump(&mut excluded, "duplicate_url");
                continue;
            }
            let (modified, source) = lastmod(&page)?;
            bump(&mut lastmods, source);
            writers[0].add(&entry(&page_loc, modified.as_deref(), None, "url"), owned)?;
            emitted += 1;
            if include_images {
                let found = images(&con, docs, &page, &page_loc, &mut image_coverage, exclude)?;
                if !found.is_empty() {
                    writers[1].add(
                        &entry(&page_loc, modified.as_deref(), Some(&found), "url"),
                        owned,
                    )?;
                    image_urls += found.len() as i64;
                    image_page_count += 1;
                }
            }
        }
    }
    for writer in writers.iter_mut() {
        writer.finish()?;
    }
    let files: Vec<Map<String, Value>> = writers.iter().flat_map(|w| w.files.clone()).collect();
    if files.is_empty() {
        return Err(scan_msg("scan contains no eligible sitemap URLs"));
    }
    if files.len() as i64 > MAX_URLS {
        return Err(scan_msg("sitemap index exceeds protocol limits"));
    }
    let mut index_chunks: Vec<Vec<u8>> = vec![format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<sitemapindex xmlns=\"{NAMESPACE}\">\n"
    )
    .into_bytes()];
    let footer = b"</sitemapindex>\n".to_vec();
    let mut size = index_chunks[0].len() + footer.len();
    for item in &files {
        let name = item["name"].as_str().unwrap_or("");
        let joined = urljoin(&base, name).map_err(|e| e.to_string())?;
        let chunk = entry(&loc(&joined)?, None, None, "sitemap");
        size += chunk.len();
        if size as i64 > MAX_BYTES {
            return Err(scan_msg("sitemap index exceeds protocol limits"));
        }
        index_chunks.push(chunk);
    }
    index_chunks.push(footer);
    let index_path = destination.join("sitemap-index.xml");
    write_file(&index_path, index_chunks, owned).map_err(sqlite_text)?;
    std::fs::File::open(destination)
        .and_then(|d| d.sync_all())
        .map_err(|e| e.to_string())?;
    *published = true;
    let gaps: i64 = image_coverage
        .iter()
        .filter(|(k, _)| k.starts_with("body_unavailable"))
        .map(|(_, v)| *v)
        .sum();
    let flag = |k: &str| header.get(k).and_then(Value::as_i64).unwrap_or(0) != 0;
    let partial = flag("crawl_partial") || header.get("lifecycle") != Some(&json!("finished"));
    let image_state = if !include_images {
        "not_requested"
    } else if emitted > 0 && gaps == emitted {
        "unavailable"
    } else if gaps > 0
        || image_coverage
            .get("image_limit_omitted")
            .copied()
            .unwrap_or(0)
            > 0
        || image_coverage
            .get("image_response_unavailable")
            .copied()
            .unwrap_or(0)
            > 0
    {
        "partial"
    } else {
        "complete"
    };
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("format".into(), json!(FORMAT));
    out.insert("path".into(), json!(destination.to_string_lossy()));
    out.insert("index".into(), json!(index_path.to_string_lossy()));
    out.insert("sitemap_base_url".into(), json!(base));
    out.insert(
        "files".into(),
        Value::Array(files.into_iter().map(Value::Object).collect()),
    );
    out.insert(
        "counts".into(),
        json!({
            "pages_read": total, "urls": emitted,
            "excluded": excluded.values().sum::<i64>(),
            "image_pages": image_page_count, "images": image_urls,
        }),
    );
    out.insert("exclusions".into(), counts_json(&excluded));
    let mut provenance = Map::new();
    for key in [
        "scan_uuid",
        "evidence_revision",
        "writer_revision",
        "lifecycle",
    ] {
        provenance.insert(key.into(), header.get(key).cloned().unwrap_or(Value::Null));
    }
    out.insert("provenance".into(), Value::Object(provenance));
    out.insert(
        "coverage".into(),
        json!({
            "state": if partial { "partial" } else { "complete" },
            "crawl_partial": partial,
            "lastmod_sources": counts_json(&lastmods),
            "images": {
                "state": image_state,
                "evidence": "retained HTML references; images without an observed HTTP status remain unverified",
                "counts": counts_json(&image_coverage),
            },
        }),
    );
    out.insert(
        "limits".into(),
        json!({
            "urls_per_file": MAX_URLS, "bytes_per_file": MAX_BYTES,
            "images_per_page": MAX_IMAGES, "document_bytes": MAX_DOCUMENT_BYTES,
        }),
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(v: Value) -> Row {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn loc_normalises_and_refuses() {
        assert_eq!(
            loc("HTTP://Example.COM:80/a b?q=1 2").unwrap(),
            "http://example.com/a%20b?q=1%202"
        );
        assert_eq!(loc("https://example.com").unwrap(), "https://example.com/");
        assert_eq!(
            loc("https://example.com:8443/x").unwrap(),
            "https://example.com:8443/x"
        );
        assert!(loc("ftp://example.com/").is_err());
        assert!(loc("https://u:p@example.com/").is_err());
        assert!(loc("https://example.com/#frag").is_err());
        assert!(loc("https://example.com/\u{1}").is_err());
        assert!(loc(&format!("https://example.com/{}", "a".repeat(2100))).is_err());
        assert_eq!(
            loc("https://bücher.example/").unwrap(),
            "https://xn--bcher-kva.example/"
        );
    }

    #[test]
    fn dates_are_normalised_or_refused() {
        assert_eq!(date("2026-01-02", false).as_deref(), Some("2026-01-02"));
        assert_eq!(date("2026-02-30", false), None);
        assert_eq!(
            date("2026-01-02T03:04:05+02:00", false).as_deref(),
            Some("2026-01-02T01:04:05Z")
        );
        assert_eq!(
            date("2026-01-02T03:04:05.5Z", false).as_deref(),
            Some("2026-01-02T03:04:05.500000Z")
        );
        assert_eq!(date("2026-01-02T03:04:05", false), None); // naive
        assert_eq!(
            date("Wed, 21 Oct 2015 07:28:00 GMT", true).as_deref(),
            Some("2015-10-21T07:28:00Z")
        );
        assert_eq!(date("Wed, 21 Oct 2015 07:28:00 GMT", false), None);
        assert_eq!(date("garbage", true), None);
        // review: a 5-byte zone with a multi-byte digit used to panic on byte slicing.
        assert_eq!(date("Wed, 21 Oct 2015 07:28:00 +1\u{e9}2", true), None);
    }

    #[test]
    fn lastmod_prefers_header_then_high_confidence_signal() {
        let p =
            page(json!({"headers": "[[\"Last-Modified\", \"Wed, 21 Oct 2015 07:28:00 GMT\"]]"}));
        assert_eq!(
            lastmod(&p).unwrap(),
            (Some("2015-10-21T07:28:00Z".into()), "last-modified".into())
        );
        let p = page(
            json!({"trust_signals_json": "{\"dates\":[{\"confidence\":\"high\",\"signal\":\"meta_article_modified\",\"value\":\"2026-03-04\"},{\"confidence\":\"low\",\"signal\":\"meta_og_updated\",\"value\":\"2020-01-01\"}]}"}),
        );
        assert_eq!(
            lastmod(&p).unwrap(),
            (Some("2026-03-04".into()), "meta_article_modified".into())
        );
        assert_eq!(
            lastmod(&page(json!({}))).unwrap(),
            (None, "unavailable".into())
        );
    }

    #[test]
    fn entries_escape_and_nest_images() {
        assert_eq!(
            entry("https://e.test/?a=1&b=2", Some("2026-01-01"), None, "url"),
            b"<url><loc>https://e.test/?a=1&amp;b=2</loc><lastmod>2026-01-01</lastmod></url>\n"
        );
        assert_eq!(
            entry("https://e.test/", None, Some(&["https://e.test/i.png".to_string()]), "url"),
            b"<url><loc>https://e.test/</loc><image:image><image:loc>https://e.test/i.png</image:loc></image:image></url>\n"
        );
        assert_eq!(
            entry("https://e.test/s.xml", None, None, "sitemap"),
            b"<sitemap><loc>https://e.test/s.xml</loc></sitemap>\n"
        );
    }

    #[test]
    fn fnmatch_matches_reference() {
        assert!(fnmatchcase("https://e.test/a/b", "https://e.test/a/*"));
        assert!(!fnmatchcase("https://e.test/B", "https://e.test/b"));
        assert!(fnmatchcase("https://e.test/a1", "https://e.test/a?"));
        assert!(fnmatchcase("a2", "a[0-9]"));
        assert!(!fnmatchcase("a2", "a[!0-9]"));
        assert!(fnmatchcase("a[", "a["));
    }

    #[test]
    fn exclusion_reasons_in_order() {
        let here = ("https".to_string(), "e.test".to_string());
        let ok = |extra: Value| {
            let mut p = page(
                json!({"blocked": 0, "status_code": 200, "error": "", "redirect_url": "", "meta_robots": "", "x_robots": ""}),
            );
            p.extend(page(extra));
            p
        };
        assert_eq!(
            exclusion(&ok(json!({"blocked": 1})), "https://e.test/", &here, &[]),
            "robots_blocked"
        );
        assert_eq!(
            exclusion(
                &ok(json!({"status_code": 404})),
                "https://e.test/",
                &here,
                &[]
            ),
            "not_200"
        );
        assert_eq!(
            exclusion(
                &ok(json!({"redirect_url": "/x"})),
                "https://e.test/",
                &here,
                &[]
            ),
            "response_error_or_redirect"
        );
    }

    #[test]
    fn writer_rolls_files_and_reports_bytes() {
        let dir = std::env::temp_dir().join("w2s6-sitemap-writer");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut owned = Owned::new();
        let mut writer = Writer::new(&dir, "sitemap", false);
        writer
            .add(&entry("https://e.test/a", None, None, "url"), &mut owned)
            .unwrap();
        writer
            .add(&entry("https://e.test/b", None, None, "url"), &mut owned)
            .unwrap();
        writer.finish().unwrap();
        assert_eq!(writer.files.len(), 1);
        assert_eq!(writer.files[0]["name"], json!("sitemap-0001.xml"));
        assert_eq!(writer.files[0]["urls"], json!(2));
        let size = std::fs::metadata(dir.join("sitemap-0001.xml"))
            .unwrap()
            .len() as i64;
        assert_eq!(writer.files[0]["bytes"], json!(size));
        assert_eq!(owned.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_arguments_return_error_payload() {
        let dir = std::env::temp_dir().join("w2s6-sitemap-args");
        let _ = std::fs::remove_dir_all(&dir);
        let r = generate_sitemaps(
            Path::new("/nonexistent.sqlite"),
            &dir,
            "https://e.test/dir",
            false,
            None,
            &StoredDocuments,
        );
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("ending in /"));
        let r = generate_sitemaps(
            Path::new("/nonexistent.sqlite"),
            &dir,
            "https://e.test/",
            false,
            Some(&[String::new()]),
            &StoredDocuments,
        );
        assert!(r["error"].as_str().unwrap().starts_with("exclude must be"));
        assert!(!dir.exists());
    }
}
