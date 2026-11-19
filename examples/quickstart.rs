//! Lists screens and fetches the first one.
//!
//!     VOJO_SECRET_KEY=vojo_live_... cargo run --example quickstart
//!
//! Set VOJO_BASE_URL to run it against a mock of the API instead.

use vojo::params::ScreensListParams;
use vojo::Client;

#[tokio::main]
async fn main() -> vojo::Result<()> {
    let mut builder = Client::builder(std::env::var("VOJO_SECRET_KEY").unwrap_or_default());
    if let Ok(url) = std::env::var("VOJO_BASE_URL") {
        builder = builder.base_url(url);
    }
    let client = builder.build()?;

    let page = client.screens().list(ScreensListParams::new().limit(10)).first_page().await?;
    println!("{} screens on the first page (more: {})", page.data.len(), page.has_more);

    if let Some(first) = page.data.first() {
        let screen = client.screens().retrieve(&first.id).await?;
        println!("{} {} (request id: {:?})", screen.id, screen.name, screen.info().request_id);
    }
    Ok(())
}
