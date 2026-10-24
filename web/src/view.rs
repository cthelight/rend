//! Serializable views of server state, shaped for the frontend.

use std::sync::Arc;

use axum::response::sse::Event;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rend_core::Toc;
use rend_meta::DiscMeta;
use rend_rip::RipEvent;
use serde::Serialize;

/// A full snapshot of everything the frontend needs to render.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateView {
    pub out_dir: String,
    pub formats: Vec<FormatView>,
    pub force: bool,
    pub drives: Vec<DriveView>,
}

/// One output format the local ffmpeg installation can serve.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatView {
    pub id: String,
    pub label: String,
    pub selected: bool,
    pub selected_quality: Option<i32>,
    pub selected_quality_label: Option<String>,
    pub quality_options: Vec<QualityOptionView>,
}

/// One selectable quality value for an output format.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityOptionView {
    pub value: i32,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveView {
    pub index: usize,
    pub path: String,
    pub label: String,
    pub status: String,
    pub disc: Option<String>,
    pub demo: bool,
    pub loading_toc: bool,
    pub toc: Option<TocView>,
    pub mcn: Option<String>,
    /// URL of the MusicBrainz "add this disc" page, when a TOC is loaded.
    pub register_url: Option<String>,
    pub lookup_status: String,
    pub matches: Vec<MatchView>,
    pub match_sel: usize,
    pub meta: Option<MetaView>,
    /// Cover art as a base64 data URL, if fetched.
    pub cover: Option<Arc<String>>,
    pub cover_fetching: bool,
    /// True when the cover was supplied by the user instead of fetched from
    /// the Cover Art Archive.
    pub cover_custom: bool,
    pub rip: Option<RipView>,
    /// Track numbers that already have output files for the selected
    /// formats, so the frontend can offer an explicit re-rip.
    pub existing_tracks: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TocView {
    pub leadout_lba: u32,
    pub tracks: Vec<TrackView>,
}

impl TocView {
    pub fn from_toc(toc: &Toc) -> Self {
        let tracks = toc
            .tracks
            .iter()
            .map(|t| {
                let duration_secs = toc
                    .end_lba(t.number)
                    .map(|end| u64::from(end.saturating_sub(t.start_lba)) / 75);
                TrackView {
                    number: t.number,
                    kind: t.kind.to_string(),
                    start_lba: t.start_lba,
                    start_msf: t.start_msf().map(|m| m.to_string()),
                    duration_secs,
                }
            })
            .collect();
        Self {
            leadout_lba: toc.leadout_lba,
            tracks,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackView {
    pub number: u8,
    pub kind: String,
    pub start_lba: u32,
    pub start_msf: Option<String>,
    pub duration_secs: Option<u64>,
}

/// One MusicBrainz release candidate for the loaded disc.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchView {
    pub artist: String,
    pub album: String,
    pub album_artist: Option<String>,
    pub year: Option<String>,
    pub release_id: String,
    pub disc_position: Option<String>,
    pub max_diff_ms: u64,
    pub tracks: Vec<MatchTrackView>,
}

impl MatchView {
    pub fn from_candidate(c: &rend_meta::Candidate) -> Self {
        let m = &c.meta;
        Self {
            artist: m.artist.clone(),
            album: m.album.clone(),
            album_artist: m.album_artist.clone(),
            year: m.year.clone(),
            release_id: m.release_id.clone(),
            disc_position: m.disc_position(),
            max_diff_ms: c.max_diff_ms,
            tracks: m
                .tracks
                .iter()
                .map(|t| MatchTrackView {
                    title: t.title.clone(),
                    artist: t.artist.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchTrackView {
    pub title: String,
    pub artist: Option<String>,
}

/// The currently selected metadata, after any local edits.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaView {
    pub album: String,
    pub artist: String,
    pub album_artist: Option<String>,
    pub year: Option<String>,
    pub release_id: String,
    pub disc_position: Option<String>,
    pub tracks: Vec<MetaTrackView>,
}

impl MetaView {
    pub fn from_meta(meta: &DiscMeta) -> Self {
        Self {
            album: meta.album.clone(),
            artist: meta.artist.clone(),
            album_artist: meta.album_artist.clone(),
            year: meta.year.clone(),
            release_id: meta.release_id.clone(),
            disc_position: meta.disc_position(),
            tracks: meta
                .tracks
                .iter()
                .map(|t| MetaTrackView {
                    title: t.title.clone(),
                    artist: t.artist.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaTrackView {
    pub title: String,
    pub artist: Option<String>,
}

/// The per-track state of a running (or just finished) rip.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RipView {
    pub active: bool,
    pub tracks: Vec<u8>,
    pub out_dir: String,
    pub formats: Vec<String>,
    pub force: bool,
    pub states: Vec<TrackStateView>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackStateView {
    pub number: u8,
    /// "pending" | "ripping" | "done" | "skipped" | "failed"
    pub status: String,
    pub detail: Option<String>,
    pub done: u64,
    pub total: u64,
}

/// A rip worker event, as pushed to the frontend.
///
/// Variant tags and field names are camelCase, matching the rest of the
/// wire protocol (the `StateView` snapshot) and the frontend's expectations.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum RipEventView {
    TrackStarted {
        number: u8,
    },
    Progress {
        number: u8,
        #[serde(rename = "bytesDone")]
        bytes_done: u64,
        #[serde(rename = "bytesTotal")]
        bytes_total: u64,
    },
    TrackDone {
        number: u8,
        path: String,
        bytes: u64,
    },
    TrackFailed {
        number: u8,
        error: String,
    },
    TrackSkipped {
        number: u8,
        reason: String,
    },
    Finished {
        failed: usize,
        total: usize,
        stopped: bool,
    },
}

impl From<&RipEvent> for RipEventView {
    fn from(ev: &RipEvent) -> Self {
        match ev {
            RipEvent::TrackStarted { number } => Self::TrackStarted { number: *number },
            RipEvent::Progress {
                number,
                bytes_done,
                bytes_total,
            } => Self::Progress {
                number: *number,
                bytes_done: *bytes_done,
                bytes_total: *bytes_total,
            },
            RipEvent::TrackDone {
                number,
                path,
                bytes,
            } => Self::TrackDone {
                number: *number,
                path: path.to_string_lossy().into_owned(),
                bytes: *bytes,
            },
            RipEvent::TrackFailed { number, error } => Self::TrackFailed {
                number: *number,
                error: error.clone(),
            },
            RipEvent::TrackSkipped { number, reason, .. } => Self::TrackSkipped {
                number: *number,
                reason: reason.clone(),
            },
            RipEvent::Finished {
                failed,
                total,
                stopped,
            } => Self::Finished {
                failed: *failed,
                total: *total,
                stopped: *stopped,
            },
        }
    }
}

/// Events pushed over the SSE stream, tagged camelCase like the rest of
/// the wire protocol.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerEvent {
    /// The whole drive view changed (low frequency).
    DriveChanged { drive: Box<DriveView> },
    /// A per-track rip event (high frequency).
    Rip { drive: String, event: RipEventView },
    /// A line for the activity log.
    Log { msg: String },
    /// The client missed `skipped` events; refetch /api/state.
    Resync { skipped: u64 },
}

impl ServerEvent {
    /// Encode as an SSE event carrying the JSON payload.
    pub fn to_sse(&self) -> Event {
        let data = serde_json::to_string(self).expect("ServerEvent is serializable");
        Event::default().data(data)
    }
}

/// Encode cover art as a data URL the browser can display.
///
/// The bytes are normalised first (see [`rend_meta::normalize_art`]) so the
/// preview always matches what gets embedded in the files: small JPEG and
/// PNG art passes through unchanged, anything else is re-encoded as JPEG.
/// Browsers cannot display formats such as BMP or TIFF, so leaving them raw
/// would show a broken image even though the files are tagged correctly.
///
/// Returns `None` when the bytes do not decode, in which case the tag is
/// dropped too and the UI falls back to the placeholder.
pub fn cover_data_url(bytes: &[u8]) -> Option<String> {
    let art = rend_meta::normalize_art(bytes).ok()?;
    let mime = rend_meta::sniff_mime(&art)?;
    Some(format!("data:{mime};base64,{}", STANDARD.encode(&art)))
}

/// Decode user-supplied cover art into raw bytes.
///
/// Accepts either a `data:` URL (`data:image/png;base64,....`) or a bare
/// base64 string, as produced by the browser's file reader and clipboard.
/// Whitespace in the base64 payload is ignored.
pub fn decode_image_data(input: &str) -> Result<Vec<u8>, String> {
    let payload = input.trim();
    let b64 = match payload.find(',') {
        Some(comma) if payload.starts_with("data:") => &payload[comma + 1..],
        Some(_) => return Err("not a data URL or a base64 string".to_string()),
        None => payload,
    };
    let b64: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = STANDARD
        .decode(&b64)
        .map_err(|_| "invalid base64 in the cover data".to_string())?;
    if bytes.is_empty() {
        return Err("the cover data is empty".to_string());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid 1x1 RGB PNG.
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x09, 0x70, 0x48, 0x59, 0x73, 0x00, 0x00, 0x00, 0x01,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x4F, 0x25, 0xC4, 0xD6, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44,
        0x41, 0x54, 0x78, 0x9C, 0x63, 0x60, 0x60, 0x60, 0x00, 0x00, 0x00, 0x04, 0x00, 0x01, 0xF6,
        0x17, 0x38, 0x55, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    /// A valid 1x1 lossless WebP.
    const WEBP_1X1: &[u8] = &[
        0x52, 0x49, 0x46, 0x46, 0x1C, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38,
        0x4C, 0x0F, 0x00, 0x00, 0x00, 0x2F, 0x00, 0x00, 0x00, 0x00, 0x07, 0x10, 0xF5, 0x8F, 0xFE,
        0x07, 0x22, 0xA2, 0xFF, 0x01, 0x00,
    ];

    /// A valid 1x1 24-bit BMP.
    const BMP_1X1: &[u8] = &[
        0x42, 0x4D, 0x3A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x36, 0x00, 0x00, 0x00, 0x28,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x18, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x80, 0x01, 0x00,
    ];

    #[test]
    fn cover_preview_is_browser_renderable() {
        // Small JPEG/PNG art passes through unchanged, keeping its format.
        assert!(
            cover_data_url(PNG_1X1)
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        // Formats a browser cannot display are re-encoded to JPEG, the same
        // bytes that get embedded in the files.
        assert!(
            cover_data_url(WEBP_1X1)
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
        assert!(
            cover_data_url(BMP_1X1)
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
    }

    #[test]
    fn cover_preview_rejects_undecodable_bytes() {
        assert_eq!(cover_data_url(b"<html>"), None);
        assert_eq!(cover_data_url(b""), None);
        // Sniffable but truncated: no preview, matching the dropped tag.
        assert_eq!(cover_data_url(&[0x89, b'P', b'N', b'G', 0, 0, 0, 0]), None);
        assert_eq!(cover_data_url(&[0xFF, 0xD8, 0xFF, 0xE0]), None);
    }

    #[test]
    fn decode_accepts_data_urls_and_bare_base64() {
        let png = &[0x89, b'P', b'N', b'G', 0, 0, 0, 0];
        let b64 = STANDARD.encode(png);
        assert_eq!(decode_image_data(&b64), Ok(png.to_vec()));
        assert_eq!(
            decode_image_data(&format!("data:image/png;base64,{b64}")),
            Ok(png.to_vec())
        );
        // Whitespace in the payload is tolerated.
        let spaced =
            b64.chars()
                .collect::<Vec<_>>()
                .chunks(4)
                .fold(String::new(), |mut acc, chunk| {
                    acc.extend(chunk);
                    acc.push('\n');
                    acc
                });
        assert_eq!(decode_image_data(&spaced), Ok(png.to_vec()));
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode_image_data("not base64 !!!").is_err());
        assert!(decode_image_data("http://example.com/x.png").is_err());
        assert!(decode_image_data("").is_err());
    }

    #[test]
    fn server_event_serializes_tagged() {
        let ev = ServerEvent::Log {
            msg: "hello".into(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "log");
        assert_eq!(json["msg"], "hello");

        let ev = ServerEvent::Resync { skipped: 3 };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "resync");
        assert_eq!(json["skipped"], 3);

        // Multi-word tags must come out camelCase, exactly as the
        // frontend's event switch matches them.
        let ev = ServerEvent::DriveChanged {
            drive: Box::new(DriveView {
                index: 0,
                path: "/dev/sr0".into(),
                label: "sr0".into(),
                status: "ready".into(),
                disc: None,
                demo: false,
                loading_toc: false,
                toc: None,
                mcn: None,
                register_url: None,
                lookup_status: "idle".into(),
                matches: Vec::new(),
                match_sel: 0,
                meta: None,
                cover: None,
                cover_fetching: false,
                cover_custom: false,
                rip: None,
                existing_tracks: Vec::new(),
            }),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "driveChanged");
        assert_eq!(json["drive"]["path"], "/dev/sr0");

        let ev = ServerEvent::Rip {
            drive: "/dev/sr0".into(),
            event: RipEventView::Progress {
                number: 2,
                bytes_done: 1,
                bytes_total: 2,
            },
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "rip");
        assert_eq!(json["event"]["event"], "progress");
        assert_eq!(json["event"]["number"], 2);
        // The progress byte fields are camelCase on the wire too.
        assert_eq!(json["event"]["bytesDone"], 1);
        assert_eq!(json["event"]["bytesTotal"], 2);

        let json = serde_json::to_value(&RipEventView::TrackStarted { number: 1 }).unwrap();
        assert_eq!(json["event"], "trackStarted");
        let json = serde_json::to_value(&RipEventView::TrackDone {
            number: 1,
            path: "x".into(),
            bytes: 2,
        })
        .unwrap();
        assert_eq!(json["event"], "trackDone");
    }
}
