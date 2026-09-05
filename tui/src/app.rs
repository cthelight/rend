//! Application state: drives, table of contents, selection, and ripping.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Margin, Position, Rect};

use rend_core::{Device, DeviceInfo, DriveStatus, Toc};
use rend_encode::{Format, ffmpeg_available};
use rend_meta::{DiscMeta, cover_art, disc_id, lookup_disc};

use crate::demo::{DEMO_DEVICE, DEMO_LABEL, DEMO_MCN, DemoDisc, DemoSource, demo_cover, demo_meta};
use crate::rip::{self, RipEvent, RipJob, RipSource};

/// Which panel has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Drives,
    Tracks,
}

/// Where the mouse pointer is, for hover highlighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Hover {
    #[default]
    None,
    Drive(usize),
    Track(usize),
    RipSelected,
    RipAll,
    Eject,
    Stop,
}

/// Screen regions for mouse hit-testing, filled in during each draw.
#[derive(Debug, Clone, Copy, Default)]
pub struct Regions {
    pub drives: Rect,
    pub tracks: Rect,
    pub rip_selected: Rect,
    pub rip_all: Rect,
    pub eject: Rect,
    pub stop: Rect,
}

impl Regions {
    /// The drive index under the given position, if any.
    pub fn drive_at(&self, col: u16, row: u16, scroll: usize, count: usize) -> Option<usize> {
        self.row_at(self.drives, col, row, scroll, count)
    }

    /// The track index under the given position, if any.
    pub fn track_at(&self, col: u16, row: u16, scroll: usize, count: usize) -> Option<usize> {
        self.row_at(self.tracks, col, row, scroll, count)
    }

    fn row_at(&self, rect: Rect, col: u16, row: u16, scroll: usize, count: usize) -> Option<usize> {
        let inner = rect.inner(Margin::new(1, 1));
        if !inner.contains(Position::new(col, row)) {
            return None;
        }
        let idx = (row - inner.y) as usize + scroll;
        (idx < count).then_some(idx)
    }
}

/// Per-track rip state.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum TrackState {
    /// Not part of the current rip.
    #[default]
    Idle,
    /// Being read; bytes transferred so far.
    Ripping { bytes_done: u64, bytes_total: u64 },
    /// Finished; total bytes written.
    Done { bytes: u64 },
    /// Failed with an error message.
    Failed(String),
    /// Skipped (e.g. output exists and force is off).
    Skipped(String),
}

/// Aggregate rip progress for the current disc.
#[derive(Debug)]
pub struct RipState {
    pub active: bool,
    pub states: Vec<TrackState>,
    /// Smoothed read speed in bytes/second.
    pub speed: f64,
    /// Final summary of the last rip, if any.
    pub summary: Option<String>,
    sample: Option<(u64, Instant)>,
}

impl RipState {
    fn fresh(track_count: usize) -> Self {
        Self {
            active: false,
            states: vec![TrackState::Idle; track_count],
            speed: 0.0,
            summary: None,
            sample: None,
        }
    }

    /// Total bytes written and expected across this rip.
    pub fn totals(&self) -> (u64, u64) {
        let mut done = 0u64;
        let mut total = 0u64;
        for state in &self.states {
            match state {
                TrackState::Ripping {
                    bytes_done,
                    bytes_total,
                } => {
                    done += bytes_done;
                    total += bytes_total;
                }
                TrackState::Done { bytes } => {
                    done += bytes;
                    total += bytes;
                }
                _ => {}
            }
        }
        (done, total)
    }

    /// Recomputes the smoothed read speed from the newest progress sample.
    pub fn update_speed(&mut self) {
        let now = Instant::now();
        let done: u64 = self
            .states
            .iter()
            .map(|s| match s {
                TrackState::Ripping { bytes_done, .. } => *bytes_done,
                TrackState::Done { bytes } => *bytes,
                _ => 0,
            })
            .sum();
        if let Some((prev, when)) = self.sample {
            let dt = now.duration_since(when).as_secs_f64();
            if dt > 0.05 {
                let inst = done.saturating_sub(prev) as f64 / dt;
                self.speed = if self.speed == 0.0 {
                    inst
                } else {
                    self.speed * 0.7 + inst * 0.3
                };
                self.sample = Some((done, now));
            }
        } else {
            self.sample = Some((done, now));
        }
    }
}

impl Default for RipState {
    fn default() -> Self {
        Self::fresh(0)
    }
}

