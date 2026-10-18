//! Server state: the drives, their metadata, the rip worker controls,
//! and the event bus that pushes changes to the browser.
//!
//! The flows mirror `rend-tui`'s `App`: reading the TOC, MusicBrainz
//! lookup with a per-drive generation counter so a newer request
//! invalidates an in-flight one, cover fetching, and background ripping
//! through the shared [`rend_rip::spawn`] worker.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use rend_core::{Device, DeviceInfo, DriveStatus, Toc};
use rend_encode::{Format, ffmpeg_available};
use rend_meta::{
    Candidate, DiscMeta, DiscToc, MetaCache, Template, TrackMeta, cover_art, lookup_candidates,
    register_disc_id_url,
};
use rend_rip::demo::{
    DEMO_DEVICE, DEMO_LABEL, DEMO_MCN, DemoDisc, DemoSource, demo_cover, demo_meta_all,
};
use rend_rip::{RipEvent, RipJob, RipSource, spawn};
use tokio::sync::broadcast;

use crate::view::{
    DriveView, MatchView, MetaView, RipEventView, RipView, ServerEvent, StateView, TocView,
    TrackStateView, cover_data_url,
};

/// The drives and their per-drive state, plus the shared metadata cache
/// and the SSE event bus.
pub struct AppState {
    drives: Mutex<Vec<Drive>>,
    cache: Mutex<MetaCache>,
    settings: Mutex<Settings>,
    events: broadcast::Sender<ServerEvent>,
    out_dir: PathBuf,
    /// Whether every drive rips into its own subdirectory of `out_dir`.
    /// `None` keeps the default: per-drive subdirectories only when
    /// several drives are present.
    drive_dirs: Option<bool>,
    ffmpeg: bool,
    meta_tx: Mutex<mpsc::Sender<MetaEvent>>,
    meta_rx: Mutex<Option<mpsc::Receiver<MetaEvent>>>,
    running: AtomicBool,
    tick_handle: Mutex<Option<JoinHandle<()>>>,
    /// Self-reference stored after construction, so the background threads
    /// can own an `Arc<AppState>`.
    arc: Mutex<Option<Arc<Self>>>,
}

/// A failure of a drive or settings action, carrying the HTTP status the
/// router should answer with.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    /// Bad request: the action's arguments are wrong for this drive.
    pub fn bad(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    /// Not found: the drive index does not name a drive.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    /// Conflict: the drive's current state does not allow the action.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }
}

/// One drive: a real CD-ROM device or the simulated disc.
#[derive(Debug)]
pub struct Drive {
    pub path: String,
    pub label: String,
    pub status: String,
    pub disc: Option<String>,
    pub demo: bool,
    pub loading_toc: bool,
    /// The last TOC read failed; don't auto-retry on every poll.
    pub toc_failed: bool,
    pub toc: Option<Toc>,
    pub mcn: Option<String>,
    pub register_url: Option<String>,
    pub lookup_status: String,
    pub candidates: Vec<Candidate>,
    pub match_sel: usize,
    pub meta: Option<DiscMeta>,
    pub cover: Option<Vec<u8>>,
    /// The cached data-URL encoding of `cover`, shared with every view
    /// snapshot so building one only bumps a reference.
    pub cover_data: Option<Arc<String>>,
    pub cover_fetching: bool,
    pub meta_edited: bool,
    pub rip: Option<RipControl>,
    /// Bumped before every background read; stale results are dropped.
    pub meta_gen: u64,
    pub last_status: Option<DriveStatus>,
}

/// A running (or finished) rip on one drive.
#[derive(Debug)]
pub struct RipControl {
    pub active: bool,
    pub stopped: bool,
    pub tracks: Vec<u8>,
    pub out_dir: PathBuf,
    pub format: Format,
    pub force: bool,
    pub toc: Toc,
    pub states: Vec<RipTrackState>,
    pub summary: Option<String>,
    pub stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    forwarder: Option<JoinHandle<()>>,
}

/// The per-track state of a rip, in TOC order.
#[derive(Debug)]
pub struct RipTrackState {
    pub number: u8,
    pub state: TrackState,
}

#[derive(Debug)]
pub enum TrackState {
    Pending,
    Ripping { done: u64, total: u64 },
    Done { bytes: u64 },
    Skipped(String),
    Failed(String),
}

#[derive(Debug)]
struct Settings {
    format: Format,
    force: bool,
    template: Template,
}

enum MetaEvent {
    TocRead {
        drive: usize,
        id: u64,
        result: Result<(Toc, Option<String>), String>,
    },
    Candidates {
        drive: usize,
        id: u64,
        disc: DiscToc,
        result: Result<Vec<Candidate>, String>,
    },
    Cover {
        drive: usize,
        id: u64,
        release_id: String,
        result: Result<Option<Vec<u8>>, String>,
    },
}

