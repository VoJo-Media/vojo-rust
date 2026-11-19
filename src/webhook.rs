//! Verifies deliveries from Vojo. Needs no API key or client.
//!
//! Pass the raw request body, exactly as received. Decoding and re-encoding the
//! JSON changes the bytes, and the signature will not match.
//!
//! ```
//! use vojo::webhook;
//!
//! let secret = "vwsec_example";
//! let body = br#"{"eventType":"verification.ping","eventId":"evt_1","attemptId":"att_1"}"#;
//! let header = webhook::generate_test_header(body, secret, std::time::SystemTime::now());
//!
//! let event = webhook::construct_event(body, Some(&header), secret)?;
//! assert_eq!(event.event_type, webhook::event_type::VERIFICATION_PING);
//! # Ok::<(), vojo::Error>(())
//! ```

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::util::hex;

pub use crate::generated::webhook_events::{event_type, events};

/// The HTTP header carrying a delivery's signature.
pub const SIGNATURE_HEADER: &str = "X-Vojo-Signature";

/// How far a signature's timestamp may be from the current time, either way.
pub const DEFAULT_TOLERANCE: Duration = Duration::from_secs(300);

/// The largest tolerance allowed, one day. A longer one would turn the replay check off.
pub const MAX_TOLERANCE: Duration = Duration::from_secs(86_400);

/// Why a delivery failed verification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VerificationErrorKind {
    /// No signature header, or an empty one.
    MissingHeader,
    /// The header has no single numeric timestamp, or no `v1` signature.
    MalformedHeader,
    /// No signature matches any secret held.
    NoMatchingSignature,
    /// The signature's timestamp is too far from now.
    TimestampOutOfTolerance,
    /// The signed body is not a JSON object.
    InvalidPayload,
}

impl VerificationErrorKind {
    /// The kind as named across every Vojo library, such as `missing_header`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MissingHeader => "missing_header",
            Self::MalformedHeader => "malformed_header",
            Self::NoMatchingSignature => "no_matching_signature",
            Self::TimestampOutOfTolerance => "timestamp_out_of_tolerance",
            Self::InvalidPayload => "invalid_payload",
        }
    }
}

/// A delivery that is not authentic. Reject it with a 400.
#[derive(Clone, Debug)]
pub struct VerificationError {
    kind: VerificationErrorKind,
    message: String,
}

impl VerificationError {
    fn new(kind: VerificationErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    /// Why it failed.
    pub fn kind(&self) -> VerificationErrorKind {
        self.kind
    }
}

impl fmt::Display for VerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "vojo webhook: {}", self.message)
    }
}

impl std::error::Error for VerificationError {}

/// A verified delivery.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Event {
    /// The event type, such as `verification.ping`. Compare with [`event_type`].
    pub event_type: String,
    /// Stays the same across retries of the same event. Dedupe on it.
    pub id: String,
    /// Changes on every delivery attempt.
    pub attempt_id: String,
    /// The whole body.
    pub payload: serde_json::Value,
}

impl Event {
    /// Decodes the whole body, such as into the matching type in [`events`].
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T> {
        T::deserialize(&self.payload).map_err(|e| {
            Error::WebhookVerification(VerificationError::new(
                VerificationErrorKind::InvalidPayload,
                format!("the event does not decode into that type: {e}"),
            ))
        })
    }
}

/// Verifies deliveries with settings other than the defaults.
///
/// While you roll a secret, hold both: a delivery matching any secret is accepted.
///
/// ```
/// # let (body, header) = (b"{}", "t=1,v1=00");
/// use vojo::webhook::Verifier;
///
/// let result = Verifier::new("vwsec_new").secret("vwsec_old").construct_event(body, Some(header));
/// # assert!(result.is_err());
/// ```
#[derive(Clone)]
pub struct Verifier {
    secrets: Vec<String>,
    tolerance: Duration,
    now: Option<SystemTime>,
}

impl fmt::Debug for Verifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Verifier")
            .field("secrets", &self.secrets.len())
            .field("tolerance", &self.tolerance)
            .finish()
    }
}

impl Verifier {
    /// A verifier holding one secret.
    pub fn new(secret: impl Into<String>) -> Self {
        Self { secrets: vec![secret.into()], tolerance: DEFAULT_TOLERANCE, now: None }
    }

