//! Background ripping: reads CDDA frames on a worker thread and reports
//! progress over an [`std::sync::mpsc`] channel.

use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

use rend_core::{CddaStream, Device, FRAME_SIZE, FRAMES_PER_SECOND, FrameSource, Toc, Track};
use rend_encode::Format;
use rend_meta::{DiscMeta, Template, TrackTags, apply};

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
    /// A track finished and its output file was written.
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
    /// The format the track files are written in.
    pub format: Format,
    /// The naming template the track files are laid out with.
    pub template: Template,
    pub force: bool,
    /// Set to make the worker stop between chunks.
    pub stop: Arc<AtomicBool>,
    /// The disc's looked-up metadata, to embed in the track files.
    pub meta: Option<DiscMeta>,
    /// The disc's cover art, if any, to embed in the track files.
    pub cover: Option<Vec<u8>>,
    /// The disc's catalog number (MCN), if the drive reported one.
    pub catalog_number: Option<String>,
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

/// Everything needed to read and encode a single track.
struct TrackSpec {
    number: u8,
    lba: u32,
    frames: u32,
    path: PathBuf,
    format: Format,
}

impl TrackSpec {
    fn new(job: &RipJob, track: &Track, frames: u32) -> Self {
        // With looked-up metadata the file path comes from the naming
        // template; without it the flat `trackNN` name is kept.
        let path = match &job.meta {
            Some(disc) => {
                let position = job
                    .toc
                    .audio_tracks()
                    .position(|t| t.number == track.number)
                    .map(|i| i + 1)
                    .unwrap_or(1);
                job.template.track_path(
                    &job.out_dir,
                    disc,
                    track.number,
                    position,
                    job.format.extension(),
                )
            }
            None => job.out_dir.join(format!(
                "track{:02}.{}",
                track.number,
                job.format.extension()
            )),
        };
        Self {
            number: track.number,
            lba: track.start_lba,
            frames,
            path,
            format: job.format,
        }
    }
}

fn rip_track(job: &mut RipJob, number: u8, tx: &Sender<RipEvent>) -> Result<(), String> {
    let track = match job.toc.track(number) {
        Some(track) => track,
        None => return Err(format!("track {number} not found on disc")),
    };
    let end = job.toc.end_lba(number).unwrap_or(job.toc.leadout_lba);
    let spec = TrackSpec::new(job, track, track.frames(end));

    if spec.path.exists() && !job.force {
        tx.send(RipEvent::TrackSkipped {
            number,
            reason: "output exists (force is off)".into(),
        })
        .ok();
        return Ok(());
    }

    if let Some(parent) = spec.path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    tx.send(RipEvent::TrackStarted { number }).ok();
    match read_track(&mut job.source, &spec, tx, &job.stop) {
        Ok(bytes) => {
            tag_track(job, &spec);
            let bytes = std::fs::metadata(&spec.path)
                .map(|m| m.len())
                .unwrap_or(bytes);
            tx.send(RipEvent::TrackDone {
                number,
                path: spec.path,
                bytes,
            })
            .ok();
            Ok(())
        }
        Err(e) => {
            std::fs::remove_file(&spec.path).ok();
            Err(e.to_string())
        }
    }
}

/// Embeds the looked-up metadata and cover art into a finished track file.
/// Best effort: a tagging failure never fails the rip.
fn tag_track(job: &RipJob, spec: &TrackSpec) {
    let Some(disc) = &job.meta else {
        return;
    };
    let total = job.toc.audio_tracks().count();
    let Some(position) = job
        .toc
        .audio_tracks()
        .position(|t| t.number == spec.number)
        .map(|i| i + 1)
    else {
        return;
    };
    let Some(mut tags) = TrackTags::for_track(disc, position, total) else {
        return;
    };
    tags.catalog_number = job.catalog_number.clone();
    let _ = apply(&spec.path, &tags, job.cover.as_deref());
}

