//! Cover art helpers shared by the front ends.
//!
//! A front end (the web UI today, the terminal UI later) can let the user
//! supply their own cover art instead of the one fetched from the Cover Art
//! Archive. [`sniff_mime`] recognises the format of raw image bytes so a
//! front end can reject data that is not a picture, and [`fetch_image`]
//! pulls the bytes of an image from an arbitrary URL the user provides.

use std::time::Duration;

use ureq::config::Config;
use ureq::{Agent, Error as HttpError};

use crate::lookup::Error;

/// How long a single image request may run before it is dropped.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Recognise the image format of `bytes` by its magic number.
///
/// Returns the MIME type when the bytes look like a picture a browser or
/// media player can display, and `None` otherwise.
pub fn sniff_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.starts_with(b"BM") {
        Some("image/bmp")
    } else {
        None
    }
}

/// Fetch the image at `url`, returning its raw bytes.
///
/// Unlike the Cover Art Archive lookup this is not rate limited: the user is
/// explicitly asking for a single image from a URL of their choosing. The
/// body is capped by ureq's default 10 MB limit.
pub fn fetch_image(url: &str) -> Result<Vec<u8>, Error> {
    let config = Config::builder()
        .user_agent(concat!("rend/", env!("CARGO_PKG_VERSION")))
        .timeout_global(Some(TIMEOUT))
        .build();
    let agent = Agent::new_with_config(config);
    match agent.get(url).call() {
        Ok(resp) => {
            let mut body = resp.into_body();
            let bytes = body
                .read_to_vec()
                .map_err(|e| Error::Network(e.to_string()))?;
            if bytes.is_empty() {
                Err(Error::NotFound)
            } else {
                Ok(bytes)
            }
        }
        Err(HttpError::StatusCode(404)) => Err(Error::NotFound),
        Err(HttpError::StatusCode(code)) => Err(Error::HttpStatus(code)),
        Err(e) => Err(Error::Network(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_recognises_known_formats() {
        assert_eq!(
            sniff_mime(&[0x89, b'P', b'N', b'G', 0, 0, 0, 0]),
            Some("image/png")
        );
        assert_eq!(sniff_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_mime(b"GIF89a"), Some("image/gif"));
        assert_eq!(sniff_mime(b"BMxxxx"), Some("image/bmp"));
    }

    #[test]
    fn sniff_rejects_non_images() {
        assert_eq!(sniff_mime(b"<html>"), None);
        assert_eq!(sniff_mime(b"RIIF\x00\x00"), None);
        assert_eq!(sniff_mime(b""), None);
        assert_eq!(sniff_mime(&[0xFF, 0xD8]), None);
    }
}
