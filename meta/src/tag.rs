//! Writing metadata into finished audio files, via `lofty`.
//!
//! FLAC files get Vorbis comments and a `PICTURE` block for the cover art;
//! WAV files get an ID3v2 tag with the equivalent frames. `lofty` rewrites
//! the tag container of an existing file, so this runs after the audio has
//! been encoded.

use std::fs::OpenOptions;
use std::path::Path;

use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::tag::{Accessor, ItemKey, Tag, TagType};

use crate::lookup::DiscMeta;

/// The tags to write into one track file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackTags {
    /// Track title.
    pub title: String,
    /// Track artist.
    pub artist: String,
    /// Album title.
    pub album: String,
    /// Album artist, if known.
    pub album_artist: Option<String>,
    /// 1-based track position on the disc.
    pub track_number: u32,
    /// Total audio tracks on the disc, if known.
    pub track_count: Option<u32>,
    /// 1-based disc position within the release, for multi-disc releases.
    pub disc_number: Option<u32>,
    /// Total discs in the release, for multi-disc releases.
    pub disc_count: Option<u32>,
    /// Release year, if known.
    pub year: Option<String>,
    /// The disc's catalog number (MCN), when the drive reported one.
    pub catalog_number: Option<String>,
    /// MusicBrainz release id, when looked up.
    pub release_id: Option<String>,
    /// MusicBrainz id of the release artist, when looked up.
    pub release_artist_id: Option<String>,
    /// MusicBrainz id of the track's artist (falling back to the release
    /// artist), when known.
    pub artist_id: Option<String>,
    /// MusicBrainz recording id, when known.
    pub recording_id: Option<String>,
    /// MusicBrainz release-track id, when known.
    pub release_track_id: Option<String>,
}

impl TrackTags {
    /// The tags for the track at the given 1-based position of a looked-up
    /// disc; `total` is the number of audio tracks on the disc.
    ///
    /// The track artist falls back to the release artist, and the album
    /// artist to the release artist as well.
    ///
    /// Returns `None` when the lookup knows nothing about that position.
    pub fn for_track(disc: &DiscMeta, position: usize, total: usize) -> Option<Self> {
        let track = disc.track(position)?;
        let release_artist = (!disc.artist.is_empty()).then(|| disc.artist.clone());
        Some(Self {
            title: track.title.clone(),
            artist: track.artist.clone().unwrap_or_else(|| disc.artist.clone()),
            album: disc.album.clone(),
            album_artist: disc
                .album_artist
                .clone()
                .filter(|a| !a.is_empty())
                .or(release_artist),
            track_number: position as u32,
            track_count: (total > 1).then_some(total as u32),
            disc_number: disc.disc_number,
            disc_count: disc.disc_count,
            year: disc.year.clone(),
            catalog_number: None,
            release_id: (!disc.release_id.is_empty()).then(|| disc.release_id.clone()),
            release_artist_id: disc.release_artist_id.clone(),
            artist_id: track.artist_id.clone().or(disc.release_artist_id.clone()),
            recording_id: track.recording_id.clone(),
            release_track_id: track.release_track_id.clone(),
        })
    }
}

