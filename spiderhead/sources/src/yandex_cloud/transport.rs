//! Transport seam and live HTTP send for the Yandex Cloud client.

use super::*;

/// Clock seam in seconds; tests inject it to make rate limiting and deadlines deterministic.
pub type Monotonic<'a> = dyn Fn() -> f64 + 'a;

/// Result of one send: HTTP status plus raw body.
/// `strict` is true for a 2xx response, whose body must be valid UTF-8; false for an HTTP error body, which is decoded with replacement characters.
#[derive(Debug, Clone)]
pub struct CloudAnswer {
    /// HTTP status code.
    pub status: i64,
    /// Raw body bytes.
    pub body: Vec<u8>,
    /// True for a successful response; false for an HTTP error response.
    pub strict: bool,
}

/// Scripted transport used in tests.
pub type CloudTransport<'a> =
    dyn Fn(&str, &str, Option<&Value>, &str) -> Result<CloudAnswer, CloudError> + Send + Sync + 'a;

/// Adapts an injected [`CloudTransport`] to the [`CloudCall`] trait.
pub struct SyncCloudCall<'x>(pub &'x CloudTransport<'x>);

impl CloudCall for SyncCloudCall<'_> {
    fn send<'s>(
        &'s mut self,
        method: &'s str,
        url: &'s str,
        body: Option<&'s Value>,
        key: &'s str,
    ) -> Pin<Box<dyn Future<Output = Result<CloudAnswer, CloudError>> + Send + 's>> {
        let result = self.0(method, url, body, key);
        Box::pin(std::future::ready(result))
    }
}

/// Transport boundary used by the request helper.
pub trait CloudCall {
    /// Send one request. Resolves to the status and raw body, or a classified failure.
    /// Implemented by both the scripted [`CloudTransport`] and the live [`LiveCloudCall`].
    fn send<'s>(
        &'s mut self,
        method: &'s str,
        url: &'s str,
        body: Option<&'s Value>,
        key: &'s str,
    ) -> Pin<Box<dyn Future<Output = Result<CloudAnswer, CloudError>> + Send + 's>>;
}

/// Live send over the shared HTTP client, without following redirects.
pub struct LiveCloudCall<'x, T: Transport>(pub &'x T);

impl<T: Transport> CloudCall for LiveCloudCall<'_, T> {
    fn send<'s>(
        &'s mut self,
        method: &'s str,
        url: &'s str,
        body: Option<&'s Value>,
        key: &'s str,
    ) -> Pin<Box<dyn Future<Output = Result<CloudAnswer, CloudError>> + Send + 's>> {
        Box::pin(async move {
            let mut request = Request::new(method, url)
                .header("Authorization", format!("Api-Key {key}"))
                .timeout(45.0);
            if let Some(payload) = body {
                request = request
                    .header("Content-Type", "application/json")
                    .body(pyjson::dumps(payload).into_bytes());
            }
            match self.0.send(&request).await {
                Ok(response) => Ok(CloudAnswer {
                    status: i64::from(response.status),
                    body: response.body,
                    strict: true,
                }),
                Err(TransportError::Http(response)) => Ok(CloudAnswer {
                    status: i64::from(response.status),
                    body: response.body,
                    strict: false,
                }),
                Err(TransportError::Connect(kind)) => Err(CloudError::Url(pyerr::url_error(&kind))),
                Err(TransportError::InvalidRequest(kind)) => {
                    Err(CloudError::python("ValueError", kind))
                }
            }
        })
    }
}

/// Dependencies every Cloud call needs; `live` wires the production ones.
pub struct CloudEnv<'a, T: Transport> {
    /// Credential store and spend-ledger host.
    pub host: &'a dyn Host,
    /// Root directory for credential config.
    pub root: &'a Path,
    /// HTTP boundary for live sends.
    pub transport: &'a T,
    /// Sleep function for backoff and operation polling.
    pub sleeper: &'a Sleeper,
    /// Spend-record sink; `None` writes to the real ledger.
    pub spend: Option<&'a SpendSink<'a>>,
    /// Monotonic clock; `None` uses the process clock.
    pub monotonic: Option<&'a Monotonic<'a>>,
    /// Overrides the API key lookup; `None` reads the credential store.
    pub api_key: Option<&'a dyn Fn() -> Result<String, CredentialError>>,
    /// Overrides the folder ID lookup; `None` reads the credential store.
    pub folder_id: Option<&'a dyn Fn() -> Result<String, CredentialError>>,
}

impl<'a, T: Transport> CloudEnv<'a, T> {
    /// Environment for production use.
    pub fn live(host: &'a dyn Host, root: &'a Path, transport: &'a T) -> Self {
        CloudEnv {
            host,
            root,
            transport,
            sleeper: &Sleeper::Real,
            spend: None,
            monotonic: None,
            api_key: None,
            folder_id: None,
        }
    }

    pub(super) fn monotonic(&self) -> f64 {
        match self.monotonic {
            Some(clock) => clock(),
            None => {
                let start = MONOTONIC_START.get_or_init(std::time::Instant::now);
                start.elapsed().as_secs_f64()
            }
        }
    }

    /// Record a spend entry; I/O errors are returned to the caller.
    pub(super) fn spend(&self, rec: &Record) -> Result<Value, CloudError> {
        match self.spend {
            Some(sink) => sink(rec),
            None => spend::record(self.host, rec),
        }
        .map_err(|err| CloudError::python("OSError", err.to_string()))
    }
}

pub(super) static MONOTONIC_START: std::sync::OnceLock<std::time::Instant> =
    std::sync::OnceLock::new();
