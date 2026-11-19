use std::ops::{Deref, DerefMut};

use serde::de::DeserializeOwned;

use crate::error::{Error, Result};
use crate::http::Headers;

/// The HTTP details of the response behind a result.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ResponseInfo {
    /// The HTTP status.
    pub status: u16,
    /// The response headers.
    pub headers: Headers,
    /// The `Vojo-Request-Id`, to quote to Vojo.
    pub request_id: Option<String>,
    /// The response's `meta`, or `Null` when it had none.
    pub meta: serde_json::Value,
    /// True when an idempotency key replayed an earlier result.
    pub idempotent_replayed: bool,
}

impl ResponseInfo {
    pub(crate) fn new(status: u16, headers: Headers, meta: serde_json::Value) -> Self {
        Self {
            status,
            request_id: headers.get("Vojo-Request-Id").map(str::to_owned),
            idempotent_replayed: headers.get("Idempotent-Replayed") == Some("true"),
            headers,
            meta,
        }
    }
}

/// A result together with its HTTP details. It dereferences to the result, so
/// `screen.name` works on a `Response<Screen>`.
#[derive(Clone, Debug)]
pub struct Response<T> {
    data: T,
    info: ResponseInfo,
}

impl<T> Response<T> {
    pub(crate) fn new(data: T, info: ResponseInfo) -> Self {
        Self { data, info }
    }

    /// The HTTP details.
    pub fn info(&self) -> &ResponseInfo {
        &self.info
    }

    /// The response's `meta`. Statistics responses carry real information here.
    pub fn meta(&self) -> &serde_json::Value {
        &self.info.meta
    }

    /// Decodes the response's `meta` into your own type.
    pub fn meta_as<M: DeserializeOwned>(&self) -> Result<M> {
        M::deserialize(&self.info.meta)
            .map_err(|e| Error::Decode { status: self.info.status, message: e.to_string() })
    }

    /// The result, without the HTTP details.
    pub fn into_inner(self) -> T {
        self.data
    }

    /// The result and the HTTP details.
    pub fn into_parts(self) -> (T, ResponseInfo) {
        (self.data, self.info)
    }
}

impl<T> Deref for Response<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.data
    }
}

impl<T> DerefMut for Response<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.data
    }
}
