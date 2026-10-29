//! Dated on-disk cache of vendor range lists with fallback to the last good copy.
//!
//! Layout of the cache directory: `<provider>.json` (the list exactly as fetched) and
//! `<provider>.meta.json` (`fetched_at`, `url`, `count`). Rules:
//! * A list file without a meta file was put there by the user and is pinned: it is never
//!   re-fetched or overwritten.
//! * A list younger than `max_age` is used without a request.
//! * A failed or invalid download keeps the previous copy (reported as `stale`); a bad
//!   download never replaces a good file, and files are replaced atomically.
//!
//! The only network access is through the injected [`HttpGet`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::error::Result;
use crate::presets::PresetSet;
use crate::ranges::{RangeBook, RangeFetcher};

/// A blocking HTTP GET returning the body text; implemented by the caller's HTTP client.
pub trait HttpGet: Send + Sync {
    /// Fetch `url` or describe the failure.
    fn get(&self, url: &str) -> std::result::Result<String, String>;
}

/// [`RangeFetcher`] that downloads each provider from the URL configured in a preset set.
pub struct UrlFetcher<'a> {
    pub presets: &'a PresetSet,
    pub http: &'a dyn HttpGet,
}

impl RangeFetcher for UrlFetcher<'_> {
    fn fetch(&self, provider: &str) -> std::result::Result<String, String> {
        let url = self
            .presets
            .url_of(provider)
            .ok_or_else(|| format!("no download URL is configured for {provider}"))?;
        self.http.get(url)
    }
}

/// Where a provider's list in the book came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Downloaded now.
    Fresh,
    /// Cached copy younger than the limit.
    Cache,
    /// Cached copy older than the limit, kept because the download failed.
    Stale,
    /// User-pinned file without meta.
    Pinned,
    /// No list could be loaded.
    Missing,
}

impl Origin {
    /// Machine name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Cache => "cache",
            Self::Stale => "stale",
            Self::Pinned => "pinned",
            Self::Missing => "missing",
        }
    }
}

/// Outcome for one provider.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub provider: String,
    pub origin: Origin,
    pub fetched_at: Option<f64>,
    pub count: usize,
    /// Fetch attempts made this call (1 + retries that ran).
    pub attempts: u32,
    pub error: Option<String>,
}

impl Loaded {
    /// JSON form for reports: `available` is the explicit "list usable" flag and
    /// `error` carries why a list stayed unavailable.
    pub fn to_json(&self) -> Value {
        json!({"provider": self.provider, "origin": self.origin.name(),
               "fetched_at": self.fetched_at, "count": self.count,
               "available": self.origin != Origin::Missing,
               "attempts": self.attempts, "error": self.error})
    }
}

/// The cache directory.
pub struct RangeCache {
    pub dir: PathBuf,
    /// Seconds a cached list stays current.
    pub max_age: f64,
    /// Extra fetch attempts after the first failure.
    pub retries: u32,
    /// First backoff between attempts; doubles per attempt.
    pub backoff: Duration,
    /// Sleep between attempts (a no-op in tests).
    pub sleep: fn(Duration),
}

/// Default freshness of a cached list: one day.
pub const DEFAULT_MAX_AGE: f64 = 86_400.0;

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

