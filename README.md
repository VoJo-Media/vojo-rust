# Vojo Rust library

The official Rust client for the [Vojo](https://vojo-media.com) API: proof that your ads actually played on digital out-of-home screens.

> **Pre-1.0 and not yet published.** Expect changes before 1.0.

## Install

```toml
[dependencies]
vojo = { git = "https://github.com/VoJo-Media/vojo-rust" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Requires Rust 1.88 or newer and a Tokio runtime. TLS is rustls by default; use `default-features = false, features = ["native-tls"]` for the platform's own. The `blocking` feature adds a client for code that is not async.

## First call

An organisation owner creates keys in the Vojo portal. A live key starts with `vojo_live_` and comes from **Developers → API keys**. A test key starts with `vojo_test_` and comes from **Developers → Sandboxes**: it reaches only its own sandbox, with the same base URL and endpoints. Objects returned with a test key have `livemode` set to `false` (`screen.livemode`). See [Authentication](https://developers.vojo-media.com/getting-started/authentication) and [Test in a sandbox](https://developers.vojo-media.com/guides/test-in-a-sandbox) on https://developers.vojo-media.com.

```rust,no_run
use vojo::Client;

#[tokio::main]
async fn main() -> vojo::Result<()> {
    let client = Client::new("vojo_live_...")?; // or Client::from_env() to read VOJO_SECRET_KEY

    let mut screens = client.screens().list(Default::default());
    while let Some(screen) = screens.next().await {
        let screen = screen?;
        println!("{} {}", screen.id, screen.name);
    }
    Ok(())
}
```

`next` walks every item across pages, fetching as it goes.

## Common tasks

```rust,no_run
use vojo::models::{CreateSiteRequest, UpdateScreenRequest};
use vojo::params::{PlaysListParams, StatsPlaysRetrieveParams};
use vojo::RequestOptions;

# async fn run(client: vojo::Client) -> vojo::Result<()> {
// One object. It dereferences to the model, so screen.name works.
let screen = client.screens().retrieve("scr_123").await?;

// Create. A retry can never create it twice: every attempt carries one Idempotency-Key.
let site = CreateSiteRequest { name: "Kings Cross".into(), ..Default::default() };
client.sites().create(&site).await?;
client
    .with_options(RequestOptions::new().idempotency_key("my-own-key-123"))
    .sites()
    .create(&site)
    .await?;

// Update. None leaves a field unchanged; Some(None) sends null.
let update = UpdateScreenRequest {
    name: Some("Platform 1".into()),
    notes: Some(None),
    ..Default::default()
};
client.screens().update("scr_123", &update).await?;

// Sub-resources follow the path
client.webhook_endpoints().deliveries().retry("whe_123", "del_123").await?;

// One page at a time. limit is the page size, at most 100.
let page = client.plays().list(PlaysListParams::new().screen_id("scr_123").limit(100)).first_page().await?;
println!("{} {}", page.data.len(), page.has_more);

// Every item at once
let plays = client.plays().list(PlaysListParams::new().screen_id("scr_123")).collect().await?;

// Statistics keep their meta
let stats = client.stats().plays().retrieve(StatsPlaysRetrieveParams::new().range("7d")).await?;
println!("{:?} {}", *stats, stats.meta());
# Ok(())
# }
```

Some operations are not served yet during early access. Their docs say so, and calling one answers with a 404 whose message says the same.

## Errors

Every call returns `vojo::Result<T>`, whose error is `vojo::Error`.

```rust,no_run
use vojo::{ApiErrorKind, Error};

# async fn run(client: vojo::Client) {
match client.screens().retrieve("scr_missing").await {
    Ok(screen) => println!("{}", screen.name),
    Err(Error::Api(e)) if e.kind() == ApiErrorKind::NotFound => println!("no such screen"),
    Err(Error::Api(e)) if e.kind() == ApiErrorKind::RateLimit => println!("wait {:?}", e.retry_after()),
    Err(Error::Api(e)) => println!("{} {:?} {:?} {}", e.status, e.code, e.request_id, e.problem),
    Err(Error::Connection(e)) => println!("no response (timed out: {})", e.timeout),
    Err(other) => println!("{other}"),
}
# }
```

| `ApiErrorKind` | When |
| --- | --- |
| `Authentication` | 401 |
| `Permission` | 402 (plan limit: see `problem`) and 403 |
| `NotFound` | 404 |
| `Validation` | 400, 413, 422 (field reasons in `errors`) |
| `Conflict` | 409 |
| `RateLimit` | 429 (`retry_after()`) |
| `Server` | 5xx |
| `Other` | Any other status |

`Error::Pagination` means the API returned paging data that cannot be followed safely. `Error::InvalidArgument` is a mistake caught before sending, such as an empty id.

Codes can be added within v1, so always handle a code you have not seen.

## Webhooks

Pass the raw body, exactly as received. No API key is needed. This handler works with any web framework:

```rust
use vojo::webhook::{self, event_type, events};

/// Returns the HTTP status to answer with.
fn handle(body: &[u8], signature: Option<&str>, signing_secret: &str) -> u16 {
    let Ok(event) = webhook::construct_event(body, signature, signing_secret) else {
        return 400;
    };

    // Dedupe on event.id: it stays the same across retries.
    if event.event_type == event_type::VERIFICATION_SCORE_FINAL {
        let Ok(score) = event.decode::<events::VerificationScoreEvent>() else {
            return 400;
        };
        println!("{} {:?}", score.play_id, score.score.band);
    }
    204
}
# let body = br#"{"eventType":"verification.ping","eventId":"e","attemptId":"a"}"#;
# let header = webhook::generate_test_header(body, "vwsec_1", std::time::SystemTime::now());
# assert_eq!(handle(body, Some(&header), "vwsec_1"), 204);
# assert_eq!(handle(body, None, "vwsec_1"), 400);
```

Read `signature` from the `webhook::SIGNATURE_HEADER` header. While you roll a secret, hold both: `webhook::Verifier::new(new_secret).secret(old_secret).construct_event(body, signature)`.

`Error::WebhookVerification` gives the reason in `kind()`. To test your own handler, sign a payload with `webhook::generate_test_header(body, secret, SystemTime::now())`.

## Configuration

```rust,no_run
use std::time::Duration;
use vojo::{Client, RequestOptions};

# async fn run() -> vojo::Result<()> {
let client = Client::builder("vojo_live_...")
    .organisation("org_123") // OAuth access tokens must set this; secret keys do not need it
    .version("2026-10-07") // defaults to the version this release was built against
    .timeout(Duration::from_secs(30)) // per attempt, default 60s
    .max_retries(5) // default 2
    .build()?;

// The same settings for some calls only
let screen = client
    .with_options(RequestOptions::new().timeout(Duration::from_secs(5)))
    .screens()
    .retrieve("scr_123")
    .await?;
# Ok(())
# }
```

Failed requests are retried only when that is safe: connection failures, 429 and 5xx on operations that cannot apply twice. A `Retry-After` is honoured, up to 60 seconds; beyond that you get the error.

`ClientBuilder::http_client` takes your own `vojo::http::HttpClient`, for proxies, custom transports or tests.

## Response details

Results carry the HTTP details: `status`, `headers`, `request_id`, `meta` and `idempotent_replayed`.

```rust,no_run
# async fn run(client: vojo::Client) -> vojo::Result<()> {
let wrapper = client.wrappers().retrieve("wrp_123").await?;
println!("{:?} {}", wrapper.info().request_id, wrapper.meta());
# Ok(())
# }
```

## Without async

With the `blocking` feature:

```rust,ignore
let client = vojo::blocking::Client::new("vojo_live_...")?;
for screen in client.screens().list(Default::default()) {
    println!("{}", screen?.name);
}
```

Do not use it inside an async runtime: use `vojo::Client` there.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md).
