use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::error::{ApiError, ConnectionError, Error, Result};
use crate::generated::API_VERSION;
use crate::http::{Headers, HttpClient, HttpRequest, HttpResponse, TransportErrorKind};
use crate::response::{Response, ResponseInfo};
use crate::util;

/// This library's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The production API.
pub const DEFAULT_BASE_URL: &str = "https://api.vojo-media.com";
/// The limit on each attempt, including reading the response.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// How many times a failed request is retried when that is safe.
pub const DEFAULT_MAX_RETRIES: u32 = 2;

const FIRST_BACKOFF: Duration = Duration::from_millis(500);
const MOST_BACKOFF: Duration = Duration::from_secs(8);
// A longer Retry-After is not slept through: the caller gets the error and decides.
const LONGEST_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Talks to the Vojo API. Cloning is cheap and clones share one connection
/// pool. Its debug form never includes the key.
///
/// ```no_run
/// # async fn run() -> vojo::Result<()> {
/// let client = vojo::Client::new("vojo_live_...")?;
/// let screen = client.screens().retrieve("scr_123").await?;
/// println!("{}", screen.name);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
    options: RequestOptions,
}

struct Inner {
    key: String,
    organisation: Option<String>,
    base_url: String,
    version: String,
    timeout: Duration,
    max_retries: u32,
    http: Arc<dyn HttpClient>,
}

impl Client {
    /// A client with the default settings. Keys are issued by Vojo during early
    /// access. OAuth access tokens are passed the same way, and those callers
    /// must also set [`ClientBuilder::organisation`].
    pub fn new(secret_key: impl Into<String>) -> Result<Self> {
        Self::builder(secret_key).build()
    }

    /// A client using the key in `VOJO_SECRET_KEY`.
    pub fn from_env() -> Result<Self> {
        Self::builder(std::env::var("VOJO_SECRET_KEY").unwrap_or_default()).build()
    }

    /// A builder, to change the settings.
    pub fn builder(secret_key: impl Into<String>) -> ClientBuilder {
        ClientBuilder {
            key: secret_key.into(),
            organisation: None,
            base_url: DEFAULT_BASE_URL.to_owned(),
            version: API_VERSION.to_owned(),
            timeout: DEFAULT_TIMEOUT,
            max_retries: DEFAULT_MAX_RETRIES,
            http: None,
        }
    }

    /// A client that applies these options to its calls, sharing this client's
    /// connections. Use it for one call when you set an idempotency key:
    ///
    /// ```no_run
    /// # async fn run(client: vojo::Client) -> vojo::Result<()> {
    /// use std::time::Duration;
    /// use vojo::RequestOptions;
    ///
    /// let screen = client
    ///     .with_options(RequestOptions::new().timeout(Duration::from_secs(5)))
    ///     .screens()
    ///     .retrieve("scr_123")
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_options(&self, options: RequestOptions) -> Self {
        Self { inner: Arc::clone(&self.inner), options: self.options.merge(options) }
    }

    /// Webhook verification, the same as the functions in [`webhook`](crate::webhook).
    pub fn webhooks(&self) -> crate::webhook::Webhooks {
        crate::webhook::Webhooks
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.inner.base_url)
            .field("organisation", &self.inner.organisation)
            .finish_non_exhaustive()
    }
}

/// Builds a [`Client`].
pub struct ClientBuilder {
    key: String,
    organisation: Option<String>,
    base_url: String,
    version: String,
    timeout: Duration,
    max_retries: u32,
    http: Option<Arc<dyn HttpClient>>,
}

impl ClientBuilder {
    /// Sets `Vojo-Organisation`. OAuth callers must send it; secret keys are
    /// bound to one organisation and do not need it.
    pub fn organisation(mut self, id: impl Into<String>) -> Self {
        self.organisation = Some(id.into());
        self
    }

