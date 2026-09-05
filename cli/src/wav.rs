//! Streaming WAV (PCM) file writer.
//!
//! The RIFF header is written up front with placeholder sizes and patched
//! in place by [`WavWriter::finish`], so arbitrarily long tracks can be
//! written without buffering them in memory.

use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BITS_PER_SAMPLE: u16 = 16;
const BLOCK_ALIGN: u16 = CHANNELS * (BITS_PER_SAMPLE / 8);
const BYTE_RATE: u32 = SAMPLE_RATE * BLOCK_ALIGN as u32;

/// A writer for 16-bit stereo 44.1 kHz PCM WAV files.
pub struct WavWriter {
    file: File,
    data_size: u64,
}

impl WavWriter {
    /// Creates a new WAV file, writing the (placeholder) header.
    pub fn create(path: &Path) -> io::Result<Self> {
        let mut file = File::create(path)?;
        file.write_all(&Self::header(0))?;
        Ok(Self { file, data_size: 0 })
    }

    /// Appends raw PCM samples.
    pub fn write(&mut self, pcm: &[u8]) -> io::Result<()> {
        self.file.write_all(pcm)?;
        self.data_size += pcm.len() as u64;
        Ok(())
    }

    /// Writes the final header with the correct sizes and flushes.
    ///
    /// The writer must not be used afterwards.
    pub fn finish(mut self) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&Self::header(self.data_size))?;
        self.file.flush()
    }

    fn header(data_size: u64) -> [u8; 44] {
        let mut h = [0u8; 44];
        h[0..4].copy_from_slice(b"RIFF");
        h[4..8].copy_from_slice(&((36 + data_size) as u32).to_le_bytes());
        h[8..12].copy_from_slice(b"WAVE");
        h[12..16].copy_from_slice(b"fmt ");
        h[16..20].copy_from_slice(&16u32.to_le_bytes()); // fmt chunk size
        h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
        h[22..24].copy_from_slice(&CHANNELS.to_le_bytes());
        h[24..28].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
        h[28..32].copy_from_slice(&BYTE_RATE.to_le_bytes());
        h[32..34].copy_from_slice(&BLOCK_ALIGN.to_le_bytes());
        h[34..36].copy_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
        h[36..40].copy_from_slice(b"data");
        h[40..44].copy_from_slice(&(data_size as u32).to_le_bytes());
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_header_and_payload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wav");
        let pcm = vec![0xA5u8; 44_100 * 4]; // one second of silence-ish

        {
            let mut w = WavWriter::create(&path).unwrap();
            w.write(&pcm[..1000]).unwrap();
            w.write(&pcm[1000..]).unwrap();
            w.finish().unwrap();
        }

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44 + pcm.len());
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            36 + pcm.len() as u32
        );
        assert_eq!(&bytes[20..22], &[1, 0]); // PCM
        assert_eq!(&bytes[22..24], &[2, 0]); // channels
        assert_eq!(&bytes[24..28], &44_100u32.to_le_bytes());
        assert_eq!(&bytes[28..32], &176_400u32.to_le_bytes()); // byte rate
        assert_eq!(&bytes[32..34], &[4, 0]); // block align
        assert_eq!(&bytes[34..36], &[16, 0]); // bits per sample
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(
            u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
            pcm.len() as u32
        );
        assert_eq!(&bytes[44..], &pcm[..]);
    }

    #[test]
    fn empty_file_is_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.wav");
        WavWriter::create(&path).unwrap().finish().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 36);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 0);
    }
}
