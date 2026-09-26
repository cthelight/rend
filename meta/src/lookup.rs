//! Looking up what a disc is, via MusicBrainz.
//!
//! The disc's layout is submitted to MusicBrainz's disc endpoint in two
//! steps. First the exact disc id of the TOC is looked up — and, since
//! some TOC readers report track addresses relative to the disc start
//! rather than to index 0, the id of the same TOC shifted by the
//! 150-sector lead-in as well. If no exact registration exists, the
//! disc's track durations are matched against candidate releases,
//! keeping the one whose medium's durations come closest.
//! [`lookup_disc`] returns that best match; [`lookup_disc_all`] lists
//! every candidate, best first, so a worse match can be chosen
//! deliberately, and [`lookup_candidates`] exposes the same candidates
//! with how far off each one is. The matched release yields album,
//! artist, year, and
//! track titles, plus a release id that [`cover_art`] uses to fetch the
//! front cover from the Cover Art Archive. Every outgoing request is
//! spaced out by a process-wide [`Throttle`] to stay under
//! MusicBrainz's one-request-per-second limit, and a request the server
//! throttles (HTTP 503 or 429) is retried after a short backoff.

use std::sync::LazyLock;
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use ureq::config::Config;
use ureq::{Agent, Error as HttpError};

use crate::discid::mb_discid;
use crate::throttle::Throttle;

/// How long a single request may run before it is dropped.
const TIMEOUT: Duration = Duration::from_secs(15);
/// MusicBrainz's web service base URL.
const MUSICBRAINZ: &str = "https://musicbrainz.org";
/// The Cover Art Archive base URL.
const COVER_ART: &str = "https://coverartarchive.org";
/// Lead-in sectors between the disc start and the first track.
const DEBIAS: u32 = 150;
/// A fuzzy candidate is accepted only if every track's duration is within
/// this many milliseconds of the disc's.
const FUZZY_TOLERANCE_MS: u64 = 5_000;
/// MusicBrainz allows at most one request per second per IP address and
/// answers faster bursts with a 503, so outgoing requests are spaced a
/// little further apart than that.
const MIN_REQUEST_GAP: Duration = Duration::from_millis(1_100);
/// A request the server throttles (HTTP 503 or 429) is retried up to this
/// many times in total, backing off by this long after each refusal.
const MAX_ATTEMPTS: u32 = 3;
const RETRY_BACKOFF: Duration = Duration::from_secs(1);

/// The disc's track layout, in LBA units.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiscToc {
    /// Start LBA of each audio track, in disc order.
    pub offsets: Vec<u32>,
    /// Leadout LSN.
    pub leadout: u32,
}

/// Metadata for a disc, as looked up from MusicBrainz.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscMeta {
    /// Album title.
    pub album: String,
    /// The release's artist.
    pub artist: String,
    /// The album artist, when it differs from the release artist (e.g. a
    /// compilation).
    pub album_artist: Option<String>,
    /// Release year, if known.
    pub year: Option<String>,
    /// MusicBrainz release id, for fetching cover art. Empty for metadata
    /// that was entered by hand instead of looked up.
    pub release_id: String,
    /// MusicBrainz id of the release artist, when looked up.
    pub release_artist_id: Option<String>,
    /// 1-based position of this disc within the release, when the release
    /// has more than one disc.
    pub disc_number: Option<u32>,
    /// Total discs in the release, when the release has more than one disc.
    pub disc_count: Option<u32>,
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
    /// MusicBrainz id of the track's artist, when known.
    pub artist_id: Option<String>,
    /// MusicBrainz recording id, when known.
    pub recording_id: Option<String>,
    /// MusicBrainz release-track id, when known.
    pub release_track_id: Option<String>,
}

impl DiscMeta {
    /// The metadata of the track at the given 1-based position, if known.
    pub fn track(&self, position: usize) -> Option<&TrackMeta> {
        position.checked_sub(1).and_then(|i| self.tracks.get(i))
    }