impl AppState {
    /// Creates the state, discovering real drives (or the demo drive).
    ///
    /// Fails when an explicitly requested device cannot be opened: that is
    /// a startup error, not a drive to be discovered around.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        out_dir: PathBuf,
        device: Option<&str>,
        format: Format,
        force: bool,
        template: Template,
        drive_dirs: Option<bool>,
        demo: bool,
    ) -> Result<Arc<Self>, String> {
        let ffmpeg = ffmpeg_available();
        let (events, _) = broadcast::channel(1024);
        let (meta_tx, meta_rx) = mpsc::channel();
        let drives = if demo {
            vec![Drive::demo()]
        } else {
            let devices = match device {
                Some(path) => match Device::open(path) {
                    Ok(device) => vec![device],
                    Err(error) => return Err(format!("{path}: {error}")),
                },
                None => Device::discover().unwrap_or_default(),
            };
            devices.into_iter().map(Drive::real).collect()
        };
        let state = Arc::new(Self {
            drives: Mutex::new(drives),
            cache: Mutex::new(MetaCache::new()),
            settings: Mutex::new(Settings {
                format,
                force,
                template,
            }),
            events,
            out_dir,
            drive_dirs,
            ffmpeg,
            meta_tx: Mutex::new(meta_tx),
            meta_rx: Mutex::new(Some(meta_rx)),
            running: AtomicBool::new(false),
            tick_handle: Mutex::new(None),
            arc: Mutex::new(None),
        });
        *state.arc_guard() = Some(state.clone());
        Ok(state)
    }

    fn arc(&self) -> Arc<Self> {
        self.arc_guard()
            .clone()
            .expect("AppState is accessed through Arc")
    }

    /// Locks a state mutex, recovering the guard if a previous holder
    /// panicked: the state may be stale, but a wedged lock must not take
    /// the whole server down with it.
    fn drives_guard(&self) -> MutexGuard<'_, Vec<Drive>> {
        self.drives
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn cache_guard(&self) -> MutexGuard<'_, MetaCache> {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn settings_guard(&self) -> MutexGuard<'_, Settings> {
        self.settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn tick_guard(&self) -> MutexGuard<'_, Option<JoinHandle<()>>> {
        self.tick_handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn arc_guard(&self) -> MutexGuard<'_, Option<Arc<Self>>> {
        self.arc
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn meta_tx_guard(&self) -> MutexGuard<'_, mpsc::Sender<MetaEvent>> {
        self.meta_tx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn meta_rx_guard(&self) -> MutexGuard<'_, Option<mpsc::Receiver<MetaEvent>>> {
        self.meta_rx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Starts the background poll/meta thread.
    pub fn start(&self) {
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        self.arm_meta();
        let state = self.arc();
        let handle = std::thread::Builder::new()
            .name("rend-wui".into())
            .spawn(move || state.run())
            .expect("failed to spawn the web state thread");
        *self.tick_guard() = Some(handle);
    }

    /// Gives the meta pipeline a live channel.
    ///
    /// The first `run` consumes the receiver, so a restart needs a fresh
    /// channel. Readers still in flight from the previous run now report
    /// into the void, so their pending state is dropped and the poll can
    /// retry the read.
    fn arm_meta(&self) {
        let mut rx = self.meta_rx_guard();
        if rx.is_some() {
            return;
        }
        let (tx, new_rx) = mpsc::channel();
        *self.meta_tx_guard() = tx;
        *rx = Some(new_rx);
        for drive in self.drives_guard().iter_mut() {
            drive.loading_toc = false;
            drive.cover_fetching = false;
        }
    }

    /// Stops any running rips and joins the background poll/meta thread.
    pub fn shutdown(&self) {
        self.running.store(false, Ordering::SeqCst);
        struct RipHandles {
            worker: Option<JoinHandle<()>>,
            forwarder: Option<JoinHandle<()>>,
        }
        let (rips, tick) = {
            let mut drives = self.drives_guard();
            let rips: Vec<RipHandles> = drives
                .iter_mut()
                .filter_map(|drive| drive.rip.as_mut())
                .filter(|rip| rip.active)
                .map(|rip| {
                    rip.stop.store(true, Ordering::Relaxed);
                    rip.active = false;
                    RipHandles {
                        worker: rip.worker.take(),
                        forwarder: rip.forwarder.take(),
                    }
                })
                .collect();
            let tick = self.tick_guard().take();
            (rips, tick)
        };
        // Join outside the drive lock: the forwarder re-acquires it.
        for handles in rips {
            if let Some(worker) = handles.worker {
                worker.join().ok();
            }
            if let Some(forwarder) = handles.forwarder {
                forwarder.join().ok();
            }
        }
        if let Some(handle) = tick {
            handle.join().ok();
        }
    }

    pub fn view(&self) -> StateView {
        let settings = self.settings_guard();
        let drives = self.drives_guard();
        StateView {
            out_dir: self.out_dir.to_string_lossy().into_owned(),
            ffmpeg: self.ffmpeg,
            format: settings.format.label().to_string(),
            force: settings.force,
            drives: drives
                .iter()
                .enumerate()
                .map(|(index, drive)| drive.view(index))
                .collect(),
        }
    }

    pub fn events_receiver(&self) -> broadcast::Receiver<ServerEvent> {
        self.events.subscribe()
    }

    pub fn set_format(&self, format: Format) {
        let changed = {
            let mut settings = self.settings_guard();
            let changed = settings.format != format;
            settings.format = format;
            changed
        };
        if changed {
            self.emit_log(format!("output format set to {}", format.label()));
        }
    }

    pub fn set_force(&self, force: bool) {
        let changed = {
            let mut settings = self.settings_guard();
            let changed = settings.force != force;
            settings.force = force;
            changed
        };
        if changed {
            self.emit_log(if force {
                "force is on: existing files will be overwritten".to_string()
            } else {
                "force is off: existing files are kept".to_string()
            });
        }
    }

    pub fn load_toc(&self, index: usize) -> Result<(), ApiError> {
        let demo = {
            let drives = self.drives_guard();
            let Some(drive) = drives.get(index) else {
                return Err(ApiError::not_found("unknown drive"));
            };
            if drive.rip.as_ref().is_some_and(|rip| rip.active) {
                return Err(ApiError::conflict("a rip is in progress on this drive"));
            }
            drive.demo
        };
        if demo {
            let loaded = {
                let mut drives = self.drives_guard();
                let Some(drive) = drives.get_mut(index) else {
                    return Err(ApiError::not_found("unknown drive"));
                };
                if drive.toc.is_none() {
                    let demo = DemoDisc::new();
                    drive.toc = Some(demo.toc.clone());
                    drive.mcn = Some(DEMO_MCN.to_string());
                    drive.register_url = Some(register_disc_id_url(&disc_toc(&demo.toc)));
                    drive.candidates = demo_candidates();
                    drive.match_sel = 0;
                    drive.meta = drive.candidates.first().map(|c| c.meta.clone());
                    drive.set_cover(Some(demo_cover()));
                    drive.cover_fetching = false;
                    drive.lookup_status = "demo disc".to_string();
                    true
                } else {
                    false
                }
            };
            if loaded {
                self.drive_changed(index);
            }
        } else {
            self.request_toc(index);
        }
        Ok(())
    }

    pub fn select_match(&self, index: usize, pos: usize) -> Result<(), ApiError> {
        let release_id = {
            let mut drives = self.drives_guard();
            let Some(drive) = drives.get_mut(index) else {
                return Err(ApiError::not_found("unknown drive"));
            };
            if drive.candidates.is_empty() {
                return Err(ApiError::conflict("no candidates to select"));
            }
            drive.match_sel = pos.rem_euclid(drive.candidates.len());
            drive.meta = Some(drive.candidates[drive.match_sel].meta.clone());
            drive.set_cover(None);
            drive.cover_fetching = false;
            drive.meta_edited = false;
            if drive.candidates.len() > 1 {
                drive.lookup_status = format!(
                    "match {} of {}{}",
                    drive.match_sel + 1,
                    drive.candidates.len(),
                    if drive.meta_edited { " (edited)" } else { "" }
                );
            }
            drive
                .meta
                .as_ref()
                .map(|meta| meta.release_id.clone())
                .filter(|release_id| !release_id.is_empty())
        };
        self.drive_changed(index);
        if let Some(release_id) = release_id {
            self.fetch_cover(index, release_id);
        }
        Ok(())
    }

    pub fn set_tags(
        &self,
        index: usize,
        album: Option<String>,
        artist: Option<String>,
        album_artist: Option<String>,
        year: Option<String>,
        tracks: Vec<(Option<String>, Option<String>)>,
    ) -> Result<(), ApiError> {
        {
            let mut drives = self.drives_guard();
            let Some(drive) = drives.get_mut(index) else {
                return Err(ApiError::not_found("unknown drive"));
            };
            if drive.rip.as_ref().is_some_and(|rip| rip.active) {
                return Err(ApiError::conflict("stop the rip before editing tags"));
            }
            let track_count = drive
                .toc
                .as_ref()
                .map(|toc| toc.audio_tracks().count())
                .unwrap_or(tracks.len());
            let mut meta = drive.meta.clone().unwrap_or_else(blank_meta);
            if let Some(album) = album {
                meta.album = album;
            }
            if let Some(artist) = artist {
                meta.artist = artist;
            }
            meta.album_artist = album_artist
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            meta.year = year
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            meta.tracks.resize_with(track_count, || TrackMeta {
                title: "Unknown".to_string(),
                artist: None,
                artist_id: None,
                recording_id: None,
                release_track_id: None,
            });
            for (field, track) in tracks.iter().zip(meta.tracks.iter_mut()) {
                if let Some(title) = &field.0 {
                    track.title = title.clone();
                }
                if let Some(artist) = &field.1 {
                    track.artist = if artist.trim().is_empty() {
                        None
                    } else {
                        Some(artist.trim().to_string())
                    };
                }
            }
            drive.meta = Some(meta);
            drive.meta_edited = true;
            if drive.candidates.is_empty() {
                drive.lookup_status = "manual".to_string();
            } else {
                drive.lookup_status = format!(
                    "match {} of {} (edited)",
                    drive.match_sel + 1,
                    drive.candidates.len()
                );
            }
        }
        self.drive_changed(index);
        Ok(())
    }

    pub fn rip(&self, index: usize, requested: Vec<u8>) -> Result<(), ApiError> {
        let (format, force, template) = {
            let settings = self.settings_guard();
            (settings.format, settings.force, settings.template.clone())
        };
        let (path, demo, toc, tracks, out_dir, meta, cover, mcn) = {
            let drives = self.drives_guard();
            let Some(drive) = drives.get(index) else {
                return Err(ApiError::not_found("unknown drive"));
            };
            if drive.rip.as_ref().is_some_and(|rip| rip.active) {
                return Err(ApiError::conflict("a rip is already running on this drive"));
            }
            let toc = drive
                .toc
                .clone()
                .ok_or_else(|| ApiError::bad("no disc is loaded"))?;
            let tracks = if requested.is_empty() {
                toc.audio_tracks().map(|track| track.number).collect()
            } else {
                requested
            };
            if tracks.is_empty() {
                return Err(ApiError::bad("the disc has no audio tracks"));
            }
            let mut seen = [false; 256];
            for &number in &tracks {
                if seen[number as usize] {
                    return Err(ApiError::bad(format!("track {number} was requested twice")));
                }
                seen[number as usize] = true;
                if toc.track(number).is_none_or(|track| !track.is_audio()) {
                    return Err(ApiError::bad(format!(
                        "track {number} is not an audio track"
                    )));
                }
            }
            if format.requires_ffmpeg() && !self.ffmpeg {
                return Err(ApiError::bad("ffmpeg is required for flac output"));
            }
            let per_drive = self.drive_dirs.unwrap_or(drives.len() > 1);
            let out_dir = if per_drive {
                let name = drive.path.rsplit('/').next().unwrap_or("drive").to_string();
                self.out_dir.join(name)
            } else {
                self.out_dir.clone()
            };
            (
                drive.path.clone(),
                drive.demo,
                toc,
                tracks,
                out_dir,
                drive.meta.clone(),
                drive.cover.clone(),
                drive.mcn.clone(),
            )
        };
        let track_count = tracks.len();

        let states = toc
            .tracks
            .iter()
            .map(|track| {
                let state = if tracks.contains(&track.number) {
                    TrackState::Pending
                } else {
                    TrackState::Skipped("not selected".to_string())
                };
                RipTrackState {
                    number: track.number,
                    state,
                }
            })
            .collect();
        let stop = Arc::new(AtomicBool::new(false));

        // Reserve the rip slot before the (blocking) device open, so two
        // concurrent requests cannot both start a rip on this drive.
        {
            let mut drives = self.drives_guard();
            let Some(drive) = drives.get_mut(index) else {
                return Err(ApiError::not_found("unknown drive"));
            };
            drive.rip = Some(RipControl {
                active: true,
                stopped: false,
                tracks: tracks.clone(),
                out_dir: out_dir.clone(),
                format,
                force,
                toc: toc.clone(),
                states,
                summary: None,
                stop: stop.clone(),
                worker: None,
                forwarder: None,
            });
        }

        let source = if demo {
            RipSource::Demo(DemoSource::new())
        } else {
            let device = match Device::open(&path) {
                Ok(device) => device,
                Err(error) => {
                    self.clear_rip(index);
                    return Err(ApiError::bad(error.to_string()));
                }
            };
            RipSource::Device(device)
        };

        let (worker_tx, worker_rx) = mpsc::channel();
        let job = RipJob {
            source,
            toc,
            tracks,
            out_dir,
            format,
            template,
            force,
            stop: stop.clone(),
            meta,
            cover,
            catalog_number: mcn,
        };
        let worker = spawn(job, worker_tx);
        {
            let mut drives = self.drives_guard();
            if let Some(rip) = drives.get_mut(index).and_then(|d| d.rip.as_mut()) {
                rip.worker = Some(worker);
            }
        }

        let state = self.arc();
        let rip_path = path.clone();
        let forwarder = std::thread::Builder::new()
            .name(format!("rend-wui-rip-{}", index))
            .spawn(move || state.forward_rip(index, rip_path, worker_rx))
            .expect("failed to spawn the rip forwarder");
        {
            let mut drives = self.drives_guard();
            if let Some(rip) = drives.get_mut(index).and_then(|d| d.rip.as_mut()) {
                rip.forwarder = Some(forwarder);
            }
        }

        let message = format!("ripping {track_count} track(s) on {path}");
        self.drive_changed(index);
        self.emit_log(message);
        Ok(())
    }

    /// Clears a reserved rip slot after a setup failure, such as the device
    /// not opening.
    fn clear_rip(&self, index: usize) {
        let mut drives = self.drives_guard();
        if let Some(drive) = drives.get_mut(index) {
            drive.rip = None;
        }
    }

    pub fn stop_rip(&self, index: usize) -> Result<(), ApiError> {
        let stop = {
            let mut drives = self.drives_guard();
            let Some(drive) = drives.get_mut(index) else {
                return Err(ApiError::not_found("unknown drive"));
            };
            let Some(rip) = drive.rip.as_mut().filter(|rip| rip.active) else {
                return Err(ApiError::conflict("no active rip on this drive"));
            };
            rip.stopped = true;
            rip.stop.clone()
        };
        stop.store(true, Ordering::Relaxed);
        self.emit_log(format!("{}: stop requested", self.drive_path(index)));
        Ok(())
    }

    pub fn eject(&self, index: usize) -> Result<(), ApiError> {
        let path = {
            let drives = self.drives_guard();
            let Some(drive) = drives.get(index) else {
                return Err(ApiError::not_found("unknown drive"));
            };
            if drive.demo {
                return Err(ApiError::bad("cannot eject the simulated drive"));
            }
            if drive.rip.as_ref().is_some_and(|rip| rip.active) {
                return Err(ApiError::conflict("stop the rip before ejecting"));
            }
            drive.path.clone()
        };
        let device = Device::open(&path).map_err(|e| ApiError::bad(e.to_string()))?;
        device.eject().map_err(|e| ApiError::bad(e.to_string()))?;
        {
            let mut drives = self.drives_guard();
            let Some(drive) = drives.get_mut(index) else {
                return Err(ApiError::not_found("unknown drive"));
            };
            drive.clear_disc_state();
            drive.status = "tray open".to_string();
            drive.disc = None;
            drive.last_status = Some(DriveStatus::TrayOpen);
        }
        self.drive_changed(index);
        self.emit_log(format!("{path}: ejected"));
        Ok(())
    }

    fn request_toc(&self, index: usize) {
        let (path, id) = {
            let mut drives = self.drives_guard();
            let Some(drive) = drives.get_mut(index) else {
                return;
            };
            if drive.demo || drive.loading_toc || drive.rip.as_ref().is_some_and(|r| r.active) {
                return;
            }
            drive.loading_toc = true;
            drive.toc_failed = false;
            drive.lookup_status = "reading TOC…".to_string();
            drive.meta_gen += 1;
            (drive.path.clone(), drive.meta_gen)
        };
        self.drive_changed(index);
        let tx = self.meta_tx_guard().clone();
        let spawned = std::thread::Builder::new()
            .name(format!("rend-wui-toc-{index}"))
            .spawn(move || {
                let result = read_toc(&path);
                tx.send(MetaEvent::TocRead {
                    drive: index,
                    id,
                    result,
                })
                .ok();
            })
            .is_ok();
        if !spawned {
            let stale = {
                let mut drives = self.drives_guard();
                match drives.get_mut(index) {
                    Some(drive) if drive.meta_gen == id => {
                        drive.loading_toc = false;
                        drive.toc_failed = true;
                        drive.lookup_status =
                            "TOC read failed: could not start the reader".to_string();
                        false
                    }
                    Some(_) => true,
                    None => true,
                }
            };
            if !stale {
                self.drive_changed(index);
                self.emit_log(format!(
                    "{}: could not start the TOC reader",
                    self.drive_path(index)
                ));
            }
        }
    }

    fn start_lookup(&self, index: usize) {
        let (disc, cached) = {
            let drives = self.drives_guard();
            let Some(drive) = drives.get(index) else {
                return;
            };
            let Some(toc) = drive.toc.as_ref() else {
                return;
            };
            let disc = disc_toc(toc);
            let cached = self
                .cache_guard()
                .candidates(&disc)
                .map(|candidates| candidates.to_vec());
            (disc, cached)
        };
        if let Some(candidates) = cached {
            self.apply_candidates(index, None, disc, candidates);
            return;
        }

        let id = {
            let mut drives = self.drives_guard();
            let Some(drive) = drives.get_mut(index) else {
                return;
            };
            if drive.demo {
                return;
            }
            drive.meta_gen += 1;
            drive.lookup_status = "looking up…".to_string();
            drive.meta_gen
        };
        self.drive_changed(index);
        let tx = self.meta_tx_guard().clone();
        let disc_for_thread = disc.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("rend-wui-lookup-{index}"))
            .spawn(move || {
                let result = lookup_candidates(&disc_for_thread).map_err(|e| e.to_string());
                tx.send(MetaEvent::Candidates {
                    drive: index,
                    id,
                    disc: disc_for_thread,
                    result,
                })
                .ok();
            })
            .is_ok();
        if !spawned {
            let stale = {
                let mut drives = self.drives_guard();
                match drives.get_mut(index) {
                    Some(drive) if drive.meta_gen == id => {
                        drive.lookup_status =
                            "lookup failed: could not start the lookup".to_string();
                        false
                    }
                    Some(_) => true,
                    None => true,
                }
            };
            if !stale {
                self.drive_changed(index);
                self.emit_log(format!(
                    "{}: could not start the lookup",
                    self.drive_path(index)
                ));
            }
        }
    }

    fn fetch_cover(&self, index: usize, release_id: String) {
        if release_id.is_empty() {
            return;
        }
        let fetch_id = {
            let mut drives = self.drives_guard();
            let Some(drive) = drives.get_mut(index) else {
                return;
            };
            if drive.demo {
                drive.set_cover(Some(demo_cover()));
                drive.cover_fetching = false;
                None
            } else if drive.cover_fetching {
                None
            } else {
                let cached = self.cache_guard().cover(&release_id).cloned();
                if let Some(cover) = cached {
                    drive.set_cover(cover);
                    drive.cover_fetching = false;
                    None
                } else {
                    drive.cover_fetching = true;
                    drive.meta_gen += 1;
                    Some(drive.meta_gen)
                }
            }
        };
        self.drive_changed(index);
        let Some(id) = fetch_id else {
            return;
        };
        let tx = self.meta_tx_guard().clone();
        let spawned = std::thread::Builder::new()
            .name(format!("rend-wui-cover-{index}"))
            .spawn(move || {
                let result = cover_art(&release_id).map_err(|e| e.to_string());
                tx.send(MetaEvent::Cover {
                    drive: index,
                    id,
                    release_id,
                    result,
                })
                .ok();
            })
            .is_ok();
        if !spawned {
            let stale = {
                let mut drives = self.drives_guard();
                match drives.get_mut(index) {
                    Some(drive) if drive.meta_gen == id => {
                        drive.cover_fetching = false;
                        false
                    }
                    Some(_) => true,
                    None => true,
                }
            };
            if !stale {
                self.drive_changed(index);
                self.emit_log(format!(
                    "{}: could not start the cover fetch",
                    self.drive_path(index)
                ));
            }
        }
    }

    fn apply_candidates(
        &self,
        index: usize,
        id: Option<u64>,
        disc: DiscToc,
        candidates: Vec<Candidate>,
    ) {
        let release_id = {
            let mut drives = self.drives_guard();
            let Some(drive) = drives.get_mut(index) else {
                return;
            };
            if drive.toc.as_ref().is_none_or(|toc| disc_toc(toc) != disc) {
                return;
            }
            if let Some(id) = id
                && drive.meta_gen != id
            {
                return;
            }
            let mut cache = self.cache_guard();
            cache.insert(disc, candidates.clone());
            drive.candidates = candidates;
            drive.match_sel = 0;
            drive.meta = drive.candidates.first().map(|c| c.meta.clone());
            drive.set_cover(None);
            drive.cover_fetching = false;
            drive.meta_edited = false;
            if drive.candidates.is_empty() {
                drive.lookup_status = "no match".to_string();
            } else if drive.candidates.len() == 1 {
                drive.lookup_status = "found".to_string();
            } else {
                drive.lookup_status = format!("{} matches", drive.candidates.len());
            }
            let release_id = drive
                .meta
                .as_ref()
                .map(|meta| meta.release_id.clone())
                .filter(|release_id| !release_id.is_empty());
            if let Some(release_id) = &release_id {
                if let Some(cover) = cache.cover(release_id).cloned() {
                    drive.set_cover(cover);
                }
            }
            release_id
        };
        self.drive_changed(index);
        if let Some(release_id) = release_id {
            self.fetch_cover(index, release_id);
        }
    }

    fn poll_drive(&self, index: usize) {
        if self.reap_dead_rip(index) {
            // A dead rip was cleaned up this tick; refresh on the next.
            return;
        }
        let path = {
            let drives = self.drives_guard();
            let Some(drive) = drives.get(index) else {
                return;
            };
            if drive.demo {
                return;
            }
            // The rip owns the device while it runs; don't poll it.
            if drive.rip.as_ref().is_some_and(|rip| rip.active) {
                return;
            }
            drive.path.clone()
        };
        // The blocking ioctls run outside the drive lock, so a slow or
        // unreadable drive cannot stall the view and the action handlers.
        let info = read_drive_info(&path);
        let (changed, need_load) = {
            let mut drives = self.drives_guard();
            let Some(drive) = drives.get_mut(index) else {
                return;
            };
            let old_label = drive.label.clone();
            let old_status = drive.status.clone();
            let old_disc = drive.disc.clone();
            drive.apply_poll(info);
            let need_load = drive.last_status == Some(DriveStatus::DiscOk)
                && drive.toc.is_none()
                && !drive.loading_toc
                && !drive.toc_failed
                && drive.rip.as_ref().is_none_or(|rip| !rip.active);
            let changed =
                old_label != drive.label || old_status != drive.status || old_disc != drive.disc;
            (changed, need_load)
        };
        if changed {
            self.drive_changed(index);
        }
        if need_load {
            self.request_toc(index);
        }
    }

    /// Reaps a rip whose forwarder thread died while the rip was still
    /// active, which would otherwise leave the slot active forever. The
    /// worker is told to stop and both handles are joined outside the
    /// drive lock.
    fn reap_dead_rip(&self, index: usize) -> bool {
        let reaped = {
            let mut drives = self.drives_guard();
            let Some(drive) = drives.get_mut(index) else {
                return false;
            };
            let Some(rip) = drive.rip.as_mut().filter(|rip| rip.active) else {
                return false;
            };
            if !rip
                .forwarder
                .as_ref()
                .is_some_and(|forwarder| forwarder.is_finished())
            {
                return false;
            }
            rip.stop.store(true, Ordering::Relaxed);
            rip.active = false;
            rip.summary = Some(format!("rip for {} died unexpectedly", drive.path));
            (drive.path.clone(), rip.worker.take(), rip.forwarder.take())
        };
        let (path, worker, forwarder) = reaped;
        if let Some(forwarder) = forwarder {
            forwarder.join().ok();
        }
        if let Some(worker) = worker {
            worker.join().ok();
        }
        self.emit_log(format!("{path}: the rip forwarder died unexpectedly"));
        self.drive_changed(index);
        true
    }

    fn handle_meta(&self, event: MetaEvent) {
        match event {
            MetaEvent::TocRead { drive, id, result } => {
                let loaded = {
                    let mut drives = self.drives_guard();
                    let Some(d) = drives.get_mut(drive) else {
                        return;
                    };
                    if d.meta_gen != id {
                        return;
                    }
                    d.loading_toc = false;
                    match result {
                        Ok((toc, mcn)) => {
                            d.toc = Some(toc.clone());
                            d.mcn = mcn;
                            d.register_url = Some(register_disc_id_url(&disc_toc(&toc)));
                            d.toc_failed = false;
                            d.lookup_status = "loaded".to_string();
                            d.candidates.clear();
                            d.meta = None;
                            d.match_sel = 0;
                            d.set_cover(None);
                            d.cover_fetching = false;
                            true
                        }
                        Err(error) => {
                            d.toc_failed = true;
                            d.lookup_status = format!("TOC read failed: {error}");
                            false
                        }
                    }
                };
                self.drive_changed(drive);
                if loaded {
                    self.start_lookup(drive);
                }
            }
            MetaEvent::Candidates {
                drive,
                id,
                disc,
                result,
            } => match result {
                Ok(candidates) => self.apply_candidates(drive, Some(id), disc, candidates),
                Err(error) => {
                    let stale = {
                        let mut drives = self.drives_guard();
                        match drives.get_mut(drive) {
                            Some(d) if d.meta_gen == id => {
                                d.lookup_status = format!("lookup failed: {error}");
                                false
                            }
                            Some(_) => true,
                            None => true,
                        }
                    };
                    if !stale {
                        self.drive_changed(drive);
                        self.emit_log(format!(
                            "{}: lookup failed: {error}",
                            self.drive_path(drive)
                        ));
                    }
                }
            },
            MetaEvent::Cover {
                drive,
                id,
                release_id,
                result,
            } => {
                let cover = match result {
                    Ok(cover) => cover,
                    Err(error) => {
                        let stale = {
                            let mut drives = self.drives_guard();
                            match drives.get_mut(drive) {
                                Some(d) if d.meta_gen == id => {
                                    d.cover_fetching = false;
                                    false
                                }
                                Some(_) => true,
                                None => true,
                            }
                        };
                        if !stale {
                            self.drive_changed(drive);
                            self.emit_log(format!(
                                "{}: cover fetch failed: {error}",
                                self.drive_path(drive)
                            ));
                        }
                        return;
                    }
                };
                let applied = {
                    let mut drives = self.drives_guard();
                    let Some(d) = drives.get_mut(drive) else {
                        return;
                    };
                    if d.meta_gen != id {
                        return;
                    }
                    let mut cache = self.cache_guard();
                    cache.cover_insert(release_id, cover.clone());
                    d.set_cover(cover);
                    d.cover_fetching = false;
                    true
                };
                if applied {
                    self.drive_changed(drive);
                }
            }
        }
    }

    fn forward_rip(&self, index: usize, path: String, rx: mpsc::Receiver<RipEvent>) {
        let mut finished = false;
        while let Ok(event) = rx.recv() {
            if matches!(event, RipEvent::Finished { .. }) {
                finished = true;
            }
            {
                let mut drives = self.drives_guard();
                if let Some(rip) = drives.get_mut(index).and_then(|d| d.rip.as_mut()) {
                    rip.apply_event(&event);
                }
            }
            let view = RipEventView::from(&event);
            self.emit(ServerEvent::Rip {
                drive: path.clone(),
                event: view,
            });
            if !matches!(event, RipEvent::Progress { .. }) {
                self.drive_changed(index);
            }
            if finished {
                let worker = {
                    let mut drives = self.drives_guard();
                    drives
                        .get_mut(index)
                        .and_then(|d| d.rip.as_mut())
                        .and_then(|rip| rip.worker.take())
                };
                if let Some(worker) = worker {
                    worker.join().ok();
                }
                self.drive_changed(index);
                break;
            }
        }
        // The worker ended without reporting completion (for example, it
        // panicked): clear the stuck rip slot so the drive is usable again.
        let died = {
            let mut drives = self.drives_guard();
            let mut died = false;
            if let Some(rip) = drives
                .get_mut(index)
                .and_then(|d| d.rip.as_mut())
                .filter(|rip| rip.active)
            {
                died = true;
                rip.active = false;
                rip.summary = Some(format!("rip worker for {path} died unexpectedly"));
            }
            died
        };
        if died {
            self.emit_log(format!("{path}: rip worker died unexpectedly"));
            self.drive_changed(index);
        }
    }

    fn drive_changed(&self, index: usize) {
        let drive = {
            let drives = self.drives_guard();
            drives.get(index).map(|drive| drive.view(index))
        };
        if let Some(drive) = drive {
            self.emit(ServerEvent::DriveChanged {
                drive: Box::new(drive),
            });
        }
    }

    fn drive_path(&self, index: usize) -> String {
        let drives = self.drives_guard();
        drives
            .get(index)
            .map(|drive| drive.path.clone())
            .unwrap_or_else(|| format!("drive {index}"))
    }

    fn emit(&self, event: ServerEvent) {
        let _ = self.events.send(event);
    }

    fn emit_log(&self, msg: String) {
        self.emit(ServerEvent::Log { msg });
    }

    fn run(&self) {
        let Some(rx) = self.meta_rx_guard().take() else {
            return;
        };
        let mut last_poll = Instant::now();
        loop {
            if !self.running.load(Ordering::SeqCst) {
                break;
            }
            if last_poll.elapsed() >= Duration::from_secs(1) {
                let count = self.drives_guard().len();
                for index in 0..count {
                    self.poll_drive(index);
                }
                last_poll = Instant::now();
            }
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(event) => self.handle_meta(event),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }
}

