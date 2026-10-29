//! The provider HTTP seam shared by the networked source handlers.
//!
//! Production traffic goes straight to the providers. When
//! `SPIDERHEAD_PROVIDER_LOOPBACK` names an `http://127.0.0.1:<port>` origin,
//! every request is re-aimed at that origin and carries the provider's own host
//! in `X-Fake-Host`, so a scripted local server can stand in for all providers
//! without any real endpoint or key being touched. Any other origin is ignored.

use std::future::Future;

use spiderhead_sources::http::{Request, ReqwestTransport, Response, Transport, TransportError};

/// Environment variable naming the loopback stand-in origin.
pub const LOOPBACK_ENV: &str = "SPIDERHEAD_PROVIDER_LOOPBACK";

/// The loopback origin when configured and genuinely local.
fn loopback_origin() -> Option<String> {
    let origin = std::env::var(LOOPBACK_ENV).ok()?;
    let origin = origin.trim_end_matches('/');
    let rest = origin.strip_prefix("http://")?;
    let host = rest.split(':').next().unwrap_or_default();
    (host == "127.0.0.1" || host == "localhost").then(|| origin.to_owned())
}

/// A transport that honors the loopback override and otherwise delegates.
pub struct ProviderTransport {
    inner: ReqwestTransport,
    origin: Option<String>,
}

impl ProviderTransport {
    /// Build the transport for one handler call.
    pub fn new() -> Result<Self, TransportError> {
        Ok(Self {
            inner: ReqwestTransport::new()?,
            origin: loopback_origin(),
        })
    }
}

impl Transport for ProviderTransport {
    async fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let Some(origin) = &self.origin else {
            return self.inner.send(request).await;
        };
        let after_scheme = request
            .url
            .split_once("://")
            .map_or(request.url.as_str(), |(_, rest)| rest);
        let (host, tail) = match after_scheme.find(['/', '?']) {
            Some(at) => (&after_scheme[..at], &after_scheme[at..]),
            None => (after_scheme, "/"),
        };
        let tail = if tail.starts_with('?') {
            format!("/{tail}")
        } else {
            tail.to_owned()
        };
        let mut rewritten = request.clone();
        rewritten.url = format!("{origin}{tail}");
        rewritten
            .headers
            .push(("X-Fake-Host".to_owned(), host.to_owned()));
        self.inner.send(&rewritten).await
    }
}

/// Drive one provider future to completion on a private runtime.
pub fn block_on<F: Future>(future: F) -> Result<F::Output, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("http runtime build failed: {error}"))?;
    Ok(runtime.block_on(future))
}
