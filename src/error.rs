use std::fmt;
use std::time::Duration;

use crate::http::Headers;
use crate::util::redact;
use crate::webhook::VerificationError;

/// A `Result` whose error is [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Every error this library returns.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The API answered with an error status. Boxed, to keep `Result` small.
    Api(Box<ApiError>),
    /// The request got no response.
    Connection(ConnectionError),
    /// The API returned paging data that cannot be followed safely.
    Pagination(String),
    /// A webhook delivery is not authentic. Reject it with a 400.
    WebhookVerification(VerificationError),
    /// The API answered with a success status and a body that could not be decoded.
    Decode {
        /// The HTTP status.
        status: u16,
        /// What went wrong.
        message: String,
    },
    /// A programming error caught before sending, such as an empty id or a missing key.
    InvalidArgument(String),
}

impl Error {
    /// The API error, if this is one.
    pub fn as_api(&self) -> Option<&ApiError> {
        match self {
            Self::Api(e) => Some(e.as_ref()),
            _ => None,
        }
    }

    /// The HTTP status, if the API answered.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api(e) => Some(e.status),
            Self::Decode { status, .. } => Some(*status),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api(e) => e.fmt(f),
            Self::Connection(e) => e.fmt(f),
            Self::Pagination(m) => write!(f, "vojo: pagination: {m}"),
            Self::WebhookVerification(e) => e.fmt(f),
            Self::Decode { status, message } => {
                write!(
                    f,
                    "vojo: the API returned HTTP {status} with a body that could not be decoded: {message}"
                )
            }
            Self::InvalidArgument(m) => write!(f, "vojo: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Api(e) => Some(e.as_ref()),
            Self::Connection(e) => Some(e),
            Self::WebhookVerification(e) => Some(e),
            _ => None,
        }
    }
}

impl From<VerificationError> for Error {
    fn from(e: VerificationError) -> Self {
        Self::WebhookVerification(e)
    }
}

/// What kind of API error this is, from its status. Branch on this, or on
/// [`ApiError::code`] for detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ApiErrorKind {
    /// 401.
    Authentication,
    /// 402, a plan limit (`feature_key`, `limit` and `used` are in the problem), or 403.
    Permission,
    /// 404.
    NotFound,
    /// 400, 413 or 422. The field reasons are in [`ApiError::errors`].
    Validation,
    /// 409.
    Conflict,
    /// 429. See [`ApiError::retry_after`].
    RateLimit,
    /// 5xx.
    Server,
    /// Any other status.
    Other,
}

/// One field-level reason on a validation failure.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct FieldError {
    /// A JSON pointer to the field. Empty when the API gave none.
    pub pointer: String,
    /// A machine-readable reason. Empty when the API gave none.
    pub code: String,
    /// A human-readable reason. Empty when the API gave none.
    pub detail: String,
}

/// An error status from the API. Codes can be added within v1, so always
/// handle a code you have not seen.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ApiError {
    /// The HTTP status.
    pub status: u16,
    /// The error code, or `None` when a proxy, not the API, produced the response.
    pub code: Option<String>,
    /// The problem's `detail`, else its `title`, else a line with the status.
    pub message: String,
    /// The request id, to quote to Vojo.
    pub request_id: Option<String>,
    /// The field reasons, on a validation failure.
    pub errors: Vec<FieldError>,
    /// The whole problem+json body, including members this version does not
    /// know about. `Null` when the body was not JSON.
    pub problem: serde_json::Value,
    /// The response headers.
    pub headers: Headers,
}

impl ApiError {
    /// What kind of error this is.
    pub fn kind(&self) -> ApiErrorKind {
        match self.status {
            401 => ApiErrorKind::Authentication,
            402 | 403 => ApiErrorKind::Permission,
            404 => ApiErrorKind::NotFound,
            400 | 413 | 422 => ApiErrorKind::Validation,
            409 => ApiErrorKind::Conflict,
            429 => ApiErrorKind::RateLimit,
            500..=599 => ApiErrorKind::Server,
            _ => ApiErrorKind::Other,
        }
    }