/// Errors writing tags into a file.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The file could not be opened.
    #[error("could not open the audio file: {0}")]
    Io(#[from] std::io::Error),
    /// The file could not be read as audio.
    #[error("could not read the audio file: {0}")]
    Read(#[from] lofty::error::FileParseError),
    /// The tags could not be written.
    #[error("could not write the tags: {0}")]
    Write(#[from] lofty::error::FileEncodingError),
}

/// Writes `tags` (and the cover `art`, if any) into the audio file at
/// `path`, in place.
///
/// FLAC, Ogg Vorbis, and Opus files get every field, including all six
/// MusicBrainz ids, via Vorbis comments. M4A files get MP4 tags. Other
/// formats are written as ID3v2, where lofty can only carry the catalog
/// number, release-artist id and artist id; the release and release-track ids
/// have no standard four-letter frame and the recording id's UFID frame is
/// dropped, so those three are not written for ID3v2.
pub fn apply(path: &Path, tags: &TrackTags, art: Option<&[u8]>) -> Result<(), Error> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let mut audio = lofty::read_from(&mut file)?;
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let tag_type = match extension.as_str() {
        "flac" | "ogg" | "opus" => TagType::VorbisComments,
        "m4a" | "mp4" | "mp4a" => TagType::Mp4Ilst,
        _ => TagType::Id3v2,
    };
    if audio.tag(tag_type).is_none() {
        audio.insert_tag(Tag::new(tag_type));
    }
    let tag = audio.tag_mut(tag_type).expect("tag was just inserted");

    tag.set_title(tags.title.clone());
    tag.set_artist(tags.artist.clone());
    tag.set_album(tags.album.clone());
    if let Some(album_artist) = &tags.album_artist {
        tag.insert_text(ItemKey::AlbumArtist, album_artist.clone());
    }
    tag.set_track(tags.track_number);
    if let Some(total) = tags.track_count {
        tag.set_track_total(total);
    }
    if let Some(number) = tags.disc_number {
        tag.set_disk(number);
    }
    if let Some(total) = tags.disc_count {
        tag.set_disk_total(total);
    }
    if let Some(year) = &tags.year {
        tag.insert_text(ItemKey::Year, year.clone());
    }
    if let Some(catalog) = &tags.catalog_number {
        tag.insert_text(ItemKey::CatalogNumber, catalog.clone());
    }
    if let Some(id) = &tags.release_id {
        tag.insert_text(ItemKey::MusicBrainzReleaseId, id.clone());
    }
    if let Some(id) = &tags.release_artist_id {
        tag.insert_text(ItemKey::MusicBrainzReleaseArtistId, id.clone());
    }
    if let Some(id) = &tags.artist_id {
        tag.insert_text(ItemKey::MusicBrainzArtistId, id.clone());
    }
    if let Some(id) = &tags.recording_id {
        tag.insert_text(ItemKey::MusicBrainzRecordingId, id.clone());
    }
    if let Some(id) = &tags.release_track_id {
        tag.insert_text(ItemKey::MusicBrainzTrackId, id.clone());
    }

    if let Some(art) = art {
        // The tag containers only carry JPEG and PNG well, so anything else
        // is re-encoded first. A picture that cannot be decoded is dropped
        // rather than losing the tags with it.
        if let Ok(art) = crate::image::normalize_art(art) {
            let mime = mime_of(&art);
            let picture = Picture::unchecked(art)
                .pic_type(PictureType::CoverFront)
                .mime_type(mime)
                .build();
            tag.push_picture(picture);
        }
    }

    audio.save_to(&mut file, WriteOptions::default())?;
    Ok(())
}

/// Guesses the image MIME type from the file's magic bytes.
fn mime_of(bytes: &[u8]) -> MimeType {
    if bytes.starts_with(&[0xFF, 0xD8]) {
        MimeType::Jpeg
    } else if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        MimeType::Png
    } else {
        MimeType::Unknown("application/octet-stream".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    /// A tiny valid JPEG (a 1x1 black pixel), for picture roundtrips.
    fn jpeg_1x1() -> Vec<u8> {
        let pixel = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut out)
            .encode_image(&pixel)
            .unwrap();
        out
    }

    fn disc_meta() -> DiscMeta {
        DiscMeta {
            album: "The Album".into(),
            artist: "The Band".into(),
            album_artist: None,
            year: Some("1997".into()),
            release_id: "rel-1".into(),
            release_artist_id: Some("ar-1".into()),
            disc_number: Some(1),
            disc_count: Some(2),
            tracks: vec![
                crate::lookup::TrackMeta {
                    title: "First Song".into(),
                    artist: None,
                    artist_id: None,
                    recording_id: Some("rec-1".into()),
                    release_track_id: Some("rt-1".into()),
                },
                crate::lookup::TrackMeta {
                    title: "Second Song".into(),
                    artist: Some("Guest".into()),
                    artist_id: Some("ar-2".into()),
                    recording_id: Some("rec-2".into()),
                    release_track_id: Some("rt-2".into()),
                },
            ],
        }
    }

    #[test]
    fn tags_for_track_falls_back_to_the_release() {
        let disc = disc_meta();
        let tags = TrackTags::for_track(&disc, 1, 2).unwrap();
        assert_eq!(tags.title, "First Song");
        assert_eq!(tags.artist, "The Band");
        assert_eq!(tags.album, "The Album");
        assert_eq!(tags.album_artist.as_deref(), Some("The Band"));
        assert_eq!(tags.track_number, 1);
        assert_eq!(tags.track_count, Some(2));
        assert_eq!(tags.disc_number, Some(1));
        assert_eq!(tags.disc_count, Some(2));
        assert_eq!(tags.year.as_deref(), Some("1997"));
        // The catalog number is not known until the rip, so it starts out
        // empty and is filled in per disc.
        assert_eq!(tags.catalog_number, None);
        assert_eq!(tags.release_id.as_deref(), Some("rel-1"));
        assert_eq!(tags.release_artist_id.as_deref(), Some("ar-1"));
        // An uncredited track's artist id falls back to the release artist's.
        assert_eq!(tags.artist_id.as_deref(), Some("ar-1"));
        assert_eq!(tags.recording_id.as_deref(), Some("rec-1"));
        assert_eq!(tags.release_track_id.as_deref(), Some("rt-1"));

        // A per-track artist wins over the release artist.
        let tags = TrackTags::for_track(&disc, 2, 2).unwrap();
        assert_eq!(tags.artist, "Guest");
        assert_eq!(tags.artist_id.as_deref(), Some("ar-2"));
        assert_eq!(tags.recording_id.as_deref(), Some("rec-2"));
        assert_eq!(tags.release_track_id.as_deref(), Some("rt-2"));

        // Unknown positions yield no tags at all.
        assert_eq!(TrackTags::for_track(&disc, 3, 2), None);
        assert_eq!(TrackTags::for_track(&disc, 0, 2), None);
    }

    #[test]
    fn single_disc_tracks_get_no_disc_tags() {
        let mut disc = disc_meta();
        disc.disc_number = None;
        disc.disc_count = None;
        let tags = TrackTags::for_track(&disc, 1, 2).unwrap();
        assert_eq!(tags.disc_number, None);
        assert_eq!(tags.disc_count, None);
        // The track numbering is unaffected.
        assert_eq!(tags.track_number, 1);
        assert_eq!(tags.track_count, Some(2));
    }

    #[test]
    fn an_empty_release_id_yields_no_release_tags() {
        let mut disc = disc_meta();
        disc.release_id = String::new();
        disc.release_artist_id = None;
        let tags = TrackTags::for_track(&disc, 1, 2).unwrap();
        assert_eq!(tags.release_id, None);
        assert_eq!(tags.release_artist_id, None);
        // With no release artist id, an uncredited track has no artist id.
        assert_eq!(tags.artist_id, None);
        // The other ids are unaffected.
        assert_eq!(tags.recording_id.as_deref(), Some("rec-1"));
        assert_eq!(tags.release_track_id.as_deref(), Some("rt-1"));
    }

    #[test]
    fn an_explicit_album_artist_wins_over_the_fallback() {
        let mut disc = disc_meta();
        disc.album_artist = Some("Various Artists".into());
        let tags = TrackTags::for_track(&disc, 1, 2).unwrap();
        assert_eq!(tags.album_artist.as_deref(), Some("Various Artists"));
        // The track artist still falls back to the release artist.
        assert_eq!(tags.artist, "The Band");
    }

    #[test]
    fn mime_of_sniffs_the_magic_bytes() {
        assert!(matches!(mime_of(&jpeg_1x1()), MimeType::Jpeg));
        assert!(matches!(
            mime_of(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]),
            MimeType::Png
        ));
        assert!(matches!(mime_of(&[1, 2, 3]), MimeType::Unknown(_)));
    }

    /// Writes a one-second WAV through the real encoder, tags it, and reads
    /// the tags back with lofty.
    #[test]
    fn tags_a_wav_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wav");
        {
            let mut w = rend_encode::wav::WavWriter::create(&path).unwrap();
            w.write(&vec![0u8; 44_100 * 4]).unwrap();
            w.finish().unwrap();
        }

        let tags = TrackTags::for_track(&disc_meta(), 1, 2).unwrap();
        let jpeg = jpeg_1x1();
        apply(&path, &tags, Some(&jpeg)).unwrap();

        let file = lofty::read_from_path(&path).unwrap();
        let tag = file.tag(TagType::Id3v2).expect("an ID3v2 tag was written");
        assert_eq!(
            tag.title().map(Cow::into_owned).as_deref(),
            Some("First Song")
        );
        assert_eq!(
            tag.artist().map(Cow::into_owned).as_deref(),
            Some("The Band")
        );
        assert_eq!(
            tag.album().map(Cow::into_owned).as_deref(),
            Some("The Album")
        );
        assert_eq!(tag.track(), Some(1));
        assert_eq!(tag.track_total(), Some(2));
        assert_eq!(tag.disk(), Some(1));
        assert_eq!(tag.disk_total(), Some(2));
        let pic = tag.get_picture_type(PictureType::CoverFront).unwrap();
        assert_eq!(pic.data(), &jpeg[..]);
        assert!(matches!(pic.mime_type().unwrap(), MimeType::Jpeg));
    }

    /// A track with a catalog number set at rip time gets it and the
    /// MusicBrainz ids written. ID3v2 has no standard four-letter frame for the
    /// release and release-track ids, and lofty's WAV writer drops the
    /// recording id's UFID frame, so only the catalog number and the
    /// release-artist and artist ids survive on ID3v2 (the full set is covered
    /// by the FLAC test).
    #[test]
    fn the_catalog_number_and_musicbrainz_ids_are_written_to_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wav");
        {
            let mut w = rend_encode::wav::WavWriter::create(&path).unwrap();
            w.write(&vec![0u8; 44_100 * 4]).unwrap();
            w.finish().unwrap();
        }

        let mut tags = TrackTags::for_track(&disc_meta(), 1, 2).unwrap();
        tags.catalog_number = Some("MCN 123".into());
        apply(&path, &tags, None).unwrap();

        let file = lofty::read_from_path(&path).unwrap();
        let tag = file.tag(TagType::Id3v2).expect("an ID3v2 tag was written");
        assert_eq!(tag.get_string(ItemKey::CatalogNumber), Some("MCN 123"));
        assert_eq!(
            tag.get_string(ItemKey::MusicBrainzReleaseArtistId),
            Some("ar-1")
        );
        assert_eq!(tag.get_string(ItemKey::MusicBrainzArtistId), Some("ar-1"));
    }

    /// The full set of ids (including the release and release-track ids, which
    /// have no ID3v2 frame) round-trips through Vorbis comments when the track
    /// is a FLAC file (skipped on hosts without ffmpeg to encode one).
    #[test]
    fn the_catalog_number_and_musicbrainz_ids_are_written_to_flac() {
        if !rend_encode::ffmpeg_available() {
            eprintln!("skipping: ffmpeg not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.flac");
        {
            let mut w = rend_encode::Format::parse("flac")
                .unwrap()
                .create_file(&path)
                .unwrap();
            w.write(&vec![0u8; 44_100 * 4]).unwrap();
            w.finish().unwrap();
        }

        let mut tags = TrackTags::for_track(&disc_meta(), 1, 2).unwrap();
        tags.catalog_number = Some("MCN 123".into());
        apply(&path, &tags, None).unwrap();

        let file = lofty::read_from_path(&path).unwrap();
        let tag = file
            .tag(TagType::VorbisComments)
            .expect("vorbis comments were written");
        assert_eq!(tag.get_string(ItemKey::CatalogNumber), Some("MCN 123"));
        assert_eq!(tag.get_string(ItemKey::MusicBrainzReleaseId), Some("rel-1"));
        assert_eq!(
            tag.get_string(ItemKey::MusicBrainzReleaseArtistId),
            Some("ar-1")
        );
        assert_eq!(tag.get_string(ItemKey::MusicBrainzArtistId), Some("ar-1"));
        assert_eq!(
            tag.get_string(ItemKey::MusicBrainzRecordingId),
            Some("rec-1")
        );
        assert_eq!(tag.get_string(ItemKey::MusicBrainzTrackId), Some("rt-1"));
    }

    /// A 1x1 red lossless WebP.
    const WEBP_1X1: &[u8] = &[
        0x52, 0x49, 0x46, 0x46, 0x1C, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38,
        0x4C, 0x0F, 0x00, 0x00, 0x00, 0x2F, 0x00, 0x00, 0x00, 0x00, 0x07, 0x10, 0xF5, 0x8F, 0xFE,
        0x07, 0x22, 0xA2, 0xFF, 0x01, 0x00,
    ];

    /// A WebP cover is re-encoded to the JPEG the ID3v2 picture frame carries,
    /// not embedded as-is.
    #[test]
    fn a_webp_cover_is_embedded_as_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wav");
        {
            let mut w = rend_encode::wav::WavWriter::create(&path).unwrap();
            w.write(&vec![0u8; 44_100 * 4]).unwrap();
            w.finish().unwrap();
        }

        let tags = TrackTags::for_track(&disc_meta(), 1, 2).unwrap();
        apply(&path, &tags, Some(WEBP_1X1)).unwrap();

        let file = lofty::read_from_path(&path).unwrap();
        let tag = file.tag(TagType::Id3v2).expect("an ID3v2 tag was written");
        let pic = tag.get_picture_type(PictureType::CoverFront).unwrap();
        assert!(pic.data().starts_with(&[0xFF, 0xD8, 0xFF]));
        assert_ne!(pic.data(), WEBP_1X1);
        assert!(matches!(pic.mime_type().unwrap(), MimeType::Jpeg));
    }

    /// A cover that cannot be decoded is dropped so the tags are still
    /// written.
    #[test]
    fn an_undecodable_cover_is_dropped_but_the_tags_are_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wav");
        {
            let mut w = rend_encode::wav::WavWriter::create(&path).unwrap();
            w.write(&vec![0u8; 44_100 * 4]).unwrap();
            w.finish().unwrap();
        }

        let tags = TrackTags::for_track(&disc_meta(), 1, 2).unwrap();
        apply(&path, &tags, Some(b"not an image")).unwrap();

        let file = lofty::read_from_path(&path).unwrap();
        let tag = file.tag(TagType::Id3v2).expect("an ID3v2 tag was written");
        assert_eq!(
            tag.title().map(Cow::into_owned).as_deref(),
            Some("First Song")
        );
        assert!(tag.get_picture_type(PictureType::CoverFront).is_none());
    }

    /// Same roundtrip through a real FLAC file, when ffmpeg is available to
    /// encode one (skipped on hosts without it).
    #[test]
    fn tags_a_flac_file_when_ffmpeg_is_available() {
        if !rend_encode::ffmpeg_available() {
            eprintln!("skipping: ffmpeg not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.flac");
        {
            let mut w = rend_encode::Format::parse("flac")
                .unwrap()
                .create_file(&path)
                .unwrap();
            w.write(&vec![0u8; 44_100 * 4]).unwrap();
            w.finish().unwrap();
        }

        let tags = TrackTags::for_track(&disc_meta(), 2, 2).unwrap();
        let jpeg = jpeg_1x1();
        apply(&path, &tags, Some(&jpeg)).unwrap();

        let file = lofty::read_from_path(&path).unwrap();
        let tag = file
            .tag(TagType::VorbisComments)
            .expect("vorbis comments were written");
        assert_eq!(
            tag.title().map(Cow::into_owned).as_deref(),
            Some("Second Song")
        );
        assert_eq!(tag.artist().map(Cow::into_owned).as_deref(), Some("Guest"));
        assert_eq!(
            tag.album().map(Cow::into_owned).as_deref(),
            Some("The Album")
        );
        assert_eq!(tag.track(), Some(2));
        assert_eq!(tag.disk(), Some(1));
        assert_eq!(tag.disk_total(), Some(2));
        let pic = tag.get_picture_type(PictureType::CoverFront).unwrap();
        assert_eq!(pic.data(), &jpeg[..]);
    }
}