impl RangeCache {
    /// A cache in `dir` with the default freshness, two fetch retries and real
    /// backoff sleeps.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            max_age: DEFAULT_MAX_AGE,
            retries: 2,
            backoff: Duration::from_millis(200),
            sleep: std::thread::sleep,
        }
    }

    fn list(&self, provider: &str) -> PathBuf {
        self.dir.join(format!("{provider}.json"))
    }

    fn meta(&self, provider: &str) -> PathBuf {
        self.dir.join(format!("{provider}.meta.json"))
    }

    fn fetched_at(&self, provider: &str) -> Option<f64> {
        let text = std::fs::read_to_string(self.meta(provider)).ok()?;
        serde_json::from_str::<Value>(&text)
            .ok()?
            .get("fetched_at")?
            .as_f64()
    }

    /// Load `providers` into `book`. With `fetcher == None` only the directory is read.
    pub fn load(
        &self,
        book: &mut RangeBook,
        fetcher: Option<&dyn RangeFetcher>,
        providers: &[String],
        now: f64,
    ) -> Vec<Loaded> {
        providers
            .iter()
            .map(|p| self.load_one(book, fetcher, p, now))
            .collect()
    }

    fn load_one(
        &self,
        book: &mut RangeBook,
        fetcher: Option<&dyn RangeFetcher>,
        provider: &str,
        now: f64,
    ) -> Loaded {
        let mut out = Loaded {
            provider: provider.to_string(),
            origin: Origin::Missing,
            fetched_at: None,
            count: 0,
            attempts: 0,
            error: None,
        };
        let path = self.list(provider);
        let stamp = self.fetched_at(provider);
        let have = path.is_file();
        let pinned = have && !self.meta(provider).exists();
        let fresh = stamp.is_some_and(|t| (0.0..self.max_age).contains(&(now - t)));
        if have && (pinned || fresh || fetcher.is_none()) {
            return self.load_file(book, provider, out, stamp, pinned, now);
        }
        if let Some(fetcher) = fetcher {
            let mut fetched = fetcher.fetch(provider);
            out.attempts = 1;
            for retry in 0..self.retries {
                if fetched.is_ok() {
                    break;
                }
                (self.sleep)(self.backoff * 2u32.pow(retry));
                fetched = fetcher.fetch(provider);
                out.attempts += 1;
            }
            match fetched {
                Ok(text) => {
                    let mut probe = RangeBook::new();
                    match probe.load_json(provider, &text) {
                        Ok(count) => {
                            let saved = std::fs::create_dir_all(&self.dir)
                                .map_err(crate::error::LogsError::from)
                                .and_then(|()| write_atomic(&path, &text))
                                .and_then(|()| {
                                    write_atomic(
                                        &self.meta(provider),
                                        &json!({"fetched_at": now, "count": count}).to_string(),
                                    )
                                });
                            if let Err(e) = saved {
                                out.error = Some(format!("cache not written: {e}"));
                            }
                            let _ = book.load_json(provider, &text);
                            out.origin = Origin::Fresh;
                            out.fetched_at = Some(now);
                            out.count = count;
                            return out;
                        }
                        Err(e) => out.error = Some(e.to_string()),
                    }
                }
                Err(e) => out.error = Some(e),
            }
        }
        if have {
            let error = out.error.take();
            let mut kept = self.load_file(book, provider, out, stamp, false, now);
            kept.error = error;
            return kept;
        }
        out
    }

    fn load_file(
        &self,
        book: &mut RangeBook,
        provider: &str,
        mut out: Loaded,
        stamp: Option<f64>,
        pinned: bool,
        now: f64,
    ) -> Loaded {
        match book.load_file(provider, &self.list(provider)) {
            Ok(count) => {
                out.count = count;
                out.fetched_at = stamp;
                out.origin = if pinned {
                    Origin::Pinned
                } else if stamp.is_some_and(|t| (0.0..self.max_age).contains(&(now - t))) {
                    Origin::Cache
                } else {
                    Origin::Stale
                };
            }
            Err(e) => out.error = Some(e.to_string()),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Web(Mutex<Vec<String>>, bool);
    impl RangeFetcher for Web {
        fn fetch(&self, provider: &str) -> std::result::Result<String, String> {
            self.0.lock().unwrap().push(provider.into());
            if self.1 {
                Ok(r#"["10.0.0.0/8"]"#.into())
            } else {
                Err("offline".into())
            }
        }
    }

    #[test]
    fn fresh_cache_skips_the_request_and_old_cache_survives_an_outage() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = RangeCache::new(tmp.path().join("r"));
        let p = vec!["gptbot".to_string()];
        let mut book = RangeBook::new();
        let web = Web(Mutex::new(vec![]), true);
        assert_eq!(
            cache.load(&mut book, Some(&web), &p, 100.0)[0].origin,
            Origin::Fresh
        );
        assert_eq!(
            cache.load(&mut book, Some(&web), &p, 200.0)[0].origin,
            Origin::Cache
        );
        assert_eq!(web.0.lock().unwrap().len(), 1);
        let down = Web(Mutex::new(vec![]), false);
        let mut later = RangeBook::new();
        let r = &cache.load(&mut later, Some(&down), &p, 100.0 + DEFAULT_MAX_AGE + 1.0)[0];
        assert_eq!(r.origin, Origin::Stale);
        assert!(r.error.is_some());
        assert_eq!(later.contains("gptbot", "10.1.1.1"), Some(true));
    }

    #[test]
    fn invalid_download_never_replaces_a_good_file_and_pinned_files_are_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = RangeCache::new(tmp.path());
        let p = vec!["claudebot".to_string()];
        std::fs::write(tmp.path().join("claudebot.json"), r#"["160.79.104.0/23"]"#).unwrap();
        let web = Web(Mutex::new(vec![]), true);
        let mut book = RangeBook::new();
        let r = &cache.load(&mut book, Some(&web), &p, 5.0)[0];
        assert_eq!(r.origin, Origin::Pinned);
        assert!(web.0.lock().unwrap().is_empty());
        let none = vec!["ccbot".to_string()];
        assert_eq!(
            cache.load(&mut book, None, &none, 5.0)[0].origin,
            Origin::Missing
        );
    }
}
