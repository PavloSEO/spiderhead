//! Deterministic loopback fixture server for the oracle parity tests.
//!
//! A site directory (`oracle/sites/<name>`) is `pages/**` static files plus an
//! optional `routes.toml` of `[[route]]` overrides (`path`, `status`,
//! `location`, `file`, `content_type`, `headers`, `body`). Responses carry
//! fixed `Date`/`Last-Modified`/`Server` headers so the bytes depend on the
//! request only; `{{BASE}}` in served text files and route locations expands
//! to the `http://127.0.0.1:<port>` origin of that run.

use indexmap::IndexMap;
use serde::Deserialize;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

const FIXED_DATE: &str = "Mon, 01 Jan 2026 00:00:00 GMT";
const NOT_FOUND: &[u8] = b"<!doctype html><html><head><title>Not Found</title></head><body><h1>Not Found</h1></body></html>\n";

/// Content types keyed by file extension; anything absent is `application/octet-stream`.
fn content_type(suffix: &str) -> &'static str {
    match suffix {
        ".html" => "text/html; charset=utf-8",
        ".xml" => "application/xml",
        ".txt" => "text/plain; charset=utf-8",
        ".svg" => "image/svg+xml",
        ".json" => "application/json",
        ".css" => "text/css",
        ".js" => "text/javascript",
        _ => "application/octet-stream",
    }
}

/// Suffixes whose bytes get the `{{BASE}}` substitution before serving.
fn is_textual(suffix: &str) -> bool {
    matches!(
        suffix,
        ".html" | ".xml" | ".txt" | ".svg" | ".json" | ".css" | ".js"
    )
}

/// Status-line reason phrases for `HTTP/1.1 <code> <reason>`; empty for codes
/// the table does not know.
fn reason(code: u16) -> &'static str {
    match code {
        100 => "Continue",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        410 => "Gone",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "",
    }
}

#[derive(Deserialize)]
struct Route {
    path: String,
    status: Option<u16>,
    location: Option<String>,
    file: Option<String>,
    content_type: Option<String>,
    #[serde(default)]
    headers: IndexMap<String, String>,
    body: Option<String>,
}

#[derive(Deserialize)]
struct SiteConfig {
    #[serde(default)]
    route: Vec<Route>,
}

/// One loaded site: the canonicalised `pages/` root and the path-keyed routes.
struct Site {
    pages: PathBuf,
    routes: IndexMap<String, Route>,
}

impl Site {
    fn load(dir: &Path) -> Site {
        let pages = dir
            .join("pages")
            .canonicalize()
            .unwrap_or_else(|e| panic!("fixture site {}: {e}", dir.display()));
        let routes_path = dir.join("routes.toml");
        let mut routes: IndexMap<String, Route> = IndexMap::new();
        if routes_path.exists() {
            let text = std::fs::read_to_string(&routes_path).expect("routes.toml");
            let cfg: SiteConfig = toml::from_str(&text).expect("routes.toml parse");
            // A repeated path keeps the last entry, so later rules win.
            for route in cfg.route {
                routes.insert(route.path.clone(), route);
            }
        }
        Site { pages, routes }
    }

    /// Resolve a request path to a readable file under `pages/`; `..` escapes
    /// and missing files resolve to `None`.
    fn resolve(&self, path: &str, route: Option<&Route>) -> Option<PathBuf> {
        let candidate = match route.and_then(|r| r.file.as_deref()) {
            Some(file) => self.pages.join(file),
            None => {
                let rel = unquote(path);
                let rel = rel.trim_start_matches('/');
                if rel.is_empty() || rel.ends_with('/') {
                    self.pages.join(format!("{rel}index.html"))
                } else {
                    self.pages.join(rel)
                }
            }
        };
        let resolved = normalize(&candidate);
        if resolved.is_file() && resolved.starts_with(&self.pages) {
            Some(resolved)
        } else {
            None
        }
    }
}

