//! Audio output through an `ffmpeg` process.
//!
//! The track is encoded in a single pass — raw PCM is written to ffmpeg's
//! stdin pipe while it writes the finished file itself, so no intermediate
//! file ever touches the disk. The codec and its quality arguments are
//! supplied by the caller, so one writer serves every ffmpeg-backed format.

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

/// The external encoder used for every ffmpeg-backed output format.
pub const FFMPEG: &str = "ffmpeg";

/// A file being written by an ffmpeg process fed over stdin.
pub struct FfmpegWriter {
    child: Child,
    stdin: Option<ChildStdin>,
    stderr: Arc<Mutex<Vec<u8>>>,
}

impl FfmpegWriter {
    /// Spawns ffmpeg, ready to receive raw PCM (s16le, 44 100 Hz, stereo) on
    /// stdin and write a file at `path` using `encoder` and `quality_args`.
    pub fn create(path: &Path, encoder: &str, quality_args: Vec<String>) -> io::Result<Self> {
        let mut args: Vec<String> = vec![
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-f".to_string(),
            "s16le".to_string(),
            "-ar".to_string(),
            "44100".to_string(),
            "-ac".to_string(),
            "2".to_string(),
            "-i".to_string(),
            "pipe:0".to_string(),
            "-c:a".to_string(),
            encoder.to_string(),
        ];
        args.extend(quality_args);
        args.push("-y".to_string());

        let mut command = Command::new(FFMPEG);
        for arg in &args {
            command.arg(arg);
        }
        let mut child = command
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

impl Drop for FfmpegWriter {
    /// Kills a still-running encoder (e.g. the track was stopped mid-read).
    fn drop(&mut self) {
        self.child.kill().ok();
    }
}
