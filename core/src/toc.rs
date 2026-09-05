//! Table of contents (TOC) of a CD.

use crate::msf::Msf;
use crate::sys::{CDROM_DATA_TRACK, Cdrom_tocentry};

/// A single track in the table of contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    /// 1-based track number, as reported by the drive.
    pub number: u8,
    /// Track content type.
    pub kind: TrackType,
    /// LBA of the track's first frame.
    pub start_lba: u32,
}

impl Track {
    /// The track start as an MSF address (for display).
    pub fn start_msf(&self) -> Option<Msf> {
        Msf::from_lba(self.start_lba as i64)
    }

    /// `true` if this is a Red Book audio track.
    pub fn is_audio(&self) -> bool {
        matches!(self.kind, TrackType::Audio)
    }

    /// Duration of the track in frames, given its end LBA.
    pub fn frames(&self, end_lba: u32) -> u32 {
        end_lba.saturating_sub(self.start_lba)
    }
}

/// Track content type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackType {
    /// Red Book audio.
    Audio,
    /// Data track (Yellow/Green Book).
    Data,
}

impl TrackType {
    /// Classify a track from the TOC entry's ADR/control byte.
    pub fn from_adr_ctrl(adr_ctrl: u8) -> Self {
        if adr_ctrl & CDROM_DATA_TRACK != 0 {
            TrackType::Data
        } else {
            TrackType::Audio
        }
    }
}

/// The table of contents of a CD.
#[derive(Debug, Clone, Default)]
pub struct Toc {
    /// Tracks in disc order (excluding the lead-out).
    pub tracks: Vec<Track>,
    /// LBA of the lead-out: one past the last frame on the disc.
    pub leadout_lba: u32,
}

impl Toc {
    /// Build a TOC from the per-track entries and the lead-out entry.
    ///
    /// All entries must have been read with LBA addressing.
    pub fn from_entries(tracks: &[Cdrom_tocentry], leadout: &Cdrom_tocentry) -> Self {
        Self {
            tracks: tracks
                .iter()
                .map(|e| Track {
                    number: e.track,
                    kind: TrackType::from_adr_ctrl(e.adr_ctrl),
                    start_lba: e.addr,
                })
                .collect(),
            leadout_lba: leadout.addr,
        }
    }

    /// The track with the given 1-based number, if any.
    pub fn track(&self, number: u8) -> Option<&Track> {
        self.tracks.iter().find(|t| t.number == number)
    }

    /// Audio tracks only, in disc order.
    pub fn audio_tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.kind == TrackType::Audio)
    }

    /// One past the last frame of the given track: the start of the next
    /// track, or the lead-out for the final track.
    pub fn end_lba(&self, number: u8) -> Option<u32> {
        let idx = self.tracks.iter().position(|t| t.number == number)?;
        Some(
            self.tracks
                .get(idx + 1)
                .map(|t| t.start_lba)
                .unwrap_or(self.leadout_lba),
        )
    }

    /// Number of frames between `lba` and the lead-out.
    pub fn frames_remaining(&self, lba: u32) -> u32 {
        self.leadout_lba.saturating_sub(lba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{CDROM_LBA, CDROM_LEADOUT};

    fn audio_entry(track: u8, lba: u32) -> Cdrom_tocentry {
        let mut e = Cdrom_tocentry::new(track, CDROM_LBA);
        e.addr = lba;
        e
    }

    fn data_entry(track: u8, lba: u32) -> Cdrom_tocentry {
        let mut e = Cdrom_tocentry::new(track, CDROM_LBA);
        e.adr_ctrl = 0x04;
        e.addr = lba;
        e
    }

    fn leadout(lba: u32) -> Cdrom_tocentry {
        let mut e = Cdrom_tocentry::new(CDROM_LEADOUT, CDROM_LBA);
        e.addr = lba;
        e
    }

    #[test]
    fn track_type_from_adr_ctrl() {
        assert_eq!(TrackType::from_adr_ctrl(0x00), TrackType::Audio);
        assert_eq!(TrackType::from_adr_ctrl(0x04), TrackType::Data);
        // High nibble (ADR) must not affect classification.
        assert_eq!(TrackType::from_adr_ctrl(0x14), TrackType::Data);
        assert_eq!(TrackType::from_adr_ctrl(0x10), TrackType::Audio);
    }

    #[test]
    fn parses_audio_disc() {
        let entries = [
            audio_entry(1, 0),
            audio_entry(2, 1500),
            audio_entry(3, 2500),
        ];
        let toc = Toc::from_entries(&entries, &leadout(3000));

        assert_eq!(toc.tracks.len(), 3);
        assert_eq!(toc.leadout_lba, 3000);
        assert!(toc.tracks.iter().all(|t| t.is_audio()));
        assert_eq!(toc.audio_tracks().count(), 3);
    }

    #[test]
    fn skips_data_tracks() {
        let entries = [audio_entry(1, 0), data_entry(2, 1000), audio_entry(3, 2000)];
        let toc = Toc::from_entries(&entries, &leadout(3000));

        let audio: Vec<u8> = toc.audio_tracks().map(|t| t.number).collect();
        assert_eq!(audio, vec![1, 3]);
        assert_eq!(toc.track(2).unwrap().kind, TrackType::Data);
    }

    #[test]
    fn end_lba_and_duration() {
        let entries = [audio_entry(1, 0), audio_entry(2, 1500)];
        let toc = Toc::from_entries(&entries, &leadout(3000));

        assert_eq!(toc.end_lba(1), Some(1500));
        assert_eq!(toc.end_lba(2), Some(3000));
        assert_eq!(toc.end_lba(9), None);

        let t1 = toc.track(1).unwrap();
        assert_eq!(t1.frames(1500), 1500);
        let t2 = toc.track(2).unwrap();
        assert_eq!(t2.frames(3000), 1500);
        assert_eq!(toc.frames_remaining(0), 3000);
        assert_eq!(toc.frames_remaining(3000), 0);
    }

    #[test]
    fn start_msf() {
        let entries = [audio_entry(1, 0), audio_entry(2, 4500)];
        let toc = Toc::from_entries(&entries, &leadout(9000));

        assert_eq!(toc.track(1).unwrap().start_msf(), Some(Msf::new(0, 2, 0)));
        // LBA 4500 is one minute of audio past the start.
        assert_eq!(toc.track(2).unwrap().start_msf(), Some(Msf::new(1, 2, 0)));
    }
}
