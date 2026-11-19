//! The HTTP layer. Swap it with [`ClientBuilder::http_client`](crate::ClientBuilder::http_client),
//! for a proxy, a custom TLS setup or tests.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// A boxed future, so [`HttpClient`] can be used as a trait object.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Sends one HTTP attempt. Retries, timeouts and errors are handled above it.
///
/// It must not follow redirects: the API never sends one, and following one can
/// turn a POST into a GET that looks like a success, or resend the key elsewhere.
///
/// Return [`TransportError::connect`] only when the request certainly never left
/// this machine, such as a refused connection or a failed DNS lookup: the client
/// then resends even a request that is not safe to repeat.
pub trait HttpClient: Send + Sync {
    /// Sends the request and reads the whole response body.
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>>;
}

/// HTTP headers, in order, with names compared case-insensitively. Their debug
/// form hides the `Authorization` value.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    /// No headers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a header, keeping any already there with the same name.
    pub fn append(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.0.push((name.into(), value.into()));
    }

    /// The first value for `name`, if any.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// Every header, in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }
}

impl fmt::Debug for Headers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.0.iter().map(|(n, v)| {
                let shown = if n.eq_ignore_ascii_case("authorization") { "<redacted>" } else { v.as_str() };
                (n.as_str(), shown)
            }))
            .finish()
    }
}

impl<N: Into<String>, V: Into<String>> FromIterator<(N, V)> for Headers {
    fn from_iter<I: IntoIterator<Item = (N, V)>>(iter: I) -> Self {
        Self(iter.into_iter().map(|(n, v)| (n.into(), v.into())).collect())
    }
}

/// One HTTP attempt.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct HttpRequest {
    /// `GET`, `POST` and so on.
    pub method: String,
    /// The full URL, query included.
    pub url: String,
    /// Request headers, including `Authorization`.
    pub headers: Headers,
    /// The JSON body, if any.
    pub body: Option<Vec<u8>>,
    /// The limit on this attempt. The client enforces it too.
    pub timeout: Option<Duration>,
}

/// A whole HTTP response.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct HttpResponse {
    /// The status code.
    pub status: u16,
    /// Response headers.
    pub headers: Headers,
    /// The whole body.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// A response, for your own [`HttpClient`].
    pub fn new(status: u16, headers: Headers, body: impl Into<Vec<u8>>) -> Self {
        Self { status, headers, body: body.into() }
    }
}

/// How an attempt failed to get a response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransportErrorKind {
    /// The connection never opened, so the server cannot have seen the request.
    Connect,
    /// The attempt timed out.
    Timeout,
    /// Anything else. The server may have seen the request.
    Other,
}

/// An attempt that got no response.
#[derive(Clone, Debug)]
pub struct TransportError {
    kind: TransportErrorKind,
    message: String,
}

impl TransportError {
    /// The connection never opened.
    pub fn connect(message: impl fmt::Display) -> Self {
        Self { kind: TransportErrorKind::Connect, message: message.to_string() }
    }

    /// The attempt timed out.
    pub fn timeout(message: impl fmt::Display) -> Self {
        Self { kind: TransportErrorKind::Timeout, message: message.to_string() }
    }

    /// Any other failure.
    pub fn other(message: impl fmt::Display) -> Self {
        Self { kind: TransportErrorKind::Other, message: message.to_string() }
    }

    /// How it failed.
    pub fn kind(&self) -> TransportErrorKind {
        self.kind
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TransportError {}

#[cfg(feature = "reqwest")]
pub(crate) struct ReqwestClient(reqwest::Client);

#[cfg(feature = "reqwest")]
impl ReqwestClient {
    pub(crate) fn new() -> Result<Self, String> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map(Self)
            .map_err(|e| e.to_string())
    }
}

#[cfg(feature = "reqwest")]
impl HttpClient for ReqwestClient {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        Box::pin(async move {
            let method =
                reqwest::Method::from_bytes(request.method.as_bytes()).map_err(TransportError::connect)?;
            let mut builder = self.0.request(method, &request.url);
            for (name, value) in request.headers.iter() {
                builder = builder.header(name, value);
            }
            if let Some(timeout) = request.timeout {
                builder = builder.timeout(timeout);
            }
            if let Some(body) = request.body {
                builder = builder.body(body);
            }
            let response = builder.send().await.map_err(transport_error)?;
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .map(|(n, v)| (n.as_str().to_owned(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
                .collect();
            let body = response.bytes().await.map_err(transport_error)?;
            Ok(HttpResponse::new(status, headers, body.to_vec()))
        })
    }
}

#[cfg(feature = "reqwest")]
fn transport_error(error: reqwest::Error) -> TransportError {
    // reqwest's own message is terse; the causes say what actually went wrong.
    let mut message = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    if error.is_connect() {
        TransportError::connect(message)
    } else if error.is_timeout() {
        TransportError::timeout(message)
    } else {
        TransportError::other(message)
    }
}