impl Drive {
    fn real(device: Device) -> Self {
        let mut drive = Self {
            path: device.path().to_string(),
            label: "unknown".to_string(),
            status: String::new(),
            disc: None,
            demo: false,
            loading_toc: false,
            toc_failed: false,
            toc: None,
            mcn: None,
            register_url: None,
            lookup_status: String::new(),
            candidates: Vec::new(),
            match_sel: 0,
            meta: None,
            cover: None,
            cover_data: None,
            cover_fetching: false,
            meta_edited: false,
            rip: None,
            meta_gen: 0,
            last_status: None,
        };
        drive.refresh();
        drive
    }

    fn demo() -> Self {
        let toc = DemoDisc::new().toc;
        let candidates = demo_candidates();
        let meta = candidates.first().map(|c| c.meta.clone());
        let cover = Some(demo_cover());
        let cover_data = cover.as_deref().and_then(cover_data_url).map(Arc::new);
        Self {
            path: DEMO_DEVICE.to_string(),
            label: DEMO_LABEL.to_string(),
            status: DriveStatus::DiscOk.to_string(),
            disc: Some("audio".to_string()),
            demo: true,
            loading_toc: false,
            toc_failed: false,
            toc: Some(toc.clone()),
            mcn: Some(DEMO_MCN.to_string()),
            register_url: Some(register_disc_id_url(&disc_toc(&toc))),
            lookup_status: "demo disc".to_string(),
            candidates,
            match_sel: 0,
            meta,
            cover,
            cover_data,
            cover_fetching: false,
            meta_edited: false,
            rip: None,
            meta_gen: 0,
            last_status: Some(DriveStatus::DiscOk),
        }
    }

