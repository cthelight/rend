//! Background ripping: reads CDDA frames on a worker thread and reports
//! progress over an [`std::sync::mpsc`] channel.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

use rend_core::{CddaStream, Device, FRAME_SIZE, FRAMES_PER_SECOND, FrameSource, Toc, WavWriter};

use crate::demo::DemoSource;

/// Progress events emitted by the rip worker.
#[derive(Debug, Clone)]
pub enum RipEvent {
    /// A track started being read.
    TrackStarted { number: u8 },
    /// A track was skipped (e.g. its output file exists and force is off).
    TrackSkipped { number: u8, reason: String },
    /// A track produced more data.
    Progress {
        number: u8,
        bytes_done: u64,
        bytes_total: u64,
    },
    /// A track finished and its WAV file was written.
    TrackDone {
        number: u8,
        path: PathBuf,
        bytes: u64,
    },
    /// A track failed.
    TrackFailed { number: u8, error: String },
    /// All requested tracks were attempted.
    Finished {
        failed: usize,
        total: usize,
        stopped: bool,
    },
}

/// What the worker reads frames from.
pub enum RipSource {
    /// A real CD-ROM device (opened on the worker thread).
    Device(Device),
    /// The simulated disc.
    Demo(DemoSource),
}

impl FrameSource for RipSource {
    fn read_frames(&mut self, lba: u32, frames: u32, buf: &mut [u8]) -> io::Result<()> {
        match self {
            Self::Device(device) => device.read_frames(lba, frames, buf),
            Self::Demo(source) => source.read_frames(lba, frames, buf),
        }
    }
}

impl FrameSource for &mut RipSource {
    fn read_frames(&mut self, lba: u32, frames: u32, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_frames(lba, frames, buf)
    }
}

/// Everything the worker needs to know about a rip job.
pub struct RipJob {
    pub source: RipSource,
    pub toc: Toc,
    pub tracks: Vec<u8>,
    pub out_dir: PathBuf,
    pub force: bool,
    /// Set to make the worker stop between chunks.
    pub stop: Arc<AtomicBool>,
}

/// Spawns the rip worker.
pub fn spawn(job: RipJob, tx: Sender<RipEvent>) -> JoinHandle<()> {
    thread::Builder::new()
        .name("rend-rip".into())
        .spawn(move || worker(job, tx))
        .expect("failed to spawn rip worker thread")
}

fn worker(mut job: RipJob, tx: Sender<RipEvent>) {
    if let RipSource::Device(device) = &mut job.source {
        device.spin_up().ok();
    }

    let tracks = job.tracks.clone();
    let mut failed = 0usize;
    let mut stopped = false;
    for &number in &tracks {
        if job.stop.load(Ordering::Relaxed) {
            stopped = true;
            break;
        }
        if let Err(error) = rip_track(&mut job, number, &tx) {
            tx.send(RipEvent::TrackFailed { number, error }).ok();
            failed += 1;
        }
    }

    if let RipSource::Device(device) = &mut job.source {
        device.spin_down().ok();
    }

    tx.send(RipEvent::Finished {
        failed,
        total: job.tracks.len(),
        stopped,
    })
    .ok();
}

fn rip_track(job: &mut RipJob, number: u8, tx: &Sender<RipEvent>) -> Result<(), String> {
    let track = match job.toc.track(number) {
        Some(track) => track,
        None => return Err(format!("track {number} not found on disc")),
    };
    let end = job.toc.end_lba(number).unwrap_or(job.toc.leadout_lba);
    let frames = track.frames(end);
    let path = job.out_dir.join(format!("track{number:02}.wav"));

    if path.exists() && !job.force {
        tx.send(RipEvent::TrackSkipped {
            number,
            reason: "output exists (force is off)".into(),
        })
        .ok();
        return Ok(());
    }

    std::fs::create_dir_all(&job.out_dir).map_err(|e| e.to_string())?;

    tx.send(RipEvent::TrackStarted { number }).ok();
    match read_track(
        &mut job.source,
        number,
        track.start_lba,
        frames,
        &path,
        tx,
        &job.stop,
    ) {
        Ok(bytes) => {
            tx.send(RipEvent::TrackDone {
                number,
                path,
                bytes,
            })
            .ok();
            Ok(())
        }
        Err(e) => {
            std::fs::remove_file(&path).ok();
            Err(e.to_string())
        }
    }
}

