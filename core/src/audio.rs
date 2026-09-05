//! Raw CDDA (Red Book audio) streaming.
//!
//! A CD audio track is a sequence of raw frames of 2352 bytes each:
//! 16-bit signed PCM, 2 channels, 44 100 Hz sample rate (588 samples per
//! frame at 75 frames per second). The kernel's `CDROMREADAUDIO` ioctl
//! reads 1..=75 frames per call, so [`CddaStream`] chunks reads at that
//! granularity while presenting a plain [`std::io::Read`] interface.

use std::io::{self, Read};

use libc::c_char;

use crate::device::Device;
use crate::error::Result;
use crate::sys::{
    self, CDROM_AUDIO_COMPLETED, CDROM_AUDIO_ERROR, CDROM_AUDIO_INVALID, CDROM_AUDIO_NO_STATUS,
    CDROM_AUDIO_PAUSED, CDROM_AUDIO_PLAY, CDROM_LBA, Cdrom_read_audio, Cdrom_subchnl, ioctl_with,
};

/// Number of raw frames per second.
pub const FRAMES_PER_SECOND: u32 = sys::CD_FRAMES;
/// Bytes per raw frame.
pub const FRAME_SIZE: usize = sys::CD_FRAMESIZE_RAW;
/// Maximum frames the kernel reads per ioctl call.
const MAX_CHUNK: u32 = sys::CD_FRAMES;

/// A source of raw CDDA frames.
pub trait FrameSource {
    /// Reads `frames` raw frames starting at LBA `lba` into `buf`.
    ///
    /// `frames` is in 1..=`MAX_CHUNK` and `buf` has at least
    /// `frames * FRAME_SIZE` bytes.
    fn read_frames(&mut self, lba: u32, frames: u32, buf: &mut [u8]) -> io::Result<()>;
}

impl FrameSource for Device {
    fn read_frames(&mut self, lba: u32, frames: u32, buf: &mut [u8]) -> io::Result<()> {
        debug_assert!((1..=MAX_CHUNK).contains(&frames));
        debug_assert!(buf.len() >= frames as usize * FRAME_SIZE);
        let mut req =
            Cdrom_read_audio::new_lba(lba, frames as i32, buf.as_mut_ptr().cast::<c_char>());
        ioctl_with(self.file(), sys::CDROMREADAUDIO, &mut req)
    }
}

impl FrameSource for &mut Device {
    fn read_frames(&mut self, lba: u32, frames: u32, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_frames(lba, frames, buf)
    }
}

/// A stream of raw CDDA frames, implementing [`std::io::Read`].
///
/// The stream starts at a given LBA and produces exactly `total_frames`
/// frames (typically the frame count of one track). Reads are served from
/// the underlying [`FrameSource`] in chunks of up to [`MAX_CHUNK`] frames.
pub struct CddaStream<S: FrameSource> {
    source: S,
    next_lba: u32,
    total_frames: u32,
    remaining: u32,
}

impl<S: FrameSource> CddaStream<S> {
    /// Creates a stream over `total_frames` frames starting at `lba`.
    pub fn new(source: S, lba: u32, total_frames: u32) -> Self {
        Self {
            source,
            next_lba: lba,
            total_frames,
            remaining: total_frames,
        }
    }

    /// The full length of the stream in frames.
    pub fn total_frames(&self) -> u32 {
        self.total_frames
    }

    /// Frames not yet read.
    pub fn frames_remaining(&self) -> u32 {
        self.remaining
    }

    /// LBA of the next frame to be read.
    pub fn next_lba(&self) -> u32 {
        self.next_lba
    }

    /// Total output length in bytes.
    pub fn total_bytes(&self) -> usize {
        self.total_frames as usize * FRAME_SIZE
    }
}

