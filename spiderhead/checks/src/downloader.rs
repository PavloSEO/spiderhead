//! Bounded image downloader with deterministic, filesystem-safe paths.
//!
//! Downloads a list of image URLs to disk, deriving each target filename from
//! the URL and the response `Content-Type`. Two folder layouts: `flat` puts
//! every file directly into the output directory, `domain-path` nests them
//! under `<output>/<host>/<url path>/`. Filename derivation lives in the pure
//! [`target_path`] so it is testable without the network or the filesystem.
//!
//! [`download_images`] never fails as a whole: each URL yields one result
//! object carrying an `ok` flag and, on failure, an `error` message. Every
//! successful download (fresh or skipped) is recorded in
//! `<output>/manifest.json`, mapping each URL to the file it produced, so a
//! later log-scan `--images-dir` run can compare a crawl's claims against
//! reality.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde_json::{json, Map, Value};

use spiderhead_parse::py_url::urlsplit;

/// The browser identity used when the caller does not name one.
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";

/// Windows-reserved characters plus control characters (0x00-0x1F).
const UNSAFE_CHARS_PATTERN: &str = r#"[<>:"/\\|?*\x00-\x1F]"#;
const WHITESPACE_PATTERN: &str = r"\s+";
const UNDERSCORES_PATTERN: &str = r"_+";

/// Lazily compiled, failure-tolerant pattern cache (mirrors `safe_segment`'s
/// three rewrites; none of the patterns can fail to compile).
fn compiled(
    cell: &'static OnceLock<Option<Regex>>,
    pattern: &'static str,
) -> Option<&'static Regex> {
    cell.get_or_init(|| Regex::new(pattern).ok()).as_ref()
}

fn unsafe_chars() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    compiled(&CELL, UNSAFE_CHARS_PATTERN)
}

fn whitespace_runs() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    compiled(&CELL, WHITESPACE_PATTERN)
}

fn underscore_runs() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    compiled(&CELL, UNDERSCORES_PATTERN)
}

/// Sanitize an arbitrary value into one safe filename/path segment: unsafe
/// characters become `_`, whitespace runs collapse to `-`, underscore runs
/// collapse to a single `_`, the result is truncated to 160 characters, and an
/// empty result falls back to `"file"`.
pub fn safe_segment(value: &str) -> String {
    let mut text = value.to_owned();
    if let Some(pattern) = unsafe_chars() {
        text = pattern.replace_all(&text, "_").into_owned();
    }
    if let Some(pattern) = whitespace_runs() {
        text = pattern.replace_all(&text, "-").into_owned();
    }
    if let Some(pattern) = underscore_runs() {
        text = pattern.replace_all(&text, "_").into_owned();
    }
    let text: String = text.chars().take(160).collect();
    if text.is_empty() {
        "file".to_owned()
    } else {
        text
    }
}

/// File extension (with leading dot) inferred from a MIME type, or `""` when
/// the type is unknown.
pub fn pick_extension_from_content_type(content_type: Option<&str>) -> String {
    let ct = content_type.unwrap_or("").to_lowercase();
    if ct.contains("image/jpeg") || ct.contains("image/jpg") {
        return ".jpg".to_owned();
    }
    if ct.contains("image/png") {
        return ".png".to_owned();
    }
    if ct.contains("image/webp") {
        return ".webp".to_owned();
    }
    if ct.contains("image/gif") {
        return ".gif".to_owned();
    }
    if ct.contains("image/svg") {
        return ".svg".to_owned();
    }
    if ct.contains("image/avif") {
        return ".avif".to_owned();
    }
    if ct.contains("image/tiff") {
        return ".tiff".to_owned();
    }
    String::new()
}

/// POSIX `os.path.basename`: a trailing separator yields an empty name.
fn py_basename(pathname: &str) -> &str {
    match pathname.rfind('/') {
        Some(index) => &pathname[index + 1..],
        None => pathname,
    }
}

/// POSIX `os.path.splitext` including CPython's leading-dot rule (`.png` has no
/// extension; a run of dots at the start defeats it). Only ever called on a
/// single path segment, so byte indices on `.` are safe.
fn py_splitext(name: &str) -> (&str, &str) {
    let Some(dot) = name.rfind('.') else {
        return (name, "");
    };
    let mut index = dot as isize - 1;
    while index > -1 {
        if name.as_bytes()[index as usize] != b'.' {
            return (&name[..dot], &name[dot..]);
        }
        index -= 1;
    }
    (name, "")
}