fn read_track(
    source: &mut RipSource,
    number: u8,
    lba: u32,
    frames: u32,
    path: &Path,
    tx: &Sender<RipEvent>,
    stop: &AtomicBool,
) -> io::Result<u64> {
    let mut stream = CddaStream::new(source, lba, frames);
    let mut wav = WavWriter::create(path)?;
    let total = stream.total_bytes() as u64;
    let mut buf = vec![0u8; FRAMES_PER_SECOND as usize * FRAME_SIZE];
    let mut done = 0u64;

    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "ripping stopped",
            ));
        }
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        wav.write(&buf[..n])?;
        done += n as u64;
        tx.send(RipEvent::Progress {
            number,
            bytes_done: done,
            bytes_total: total,
        })
        .ok();
    }

    wav.finish()?;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    use crate::demo::DemoDisc;

    fn run_job(tracks: Vec<u8>, out_dir: PathBuf, force: bool) -> Vec<RipEvent> {
        let (tx, rx) = mpsc::channel();
        let job = RipJob {
            source: RipSource::Demo(DemoSource::with_delay(Duration::ZERO)),
            toc: DemoDisc::new().toc,
            tracks,
            out_dir,
            force,
            stop: Arc::new(AtomicBool::new(false)),
        };
        let handle = spawn(job, tx);
        let mut events = Vec::new();
        for event in rx {
            let finished = matches!(event, RipEvent::Finished { .. });
            events.push(event);
            if finished {
                break;
            }
        }
        handle.join().unwrap();
        events
    }

    #[test]
    fn rips_a_demo_track_to_wav() {
        let dir = tempfile::tempdir().unwrap();
        let events = run_job(vec![3], dir.path().to_path_buf(), false);

        let expected = dir.path().join("track03.wav");
        assert!(expected.exists());
        let bytes = std::fs::read(&expected).unwrap();
        assert_eq!(bytes.len(), 44 + 150 * FRAME_SIZE);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");

        let mut seen_started = false;
        let mut seen_done = false;
        for event in &events {
            match event {
                RipEvent::TrackStarted { number } => {
                    seen_started = true;
                    assert_eq!(*number, 3);
                }
                RipEvent::Progress {
                    number,
                    bytes_done,
                    bytes_total,
                } => {
                    assert_eq!(*number, 3);
                    assert!(*bytes_done <= *bytes_total);
                }
                RipEvent::TrackDone {
                    number,
                    path,
                    bytes,
                } => {
                    seen_done = true;
                    assert_eq!(*number, 3);
                    assert_eq!(path.as_path(), expected.as_path());
                    assert_eq!(*bytes, (150 * FRAME_SIZE) as u64);
                }
                RipEvent::Finished {
                    failed,
                    total,
                    stopped,
                } => {
                    assert_eq!(*failed, 0);
                    assert_eq!(*total, 1);
                    assert!(!*stopped);
                }
                other => panic!("unexpected event {other:?}"),
            }
        }
        assert!(seen_started && seen_done);
        assert!(matches!(
            events.last(),
            Some(RipEvent::Finished {
                failed: 0,
                total: 1,
                stopped: false
            })
        ));
    }

    #[test]
    fn skips_existing_output_without_force() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("track03.wav"), b"old").unwrap();

        let events = run_job(vec![3], dir.path().to_path_buf(), false);

        assert!(
            events
                .iter()
                .any(|e| matches!(e, RipEvent::TrackSkipped { number: 3, .. }))
        );
        assert!(matches!(
            events.last(),
            Some(RipEvent::Finished { failed: 0, .. })
        ));
        assert_eq!(
            std::fs::read(dir.path().join("track03.wav")).unwrap(),
            b"old"
        );
    }

    #[test]
    fn force_overwrites_existing_output() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("track03.wav"), b"old").unwrap();

        let events = run_job(vec![3], dir.path().to_path_buf(), true);

        assert!(
            events
                .iter()
                .any(|e| matches!(e, RipEvent::TrackDone { number: 3, .. }))
        );
        assert_eq!(
            std::fs::read(dir.path().join("track03.wav")).unwrap().len(),
            44 + 150 * FRAME_SIZE
        );
    }

    #[test]
    fn unknown_track_fails() {
        let dir = tempfile::tempdir().unwrap();
        let events = run_job(vec![9], dir.path().to_path_buf(), false);

        assert!(
            events
                .iter()
                .any(|e| matches!(e, RipEvent::TrackFailed { number: 9, .. }))
        );
        assert!(matches!(
            events.last(),
            Some(RipEvent::Finished {
                failed: 1,
                total: 1,
                stopped: false
            })
        ));
    }
}