    fn refresh(&mut self) {
        self.apply_info(read_drive_info(&self.path));
    }

    /// Applies a drive status read to the drive.
    ///
    /// `None` means the drive could not be read at all (device missing or a
    /// transient error). The last known status is cleared so callers don't
    /// act on a stale one, but the loaded disc and any user edits are kept:
    /// a read blip is not a disc removal.
    fn apply_info(&mut self, info: Option<DeviceInfo>) {
        match info {
            Some(info) => {
                self.label = drive_label(&info);
                self.status = info.drive_status.to_string();
                self.disc = info.disc_status.map(|status| status.to_string());
                self.last_status = Some(info.drive_status);
            }
            None => {
                self.status = "unreadable".to_string();
                self.disc = None;
                self.last_status = None;
            }
        }
    }

    /// Applies a polled drive status, clearing the loaded disc state when
    /// the drive positively reports an empty tray. Transient states (an
    /// unreadable drive, "not ready", "no information") keep it.
    fn apply_poll(&mut self, info: Option<DeviceInfo>) {
        let removed = matches!(
            info.as_ref().map(|info| info.drive_status),
            Some(DriveStatus::NoDisc | DriveStatus::TrayOpen)
        );
        self.apply_info(info);
        if removed && self.toc.is_some() {
            self.clear_disc_state();
        }
    }

