//! Looking up what a disc is, via MusicBrainz.
//!
//! The disc's CDDB id is submitted to MusicBrainz's disc endpoint, which
//! answers with the matching release — album, artist, year, track titles —
//! plus a release id that [`cover_art`] uses to fetch the front cover from
//! the Cover Art Archive.

use std::time::Duration;

use serde::Deserialize;
use ureq::config::Config;
use ureq::{Agent, Error as HttpError};

/// How long a single request may run before it is dropped.
const TIMEOUT: Duration = Duration::from_secs(15);
/// MusicBrainz's web service base URL.
const MUSICBRAINZ: &str = "https://musicbrainz.org";
/// The Cover Art Archive base URL.
const COVER_ART: &str = "https://coverartarchive.org";

/// Metadata for a disc, as looked up from MusicBrainz.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscMeta {
    /// Album title.
    pub album: String,
    /// The release's artist.
    pub artist: String,
    /// Release year, if known.
    pub year: Option<String>,
    /// MusicBrainz release id, for fetching cover art.
    pub release_id: String,
    /// Per-track metadata, in disc order (audio tracks only).
    pub tracks: Vec<TrackMeta>,
}

/// Metadata for a single track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackMeta {
    /// Track title.
    pub title: String,
    /// The track's artist, if it differs from the release artist.
    pub artist: Option<String>,
}

impl DiscMeta {
    /// The metadata of the track at the given 1-based position, if known.
    pub fn track(&self, position: usize) -> Option<&TrackMeta> {
        position.checked_sub(1).and_then(|i| self.tracks.get(i))
    }
}

/// Errors while looking a disc up.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No release matched the disc.
    #[error("no matching release found for the disc")]
    NotFound,
    /// The server answered with an unexpected status.
    #[error("the server answered with HTTP {0}")]
    HttpStatus(u16),
    /// The server's response was not valid JSON.
    #[error("the server's response was not valid JSON: {0}")]
    InvalidJson(String),
    /// The request could not be completed (no network, timeout, …).
    #[error("network error: {0}")]
    Network(String),
}

fn agent() -> Agent {
    let config = Config::builder()
        .user_agent(concat!("rend/", env!("CARGO_PKG_VERSION")))
        .timeout_global(Some(TIMEOUT))
        .build();
    Agent::new_with_config(config)
}

/// Looks up the disc with the given CDDB id on MusicBrainz.
pub fn lookup_disc(disc_id: &str) -> Result<DiscMeta, Error> {
    let url = format!("{MUSICBRAINZ}/ws/2/disc/{disc_id}?fmt=json");
    lookup_from(&url)
}

/// Fetches the front cover (250 px) for the given MusicBrainz release id.
///
/// Returns `None` when the release has no cover.
pub fn cover_art(release_id: &str) -> Result<Option<Vec<u8>>, Error> {
    let url = format!("{COVER_ART}/release/{release_id}/front-250");
    cover_art_from(&url)
}

fn cover_art_from(url: &str) -> Result<Option<Vec<u8>>, Error> {
    match agent().get(url).call() {
        Ok(resp) => {
            let mut body = resp.into_body();
            let bytes = body
                .read_to_vec()
                .map_err(|e| Error::Network(e.to_string()))?;
            if bytes.is_empty() {
                Ok(None)
            } else {
                Ok(Some(bytes))
            }
        }
        Err(HttpError::StatusCode(404)) => Ok(None),
        Err(e) => Err(network(e)),
    }
}

fn lookup_from(url: &str) -> Result<DiscMeta, Error> {
    match agent().get(url).call() {
        Ok(resp) => {
            let discs: MbDiscs = resp
                .into_body()
                .read_json()
                .map_err(|e| Error::InvalidJson(e.to_string()))?;
            parse_disc(discs)
        }
        Err(HttpError::StatusCode(404)) => Err(Error::NotFound),
        Err(HttpError::StatusCode(code)) => Err(Error::HttpStatus(code)),
        Err(e) => Err(network(e)),
    }
}

fn network(e: HttpError) -> Error {
    let detail = match &e {
        HttpError::HostNotFound => "host not found (offline?)".to_string(),
        HttpError::Timeout(_) => format!("timed out after {TIMEOUT:?}"),
        other => other.to_string(),
    };
    Error::Network(detail)
}

fn parse_disc(discs: MbDiscs) -> Result<DiscMeta, Error> {
    let Some(disc) = discs.discs.into_iter().next() else {
        return Err(Error::NotFound);
    };
    let Some(release) = disc.releases.into_iter().next() else {
        return Err(Error::NotFound);
    };
    let tracks = disc
        .tracks
        .into_iter()
        .map(|t| {
            let recording = t.recording;
            TrackMeta {
                title: recording
                    .as_ref()
                    .filter(|r| !r.title.is_empty())
                    .map(|r| r.title.clone())
                    .unwrap_or_else(|| "Unknown".into()),
                artist: recording.and_then(|r| r.artist.map(|a| a.name)),
            }
        })
        .collect();
    Ok(DiscMeta {
        album: release.title,
        artist: release.artist.map(|a| a.name).unwrap_or_default(),
        year: release.date.as_deref().and_then(year_of),
        release_id: release.id,
        tracks,
    })
}

