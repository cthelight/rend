//! rend-encode: the output-format layer for rend.
//!
//! Ripping a disc yields a stream of raw CDDA PCM — 16-bit little-endian,
//! 44 100 Hz, 2 channels. This crate turns that stream into finished audio
//! files, keeping the "which format" decision separate from the "read the
//! disc" machinery in `rend-core`.
//!
//! Supported formats are FLAC (the default), transcoded in a single pass by
//! streaming the PCM into `ffmpeg`, and uncompressed WAV, written directly.
//! To add a format: add a variant to [`Format`], a writer module, and a
//! case in [`TrackFile`].

use std::io;
use std::path::Path;
use std::process::Command;

pub mod flac;
pub mod wav;

/// The audio format of a ripped track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// Lossless FLAC, transcoded with ffmpeg at the highest compression.
    #[default]
    Flac,
    /// Uncompressed 16-bit PCM WAV, written directly.
    Wav,
}

impl Format {
    /// Every supported format, in toggle order.
    pub const ALL: [Format; 2] = [Self::Flac, Self::Wav];

    /// The next format in [`Format::ALL`], wrapping around (for toggles).
    pub fn next(self) -> Self {
        let all = Self::ALL;
        let i = all
            .iter()
            .position(|f| *f == self)
            .expect("every Format is in ALL");
        all[(i + 1) % all.len()]
    }

    /// The file extension of this format, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Wav => "wav",
        }
    }

    /// Short lowercase label, for UIs and CLI help.
    pub fn label(self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Wav => "wav",
        }
    }

    /// Parses a format name, case-insensitively.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "flac" => Ok(Self::Flac),
            "wav" => Ok(Self::Wav),
            other => Err(format!("unknown format '{other}' (expected flac or wav)")),
        }
    }

    /// `true` if encoding this format needs an external tool.
    pub fn requires_ffmpeg(self) -> bool {
        matches!(self, Self::Flac)
    }

    /// Creates a new track output file of this format at `path`.
    pub fn create_file(self, path: &Path) -> io::Result<TrackFile> {
        match self {
            Self::Flac => Ok(TrackFile::Flac(flac::FlacWriter::create(path)?)),
            Self::Wav => Ok(TrackFile::Wav(wav::WavWriter::create(path)?)),
        }
    }
}

/// A track's output file, fed raw PCM as the track is read.
pub enum TrackFile {
    Flac(flac::FlacWriter),
    Wav(wav::WavWriter),
}

impl TrackFile {
    /// Appends raw PCM samples (s16le, 44 100 Hz, stereo).
    pub fn write(&mut self, pcm: &[u8]) -> io::Result<()> {
        match self {
            Self::Flac(f) => f.write(pcm),
            Self::Wav(w) => w.write(pcm),
        }
    }

    /// Finalizes the file and flushes it to disk.
    ///
    /// The writer must not be used afterwards.
    pub fn finish(self) -> io::Result<()> {
        match self {
            Self::Flac(f) => f.finish(),
            Self::Wav(w) => w.finish(),
        }
    }
}

/// `true` if the `ffmpeg` executable can be run from the current PATH.
pub fn ffmpeg_available() -> bool {
    Command::new(flac::FFMPEG)
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_is_case_insensitive() {
        assert_eq!(Format::parse("flac").unwrap(), Format::Flac);
        assert_eq!(Format::parse("FLAC").unwrap(), Format::Flac);
        assert_eq!(Format::parse("wav").unwrap(), Format::Wav);
        assert_eq!(Format::parse("WAV").unwrap(), Format::Wav);
        assert!(Format::parse("mp3").is_err());
        assert!(Format::parse("").is_err());
    }

    #[test]
    fn default_is_flac() {
        assert_eq!(Format::default(), Format::Flac);
    }

    #[test]
    fn next_cycles() {
        assert_eq!(Format::Flac.next(), Format::Wav);
        assert_eq!(Format::Wav.next(), Format::Flac);
    }

    #[test]
    fn labels_and_extensions() {
        assert_eq!(Format::Flac.extension(), "flac");
        assert_eq!(Format::Wav.extension(), "wav");
        assert_eq!(Format::Flac.label(), "flac");
        assert_eq!(Format::Wav.label(), "wav");
    }

    #[test]
    fn flac_requires_ffmpeg() {
        assert!(Format::Flac.requires_ffmpeg());
        assert!(!Format::Wav.requires_ffmpeg());
    }
}