    /// Also accepts signatures from another secret.
    pub fn secret(mut self, secret: impl Into<String>) -> Self {
        self.secrets.push(secret.into());
        self
    }

    /// Changes [`DEFAULT_TOLERANCE`]. It must be above zero and at most
    /// [`MAX_TOLERANCE`]; the check cannot be turned off.
    pub fn tolerance(mut self, tolerance: Duration) -> Self {
        self.tolerance = tolerance;
        self
    }

    /// Overrides the current time, for tests.
    pub fn now(mut self, now: SystemTime) -> Self {
        self.now = Some(now);
        self
    }

    /// Returns an error unless the delivery is authentic.
    pub fn verify(&self, body: &[u8], header: Option<&str>) -> Result<()> {
        if self.secrets.iter().any(String::is_empty) {
            return Err(Error::InvalidArgument("the webhook signing secret is empty".into()));
        }
        if self.tolerance.is_zero() || self.tolerance > MAX_TOLERANCE {
            return Err(Error::InvalidArgument(
                "the webhook tolerance must be above zero and at most MAX_TOLERANCE, one day".into(),
            ));
        }
        let header = header.unwrap_or("");
        if header.is_empty() {
            return Err(invalid(
                VerificationErrorKind::MissingHeader,
                format!(
                    "the {SIGNATURE_HEADER} header is missing; an endpoint whose secret predates signing \
                     sends no header until its secret is rolled"
                ),
            ));
        }
        let (timestamp, signatures) = parse_header(header)?;

        // Every comparison runs even after a match, so timing reveals nothing about
        // which signature matched or how many were sent.
        let mut matched = false;
        for secret in &self.secrets {
            let expected = sign(secret, timestamp, body);
            for candidate in &signatures {
                matched |= constant_time_eq(candidate.as_bytes(), expected.as_bytes());
            }
        }
        if !matched {
            return Err(invalid(
                VerificationErrorKind::NoMatchingSignature,
                "no signature matches; check the signing secret, and that you passed the raw request body \
                 rather than decoded or re-encoded JSON",
            ));
        }

        let seconds: i128 = timestamp.parse().map_err(|_| bad_timestamp())?;
        let now = self.now.unwrap_or_else(SystemTime::now);
        let now = match now.duration_since(UNIX_EPOCH) {
            Ok(since) => since.as_secs_f64(),
            Err(before) => -before.duration().as_secs_f64(),
        };
        if (now - seconds as f64).abs() > self.tolerance.as_secs_f64() {
            return Err(invalid(
                VerificationErrorKind::TimestampOutOfTolerance,
                format!(
                    "the signature timestamp is outside the {:?} tolerance; check this machine's clock, \
                     or this may be a replayed delivery",
                    self.tolerance
                ),
            ));
        }
        Ok(())
    }

    /// Verifies a delivery and returns it as an [`Event`].
    pub fn construct_event(&self, body: &[u8], header: Option<&str>) -> Result<Event> {
        self.verify(body, header)?;
        let payload: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
        let Some(fields) = payload.as_object() else {
            return Err(invalid(
                VerificationErrorKind::InvalidPayload,
                "the signed body is not a JSON object",
            ));
        };
        let first = |names: [&str; 2]| {
            names
                .iter()
                .find_map(|name| fields.get(*name).and_then(|v| v.as_str()).filter(|s| !s.is_empty()))
                .unwrap_or_default()
                .to_owned()
        };
        Ok(Event {
            event_type: first(["eventType", "event_type"]),
            id: first(["eventId", "event_id"]),
            attempt_id: first(["attemptId", "attempt_id"]),
            payload,
        })
    }
}

/// Returns an error unless the delivery is authentic. `header` is the
/// [`SIGNATURE_HEADER`] value, or `None` when it is absent.
pub fn verify_signature(body: &[u8], header: Option<&str>, secret: &str) -> Result<()> {
    Verifier::new(secret).verify(body, header)
}

/// Verifies a delivery and returns it as an [`Event`].
pub fn construct_event(body: &[u8], header: Option<&str>, secret: &str) -> Result<Event> {
    Verifier::new(secret).construct_event(body, header)
}