/// Fold `.`/`..` lexically so a path can be containment-checked without the
/// filesystem resolving symlinks the fixtures never have.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Percent-decode into a string; `+` stays literal and invalid UTF-8 decodes
/// lossily, matching the fixture semantics.
fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (i + 2 < bytes.len()).then(|| &text[i + 1..i + 3]);
        if bytes[i] == b'%' && hex.is_some_and(|h| u8::from_str_radix(h, 16).is_ok()) {
            out.push(u8::from_str_radix(hex.unwrap_or("00"), 16).unwrap_or(0));
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The path part of a request target: query and fragment stripped, plus the
/// scheme/authority of absolute-form and protocol-relative targets.
fn route_path(target: &str) -> &str {
    let bare = target.split(['?', '#']).next().unwrap_or_default();
    let authority = bare
        .strip_prefix("//")
        .or_else(|| bare.find("://").map(|i| &bare[i + 3..]));
    match authority {
        Some(rest) => rest.find('/').map(|i| &rest[i..]).unwrap_or(""),
        None => bare,
    }
}

/// Serialise one response exactly as the fixtures expect: status line, the
/// six fixed headers in order, then the route's own headers in TOML order.
fn write_response(
    stream: &mut TcpStream,
    status: u16,
    ctype: &str,
    headers: &IndexMap<String, String>,
    body: &[u8],
    head_only: bool,
) {
    let mut out = Vec::with_capacity(body.len() + 512);
    out.extend_from_slice(format!("HTTP/1.1 {status} {}\r\n", reason(status)).as_bytes());
    out.extend_from_slice(format!("Date: {FIXED_DATE}\r\n").as_bytes());
    out.extend_from_slice(b"Server: oracle-fixture\r\n");
    out.extend_from_slice(format!("Content-Type: {ctype}\r\n").as_bytes());
    out.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    out.extend_from_slice(format!("Last-Modified: {FIXED_DATE}\r\n").as_bytes());
    out.extend_from_slice(b"Connection: close\r\n");
    for (key, value) in headers {
        out.extend_from_slice(format!("{key}: {value}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    if !head_only {
        out.extend_from_slice(body);
    }
    let _ = stream.write_all(&out);
}

/// The one fixed error page for malformed or unsupported requests.
fn write_error(stream: &mut TcpStream, code: u16, message: &str) {
    let body = format!(
        "<!DOCTYPE HTML>\n<html>\n<head><title>Error response</title></head>\n\
         <body>\n<h1>Error response</h1>\n<p>Error code: {code}</p>\n\
         <p>Message: {message}.</p>\n</body>\n</html>\n"
    );
    write_response(
        stream,
        code,
        "text/html;charset=utf-8",
        &IndexMap::new(),
        body.as_bytes(),
        false,
    );
}

/// Route one request to a static file, a `routes.toml` override or the fixed
/// 404 body, then write it.
fn answer(stream: &mut TcpStream, site: &Site, base: &str, target: &str, head_only: bool) {
    let path = route_path(target);
    let route = site.routes.get(path);
    let file = site.resolve(path, route);
    let status = route.and_then(|r| r.status).unwrap_or({
        if file.is_some() || route.is_some_and(|r| r.body.is_some()) {
            200
        } else {
            404
        }
    });
    let mut headers = route.map(|r| r.headers.clone()).unwrap_or_default();
    if let Some(location) = route.and_then(|r| r.location.as_ref()) {
        headers.insert("Location".into(), location.replace("{{BASE}}", base));
    }
    let (mut body, mut ctype): (Vec<u8>, String) =
        if let Some(text) = route.and_then(|r| r.body.as_ref()) {
            (text.clone().into_bytes(), content_type(".html").into())
        } else if let Some(file) = &file {
            let mut bytes = std::fs::read(file).expect("serve fixture file");
            let suffix = file.extension().map_or_else(String::new, |e| {
                format!(".{}", e.to_string_lossy().to_lowercase())
            });
            if is_textual(&suffix) {
                bytes = replace_all(&bytes, b"{{BASE}}", base.as_bytes());
            }
            (bytes, content_type(&suffix).into())
        } else if status != 200 {
            (NOT_FOUND.to_vec(), content_type(".html").into())
        } else {
            (Vec::new(), content_type(".html").into())
        };
    // Redirects without a resolved file answer with headers only.
    if matches!(status, 301 | 302 | 303 | 307 | 308) && file.is_none() {
        body = Vec::new();
    }
    if let Some(override_ct) = route.and_then(|r| r.content_type.as_ref()) {
        ctype = override_ct.clone();
    }
    write_response(stream, status, &ctype, &headers, &body, head_only);
}

/// `bytes.replace(needle, with)` — the same substitution served text gets.
fn replace_all(haystack: &[u8], needle: &[u8], with: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(haystack.len());
    let mut i = 0;
    while i < haystack.len() {
        if haystack[i..].starts_with(needle) {
            out.extend_from_slice(with);
            i += needle.len();
        } else {
            out.push(haystack[i]);
            i += 1;
        }
    }
    out
}

/// Read request headers (up to the blank line), dispatch GET/HEAD and close;
/// every response advertises `Connection: close`.
fn handle_conn(mut stream: TcpStream, site: &Site, base: &str) {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 8192];
    loop {
        if header_end(&buf).is_some() {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 65_536 {
                    return;
                }
            }
        }
    }
    let request_line = buf
        .iter()
        .position(|b| *b == b'\n')
        .map(|i| &buf[..i])
        .unwrap_or(&buf);
    let request_line = String::from_utf8_lossy(request_line);
    let mut words = request_line.split_whitespace();
    let (method, target) = match (words.next(), words.next()) {
        (Some(m), Some(t)) => (m, t.to_owned()),
        _ => {
            write_error(&mut stream, 400, "Bad request syntax");
            return;
        }
    };
    match method {
        "GET" => answer(&mut stream, site, base, &target, false),
        "HEAD" => answer(&mut stream, site, base, &target, true),
        other => write_error(&mut stream, 501, &format!("Unsupported method ({other:?})")),
    }
}

/// Offset just past the blank line that ends the request headers.
fn header_end(buf: &[u8]) -> Option<usize> {
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2);
    match (crlf, lf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (one, None) | (None, one) => one,
    }
}