/// The year of a MusicBrainz date ("1997", "1997-05", "1997-05-20", …).
fn year_of(date: &str) -> Option<String> {
    let year = date.split('-').next()?.trim();
    (!year.is_empty()).then(|| year.to_string())
}

#[derive(Deserialize)]
struct MbDiscs {
    #[serde(default)]
    discs: Vec<MbDisc>,
}

#[derive(Deserialize)]
struct MbDisc {
    #[serde(default)]
    tracks: Vec<MbTrack>,
    #[serde(default)]
    releases: Vec<MbRelease>,
}

#[derive(Deserialize)]
struct MbTrack {
    recording: Option<MbRecording>,
}

#[derive(Deserialize)]
struct MbRecording {
    title: String,
    artist: Option<MbName>,
}

#[derive(Deserialize)]
struct MbName {
    name: String,
}

#[derive(Deserialize)]
struct MbRelease {
    id: String,
    title: String,
    artist: Option<MbName>,
    date: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    const FIXTURE: &str = r#"
    {
      "discs": [
        {
          "tracks": [
            {
              "number": "1",
              "position": "1",
              "recording": {
                "id": "rec-1",
                "title": "First Song",
                "artist": { "id": "art-1", "name": "The Band" },
                "is-instrumental": false
              }
            },
            {
              "number": "2",
              "position": "2",
              "recording": { "id": "rec-2", "title": "Second Song" }
            },
            { "number": "3", "position": "3" }
          ],
          "releases": [
            {
              "id": "rel-1",
              "title": "The Album",
              "artist": { "id": "art-1", "name": "The Band" },
              "release-group": { "id": "rg-1", "title": "The Album" },
              "date": "1997-05-20",
              "country": "US",
              "length": 2480000,
              "medium-count": 1
            }
          ]
        }
      ]
    }
    "#;

    #[test]
    fn parses_a_disc_response() {
        let meta = parse_disc(serde_json::from_str(FIXTURE).unwrap()).unwrap();

        assert_eq!(meta.album, "The Album");
        assert_eq!(meta.artist, "The Band");
        assert_eq!(meta.year.as_deref(), Some("1997"));
        assert_eq!(meta.release_id, "rel-1");
        assert_eq!(meta.tracks.len(), 3);
        assert_eq!(meta.tracks[0].title, "First Song");
        assert_eq!(meta.tracks[0].artist.as_deref(), Some("The Band"));
        assert_eq!(meta.tracks[1].title, "Second Song");
        assert_eq!(meta.tracks[1].artist, None);
        // A track with no recording falls back to "Unknown".
        assert_eq!(meta.tracks[2].title, "Unknown");
    }

    #[test]
    fn parse_requires_a_release() {
        let body = r#"{"discs": [{"tracks": [], "releases": []}]}"#;
        assert!(matches!(
            parse_disc(serde_json::from_str(body).unwrap()),
            Err(Error::NotFound)
        ));
        let empty: MbDiscs = serde_json::from_str(r#"{}"#).unwrap();
        assert!(matches!(parse_disc(empty), Err(Error::NotFound)));
    }

    #[test]
    fn track_positions_are_one_based() {
        let meta = parse_disc(serde_json::from_str(FIXTURE).unwrap()).unwrap();
        assert_eq!(meta.track(1).unwrap().title, "First Song");
        assert_eq!(meta.track(3).unwrap().title, "Unknown");
        assert_eq!(meta.track(0), None);
        assert_eq!(meta.track(4), None);
    }

    #[test]
    fn year_of_handles_partial_dates() {
        assert_eq!(year_of("1997").as_deref(), Some("1997"));
        assert_eq!(year_of("1997-05").as_deref(), Some("1997"));
        assert_eq!(year_of("1997-05-20").as_deref(), Some("1997"));
        assert_eq!(year_of(""), None);
    }

    /// A throwaway HTTP server that answers one request with `status` and
    /// `body`, so the ureq path can be exercised without the network.
    fn serve_once(status: &str, body: &str) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
        let status = status.to_string();
        let body = body.to_string();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            // Drain the request until its header block ends.
            let mut buf = Vec::new();
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut tmp = [0u8; 1024];
                match sock.read(&mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                }
            }
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(response.as_bytes()).ok();
        });
        (addr, handle)
    }

    #[test]
    fn lookup_roundtrips_over_http() {
        let (addr, handle) = serve_once("200 OK", FIXTURE);
        let meta = lookup_from(&format!("http://{addr}/lookup")).unwrap();
        handle.join().unwrap();

        assert_eq!(meta.album, "The Album");
        assert_eq!(meta.tracks.len(), 3);
    }

    #[test]
    fn lookup_404_is_not_found() {
        let (addr, handle) = serve_once("404 Not Found", "");
        let err = lookup_from(&format!("http://{addr}/lookup")).unwrap_err();
        handle.join().unwrap();

        assert!(matches!(err, Error::NotFound));
    }

    #[test]
    fn cover_404_is_no_cover() {
        let (addr, handle) = serve_once("404 Not Found", "");
        let url = format!("http://{addr}/cover");
        let art = crate::lookup::cover_art_from(&url).unwrap();
        handle.join().unwrap();

        assert!(art.is_none());
    }
}
