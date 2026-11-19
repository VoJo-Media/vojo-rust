//! A scripted stub for the HTTP layer. It records every request it is sent.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use vojo::http::{BoxFuture, Headers, HttpClient, HttpRequest, HttpResponse, TransportError};
use vojo::Client;

/// A live-mode key, built in parts so no file holds one as a literal.
pub fn key() -> String {
    ["vojo_", "live_", "0123456789abcdef0123456789abcdef"].concat()
}

pub enum Step {
    Respond(u16, Vec<(&'static str, String)>, String),
    Fail(TransportError),
    Hang,
}

pub fn respond(status: u16, body: &str) -> Step {
    Step::Respond(status, Vec::new(), body.to_owned())
}

pub fn respond_with(status: u16, headers: &[(&'static str, &str)], body: &str) -> Step {
    Step::Respond(status, headers.iter().map(|(n, v)| (*n, (*v).to_owned())).collect(), body.to_owned())
}

pub fn problem(status: u16, code: &str) -> Step {
    respond(
        status,
        &format!(r#"{{"title":"t","detail":"why","status":{status},"code":"{code}","request_id":"req_1"}}"#),
    )
}

pub fn ok() -> Step {
    respond(200, r#"{"data":{"id":"scr_1","name":"Platform 1"},"meta":{}}"#)
}

#[derive(Clone, Default)]
pub struct Stub {
    script: Arc<Mutex<VecDeque<Step>>>,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
}

impl Stub {
    pub fn new(script: Vec<Step>) -> Self {
        let stub = Self::default();
        *stub.script.lock().unwrap() = script.into();
        stub
    }

    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl HttpClient for Stub {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        self.requests.lock().unwrap().push(request);
        let step = self.script.lock().unwrap().pop_front().expect("the stub ran out of responses");
        Box::pin(async move {
            match step {
                Step::Respond(status, headers, body) => {
                    let headers: Headers = headers.into_iter().collect();
                    Ok(HttpResponse::new(status, headers, body))
                }
                Step::Fail(error) => Err(error),
                Step::Hang => std::future::pending().await,
            }
        })
    }
}

pub fn stub_client(script: Vec<Step>) -> (Client, Stub) {
    stub_client_with(script, |b| b)
}

pub fn stub_client_with(
    script: Vec<Step>,
    configure: impl FnOnce(vojo::ClientBuilder) -> vojo::ClientBuilder,
) -> (Client, Stub) {
    let stub = Stub::new(script);
    let builder = Client::builder(key()).base_url("https://stub.example").http_client(stub.clone());
    (configure(builder).build().unwrap(), stub)
}

pub fn query_of(url: &str) -> &str {
    url.split_once('?').map(|(_, q)| q).unwrap_or("")
}
