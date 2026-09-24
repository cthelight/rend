//! A simulated CD drive and disc, so the TUI can be exercised without
//! hardware (`rend-tui --demo`).

use std::io;
use std::time::Duration;

use rend_core::{FRAMES_PER_SECOND, FrameSource, Toc, Track, TrackType};
use rend_meta::{DiscMeta, TrackMeta};

/// Device path shown for the simulated drive.
pub const DEMO_DEVICE: &str = "/dev/sr-demo";
/// Device label shown for the simulated drive.
pub const DEMO_LABEL: &str = "SIMULATED AudioDrive v1.0";
/// Medium catalog number reported for the simulated disc.
pub const DEMO_MCN: &str = "01234567890101";

/// How many times faster than real time the simulated drive reads.
const DEMO_SPEED: u32 = 8;
/// Frequency of the tone the simulated disc plays back.
const DEMO_FREQ: f64 = 440.0;
/// Amplitude of the tone (16-bit full scale is 32 767).
const DEMO_AMPLITUDE: f64 = 12_000.0;

/// A simulated disc: a fixed TOC plus read parameters.
#[derive(Debug, Clone)]
pub struct DemoDisc {
    /// Table of contents of the simulated disc.
    pub toc: Toc,
    /// Simulated read latency for a full 75-frame chunk.
    pub chunk_delay: Duration,
}

impl Default for DemoDisc {
    fn default() -> Self {
        Self::new()
    }
}

impl DemoDisc {
    /// The standard simulated disc: five short audio tracks.
    pub fn new() -> Self {
        let chunk_delay = Duration::from_millis(1000 / DEMO_SPEED as u64);
        let toc = Toc {
            tracks: [
                Track {
                    number: 1,
                    kind: TrackType::Audio,
                    start_lba: 0,
                },
                Track {
                    number: 2,
                    kind: TrackType::Audio,
                    start_lba: 1500,
                },
                Track {
                    number: 3,
                    kind: TrackType::Audio,
                    start_lba: 3750,
                },
                Track {
                    number: 4,
                    kind: TrackType::Audio,
                    start_lba: 3900,
                },
                Track {
                    number: 5,
                    kind: TrackType::Audio,
                    start_lba: 7500,
                },
            ]
            .into_iter()
            .collect(),
            leadout_lba: 10500,
        };
        Self { toc, chunk_delay }
    }
}

/// The metadata reported for the simulated disc, so the TUI can exercise
/// the metadata flow without a network. One entry per audio track, in disc
/// order; track 3 (the short one) has its own artist.
pub fn demo_meta() -> DiscMeta {
    DiscMeta {
        album: "Demo Album".into(),
        artist: "The Demo Band".into(),
        album_artist: None,
        year: Some("2024".into()),
        release_id: "demo-release".into(),
        release_artist_id: None,
        disc_number: Some(1),
        disc_count: Some(2),
        tracks: vec![
            TrackMeta {
                title: "Intro".into(),
                artist: None,
                artist_id: None,
                recording_id: None,
                release_track_id: None,
            },
            TrackMeta {
                title: "First Track".into(),
                artist: None,
                artist_id: None,
                recording_id: None,
                release_track_id: None,
            },
            TrackMeta {
                title: "Short One".into(),
                artist: Some("Guest Artist".into()),
                artist_id: None,
                recording_id: None,
                release_track_id: None,
            },
            TrackMeta {
                title: "Interlude".into(),
                artist: None,
                artist_id: None,
                recording_id: None,
                release_track_id: None,
            },
            TrackMeta {
                title: "Outro".into(),
                artist: None,
                artist_id: None,
                recording_id: None,
                release_track_id: None,
            },
        ],
    }
}

/// A second candidate for the simulated disc, an "official reissue", so
/// the candidate-switching flow can be exercised without a network.
pub fn demo_meta_reissue() -> DiscMeta {
    let mut meta = demo_meta();
    meta.album = "Demo Album (Reissue)".into();
    meta.year = Some("2025".into());
    meta.release_id = "demo-release-reissue".into();
    meta.tracks[2].title = "Short One (Reprise)".into();
    meta
}

