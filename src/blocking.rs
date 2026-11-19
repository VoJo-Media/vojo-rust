//! A client for code that is not async. Enable the `blocking` feature.
//!
//! ```no_run
//! # fn run() -> vojo::Result<()> {
//! let client = vojo::blocking::Client::new("vojo_live_...")?;
//! for screen in client.screens().list(Default::default()) {
//!     println!("{}", screen?.name);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Each client runs its own single-threaded runtime. Do not call it from inside
//! an async runtime: use [`crate::Client`] there.

use std::fmt;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use tokio::runtime::Runtime;

use crate::error::{Error, Result};
use crate::{ClientBuilder, Page, RequestOptions};

/// The resources, reached from [`Client`].
pub mod resources {
    pub use crate::generated::blocking::*;
}

/// The blocking twin of [`crate::Client`]. Cloning is cheap.
#[derive(Clone)]
pub struct Client {
    pub(crate) inner: crate::Client,
    pub(crate) runtime: Arc<Runtime>,
}

impl Client {
    /// A client with the default settings.
    pub fn new(secret_key: impl Into<String>) -> Result<Self> {
        Self::from_async(crate::Client::new(secret_key)?)
    }

    /// A client using the key in `VOJO_SECRET_KEY`.
    pub fn from_env() -> Result<Self> {
        Self::from_async(crate::Client::from_env()?)
    }

    /// A client built with [`crate::Client::builder`].
    pub fn from_builder(builder: ClientBuilder) -> Result<Self> {
        Self::from_async(builder.build()?)
    }

    fn from_async(inner: crate::Client) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::InvalidArgument(format!("could not start a runtime: {e}")))?;
        Ok(Self { inner, runtime: Arc::new(runtime) })
    }

    /// See [`crate::Client::with_options`].
    pub fn with_options(&self, options: RequestOptions) -> Self {
        Self { inner: self.inner.with_options(options), runtime: Arc::clone(&self.runtime) }
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

/// Returned by list operations: an iterator over every item, fetching pages as
/// it goes. Nothing is sent until you use it.
pub struct List<T> {
    inner: crate::List<T>,
    runtime: Arc<Runtime>,
}

impl<T> fmt::Debug for List<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

impl<T: DeserializeOwned> List<T> {
    pub(crate) fn new(inner: crate::List<T>, runtime: Arc<Runtime>) -> Self {
        Self { inner, runtime }
    }

    /// Fetches the first page only.
    pub fn first_page(&self) -> Result<Page<T>> {
        self.runtime.block_on(self.inner.first_page())
    }

    /// The next page, or `None` after the last one.
    pub fn next_page(&mut self) -> Option<Result<Page<T>>> {
        self.runtime.block_on(self.inner.next_page())
    }
}

impl<T: DeserializeOwned> Iterator for List<T> {
    type Item = Result<T>;

    fn next(&mut self) -> Option<Result<T>> {
        self.runtime.block_on(self.inner.next())
    }
}
