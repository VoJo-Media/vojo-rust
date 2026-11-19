#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_auto_cfg))]

mod client;
mod error;
mod generated;
pub mod http;
mod paging;
mod response;
mod util;
pub mod webhook;

#[cfg(test)]
mod test_support;

#[cfg(feature = "blocking")]
pub mod blocking;

pub use client::{
    Client, ClientBuilder, RequestOptions, DEFAULT_BASE_URL, DEFAULT_MAX_RETRIES, DEFAULT_TIMEOUT, VERSION,
};
pub use error::{ApiError, ApiErrorKind, ConnectionError, Error, FieldError, Result};
pub use generated::API_VERSION;
pub use paging::{List, Page};
pub use response::{Response, ResponseInfo};

/// The objects the API returns and the request bodies it takes.
pub mod models {
    pub use crate::generated::models::*;
}

/// The query of each call that takes one. Every field is optional.
pub mod params {
    pub use crate::generated::params::*;
}

/// The resources, reached from [`Client`]: `client.screens()`,
/// `client.webhook_endpoints().deliveries()` and so on.
pub mod resources {
    pub use crate::generated::resources::*;
}