impl<S: FrameSource> Read for CddaStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        if buf.len() < FRAME_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("buffer must hold at least one frame ({FRAME_SIZE} bytes)"),
            ));
        }
        let nframes = self
            .remaining
            .min(buf.len() as u32 / FRAME_SIZE as u32)
            .min(MAX_CHUNK);
        let n = nframes as usize * FRAME_SIZE;
        self.source
            .read_frames(self.next_lba, nframes, &mut buf[..n])?;
        self.next_lba += nframes;
        self.remaining -= nframes;
        Ok(n)
    }
}

/// Audio playback status (from `cdrom_subchnl.cdsc_audiostatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioStatus {
    /// The drive does not report audio status.
    Invalid,
    /// Playback is in progress.
    Playing,
    /// Playback is paused.
    Paused,
    /// Playback completed successfully.
    Completed,
    /// Playback stopped due to an error.
    Error,
    /// No current audio status.
    NoStatus,
}

impl AudioStatus {
    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            CDROM_AUDIO_INVALID => Self::Invalid,
            CDROM_AUDIO_PLAY => Self::Playing,
            CDROM_AUDIO_PAUSED => Self::Paused,
            CDROM_AUDIO_COMPLETED => Self::Completed,
            CDROM_AUDIO_ERROR => Self::Error,
            CDROM_AUDIO_NO_STATUS => Self::NoStatus,
            _ => return None,
        })
    }
}

/// Subchannel (playback position) information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subchannel {
    /// Current audio status.
    pub status: AudioStatus,
    /// Current track number.
    pub track: u8,
    /// Current index number.
    pub index: u8,
    /// Absolute position on the disc (LBA).
    pub abs_lba: u32,
    /// Position relative to the current track start (LBA).
    pub rel_lba: u32,
}

impl Device {
    /// Queries the subchannel (current playback position and status).
    pub fn subchannel(&self) -> Result<Subchannel> {
        let mut q = Cdrom_subchnl::new(CDROM_LBA);
        ioctl_with(self.file(), sys::CDROMSUBCHNL, &mut q)?;
        Ok(Subchannel {
            status: AudioStatus::from_code(q.audiostatus).unwrap_or(AudioStatus::Invalid),
            track: q.trk,
            index: q.ind,
            abs_lba: q.absaddr,
            rel_lba: q.reladdr,
        })
    }
}

#[cfg(test)]
mod subchannel_tests {
    use super::*;

