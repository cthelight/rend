//! Background ripping: reads CDDA frames on a worker thread and reports
//! progress over an [`std::sync::mpsc`] channel.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
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
    TrackSkipped {
        number: u8,
        reason: String,
        path: PathBuf,
    },
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
    /// The formats each track is written in.
    pub formats: Vec<Format>,
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

/// One output file for a track.
#[derive(Debug, Clone)]
struct TrackOutput {
    number: u8,
    path: PathBuf,
    format: Format,
}

/// Everything needed to read and encode a single track.
struct TrackSpec {
    number: u8,
    lba: u32,
    frames: u32,
    outputs: Vec<TrackOutput>,
}

impl TrackSpec {
    fn new(job: &RipJob, track: &Track, frames: u32) -> Self {
        let outputs = job
            .formats
            .iter()
            .map(|format| {
                let path = track_output_path(
                    &job.out_dir,
                    &job.meta,
                    &job.template,
                    &job.toc,
                    track.number,
                    format,
                );
                TrackOutput {
                    number: track.number,
                    path,
                    format: *format,
                }
            })
            .collect();
        Self {
            number: track.number,
            lba: track.start_lba,
            frames,
            outputs,
        }
    }
}

/// The output path of one track in one format.
///
/// With looked-up metadata the path comes from the naming template; without
/// it the flat `trackNN` name is kept.
fn track_output_path(
    out_dir: &Path,
    meta: &Option<DiscMeta>,
    template: &Template,
    toc: &Toc,
    number: u8,
    format: &Format,
) -> PathBuf {
    let position = toc
        .audio_tracks()
        .position(|t| t.number == number)
        .map(|i| i + 1)
        .unwrap_or(1);
    match meta {
        Some(disc) => template.track_path(out_dir, disc, number, position, format.extension()),
        None => out_dir.join(format!("track{:02}.{}", number, format.extension())),
    }
}

/// The output paths of the requested tracks, in request order.
///
/// Unknown track numbers are omitted.
pub fn output_paths(
    toc: &Toc,
    out_dir: &Path,
    meta: &Option<DiscMeta>,
    template: &Template,
    formats: &[Format],
    tracks: &[u8],
) -> Vec<(u8, Vec<PathBuf>)> {
    tracks
        .iter()
        .copied()
        .filter(|number| toc.track(*number).is_some())
        .map(|number| {
            (
                number,
                formats
                    .iter()
                    .map(|format| track_output_path(out_dir, meta, template, toc, number, format))
                    .collect(),
            )
        })
        .collect()
}

fn rip_track(job: &mut RipJob, number: u8, tx: &Sender<RipEvent>) -> Result<(), String> {
    let track = match job.toc.track(number) {
        Some(track) => track,
        None => return Err(format!("track {number} not found on disc")),
    };
    let end = job.toc.end_lba(number).unwrap_or(job.toc.leadout_lba);
    let spec = TrackSpec::new(job, track, track.frames(end));

    let mut to_write = Vec::new();
    for output in &spec.outputs {
        if output.path.exists() && !job.force {
            tx.send(RipEvent::TrackSkipped {
                number,
                reason: "output exists (force is off)".into(),
                path: output.path.clone(),
            })
            .ok();
        } else {
            to_write.push(output.clone());
        }
    }
    if to_write.is_empty() {
        return Ok(());
    }

    for output in &to_write {
        if let Some(parent) = output.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }

    tx.send(RipEvent::TrackStarted { number }).ok();
    match read_track(
        &mut job.source,
        spec.number,
        spec.lba,
        spec.frames,
        &to_write,
        tx,
        &job.stop,
    ) {
        Ok(bytes) => {
            for (output, bytes) in to_write.iter().zip(bytes) {
                tag_track(job, output.number, &output.path);
                let bytes = std::fs::metadata(&output.path)
                    .map(|m| m.len())
                    .unwrap_or(bytes);
                tx.send(RipEvent::TrackDone {
                    number: output.number,
                    path: output.path.clone(),
                    bytes,
                })
                .ok();
            }
            Ok(())
        }
        Err(e) => {
            for output in &to_write {
                std::fs::remove_file(&output.path).ok();
            }
            Err(e.to_string())
        }
    }
}

/// Embeds the looked-up metadata and cover art into a finished track file.
/// Best effort: a tagging failure never fails the rip.
fn tag_track(job: &RipJob, number: u8, path: &std::path::Path) {
    let Some(disc) = &job.meta else {
        return;
    };
    let total = job.toc.audio_tracks().count();
    let Some(position) = job
        .toc
        .audio_tracks()
        .position(|t| t.number == number)
        .map(|i| i + 1)
    else {
        return;
    };
    let Some(mut tags) = TrackTags::for_track(disc, position, total) else {
        return;
    };
    tags.catalog_number = job.catalog_number.clone();
    let _ = apply(path, &tags, job.cover.as_deref());
}

