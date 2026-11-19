//! A scripted HTTP stub for the crate's own tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::http::{BoxFuture, Headers, HttpClient, HttpRequest, HttpResponse, TransportError};
use crate::Client;

#[derive(Default)]
pub(crate) struct Stub {
    script: Mutex<VecDeque<(u16, String)>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl Stub {
    pub(crate) fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

struct Shared(Arc<Stub>);

impl HttpClient for Shared {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        self.0.requests.lock().unwrap().push(request);
        let next = self.0.script.lock().unwrap().pop_front();
        Box::pin(async move {
            let (status, body) = next.expect("the stub ran out of responses");
            Ok(HttpResponse::new(status, Headers::new(), body))
        })
    }
}

pub(crate) fn stub_client(script: Vec<(u16, &str)>) -> (Client, Arc<Stub>) {
    let stub = Arc::new(Stub::default());
    *stub.script.lock().unwrap() = script.into_iter().map(|(s, b)| (s, b.to_owned())).collect();
    let client = Client::builder(["vojo_", "live_", "unit"].concat())
        .base_url("https://stub.example")
        .http_client(Shared(Arc::clone(&stub)))
        .build()
        .unwrap();
    (client, stub)
}

pub(crate) fn path_of(url: &str) -> &str {
    let path = url.strip_prefix("https://stub.example").unwrap_or(url);
    path.split('?').next().unwrap_or(path)
}