    #[test]
    fn audio_status_codes() {
        assert_eq!(AudioStatus::from_code(0x00), Some(AudioStatus::Invalid));
        assert_eq!(AudioStatus::from_code(0x11), Some(AudioStatus::Playing));
        assert_eq!(AudioStatus::from_code(0x12), Some(AudioStatus::Paused));
        assert_eq!(AudioStatus::from_code(0x13), Some(AudioStatus::Completed));
        assert_eq!(AudioStatus::from_code(0x14), Some(AudioStatus::Error));
        assert_eq!(AudioStatus::from_code(0x15), Some(AudioStatus::NoStatus));
        assert_eq!(AudioStatus::from_code(0x16), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake source producing deterministic PCM: each byte is a
    /// position-dependent counter, so content is checkable without a disc.
    #[derive(Debug, Default)]
    struct FakeSource;

    impl FakeSource {
        fn new() -> Self {
            Self
        }
    }

    fn expected_byte(lba: u32, offset: usize) -> u8 {
        // Deterministic but position-dependent pattern.
        ((lba * 131 + offset as u32) % 251) as u8
    }

    impl FrameSource for FakeSource {
        fn read_frames(&mut self, lba: u32, frames: u32, buf: &mut [u8]) -> io::Result<()> {
            assert!((1..=MAX_CHUNK).contains(&frames));
            for f in 0..frames as usize {
                let start = f * FRAME_SIZE;
                for (i, b) in buf[start..start + FRAME_SIZE].iter_mut().enumerate() {
                    *b = expected_byte(lba + f as u32, i);
                }
            }
            Ok(())
        }
    }

    fn fill(start_lba: u32, total_frames: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(total_frames as usize * FRAME_SIZE);
        for lba in start_lba..start_lba + total_frames {
            for i in 0..FRAME_SIZE {
                out.push(expected_byte(lba, i));
            }
        }
        out
    }

    fn read_all(stream: &mut CddaStream<FakeSource>, out: &mut Vec<u8>) {
        let mut buf = [0u8; MAX_CHUNK as usize * FRAME_SIZE];
        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
    }

    #[test]
    fn reads_exact_track() {
        let mut stream = CddaStream::new(FakeSource::new(), 100, 200);
        let expected = fill(100, 200);

        let mut out = Vec::new();
        read_all(&mut stream, &mut out);

        assert_eq!(out, expected);
        assert_eq!(stream.frames_remaining(), 0);
    }

    #[test]
    fn respects_lba_start() {
        let mut stream = CddaStream::new(FakeSource::new(), 75, 10);
        let expected = (0..10u32)
            .flat_map(|f| (0..FRAME_SIZE).map(move |i| expected_byte(75 + f, i)))
            .collect::<Vec<_>>();

        let mut out = Vec::new();
        read_all(&mut stream, &mut out);
        assert_eq!(out, expected);
    }

    #[test]
    fn chunks_at_kernel_limit() {
        // 200 frames into a 75-frame buffer -> reads of 75, 75, 50.
        let mut stream = CddaStream::new(FakeSource::new(), 0, 200);
        let mut buf = vec![0u8; 75 * FRAME_SIZE];

        assert_eq!(stream.read(&mut buf).unwrap(), 75 * FRAME_SIZE);
        assert_eq!(stream.read(&mut buf).unwrap(), 75 * FRAME_SIZE);
        assert_eq!(stream.read(&mut buf).unwrap(), 50 * FRAME_SIZE);
        assert_eq!(stream.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn respects_small_buffers() {
        // 100 frames into a 10-frame buffer -> ten reads of 10 frames.
        let mut stream = CddaStream::new(FakeSource::new(), 0, 100);
        let mut buf = vec![0u8; 10 * FRAME_SIZE];

        for _ in 0..10 {
            assert_eq!(stream.read(&mut buf).unwrap(), 10 * FRAME_SIZE);
        }
        assert_eq!(stream.read(&mut buf).unwrap(), 0);
        assert_eq!(stream.frames_remaining(), 0);
    }

    #[test]
    fn eof_reports_zero() {
        let mut stream = CddaStream::new(FakeSource::new(), 0, 0);
        let mut buf = [0u8; FRAME_SIZE];
        assert_eq!(stream.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn undersized_buffer_errors() {
        let mut stream = CddaStream::new(FakeSource::new(), 0, 10);
        let mut buf = [0u8; FRAME_SIZE - 1];
        assert!(stream.read(&mut buf).is_err());
    }

    #[test]
    fn next_lba_advances() {
        let mut stream = CddaStream::new(FakeSource::new(), 1000, 150);
        assert_eq!(stream.next_lba(), 1000);
        let mut buf = vec![0u8; FRAME_SIZE * 80];
        // Reads are capped at MAX_CHUNK (75) frames even with a larger buffer.
        let chunk = MAX_CHUNK as usize * FRAME_SIZE;
        assert_eq!(stream.read(&mut buf).unwrap(), chunk);
        assert_eq!(stream.next_lba(), 1075);
        assert_eq!(stream.read(&mut buf).unwrap(), chunk);
        assert_eq!(stream.next_lba(), 1150);
        assert_eq!(stream.frames_remaining(), 0);
    }

    #[test]
    fn total_bytes() {
        let stream = CddaStream::new(FakeSource::new(), 0, 75);
        assert_eq!(stream.total_bytes(), 75 * FRAME_SIZE);
        assert_eq!(stream.total_frames(), 75);
    }
}