    /// This disc's position within the release, as "n/total" (or the bare
    /// "n" when the total is unknown). `None` for a single-disc release.
    pub fn disc_position(&self) -> Option<String> {
        let number = self.disc_number?;
        match self.disc_count {
            Some(total) => Some(format!("{number}/{total}")),
            None => Some(number.to_string()),
        }
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

/// The process-wide request gate: every outgoing request reserves a slot
/// here first, so no thread can outrun MusicBrainz's rate limit on its
/// own.
static THROTTLE: LazyLock<Throttle> = LazyLock::new(|| Throttle::new(MIN_REQUEST_GAP));

/// A candidate match and how far off its worst track's duration is, in
/// milliseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub meta: DiscMeta,
    pub max_diff_ms: u64,
}

impl Candidate {
    /// Whether the match is close enough to trust without asking.
    pub fn accepted(&self) -> bool {
        self.max_diff_ms <= FUZZY_TOLERANCE_MS
    }
}

/// Looks up the disc with layout `toc` on MusicBrainz.
///
/// An exact disc-id hit is returned as-is. Otherwise the disc's track
/// durations are matched against candidate releases and the best match is
/// accepted only if every track's duration is within
/// [`FUZZY_TOLERANCE_MS`] of the disc's; a closer-but-distant release is
/// no match, and [`lookup_disc_all`] can be used to see it anyway.
pub fn lookup_disc(toc: &DiscToc) -> Result<DiscMeta, Error> {
    lookup_candidates(toc)?
        .into_iter()
        .find(|c| c.accepted())
        .map(|c| c.meta)
        .ok_or(Error::NotFound)
}

/// Looks up the disc with layout `toc` and returns every candidate match,
/// best first.
///
/// An exact disc-id hit yields a single candidate. Otherwise the disc's
/// track durations are matched against candidate releases; every release
/// whose medium has the right track count and known durations is listed,
/// ranked by the worst per-track duration difference. No tolerance
/// applies — a distant release is still a candidate.
pub fn lookup_disc_all(toc: &DiscToc) -> Result<Vec<DiscMeta>, Error> {
    Ok(lookup_candidates(toc)?
        .into_iter()
        .map(|c| c.meta)
        .collect())
}

/// Every candidate match for the disc with layout `toc`, best first.
///
/// An exact disc-id hit yields a single candidate with a zero difference.
/// Otherwise the disc's track durations are matched against candidate
/// releases; every release whose medium has the right track count and
/// known durations is listed, ranked by the worst per-track duration
/// difference. No tolerance applies — a distant release is still a
/// candidate.
pub fn lookup_candidates(toc: &DiscToc) -> Result<Vec<Candidate>, Error> {
    candidates(toc, &THROTTLE)
}

/// Finds every candidate for the disc: first an exact disc-id hit (the
/// TOC as reported, then shifted by the lead-in, since registered ids
/// were computed from either), else a duration-ranked list.
fn candidates(toc: &DiscToc, throttle: &Throttle) -> Result<Vec<Candidate>, Error> {
    for &debias in [0u32, DEBIAS].iter() {
        let offsets: Vec<u32> = toc.offsets.iter().map(|&lba| lba + debias).collect();
        let id = mb_discid(toc.leadout + debias, &offsets);
        match lookup_cdtoc(&id, throttle) {
            Ok(Some(meta)) => {
                return Ok(vec![Candidate {
                    meta,
                    max_diff_ms: 0,
                }]);
            }
            Ok(None) | Err(Error::NotFound) => {}
            Err(e) => return Err(e),
        }
    }
    let releases = lookup_release_list(&toc_param(toc), throttle)?;
    Ok(rank_candidates(&releases.releases, toc).unwrap_or_default())
}

/// The MusicBrainz web page for registering the disc's layout as a disc id.
pub fn register_disc_id_url(toc: &DiscToc) -> String {
    format!("{MUSICBRAINZ}/cdtoc/attach?toc={}", toc_param(toc))
}

/// Looks up the disc with the given MusicBrainz disc id.
///
/// Returns `Ok(None)` when the id is not registered.
fn lookup_cdtoc(id: &str, throttle: &Throttle) -> Result<Option<DiscMeta>, Error> {
    let url = format!("{MUSICBRAINZ}/ws/2/discid/{id}?fmt=json&inc=artists+recordings&cdstubs=no");
    cdtoc_from(&url, id, throttle)
}

fn cdtoc_from(url: &str, id: &str, throttle: &Throttle) -> Result<Option<DiscMeta>, Error> {
    // A 404 means the id is not registered: no match, not an error.
    let cdtoc: MbCdtoc = match get_json(url, throttle) {
        Ok(cdtoc) => cdtoc,
        Err(Error::NotFound) => return Ok(None),
        Err(e) => return Err(e),
    };
    let Some((release, medium)) = pick_release_and_medium(&cdtoc.releases, id) else {
        return Ok(None);
    };
    Ok(Some(to_disc_meta(release, medium)))
}

/// Selects the release to describe from an exact-lookup response: the one
/// whose medium carries the disc id, else the first release (whose first
/// medium is used). `None` when there is no release at all.
fn pick_release_and_medium<'a>(
    releases: &'a [MbRelease],
    id: &str,
) -> Option<(&'a MbRelease, Option<&'a MbMedium>)> {
    releases
        .iter()
        .find_map(|release| matched_medium(release, id))
        .map(|(release, medium)| (release, Some(medium)))
        .or_else(|| {
            releases
                .first()
                .map(|release| (release, release.media.first()))
        })
}

/// Asks MusicBrainz for releases whose track durations resemble the TOC,
/// encoded as `1+{last track}+{leadout}+{offsets…}`, `+`-separated.
fn lookup_release_list(toc_param: &str, throttle: &Throttle) -> Result<MbReleaseList, Error> {
    let url = format!(
        "{MUSICBRAINZ}/ws/2/discid/-?toc={toc_param}&fmt=json&inc=artists+recordings&limit=25"
    );
    get_json(&url, throttle)
}

fn toc_param(toc: &DiscToc) -> String {
    let mut param = format!("1+{}", toc.offsets.len());
    param.push_str(&format!("+{}", toc.leadout));
    for &lba in &toc.offsets {
        param.push_str(&format!("+{lba}"));
    }
    param
}

/// The disc's track durations in ms, from consecutive track starts and
/// the leadout.
fn durations_ms(toc: &DiscToc) -> Vec<u64> {
    let mut ends: Vec<u32> = toc.offsets.to_vec();
    ends.push(toc.leadout);
    ends.windows(2)
        .map(|w| u64::from(w[1].saturating_sub(w[0])) * 1000 / 75)
        .collect()
}