/// Every candidate for the simulated disc, best first.
pub fn demo_meta_all() -> Vec<DiscMeta> {
    vec![demo_meta(), demo_meta_reissue()]
}

/// A tiny 1x1 PNG used as the simulated disc's cover art.
pub fn demo_cover() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0x64,
        0x60, 0xF8, 0x9F, 0x0F, 0x00, 0x02, 0x87, 0x01, 0x80, 0xEB, 0x47, 0xBA, 0x92, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

/// A [`FrameSource`] producing a 440 Hz sine wave, so demo rips yield
/// playable WAV files.
#[derive(Debug)]
pub struct DemoSource {
    chunk_delay: Duration,
    next_sample: u64,
}

impl Default for DemoSource {
    fn default() -> Self {
        Self::new()
    }
}

impl DemoSource {
    /// Creates a source with the standard simulated read latency.
    pub fn new() -> Self {
        Self::with_delay(DemoDisc::new().chunk_delay)
    }

    /// Creates a source with a specific per-chunk latency.
    pub fn with_delay(chunk_delay: Duration) -> Self {
        Self {
            chunk_delay,
            next_sample: 0,
        }
    }
}

impl FrameSource for DemoSource {
    fn read_frames(&mut self, _lba: u32, frames: u32, buf: &mut [u8]) -> io::Result<()> {
        for (i, chunk) in buf.chunks_exact_mut(2).enumerate() {
            let sample = self.next_sample + i as u64;
            let phase = sample as f64 * (2.0 * std::f64::consts::PI * DEMO_FREQ / 44_100.0);
            let value = (phase.sin() * DEMO_AMPLITUDE) as i16;
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        self.next_sample += buf.len() as u64 / 2;
        if frames > 0 {
            std::thread::sleep(self.chunk_delay * frames / FRAMES_PER_SECOND);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rend_core::FRAME_SIZE;

    #[test]
    fn toc_shape() {
        let disc = DemoDisc::new();
        let toc = &disc.toc;

        assert_eq!(toc.tracks.len(), 5);
        assert!(toc.tracks.iter().all(|t| t.is_audio()));
        assert_eq!(toc.leadout_lba, 10500);
        for (a, b) in toc.tracks.iter().zip(toc.tracks.iter().skip(1)) {
            assert!(a.start_lba < b.start_lba);
            assert_eq!(toc.end_lba(a.number), Some(b.start_lba));
        }
        assert_eq!(toc.end_lba(5), Some(10500));
        assert_eq!(toc.frames_remaining(0), 10500);
    }

    #[test]
    fn meta_matches_the_toc() {
        let toc = DemoDisc::new().toc;
        let meta = demo_meta();
        assert_eq!(meta.tracks.len(), toc.audio_tracks().count());
        assert!(demo_cover().starts_with(&[0x89, 0x50, 0x4E, 0x47]));
    }

    #[test]
    fn source_is_deterministic() {
        let mut a = DemoSource::with_delay(Duration::ZERO);
        let mut b = DemoSource::with_delay(Duration::ZERO);
        let mut buf_a = vec![0u8; FRAME_SIZE * 10];
        let mut buf_b = vec![0u8; FRAME_SIZE * 10];

        a.read_frames(0, 10, &mut buf_a).unwrap();
        b.read_frames(0, 10, &mut buf_b).unwrap();

        assert_eq!(buf_a, buf_b);
        assert!(buf_a.iter().any(|&v| v != 0));
    }

    #[test]
    fn sine_wave_is_not_silence() {
        let mut src = DemoSource::with_delay(Duration::ZERO);
        let mut buf = vec![0u8; FRAME_SIZE * 5];
        src.read_frames(0, 5, &mut buf).unwrap();

        let peak = buf
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs() as u32)
            .max()
            .unwrap();
        assert!(peak > 1000);
    }
}