/// Python's `parts.hostname`: drop userinfo, unbracket IPv6 literals, drop the
/// port, lower-case.
fn hostname_from_netloc(netloc: &str) -> String {
    let after_userinfo = netloc.rsplit('@').next().unwrap_or("");
    if after_userinfo.starts_with('[') {
        return after_userinfo
            .trim_start_matches('[')
            .split(']')
            .next()
            .unwrap_or("")
            .to_lowercase();
    }
    after_userinfo
        .split(':')
        .next()
        .unwrap_or("")
        .to_lowercase()
}

/// Parse an absolute HTTP-style URL with strict scheme and host requirements.
/// Returns `(hostname, path, origin)` or `None` when the string has no scheme
/// or no host.
fn parse_url(url: &str) -> Option<(String, String, String)> {
    let parts = urlsplit(url).ok()?;
    if parts.scheme.is_empty() {
        return None;
    }
    let hostname = hostname_from_netloc(&parts.netloc);
    if hostname.is_empty() {
        return None;
    }
    Some((
        hostname,
        if parts.path.is_empty() {
            String::new()
        } else {
            parts.path.clone()
        },
        format!("{}://{}", parts.scheme, parts.netloc),
    ))
}

/// Derive a base filename from a URL host and path: the last path segment when
/// present (query stripped), otherwise `<host>-image`.
pub fn infer_base_name(hostname: &str, pathname: &str) -> String {
    let raw_name = py_basename(pathname);
    let no_query_name = raw_name.split('?').next().unwrap_or("");
    if !no_query_name.is_empty() {
        return no_query_name.to_owned();
    }
    format!("{}-image", safe_segment(hostname))
}

/// Compute the destination path for a downloaded image. Deterministic and
/// side-effect free for valid URLs — it touches neither the filesystem nor
/// uniqueness (the download loop layers that on top).
///
/// * `structure == "flat"`  -> `<output>/<base_name>`
/// * otherwise (`domain-path`) -> `<output>/<host>/<path dirs>/<base_name>`
pub fn target_path(
    url: &str,
    output_dir: &Path,
    structure: &str,
    content_type: Option<&str>,
) -> PathBuf {
    let Some((hostname, pathname, _origin)) = parse_url(url) else {
        // Invalid URL: fall back to a timestamped binary name.
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or(0);
        return output_dir.join(format!("image_{millis}.bin"));
    };

    let base_name = infer_base_name(&hostname, &pathname);
    // A fallback name such as `example.com-image` contains a dot but no file
    // extension. Only a real final URL path segment may contribute an existing
    // extension.
    let ext = if py_basename(&pathname).is_empty() {
        String::new()
    } else {
        py_splitext(&base_name).1.to_owned()
    };
    let base_name = if ext.is_empty() {
        let from_type = pick_extension_from_content_type(content_type);
        format!(
            "{base_name}{}",
            if from_type.is_empty() {
                ".bin"
            } else {
                &from_type
            }
        )
    } else {
        base_name
    };
    let base_name = safe_segment(&base_name);

    if structure == "flat" {
        return output_dir.join(base_name);
    }

    let mut segments: Vec<String> = pathname
        .split('/')
        .filter(|part| !part.is_empty())
        .map(safe_segment)
        .collect();
    segments.pop();
    let mut path = output_dir.join(safe_segment(&hostname));
    for segment in segments {
        path = path.join(segment);
    }
    path.join(base_name)
}

/// Return `file_path` or, if it exists, a `name-N.ext` variant that does not.
fn unique_path(file_path: PathBuf) -> PathBuf {
    if !file_path.exists() {
        return file_path;
    }
    let directory = file_path.parent().unwrap_or(Path::new("")).to_path_buf();
    let name = file_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned();
    let (base, ext) = py_splitext(&name);
    let mut candidate = file_path;
    let mut index = 1;
    while candidate.exists() {
        candidate = directory.join(format!("{base}-{index}{ext}"));
        index += 1;
    }
    candidate
}

/// Path of `path` relative to `start`, as `os.path.relpath` behaves for the
/// paths this module builds (always beneath `start`).
fn relpath(path: &Path, start: &Path) -> String {
    path.strip_prefix(start)
        .map(|relative| relative.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string_lossy().into_owned())
}