    /// Points the client somewhere else, such as a local mock.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        // Contract paths already include /v1, so a base URL ending in it would double up.
        let url = url.into();
        let url = url.trim_end_matches('/');
        self.base_url = url.strip_suffix("/v1").unwrap_or(url).trim_end_matches('/').to_owned();
        self
    }

    /// Overrides the pinned `Vojo-Version`.
    pub fn version(mut self, date: impl Into<String>) -> Self {
        self.version = date.into();
        self
    }

    /// The limit on each attempt. Zero means none.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// How many times a failed request is retried. Zero disables retries.
    pub fn max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// Supplies the HTTP layer, for proxies, custom transports or tests.
    pub fn http_client(mut self, http: impl HttpClient + 'static) -> Self {
        self.http = Some(Arc::new(http));
        self
    }

    /// Builds the client.
    pub fn build(self) -> Result<Client> {
        if self.key.is_empty() {
            return Err(Error::InvalidArgument(
                "no secret key: pass one to Client::new, or set VOJO_SECRET_KEY and use Client::from_env; \
                 keys are issued by Vojo during early access"
                    .into(),
            ));
        }
        // An HTTP library may quote a bad header value in its own error, so catch it
        // here, where the message can leave the key out.
        if self.key.bytes().any(|b| b <= 0x20 || b >= 0x7f) {
            return Err(Error::InvalidArgument(
                "the secret key contains spaces or control characters; check how it was copied".into(),
            ));
        }
        let http = match self.http {
            Some(http) => http,
            None => default_http()?,
        };
        Ok(Client {
            inner: Arc::new(Inner {
                key: self.key,
                organisation: self.organisation,
                base_url: self.base_url,
                version: self.version,
                timeout: self.timeout,
                max_retries: self.max_retries,
                http,
            }),
            options: RequestOptions::default(),
        })
    }
}

impl fmt::Debug for ClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientBuilder").field("base_url", &self.base_url).finish_non_exhaustive()
    }
}

#[cfg(feature = "reqwest")]
fn default_http() -> Result<Arc<dyn HttpClient>> {
    let http = crate::http::ReqwestClient::new()
        .map_err(|e| Error::InvalidArgument(format!("could not start the HTTP client: {e}")))?;
    Ok(Arc::new(http))
}

#[cfg(not(feature = "reqwest"))]
fn default_http() -> Result<Arc<dyn HttpClient>> {
    Err(Error::InvalidArgument(
        "built without an HTTP client: enable the rustls-tls or native-tls feature, or pass one to ClientBuilder::http_client"
            .into(),
    ))
}

/// Settings for the calls made through [`Client::with_options`].
#[derive(Clone, Debug, Default)]
pub struct RequestOptions {
    timeout: Option<Duration>,
    max_retries: Option<u32>,
    version: Option<String>,
    organisation: Option<String>,
    idempotency_key: Option<Arc<OneKey>>,
}

/// An idempotency key that one create may use. A second create with it would get
/// the first one's result replayed, so it is refused instead.
#[derive(Debug)]
struct OneKey {
    key: String,
    used: AtomicBool,
}

impl RequestOptions {
    /// No overrides.
    pub fn new() -> Self {
        Self::default()
    }

    /// The limit on each attempt.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// How many times a failed request is retried.
    pub fn max_retries(mut self, retries: u32) -> Self {
        self.max_retries = Some(retries);
        self
    }

    /// The `Vojo-Version` to send.
    pub fn version(mut self, date: impl Into<String>) -> Self {
        self.version = Some(date.into());
        self
    }

    /// The `Vojo-Organisation` to send.
    pub fn organisation(mut self, id: impl Into<String>) -> Self {
        self.organisation = Some(id.into());
        self
    }

    /// The `Idempotency-Key` for one create, in place of a generated one. Its
    /// retries reuse it. It has no effect on other calls, and a second create
    /// through the same options is refused, because the API would replay the
    /// first one's result.
    pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(Arc::new(OneKey { key: key.into(), used: AtomicBool::new(false) }));
        self
    }

    fn merge(&self, other: Self) -> Self {
        Self {
            timeout: other.timeout.or(self.timeout),
            max_retries: other.max_retries.or(self.max_retries),
            version: other.version.or_else(|| self.version.clone()),
            organisation: other.organisation.or_else(|| self.organisation.clone()),
            idempotency_key: other.idempotency_key.or_else(|| self.idempotency_key.clone()),
        }
    }
}

/// One logical API call, which may take several attempts.
#[derive(Clone)]
pub(crate) struct Request {
    pub(crate) method: &'static str,
    pub(crate) path: String,
    pub(crate) query: Vec<(&'static str, String)>,
    pub(crate) body: Option<Vec<u8>>,
    pub(crate) retry_safe: bool,
    // The contract accepts Idempotency-Key on some creates only, so a method opts in.
    pub(crate) keyed: bool,
    // False for an operation not served during early access: a 404 then says so.
    pub(crate) early_access: bool,
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
    #[serde(default)]
    meta: serde_json::Value,
}

impl Client {
    pub(crate) async fn data<T: DeserializeOwned>(&self, request: Request) -> Result<Response<T>> {
        let response = self.send(&request).await?;
        let envelope: Envelope<T> = decode(&response)?;
        Ok(Response::new(envelope.data, ResponseInfo::new(response.status, response.headers, envelope.meta)))
    }

    pub(crate) async fn void(&self, request: Request) -> Result<()> {
        self.send(&request).await.map(|_| ())
    }