    /// The wait the API asked for, if it gave one.
    pub fn retry_after(&self) -> Option<Duration> {
        parse_retry_after(self.headers.get("Retry-After")?)
    }

    pub(crate) fn new(status: u16, headers: Headers, body: &[u8], early_access: bool) -> Self {
        // Redacted before parsing, so an echoed key is gone from the problem as well as the message.
        let text = redact(&String::from_utf8_lossy(body));
        // A proxy's HTML error page is not JSON; it still maps by status, without a code.
        let problem: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        let field = |name: &str| {
            problem.get(name).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_owned)
        };
        let errors = field_errors(&problem);
        let mut message = field("detail")
            .or_else(|| field("title"))
            .unwrap_or_else(|| format!("the API returned HTTP {status}"));
        if matches!(status, 400 | 413 | 422) {
            message.push_str(&describe(&errors));
        }
        if (300..400).contains(&status) {
            message.push_str(" The API never redirects: check the base URL.");
        }
        if status == 404 && !early_access {
            message.push_str(" This operation may not be available yet during early access.");
        }
        let request_id = field("request_id").or_else(|| headers.get("Vojo-Request-Id").map(str::to_owned));
        Self { status, code: field("code"), message, request_id, errors, problem, headers }
    }
}

/// Reads `Retry-After` as seconds. A value that is not plain digits, with an
/// optional fraction, counts as not given. One too large for a `Duration` is the
/// longest one, which is never slept through.
pub(crate) fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    let (whole, fraction) = value.split_once('.').unwrap_or((value, "0"));
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || !digits(fraction) {
        return None;
    }
    let seconds: f64 = value.parse().ok()?;
    Some(Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX))
}

/// The field errors in a problem. A part that is not a non-empty string counts as missing.
fn field_errors(problem: &serde_json::Value) -> Vec<FieldError> {
    let part = |entry: &serde_json::Value, name: &str| {
        entry.get(name).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or_default().to_owned()
    };
    let Some(entries) = problem.get("errors").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    entries
        .iter()
        .map(|e| FieldError { pointer: part(e, "pointer"), code: part(e, "code"), detail: part(e, "detail") })
        .collect()
}

/// The field errors appended to a validation message, as every Vojo library writes them.
fn describe(errors: &[FieldError]) -> String {
    let entries: Vec<String> = errors
        .iter()
        .map(|e| {
            let mut entry = e.pointer.clone();
            if !e.code.is_empty() {
                entry.push_str(&if entry.is_empty() {
                    format!("({})", e.code)
                } else {
                    format!(" ({})", e.code)
                });
            }
            if !e.detail.is_empty() {
                entry.push_str(&if entry.is_empty() { e.detail.clone() } else { format!(": {}", e.detail) });
            }
            entry
        })
        .filter(|entry| !entry.is_empty())
        .collect();
    if entries.is_empty() {
        return String::new();
    }
    let mut shown: Vec<String> = entries.iter().take(5).cloned().collect();
    if entries.len() > 5 {
        shown.push(format!("and {} more", entries.len() - 5));
    }
    format!(" {}", shown.join("; "))
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "vojo: {}", self.status)?;
        if let Some(code) = &self.code {
            write!(f, " {code}")?;
        }
        write!(f, ": {}", self.message)?;
        if let Some(id) = &self.request_id {
            write!(f, " (request id: {id})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {}

/// A request that got no response.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ConnectionError {
    /// True when it timed out.
    pub timeout: bool,
    /// What went wrong, with any key redacted.
    pub message: String,
}

impl ConnectionError {
    pub(crate) fn new(timeout: bool, cause: &str) -> Self {
        Self { timeout, message: redact(cause) }
    }
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.timeout {
            write!(f, "vojo: the request timed out: {}", self.message)
        } else {
            write!(f, "vojo: could not reach the API: {}", self.message)
        }
    }
}

impl std::error::Error for ConnectionError {}
