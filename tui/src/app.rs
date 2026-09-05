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

use rend_core::{Device, DeviceInfo, DriveStatus, FRAME_SIZE, Toc};
use rend_encode::{Format, ffmpeg_available};
use rend_meta::{
    Candidate, DiscMeta, DiscToc, MetaCache, Template, TrackMeta, cover_art, lookup_candidates,
};

use crate::demo::{
    DEMO_DEVICE, DEMO_LABEL, DEMO_MCN, DemoDisc, DemoSource, demo_cover, demo_meta_all,
};
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
    EditTags,
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
    pub edit_tags: Rect,
    pub eject: Rect,
    pub stop: Rect,
}

impl Regions {
    /// The drive entry under the given position, if any. Each entry is two
    /// lines tall: the drive's name, then its album or status.
    pub fn drive_at(&self, col: u16, row: u16, scroll: usize, count: usize) -> Option<usize> {
        let inner = self.drives.inner(Margin::new(1, 1));
        if !inner.contains(Position::new(col, row)) {
            return None;
        }
        let entry = ((row - inner.y) as usize) / 2;
        let idx = entry + scroll;
        (idx < count).then_some(idx)
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

    /// Raw PCM bytes written and expected across this rip.
    ///
    /// The total is fixed for the life of the rip (every track in the job),
    /// and finished or skipped tracks count as fully done, so the progress
    /// bar grows steadily to 100% without resetting between tracks.
    pub fn totals(&self, toc: &Toc, tracks: &[u8]) -> (u64, u64) {
        let mut done = 0u64;
        let mut total = 0u64;
        for &number in tracks {
            let Some(i) = track_index(toc, number) else {
                continue;
            };
            let track = &toc.tracks[i];
            let end = toc.end_lba(number).unwrap_or(toc.leadout_lba);
            let pcm = track.frames(end) as u64 * FRAME_SIZE as u64;
            total += pcm;
            done += match self.states.get(i) {
                Some(TrackState::Ripping { bytes_done, .. }) => *bytes_done,
                Some(TrackState::Done { .. }) | Some(TrackState::Skipped(_)) => pcm,
                _ => 0,
            };
        }
        (done, total)
    }

    /// Recomputes the smoothed read speed from the newest progress sample.
    pub fn update_speed(&mut self, toc: &Toc, tracks: &[u8]) {
        let now = Instant::now();
        let (done, _) = self.totals(toc, tracks);
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
    /// The album matched to this drive's loaded disc, remembered so the
    /// drives panel can show each drive's album at a glance.
    pub album: Option<String>,
    device: Option<Device>,
    last_status: Option<DriveStatus>,
    #[cfg(test)]
    simulated_status: Option<DriveStatus>,
}

impl Drive {
    fn real(device: Device) -> Self {
        let mut drive = Self {
            path: device.path().to_string(),
            label: "unknown".into(),
            status: String::new(),
            disc: None,
            album: None,
            device: Some(device),
            last_status: None,
            #[cfg(test)]
            simulated_status: None,
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
            album: None,
            device: None,
            last_status: Some(DriveStatus::DiscOk),
            #[cfg(test)]
            simulated_status: None,
        }
    }

    /// `true` for the simulated drive, which has no real device behind it.
    pub fn is_demo(&self) -> bool {
        self.device.is_none()
    }

    /// Test seam: makes the next [`Drive::refresh`] report `status` instead
    /// of querying the device.
    #[cfg(test)]
    fn simulate(&mut self, status: DriveStatus) {
        self.simulated_status = Some(status);
    }

    fn refresh(&mut self) {
        #[cfg(test)]
        if let Some(status) = self.simulated_status.take() {
            self.status = status.to_string();
            self.last_status = Some(status);
            return;
        }
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

/// One editable line of the metadata editor: a label, its value, and the
/// cursor position as a byte offset into `value` (always a char boundary).
#[derive(Debug, Clone)]
pub struct EditField {
    /// What the line edits, e.g. `album` or `03 artist`.
    pub label: String,
    /// The line's current text.
    pub value: String,
    /// Cursor position as a byte offset into `value`.
    pub cursor: usize,
}

impl EditField {
    fn new(label: String, value: String) -> Self {
        Self {
            label,
            cursor: value.len(),
            value,
        }
    }

    /// Inserts a character at the cursor, moving the cursor past it.
    fn insert(&mut self, c: char) {
        self.value.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Deletes the character before the cursor, if any.
    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.char_start_before(self.cursor);
        self.value.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Deletes the character at the cursor, if any.
    fn delete(&mut self) {
        if self.cursor >= self.value.len() {
            return;
        }
        let end = self.char_end_at(self.cursor);
        self.value.replace_range(self.cursor..end, "");
    }

    /// Moves the cursor left by one character.
    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.char_start_before(self.cursor);
        }
    }

    /// Moves the cursor right by one character.
    fn move_right(&mut self) {
        if self.cursor < self.value.len() {
            self.cursor = self.char_end_at(self.cursor);
        }
    }

    /// The start of the character that ends at `pos`. `pos` must be a
    /// nonzero char boundary.
    fn char_start_before(&self, pos: usize) -> usize {
        let mut i = pos - 1;
        while i > 0 && !self.value.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    /// The end of the character that starts at `pos`. `pos` must be a char
    /// boundary before the end of the value.
    fn char_end_at(&self, pos: usize) -> usize {
        let mut i = pos + 1;
        while i < self.value.len() && !self.value.is_char_boundary(i) {
            i += 1;
        }
        i
    }
}

/// The open metadata editor: the fields being edited and which one has
/// keyboard focus.
#[derive(Debug, Clone)]
pub struct MetaEdit {
    /// The fields, in display order: album, artist, album artist, year,
    /// then title and artist per track.
    pub fields: Vec<EditField>,
    /// The focused field.
    pub sel: usize,
    /// The first displayed field.
    pub scroll: usize,
}

/// The outcome of a background metadata lookup. `id` records which lookup
/// produced it, so results from a previous disc can be discarded.
enum MetaEvent {
    /// The candidate list arrived, best first. `toc` keys the answer in
    /// the cache when it lands.
    Candidates {
        id: u64,
        toc: DiscToc,
        candidates: Vec<Candidate>,
    },
    /// Cover art for the given release arrived.
    Cover {
        id: u64,
        release_id: String,
        cover: Result<Option<Vec<u8>>, String>,
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
    /// The track numbers this rip covers, for overall progress.
    tracks: Vec<u8>,
    rx: Option<mpsc::Receiver<RipEvent>>,
    thread: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

/// The TUI application state.
pub struct App {
    pub running: bool,
    /// The keybinds help window, opened with `?`.
    pub help: bool,
    pub focus: Focus,
    pub drives: Vec<Drive>,
    pub drive_sel: usize,
    pub drives_scroll: usize,
    pub toc: Option<Toc>,
    pub disc_id: Option<String>,
    /// The looked-up candidate metadata for the loaded disc, best first.
    pub meta: Option<Vec<DiscMeta>>,
    /// The selected candidate within `meta`.
    meta_sel: usize,
    /// The selected candidate's cover art, once fetched.
    cover: Option<Vec<u8>>,
    /// The open metadata editor, if one is being edited.
    pub editing: Option<MetaEdit>,
    /// Pending results from the metadata lookup workers.
    meta_rx: Option<mpsc::Receiver<MetaEvent>>,
    /// The sender of the metadata lookup channel, for the cover worker.
    meta_tx: Option<mpsc::Sender<MetaEvent>>,
    /// Generation counter invalidating in-flight lookups on disc changes.
    meta_gen: u64,
    /// Lookups and covers this session has already fetched, so re-reading
    /// a known disc skips the network.
    meta_cache: MetaCache,
    pub track_sel: usize,
    pub tracks_scroll: usize,
    /// Rips keyed by drive index; several drives can rip at once.
    rips: HashMap<usize, DriveRip>,
    /// The format new rips write in.
    pub format: Format,
    /// The naming template new rips lay their track files out with.
    pub template: Template,
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
        template: Template,
        demo: bool,
    ) -> Self {
        let mut app = Self {
            running: true,
            help: false,
            focus: Focus::Drives,
            drives: Vec::new(),
            drive_sel: 0,
            drives_scroll: 0,
            toc: None,
            disc_id: None,
            meta: None,
            meta_sel: 0,
            cover: None,
            editing: None,
            meta_rx: None,
            meta_tx: None,
            meta_gen: 0,
            meta_cache: MetaCache::new(),
            track_sel: 0,
            tracks_scroll: 0,
            rips: HashMap::new(),
            format,
            template,
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
        self.load_disc();
    }

    /// Reads the selected drive's table of contents and starts its metadata
    /// lookup, replacing whatever the drive showed before.
    fn load_disc(&mut self) {
        self.track_sel = 0;
        self.tracks_scroll = 0;
        self.toc = None;
        self.disc_id = None;
        self.meta = None;
        self.meta_sel = 0;
        self.cover = None;
        self.editing = None;
        self.remember_album();
        let idx = self.drive_sel;
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

    /// Forgets the loaded disc after the selected drive's state changed, so
    /// the drive is read again on the next enter (or on disc insertion).
    fn invalidate_disc(&mut self) {
        let selected = self.drive_sel;
        self.toc = None;
        self.disc_id = None;
        self.meta = None;
        self.meta_sel = 0;
        self.cover = None;
        self.editing = None;
        self.meta_rx = None;
        self.meta_tx = None;
        self.meta_gen += 1;
        self.remember_album();
        if let Some(rip) = self.rips.get_mut(&selected) {
            rip.state.states.clear();
        }
        let path = self.drives[selected].path.clone();
        self.status = Some(format!(
            "disc state changed on {path} — press enter to reload"
        ));
    }

    /// Reacts to the selected drive's status having changed.
    fn selected_status_changed(&mut self, now: Option<DriveStatus>) {
        match now {
            // A disc appeared in the selected, empty drive: read it.
            Some(DriveStatus::DiscOk) if self.toc.is_none() => self.load_disc(),
            // A loaded TOC is stale: forget it.
            _ if self.toc.is_some() => self.invalidate_disc(),
            _ => {}
        }
    }

    /// Starts looking up the disc's candidate metadata: built in for the
    /// simulated disc, on a background thread for a real one.
    fn lookup_metadata(&mut self, toc: &Toc) {
        let lbas: Vec<u32> = toc.audio_tracks().map(|t| t.start_lba).collect();
        if lbas.is_empty() {
            self.status = Some("no audio tracks to look up".into());
            return;
        }
        self.meta = None;
        self.meta_sel = 0;
        self.cover = None;
        self.meta_rx = None;
        self.meta_tx = None;
        if self.drives[self.drive_sel].is_demo() {
            self.meta = Some(demo_meta_all());
            self.status = Some(self.match_status().unwrap_or_default());
            self.cover = Some(demo_cover());
            self.remember_album();
            return;
        }
        let disc_toc = DiscToc {
            offsets: lbas,
            leadout: toc.leadout_lba,
        };
        // A disc this session has already looked up is answered from
        // memory, cover included.
        if let Some(candidates) = self.meta_cache.candidates(&disc_toc).map(|c| c.to_vec()) {
            self.apply_cached_meta(&candidates);
            return;
        }
        let lookup_id = self.meta_gen + 1;
        self.meta_gen = lookup_id;
        self.status = Some("looking up metadata…".into());
        let (tx, rx) = mpsc::channel();
        self.meta_rx = Some(rx);
        self.meta_tx = Some(tx.clone());
        std::thread::Builder::new()
            .name("rend-meta-lookup".into())
            .spawn(move || {
                let candidates = match lookup_candidates(&disc_toc) {
                    Ok(candidates) => candidates,
                    Err(e) => {
                        let _ = tx.send(MetaEvent::Failed {
                            id: lookup_id,
                            reason: e.to_string(),
                        });
                        return;
                    }
                };
                let _ = tx.send(MetaEvent::Candidates {
                    id: lookup_id,
                    toc: disc_toc,
                    candidates,
                });
            })
            .ok();
    }

    /// Applies a candidate list that is already in the cache: the
    /// candidates and their status immediately, the selected candidate's
    /// cover through [`Self::fetch_cover`], which hits the cover cache.
    fn apply_cached_meta(&mut self, candidates: &[Candidate]) {
        if candidates.is_empty() {
            self.status = Some("no release matched the disc".into());
        } else {
            self.meta = Some(candidates.iter().map(|c| c.meta.clone()).collect());
            self.status = Some(self.match_status().unwrap_or_default());
            self.fetch_cover();
        }
        self.remember_album();
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
                MetaEvent::Candidates {
                    id,
                    toc,
                    candidates,
                } => {
                    // The answer is definitive for this session even when
                    // stale (a newer lookup supersedes it) or empty (no
                    // match is not worth retrying).
                    self.meta_cache.insert(toc, candidates.clone());
                    if id != self.meta_gen {
                        continue;
                    }
                    if candidates.is_empty() {
                        self.status = Some("no release matched the disc".into());
                        continue;
                    }
                    self.meta = Some(candidates.into_iter().map(|c| c.meta).collect());
                    self.status = Some(self.match_status().unwrap_or_default());
                    self.fetch_cover();
                    self.remember_album();
                }
                MetaEvent::Cover {
                    id,
                    release_id,
                    cover,
                } => {
                    if id != self.meta_gen {
                        continue;
                    }
                    // Ignore a cover whose candidate is no longer selected.
                    let current = self
                        .selected_meta()
                        .is_some_and(|d| d.release_id == release_id);
                    match cover {
                        Ok(cover) => {
                            // Remember the cover, or its absence, for this
                            // release.
                            self.meta_cache
                                .cover_insert(release_id.clone(), cover.clone());
                            if current {
                                self.cover = cover;
                            }
                        }
                        Err(reason) => {
                            if current {
                                self.status = Some(format!("cover art unavailable: {reason}"));
                            }
                        }
                    }
                    self.meta_rx = None;
                    self.meta_tx = None;
                }
                MetaEvent::Failed { id, reason } => {
                    if id != self.meta_gen {
                        continue;
                    }
                    self.status = Some(format!("metadata lookup failed: {reason}"));
                    self.meta_rx = None;
                    self.meta_tx = None;
                }
            }
        }
    }

    /// The currently selected candidate match, if any.
    pub fn selected_meta(&self) -> Option<&DiscMeta> {
        self.meta.as_ref().and_then(|m| m.get(self.meta_sel))
    }

    /// The selected candidate as `artist — album (year)`, for the drives
    /// panel. `None` while the lookup is pending or when nothing is filled
    /// in yet.
    fn album_summary(&self) -> Option<String> {
        let disc = self.selected_meta()?;
        if disc.album.is_empty() && disc.artist.is_empty() {
            return None;
        }
        let year = disc
            .year
            .as_deref()
            .map(|y| format!(" ({y})"))
            .unwrap_or_default();
        let album = if disc.album.is_empty() {
            "untitled"
        } else {
            disc.album.as_str()
        };
        let artist = if disc.artist.is_empty() {
            "unknown artist"
        } else {
            disc.artist.as_str()
        };
        Some(format!("{artist} — {album}{year}"))
    }

    /// Remembers the loaded disc's album on the selected drive, so its panel
    /// line keeps showing it after the selection moves on.
    fn remember_album(&mut self) {
        let album = self.album_summary();
        if let Some(d) = self.drives.get_mut(self.drive_sel) {
            d.album = album;
        }
    }

    /// The status line describing the selected candidate, if any.
    pub fn match_status(&self) -> Option<String> {
        let candidates = self.meta.as_ref()?;
        let disc = candidates.get(self.meta_sel)?;
        let year = disc
            .year
            .as_deref()
            .map(|y| format!(" ({y})"))
            .unwrap_or_default();
        let mut prefix = if candidates.len() > 1 {
            format!("match {} of {} · ", self.meta_sel + 1, candidates.len())
        } else {
            String::new()
        };
        if disc.release_id.is_empty() {
            prefix.push_str("manual · ");
        }
        let album = if disc.album.is_empty() {
            "untitled"
        } else {
            disc.album.as_str()
        };
        let artist = if disc.artist.is_empty() {
            "unknown artist"
        } else {
            disc.artist.as_str()
        };
        Some(format!("{prefix}{artist} — {album}{year}"))
    }

    /// Switches to the next candidate match, wrapping around.
    pub fn next_match(&mut self) {
        let Some(count) = self.meta.as_ref().map(|m| m.len()) else {
            self.status = Some("no matches to switch to — the lookup is pending or failed".into());
            return;
        };
        if count == 1 {
            self.status = Some("only one candidate match".into());
            return;
        }
        self.meta_sel = (self.meta_sel + 1) % count;
        self.cover = None;
        self.status = Some(self.match_status().unwrap_or_default());
        self.fetch_cover();
        self.remember_album();
    }

    /// Starts fetching the selected candidate's cover art: built in for the
    /// simulated disc, on a background thread for a real one. Hand-entered
    /// metadata has no release to fetch a cover for.
    fn fetch_cover(&mut self) {
        let Some(disc) = self.selected_meta().cloned() else {
            return;
        };
        if disc.release_id.is_empty() {
            self.cover = None;
            return;
        }
        if self.drives.get(self.drive_sel).is_some_and(Drive::is_demo) {
            self.cover = Some(demo_cover());
            return;
        }
        // A cover this session has already fetched (or found absent) for
        // the release is answered from memory.
        if let Some(cover) = self.meta_cache.cover(&disc.release_id) {
            self.cover = cover.clone();
            return;
        }
        if self.meta_tx.is_none() {
            let (tx, rx) = mpsc::channel();
            self.meta_tx = Some(tx.clone());
            self.meta_rx = Some(rx);
        }
        let tx = self.meta_tx.clone().unwrap();
        let id = self.meta_gen;
        let release_id = disc.release_id;
        std::thread::Builder::new()
            .name("rend-cover".into())
            .spawn(move || {
                let cover = cover_art(&release_id).map_err(|e| e.to_string());
                let _ = tx.send(MetaEvent::Cover {
                    id,
                    release_id,
                    cover,
                });
            })
            .ok();
    }

    /// Handles a key press. While the metadata editor is open, every key
    /// goes to it instead.
    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.editing.is_some() {
            self.edit_key(key);
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if key.code == KeyCode::Char('c') {
                self.running = false;
            }
            return;
        }
        if self.help {
            match key.code {
                // `?` and esc close the help; `q` still quits.
                KeyCode::Char('?') | KeyCode::Esc => self.help = false,
                KeyCode::Char('q') => self.running = false,
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.running = false,
            KeyCode::Char('?') => self.help = true,
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
            KeyCode::Char('m') => self.next_match(),
            KeyCode::Char('t') => self.start_editing(),
            KeyCode::Char('e') => self.eject(),
            KeyCode::Char('s') => self.stop_rip(),
            _ => {}
        }
    }

    /// Opens the metadata editor on the selected candidate. When the lookup
    /// found no candidates, a blank entry is created so the disc can still
    /// be tagged by hand.
    pub fn start_editing(&mut self) {
        if self.toc.is_none() {
            self.status = Some("no disc loaded".into());
            return;
        }
        if self.drive_ripping(self.drive_sel) {
            self.status = Some("stop the rip before editing tags".into());
            return;
        }
        if self.selected_meta().is_none() {
            self.create_manual_meta();
        }
        let Some(disc) = self.selected_meta().cloned() else {
            return;
        };
        let mut fields = vec![
            EditField::new("album".into(), disc.album.clone()),
            EditField::new("artist".into(), disc.artist.clone()),
            EditField::new(
                "album artist".into(),
                disc.album_artist.clone().unwrap_or_default(),
            ),
            EditField::new("year".into(), disc.year.clone().unwrap_or_default()),
        ];
        for (i, track) in disc.tracks.iter().enumerate() {
            fields.push(EditField::new(
                format!("{:02} title", i + 1),
                track.title.clone(),
            ));
            fields.push(EditField::new(
                format!("{:02} artist", i + 1),
                track.artist.clone().unwrap_or_default(),
            ));
        }
        self.editing = Some(MetaEdit {
            fields,
            sel: 0,
            scroll: 0,
        });
        self.status = None;
    }

    /// Appends a blank, hand-entered entry for a disc the lookup could not
    /// match, and selects it. An in-flight lookup is cancelled: the manual
    /// entry takes precedence.
    fn create_manual_meta(&mut self) {
        let Some(toc) = self.toc.as_ref() else {
            self.status = Some("no disc loaded".into());
            return;
        };
        let tracks = toc
            .audio_tracks()
            .map(|t| TrackMeta {
                title: format!("Track {}", t.number),
                artist: None,
            })
            .collect();
        self.meta_gen += 1;
        self.meta_rx = None;
        self.meta_tx = None;
        self.cover = None;
        self.meta = Some(vec![DiscMeta {
            album: String::new(),
            artist: String::new(),
            album_artist: None,
            year: None,
            release_id: String::new(),
            tracks,
        }]);
        self.meta_sel = 0;
        self.status = Some("no match found — enter the tags by hand".into());
        self.remember_album();
    }

    /// Applies the editor's values to the selected candidate and closes it.
    pub fn save_editing(&mut self) {
        let Some(edit) = self.editing.take() else {
            return;
        };
        let Some(disc) = self.meta.as_mut().and_then(|m| m.get_mut(self.meta_sel)) else {
            return;
        };
        let field = |i: usize| edit.fields.get(i).map(|f| f.value.trim().to_string());
        if let Some(album) = field(0) {
            disc.album = album;
        }
        if let Some(artist) = field(1) {
            disc.artist = artist;
        }
        if let Some(album_artist) = field(2) {
            disc.album_artist = (!album_artist.is_empty()).then_some(album_artist);
        }
        if let Some(year) = field(3) {
            disc.year = (!year.is_empty()).then_some(year);
        }
        for (i, track) in disc.tracks.iter_mut().enumerate() {
            if let Some(title) = field(4 + 2 * i) {
                track.title = title;
            }
            if let Some(artist) = field(5 + 2 * i) {
                track.artist = (!artist.is_empty()).then_some(artist);
            }
        }
        self.status = Some(self.match_status().unwrap_or_default());
        self.remember_album();
    }

    /// Closes the editor without saving.
    pub fn cancel_editing(&mut self) {
        if self.editing.take().is_some() {
            self.status = Some("tag editing cancelled".into());
        }
    }

    /// Routes a key press to the open metadata editor.
    fn edit_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if key.code == KeyCode::Char('c') {
                self.cancel_editing();
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            return;
        }
        match key.code {
            KeyCode::Esc => self.cancel_editing(),
            KeyCode::Enter => self.save_editing(),
            _ => self.edit_field_key(key),
        }
    }

    /// Edits the focused field, or moves the focus between fields.
    fn edit_field_key(&mut self, key: KeyEvent) {
        let Some(edit) = self.editing.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Up | KeyCode::Tab => edit.sel = edit.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::BackTab => {
                edit.sel = (edit.sel + 1).min(edit.fields.len().saturating_sub(1))
            }
            _ => {
                let Some(field) = edit.fields.get_mut(edit.sel) else {
                    return;
                };
                match key.code {
                    KeyCode::Backspace => field.backspace(),
                    KeyCode::Delete => field.delete(),
                    KeyCode::Left => field.move_left(),
                    KeyCode::Right => field.move_right(),
                    KeyCode::Home => field.cursor = 0,
                    KeyCode::End => field.cursor = field.value.len(),
                    KeyCode::Char(c) => field.insert(c),
                    _ => {}
                }
            }
        }
    }

    /// Handles a mouse event (click, double-click, move, scroll). While the
    /// metadata editor or the help window is open, mouse input is ignored.
    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        if self.editing.is_some() || self.help {
            return;
        }
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
                    Hover::EditTags => self.start_editing(),
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
                    // The drives panel shows two lines per entry.
                    self.drives_scroll = scroll(
                        self.drives_scroll,
                        down,
                        self.drives.len(),
                        self.visible(self.regions.drives) / 2,
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
                        rip.state.update_speed(&rip.toc, &rip.tracks);
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
                        rip.state.update_speed(&rip.toc, &rip.tracks);
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

    /// Re-queries drive statuses, and reacts when the selected drive's disc
    /// state changed: a newly inserted disc is loaded automatically, a
    /// removed (or changed) one invalidates the loaded TOC.
    pub fn refresh_drives(&mut self) {
        if self.drives.iter().all(Drive::is_demo) || self.any_rip_active() {
            return;
        }
        let selected = self.drive_sel;
        let mut changed_to: Option<Option<DriveStatus>> = None;
        for (i, drive) in self.drives.iter_mut().enumerate() {
            let prev = drive.last_status;
            drive.refresh();
            if i == selected && prev != drive.last_status {
                changed_to = Some(drive.last_status);
            }
        }
        let Some(now) = changed_to else {
            return;
        };
        self.selected_status_changed(now);
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
        self.rip_progress(self.drive_sel)
    }

    /// Total bytes written and expected across the given drive's rip.
    pub fn rip_progress(&self, idx: usize) -> (u64, u64) {
        self.rips
            .get(&idx)
            .map(|r| r.state.totals(&r.toc, &r.tracks))
            .unwrap_or((0, 0))
    }

    /// The number of tracks in the selected drive's rip job, if any.
    pub fn rip_track_count(&self) -> Option<usize> {
        self.rips.get(&self.drive_sel).map(|r| r.tracks.len())
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
                self.meta_sel = 0;
                self.cover = None;
                self.editing = None;
                self.meta_rx = None;
                self.meta_tx = None;
                self.meta_gen += 1;
                self.remember_album();
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
            tracks: tracks.clone(),
            out_dir: out_dir.clone(),
            format: self.format,
            template: self.template.clone(),
            force: self.force,
            stop: stop.clone(),
            meta: self.selected_meta().cloned(),
            cover: self.cover.clone(),
        };
        let thread = rip::spawn(job, tx);
        let mut state = RipState::fresh(self.track_count());
        state.active = true;
        let rip = DriveRip {
            state,
            out_dir: out_dir.clone(),
            toc,
            tracks,
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
        if self.regions.edit_tags.contains(pos) {
            return Hover::EditTags;
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
        App::new(
            None,
            dir.to_path_buf(),
            false,
            Format::default(),
            Template::default(),
            true,
        )
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
        let meta = app.selected_meta().unwrap();
        assert_eq!(meta.album, "Demo Album");
        assert_eq!(meta.tracks.len(), 5);
        // The simulated disc carries a second candidate for the switch flow.
        assert_eq!(app.meta.as_ref().unwrap().len(), 2);
        assert!(app.cover.is_some());
    }

    #[test]
    fn switches_candidate_matches_with_m() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        assert_eq!(app.selected_meta().unwrap().album, "Demo Album");

        app.handle_key(key('m'));
        assert_eq!(app.selected_meta().unwrap().album, "Demo Album (Reissue)");
        // The reissue's cover replaces the first candidate's.
        assert_eq!(app.cover.as_deref(), Some(demo_cover().as_slice()));

        // The key wraps back to the first candidate.
        app.handle_key(key('m'));
        assert_eq!(app.selected_meta().unwrap().album, "Demo Album");
    }

    #[test]
    fn rip_uses_the_switched_match() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        app.handle_key(key('m'));
        assert_eq!(app.selected_meta().unwrap().album, "Demo Album (Reissue)");
        app.track_sel = 2;
        app.rip_selected_track();
        drain_until_finished(&mut app, Duration::from_secs(10));

        // The file is named after the reissue's track title, not the
        // original candidate's.
        let flac = dir
            .path()
            .join("The_Demo_Band/Demo_Album__Reissue_/03_Short_One__Reprise_.flac");
        assert!(flac.exists());
        assert!(
            !dir.path()
                .join("The_Demo_Band/Demo_Album/03_Short_One.flac")
                .exists()
        );
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
        let flac = dir
            .path()
            .join("The_Demo_Band/Demo_Album/03_Short_One.flac");
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
        // With two drives present, each wrote into its own per-drive subdir,
        // and the tracks live under the looked-up artist and album
        // directories.
        assert!(
            dir.path()
                .join("sr-demo/The_Demo_Band/Demo_Album/03_Short_One.flac")
                .exists()
        );
        assert!(
            dir.path()
                .join("sr-demo2/The_Demo_Band/Demo_Album/03_Short_One.flac")
                .exists()
        );
    }

    #[test]
    fn skips_existing_output_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir
            .path()
            .join("The_Demo_Band/Demo_Album/03_Short_One.flac");
        std::fs::create_dir_all(existing.parent().unwrap()).unwrap();
        std::fs::write(&existing, b"old").unwrap();
        let mut app = demo_app(dir.path());
        app.track_sel = 2;
        app.rip_selected_track();
        drain_until_finished(&mut app, Duration::from_secs(5));

        assert!(matches!(
            app.selected_rip_state().unwrap().states[2],
            TrackState::Skipped(_)
        ));
        assert_eq!(std::fs::read(&existing).unwrap(), b"old");
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

        let wav = dir.path().join("The_Demo_Band/Demo_Album/03_Short_One.wav");
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

    fn key_event(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_c() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_key(key(c));
        }
    }

    /// Moves the editor focus down by one field.
    fn field_down(app: &mut App) {
        app.handle_key(key_event(KeyCode::Down));
    }

    /// Clears the focused field by backspacing out its current contents.
    fn clear_field(app: &mut App) {
        let len = app
            .editing
            .as_ref()
            .and_then(|e| e.fields.get(e.sel))
            .map_or(0, |f| f.value.chars().count());
        for _ in 0..len {
            app.handle_key(key_event(KeyCode::Backspace));
        }
    }

    #[test]
    fn edits_metadata_with_t() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());

        app.handle_key(key('t'));
        assert!(app.editing.is_some());
        assert_eq!(app.editing.as_ref().unwrap().fields[0].label, "album");

        // The album field is focused; append to it, then move to the track 3
        // title (fields: album, artist, album artist, year, then two per track)
        // and append there too.
        type_text(&mut app, " (Deluxe)");
        for _ in 0..8 {
            field_down(&mut app);
        }
        type_text(&mut app, " (Edit)");
        app.handle_key(key_event(KeyCode::Enter));

        assert!(app.editing.is_none());
        let disc = app.selected_meta().unwrap();
        assert_eq!(disc.album, "Demo Album (Deluxe)");
        assert_eq!(disc.tracks[2].title, "Short One (Edit)");
        // The renamed album shows up in the candidate summary.
        assert!(app.match_status().unwrap().contains("Demo Album (Deluxe)"));
    }

