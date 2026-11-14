//! Bounded, resumable, checksummed downloads over a small [`Fetch`] seam.
//!
//! A transfer streams into `<name>.part` with a `<name>.part.json` that binds the part to its URL
//! and digest; an interrupted transfer continues from the part, and the finished file is published
//! only when its SHA-256 matches. The seam keeps the engine free of any HTTP client.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use thiserror::Error;

use super::trust::sha256_file;

/// What the server said about a transfer once the body was delivered.
#[derive(Debug, Clone, Default)]
pub struct FetchHead {
    /// 200 (whole body) or 206 (from the requested offset).
    pub status: u16,
    /// First byte of a 206 body, from `Content-Range`.
    pub range_start: Option<u64>,
    /// Size of the whole resource, when known.
    pub total: Option<u64>,
    /// Validator of the resource, when sent.
    pub etag: Option<String>,
}

/// Why a transfer failed.
#[derive(Debug)]
pub enum FetchFailure {
    /// The connection or the sink failed.
    Transport(String),
    /// The server answered with this status instead of a body.
    Http(u16),
    /// The body exceeded the byte limit.
    TooLarge,
}

impl std::fmt::Display for FetchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "{message}"),
            Self::Http(status) => write!(f, "HTTP {status}"),
            Self::TooLarge => write!(f, "the response exceeded its byte limit"),
        }
    }
}

/// A source of bytes: GET `url` from byte `offset` (`Range: bytes=offset-`), body streamed to `sink`.
pub trait Fetch {
    /// Stream the body; non-success statuses are `Err(Http)` and write nothing.
    fn fetch_into(
        &self,
        url: &str,
        offset: u64,
        sink: &mut dyn Write,
    ) -> Result<FetchHead, FetchFailure>;
}

/// A feed that lives in a directory: `url` is a path or a `file://` URL.
pub struct FileFetch;

impl Fetch for FileFetch {
    fn fetch_into(
        &self,
        url: &str,
        offset: u64,
        sink: &mut dyn Write,
    ) -> Result<FetchHead, FetchFailure> {
        let io = |e: std::io::Error| FetchFailure::Transport(e.to_string());
        let mut file = File::open(url.strip_prefix("file://").unwrap_or(url)).map_err(io)?;
        let total = file.metadata().map_err(io)?.len();
        file.seek(SeekFrom::Start(offset)).map_err(io)?;
        std::io::copy(&mut file, sink).map_err(io)?;
        Ok(FetchHead {
            status: if offset == 0 { 200 } else { 206 },
            range_start: (offset > 0).then_some(offset),
            total: Some(total),
            etag: None,
        })
    }
}

/// A writer that refuses to pass more than `max` bytes in total (the already-present `base` counts).
struct Limited<W> {
    inner: W,
    written: u64,
    max: u64,
    exceeded: bool,
}

impl<W: Write> Write for Limited<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written + buf.len() as u64 > self.max {
            self.exceeded = true;
            return Err(std::io::Error::other("byte limit exceeded"));
        }
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Failures of a verified download.
#[derive(Debug, Error)]
pub enum DownloadError {
    /// The transfer failed on every attempt.
    #[error("download failed: {0}")]
    Fetch(FetchFailure),
    /// The file does not match the expected SHA-256 (it was removed).
    #[error("the downloaded file does not match its SHA-256")]
    ChecksumMismatch,
    /// The body is larger than allowed.
    #[error("the download exceeds its byte limit")]
    SizeLimit,
    /// Local file trouble.
    #[error("download storage error: {0}")]
    Io(#[from] std::io::Error),
}

/// One asset to fetch into `dir` (`updates/<tag>/`).
pub struct Download<'a> {
    /// Where the bytes come from.
    pub url: &'a str,
    /// File name inside `dir`.
    pub name: &'a str,
    /// Expected lowercase SHA-256 hex.
    pub sha256: &'a str,
    /// Largest acceptable file.
    pub max_bytes: u64,
    /// Cache directory (created private).
    pub dir: &'a Path,
}

