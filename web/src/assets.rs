//! Embedded static assets for the single-page frontend.

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "static/"]
struct Assets;

/// Returns the embedded file bytes and a content type for a known path.
pub fn file(path: &str) -> Option<(Vec<u8>, &'static str)> {
    let data = Assets::get(path)?;
    Some((data.data.to_vec(), mime(path)))
}

fn mime(path: &str) -> &'static str {
    match path {
        "index.html" => "text/html; charset=utf-8",
        "style.css" => "text/css; charset=utf-8",
        "app.js" => "text/javascript; charset=utf-8",
        _ => "application/octet-stream",
    }
}