fn read_track(
    source: &mut RipSource,
    number: u8,
    lba: u32,
    frames: u32,
    outputs: &[TrackOutput],
    tx: &Sender<RipEvent>,
    stop: &AtomicBool,
) -> io::Result<Vec<u64>> {
    let mut stream = CddaStream::new(source, lba, frames);
    let total = stream.total_bytes() as u64;
    let mut writers = Vec::new();
    for output in outputs {
        match output.format.create_file(&output.path) {
            Ok(file) => writers.push((output, file)),
            Err(error) => {
                for (created, _) in &writers {
                    std::fs::remove_file(&created.path).ok();
                }
                return Err(error);
            }
        }
    }

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
        for (_, file) in &mut writers {
            file.write(&buf[..n])?;
        }
        done += n as u64;
        tx.send(RipEvent::Progress {
            number,
            bytes_done: done,
            bytes_total: total,
        })
        .ok();
    }

    let mut bytes = Vec::with_capacity(writers.len());
    for (output, file) in writers {
        file.finish()?;
        bytes.push(std::fs::metadata(&output.path)?.len());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;

    use crate::demo::DemoDisc;

    fn run_job(
        tracks: Vec<u8>,
        out_dir: PathBuf,
        force: bool,
        formats: Vec<Format>,
    ) -> Vec<RipEvent> {
        let (tx, rx) = mpsc::channel();
        let job = RipJob {
            source: RipSource::Demo(DemoSource::with_delay(Duration::ZERO)),
            toc: DemoDisc::new().toc,
            tracks,
            out_dir,
            formats,
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
        let events = run_job(
            vec![3],
            dir.path().to_path_buf(),
            false,
            vec![Format::default()],
        );

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
        let events = run_job(
            vec![3],
            dir.path().to_path_buf(),
            false,
            vec![Format::parse("wav").unwrap()],
        );

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
    fn rips_a_demo_track_to_multiple_formats() {
        let dir = tempfile::tempdir().unwrap();
        let events = run_job(
            vec![3],
            dir.path().to_path_buf(),
            false,
            vec![Format::default(), Format::parse("wav").unwrap()],
        );

        let flac = dir.path().join("track03.flac");
        let wav = dir.path().join("track03.wav");
        assert!(flac.exists());
        assert!(wav.exists());

        let started = events
            .iter()
            .filter(|event| matches!(event, RipEvent::TrackStarted { number: 3, .. }))
            .count();
        assert_eq!(started, 1);

        let done_paths = events
            .iter()
            .filter_map(|event| match event {
                RipEvent::TrackDone { number, path, .. } if *number == 3 => Some(path.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(done_paths.contains(&flac));
        assert!(done_paths.contains(&wav));
        assert_eq!(done_paths.len(), 2);
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
            formats: vec![Format::default()],
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
            .join("The Demo Band/Demo Album/03 Short One.flac");
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

        let events = run_job(
            vec![3],
            dir.path().to_path_buf(),
            false,
            vec![Format::default()],
        );

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

        let events = run_job(
            vec![3],
            dir.path().to_path_buf(),
            true,
            vec![Format::default()],
        );

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
        let events = run_job(
            vec![9],
            dir.path().to_path_buf(),
            false,
            vec![Format::default()],
        );

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

    #[test]
    fn output_paths_follow_request_order_and_omit_unknown_tracks() {
        let disc = DemoDisc::new();
        let dir = tempfile::tempdir().unwrap();
        let paths = output_paths(
            &disc.toc,
            dir.path(),
            &None,
            &Template::default(),
            &[Format::default()],
            &[3, 2, 9],
        );
        assert_eq!(
            paths,
            vec![
                (3, vec![dir.path().join("track03.flac")]),
                (2, vec![dir.path().join("track02.flac")]),
            ]
        );
    }

    #[test]
    fn output_paths_use_the_naming_template_when_metadata_is_present() {
        let disc = DemoDisc::new();
        let dir = tempfile::tempdir().unwrap();
        let meta = crate::demo::demo_meta();
        let paths = output_paths(
            &disc.toc,
            dir.path(),
            &Some(meta),
            &Template::default(),
            &[Format::default()],
            &[3],
        );
        assert_eq!(
            paths,
            vec![(
                3,
                vec![
                    dir.path()
                        .join("The Demo Band/Demo Album/03 Short One.flac")
                ]
            )]
        );
    }
}
