//! FLAC output: raw PCM streamed into an `ffmpeg` process.
//!
//! The track is encoded in a single pass — PCM is written to ffmpeg's stdin
//! pipe while it writes the finished FLAC file itself, so no intermediate
//! WAV ever touches the disk. `ffmpeg` is the one external tool rend needs;
//! it is invoked with long-supported flags only.

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

/// The external encoder used for FLAC output.
pub const FFMPEG: &str = "ffmpeg";

/// The highest FLAC compression level (the scale runs 0–8).
const COMPRESSION_LEVEL: &str = "8";

/// A FLAC file being written by an ffmpeg process fed over stdin.
pub struct FlacWriter {
    child: Child,
    stdin: Option<ChildStdin>,
    stderr: Arc<Mutex<Vec<u8>>>,
}

impl FlacWriter {
    /// Spawns ffmpeg, ready to receive raw PCM (s16le, 44 100 Hz, stereo) on
    /// stdin and write a FLAC file at `path`.
    pub fn create(path: &Path) -> io::Result<Self> {
        let mut child = Command::new(FFMPEG)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "s16le",
                "-ar",
                "44100",
                "-ac",
                "2",
                "-i",
                "pipe:0",
                "-c:a",
                "flac",
                "-compression_level",
                COMPRESSION_LEVEL,
                "-y",
            ])
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "ffmpeg not found in PATH — install it, or rip with --format wav",
                    )
                } else {
                    e
                }
            })?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let stderr_buf = Arc::new(Mutex::new(Vec::new()));
        let buf = stderr_buf.clone();
        let drain = thread::Builder::new().name("rend-ffmpeg-stderr".into());
        drain
            .spawn(move || {
                let mut out = Vec::new();
                stderr.read_to_end(&mut out).ok();
                *buf.lock().unwrap() = out;
            })
            .ok();
        Ok(Self {
            child,
            stdin: Some(stdin),
            stderr: stderr_buf,
        })
    }

    /// Appends raw PCM samples to the encoder.
    pub fn write(&mut self, pcm: &[u8]) -> io::Result<()> {
        self.stdin
            .as_mut()
            .expect("stdin stays open until finish")
            .write_all(pcm)
    }

    /// Closes the input, waits for ffmpeg to exit, and reports any failure.
    ///
    /// The writer must not be used afterwards.
    pub fn finish(mut self) -> io::Result<()> {
        self.stdin.take();
        let status = self.child.wait()?;
        let stderr = String::from_utf8_lossy(&self.stderr.lock().unwrap()).into_owned();
        if !status.success() {
            let detail = stderr.trim();
            return Err(io::Error::other(if detail.is_empty() {
                format!("ffmpeg exited with {status}")
            } else {
                format!("ffmpeg failed ({status}): {detail}")
            }));
        }
        Ok(())
    }
}

impl Drop for FlacWriter {
    /// Kills a still-running encoder (e.g. the track was stopped mid-read).
    fn drop(&mut self) {
        self.child.kill().ok();
    }
}