    pub(crate) async fn send(&self, request: &Request) -> Result<HttpResponse> {
        let inner = &*self.inner;
        let options = &self.options;
        let timeout = options.timeout.unwrap_or(inner.timeout);
        let max_retries = options.max_retries.unwrap_or(inner.max_retries);

        let mut url = format!("{}{}", inner.base_url, request.path);
        for (i, (name, value)) in request.query.iter().enumerate() {
            url.push(if i == 0 { '?' } else { '&' });
            url.push_str(&format!("{}={}", util::encode(name), util::encode(value)));
        }

        let mut headers = Headers::new();
        headers.append("Authorization", format!("Bearer {}", inner.key));
        headers.append("Accept", "application/json");
        headers.append("User-Agent", format!("vojo-rust/{VERSION}"));
        headers.append("Vojo-Version", options.version.as_deref().unwrap_or(&inner.version));
        if request.body.is_some() {
            headers.append("Content-Type", "application/json");
        }
        if let Some(org) = options.organisation.as_deref().or(inner.organisation.as_deref()) {
            headers.append("Vojo-Organisation", org);
        }
        // Fixed before the first attempt, so every retry of a create carries the same key.
        if request.keyed {
            let key = match &options.idempotency_key {
                Some(one) if one.used.swap(true, Ordering::SeqCst) => return Err(Error::InvalidArgument(
                    "that idempotency key was already used by another create; set a new one for each call"
                        .into(),
                )),
                Some(one) => one.key.clone(),
                None => util::uuid_v4()?,
            };
            headers.append("Idempotency-Key", key);
        }

        let mut attempt: u32 = 0;
        loop {
            let retries_left = attempt < max_retries;
            let http_request = HttpRequest {
                method: request.method.to_owned(),
                url: url.clone(),
                headers: headers.clone(),
                body: request.body.clone(),
                timeout: (!timeout.is_zero()).then_some(timeout),
            };
            let sent = inner.http.send(http_request);
            let outcome = if timeout.is_zero() {
                sent.await
            } else {
                match tokio::time::timeout(timeout, sent).await {
                    Ok(outcome) => outcome,
                    Err(_) => {
                        Err(crate::http::TransportError::timeout(format!("no response within {timeout:?}")))
                    }
                }
            };

            let (error, retry, wait) = match outcome {
                Ok(response) if (200..300).contains(&response.status) => return Ok(response),
                Ok(response) => {
                    let error = ApiError::new(
                        response.status,
                        response.headers,
                        &response.body,
                        request.early_access,
                    );
                    let retry = should_retry_status(request, &error);
                    let wait = error.retry_after();
                    (Error::Api(Box::new(error)), retry, wait)
                }
                Err(failure) => {
                    // A connection that never opened cannot have reached the server, so
                    // resending is safe for anything.
                    let retry = failure.kind() == TransportErrorKind::Connect || request.retry_safe;
                    let timed_out = failure.kind() == TransportErrorKind::Timeout;
                    (Error::Connection(ConnectionError::new(timed_out, &failure.to_string())), retry, None)
                }
            };
            if !retries_left || !retry || wait.is_some_and(|wait| wait > LONGEST_RETRY_AFTER) {
                return Err(error);
            }
            tokio::time::sleep(wait.unwrap_or_else(|| backoff(attempt))).await;
            attempt += 1;
        }
    }
}

pub(crate) fn decode<T: DeserializeOwned>(response: &HttpResponse) -> Result<T> {
    serde_json::from_slice(&response.body)
        .map_err(|e| Error::Decode { status: response.status, message: util::redact(&e.to_string()) })
}

fn should_retry_status(request: &Request, error: &ApiError) -> bool {
    match error.status {
        // Rejected before the handler ran, whatever the method.
        429 => true,
        // Our earlier attempt is still running, typically after a dropped connection.
        409 if request.keyed => error.code.as_deref() == Some("idempotency_request_in_progress"),
        status => status >= 500 && request.retry_safe,
    }
}

fn backoff(attempt: u32) -> Duration {
    let ceiling = FIRST_BACKOFF.saturating_mul(1 << attempt.min(5)).min(MOST_BACKOFF);
    let half = ceiling / 2;
    let jitter = util::random_bytes::<8>().map(u64::from_le_bytes).unwrap_or(0);
    let nanos = half.as_nanos() as u64;
    half + Duration::from_nanos(if nanos == 0 { 0 } else { jitter % nanos })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_with_jitter_and_caps() {
        for (attempt, ceiling) in [(0, 500), (1, 1000), (2, 2000), (3, 4000), (4, 8000), (9, 8000)] {
            for _ in 0..50 {
                let wait = backoff(attempt).as_millis() as u64;
                assert!(wait >= ceiling / 2 && wait < ceiling, "attempt {attempt}: {wait}ms");
            }
        }
    }
}
