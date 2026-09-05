/// Table of contents of a CD disc.
#[derive(Debug, Clone, Default)]
pub struct Toc {
    /// Tracks, in disc order.
    pub tracks: Vec<Track>,
}

/// A single track from the table of contents.
#[derive(Debug, Clone)]
pub struct Track {
    /// Track number (1-based, as reported by the drive).
    pub number: u8,
    /// Track type (audio, data, ...).
    pub kind: TrackType,
    /// Start of the track in frames (LBA).
    pub start_lba: u32,
}

/// Track content type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackType {
    /// Red Book audio.
    Audio,
    /// Data track.
    Data,
    /// XA / mixed-mode track.
    Xa,
}