/// A CD-ROM drive, or the simulated drive in demo mode.
#[derive(Debug)]
pub struct Drive {
    pub path: String,
    pub label: String,
    pub status: String,
    pub disc: Option<String>,
    device: Option<Device>,
    last_status: Option<DriveStatus>,
}

impl Drive {
    fn real(device: Device) -> Self {
        let mut drive = Self {
            path: device.path().to_string(),
            label: "unknown".into(),
            status: String::new(),
            disc: None,
            device: Some(device),
            last_status: None,
        };
        drive.refresh();
        drive
    }

    fn demo() -> Self {
        Self {
            path: DEMO_DEVICE.into(),
            label: DEMO_LABEL.into(),
            status: "disc present".into(),
            disc: Some("audio".into()),
            device: None,
            last_status: Some(DriveStatus::DiscOk),
        }
    }

    /// `true` for the simulated drive, which has no real device behind it.
    pub fn is_demo(&self) -> bool {
        self.device.is_none()
    }

    fn refresh(&mut self) {
        let Some(device) = self.device.as_ref() else {
            return;
        };
        match device.info() {
            Ok(info) => {
                self.label = drive_label(&info);
                self.status = info.drive_status.to_string();
                self.disc = info.disc_status.map(|d| d.to_string());
                self.last_status = Some(info.drive_status);
            }
            Err(_) => {
                self.status = "unreadable".into();
            }
        }
    }

    fn toc(&self) -> Result<Toc, String> {
        match &self.device {
            Some(device) => device.toc().map_err(|e| e.to_string()),
            None => Ok(DemoDisc::new().toc),
        }
    }

    fn mcn(&self) -> Option<String> {
        match &self.device {
            Some(device) => device.mcn().ok().flatten(),
            None => Some(DEMO_MCN.into()),
        }
    }

    fn eject(&self) -> Result<(), String> {
        match &self.device {
            Some(device) => device.eject().map_err(|e| e.to_string()),
            None => Err("cannot eject the simulated drive".into()),
        }
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
        .filter(|v| !v.is_empty())
    {
        label.push_str(&format!(" v{version}"));
    }
    label
}

/// The outcome of a background metadata lookup. `id` records which lookup
/// produced it, so results from a previous disc can be discarded.
enum MetaEvent {
    /// The lookup succeeded.
    Resolved {
        id: u64,
        meta: DiscMeta,
        cover: Option<Vec<u8>>,
    },
    /// The lookup failed.
    Failed { id: u64, reason: String },
}

/// Everything about the rip running (or last run) on a single drive: its
/// per-track state plus the worker thread and stop flag that drive it.
struct DriveRip {
    state: RipState,
    /// Where this drive's WAV files go (a per-drive subdir when several
    /// drives are present, so track numbers never collide).
    out_dir: PathBuf,
    /// The TOC the rip was started on, for mapping track numbers to indices.
    toc: Toc,
    rx: Option<mpsc::Receiver<RipEvent>>,
    thread: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

/// The TUI application state.
pub struct App {
    pub running: bool,
    pub focus: Focus,
    pub drives: Vec<Drive>,
    pub drive_sel: usize,
    pub drives_scroll: usize,
    pub toc: Option<Toc>,
    pub disc_id: Option<String>,
    /// The looked-up metadata for the loaded disc, once resolved.
    pub meta: Option<DiscMeta>,
    /// The disc's cover art, once fetched.
    cover: Option<Vec<u8>>,
    /// Pending results from the metadata lookup worker.
    meta_rx: Option<mpsc::Receiver<MetaEvent>>,
    /// Generation counter invalidating in-flight lookups on disc changes.
    meta_gen: u64,
    pub track_sel: usize,
    pub tracks_scroll: usize,
    /// Rips keyed by drive index; several drives can rip at once.
    rips: HashMap<usize, DriveRip>,
    /// The format new rips write in.
    pub format: Format,
    pub force: bool,
    pub status: Option<String>,
    pub out_dir: PathBuf,
    pub hover: Hover,
    pub regions: Regions,
    last_click: Option<(u16, u16, Instant)>,
}

impl App {
    /// Creates the app, discovering drives (or the simulated drive).
    pub fn new(
        device: Option<&str>,
        out_dir: PathBuf,
        force: bool,
        format: Format,
        demo: bool,
    ) -> Self {
        let mut app = Self {
            running: true,
            focus: Focus::Drives,
            drives: Vec::new(),
            drive_sel: 0,
            drives_scroll: 0,
            toc: None,
            disc_id: None,
            meta: None,
            cover: None,
            meta_rx: None,
            meta_gen: 0,
            track_sel: 0,
            tracks_scroll: 0,
            rips: HashMap::new(),
            format,
            force,
            status: None,
            out_dir,
            hover: Hover::None,
            regions: Regions::default(),
            last_click: None,
        };

        if demo {
            app.drives.push(Drive::demo());
        } else {
            let devices = match device {
                Some(path) => match Device::open(path) {
                    Ok(dev) => vec![dev],
                    Err(e) => {
                        app.status = Some(e.to_string());
                        Vec::new()
                    }
                },
                None => match Device::discover() {
                    Ok(devs) if !devs.is_empty() => devs,
                    Ok(_) => {
                        app.status = Some("no CD-ROM devices found — try --demo".into());
                        Vec::new()
                    }
                    Err(e) => {
                        app.status = Some(e.to_string());
                        Vec::new()
                    }
                },
            };
            app.drives = devices.into_iter().map(Drive::real).collect();
        }

        if !app.drives.is_empty() {
            app.select_drive(0);
        }
        app
    }