/// Ranks every candidate release for the disc, best first.
///
/// Each release is scored by its best medium: the one whose track count
/// equals the disc's and whose tracks all have a known duration, with the
/// lowest worst per-track difference. Releases without such a medium are
/// dropped. The ranking is stable, so a tie keeps MusicBrainz's order.
fn rank_candidates(releases: &[MbRelease], toc: &DiscToc) -> Option<Vec<Candidate>> {
    let target = durations_ms(toc);
    let mut ranked: Vec<Candidate> = Vec::new();
    for release in releases {
        let mut best: Option<(u64, &MbMedium)> = None;
        for medium in &release.media {
            if let Some(diff) = score_medium(&medium.tracks, &target)
                && best.is_none_or(|(so_far, _)| diff < so_far)
            {
                best = Some((diff, medium));
            }
        }
        if let Some((max_diff_ms, medium)) = best {
            ranked.push(Candidate {
                meta: to_disc_meta(release, Some(medium)),
                max_diff_ms,
            });
        }
    }
    (!ranked.is_empty()).then(|| {
        ranked.sort_by_key(|c| c.max_diff_ms);
        ranked
    })
}

/// How far off the worst track's duration is, if `tracks` is a complete
/// scoring for `target` (same count, every duration known).
fn score_medium(tracks: &[MbTrack], target: &[u64]) -> Option<u64> {
    if tracks.len() != target.len() {
        return None;
    }
    let mut max_diff = 0u64;
    for (track, &expected) in tracks.iter().zip(target) {
        let actual = track.length.or_else(|| {
            track
                .recording
                .as_ref()
                .and_then(|recording| recording.length)
        })?;
        max_diff = max_diff.max(actual.abs_diff(expected));
    }
    Some(max_diff)
}

fn matched_medium<'a>(release: &'a MbRelease, id: &str) -> Option<(&'a MbRelease, &'a MbMedium)> {
    release
        .media
        .iter()
        .find(|medium| medium.discs.iter().any(|disc| disc.id == id))
        .map(|medium| (release, medium))
}

