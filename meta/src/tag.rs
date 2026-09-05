//! Writing metadata into finished audio files, via `lofty`.
//!
//! FLAC files get Vorbis comments and a `PICTURE` block for the cover art;
//! WAV files get an ID3v2 tag with the equivalent frames. `lofty` rewrites
//! the tag container of an existing file, so this runs after the audio has
//! been encoded.

use std::fs::OpenOptions;
use std::path::Path;

use lofty::config::WriteOptions;
use lofty::file::{AudioFile, FileType, TaggedFileExt};
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
    /// Release year, if known.
    pub year: Option<String>,
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
            year: disc.year.clone(),
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
pub fn apply(path: &Path, tags: &TrackTags, art: Option<&[u8]>) -> Result<(), Error> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let mut audio = lofty::read_from(&mut file)?;
    let tag_type = match audio.file_type() {
        FileType::Flac => TagType::VorbisComments,
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
    if let Some(year) = &tags.year {
        tag.insert_text(ItemKey::Year, year.clone());
    }

    if let Some(art) = art {
        let picture = Picture::unchecked(art.to_vec())
            .pic_type(PictureType::CoverFront)
            .mime_type(mime_of(art))
            .build();
        tag.push_picture(picture);
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

    /// A tiny but valid JPEG (1x1 black pixel), for picture roundtrips.
    const JPEG_1X1: &[u8] = &[
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00,
        0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06,
        0x05, 0x08, 0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x08, 0x08, 0x0A, 0x0C, 0x14, 0x0D, 0x0C,
        0x0B, 0x0B, 0x0C, 0x12, 0x10, 0x0A, 0x0B, 0x12, 0x13, 0x10, 0x11, 0x10, 0x10, 0x10, 0x10,
        0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10,
        0x10, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00, 0xFF,
        0xC4, 0x00, 0x1F, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09,
        0x0A, 0x0B, 0xFF, 0xC4, 0x00, 0xB5, 0x10, 0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03,
        0x05, 0x05, 0x04, 0x04, 0x00, 0x00, 0x01, 0x7D, 0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05,
        0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91,
        0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0, 0x24, 0x33, 0x62, 0x72, 0x82,
        0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35,
        0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54,
        0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73,
        0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A,
        0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
        0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4,
        0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA,
        0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5,
        0xF6, 0xF7, 0xF8, 0xF9, 0xFA, 0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00,
        0x7B, 0x94, 0x11, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x88, 0x5E, 0xFF, 0xD9,
    ];

    fn disc_meta() -> DiscMeta {
        DiscMeta {
            album: "The Album".into(),
            artist: "The Band".into(),
            album_artist: None,
            year: Some("1997".into()),
            release_id: "rel-1".into(),
            tracks: vec![
                crate::lookup::TrackMeta {
                    title: "First Song".into(),
                    artist: None,
                },
                crate::lookup::TrackMeta {
                    title: "Second Song".into(),
                    artist: Some("Guest".into()),
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
        assert_eq!(tags.year.as_deref(), Some("1997"));

        // A per-track artist wins over the release artist.
        let tags = TrackTags::for_track(&disc, 2, 2).unwrap();
        assert_eq!(tags.artist, "Guest");

        // Unknown positions yield no tags at all.
        assert_eq!(TrackTags::for_track(&disc, 3, 2), None);
        assert_eq!(TrackTags::for_track(&disc, 0, 2), None);
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
        assert!(matches!(mime_of(JPEG_1X1), MimeType::Jpeg));
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
        apply(&path, &tags, Some(JPEG_1X1)).unwrap();

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
        let pic = tag.get_picture_type(PictureType::CoverFront).unwrap();
        assert_eq!(pic.data(), JPEG_1X1);
        assert!(matches!(pic.mime_type().unwrap(), MimeType::Jpeg));
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
            let mut w = rend_encode::flac::FlacWriter::create(&path).unwrap();
            w.write(&vec![0u8; 44_100 * 4]).unwrap();
            w.finish().unwrap();
        }

        let tags = TrackTags::for_track(&disc_meta(), 2, 2).unwrap();
        apply(&path, &tags, Some(JPEG_1X1)).unwrap();

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
        let pic = tag.get_picture_type(PictureType::CoverFront).unwrap();
        assert_eq!(pic.data(), JPEG_1X1);
    }
}