fn is_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

/// One response body plus the metadata the download loop needs.
#[derive(Debug, Clone)]
pub struct ImageReply {
    pub status_code: u16,
    /// The URL the server finally served, when the transport tracks redirects.
    pub final_url: Option<String>,
    /// Raw `Content-Type` header value (`""` when absent).
    pub content_type: String,
    pub body: Vec<u8>,
}

/// Transport seam so unit tests can serve bytes from memory.
pub trait ImageTransport {
    /// Fetch one URL with the caller's headers applied; an `Err` is a failure
    /// the retry loop may repeat.
    fn get(&self, url: &str, headers: &[(String, String)]) -> Result<ImageReply, String>;
}

/// Options accepted by [`download_images`], mirroring the Python defaults.
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    /// `"flat"` or `"domain-path"`.
    pub structure: String,
    /// Skip URLs already recorded in `manifest.json`, or whose preview path
    /// already exists for a URL never recorded there.
    pub skip_existing: bool,
    /// Per-request timeout in seconds (used by the caller to build its client).
    pub timeout: f64,
    /// Abort a download that grows past this size.
    pub max_bytes: Option<i64>,
    /// Extra attempts per URL on failure.
    pub retries: u32,
    /// Overrides [`DEFAULT_USER_AGENT`].
    pub user_agent: String,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            structure: "domain-path".to_owned(),
            skip_existing: true,
            timeout: 30.0,
            max_bytes: None,
            retries: 1,
            user_agent: DEFAULT_USER_AGENT.to_owned(),
        }
    }
}

impl DownloadOptions {
    /// Parse the tool's `options` object with the Python fallbacks: absent
    /// members keep their defaults, an explicit `null` `retries` means zero.
    pub fn parse(options: Option<&Value>) -> Self {
        let empty = Map::new();
        let opts = options.and_then(Value::as_object).unwrap_or(&empty);
        let retries = match opts.get("retries") {
            None => 1,
            Some(value) if value.is_null() => 0,
            Some(value) => value
                .as_i64()
                .or_else(|| value.as_f64().map(|float| float as i64))
                .unwrap_or(0)
                .max(0) as u32,
        };
        Self {
            structure: opts
                .get("structure")
                .and_then(Value::as_str)
                .unwrap_or("domain-path")
                .to_owned(),
            skip_existing: opts
                .get("skip_existing")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            timeout: opts.get("timeout").and_then(Value::as_f64).unwrap_or(30.0),
            max_bytes: opts.get("max_bytes").and_then(Value::as_i64),
            retries,
            user_agent: opts
                .get("user_agent")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or(DEFAULT_USER_AGENT)
                .to_owned(),
        }
    }
}

/// One manifest row: the file path relative to the output directory and the
/// recorded byte count.
#[derive(Debug, Clone)]
struct ManifestEntry {
    path: String,
    bytes: Option<i64>,
}

fn load_manifest(output_dir: &Path) -> BTreeMap<String, ManifestEntry> {
    let Ok(raw) = fs::read_to_string(output_dir.join("manifest.json")) else {
        return BTreeMap::new();
    };
    let Ok(document) = serde_json::from_str::<Value>(&raw) else {
        return BTreeMap::new();
    };
    let entries: &[Value] = match document.as_object().and_then(|object| object.get("images")) {
        Some(value) => value.as_array().map(Vec::as_slice).unwrap_or_default(),
        None => document.as_array().map(Vec::as_slice).unwrap_or_default(),
    };
    // A stale entry — naming a file that no longer exists — would make
    // skip_existing falsely report a URL as already downloaded, and would
    // carry a dangling reference forward into the next manifest write.
    let mut live = BTreeMap::new();
    for entry in entries {
        let Some(object) = entry.as_object() else {
            continue;
        };
        let Some(url) = object
            .get("url")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
        else {
            continue;
        };
        let Some(rel_path) = object
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
        else {
            continue;
        };
        if is_file(&output_dir.join(rel_path)) {
            live.insert(
                url.to_owned(),
                ManifestEntry {
                    path: rel_path.to_owned(),
                    bytes: object.get("bytes").and_then(Value::as_i64),
                },
            );
        }
    }
    live
}