/// A running fixture site on `127.0.0.1` at an OS-assigned port; dropping it frees the port.
pub struct FixtureServer {
    /// The printed base URL, e.g. `http://127.0.0.1:41873`.
    pub base: String,
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl FixtureServer {
    /// Serve `oracle/sites/<site>` on a free port chosen by the OS; read `base` for the URL.
    /// Also switches the process to virtual pacing time (see `SPIDERHEAD_VIRTUAL_TIME`), so
    /// crawls against fixtures record their politeness delays instead of sleeping.
    pub fn start(site: &str) -> FixtureServer {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../sites")
            .join(site);
        Self::start_dir(&dir)
    }

    /// Serve an explicit site directory with the `pages/` + `routes.toml`
    /// layout.
    pub fn start_dir(site: &Path) -> FixtureServer {
        Self::serve(
            site,
            TcpListener::bind(("127.0.0.1", 0)).expect("fixture bind"),
        )
    }

    /// Serve `oracle/sites/<site>` on a fixed port. Baselines whose hashes cover the
    /// request host (such as response variant keys) were recorded on fixed ports, so
    /// those tests must reuse them; the bind waits up to 30 s for a busy port to free.
    pub fn start_on(site: &str, port: u16) -> FixtureServer {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../sites")
            .join(site);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let listener = loop {
            match TcpListener::bind(("127.0.0.1", port)) {
                Ok(listener) => break listener,
                Err(e) if std::time::Instant::now() >= deadline => {
                    panic!("fixture bind {port}: {e}")
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
            }
        };
        Self::serve(&dir, listener)
    }

    fn serve(site: &Path, listener: TcpListener) -> FixtureServer {
        if std::env::var_os("SPIDERHEAD_VIRTUAL_TIME").is_none() {
            std::env::set_var("SPIDERHEAD_VIRTUAL_TIME", "1");
        }
        let addr = listener.local_addr().expect("local addr");
        let base = format!("http://{addr}");
        let site = Arc::new(Site::load(site));
        let stop = Arc::new(AtomicBool::new(false));
        let accept = {
            let listener = listener;
            let stop = stop.clone();
            let base: Arc<str> = base.clone().into();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        let site = site.clone();
                        let base = base.clone();
                        std::thread::spawn(move || handle_conn(stream, &site, &base));
                    }
                }
            })
        };
        FixtureServer {
            base,
            addr,
            stop,
            accept: Some(accept),
        }
    }
}

impl Drop for FixtureServer {
    /// Stop the accept loop: flag it, then poke the listener with one empty
    /// connection so the blocking `accept` returns and the thread can exit.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}