    fn clear_disc_state(&mut self) {
        self.toc = None;
        self.mcn = None;
        self.register_url = None;
        self.candidates.clear();
        self.meta = None;
        self.match_sel = 0;
        self.set_cover(None);
        self.cover_fetching = false;
        self.meta_edited = false;
        self.toc_failed = false;
        self.lookup_status = "no disc".to_string();
    }

    /// Replaces the cover art and refreshes its cached data-URL encoding.
    fn set_cover(&mut self, cover: Option<Vec<u8>>) {
        self.cover_data = cover.as_deref().and_then(cover_data_url).map(Arc::new);
        self.cover = cover;
    }

    fn view(&self, index: usize) -> DriveView {
        DriveView {
            index,
            path: self.path.clone(),
            label: self.label.clone(),
            status: self.status.clone(),
            disc: self.disc.clone(),
            demo: self.demo,
            loading_toc: self.loading_toc,
            toc: self.toc.as_ref().map(TocView::from_toc),
            mcn: self.mcn.clone(),
            register_url: self.register_url.clone(),
            lookup_status: self.lookup_status.clone(),
            matches: self
                .candidates
                .iter()
                .map(MatchView::from_candidate)
                .collect(),
            match_sel: self.match_sel,
            meta: self.meta.as_ref().map(MetaView::from_meta),
            cover: self.cover_data.clone(),
            cover_fetching: self.cover_fetching,
            rip: self.rip.as_ref().map(RipControl::view),
        }
    }
}

