//! The built-in HTTP client against a real local server: a redirect is never followed.

#![cfg(feature = "reqwest")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[tokio::test]
async fn a_redirected_post_is_not_followed() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut buffer = [0u8; 8192];
            let _ = stream.read(&mut buffer);
            counter.fetch_add(1, Ordering::SeqCst);
            let reply = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{address}/v1/elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });

    let client = vojo::Client::builder(["vojo_", "live_", "local"].concat())
        .base_url(format!("http://{address}"))
        .max_retries(0)
        .build()
        .unwrap();
    let error = client.sites().create(&Default::default()).await.unwrap_err();
    assert_eq!(error.status(), Some(302), "{error}");
    assert_eq!(seen.load(Ordering::SeqCst), 1);
}