/// Signs a payload as Vojo would, to test your own webhook handler.
pub fn generate_test_header(body: &[u8], secret: &str, timestamp: SystemTime) -> String {
    let seconds = timestamp.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0).to_string();
    let signature = sign(secret, &seconds, body);
    format!("t={seconds},v1={signature}")
}

/// The webhook functions, from [`Client::webhooks`](crate::Client::webhooks).
#[derive(Clone, Copy, Debug, Default)]
pub struct Webhooks;

impl Webhooks {
    /// See [`verify_signature`].
    pub fn verify_signature(&self, body: &[u8], header: Option<&str>, secret: &str) -> Result<()> {
        verify_signature(body, header, secret)
    }

    /// See [`construct_event`].
    pub fn construct_event(&self, body: &[u8], header: Option<&str>, secret: &str) -> Result<Event> {
        construct_event(body, header, secret)
    }

    /// See [`generate_test_header`].
    pub fn generate_test_header(&self, body: &[u8], secret: &str, timestamp: SystemTime) -> String {
        generate_test_header(body, secret, timestamp)
    }
}

fn invalid(kind: VerificationErrorKind, message: impl Into<String>) -> Error {
    Error::WebhookVerification(VerificationError::new(kind, message))
}

fn bad_timestamp() -> Error {
    invalid(
        VerificationErrorKind::MalformedHeader,
        format!("the {SIGNATURE_HEADER} header has a bad timestamp"),
    )
}

fn parse_header(header: &str) -> Result<(&str, Vec<&str>)> {
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        let part = part.trim();
        let (name, value) = part.split_once('=').unwrap_or((part, ""));
        match name {
            "t" => {
                let digits =
                    !value.is_empty() && value.len() <= 18 && value.bytes().all(|b| b.is_ascii_digit());
                if timestamp.is_some() || !digits {
                    return Err(bad_timestamp());
                }
                timestamp = Some(value);
            }
            "v1" => signatures.push(value),
            _ => {}
        }
    }
    let Some(timestamp) = timestamp else {
        return Err(invalid(
            VerificationErrorKind::MalformedHeader,
            format!("the {SIGNATURE_HEADER} header has no timestamp"),
        ));
    };
    if signatures.is_empty() {
        return Err(invalid(
            VerificationErrorKind::MalformedHeader,
            format!("the {SIGNATURE_HEADER} header has no v1 signature"),
        ));
    }
    Ok((timestamp, signatures))
}

/// The signature covers the timestamp exactly as it was sent.
fn sign(secret: &str, timestamp: &str, body: &[u8]) -> String {
    hex(&hmac_sha256(secret.as_bytes(), &[timestamp.as_bytes(), b".", body]))
}

fn hmac_sha256(key: &[u8], message: &[&[u8]]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    for part in message {
        inner.update(part);
    }
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    // Hex of the wrong length simply does not match.
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test cases 2 and 6: a short key, and a key longer than one block.
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", &[b"what do ya want ", b"for nothing?"])),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex(&hmac_sha256(&[0xaa; 131], &[b"Test Using Larger Than Block-Size Key - Hash Key First"])),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn misuse_is_a_programming_error() {
        let header = generate_test_header(b"{}", "s", SystemTime::now());
        assert!(matches!(verify_signature(b"{}", Some(&header), ""), Err(Error::InvalidArgument(_))));
        for bad in [Duration::ZERO, MAX_TOLERANCE + Duration::from_secs(1), Duration::MAX] {
            let result = Verifier::new("s").tolerance(bad).verify(b"{}", Some(&header));
            assert!(matches!(result, Err(Error::InvalidArgument(_))), "{bad:?}");
        }
        assert!(Verifier::new("s").tolerance(MAX_TOLERANCE).verify(b"{}", Some(&header)).is_ok());
    }

    #[test]
    fn test_header_round_trips() {
        let body = br#"{"event_type":"wrapper.ready","event_id":"e","attempt_id":"a"}"#;
        let header = generate_test_header(body, "vwsec_x", SystemTime::now());
        let event = construct_event(body, Some(&header), "vwsec_x").unwrap();
        assert_eq!(
            (event.event_type.as_str(), event.id.as_str(), event.attempt_id.as_str()),
            ("wrapper.ready", "e", "a")
        );
    }
}