/// Create `path` as a private directory; a symbolic link in its place is refused.
pub fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::other(
            "update directory must not be a symbolic link",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Download, resuming an interrupted `.part`, and publish `<dir>/<name>` only when its SHA-256 is
/// `d.sha256`. A part that is stale, ignored by the server or corrupt is dropped and fetched anew
/// once; `attempts` bounds the retries after transport failures.
pub fn download_verified(
    fetch: &dyn Fetch,
    d: &Download,
    attempts: u32,
) -> Result<PathBuf, DownloadError> {
    private_dir(d.dir)?;
    let done = d.dir.join(d.name);
    if sha256_file(&done).is_ok_and(|have| have == d.sha256) {
        return Ok(done);
    }
    let part = d.dir.join(format!("{}.part", d.name));
    let meta = d.dir.join(format!("{}.part.json", d.name));
    let want = json!({"url": d.url, "sha256": d.sha256});
    let discard = || {
        let _ = std::fs::remove_file(&part);
        let _ = std::fs::remove_file(&meta);
    };
    // `failed` counts transport failures; `fresh` bounds the free restarts from zero.
    let (mut failed, mut fresh) = (0u32, 2i32);
    loop {
        let bound = std::fs::read(&meta)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .is_some_and(|m| m["url"] == want["url"] && m["sha256"] == want["sha256"]);
        if !bound {
            discard();
        }
        let offset = std::fs::metadata(&part).map_or(0, |m| m.len());
        std::fs::write(&meta, want.to_string())?;
        let file = OpenOptions::new().create(true).append(true).open(&part)?;
        let mut sink = Limited {
            inner: file,
            written: offset,
            max: d.max_bytes,
            exceeded: false,
        };
        let result = fetch.fetch_into(d.url, offset, &mut sink);
        let exceeded = sink.exceeded;
        drop(sink);
        // Anything wrong with a continued part: drop it and go again from zero, a bounded number of times.
        let mut start_over = false;
        match result {
            Ok(head) => {
                let straight = if offset == 0 {
                    head.status == 200
                } else {
                    head.status == 206 && head.range_start == Some(offset)
                };
                if straight && sha256_file(&part)? == d.sha256 {
                    std::fs::rename(&part, &done)?;
                    let _ = std::fs::remove_file(&meta);
                    return Ok(done);
                }
                if straight && offset == 0 {
                    discard();
                    return Err(DownloadError::ChecksumMismatch);
                }
                start_over = true;
            }
            Err(_) if exceeded && offset == 0 => {
                discard();
                return Err(DownloadError::SizeLimit);
            }
            Err(_) if exceeded => start_over = true,
            Err(FetchFailure::Http(416)) if offset > 0 => start_over = true,
            Err(failure) => {
                failed += 1;
                if failed >= attempts {
                    return Err(DownloadError::Fetch(failure));
                }
            }
        }
        if start_over {
            discard();
            fresh -= 1;
            if fresh < 0 {
                return Err(DownloadError::Fetch(FetchFailure::Transport(
                    "the server does not honour range requests".into(),
                )));
            }
        }
    }
}

/// A small file (checksums, signature) into memory, never larger than `max` bytes.
pub fn fetch_small(fetch: &dyn Fetch, url: &str, max: usize) -> Result<Vec<u8>, FetchFailure> {
    let mut sink = Limited {
        inner: Vec::new(),
        written: 0,
        max: max as u64,
        exceeded: false,
    };
    let result = fetch.fetch_into(url, 0, &mut sink);
    if sink.exceeded {
        return Err(FetchFailure::TooLarge);
    }
    result.map(|_| sink.inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::cell::Cell;

    fn body() -> Vec<u8> {
        (0..4000u32).map(|i| (i % 251) as u8).collect()
    }
    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    fn write_src(dir: &Path, bytes: &[u8]) -> String {
        let path = dir.join("asset.bin");
        std::fs::write(&path, bytes).expect("src");
        path.to_string_lossy().into_owned()
    }

    /// Serves the file but fails the first call after `cut` bytes.
    struct Flaky {
        cut: usize,
        calls: Cell<u32>,
        offsets: std::cell::RefCell<Vec<u64>>,
    }
    struct Cut<'a> {
        inner: &'a mut dyn Write,
        left: usize,
    }
    impl Write for Cut<'_> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.left == 0 {
                return Err(std::io::Error::other("connection reset"));
            }
            let n = buf.len().min(self.left);
            self.left -= n;
            self.inner.write(&buf[..n])
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }
    impl Fetch for Flaky {
        fn fetch_into(
            &self,
            url: &str,
            offset: u64,
            sink: &mut dyn Write,
        ) -> Result<FetchHead, FetchFailure> {
            self.calls.set(self.calls.get() + 1);
            self.offsets.borrow_mut().push(offset);
            if self.calls.get() == 1 {
                FileFetch.fetch_into(
                    url,
                    offset,
                    &mut Cut {
                        inner: sink,
                        left: self.cut,
                    },
                )
            } else {
                FileFetch.fetch_into(url, offset, sink)
            }
        }
    }

    /// A server that does not know ranges: always 200 with the whole body.
    struct NoRange;
    impl Fetch for NoRange {
        fn fetch_into(
            &self,
            url: &str,
            _offset: u64,
            sink: &mut dyn Write,
        ) -> Result<FetchHead, FetchFailure> {
            FileFetch.fetch_into(url, 0, sink).map(|mut head| {
                head.status = 200;
                head
            })
        }
    }

    #[test]
    fn interrupted_download_resumes_from_the_part() {
        let tmp = tempfile::tempdir().expect("tmp");
        let bytes = body();
        let url = write_src(tmp.path(), &bytes);
        let cache = tmp.path().join("cache");
        let d = Download {
            url: &url,
            name: "asset",
            sha256: &digest(&bytes),
            max_bytes: 1 << 20,
            dir: &cache,
        };
        let flaky = Flaky {
            cut: 1500,
            calls: Cell::new(0),
            offsets: Default::default(),
        };

        // One attempt only: the cut leaves a part of exactly 1500 bytes and its binding file.
        assert!(download_verified(&flaky, &d, 1).is_err());
        assert_eq!(
            std::fs::metadata(cache.join("asset.part"))
                .expect("part")
                .len(),
            1500
        );
        assert!(cache.join("asset.part.json").exists());
        assert!(!cache.join("asset").exists());

        // The next call continues from byte 1500 and finishes the verified file.
        let path = download_verified(&flaky, &d, 1).expect("resumed");
        assert_eq!(std::fs::read(path).expect("file"), bytes);
        assert_eq!(*flaky.offsets.borrow(), vec![0, 1500]);
        assert!(!cache.join("asset.part").exists());
    }

    #[test]
    fn retries_inside_one_call_resume_too() {
        let tmp = tempfile::tempdir().expect("tmp");
        let bytes = body();
        let url = write_src(tmp.path(), &bytes);
        let cache = tmp.path().join("cache");
        let d = Download {
            url: &url,
            name: "asset",
            sha256: &digest(&bytes),
            max_bytes: 1 << 20,
            dir: &cache,
        };
        let flaky = Flaky {
            cut: 700,
            calls: Cell::new(0),
            offsets: Default::default(),
        };
        download_verified(&flaky, &d, 3).expect("download");
        assert_eq!(*flaky.offsets.borrow(), vec![0, 700]);
    }

    #[test]
    fn server_ignoring_range_restarts_cleanly() {
        let tmp = tempfile::tempdir().expect("tmp");
        let bytes = body();
        let url = write_src(tmp.path(), &bytes);
        let cache = tmp.path().join("cache");
        let d = Download {
            url: &url,
            name: "asset",
            sha256: &digest(&bytes),
            max_bytes: 1 << 20,
            dir: &cache,
        };
        let flaky = Flaky {
            cut: 1000,
            calls: Cell::new(0),
            offsets: Default::default(),
        };
        assert!(download_verified(&flaky, &d, 1).is_err());
        let path = download_verified(&NoRange, &d, 1).expect("restart");
        assert_eq!(std::fs::read(path).expect("file"), bytes);
    }

    #[test]
    fn corrupted_download_is_refused_and_leaves_nothing() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut bytes = body();
        let want = digest(&bytes);
        bytes[2000] ^= 0xff;
        let url = write_src(tmp.path(), &bytes);
        let cache = tmp.path().join("cache");
        let d = Download {
            url: &url,
            name: "asset",
            sha256: &want,
            max_bytes: 1 << 20,
            dir: &cache,
        };
        assert!(matches!(
            download_verified(&FileFetch, &d, 3),
            Err(DownloadError::ChecksumMismatch)
        ));
        assert_eq!(std::fs::read_dir(&cache).expect("cache").count(), 0);
    }

    #[test]
    fn stale_part_for_another_digest_is_discarded() {
        let tmp = tempfile::tempdir().expect("tmp");
        let bytes = body();
        let url = write_src(tmp.path(), &bytes);
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).expect("cache");
        std::fs::write(cache.join("asset.part"), b"junk from another release").expect("part");
        std::fs::write(
            cache.join("asset.part.json"),
            json!({"url": url, "sha256": "0".repeat(64)}).to_string(),
        )
        .expect("meta");
        let d = Download {
            url: &url,
            name: "asset",
            sha256: &digest(&bytes),
            max_bytes: 1 << 20,
            dir: &cache,
        };
        let path = download_verified(&FileFetch, &d, 1).expect("fresh");
        assert_eq!(std::fs::read(path).expect("file"), bytes);
    }

    #[test]
    fn size_limit_applies_while_streaming() {
        let tmp = tempfile::tempdir().expect("tmp");
        let bytes = body();
        let url = write_src(tmp.path(), &bytes);
        let cache = tmp.path().join("cache");
        let d = Download {
            url: &url,
            name: "asset",
            sha256: &digest(&bytes),
            max_bytes: 100,
            dir: &cache,
        };
        assert!(matches!(
            download_verified(&FileFetch, &d, 1),
            Err(DownloadError::SizeLimit)
        ));
        // Nothing beyond the limit reached the disk, and small reads obey the same bound.
        assert!(!cache.join("asset").exists());
        assert!(matches!(
            fetch_small(&FileFetch, &url, 10),
            Err(FetchFailure::TooLarge)
        ));
        assert_eq!(fetch_small(&FileFetch, &url, 10_000).expect("small"), bytes);
    }
}