/// Write the manifest the log-scan `--images-dir` reader maps files back to
/// their URLs with. Without it there is no way to tell which URL a file on
/// disk came from. Write errors are suppressed.
fn write_manifest(output_dir: &Path, entries: &BTreeMap<String, ManifestEntry>) {
    let images: Vec<Value> = entries
        .iter()
        .map(|(url, entry)| json!({"url": url, "path": entry.path, "bytes": entry.bytes}))
        .collect();
    let document = json!({"images": images});
    let Ok(mut body) = serde_json::to_string_pretty(&document) else {
        return;
    };
    body.push('\n');
    let _ = fs::write(output_dir.join("manifest.json"), body);
}

/// Fetch a single URL and write it to disk. `Ok` carries the result object
/// (including a definitive non-200 failure); `Err` is a retryable exception
/// whose text becomes the result's `error` message.
fn download_one(
    transport: &dyn ImageTransport,
    url: &str,
    output_dir: &Path,
    structure: &str,
    max_bytes: Option<i64>,
    user_agent: &str,
) -> Result<Value, String> {
    let parsed = parse_url(url);
    let mut headers: Vec<(String, String)> = vec![("User-Agent".to_owned(), user_agent.to_owned())];
    if let Some((_, _, origin)) = &parsed {
        headers.push(("Referer".to_owned(), origin.clone()));
    }

    let reply = transport.get(url, &headers)?;
    let status = reply.status_code;
    if status != 200 {
        return Ok(json!({
            "url": url,
            "path": null,
            "bytes": null,
            "content_type": null,
            "status": status,
            "ok": false,
            "error": format!("HTTP {status}"),
        }));
    }

    let final_url = reply
        .final_url
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or(url);
    let content_type = reply.content_type.clone();
    let dest = unique_path(target_path(
        final_url,
        output_dir,
        structure,
        Some(&content_type),
    ));
    fs::create_dir_all(dest.parent().unwrap_or(Path::new(".")))
        .map_err(|error| error.to_string())?;

    let size = reply.body.len() as i64;
    if let Some(limit) = max_bytes {
        if size > limit {
            return Err(format!("Size limit exceeded ({limit} bytes)"));
        }
    }
    // A failed write leaves no file behind that could be mistaken for a good one.
    if let Err(error) = fs::write(&dest, &reply.body) {
        let _ = fs::remove_file(&dest);
        return Err(error.to_string());
    }

    Ok(json!({
        "url": url,
        "path": dest.to_string_lossy(),
        "bytes": size,
        "content_type": content_type,
        "status": status,
        "ok": true,
    }))
}