    #[test]
    fn esc_cancels_the_editor() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        let album_before = app.selected_meta().unwrap().album.clone();

        app.handle_key(key('t'));
        type_text(&mut app, "junk");
        app.handle_key(key_event(KeyCode::Esc));

        assert!(app.editing.is_none());
        assert!(app.running, "esc inside the editor must not quit");
        assert_eq!(app.selected_meta().unwrap().album, album_before);
    }

    #[test]
    fn ctrl_c_cancels_the_editor() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());

        app.handle_key(key('t'));
        type_text(&mut app, "junk");
        app.handle_key(ctrl_c());

        assert!(app.editing.is_none());
        assert!(app.running, "ctrl-c inside the editor must not quit");
        assert_eq!(app.selected_meta().unwrap().album, "Demo Album");
    }

    #[test]
    fn editor_keys_are_text_not_commands() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());

        app.handle_key(key('t'));
        // q would quit, m would switch matches; inside the editor they type.
        app.handle_key(key('q'));
        app.handle_key(key('m'));
        assert!(app.running);
        assert!(app.editing.is_some());
        assert_eq!(
            app.editing.as_ref().unwrap().fields[0].value,
            "Demo Albumqm"
        );
        app.handle_key(key_event(KeyCode::Esc));
        // The typed text was not applied.
        assert_eq!(app.selected_meta().unwrap().album, "Demo Album");
    }

    #[test]
    fn unmatched_disc_gets_manual_meta() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        app.meta = None; // Simulate a lookup that found nothing.

        app.handle_key(key('t'));
        assert!(app.editing.is_some());
        let disc = app.selected_meta().unwrap();
        assert_eq!(disc.release_id, "");
        assert_eq!(disc.tracks.len(), 5);
        assert_eq!(disc.tracks[0].title, "Track 1");
        assert!(app.cover.is_none());

        // Fill in album and artist, then save.
        type_text(&mut app, "My Album");
        field_down(&mut app);
        type_text(&mut app, "My Artist");
        app.handle_key(key_event(KeyCode::Enter));

        let disc = app.selected_meta().unwrap();
        assert_eq!(disc.album, "My Album");
        assert_eq!(disc.artist, "My Artist");
        assert!(app.match_status().unwrap().starts_with("manual · "));
    }

    #[test]
    fn manual_meta_is_saved_and_rippable() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        app.meta = None;
        app.handle_key(key('t'));
        type_text(&mut app, "My Album");
        field_down(&mut app);
        type_text(&mut app, "My Artist");
        app.handle_key(key_event(KeyCode::Enter));

        app.track_sel = 2;
        app.rip_selected_track();
        drain_until_finished(&mut app, Duration::from_secs(10));

        // The rip is named after the hand-entered artist, album, and the
        // default track title: <out>/My_Artist/My_Album/03_Track_3.flac.
        let flac = dir.path().join("My_Artist/My_Album/03_Track_3.flac");
        assert!(flac.exists());
        let file = lofty::read_from_path(&flac).unwrap();
        let tag = file.tag(lofty::tag::TagType::VorbisComments).unwrap();
        assert_eq!(
            tag.album().map(std::borrow::Cow::into_owned).as_deref(),
            Some("My Album")
        );
        assert_eq!(
            tag.artist().map(std::borrow::Cow::into_owned).as_deref(),
            Some("My Artist")
        );
    }

    #[test]
    fn rip_uses_edited_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());

        app.handle_key(key('t'));
        clear_field(&mut app);
        type_text(&mut app, "Renamed Album");
        for _ in 0..8 {
            field_down(&mut app);
        }
        type_text(&mut app, " (Edit)");
        app.handle_key(key_event(KeyCode::Enter));

        app.track_sel = 2;
        app.rip_selected_track();
        drain_until_finished(&mut app, Duration::from_secs(10));

        let flac = dir
            .path()
            .join("The_Demo_Band/Renamed_Album/03_Short_One__Edit_.flac");
        assert!(flac.exists());
        let file = lofty::read_from_path(&flac).unwrap();
        let tag = file.tag(lofty::tag::TagType::VorbisComments).unwrap();
        assert_eq!(
            tag.album().map(std::borrow::Cow::into_owned).as_deref(),
            Some("Renamed Album")
        );
        assert_eq!(
            tag.title().map(std::borrow::Cow::into_owned).as_deref(),
            Some("Short One (Edit)")
        );
    }

    #[test]
    fn editor_backspace_removes_whole_characters() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());

        app.handle_key(key('t'));
        clear_field(&mut app);
        type_text(&mut app, "café");
        // The cursor sits after the two-byte 'é'; one backspace removes it
        // whole, not half of it.
        app.handle_key(key_event(KeyCode::Backspace));

        assert_eq!(app.editing.as_ref().unwrap().fields[0].value, "caf");
        app.handle_key(key_event(KeyCode::Esc));
    }

    #[test]
    fn editing_is_refused_while_ripping() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());

        app.track_sel = 2;
        app.rip_selected_track();
        app.handle_key(key('t'));
        assert!(app.editing.is_none());
        assert!(
            app.status
                .as_deref()
                .is_some_and(|s| s.contains("stop the rip"))
        );

        app.stop_rip();
        drain_until_finished(&mut app, Duration::from_secs(10));
    }

    #[test]
    fn mouse_is_ignored_while_editing() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        with_tracks_rect(&mut app);

        app.handle_key(key('t'));
        // Track 2's row; a click would normally select it.
        app.handle_mouse(click(10, 6));

        assert_eq!(app.track_sel, 0);
        assert!(app.editing.is_some());
        app.handle_key(key_event(KeyCode::Esc));
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

    #[test]
    fn inserted_disc_on_the_selected_drive_loads_automatically() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        // The drive was selected while empty: no TOC was loaded.
        app.toc = None;
        app.disc_id = None;
        app.meta = None;
        app.cover = None;
        app.focus = Focus::Drives;

        app.selected_status_changed(Some(DriveStatus::DiscOk));

        // The disc was read and its metadata applied, as if enter had been
        // pressed.
        assert!(app.toc.is_some());
        assert_eq!(app.disc_id.as_deref(), Some(DEMO_MCN));
        assert_eq!(app.selected_meta().unwrap().album, "Demo Album");
        assert!(app.cover.is_some());
        assert_eq!(app.focus, Focus::Tracks);
    }

    #[test]
    fn removed_disc_invalidates_the_loaded_toc() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        assert!(app.toc.is_some());

        app.selected_status_changed(Some(DriveStatus::NoDisc));

        assert!(app.toc.is_none());
        assert!(app.meta.is_none());
        assert!(app.cover.is_none());
        assert!(app.disc_id.is_none());
        assert!(app.drives[0].album.is_none());
        assert!(
            app.status
                .as_deref()
                .is_some_and(|s| s.contains("press enter to reload"))
        );
    }

    #[test]
    fn question_mark_toggles_the_help_window() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        assert!(!app.help);

        app.handle_key(key('?'));
        assert!(app.help);

        // While the help is open, normal keys do nothing.
        app.handle_key(key('m'));
        assert_eq!(app.selected_meta().unwrap().album, "Demo Album");
        app.handle_key(key('r'));
        assert!(!app.drive_ripping(app.drive_sel));

        // Esc closes the help without quitting.
        app.handle_key(key_event(KeyCode::Esc));
        assert!(!app.help);
        assert!(app.running);

        // ? re-opens it, and ? closes it again.
        app.handle_key(key('?'));
        assert!(app.help);
        app.handle_key(key('?'));
        assert!(!app.help);
    }

    #[test]
    fn each_drive_remembers_its_loaded_album() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        app.drives.push(Drive::demo());
        app.drives[1].path = "/dev/sr1".into();

        app.select_drive(1);
        assert!(app.drives[0].album.is_some());
        assert!(app.drives[1].album.is_some());

        // Switching back keeps the other drive's album in place.
        app.select_drive(0);
        assert!(app.drives[0].album.is_some());
        assert!(app.drives[1].album.is_some());
        assert!(app.toc.is_some());
    }

    #[test]
    fn refresh_drives_loads_a_freshly_inserted_disc() {
        let dir = tempfile::tempdir().unwrap();
        // A "drive" that is really a plain file: its CD-ROM ioctls fail, but
        // it stands in for a real device whose state changes between polls.
        let media = dir.path().join("media");
        std::fs::write(&media, []).unwrap();
        let mut app = App::new(
            Some(media.to_str().unwrap()),
            dir.path().to_path_buf(),
            false,
            Format::default(),
            Template::default(),
            false,
        );
        // Selected while empty: the TOC read failed.
        assert!(app.toc.is_none());
        assert!(
            app.status
                .as_deref()
                .is_some_and(|s| s.starts_with("no TOC"))
        );

        // The drive reports a disc on the next poll.
        app.status = Some("sentinel".into());
        app.drives[0].simulate(DriveStatus::DiscOk);
        app.refresh_drives();

        // The insertion was detected and the drive was re-read: the sentinel
        // was replaced by the (failed, hardware-less) TOC attempt, and the
        // drive row shows the disc.
        assert_ne!(app.status.as_deref(), Some("sentinel"));
        assert!(
            app.status
                .as_deref()
                .is_some_and(|s| s.starts_with("no TOC"))
        );
        assert_eq!(app.drives[0].status, "disc present");
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
        assert!(
            dir.path()
                .join("The_Demo_Band/Demo_Album/03_Short_One.flac")
                .exists()
        );
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

    /// The raw PCM size of a track, given the TOC.
    fn pcm_of(toc: &Toc, number: u8) -> u64 {
        let track = toc.track(number).unwrap();
        let end = toc.end_lba(number).unwrap_or(toc.leadout_lba);
        track.frames(end) as u64 * FRAME_SIZE as u64
    }

    #[test]
    fn overall_progress_does_not_reset_between_tracks() {
        let toc = DemoDisc::new().toc;
        let tracks: Vec<u8> = toc.audio_tracks().map(|t| t.number).collect();
        let job_total: u64 = tracks.iter().map(|&n| pcm_of(&toc, n)).sum();
        let mut state = RipState::fresh(toc.tracks.len());

        // Before anything is read, the whole job is already the denominator.
        let (done, total) = state.totals(&toc, &tracks);
        assert_eq!(done, 0);
        assert_eq!(total, job_total);

        // Track 1 finished, track 2 halfway: the bar sits past track 1's
        // share of the whole job, not back near zero.
        let (n1, n2) = (tracks[0], tracks[1]);
        state.states[0] = TrackState::Done { bytes: 1234 };
        state.states[1] = TrackState::Ripping {
            bytes_done: pcm_of(&toc, n2) / 2,
            bytes_total: pcm_of(&toc, n2),
        };
        let (done, total) = state.totals(&toc, &tracks);
        // The total never changes as tracks start; done counts track 1 by
        // its raw PCM length, not its (much smaller) output file size.
        assert_eq!(total, job_total);
        assert_eq!(done, pcm_of(&toc, n1) + pcm_of(&toc, n2) / 2);
        assert!(done as f64 / total as f64 > 0.1);

        // A skipped track counts as fully done, so it does not drag the bar.
        state.states[0] = TrackState::Skipped("output exists".into());
        let (done, _) = state.totals(&toc, &tracks);
        assert_eq!(done, pcm_of(&toc, n1) + pcm_of(&toc, n2) / 2);
    }

    fn meta_with(release_id: &str, album: &str) -> DiscMeta {
        DiscMeta {
            album: album.into(),
            artist: "The Band".into(),
            album_artist: None,
            year: None,
            release_id: release_id.into(),
            tracks: vec![],
        }
    }

    fn candidate(release_id: &str, album: &str) -> Candidate {
        Candidate {
            meta: meta_with(release_id, album),
            max_diff_ms: 0,
        }
    }

    fn cached_toc() -> DiscToc {
        DiscToc {
            offsets: vec![150, 1650],
            leadout: 3300,
        }
    }

    #[test]
    fn cached_meta_is_applied_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        app.meta = None;
        app.cover = None;
        let toc = cached_toc();
        app.meta_cache
            .insert(toc.clone(), vec![candidate("rel-1", "Cached Album")]);

        let cached = app.meta_cache.candidates(&toc).unwrap().to_vec();
        app.apply_cached_meta(&cached);

        assert_eq!(app.meta.as_ref().unwrap()[0].album, "Cached Album");
        assert!(app.status.is_some());
        // The demo drive's cover stands in for the fetched one.
        assert!(app.cover.is_some());
    }

    #[test]
    fn cached_empty_meta_reports_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        app.meta = None;
        let toc = cached_toc();
        app.meta_cache.insert(toc.clone(), vec![]);

        let cached = app.meta_cache.candidates(&toc).unwrap().to_vec();
        app.apply_cached_meta(&cached);

        assert!(app.meta.is_none());
        assert_eq!(app.status.as_deref(), Some("no release matched the disc"));
    }

    #[test]
    fn candidates_event_is_cached_and_applied() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        let (tx, rx) = mpsc::channel();
        app.meta_rx = Some(rx);
        app.meta_gen = 1;
        app.meta = None;
        let toc = cached_toc();

        tx.send(MetaEvent::Candidates {
            id: 1,
            toc: toc.clone(),
            candidates: vec![candidate("rel-1", "Cached Album")],
        })
        .unwrap();
        app.drain_meta_events();

        assert_eq!(app.meta.as_ref().unwrap()[0].album, "Cached Album");
        assert_eq!(app.meta_cache.candidates(&toc).map(|c| c.len()), Some(1));
    }

    #[test]
    fn a_stale_candidates_event_still_fills_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        let (tx, rx) = mpsc::channel();
        app.meta_rx = Some(rx);
        app.meta_gen = 2; // a newer lookup has superseded this one
        app.meta = None;
        let toc = cached_toc();

        tx.send(MetaEvent::Candidates {
            id: 1,
            toc: toc.clone(),
            candidates: vec![candidate("rel-1", "Cached Album")],
        })
        .unwrap();
        app.drain_meta_events();

        // Not applied to the (newer) selection…
        assert!(app.meta.is_none());
        // …but remembered for the session anyway.
        assert!(app.meta_cache.candidates(&toc).is_some());
    }

    #[test]
    fn cover_event_is_cached_per_release() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        let (tx, rx) = mpsc::channel();
        app.meta_rx = Some(rx);
        app.meta_tx = Some(tx.clone());
        app.meta_gen = 1;
        app.meta = Some(vec![meta_with("rel-1", "The Album")]);

        tx.send(MetaEvent::Cover {
            id: 1,
            release_id: "rel-1".into(),
            cover: Ok(Some(vec![9, 8, 7])),
        })
        .unwrap();
        app.drain_meta_events();

        assert_eq!(app.cover.as_deref(), Some(&[9, 8, 7][..]));
        assert_eq!(app.meta_cache.cover("rel-1"), Some(&Some(vec![9u8, 8, 7])));
        // The lookup channel is done once the cover lands.
        assert!(app.meta_rx.is_none());
        assert!(app.meta_tx.is_none());
        drop(tx);
    }

    #[test]
    fn cover_error_reports_status_for_the_selected_release() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        let (tx, rx) = mpsc::channel();
        app.meta_rx = Some(rx);
        app.meta_tx = Some(tx.clone());
        app.meta_gen = 1;
        app.meta = Some(vec![meta_with("rel-1", "The Album")]);

        tx.send(MetaEvent::Cover {
            id: 1,
            release_id: "rel-1".into(),
            cover: Err("network error: host not found".into()),
        })
        .unwrap();
        app.drain_meta_events();

        assert_eq!(
            app.status.as_deref(),
            Some("cover art unavailable: network error: host not found")
        );
        // A failed fetch is not cached, so a retry can still reach it.
        assert!(app.meta_cache.cover("rel-1").is_none());
        drop(tx);
    }
}
