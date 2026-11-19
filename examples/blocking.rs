//! The quickstart without async.
//!
//!     VOJO_SECRET_KEY=vojo_live_... cargo run --example blocking --features blocking

use vojo::blocking::Client;
use vojo::params::ScreensListParams;

fn main() -> vojo::Result<()> {
    let mut builder = vojo::Client::builder(std::env::var("VOJO_SECRET_KEY").unwrap_or_default());
    if let Ok(url) = std::env::var("VOJO_BASE_URL") {
        builder = builder.base_url(url);
    }
    let client = Client::from_builder(builder)?;

    let page = client.screens().list(ScreensListParams::new().limit(10)).first_page()?;
    println!("{} screens on the first page (more: {})", page.data.len(), page.has_more);

    if let Some(first) = page.data.first() {
        let screen = client.screens().retrieve(&first.id)?;
        println!("{} {}", screen.id, screen.name);
    }
    Ok(())
}