fn to_disc_meta(release: &MbRelease, medium: Option<&MbMedium>) -> DiscMeta {
    let release_artist = credit_names(&release.artist_credit);
    let release_artist_id = credit_artist_id(&release.artist_credit);
    let tracks = medium
        .map(|m| {
            m.tracks
                .iter()
                .map(|track| TrackMeta {
                    title: first_nonempty(&track.title, recording_title(&track.recording))
                        .map(str::to_string)
                        .unwrap_or_else(|| "Unknown".into()),
                    artist: track_artist(track, &release_artist),
                    artist_id: credit_artist_id(&track.artist_credit),
                    recording_id: track.recording.as_ref().and_then(|r| r.id.clone()),
                    release_track_id: track.id.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    // The discid responses carry no medium count of their own, but the
    // server always includes every medium of the release, so the media
    // array is the count.
    let multi = release.media.len() > 1;
    let disc_number = if multi {
        medium.and_then(|m| (m.position > 0).then_some(m.position))
    } else {
        None
    };
    DiscMeta {
        album: release.title.clone(),
        artist: release_artist,
        album_artist: None,
        year: release.date.as_deref().and_then(year_of),
        release_id: release.id.clone(),
        release_artist_id,
        disc_number,
        disc_count: multi.then_some(release.media.len() as u32),
        tracks,
    }
}

/// The MusicBrainz id of the first artist in a credit, when one carries it.
fn credit_artist_id(credit: &[MbCredit]) -> Option<String> {
    credit
        .iter()
        .find_map(|c| c.artist.as_ref().and_then(|a| a.id.clone()))
}

fn track_artist(track: &MbTrack, release_artist: &str) -> Option<String> {
    let credit = credit_names(&track.artist_credit);
    (!credit.is_empty() && credit != release_artist).then_some(credit)
}

/// The display name of an artist credit: each entry's name followed by its
/// joinphrase, the connector that follows the entry — "A" with joinphrase
/// " feat. " followed by "B" renders "A feat. B".
fn credit_names(credit: &[MbCredit]) -> String {
    credit
        .iter()
        .map(|c| format!("{}{}", c.name, c.joinphrase))
        .collect()
}

fn recording_title(recording: &Option<MbRecording>) -> &str {
    recording.as_ref().map_or("", |recording| &recording.title)
}

fn first_nonempty<'a>(primary: &'a str, fallback: &'a str) -> Option<&'a str> {
    if !primary.is_empty() {
        Some(primary)
    } else {
        (!fallback.is_empty()).then_some(fallback)
    }
}

/// The year of a MusicBrainz date ("1997", "1997-05", "1997-05-20", …).
fn year_of(date: &str) -> Option<String> {
    let year = date.split('-').next()?.trim();
    (!year.is_empty()).then(|| year.to_string())
}

/// Fetches the front cover (250 px) for the given MusicBrainz release id.
///
/// Returns `None` when the release has no cover.
pub fn cover_art(release_id: &str) -> Result<Option<Vec<u8>>, Error> {
    let url = format!("{COVER_ART}/release/{release_id}/front-250");
    cover_art_from(&url, &THROTTLE)
}

fn cover_art_from(url: &str, throttle: &Throttle) -> Result<Option<Vec<u8>>, Error> {
    with_retry(|| request_cover(url, throttle))
}

fn request_cover(url: &str, throttle: &Throttle) -> Result<Option<Vec<u8>>, Error> {
    throttle.wait();
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
        Err(HttpError::StatusCode(code)) => Err(Error::HttpStatus(code)),
        Err(e) => Err(network(e)),
    }
}

fn get_json<T: DeserializeOwned>(url: &str, throttle: &Throttle) -> Result<T, Error> {
    with_retry(|| request_json(url, throttle))
}

fn request_json<T: DeserializeOwned>(url: &str, throttle: &Throttle) -> Result<T, Error> {
    throttle.wait();
    match agent().get(url).call() {
        Ok(resp) => resp
            .into_body()
            .read_json()
            .map_err(|e| Error::InvalidJson(e.to_string())),
        Err(HttpError::StatusCode(404)) => Err(Error::NotFound),
        Err(HttpError::StatusCode(code)) => Err(Error::HttpStatus(code)),
        Err(e) => Err(network(e)),
    }
}

/// Runs `request`, retrying a throttled response (HTTP 503 or 429) after
/// a short backoff, up to [`MAX_ATTEMPTS`] attempts in total.
fn with_retry<T>(mut request: impl FnMut() -> Result<T, Error>) -> Result<T, Error> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match request() {
            Ok(value) => return Ok(value),
            Err(Error::HttpStatus(code))
                if (code == 429 || code == 503) && attempt < MAX_ATTEMPTS =>
            {
                thread::sleep(RETRY_BACKOFF * attempt);
            }
            Err(e) => return Err(e),
        }
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

#[derive(Deserialize)]
struct MbCdtoc {
    #[serde(default)]
    releases: Vec<MbRelease>,
}

#[derive(Deserialize)]
struct MbReleaseList {
    #[serde(default)]
    releases: Vec<MbRelease>,
}

#[derive(Deserialize)]
struct MbRelease {
    id: String,
    title: String,
    #[serde(default, rename = "artist-credit")]
    artist_credit: Vec<MbCredit>,
    date: Option<String>,
    #[serde(default)]
    media: Vec<MbMedium>,
}

#[derive(Deserialize)]
struct MbCredit {
    name: String,
    #[serde(default)]
    joinphrase: String,
    #[serde(default)]
    artist: Option<MbArtist>,
}

#[derive(Deserialize)]
struct MbArtist {
    #[serde(default)]
    id: Option<String>,
}

#[derive(Deserialize)]
struct MbMedium {
    /// 1-based position within the release; 0 when MusicBrainz omits it.
    #[serde(default)]
    position: u32,
    #[serde(default)]
    tracks: Vec<MbTrack>,
    #[serde(default)]
    discs: Vec<MbDisc>,
}

#[derive(Deserialize)]
struct MbDisc {
    id: String,
}

#[derive(Deserialize)]
struct MbTrack {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: String,
    length: Option<u64>,
    #[serde(default, rename = "artist-credit")]
    artist_credit: Vec<MbCredit>,
    recording: Option<MbRecording>,
}

#[derive(Deserialize)]
struct MbRecording {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: String,
    length: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// The exact-lookup response for a three-track disc.
    const EXACT: &str = r#"
    {
      "id": "disc-1",
      "offset-count": 3,
      "sectors": 3300,
      "offsets": [150, 1650, 2450],
      "releases": [
        {
          "id": "rel-1",
          "title": "The Album",
          "artist-credit": [
            { "name": "The Band", "joinphrase": " feat. ", "artist": { "id": "ar-1" } },
            { "name": "The Choir", "joinphrase": "" }
          ],
          "date": "1997-05-20",
          "media": [
            {
              "position": 1,
              "format": "CD",
              "discs": [ { "id": "disc-1" } ],
              "tracks": [
                {
                  "position": 1,
                  "id": "rt-1",
                  "title": "First Song",
                  "length": 20100,
                  "artist-credit": [ { "name": "Guest Star", "artist": { "id": "ar-2" } } ],
                  "recording": { "id": "rec-1", "title": "First Song" }
                },
                {
                  "position": 2,
                  "id": "rt-2",
                  "title": "",
                  "length": 10600,
                  "recording": { "id": "rec-2", "title": "Second Song" }
                },
                { "position": 3, "length": 11400 }
              ]
            }
          ]
        }
      ]
    }
    "#;

    /// The exact-lookup response for the first disc of a two-disc release.
    const MULTI_DISC: &str = r#"
    {
      "id": "disc-1",
      "offset-count": 3,
      "sectors": 3300,
      "offsets": [150, 1650, 2450],
      "releases": [
        {
          "id": "rel-1",
          "title": "The Album",
          "artist-credit": [ { "name": "The Band" } ],
          "date": "1997-05-20",
          "media": [
            {
              "position": 1,
              "format": "CD",
              "discs": [ { "id": "disc-1" } ],
              "tracks": [
                { "position": 1, "title": "First Song", "length": 20100 },
                { "position": 2, "title": "Second Song", "length": 10600 },
                { "position": 3, "title": "Third Song", "length": 11400 }
              ]
            },
            {
              "position": 2,
              "format": "CD",
              "discs": [ ],
              "tracks": [
                { "position": 1, "title": "Bonus", "length": 30000 }
              ]
            }
          ]
        }
      ]
    }
    "#;

    /// The fuzzy-lookup response: one release whose durations are far off,
    /// one that matches the test TOC within tolerance.
    const FUZZY: &str = r#"
    {
      "release-count": 2,
      "releases": [
        {
          "id": "rel-wrong",
          "title": "Wrong Album",
          "artist-credit": [ { "name": "Other Band" } ],
          "media": [
            {
              "tracks": [
                { "title": "A", "length": 200000 },
                { "title": "B", "length": 12000 },
                { "title": "C", "length": 12000 }
              ]
            }
          ]
        },
        {
          "id": "rel-right",
          "title": "Right Album",
          "artist-credit": [ { "name": "The Band" } ],
          "date": "1998",
          "media": [
            {
              "tracks": [
                { "title": "A", "length": 20100 },
                { "title": "B", "length": 10600 },
                { "title": "C", "length": 11400 }
              ]
            }
          ]
        }
      ]
    }
    "#;

    /// A live discid response, trimmed: every key the server actually
    /// emits, which carries no medium count at all.
    const LIVE: &str = r#"
    {
      "id": "zBKu_qLsePCfpQjFYsu0ExWyB2E-",
      "offset-count": 8,
      "sectors": 186226,
      "offsets": [150, 35807, 44077, 64415, 90213, 95123, 124348, 141390],
      "releases": [
        {
          "id": "e0f3b9a5-9eca-4fee-9d15-55f42f6b2636",
          "title": "Eph Reissue",
          "artist-credit": [
            {
              "name": "Fridge",
              "joinphrase": "",
              "artist": {
                "id": "601b575c-9ef8-45a7-81df-3bc48216ca5f",
                "name": "Fridge",
                "sort-name": "Fridge",
                "type": "Group",
                "type-id": "e431f5f6-b5d2-343d-8b36-72607fffb74b",
                "country": "GB",
                "disambiguation": "UK post rock band"
              }
            }
          ],
          "asin": "B00000JOIY",
          "barcode": "656605304823",
          "country": "US",
          "date": "2002-04",
          "disambiguation": "",
          "packaging": "Jewel Case",
          "packaging-id": "ec27701a-4a22-37f4-bfac-6616e0f9750a",
          "quality": "normal",
          "status": "Official",
          "status-id": "4e304316-386d-3409-af2e-78857eec5cfe",
          "text-representation": { "language": "eng", "script": "Latn" },
          "release-events": [
            {
              "date": "2002-04",
              "area": {
                "id": "489ce91b-6658-3307-9877-795b68554c98",
                "name": "United States",
                "iso-3166-1-codes": ["US"]
              }
            }
          ],
          "cover-art-archive": {
            "artwork": true,
            "darkened": false,
            "front": true,
            "back": false,
            "count": 1
          },
          "media": [
            {
              "id": "0b470036-997f-307c-9de3-0f300597f5d4",
              "position": 1,
              "title": "Eph",
              "format": "CD",
              "format-id": "9712d52a-4509-3d4b-a1a2-67c88c643e31",
              "track-count": 8,
              "track-offset": 0,
              "discs": [
                {
                  "id": "zBKu_qLsePCfpQjFYsu0ExWyB2E-",
                  "offset-count": 8,
                  "offsets": [150, 35807, 44077, 64415, 90213, 95123, 124348, 141390],
                  "sectors": 186226
                }
              ],
              "tracks": [
                {
                  "position": 1,
                  "id": "932ae747-c913-3ad6-b68e-beb488ba07be",
                  "title": "Ark",
                  "length": 475426,
                  "recording": {
                    "id": "7645f053-a7d5-49e2-97b2-95c846875736",
                    "title": "Ark",
                    "length": 475426,
                    "first-release-date": "1999-04",
                    "disambiguation": "",
                    "video": false
                  }
                }
              ]
            },
            {
              "id": "538511c8-09eb-3928-a281-faadb2b52ae0",
              "position": 2,
              "title": "Kinoshita Terasaka, Of EP and Remixes",
              "format": "CD",
              "format-id": "9712d52a-4509-3d4b-a1a2-67c88c643e31",
              "track-count": 8,
              "track-offset": 0,
              "discs": [
                {
                  "id": "1xOiBOfHFVJmwmdGpP.gT1DlrAo-",
                  "offset-count": 8,
                  "offsets": [150, 22852, 71281, 94942, 134071, 169673, 208917, 239542],
                  "sectors": 262615
                }
              ],
              "tracks": [
                {
                  "position": 1,
                  "number": "1",
                  "id": "rt-k1",
                  "title": "Kinoshita"
                }
              ]
            }
          ]
        }
      ]
    }
    "#;

    fn test_toc() -> DiscToc {
        // Durations: (1650-150), (2450-1650), (3300-2450) frames
        // = 20000, 10666, 11333 ms.
        DiscToc {
            offsets: vec![150, 1650, 2450],
            leadout: 3300,
        }
    }

    #[test]
    fn cdtoc_parses_the_matched_release() {
        let cdtoc: MbCdtoc = serde_json::from_str(EXACT).unwrap();
        let (release, medium) = matched_medium(&cdtoc.releases[0], "disc-1").unwrap();
        let meta = to_disc_meta(release, Some(medium));

        assert_eq!(meta.album, "The Album");
        assert_eq!(meta.artist, "The Band feat. The Choir");
        assert_eq!(meta.year.as_deref(), Some("1997"));
        assert_eq!(meta.release_id, "rel-1");
        // A single-disc release carries no disc position.
        assert_eq!(meta.disc_number, None);
        assert_eq!(meta.disc_count, None);
        assert_eq!(meta.tracks.len(), 3);
        assert_eq!(meta.tracks[0].title, "First Song");
        assert_eq!(meta.tracks[0].artist.as_deref(), Some("Guest Star"));
        // A track without a title falls back to its recording's.
        assert_eq!(meta.tracks[1].title, "Second Song");
        assert_eq!(meta.tracks[1].artist, None);
        // A bare track falls back to "Unknown".
        assert_eq!(meta.tracks[2].title, "Unknown");
        assert_eq!(meta.tracks[2].artist, None);
    }

    #[test]
    fn cdtoc_parses_the_musicbrainz_ids() {
        let cdtoc: MbCdtoc = serde_json::from_str(EXACT).unwrap();
        let (release, medium) = matched_medium(&cdtoc.releases[0], "disc-1").unwrap();
        let meta = to_disc_meta(release, Some(medium));

        // The release artist id comes from the first credit that carries one.
        assert_eq!(meta.release_artist_id.as_deref(), Some("ar-1"));
        assert_eq!(meta.release_id, "rel-1");
        // A credited track carries its own artist and the recording ids.
        assert_eq!(meta.tracks[0].artist_id.as_deref(), Some("ar-2"));
        assert_eq!(meta.tracks[0].recording_id.as_deref(), Some("rec-1"));
        assert_eq!(meta.tracks[0].release_track_id.as_deref(), Some("rt-1"));
        // An uncredited track keeps the recording id but no artist id.
        assert_eq!(meta.tracks[1].artist_id, None);
        assert_eq!(meta.tracks[1].recording_id.as_deref(), Some("rec-2"));
        assert_eq!(meta.tracks[1].release_track_id.as_deref(), Some("rt-2"));
        // A bare track has no ids at all.
        assert_eq!(meta.tracks[2].artist_id, None);
        assert_eq!(meta.tracks[2].recording_id, None);
        assert_eq!(meta.tracks[2].release_track_id, None);
    }

    #[test]
    fn cdtoc_parses_disc_position_for_multi_disc() {
        let cdtoc: MbCdtoc = serde_json::from_str(MULTI_DISC).unwrap();
        let (release, medium) = matched_medium(&cdtoc.releases[0], "disc-1").unwrap();
        let meta = to_disc_meta(release, Some(medium));
        assert_eq!(meta.disc_number, Some(1));
        assert_eq!(meta.disc_count, Some(2));
        assert_eq!(meta.disc_position().as_deref(), Some("1/2"));
    }

    /// The server never sends a medium count, so a real response must still
    /// yield the disc position from the media it does carry.
    #[test]
    fn the_live_response_shape_yields_the_disc_position() {
        let cdtoc: MbCdtoc = serde_json::from_str(LIVE).unwrap();
        let (release, medium) =
            pick_release_and_medium(&cdtoc.releases, "zBKu_qLsePCfpQjFYsu0ExWyB2E-").unwrap();
        let meta = to_disc_meta(release, medium);
        assert_eq!(meta.disc_number, Some(1));
        assert_eq!(meta.disc_count, Some(2));
        assert_eq!(meta.disc_position().as_deref(), Some("1/2"));
        assert_eq!(meta.tracks[0].title, "Ark");
    }

    #[test]
    fn disc_position_labels_multi_disc_releases() {
        let meta = DiscMeta {
            album: "The Album".into(),
            artist: "The Band".into(),
            album_artist: None,
            year: None,
            release_id: "rel-1".into(),
            release_artist_id: None,
            disc_number: Some(2),
            disc_count: Some(3),
            tracks: vec![],
        };
        assert_eq!(meta.disc_position().as_deref(), Some("2/3"));
        // A single-disc release has no disc position at all.
        let single = DiscMeta {
            disc_number: None,
            disc_count: None,
            ..meta.clone()
        };
        assert_eq!(single.disc_position(), None);
        // A number without a total degrades to the bare number.
        let partial = DiscMeta {
            disc_count: None,
            ..meta
        };
        assert_eq!(partial.disc_position().as_deref(), Some("2"));
    }

    #[test]
    fn picks_the_medium_carrying_the_disc_id() {
        let cdtoc: MbCdtoc = serde_json::from_str(EXACT).unwrap();
        let (release, medium) = pick_release_and_medium(&cdtoc.releases, "disc-1").unwrap();
        assert_eq!(release.id, "rel-1");
        assert!(medium.is_some());
        // A disc id no medium lists falls back to the first medium.
        let (_, medium) = pick_release_and_medium(&cdtoc.releases, "disc-2").unwrap();
        assert!(medium.is_some());
        // No releases at all: nothing to describe.
        let none: Vec<MbRelease> = vec![];
        assert!(pick_release_and_medium(&none, "disc-1").is_none());
    }

    #[test]
    fn rank_lists_candidates_best_first() {
        let releases: MbReleaseList = serde_json::from_str(FUZZY).unwrap();
        let ranked = rank_candidates(&releases.releases, &test_toc()).unwrap();

        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].max_diff_ms, 100);
        assert_eq!(ranked[0].meta.release_id, "rel-right");
        assert_eq!(ranked[0].meta.album, "Right Album");
        assert_eq!(ranked[0].meta.year.as_deref(), Some("1998"));
        assert_eq!(ranked[0].meta.tracks[0].title, "A");
        assert!(ranked[0].accepted());
        // The distant release is still a candidate, ranked last.
        assert_eq!(ranked[1].max_diff_ms, 180_000);
        assert!(!ranked[1].accepted());
        assert_eq!(ranked[1].meta.release_id, "rel-wrong");
    }

    #[test]
    fn rank_keeps_distant_candidates() {
        let releases: MbReleaseList = serde_json::from_str(
            r#"{"releases": [{"id": "r", "title": "T", "artist-credit": [], "media": [
                {"tracks": [
                    { "title": "A", "length": 200000 },
                    { "title": "B", "length": 120000 },
                    { "title": "C", "length": 130000 }
                ]}
            ]}]}"#,
        )
        .unwrap();
        // A release no tolerance would accept is still listed, so it can
        // be chosen deliberately.
        let ranked = rank_candidates(&releases.releases, &test_toc()).unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].meta.release_id, "r");
        assert_eq!(ranked[0].max_diff_ms, 180_000);
        assert!(!ranked[0].accepted());
    }

    #[test]
    fn rank_drops_track_count_mismatches() {
        let releases: MbReleaseList = serde_json::from_str(
            r#"{"releases": [{"id": "r", "title": "T", "artist-credit": [], "media": [
                {"tracks": [ {"title": "A", "length": 20000}, {"title": "B", "length": 10666} ]}
            ]}]}"#,
        )
        .unwrap();
        assert!(rank_candidates(&releases.releases, &test_toc()).is_none());
    }

    #[test]
    fn rank_drops_missing_lengths() {
        let releases: MbReleaseList = serde_json::from_str(
            r#"{"releases": [{"id": "r", "title": "T", "artist-credit": [], "media": [
                {"tracks": [
                    { "title": "A", "length": 20000 },
                    { "title": "B", "length": 10666 },
                    { "title": "C" }
                ]}
            ]}]}"#,
        )
        .unwrap();
        assert!(rank_candidates(&releases.releases, &test_toc()).is_none());
    }

    #[test]
    fn rank_keeps_the_best_medium_per_release() {
        let releases: MbReleaseList = serde_json::from_str(
            r#"{"releases": [{"id": "r", "title": "T", "artist-credit": [], "media": [
                {"tracks": [
                    { "title": "Wrong A", "length": 90000 },
                    { "title": "Wrong B", "length": 90000 },
                    { "title": "Wrong C", "length": 90000 }
                ]},
                {"tracks": [
                    { "title": "Good A", "length": 20100 },
                    { "title": "Good B", "length": 10600 },
                    { "title": "Good C", "length": 11400 }
                ]}
            ]}]}"#,
        )
        .unwrap();
        let ranked = rank_candidates(&releases.releases, &test_toc()).unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].max_diff_ms, 100);
        assert_eq!(ranked[0].meta.tracks[0].title, "Good A");
    }

    #[test]
    fn rank_carries_disc_position_for_multi_disc() {
        let releases: MbReleaseList = serde_json::from_str(
            r#"{"releases": [{"id": "r", "title": "T", "artist-credit": [], "media": [
                {"position": 1, "tracks": [
                    { "title": "D", "length": 50000 },
                    { "title": "E", "length": 60000 }
                ]},
                {"position": 2, "tracks": [
                    { "title": "A", "length": 20100 },
                    { "title": "B", "length": 10600 },
                    { "title": "C", "length": 11400 }
                ]}
            ]}]}"#,
        )
        .unwrap();
        let ranked = rank_candidates(&releases.releases, &test_toc()).unwrap();
        assert_eq!(ranked[0].meta.disc_number, Some(2));
        assert_eq!(ranked[0].meta.disc_count, Some(2));
        assert_eq!(ranked[0].meta.disc_position().as_deref(), Some("2/2"));
    }

    #[test]
    fn acceptance_needs_every_track_within_tolerance() {
        let meta = DiscMeta {
            album: "The Album".into(),
            artist: "The Band".into(),
            album_artist: None,
            year: None,
            release_id: "rel-1".into(),
            release_artist_id: None,
            disc_number: None,
            disc_count: None,
            tracks: vec![],
        };
        // An exact hit and a boundary hit are accepted.
        assert!(
            Candidate {
                meta: meta.clone(),
                max_diff_ms: 0
            }
            .accepted()
        );
        assert!(
            Candidate {
                meta: meta.clone(),
                max_diff_ms: FUZZY_TOLERANCE_MS
            }
            .accepted()
        );
        // One millisecond past the tolerance is not.
        assert!(
            !Candidate {
                meta,
                max_diff_ms: FUZZY_TOLERANCE_MS + 1
            }
            .accepted()
        );
    }

    #[test]
    fn score_medium_needs_every_length() {
        let tracks: Vec<MbTrack> =
            serde_json::from_str(r#"[{ "length": 20000 }, { "length": 10666 }]"#).unwrap();
        assert_eq!(score_medium(&tracks, &[20_000, 10_666]), Some(0));
        let tracks: Vec<MbTrack> =
            serde_json::from_str(r#"[{ "length": 20000 }, { "length": 10666 }, { "title": "C" }]"#)
                .unwrap();
        assert_eq!(score_medium(&tracks, &[20_000, 10_666, 11_333]), None);
        assert_eq!(score_medium(&tracks, &[20_000]), None);
    }

    #[test]
    fn durations_come_from_consecutive_starts() {
        assert_eq!(durations_ms(&test_toc()), vec![20_000, 10_666, 11_333]);
    }

    #[test]
    fn toc_param_is_pluss_separated() {
        let toc = DiscToc {
            offsets: vec![150, 1650],
            leadout: 3300,
        };
        assert_eq!(toc_param(&toc), "1+2+3300+150+1650");
    }

    #[test]
    fn register_url_uses_the_cdtoc_attach_endpoint() {
        let toc = DiscToc {
            offsets: vec![150, 1650, 2450],
            leadout: 3300,
        };
        assert_eq!(
            register_disc_id_url(&toc),
            "https://musicbrainz.org/cdtoc/attach?toc=1+3+3300+150+1650+2450"
        );
    }

    #[test]
    fn credit_names_use_joinphrases() {
        let credit: Vec<MbCredit> = serde_json::from_str(
            r#"[{ "name": "The Band", "joinphrase": " feat. " }, { "name": "The Choir" }]"#,
        )
        .unwrap();
        assert_eq!(credit_names(&credit), "The Band feat. The Choir");
        assert_eq!(credit_names(&[]), "");
    }

    #[test]
    fn track_positions_are_one_based() {
        let cdtoc: MbCdtoc = serde_json::from_str(EXACT).unwrap();
        let (release, medium) = matched_medium(&cdtoc.releases[0], "disc-1").unwrap();
        let meta = to_disc_meta(release, Some(medium));
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

    /// A throttle that never waits, so the HTTP tests stay fast.
    fn fast_throttle() -> Throttle {
        Throttle::new(Duration::ZERO)
    }

    /// A throwaway HTTP server that answers `responses` in order, one per
    /// connection, so the ureq path can be exercised without the network.
    fn serve_sequence(
        responses: Vec<(&str, &str)>,
    ) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
        let responses: Vec<(String, String)> = responses
            .into_iter()
            .map(|(status, body)| (status.to_string(), body.to_string()))
            .collect();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            for (status, body) in responses {
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
            }
        });
        (addr, handle)
    }

    fn serve_once(status: &str, body: &str) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
        serve_sequence(vec![(status, body)])
    }

    #[test]
    fn cdtoc_roundtrips_over_http() {
        let (addr, handle) = serve_once("200 OK", EXACT);
        let meta = cdtoc_from(&format!("http://{addr}/cdtoc"), "disc-1", &fast_throttle()).unwrap();
        handle.join().unwrap();

        let meta = meta.unwrap();
        assert_eq!(meta.album, "The Album");
        assert_eq!(meta.tracks.len(), 3);
    }

    #[test]
    fn cdtoc_404_is_no_match() {
        let (addr, handle) = serve_once("404 Not Found", "");
        let meta = cdtoc_from(&format!("http://{addr}/cdtoc"), "disc-1", &fast_throttle()).unwrap();
        handle.join().unwrap();

        assert!(meta.is_none());
    }

    #[test]
    fn release_list_roundtrips_over_http() {
        let (addr, handle) = serve_once("200 OK", FUZZY);
        let list: MbReleaseList =
            get_json(&format!("http://{addr}/releases"), &fast_throttle()).unwrap();
        handle.join().unwrap();

        assert_eq!(list.releases.len(), 2);
        assert_eq!(list.releases[1].id, "rel-right");
    }

    #[test]
    fn release_list_404_is_not_found() {
        let (addr, handle) = serve_once("404 Not Found", "");
        let err: Result<MbReleaseList, Error> =
            get_json(&format!("http://{addr}/releases"), &fast_throttle());
        handle.join().unwrap();

        assert!(matches!(err, Err(Error::NotFound)));
    }

    #[test]
    fn throttled_requests_are_retried() {
        let (addr, handle) =
            serve_sequence(vec![("503 Service Unavailable", ""), ("200 OK", EXACT)]);
        let cdtoc: MbCdtoc = get_json(&format!("http://{addr}/cdtoc"), &fast_throttle()).unwrap();
        handle.join().unwrap();

        assert_eq!(cdtoc.releases.len(), 1);
    }

    #[test]
    fn cover_404_is_no_cover() {
        let (addr, handle) = serve_once("404 Not Found", "");
        let url = format!("http://{addr}/cover");
        let art = crate::lookup::cover_art_from(&url, &fast_throttle()).unwrap();
        handle.join().unwrap();

        assert!(art.is_none());
    }
}