    /// Selects a drive and loads its table of contents.
    ///
    /// Selecting is always allowed: any other drive keeps ripping in the
    /// background, and each drive remembers its own rip state.
    pub fn select_drive(&mut self, idx: usize) {
        if idx >= self.drives.len() {
            return;
        }
        self.drive_sel = idx;
        self.track_sel = 0;
        self.tracks_scroll = 0;
        self.toc = None;
        self.disc_id = None;
        self.meta = None;
        self.cover = None;
        match self.drives[idx].toc() {
            Ok(toc) => {
                self.disc_id = self.drives[idx].mcn();
                self.lookup_metadata(&toc);
                self.toc = Some(toc);
                self.focus = Focus::Tracks;
            }
            Err(e) => self.status = Some(format!("no TOC: {e}")),
        }
    }

    /// Starts looking up the disc's metadata: built in for the simulated
    /// disc, on a background thread for a real one.
    fn lookup_metadata(&mut self, toc: &Toc) {
        let lbas: Vec<u32> = toc.audio_tracks().map(|t| t.start_lba).collect();
        if lbas.is_empty() {
            self.status = Some("no audio tracks to look up".into());
            return;
        }
        if self.drives[self.drive_sel].is_demo() {
            let meta = demo_meta();
            let year = meta.year.as_deref().unwrap_or("");
            self.status = Some(format!("{} — {} ({year})", meta.artist, meta.album));
            self.meta = Some(meta);
            self.cover = Some(demo_cover());
            return;
        }
        let cddb_id = disc_id(&lbas);
        let lookup_id = self.meta_gen + 1;
        self.meta_gen = lookup_id;
        self.status = Some("looking up metadata…".into());
        let (tx, rx) = mpsc::channel();
        self.meta_rx = Some(rx);
        std::thread::Builder::new()
            .name("rend-meta-lookup".into())
            .spawn(move || {
                let meta = match lookup_disc(&cddb_id) {
                    Ok(meta) => meta,
                    Err(e) => {
                        let _ = tx.send(MetaEvent::Failed {
                            id: lookup_id,
                            reason: e.to_string(),
                        });
                        return;
                    }
                };
                let cover = cover_art(&meta.release_id).ok().flatten();
                let _ = tx.send(MetaEvent::Resolved {
                    id: lookup_id,
                    meta,
                    cover,
                });
            })
            .ok();
    }

    /// Applies pending metadata-lookup results for the selected disc.
    pub fn drain_meta_events(&mut self) {
        let Some(rx) = self.meta_rx.as_ref() else {
            return;
        };
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        for event in events {
            match event {
                MetaEvent::Resolved { id, meta, cover } => {
                    if id != self.meta_gen {
                        continue;
                    }
                    let year = meta
                        .year
                        .as_deref()
                        .map(|y| format!(" ({y})"))
                        .unwrap_or_default();
                    self.status = Some(format!("{} — {}{year}", meta.artist, meta.album));
                    self.meta = Some(meta);
                    self.cover = cover;
                    self.meta_rx = None;
                }
                MetaEvent::Failed { id, reason } => {
                    if id != self.meta_gen {
                        continue;
                    }
                    self.status = Some(format!("metadata lookup failed: {reason}"));
                    self.meta_rx = None;
                }
            }
        }
    }