/// Download one URL end to end, applying the manifest and preview skip rules.
fn process_url(
    url: &str,
    output_dir: &Path,
    options: &DownloadOptions,
    transport: &dyn ImageTransport,
    manifest_entries: &mut BTreeMap<String, ManifestEntry>,
) -> Value {
    // The manifest, not a guessed filename, is authoritative once a URL has
    // been downloaded: for a URL with no extension, guessing a preview name
    // before the response is fetched cannot know the real Content-Type and
    // would miss the file the previous run actually wrote (#227).
    if options.skip_existing {
        if let Some(recorded) = manifest_entries.get(url).cloned() {
            let recorded_path = output_dir.join(&recorded.path);
            if is_file(&recorded_path) {
                return match fs::metadata(&recorded_path) {
                    Ok(meta) => json!({
                        "url": url,
                        "path": recorded_path.to_string_lossy(),
                        "bytes": meta.len() as i64,
                        "content_type": null,
                        "status": null,
                        "ok": true,
                        "skipped": true,
                    }),
                    Err(error) => json!({
                        "url": url,
                        "path": null,
                        "bytes": null,
                        "content_type": null,
                        "status": null,
                        "ok": false,
                        "error": error.to_string(),
                    }),
                };
            }
        }
    }

    // Fallback for a URL the manifest has never seen: a real extension in the
    // URL path makes this guess exact, and it also covers files placed in the
    // output directory by something other than this function.
    let preview = target_path(url, output_dir, &options.structure, Some(""));
    if options.skip_existing && preview.exists() {
        match fs::metadata(&preview) {
            Ok(meta) => {
                manifest_entries.insert(
                    url.to_owned(),
                    ManifestEntry {
                        path: relpath(&preview, output_dir),
                        bytes: Some(meta.len() as i64),
                    },
                );
                return json!({
                    "url": url,
                    "path": preview.to_string_lossy(),
                    "bytes": meta.len() as i64,
                    "content_type": null,
                    "status": null,
                    "ok": true,
                    "skipped": true,
                });
            }
            Err(error) => {
                return json!({
                    "url": url,
                    "path": null,
                    "bytes": null,
                    "content_type": null,
                    "status": null,
                    "ok": false,
                    "error": error.to_string(),
                });
            }
        }
    }

    let mut last_error: Option<String> = None;
    let mut item: Option<Value> = None;
    for _ in 0..=options.retries {
        match download_one(
            transport,
            url,
            output_dir,
            &options.structure,
            options.max_bytes,
            &options.user_agent,
        ) {
            // A non-200 HTTP result is still a definitive answer; keep it
            // rather than retrying blindly.
            Ok(value) => {
                item = Some(value);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }

    match item {
        Some(value) => {
            // Only a genuinely finished file earns a manifest entry: a failed
            // or non-200 item carries path=null, so this can never point at a
            // partial download.
            if value.get("ok").and_then(Value::as_bool).unwrap_or(false)
                && value.get("path").and_then(Value::as_str).is_some()
            {
                let path = PathBuf::from(value["path"].as_str().unwrap_or_default());
                manifest_entries.insert(
                    url.to_owned(),
                    ManifestEntry {
                        path: relpath(&path, output_dir),
                        bytes: value.get("bytes").and_then(Value::as_i64),
                    },
                );
            }
            value
        }
        None => json!({
            "url": url,
            "path": null,
            "bytes": null,
            "content_type": null,
            "status": null,
            "ok": false,
            "error": last_error.unwrap_or_else(|| "Failed to download file".to_owned()),
        }),
    }
}

/// Download a list of image URLs into `output_dir`. Returns one result object
/// per input URL, shaped `{url, path, bytes, content_type, status, ok,
/// error?, skipped?}`. Never fails as a whole: transport and I/O errors are
/// captured per URL.
pub fn download_images(
    urls: &[String],
    output_dir: &Path,
    options: &DownloadOptions,
    transport: &dyn ImageTransport,
) -> Vec<Value> {
    let mut results = Vec::with_capacity(urls.len());
    let mut manifest_entries = load_manifest(output_dir);
    for url in urls {
        results.push(process_url(
            url,
            output_dir,
            options,
            transport,
            &mut manifest_entries,
        ));
    }
    if !manifest_entries.is_empty() {
        write_manifest(output_dir, &manifest_entries);
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    const SYNTHETIC_PNG: &[u8] = b"\x89PNG\r\n\x1a\nsynthetic-image";

    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "spiderhead-downloader-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir created");
        dir
    }

    fn reply(status_code: u16, content_type: &str, body: &[u8]) -> Result<ImageReply, String> {
        Ok(ImageReply {
            status_code,
            final_url: None,
            content_type: content_type.to_owned(),
            body: body.to_vec(),
        })
    }

    /// One recorded call: the requested URL and the headers it was sent with.
    type RecordedRequest = (String, Vec<(String, String)>);

    struct FakeTransport {
        replies: HashMap<String, Result<ImageReply, String>>,
        requests: RefCell<Vec<RecordedRequest>>,
    }

    impl FakeTransport {
        fn with(url: &str, result: Result<ImageReply, String>) -> Self {
            Self {
                replies: HashMap::from([(url.to_owned(), result)]),
                requests: RefCell::new(Vec::new()),
            }
        }

        fn request_count(&self) -> usize {
            self.requests.borrow().len()
        }

        fn last_headers(&self) -> Vec<(String, String)> {
            self.requests
                .borrow()
                .last()
                .map(|(_, headers)| headers.clone())
                .unwrap_or_default()
        }
    }

    impl ImageTransport for FakeTransport {
        fn get(&self, url: &str, headers: &[(String, String)]) -> Result<ImageReply, String> {
            self.requests
                .borrow_mut()
                .push((url.to_owned(), headers.to_vec()));
            match self.replies.get(url) {
                Some(Ok(reply)) => Ok(reply.clone()),
                Some(Err(error)) => Err(error.clone()),
                None => Err(format!("no route for {url}")),
            }
        }
    }

    // from checks/downloader.py::__main__ (offline self-check asserts)
    #[test]
    fn safe_segment_rules_match_the_oracle_self_check() {
        assert_eq!(safe_segment("a b/c"), "a-b_c");
        assert_eq!(safe_segment(""), "file");
        assert_eq!(safe_segment("  "), "-"); // whitespace run collapses to a single dash
    }

    // from checks/downloader.py::__main__ (offline self-check asserts)
    #[test]
    fn pick_extension_rules_match_the_oracle_self_check() {
        assert_eq!(pick_extension_from_content_type(Some("image/jpeg")), ".jpg");
        assert_eq!(
            pick_extension_from_content_type(Some("image/svg+xml")),
            ".svg"
        );
        assert_eq!(pick_extension_from_content_type(Some("text/html")), "");
    }

    // from checks/downloader.py::__main__ (offline self-check asserts)
    #[test]
    fn flat_layout_uses_the_url_filename() {
        let path = target_path(
            "https://example.com/pics/photo.png",
            Path::new("/out"),
            "flat",
            Some("image/png"),
        );
        assert_eq!(path, PathBuf::from("/out/photo.png"));
    }

    // from checks/downloader.py::__main__ (offline self-check asserts)
    #[test]
    fn domain_path_layout_nests_under_host_and_dirs() {
        let path = target_path(
            "https://example.com/a/b/photo",
            Path::new("/out"),
            "domain-path",
            Some("image/webp"),
        );
        assert_eq!(path, PathBuf::from("/out/example.com/a/b/photo.webp"));
    }

    // from checks/downloader.py::__main__ (offline self-check asserts)
    #[test]
    fn no_extension_and_no_type_falls_back_to_bin() {
        let path = target_path(
            "https://example.com/img/pic",
            Path::new("/out"),
            "flat",
            Some(""),
        );
        assert_eq!(path, PathBuf::from("/out/pic.bin"));
    }

    // from tests/test_tools_core.py::test_downloader_host_only_url_gets_content_type_extension
    #[test]
    fn host_only_url_gets_content_type_extension() {
        let path = target_path(
            "https://example.com/",
            Path::new("/out"),
            "domain-path",
            Some("image/gif"),
        );
        assert_eq!(
            path,
            PathBuf::from("/out/example.com/example.com-image.gif")
        );
    }

    // from checks/downloader.py::target_path (invalid-URL timestamp fallback)
    #[test]
    fn invalid_url_falls_back_to_timestamped_bin() {
        let path = target_path(
            "not a url",
            Path::new("/out"),
            "domain-path",
            Some("image/png"),
        );
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        assert_eq!(path.parent(), Some(Path::new("/out")));
        assert!(name.starts_with("image_"), "{name}");
        assert!(name.ends_with(".bin"), "{name}");
    }

    // from tests/test_tools_core.py::test_download_images_writes_manifest_log_scan_can_read
    #[test]
    fn download_writes_manifest_a_reader_can_map_back() {
        let url = "https://assets.example.test/rendered-image.png";
        let output_dir = temp_dir("manifest");
        let transport = FakeTransport::with(url, reply(200, "image/png", SYNTHETIC_PNG));

        let results = download_images(
            &[url.to_owned()],
            &output_dir,
            &DownloadOptions::default(),
            &transport,
        );

        assert_eq!(results.len(), 1);
        let item = &results[0];
        assert_eq!(item["ok"], json!(true));
        let expected_path = output_dir
            .join("assets.example.test")
            .join("rendered-image.png");
        assert_eq!(item["path"], json!(expected_path.to_string_lossy()));
        assert_eq!(item["bytes"], json!(SYNTHETIC_PNG.len() as i64));
        assert_eq!(item["content_type"], json!("image/png"));
        assert_eq!(item["status"], json!(200));
        assert_eq!(
            fs::read(&expected_path).expect("downloaded file"),
            SYNTHETIC_PNG,
        );
        let manifest: Value = serde_json::from_str(
            &fs::read_to_string(output_dir.join("manifest.json")).expect("manifest"),
        )
        .expect("manifest json");
        assert_eq!(
            manifest,
            json!({"images": [{"url": url, "path": "assets.example.test/rendered-image.png", "bytes": SYNTHETIC_PNG.len() as i64}]})
        );
        let _ = fs::remove_dir_all(&output_dir);
    }

    // from tests/test_tools_core.py::test_download_images_skip_existing_reuses_manifest_for_extensionless_url
    #[test]
    fn skip_existing_reuses_the_manifest_record_for_extensionless_url() {
        let url = "https://assets.example.test/rendered-image";
        let output_dir = temp_dir("skip-existing");
        let options = DownloadOptions::default();
        let first = FakeTransport::with(url, reply(200, "image/png", SYNTHETIC_PNG));
        download_images(&[url.to_owned()], &output_dir, &options, &first);
        assert_eq!(first.request_count(), 1);

        let second = FakeTransport::with(url, reply(200, "image/png", SYNTHETIC_PNG));
        let results = download_images(&[url.to_owned()], &output_dir, &options, &second);

        assert_eq!(second.request_count(), 0);
        let item = &results[0];
        assert_eq!(item["ok"], json!(true));
        assert_eq!(item["skipped"], json!(true));
        let expected_path = output_dir
            .join("assets.example.test")
            .join("rendered-image.png");
        assert_eq!(item["path"], json!(expected_path.to_string_lossy()));
        assert!(!output_dir
            .join("assets.example.test")
            .join("rendered-image-1.png")
            .exists());
        let _ = fs::remove_dir_all(&output_dir);
    }

    // from tests/test_tools_core.py::test_download_images_sends_the_caller_user_agent
    #[test]
    fn download_sends_the_caller_user_agent() {
        let url = "https://assets.example.test/rendered-image.png";
        let output_dir = temp_dir("user-agent");
        let transport = FakeTransport::with(url, reply(200, "image/png", SYNTHETIC_PNG));
        let options = DownloadOptions {
            user_agent: "AuditAgent/1.0".to_owned(),
            ..DownloadOptions::default()
        };
        download_images(&[url.to_owned()], &output_dir, &options, &transport);

        let headers = transport.last_headers();
        assert!(headers.contains(&("User-Agent".to_owned(), "AuditAgent/1.0".to_owned())));
        // The per-request headers also carry the origin as Referer.
        assert!(headers.contains(&(
            "Referer".to_owned(),
            "https://assets.example.test".to_owned()
        )));
        let _ = fs::remove_dir_all(&output_dir);
    }

    // from tests/test_tools_core.py::test_download_images_sends_the_caller_user_agent
    #[test]
    fn download_falls_back_to_the_default_user_agent() {
        let url = "https://assets.example.test/rendered-image.png";
        let output_dir = temp_dir("default-agent");
        let transport = FakeTransport::with(url, reply(200, "image/png", SYNTHETIC_PNG));
        download_images(
            &[url.to_owned()],
            &output_dir,
            &DownloadOptions::default(),
            &transport,
        );

        assert!(transport
            .last_headers()
            .contains(&("User-Agent".to_owned(), DEFAULT_USER_AGENT.to_owned())));
        let _ = fs::remove_dir_all(&output_dir);
    }

    // from spiderhead/checks/downloader.py::download_one (definitive non-200 branch)
    #[test]
    fn non_200_is_a_definitive_failure_without_file_or_manifest() {
        let url = "https://example.com/missing.png";
        let output_dir = temp_dir("non-200");
        let transport = FakeTransport::with(url, reply(404, "text/html", b"nope"));
        let results = download_images(
            &[url.to_owned()],
            &output_dir,
            &DownloadOptions::default(),
            &transport,
        );

        assert_eq!(results[0]["ok"], json!(false));
        assert_eq!(results[0]["error"], json!("HTTP 404"));
        assert_eq!(results[0]["status"], json!(404));
        assert_eq!(results[0]["path"], json!(null));
        assert!(!output_dir.join("example.com").join("missing.png").exists());
        assert!(!output_dir.join("manifest.json").exists());
        let _ = fs::remove_dir_all(&output_dir);
    }

    // from spiderhead/checks/downloader.py::download_images (retry loop)
    #[test]
    fn transport_errors_retry_then_report_the_last_error() {
        let url = "https://example.com/broken.png";
        let output_dir = temp_dir("retries");
        let transport = FakeTransport::with(url, Err("connection reset".to_owned()));
        let options = DownloadOptions {
            retries: 1,
            ..DownloadOptions::default()
        };
        let results = download_images(&[url.to_owned()], &output_dir, &options, &transport);

        assert_eq!(transport.request_count(), 2);
        assert_eq!(results[0]["ok"], json!(false));
        assert_eq!(results[0]["error"], json!("connection reset"));
        let _ = fs::remove_dir_all(&output_dir);
    }

    // from spiderhead/checks/downloader.py::download_one (size limit)
    #[test]
    fn size_limit_aborts_without_writing_the_file() {
        let url = "https://example.com/big.png";
        let output_dir = temp_dir("size-limit");
        let transport = FakeTransport::with(url, reply(200, "image/png", SYNTHETIC_PNG));
        let options = DownloadOptions {
            max_bytes: Some(4),
            ..DownloadOptions::default()
        };
        let results = download_images(&[url.to_owned()], &output_dir, &options, &transport);

        assert_eq!(results[0]["ok"], json!(false));
        assert_eq!(results[0]["error"], json!("Size limit exceeded (4 bytes)"));
        assert!(!output_dir.join("example.com").join("big.png").exists());
        assert!(!output_dir.join("manifest.json").exists());
        let _ = fs::remove_dir_all(&output_dir);
    }

    // from spiderhead/checks/downloader.py::_unique_path
    #[test]
    fn unique_path_appends_a_numeric_variant() {
        let dir = temp_dir("unique");
        let existing = dir.join("photo.png");
        fs::write(&existing, b"first").expect("seed file");
        let candidate = unique_path(existing.clone());
        assert_eq!(candidate, dir.join("photo-1.png"));
        let absent = unique_path(dir.join("other.png"));
        assert_eq!(absent, dir.join("other.png"));
        let _ = fs::remove_dir_all(&dir);
    }

    // from spiderhead/checks/downloader.py::download_images (preview-skip branch)
    #[test]
    fn preview_skip_records_an_untracked_file_in_the_manifest() {
        let url = "https://example.com/pics/photo.png";
        let output_dir = temp_dir("preview-skip");
        let expected = output_dir
            .join("example.com")
            .join("pics")
            .join("photo.png");
        fs::create_dir_all(expected.parent().expect("parent")).expect("dirs");
        fs::write(&expected, b"placed manually").expect("seed file");
        let transport = FakeTransport::with(url, reply(200, "image/png", SYNTHETIC_PNG));

        let results = download_images(
            &[url.to_owned()],
            &output_dir,
            &DownloadOptions::default(),
            &transport,
        );

        assert_eq!(transport.request_count(), 0);
        assert_eq!(results[0]["ok"], json!(true));
        assert_eq!(results[0]["skipped"], json!(true));
        assert_eq!(results[0]["path"], json!(expected.to_string_lossy()));
        let manifest: Value = serde_json::from_str(
            &fs::read_to_string(output_dir.join("manifest.json")).expect("manifest"),
        )
        .expect("manifest json");
        assert_eq!(
            manifest,
            json!({"images": [{"url": url, "path": "example.com/pics/photo.png", "bytes": 15}]})
        );
        let _ = fs::remove_dir_all(&output_dir);
    }

    // from spiderhead/checks/downloader.py::_load_manifest (stale-entry drop)
    #[test]
    fn stale_manifest_entry_is_dropped_and_the_url_redownloaded() {
        let url = "https://example.com/pics/photo.png";
        let output_dir = temp_dir("stale");
        fs::create_dir_all(&output_dir).expect("dir");
        fs::write(
            output_dir.join("manifest.json"),
            "{\"images\": [{\"url\": \"https://example.com/pics/photo.png\", \"path\": \"gone.png\", \"bytes\": 1}]}",
        )
        .expect("seed manifest");
        let transport = FakeTransport::with(url, reply(200, "image/png", SYNTHETIC_PNG));

        let results = download_images(
            &[url.to_owned()],
            &output_dir,
            &DownloadOptions::default(),
            &transport,
        );

        assert_eq!(transport.request_count(), 1);
        assert_eq!(results[0]["skipped"], json!(null)); // absent key, fresh download
        assert_eq!(results[0]["ok"], json!(true));
        let manifest: Value = serde_json::from_str(
            &fs::read_to_string(output_dir.join("manifest.json")).expect("manifest"),
        )
        .expect("manifest json");
        assert_eq!(
            manifest,
            json!({"images": [{"url": url, "path": "example.com/pics/photo.png", "bytes": SYNTHETIC_PNG.len() as i64}]})
        );
        let _ = fs::remove_dir_all(&output_dir);
    }
}
