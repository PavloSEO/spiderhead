//! The HTTP resource fetcher of the scan.v2 resource-graph pass.
//!
//! [`ResourceFetcher`](spiderhead_store::resource_capture::ResourceFetcher)
//! is a sync seam; the requests themselves run on a throwaway single-thread
//! runtime in a dedicated thread, because the pass is driven from inside the
//! crawl's runtime where blocking sends are not allowed.

use std::time::{Duration, Instant};

use spiderhead_store::resource_capture::{FetchFailure, FetchedResource, ResourceFetcher};

/// One politeness-paced GET per call, redirects not followed (the pass follows
/// them itself under its own redirect budget).
pub struct HttpResourceFetcher {
    user_agent: String,
    timeout_seconds: f64,
    min_delay: f64,
    last_request: Option<Instant>,
}

impl HttpResourceFetcher {
    /// Pacing and identity from the effective crawl options.
    pub fn new(options: &crate::spider::CrawlOptions) -> HttpResourceFetcher {
        HttpResourceFetcher {
            user_agent: options.user_agent.clone(),
            timeout_seconds: options.timeout_seconds,
            min_delay: options.min_delay.max(0.0),
            last_request: None,
        }
    }
}

impl ResourceFetcher for HttpResourceFetcher {
    fn fetch(&mut self, url: &str, max_bytes: i64) -> Result<FetchedResource, FetchFailure> {
        // The graph pass does not ride the crawl throttle; the configured
        // minimum delay stays a floor so a resource-heavy page cannot hammer
        // the origin the crawl just left.
        if let Some(last) = self.last_request.take() {
            let elapsed = last.elapsed().as_secs_f64();
            if self.min_delay > elapsed {
                std::thread::sleep(Duration::from_secs_f64(self.min_delay - elapsed));
            }
        }
        self.last_request = Some(Instant::now());
        let request = Request {
            url: url.to_owned(),
            user_agent: self.user_agent.clone(),
            timeout_seconds: self.timeout_seconds,
            max_bytes: if max_bytes < 1 { 1 } else { max_bytes },
        };
        std::thread::scope(|scope| {
            scope
                .spawn(move || request.run())
                .join()
                .unwrap_or_else(|_| {
                    Err(FetchFailure::Failed(
                        "resource fetch thread panicked".to_owned(),
                    ))
                })
        })
    }

    fn delivery_json(&self, body: &[u8], kind: &str, headers: &[(String, String)]) -> String {
        let header = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        // The graph's validator requires `cache.ok`; a response that names a
        // caching directive or cache header is cache-ok, anything else is not.
        let cache_ok = !header("cache-control").is_empty() || !header("x-cache").is_empty();
        serde_json::json!({
            "cache": { "ok": cache_ok },
            "kind": kind,
            "bytes": body.len(),
            "content_type": header("content-type"),
            "content_encoding": header("content-encoding"),
            "cache_control": header("cache-control"),
        })
        .to_string()
    }
}

/// One bounded GET, self-contained for the worker thread.
struct Request {
    url: String,
    user_agent: String,
    timeout_seconds: f64,
    max_bytes: i64,
}

impl Request {
    fn run(self) -> Result<FetchedResource, FetchFailure> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| FetchFailure::Failed(e.to_string()))?;
        // A fresh client per thread: the crawl's client belongs to its own
        // runtime, and a reqwest client must not cross runtimes.
        let client = spiderhead_fetch::client::build_client(self.timeout_seconds)
            .map_err(FetchFailure::Failed)?;
        runtime.block_on(self.fetch_bounded(&client))
    }

    async fn fetch_bounded(
        &self,
        client: &reqwest::Client,
    ) -> Result<FetchedResource, FetchFailure> {
        let transport = |error: reqwest::Error| FetchFailure::Failed(error.to_string());
        let mut response = client
            .get(&self.url)
            .header("user-agent", &self.user_agent)
            .send()
            .await
            .map_err(transport)?;
        let status = i64::from(response.status().as_u16());
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    value.to_str().unwrap_or("").to_owned(),
                )
            })
            .collect();
        let header = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
                .unwrap_or_default()
                .to_owned()
        };
        let mut body: Vec<u8> = Vec::new();
        // Read one byte past the caller's cap: a stream that ends exactly at
        // the cap is still complete, only a longer one is partial.
        let read_cap = self.max_bytes + 1;
        while let Some(chunk) = response.chunk().await.map_err(transport)? {
            let room = (read_cap - body.len() as i64).max(0) as usize;
            if chunk.len() > room {
                body.extend_from_slice(&chunk[..room]);
                break;
            }
            body.extend_from_slice(&chunk);
            if body.len() as i64 == read_cap {
                break;
            }
        }
        let complete = body.len() as i64 <= self.max_bytes;
        body.truncate(self.max_bytes as usize);
        Ok(FetchedResource {
            status_code: Some(status),
            redirect_url: header("location"),
            content_type: header("content-type"),
            error: String::new(),
            cache_status: header("x-cache"),
            content_encoding: header("content-encoding"),
            body: Some(body),
            body_complete: complete,
            headers,
        })
    }
}