    /// Handles a key press.
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if key.code == KeyCode::Char('c') {
                self.running = false;
            }
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.running = false,
            KeyCode::Char('f') => {
                self.force = !self.force;
                self.status = Some(format!(
                    "force (overwrite) {}",
                    if self.force { "on" } else { "off" }
                ));
            }
            KeyCode::Char('o') => {
                self.format = self.format.next();
                self.status = Some(format!("output format: {}", self.format.label()));
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Drives => Focus::Tracks,
                    Focus::Tracks => Focus::Drives,
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Enter => match self.focus {
                Focus::Drives => self.select_drive(self.drive_sel),
                Focus::Tracks => self.rip_selected_track(),
            },
            KeyCode::Char('r') => self.rip_selected_track(),
            KeyCode::Char('a') => self.rip_all_audio(),
            KeyCode::Char('e') => self.eject(),
            KeyCode::Char('s') => self.stop_rip(),
            _ => {}
        }
    }

    /// Handles a mouse event (click, double-click, move, scroll).
    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let hover = self.hover_at(mouse);
                let double = self.is_double_click(mouse);
                self.focus = match hover {
                    Hover::Drive(_) => Focus::Drives,
                    Hover::Track(_) => Focus::Tracks,
                    _ => self.focus,
                };
                match hover {
                    Hover::Drive(i) => self.select_drive(i),
                    Hover::Track(i) => {
                        self.track_sel = i;
                        if double {
                            self.rip_selected_track();
                        }
                    }
                    Hover::RipSelected => self.rip_selected_track(),
                    Hover::RipAll => self.rip_all_audio(),
                    Hover::Eject => self.eject(),
                    Hover::Stop => self.stop_rip(),
                    Hover::None => {}
                }
            }
            MouseEventKind::Moved => self.hover = self.hover_at(mouse),
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let down = mouse.kind == MouseEventKind::ScrollDown;
                let pos = Position::new(mouse.column, mouse.row);
                if self.regions.drives.contains(pos) {
                    self.focus = Focus::Drives;
                    self.drives_scroll = scroll(
                        self.drives_scroll,
                        down,
                        self.drives.len(),
                        self.visible(self.regions.drives),
                    );
                } else if self.regions.tracks.contains(pos) {
                    self.focus = Focus::Tracks;
                    self.tracks_scroll = scroll(
                        self.tracks_scroll,
                        down,
                        self.track_count(),
                        self.visible(self.regions.tracks),
                    );
                }
            }
            _ => {}
        }
    }

    /// True if this click is a double-click (same spot, within 300 ms).
    fn is_double_click(&mut self, mouse: MouseEvent) -> bool {
        let now = Instant::now();
        let double = self
            .last_click
            .filter(|(col, row, _)| *col == mouse.column && *row == mouse.row)
            .is_some_and(|(_, _, when)| now.duration_since(when) < Duration::from_millis(300));
        self.last_click = Some((mouse.column, mouse.row, now));
        double
    }

    /// Applies all pending rip worker events, across every drive that is
    /// currently ripping.
    pub fn drain_rip_events(&mut self) {
        let paths: Vec<String> = self.drives.iter().map(|d| d.path.clone()).collect();
        for (idx, rip) in self.rips.iter_mut() {
            let Some(rx) = rip.rx.as_ref() else {
                continue;
            };
            let mut events = Vec::new();
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
            for event in events {
                match event {
                    RipEvent::TrackStarted { number } => {
                        if let Some(i) = track_index(&rip.toc, number) {
                            if let Some(slot) = rip.state.states.get_mut(i) {
                                *slot = TrackState::Ripping {
                                    bytes_done: 0,
                                    bytes_total: 0,
                                };
                            }
                        }
                    }
                    RipEvent::Progress {
                        number,
                        bytes_done,
                        bytes_total,
                    } => {
                        if let Some(i) = track_index(&rip.toc, number) {
                            if let Some(slot) = rip.state.states.get_mut(i) {
                                *slot = TrackState::Ripping {
                                    bytes_done,
                                    bytes_total,
                                };
                            }
                        }
                        rip.state.update_speed();
                    }
                    RipEvent::TrackDone {
                        number,
                        path,
                        bytes,
                    } => {
                        if let Some(i) = track_index(&rip.toc, number) {
                            if let Some(slot) = rip.state.states.get_mut(i) {
                                *slot = TrackState::Done { bytes };
                            }
                        }
                        self.status = Some(format!("track {number:02}: wrote {}", path.display()));
                        rip.state.update_speed();
                    }
                    RipEvent::TrackFailed { number, error } => {
                        if let Some(i) = track_index(&rip.toc, number) {
                            if let Some(slot) = rip.state.states.get_mut(i) {
                                *slot = TrackState::Failed(error);
                            }
                        }
                    }
                    RipEvent::TrackSkipped { number, reason } => {
                        if let Some(i) = track_index(&rip.toc, number) {
                            if let Some(slot) = rip.state.states.get_mut(i) {
                                *slot = TrackState::Skipped(reason);
                            }
                        }
                    }
                    RipEvent::Finished {
                        failed,
                        total,
                        stopped,
                    } => {
                        let drive = paths.get(*idx).map(String::as_str).unwrap_or("");
                        let summary = if stopped {
                            "ripping stopped".to_string()
                        } else if failed == 0 {
                            format!(
                                "done — {total} track(s) on {drive} to {}",
                                rip.out_dir.display()
                            )
                        } else {
                            format!("finished — {failed} of {total} track(s) failed on {drive}")
                        };
                        rip.state.active = false;
                        rip.state.speed = 0.0;
                        rip.state.sample = None;
                        rip.state.summary = Some(summary);
                        rip.rx = None;
                        if let Some(thread) = rip.thread.take() {
                            let _ = thread.join();
                        }
                    }
                }
            }
        }
    }

    /// Re-queries drive statuses; invalidates the TOC if the disc changed.
    pub fn refresh_drives(&mut self) {
        if self.drives.iter().all(Drive::is_demo) || self.any_rip_active() {
            return;
        }
        let selected = self.drive_sel;
        let mut changed = false;
        for (i, drive) in self.drives.iter_mut().enumerate() {
            let prev = drive.last_status;
            drive.refresh();
            if i == selected && self.toc.is_some() && prev != drive.last_status {
                changed = true;
            }
        }
        if changed {
            self.toc = None;
            self.disc_id = None;
            self.meta = None;
            self.cover = None;
            self.meta_gen += 1;
            if let Some(rip) = self.rips.get_mut(&selected) {
                rip.state.states.clear();
            }
            let path = self.drives[selected].path.clone();
            self.status = Some(format!(
                "disc state changed on {path} — press enter to reload"
            ));
        }
    }

    /// Stops every in-flight rip and waits for all workers to exit.
    pub fn shutdown(&mut self) {
        for rip in self.rips.values_mut() {
            rip.stop.store(true, Ordering::SeqCst);
        }
        for rip in self.rips.values_mut() {
            if let Some(thread) = rip.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// `true` if any drive has a rip in progress.
    pub fn any_rip_active(&self) -> bool {
        self.rips.values().any(|r| r.state.active)
    }

    /// Whether the given drive has a rip in progress.
    pub fn drive_ripping(&self, idx: usize) -> bool {
        self.rips.get(&idx).is_some_and(|r| r.state.active)
    }

    /// The rip state of the selected drive, if that drive has one.
    pub fn selected_rip_state(&self) -> Option<&RipState> {
        self.rips.get(&self.drive_sel).map(|r| &r.state)
    }

    /// The output directory of the selected drive's rip, if any.
    pub fn selected_rip_out_dir(&self) -> Option<&std::path::Path> {
        self.rips.get(&self.drive_sel).map(|r| r.out_dir.as_path())
    }

    /// Total bytes written and expected across the selected drive's rip.
    pub fn rip_totals(&self) -> (u64, u64) {
        self.rips
            .get(&self.drive_sel)
            .map(|r| r.state.totals())
            .unwrap_or((0, 0))
    }

    /// The number of tracks currently loaded, if any.
    pub fn track_count(&self) -> usize {
        self.toc.as_ref().map_or(0, |t| t.tracks.len())
    }

    /// The number of visible rows in a bordered panel of the given rect.
    pub fn visible(&self, rect: Rect) -> usize {
        rect.height.saturating_sub(2) as usize
    }

    pub fn rip_selected_track(&mut self) {
        let Some(toc) = self.toc.as_ref() else {
            self.status = Some("no disc loaded".into());
            return;
        };
        let Some(track) = toc.tracks.get(self.track_sel) else {
            self.status = Some("no track selected".into());
            return;
        };
        if !track.is_audio() {
            self.status = Some(format!("track {} is a data track, not audio", track.number));
            return;
        }
        self.start_rip(vec![track.number]);
    }

    pub fn rip_all_audio(&mut self) {
        let Some(toc) = self.toc.as_ref() else {
            self.status = Some("no disc loaded".into());
            return;
        };
        let tracks: Vec<u8> = toc.audio_tracks().map(|t| t.number).collect();
        if tracks.is_empty() {
            self.status = Some("no audio tracks on this disc".into());
            return;
        }
        self.start_rip(tracks);
    }

    pub fn stop_rip(&mut self) {
        if let Some(rip) = self.rips.get_mut(&self.drive_sel) {
            if !rip.state.active {
                return;
            }
            rip.stop.store(true, Ordering::SeqCst);
            self.status = Some("stopping rip…".into());
        }
    }

    pub fn eject(&mut self) {
        if self.drive_ripping(self.drive_sel) {
            self.status = Some("stop the rip before ejecting".into());
            return;
        }
        let idx = self.drive_sel;
        match self.drives[idx].eject() {
            Ok(()) => {
                self.toc = None;
                self.disc_id = None;
                self.meta = None;
                self.cover = None;
                self.meta_gen += 1;
                self.rips.remove(&idx);
                self.status = Some(format!("ejecting {}", self.drives[idx].path));
            }
            Err(e) => self.status = Some(e),
        }
    }

    fn start_rip(&mut self, tracks: Vec<u8>) {
        if self.drive_ripping(self.drive_sel) {
            self.status = Some("a rip is already running on this drive".into());
            return;
        }
        if self.format.requires_ffmpeg() && !ffmpeg_available() {
            self.status =
                Some("ffmpeg not found in PATH — install it, or switch to wav with o".into());
            return;
        }
        let Some(toc) = self.toc.clone() else {
            return;
        };
        let idx = self.drive_sel;
        let source = if self.drives[idx].is_demo() {
            RipSource::Demo(DemoSource::new())
        } else {
            let path = self.drives[idx].path.clone();
            match Device::open(&path) {
                Ok(dev) => RipSource::Device(dev),
                Err(e) => {
                    self.status = Some(e.to_string());
                    return;
                }
            }
        };
        // When several drives are present, give each its own subdir so track
        // numbers never collide across drives.
        let out_dir = if self.drives.len() > 1 {
            let base = self.drives[idx].path.rsplit('/').next().unwrap_or("drive");
            self.out_dir.join(base)
        } else {
            self.out_dir.clone()
        };
        let count = tracks.len();
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let job = RipJob {
            source,
            toc: toc.clone(),
            tracks,
            out_dir: out_dir.clone(),
            format: self.format,
            force: self.force,
            stop: stop.clone(),
            meta: self.meta.clone(),
            cover: self.cover.clone(),
        };
        let thread = rip::spawn(job, tx);
        let mut state = RipState::fresh(self.track_count());
        state.active = true;
        let rip = DriveRip {
            state,
            out_dir: out_dir.clone(),
            toc,
            rx: Some(rx),
            thread: Some(thread),
            stop,
        };
        self.rips.insert(idx, rip);
        self.status = Some(format!(
            "ripping {} track(s) to {}",
            count,
            out_dir.display()
        ));
    }

    fn move_selection(&mut self, delta: i32) {
        let len = match self.focus {
            Focus::Drives => self.drives.len(),
            Focus::Tracks => self.track_count(),
        };
        if len == 0 {
            return;
        }
        let sel = match self.focus {
            Focus::Drives => self.drive_sel,
            Focus::Tracks => self.track_sel,
        };
        let next = (sel as i32 + delta).clamp(0, (len - 1) as i32) as usize;
        match self.focus {
            Focus::Drives => {
                if next != self.drive_sel {
                    self.select_drive(next);
                }
            }
            Focus::Tracks => self.track_sel = next,
        }
    }

    fn hover_at(&self, mouse: MouseEvent) -> Hover {
        let pos = Position::new(mouse.column, mouse.row);
        if let Some(i) = self.regions.drive_at(
            mouse.column,
            mouse.row,
            self.drives_scroll,
            self.drives.len(),
        ) {
            return Hover::Drive(i);
        }
        if let Some(i) = self.regions.track_at(
            mouse.column,
            mouse.row,
            self.tracks_scroll,
            self.track_count(),
        ) {
            return Hover::Track(i);
        }
        if self.regions.rip_selected.contains(pos) {
            return Hover::RipSelected;
        }
        if self.regions.rip_all.contains(pos) {
            return Hover::RipAll;
        }
        if self.regions.eject.contains(pos) {
            return Hover::Eject;
        }
        if self.regions.stop.contains(pos) {
            return Hover::Stop;
        }
        Hover::None
    }
}

fn scroll(current: usize, down: bool, count: usize, visible: usize) -> usize {
    let max = count.saturating_sub(visible);
    if down {
        (current + 1).min(max)
    } else {
        current.saturating_sub(1)
    }
}

/// The index of the given track number within a TOC, if present.
fn track_index(toc: &Toc, number: u8) -> Option<usize> {
    toc.tracks.iter().position(|t| t.number == number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;

    fn demo_app(dir: &std::path::Path) -> App {
        App::new(None, dir.to_path_buf(), false, Format::default(), true)
    }

    fn drain_until_finished(app: &mut App, timeout: Duration) {
        let start = Instant::now();
        while app.selected_rip_state().is_some_and(|r| r.active) {
            app.drain_rip_events();
            if start.elapsed() > timeout {
                panic!("rip did not finish in time");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        app.drain_rip_events();
    }

    /// Drains until every drive's rip has finished.
    fn drain_all_until_idle(app: &mut App, timeout: Duration) {
        let start = Instant::now();
        while app.any_rip_active() {
            app.drain_rip_events();
            if start.elapsed() > timeout {
                panic!("rips did not finish in time");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        app.drain_rip_events();
    }

    #[test]
    fn demo_app_starts_with_toc() {
        let dir = tempfile::tempdir().unwrap();
        let app = demo_app(dir.path());

        assert_eq!(app.drives.len(), 1);
        assert_eq!(app.drives[0].path, DEMO_DEVICE);
        assert!(app.drives[0].is_demo());
        assert_eq!(app.focus, Focus::Tracks);
        let toc = app.toc.as_ref().unwrap();
        assert_eq!(toc.tracks.len(), 5);
        assert_eq!(app.disc_id.as_deref(), Some(DEMO_MCN));
        // The simulated disc's metadata is available immediately, no network.
        let meta = app.meta.as_ref().unwrap();
        assert_eq!(meta.album, "Demo Album");
        assert_eq!(meta.tracks.len(), 5);
        assert!(app.cover.is_some());
    }

    #[test]
    fn rips_short_demo_track() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        app.track_sel = 2;
        app.rip_selected_track();
        assert!(app.drive_ripping(app.drive_sel));
        drain_until_finished(&mut app, Duration::from_secs(10));

        let state = app.selected_rip_state().unwrap();
        assert!(matches!(&state.states[2], TrackState::Done { .. }));
        assert!(state.summary.is_some());
        let flac = dir.path().join("track03.flac");
        let bytes = std::fs::read(&flac).unwrap();
        assert_eq!(&bytes[0..4], b"fLaC");

        // The looked-up demo metadata was embedded into the file.
        let file = lofty::read_from_path(&flac).unwrap();
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
            tag.album().map(std::borrow::Cow::into_owned).as_deref(),
            Some("Demo Album")
        );
        let pic = tag
            .get_picture_type(lofty::picture::PictureType::CoverFront)
            .unwrap();
        assert_eq!(pic.data(), demo_cover());
    }

    #[test]
    fn rips_two_demo_drives_in_parallel() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        // Add a second simulated drive to exercise the parallel path. Give it
        // a distinct path so its per-drive subdir differs from drive 0's.
        app.drives.push(Drive::demo());
        app.drives[1].path = "/dev/sr-demo2".into();

        // Rip the short track 3 on drive 0.
        app.select_drive(0);
        app.track_sel = 2;
        app.rip_selected_track();
        assert!(app.drive_ripping(0));

        // Switch to drive 1 and rip the same track there, in parallel. Drive 0
        // keeps ripping in the background; its rip state is preserved.
        app.select_drive(1);
        app.track_sel = 2;
        app.rip_selected_track();
        assert!(app.drive_ripping(1));
        assert!(app.any_rip_active());

        drain_all_until_idle(&mut app, Duration::from_secs(15));

        assert!(!app.any_rip_active());
        // With two drives present, each wrote into its own per-drive subdir.
        assert!(dir.path().join("sr-demo/track03.flac").exists());
        assert!(dir.path().join("sr-demo2/track03.flac").exists());
    }

    #[test]
    fn skips_existing_output_without_force() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("track03.flac"), b"old").unwrap();
        let mut app = demo_app(dir.path());
        app.track_sel = 2;
        app.rip_selected_track();
        drain_until_finished(&mut app, Duration::from_secs(5));

        assert!(matches!(
            app.selected_rip_state().unwrap().states[2],
            TrackState::Skipped(_)
        ));
        assert_eq!(
            std::fs::read(dir.path().join("track03.flac")).unwrap(),
            b"old"
        );
    }

    #[test]
    fn force_toggles() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        assert!(!app.force);
        app.handle_key(key('f'));
        assert!(app.force);
        app.handle_key(key('f'));
        assert!(!app.force);
    }

    #[test]
    fn format_toggles() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        assert_eq!(app.format, Format::Flac);
        app.handle_key(key('o'));
        assert_eq!(app.format, Format::Wav);
        app.handle_key(key('o'));
        assert_eq!(app.format, Format::Flac);
    }

    #[test]
    fn rips_in_wav_format_when_selected() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        app.format = Format::Wav;
        app.track_sel = 2;
        app.rip_selected_track();
        drain_until_finished(&mut app, Duration::from_secs(10));

        let wav = dir.path().join("track03.wav");
        let bytes = std::fs::read(&wav).unwrap();
        // A RIFF container whose ID3v2 tag rides in a trailing `ID3 ` chunk.
        assert_eq!(&bytes[0..4], b"RIFF");
        assert!(bytes.windows(4).any(|w| w == b"ID3 "));

        let file = lofty::read_from_path(&wav).unwrap();
        let tag = file
            .tag(lofty::tag::TagType::Id3v2)
            .expect("an ID3v2 tag was written");
        assert_eq!(
            tag.title().map(std::borrow::Cow::into_owned).as_deref(),
            Some("Short One")
        );
    }

    #[test]
    fn quit_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        app.handle_key(key('q'));
        assert!(!app.running);
    }

    #[test]
    fn data_track_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        let Some(toc) = app.toc.as_mut() else {
            panic!()
        };
        toc.tracks[0].kind = rend_core::TrackType::Data;
        app.track_sel = 0;
        app.rip_selected_track();
        assert!(!app.drive_ripping(app.drive_sel));
        assert!(app.status.is_some());
    }

    /// A TOC panel rect whose inner rows start at y = 4.
    fn with_tracks_rect(app: &mut App) {
        app.regions.tracks = Rect {
            x: 0,
            y: 3,
            width: 100,
            height: 20,
        };
        app.tracks_scroll = 0;
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn mouse_click_selects_track() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        with_tracks_rect(&mut app);

        // Track index 2 is at row inner.y (4) + 2 = 6.
        app.handle_mouse(click(10, 6));

        assert_eq!(app.track_sel, 2);
        assert_eq!(app.focus, Focus::Tracks);
    }

    #[test]
    fn mouse_click_outside_tracks_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        with_tracks_rect(&mut app);

        // Row 30 is outside the panel (inner ends at y = 21).
        app.handle_mouse(click(10, 30));

        assert_eq!(app.track_sel, 0);
        assert_eq!(app.hover, Hover::None);
    }

    #[test]
    fn mouse_double_click_rips_track() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        with_tracks_rect(&mut app);

        app.handle_mouse(click(10, 6));
        app.handle_mouse(click(10, 6));

        assert!(app.drive_ripping(app.drive_sel));
        drain_until_finished(&mut app, Duration::from_secs(10));
        assert!(app.selected_rip_state().unwrap().summary.is_some());
        assert!(dir.path().join("track03.flac").exists());
    }

    #[test]
    fn mouse_move_sets_hover() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        with_tracks_rect(&mut app);

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 10,
            row: 6,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(app.hover, Hover::Track(2));
    }

    #[test]
    fn mouse_scroll_tracks_list() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        with_tracks_rect(&mut app);
        app.tracks_scroll = 2;

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 10,
            row: 6,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.tracks_scroll, 1);
        assert_eq!(app.focus, Focus::Tracks);
    }

    fn key(code: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(code), KeyModifiers::NONE)
    }
}