impl RipControl {
    fn apply_event(&mut self, event: &RipEvent) {
        let index_of = |number: u8| self.toc.tracks.iter().position(|t| t.number == number);
        match event {
            RipEvent::TrackStarted { number } => {
                if let Some(i) = index_of(*number) {
                    self.states[i].state = TrackState::Ripping { done: 0, total: 0 };
                }
            }
            RipEvent::Progress {
                number,
                bytes_done,
                bytes_total,
            } => {
                if let Some(i) = index_of(*number) {
                    if matches!(self.states[i].state, TrackState::Ripping { .. }) {
                        self.states[i].state = TrackState::Ripping {
                            done: *bytes_done,
                            total: *bytes_total,
                        };
                    }
                }
            }
            RipEvent::TrackDone { number, bytes, .. } => {
                if let Some(i) = index_of(*number) {
                    self.states[i].state = TrackState::Done { bytes: *bytes };
                }
            }
            RipEvent::TrackFailed { number, error } => {
                if let Some(i) = index_of(*number) {
                    self.states[i].state = TrackState::Failed(error.clone());
                }
            }
            RipEvent::TrackSkipped { number, reason, .. } => {
                if let Some(i) = index_of(*number) {
                    self.states[i].state = TrackState::Skipped(reason.clone());
                }
            }
            RipEvent::Finished { stopped, .. } => {
                self.active = false;
                self.stopped |= *stopped;
                self.summary = Some(self.summarize());
            }
        }
    }

