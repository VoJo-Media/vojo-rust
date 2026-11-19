use std::collections::{HashSet, VecDeque};
use std::fmt;

use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::client::{decode, Client, Request};
use crate::error::{Error, Result};
use crate::response::ResponseInfo;

/// One page of a list.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Page<T> {
    /// The items on this page.
    pub data: Vec<T>,
    /// Whether another page exists.
    pub has_more: bool,
    /// The cursor for the next page, if there is one.
    pub next_cursor: Option<String>,
    /// The HTTP details, with the page's whole `meta`.
    pub info: ResponseInfo,
    has_more_sent: Option<bool>,
}

#[derive(Deserialize)]
struct RawPage<T> {
    data: Vec<T>,
    #[serde(default)]
    meta: serde_json::Value,
}

/// Returned by list operations. Nothing is sent until you use it.
///
/// ```no_run
/// # async fn run(client: vojo::Client) -> vojo::Result<()> {
/// let mut screens = client.screens().list(Default::default());
/// while let Some(screen) = screens.next().await {
///     println!("{}", screen?.name);
/// }
/// # Ok(())
/// # }
/// ```
///
/// A list's `limit` sets the page size (at most 100), not the number of items
/// returned. Use [`next`](Self::next) for items or [`next_page`](Self::next_page)
/// for pages, not both on one list.
pub struct List<T> {
    client: Option<Client>,
    request: Option<Request>,
    // A bad id caught before sending, reported on first use.
    failed: Option<String>,
    cursor: Option<String>,
    started: bool,
    done: bool,
    pending: Option<Error>,
    seen: HashSet<String>,
    buffer: VecDeque<T>,
}

impl<T> fmt::Debug for List<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("List").field("cursor", &self.cursor).field("done", &self.done).finish_non_exhaustive()
    }
}

impl<T: DeserializeOwned> List<T> {
    pub(crate) fn new(client: Client, request: Request, cursor: Option<String>) -> Self {
        Self {
            client: Some(client),
            request: Some(request),
            failed: None,
            cursor: cursor.filter(|c| !c.is_empty()),
            started: false,
            done: false,
            pending: None,
            seen: HashSet::new(),
            buffer: VecDeque::new(),
        }
    }

    /// A list whose first use reports `error`, for a bad id caught before sending.
    pub(crate) fn failed(error: Error) -> Self {
        let message = match error {
            Error::InvalidArgument(message) => message,
            other => other.to_string(),
        };
        Self {
            client: None,
            request: None,
            failed: Some(message),
            cursor: None,
            started: false,
            done: false,
            pending: None,
            seen: HashSet::new(),
            buffer: VecDeque::new(),
        }
    }

    /// Fetches the first page only. It does not move this list on.
    pub async fn first_page(&self) -> Result<Page<T>> {
        if let Some(message) = &self.failed {
            return Err(Error::InvalidArgument(message.clone()));
        }
        self.fetch(self.cursor.clone()).await
    }

    /// The next page, or `None` after the last one. An error is returned once,
    /// and then the list ends.
    pub async fn next_page(&mut self) -> Option<Result<Page<T>>> {
        if self.done {
            return None;
        }
        if let Some(error) = self.failed.take().map(Error::InvalidArgument).or_else(|| self.pending.take()) {
            self.done = true;
            return Some(Err(error));
        }
        if !self.started {
            self.started = true;
            // The caller's own starting cursor counts as followed.
            if let Some(cursor) = &self.cursor {
                self.seen.insert(cursor.clone());
            }
        }
        let page = match self.fetch(self.cursor.clone()).await {
            Ok(page) => page,
            Err(error) => {
                self.done = true;
                return Some(Err(error));
            }
        };
        match next_cursor(&page, &mut self.seen) {
            Ok(Some(cursor)) => self.cursor = Some(cursor),
            Ok(None) => self.done = true,
            // The page is good; the error comes on the next call.
            Err(error) => self.pending = Some(error),
        }
        Some(Ok(page))
    }

    /// The next item, fetching pages as needed, or `None` after the last one.
    pub async fn next(&mut self) -> Option<Result<T>> {
        loop {
            if let Some(item) = self.buffer.pop_front() {
                return Some(Ok(item));
            }
            match self.next_page().await? {
                Ok(page) => self.buffer.extend(page.data),
                Err(error) => return Some(Err(error)),
            }
        }
    }

    /// Every remaining item, across pages.
    pub async fn collect(mut self) -> Result<Vec<T>> {
        let mut items = Vec::new();
        while let Some(item) = self.next().await {
            items.push(item?);
        }
        Ok(items)
    }

    async fn fetch(&self, cursor: Option<String>) -> Result<Page<T>> {
        let (Some(client), Some(request)) = (&self.client, &self.request) else {
            return Err(Error::InvalidArgument("this list cannot be fetched".into()));
        };
        let mut request = request.clone();
        if let Some(cursor) = cursor {
            request.query.retain(|(name, _)| *name != "cursor");
            request.query.push(("cursor", cursor));
        }
        let response = client.send(&request).await?;
        let raw: RawPage<T> = decode(&response)?;
        let has_more_sent = raw.meta.get("has_more").and_then(|v| v.as_bool());
        let next_cursor = raw.meta.get("next_cursor").and_then(|v| v.as_str()).map(str::to_owned);
        Ok(Page {
            data: raw.data,
            has_more: has_more_sent.unwrap_or(false),
            next_cursor,
            info: ResponseInfo::new(response.status, response.headers, raw.meta),
            has_more_sent,
        })
    }
}

/// The cursor to follow next, or `None` at the end. An error rather than a loop.
fn next_cursor<T>(page: &Page<T>, seen: &mut HashSet<String>) -> Result<Option<String>> {
    // has_more is authoritative: honour false even if a cursor came with it.
    if page.has_more_sent == Some(false) {
        return Ok(None);
    }
    let cursor = page.next_cursor.as_deref().unwrap_or("");
    if cursor.is_empty() {
        if page.has_more_sent == Some(true) {
            return Err(Error::Pagination("the API reported more pages but returned no next_cursor".into()));
        }
        return Ok(None);
    }
    if !seen.insert(cursor.to_owned()) {
        return Err(Error::Pagination(format!(
            "the API repeated the cursor {cursor:?}; stopping rather than looping forever"
        )));
    }
    Ok(Some(cursor.to_owned()))
}