fn read_track(
    source: &mut RipSource,
    spec: &TrackSpec,
    tx: &Sender<RipEvent>,
    stop: &AtomicBool,
) -> io::Result<u64> {
    let mut stream = CddaStream::new(source, spec.lba, spec.frames);
    let mut file = spec.format.create_file(&spec.path)?;
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
        file.write(&buf[..n])?;
        done += n as u64;
        tx.send(RipEvent::Progress {
            number: spec.number,
            bytes_done: done,
            bytes_total: total,
        })
        .ok();
    }

    file.finish()?;
    Ok(std::fs::metadata(&spec.path)?.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;

    use crate::demo::DemoDisc;

    fn run_job(tracks: Vec<u8>, out_dir: PathBuf, force: bool, format: Format) -> Vec<RipEvent> {
        let (tx, rx) = mpsc::channel();
        let job = RipJob {
            source: RipSource::Demo(DemoSource::with_delay(Duration::ZERO)),
            toc: DemoDisc::new().toc,
            tracks,
            out_dir,
            format,
            template: Template::default(),
            force,
            stop: Arc::new(AtomicBool::new(false)),
            meta: None,
            cover: None,
            catalog_number: None,
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
    fn rips_a_demo_track_to_flac_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let events = run_job(vec![3], dir.path().to_path_buf(), false, Format::default());

        let expected = dir.path().join("track03.flac");
        assert!(expected.exists());
        let file_bytes = std::fs::read(&expected).unwrap();
        assert_eq!(&file_bytes[0..4], b"fLaC");

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
                    assert_eq!(*bytes, file_bytes.len() as u64);
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
    fn rips_a_demo_track_to_wav_when_selected() {
        let dir = tempfile::tempdir().unwrap();
        let events = run_job(vec![3], dir.path().to_path_buf(), false, Format::Wav);

        let expected = dir.path().join("track03.wav");
        assert!(expected.exists());
        let bytes = std::fs::read(&expected).unwrap();
        assert_eq!(bytes.len(), 44 + 150 * FRAME_SIZE);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");

        assert!(matches!(
            events
                .iter()
                .find(|e| matches!(e, RipEvent::TrackDone { .. })),
            Some(RipEvent::TrackDone { number, bytes, .. })
                if *number == 3 && *bytes == (44 + 150 * FRAME_SIZE) as u64
        ));
    }

    #[test]
    fn tags_the_file_when_the_job_carrying_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let job = RipJob {
            source: RipSource::Demo(DemoSource::with_delay(Duration::ZERO)),
            toc: DemoDisc::new().toc,
            tracks: vec![3],
            out_dir: dir.path().to_path_buf(),
            format: Format::default(),
            template: Template::default(),
            force: false,
            stop: Arc::new(AtomicBool::new(false)),
            meta: Some(crate::demo::demo_meta()),
            cover: Some(crate::demo::demo_cover()),
            catalog_number: Some(crate::demo::DEMO_MCN.into()),
        };
        let handle = spawn(job, tx);
        for _ in rx {}
        handle.join().unwrap();

        let path = dir
            .path()
            .join("The_Demo_Band/Demo_Album/03_Short_One.flac");
        let file = lofty::read_from_path(&path).unwrap();
        let tag = file
            .tag(lofty::tag::TagType::VorbisComments)
            .expect("vorbis comments were written");
        assert_eq!(
            tag.title().map(std::borrow::Cow::into_owned).as_deref(),
            Some("Short One")
        );
        assert_eq!(
            tag.artist().map(std::borrow::Cow::into_owned).as_deref(),
            Some("Guest Artist")
        );
        assert_eq!(
            tag.get_string(lofty::tag::ItemKey::CatalogNumber),
            Some(crate::demo::DEMO_MCN)
        );
        let pic = tag
            .get_picture_type(lofty::picture::PictureType::CoverFront)
            .unwrap();
        assert_eq!(pic.data(), crate::demo::demo_cover());
    }

    #[test]
    fn skips_existing_output_without_force() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("track03.flac"), b"old").unwrap();

        let events = run_job(vec![3], dir.path().to_path_buf(), false, Format::default());

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
            std::fs::read(dir.path().join("track03.flac")).unwrap(),
            b"old"
        );
    }

    #[test]
    fn force_overwrites_existing_output() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("track03.flac"), b"old").unwrap();

        let events = run_job(vec![3], dir.path().to_path_buf(), true, Format::default());

        assert!(
            events
                .iter()
                .any(|e| matches!(e, RipEvent::TrackDone { number: 3, .. }))
        );
        let bytes = std::fs::read(dir.path().join("track03.flac")).unwrap();
        assert_eq!(&bytes[0..4], b"fLaC");
    }

    #[test]
    fn unknown_track_fails() {
        let dir = tempfile::tempdir().unwrap();
        let events = run_job(vec![9], dir.path().to_path_buf(), false, Format::default());

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