    fn summarize(&self) -> String {
        let mut done = 0;
        let mut failed = 0;
        let mut skipped = 0;
        for &number in &self.tracks {
            let Some(state) = self.states.iter().find(|s| s.number == number) else {
                continue;
            };
            match &state.state {
                TrackState::Done { .. } => done += 1,
                TrackState::Failed(_) => failed += 1,
                TrackState::Skipped(_) => skipped += 1,
                _ => {}
            }
        }
        let total = self.tracks.len();
        if self.stopped {
            format!("stopped after {done} of {total} track(s)")
        } else if failed == 0 {
            if skipped == 0 {
                format!("ripped {done} of {total} track(s)")
            } else {
                format!("ripped {done} of {total} track(s), {skipped} skipped")
            }
        } else {
            format!("finished: {done} done, {failed} failed, {skipped} skipped")
        }
    }

    fn view(&self) -> RipView {
        RipView {
            active: self.active,
            tracks: self.tracks.clone(),
            out_dir: self.out_dir.to_string_lossy().into_owned(),
            format: self.format.label().to_string(),
            force: self.force,
            states: self.states.iter().map(RipTrackState::view).collect(),
            summary: self.summary.clone(),
        }
    }
}

impl RipTrackState {
    fn view(&self) -> TrackStateView {
        let (status, detail, done, total) = match &self.state {
            TrackState::Pending => ("pending".to_string(), None, 0, 0),
            TrackState::Ripping { done, total } => ("ripping".to_string(), None, *done, *total),
            TrackState::Done { bytes } => (
                "done".to_string(),
                Some(human_bytes(*bytes)),
                *bytes,
                *bytes,
            ),
            TrackState::Skipped(reason) => ("skipped".to_string(), Some(reason.clone()), 0, 0),
            TrackState::Failed(error) => ("failed".to_string(), Some(error.clone()), 0, 0),
        };
        TrackStateView {
            number: self.number,
            status,
            detail,
            done,
            total,
        }
    }
}

fn demo_candidates() -> Vec<Candidate> {
    demo_meta_all()
        .into_iter()
        .map(|meta| Candidate {
            meta,
            max_diff_ms: 0,
        })
        .collect()
}

fn blank_meta() -> DiscMeta {
    DiscMeta {
        album: String::new(),
        artist: String::new(),
        album_artist: None,
        year: None,
        release_id: String::new(),
        release_artist_id: None,
        disc_number: None,
        disc_count: None,
        tracks: Vec::new(),
    }
}

fn drive_label(info: &DeviceInfo) -> String {
    let vendor = info.vendor.as_deref().unwrap_or("").trim();
    let model = info.model.as_deref().unwrap_or("unknown").trim();
    let mut label = if vendor.is_empty() {
        model.to_string()
    } else {
        format!("{vendor} {model}")
    };
    if let Some(version) = info
        .version
        .as_deref()
        .map(str::trim)
        .filter(|version| !version.is_empty())
    {
        label.push_str(&format!(" v{version}"));
    }
    label
}

fn disc_toc(toc: &Toc) -> DiscToc {
    DiscToc {
        offsets: toc.audio_tracks().map(|track| track.start_lba).collect(),
        leadout: toc.leadout_lba,
    }
}

/// Reads a drive's status, `None` when the device cannot be opened or
/// read. A free function so the blocking ioctls can run outside the
/// drive lock.
fn read_drive_info(path: &str) -> Option<DeviceInfo> {
    Device::open(path).ok()?.info().ok()
}

fn read_toc(path: &str) -> Result<(Toc, Option<String>), String> {
    let device = Device::open(path).map_err(|e| e.to_string())?;
    let toc = device.toc().map_err(|e| e.to_string())?;
    let mcn = device.mcn().ok().flatten();
    Ok((toc, mcn))
}

fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let value = bytes as f64;
    if value < 1024.0 * 1024.0 {
        return format!("{:.1} KiB", value / 1024.0);
    }
    if value < 1024.0 * 1024.0 * 1024.0 {
        return format!("{:.1} MiB", value / (1024.0 * 1024.0));
    }
    format!("{:.1} GiB", value / (1024.0 * 1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn demo_state(dir: &Path, force: bool) -> Arc<AppState> {
        demo_state_with(dir, force, None)
    }

    fn demo_state_with(dir: &Path, force: bool, drive_dirs: Option<bool>) -> Arc<AppState> {
        AppState::new(
            dir.to_path_buf(),
            None,
            Format::Wav,
            force,
            Template::default(),
            drive_dirs,
            true,
        )
        .expect("demo state constructs")
    }

    fn wait_idle(state: &AppState, index: usize) {
        for _ in 0..1000 {
            let view = state.view();
            let Some(rip) = view.drives.get(index).and_then(|d| d.rip.as_ref()) else {
                return;
            };
            if !rip.active {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("rip did not finish in time");
    }

    #[test]
    fn demo_state_is_preloaded() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), false);
        let view = state.view();

        assert_eq!(view.drives.len(), 1);
        let drive = &view.drives[0];
        assert_eq!(drive.path, DEMO_DEVICE);
        assert!(drive.demo);
        assert!(drive.status.contains("disc"));
        assert!(drive.toc.is_some());
        assert_eq!(drive.toc.as_ref().unwrap().tracks.len(), 5);
        assert_eq!(drive.mcn.as_deref(), Some(DEMO_MCN));
        assert!(drive.register_url.is_some());
        assert_eq!(drive.matches.len(), 2);
        assert_eq!(drive.matches[0].album, "Demo Album");
        assert!(drive.cover.is_some());
    }

    #[test]
    fn settings_update_the_state_view() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), false);

        state.set_format(Format::Flac);
        state.set_force(true);

        let view = state.view();
        assert_eq!(view.format, "flac");
        assert!(view.force);
    }

    #[test]
    fn select_match_switches_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), false);

        state.select_match(0, 1).unwrap();

        let view = state.view();
        let drive = &view.drives[0];
        assert_eq!(drive.match_sel, 1);
        assert_eq!(drive.matches[1].album, "Demo Album (Reissue)");
        assert!(drive.cover.is_some());
    }

    #[test]
    fn tags_can_be_edited() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), false);

        state
            .set_tags(
                0,
                Some("Edited Album".to_string()),
                Some("Edited Band".to_string()),
                None,
                Some("1999".to_string()),
                vec![
                    (Some("New Intro".to_string()), None),
                    (None, Some("New Artist".to_string())),
                ],
            )
            .unwrap();

        let meta = state
            .drives
            .lock()
            .unwrap()
            .first()
            .unwrap()
            .meta
            .clone()
            .unwrap();
        assert_eq!(meta.album, "Edited Album");
        assert_eq!(meta.artist, "Edited Band");
        assert_eq!(meta.year.as_deref(), Some("1999"));
        assert_eq!(meta.tracks[0].title, "New Intro");
        assert_eq!(meta.tracks[1].artist.as_deref(), Some("New Artist"));
    }

    #[test]
    fn rejects_unknown_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), false);
        assert!(state.rip(0, vec![9]).is_err());
    }

    #[test]
    fn rejects_double_rip() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), true);

        state.rip(0, vec![1]).unwrap();
        assert!(state.rip(0, vec![1]).is_err());
        state.stop_rip(0).unwrap();
        wait_idle(&state, 0);
    }

    #[test]
    fn rips_a_demo_track_to_wav() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), true);

        state.rip(0, vec![3]).unwrap();
        wait_idle(&state, 0);

        let path = dir.path().join("The Demo Band/Demo Album/03 Short One.wav");
        assert!(path.exists());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");

        let view = state.view();
        let rip = view.drives[0].rip.as_ref().unwrap();
        assert!(!rip.active);
        assert!(rip.summary.is_some());
        assert!(rip.summary.as_ref().unwrap().contains("ripped"));
    }

    #[test]
    fn drive_dirs_on_rips_into_a_per_drive_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state_with(dir.path(), true, Some(true));

        state.rip(0, vec![3]).unwrap();
        wait_idle(&state, 0);

        let path = dir
            .path()
            .join("sr-demo/The Demo Band/Demo Album/03 Short One.wav");
        assert!(path.exists());
        assert!(!dir.path().join("The Demo Band").exists());
    }

    #[test]
    fn drive_dirs_off_keeps_the_flat_layout() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state_with(dir.path(), true, Some(false));

        state.rip(0, vec![3]).unwrap();
        wait_idle(&state, 0);

        let path = dir.path().join("The Demo Band/Demo Album/03 Short One.wav");
        assert!(path.exists());
        assert!(!dir.path().join("sr-demo").exists());
    }

    #[test]
    fn stop_stops_a_running_rip() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), true);

        state.rip(0, vec![1]).unwrap();
        state.stop_rip(0).unwrap();
        wait_idle(&state, 0);

        let view = state.view();
        let rip = view.drives[0].rip.as_ref().unwrap();
        assert!(!rip.active);
        assert!(rip.summary.as_ref().unwrap().contains("stopped"));
    }

    #[test]
    fn shutdown_stops_a_running_rip() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), true);

        state.start();
        state.rip(0, vec![1]).unwrap();
        state.shutdown();

        let view = state.view();
        let rip = view.drives[0].rip.as_ref().unwrap();
        assert!(!rip.active);
    }

    #[test]
    fn rejects_duplicate_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), false);
        assert!(state.rip(0, vec![3, 3]).is_err());
    }

    #[test]
    fn load_refuses_while_ripping() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), true);

        state.rip(0, vec![1]).unwrap();
        assert!(state.load_toc(0).is_err());
        state.stop_rip(0).unwrap();
        wait_idle(&state, 0);
    }

    #[test]
    fn tags_refuse_while_ripping() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), true);

        state.rip(0, vec![1]).unwrap();
        assert!(
            state
                .set_tags(0, Some("X".into()), None, None, None, vec![])
                .is_err()
        );
        state.stop_rip(0).unwrap();
        wait_idle(&state, 0);
    }

    #[test]
    fn eject_refuses_the_demo_drive() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), false);
        assert!(state.eject(0).is_err());
    }

    fn loaded_drive() -> Drive {
        Drive {
            path: "/dev/sr9".into(),
            label: "sr9".into(),
            status: "disc present".into(),
            disc: Some("audio".into()),
            demo: false,
            loading_toc: false,
            toc_failed: false,
            toc: Some(DemoDisc::new().toc),
            mcn: Some("MCN".into()),
            register_url: Some("https://musicbrainz.org".into()),
            lookup_status: "found".into(),
            candidates: Vec::new(),
            match_sel: 0,
            meta: Some(blank_meta()),
            cover: None,
            cover_data: None,
            cover_fetching: false,
            meta_edited: true,
            rip: None,
            meta_gen: 2,
            last_status: Some(DriveStatus::DiscOk),
        }
    }

    fn drive_info(status: DriveStatus) -> DeviceInfo {
        DeviceInfo {
            path: "/dev/sr9".into(),
            drive_status: status,
            disc_status: None,
            vendor: None,
            model: None,
            version: None,
        }
    }

    #[test]
    fn an_unreadable_blip_keeps_the_loaded_disc() {
        let mut drive = loaded_drive();
        drive.apply_info(None);

        assert_eq!(drive.status, "unreadable");
        assert_eq!(drive.disc, None);
        assert_eq!(drive.last_status, None);
        assert!(drive.toc.is_some());
        assert_eq!(drive.lookup_status, "found");
        assert!(drive.meta_edited);
    }

    #[test]
    fn a_positive_empty_tray_clears_the_loaded_disc() {
        let mut drive = loaded_drive();
        drive.apply_poll(Some(drive_info(DriveStatus::NoDisc)));

        assert_eq!(drive.status, "no disc");
        assert!(drive.toc.is_none());
        assert!(drive.mcn.is_none());
        assert_eq!(drive.lookup_status, "no disc");
        assert!(!drive.meta_edited);
    }

    #[test]
    fn a_transient_status_keeps_the_loaded_disc() {
        for status in [DriveStatus::NoInfo, DriveStatus::DriveNotReady] {
            let mut drive = loaded_drive();
            drive.apply_poll(Some(drive_info(status)));

            assert_eq!(drive.last_status, Some(status));
            assert!(drive.toc.is_some(), "{status:?} must not clear the disc");
        }
    }

    #[test]
    fn restart_rearms_the_meta_channel() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), false);

        state.start();
        state.shutdown();
        assert!(state.meta_rx.lock().unwrap().is_none());

        state.arm_meta();
        assert!(state.meta_rx.lock().unwrap().is_some());

        state.start();
        state.shutdown();
    }

    #[test]
    fn a_dead_rip_forwarder_is_reaped_on_poll() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), false);
        {
            let forwarder = std::thread::spawn(|| ());
            while !forwarder.is_finished() {
                std::thread::sleep(Duration::from_millis(1));
            }
            let mut drives = state.drives.lock().unwrap();
            let drive = drives.first_mut().unwrap();
            drive.rip = Some(RipControl {
                active: true,
                stopped: false,
                tracks: vec![1],
                out_dir: dir.path().to_path_buf(),
                format: Format::Wav,
                force: false,
                toc: DemoDisc::new().toc,
                states: vec![RipTrackState {
                    number: 1,
                    state: TrackState::Pending,
                }],
                summary: None,
                stop: Arc::new(AtomicBool::new(false)),
                worker: None,
                forwarder: Some(forwarder),
            });
        }
        state.poll_drive(0);

        let view = state.view();
        let rip = view.drives[0].rip.as_ref().unwrap();
        assert!(!rip.active);
        assert!(rip.summary.as_deref().unwrap().contains("died"));
    }

    #[test]
    fn start_and_shutdown_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let state = demo_state(dir.path(), false);

        state.start();
        state.start();
        std::thread::sleep(Duration::from_millis(100));
        state.shutdown();
        state.shutdown();
    }
}
