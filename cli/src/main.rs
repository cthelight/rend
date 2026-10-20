use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::result::Result;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use clap::Parser;
use rend_core::{Device, DriveNames, Error, FRAMES_PER_SECOND, Toc, drive_label};
use rend_encode::{Format, FormatSet};
use rend_meta::{
    Candidate, DEFAULT_TEMPLATE, DiscMeta, DiscToc, MetaCache, Template, cover_art, disc_id,
    lookup_candidates,
};
use rend_rip::{RipEvent, RipJob, RipSource, spawn};

#[derive(Parser)]
#[command(name = "rend", version, about = "Rip audio CDs from the command line")]
struct Cli {
    /// CD-ROM device(s) to use (repeatable; default: first device found).
    #[arg(short, long, global = true)]
    devices: Vec<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// List CD-ROM devices.
    Drives {
        /// Friendly name for a drive, as `DRIVE=NAME` (repeatable; `sr0`
        /// or `/dev/sr0`), replacing the vendor and model. Also
        /// `REND_DRIVE_NAMES`, comma-separated pairs.
        #[arg(
            long = "drive-name",
            value_name = "DRIVE=NAME",
            value_delimiter = ',',
            env = "REND_DRIVE_NAMES"
        )]
        drive_names: Vec<String>,
    },
    /// Show the table of contents of the disc.
    Toc,
    /// Rip audio tracks to the selected output formats (across several drives in parallel).
    Rip {
        #[command(flatten)]
        args: RipArgs,
    },
    /// Look up and show the disc's metadata (album, artist, tracks).
    Info {
        /// List every candidate match, best first, instead of only the
        /// closest one.
        #[arg(long)]
        matches: bool,
        /// Show the Nth candidate match (1 is the best).
        #[arg(long = "match", value_name = "N")]
        r#match: Option<usize>,
    },
    /// Eject the disc.
    Eject,
}

/// The `rend rip` options, grouped so the command dispatch stays narrow.
#[derive(clap::Args)]
struct RipArgs {
    /// Directory to write the track files to.
    #[arg(short, long, default_value = ".")]
    output_dir: PathBuf,
    /// Output format (repeatable; each value may be `format` or
    /// `format:quality`; default: the best available format — flac when
    /// ffmpeg supports it, otherwise wav).
    #[arg(short = 'F', long = "format", value_name = "FORMAT")]
    formats: Vec<String>,
    /// Quality for output formats that do not include an explicit quality
    /// (for example flac 0-8, mp3 320, aac 256, opus 128, vorbis 6).
    #[arg(short = 'q', long = "quality", value_name = "QUALITY")]
    quality: Option<String>,
    /// Naming template for the track files: `<artist>/<album>/<number>
    /// <title>` by default, with the tokens `<artist>`, `<album>`,
    /// `<album-artist>`, `<year>`, `<number>`, `<title>`, and
    /// `<track-artist>`.
    #[arg(short = 'T', long, default_value = DEFAULT_TEMPLATE)]
    template: String,
    /// Only rip the given track number (repeatable; default: all audio tracks).
    #[arg(short = 't', long = "track")]
    tracks: Vec<u8>,
    /// Overwrite existing files.
    #[arg(short, long)]
    force: bool,
    /// Rip every discovered drive in parallel.
    #[arg(long)]
    all: bool,
    /// Skip looking up and embedding the disc's metadata.
    #[arg(long)]
    no_metadata: bool,
    /// Tag the rip with the Nth candidate match (1 is the best; see
    /// `rend info --matches`).
    #[arg(long = "match", value_name = "N")]
    r#match: Option<usize>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rend: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), Error> {
    match cli.command {
        Command::Drives { drive_names } => cmd_drives(&drive_names),
        Command::Toc => cmd_toc(single_device(&cli.devices)?),
        Command::Rip { args } => cmd_rip(&cli.devices, &args),
        Command::Info { matches, r#match } => {
            cmd_info(single_device(&cli.devices)?, matches, r#match)
        }
        Command::Eject => cmd_eject(single_device(&cli.devices)?),
    }
}

/// A single-device subcommand must be given at most one `-d`.
fn single_device(devices: &[String]) -> Result<Option<&str>, Error> {
    if devices.len() > 1 {
        return Err(Error::Unexpected(format!(
            "expected a single device, got {len}: use `rend rip` for several at once",
            len = devices.len()
        )));
    }
    Ok(devices.first().map(String::as_str))
}

/// The devices `rend rip` works on: every discovered drive with `--all`,
/// the explicit `-d` values, or the first discovered drive when none given.
fn rip_devices(devices: &[String], all: bool) -> Result<Vec<Device>, Error> {
    if all {
        let found = Device::discover()?;
        if found.is_empty() {
            return Err(Error::NoDevices);
        }
        return Ok(found);
    }
    if devices.is_empty() {
        let mut found = Device::discover()?;
        found.truncate(1);
        if found.is_empty() {
            return Err(Error::NoDevices);
        }
        return Ok(found);
    }
    devices.iter().map(Device::open).collect()
}

/// Opens the device named by `path`, or the first device found.
fn open_device(path: Option<&str>) -> Result<Device, Error> {
    match path {
        Some(p) => Device::open(p),
        None => Device::discover()?
            .into_iter()
            .next()
            .ok_or(Error::NoDevices),
    }
}

fn cmd_drives(drive_names: &[String]) -> Result<(), Error> {
    // Parsed before discovery so a malformed pair fails even with no
    // hardware present.
    let names = DriveNames::from_pairs(drive_names)
        .map_err(|e| Error::Unexpected(format!("invalid --drive-name: {e}")))?;
    let devices = Device::discover()?;
    if devices.is_empty() {
        return Err(Error::NoDevices);
    }
    for dev in &devices {
        let info = dev.info()?;
        let label = drive_label(&names, &info);
        println!("{:<10} {:<36} {}", info.path, label, info.drive_status);
    }
    Ok(())
}

fn cmd_toc(device: Option<&str>) -> Result<(), Error> {
    let dev = open_device(device)?;
    let toc = dev.toc()?;

    println!("{}", dev.path());
    if let Some(mcn) = dev.mcn()? {
        println!("disc id {mcn}");
    }
    for track in &toc.tracks {
        let end = toc.end_lba(track.number).unwrap_or(toc.leadout_lba);
        let start = track
            .start_msf()
            .map_or_else(|| "?".into(), |m| m.to_string());
        println!(
            "track {:>2}  {:<6}  start {:>9}  length {:>8}",
            track.number,
            track.kind,
            start,
            fmt_duration(track.frames(end)),
        );
    }
    Ok(())
}

fn cmd_info(device: Option<&str>, matches: bool, r#match: Option<usize>) -> Result<(), Error> {
    let dev = open_device(device)?;
    let toc = dev.toc()?;
    let lbas: Vec<u32> = toc.audio_tracks().map(|t| t.start_lba).collect();
    if lbas.is_empty() {
        return Err(Error::NoAudioTracks {
            path: dev.path().into(),
        });
    }
    let id = disc_id(&lbas);
    let toc_ = DiscToc {
        offsets: lbas,
        leadout: toc.leadout_lba,
    };
    let candidates =
        lookup_candidates(&toc_).map_err(|e| Error::Unexpected(lookup_error(e, &id)))?;
    println!("disc id    {id}");

    if let Some(n) = r#match {
        let disc = nth_candidate(&candidates, n)?;
        print_match(disc, None);
        return Ok(());
    }
    if matches {
        for (i, candidate) in candidates.iter().enumerate() {
            print_match(&candidate.meta, Some((i + 1, candidates.len())));
        }
        return Ok(());
    }

    // The disc may still have a close-but-distant candidate the default
    // lookup is not willing to accept.
    let Some(disc) = candidates.iter().find(|c| c.accepted()) else {
        let n = candidates.len();
        let hint = if n > 0 {
            format!("; `rend info --matches` lists {n} close candidate(s)")
        } else {
            " (is it a commercial disc?)".into()
        };
        return Err(Error::Unexpected(format!(
            "no release matched disc {id}{hint}"
        )));
    };
    print_match(&disc.meta, None);
    Ok(())
}

/// Prints one match's metadata, optionally under a `match i of n` header.
fn print_match(disc: &DiscMeta, position: Option<(usize, usize)>) {
    if let Some((i, n)) = position {
        println!();
        println!("match {i} of {n}");
    }
    println!("album      {}", disc.album);
    println!("artist     {}", disc.artist);
    if let Some(year) = &disc.year {
        println!("year       {year}");
    }
    println!("release id {}", disc.release_id);
    println!();
    for (i, t) in disc.tracks.iter().enumerate() {
        let artist = t.artist.as_deref().unwrap_or(&disc.artist);
        println!("track {:>2}  {:<30} {}", i + 1, t.title, artist);
    }
}

/// A lookup error, phrased for the disc with the given id.
fn lookup_error(e: rend_meta::lookup::Error, id: &str) -> String {
    match e {
        rend_meta::lookup::Error::NotFound => {
            format!("no release matched disc {id} (is it a commercial disc?)")
        }
        other => format!("metadata lookup failed: {other}"),
    }
}

/// The `n`th (1-based) candidate, if the list is long enough.
fn nth_candidate(candidates: &[Candidate], n: usize) -> Result<&DiscMeta, Error> {
    let Some(i) = n.checked_sub(1) else {
        return Err(Error::Unexpected("--match numbers start at 1".into()));
    };
    candidates.get(i).map(|c| &c.meta).ok_or_else(|| {
        Error::Unexpected(format!(
            "no candidate {n}: the disc has {count} candidate match(es); see `rend info --matches`",
            count = candidates.len()
        ))
    })
}

/// The options shared by a drive's rip worker and its per-track rips.
struct RipOptions {
    formats: Vec<Format>,
    template: Template,
    force: bool,
    no_metadata: bool,
    r#match: Option<usize>,
    prefix: String,
    progress: bool,
    /// The lookups this run has already made, shared by every worker.
    cache: Arc<Mutex<MetaCache>>,
}

fn cmd_rip(devices: &[String], args: &RipArgs) -> Result<(), Error> {
    let RipArgs {
        output_dir,
        formats,
        quality,
        template,
        tracks,
        force,
        all,
        no_metadata,
        r#match,
    } = args;
    let set = FormatSet::probe();
    let formats = set
        .resolve_many(formats, quality.as_deref())
        .map_err(Error::Unexpected)?;
    let template = Template::parse(template)
        .map_err(|e| Error::Unexpected(format!("invalid --template: {e}")))?;
    let resolved = rip_devices(devices, *all)?;
    // Every worker looks the disc up once; the rest hit this cache.
    let cache = Arc::new(Mutex::new(MetaCache::new()));

    // One drive keeps the flat layout; several get a subdirectory each, named
    // after the device (e.g. `sr0`), so track files never collide.
    let multi = resolved.len() > 1;
    let jobs: Vec<(Device, PathBuf, RipOptions)> = resolved
        .into_iter()
        .map(|dev| {
            let base = dev
                .path()
                .rsplit('/')
                .next()
                .unwrap_or(dev.path())
                .to_string();
            let dir = if multi {
                output_dir.join(&base)
            } else {
                output_dir.to_path_buf()
            };
            let prefix = if multi {
                format!("[{base}] ")
            } else {
                String::new()
            };
            let opts = RipOptions {
                formats: formats.clone(),
                template: template.clone(),
                force: *force,
                no_metadata: *no_metadata,
                r#match: *r#match,
                prefix,
                progress: !multi,
                cache: Arc::clone(&cache),
            };
            (dev, dir, opts)
        })
        .collect();

    // One worker thread per drive; they all run at the same time.
    let mut handles = Vec::new();
    for (dev, dir, opts) in jobs {
        let only = tracks.to_vec();
        let path = dev.path().to_string();
        let handle = thread::Builder::new()
            .name(format!("rend-rip-{path}"))
            .spawn(move || rip_device(dev, &dir, &only, &opts))
            .map_err(|e| Error::Unexpected(format!("failed to spawn rip thread: {e}")))?;
        handles.push((path, handle));
    }

    let mut failed_drives = 0usize;
    let mut tracks_total = 0usize;
    let mut tracks_failed = 0usize;
    for (path, handle) in handles {
        match handle.join() {
            Ok(Ok((tracks, failed))) => {
                tracks_total += tracks;
                tracks_failed += failed;
                if failed > 0 {
                    eprintln!("rend: {path}: {failed} of {tracks} track(s) failed");
                }
            }
            Ok(Err(e)) => {
                eprintln!("rend: {path}: {e}");
                failed_drives += 1;
            }
            Err(_) => {
                eprintln!("rend: {path}: rip thread panicked");
                failed_drives += 1;
            }
        }
    }

    let failed = failed_drives + tracks_failed;
    if failed > 0 {
        return Err(Error::Unexpected(format!(
            "{failed} of {tracks_total} track(s) failed"
        )));
    }
    Ok(())
}

/// Rips one drive's selected tracks to `out_dir` in the given formats,
/// through the shared rip worker, returning the number of tracks attempted
/// and how many of them failed.
fn rip_device(
    dev: Device,
    out_dir: &Path,
    only: &[u8],
    opts: &RipOptions,
) -> Result<(usize, usize), Error> {
    let prefix = opts.prefix.as_str();
    dev.require_disc()?;
    let toc = dev.toc()?;
    // The medium catalog number, if the drive reports one: best effort, an
    // unreadable MCN is no reason to fail the rip.
    let mcn = dev.mcn().ok().flatten();

    for &n in only {
        match toc.track(n) {
            Some(t) if t.is_audio() => {}
            Some(_) => {
                return Err(Error::Unexpected(format!(
                    "track {n} on {} is a data track, not audio",
                    dev.path()
                )));
            }
            None => {
                return Err(Error::Unexpected(format!(
                    "track {n} not found on disc in {}",
                    dev.path()
                )));
            }
        }
    }

    let selected: Vec<u8> = toc
        .audio_tracks()
        .filter(|t| only.is_empty() || only.contains(&t.number))
        .map(|t| t.number)
        .collect();
    if selected.is_empty() {
        return Err(Error::NoAudioTracks {
            path: dev.path().into(),
        });
    }

    let metadata = if opts.no_metadata {
        None
    } else if let Some(n) = opts.r#match {
        Some(resolve_match(&toc, n, prefix, &opts.cache)?)
    } else {
        lookup_metadata(&toc, prefix, &opts.cache)
    };
    let (meta, cover) = match metadata {
        Some((meta, cover)) => (Some(meta), cover),
        None => (None, None),
    };

    let (tx, rx) = mpsc::channel();
    let job = RipJob {
        source: RipSource::Device(dev),
        toc,
        tracks: selected,
        out_dir: out_dir.to_path_buf(),
        formats: opts.formats.clone(),
        template: opts.template.clone(),
        force: opts.force,
        stop: Arc::new(AtomicBool::new(false)),
        meta,
        cover,
        catalog_number: mcn,
    };
    let worker = spawn(job, tx);
    let (total, failed) = drain_events(rx, prefix, opts.progress);
    if worker.join().is_err() {
        return Err(Error::Unexpected("rip worker panicked".into()));
    }
    Ok((total, failed))
}

/// Drains the worker's events until `Finished`, reporting each track's
/// outcome on stderr, and returns the number of tracks attempted and how
/// many of them failed (a skipped track counts as failed, as it did before
/// the worker existed). The `\r` progress line is only printed when
/// `progress` is on — that is only safe for a single drive, since parallel
/// drives would clobber each other's progress line.
fn drain_events(rx: mpsc::Receiver<RipEvent>, prefix: &str, progress: bool) -> (usize, usize) {
    let mut total = 0usize;
    let mut failed = 0usize;
    for event in rx {
        match event {
            RipEvent::Progress {
                number,
                bytes_done,
                bytes_total,
            } => {
                if progress {
                    eprint!(
                        "\r{prefix}track {number:02}: {:3}%  ",
                        bytes_done * 100 / bytes_total.max(1)
                    );
                }
            }
            RipEvent::TrackSkipped { path, .. } => {
                eprintln!(
                    "{prefix}{} already exists (use --force to overwrite)",
                    path.display()
                );
                failed += 1;
            }
            RipEvent::TrackDone { number, path, .. } => {
                if progress {
                    eprint!("\r");
                }
                eprintln!("{prefix}track {number:02}: wrote {}", path.display());
            }
            RipEvent::TrackFailed { number, error } => {
                if progress {
                    eprint!("\r");
                }
                eprintln!("{prefix}track {number:02}: {error}");
                failed += 1;
            }
            RipEvent::TrackStarted { .. } => {}
            RipEvent::Finished { total: n, .. } => {
                total = n;
            }
        }
    }
    (total, failed)
}

/// The candidates for `toc`, from the shared cache or a fresh lookup.
///
/// The lock is held only while consulting and updating the cache, never
/// across the network. A failed lookup is not cached, so a retry can still
/// reach the network; a successful one (even an empty list) is.
fn candidates_with_cache(
    cache: &Mutex<MetaCache>,
    toc: &DiscToc,
) -> Result<Vec<Candidate>, rend_meta::lookup::Error> {
    if let Some(candidates) = cache.lock().unwrap().candidates(toc) {
        return Ok(candidates.to_vec());
    }
    let candidates = lookup_candidates(toc)?;
    cache
        .lock()
        .unwrap()
        .insert(toc.clone(), candidates.clone());
    Ok(candidates)
}

/// The accepted match for `toc`, from the shared cache or a fresh lookup.
///
/// The same semantics as `lookup_disc`, but a disc this run has already
/// looked up never goes to the network again.
fn lookup_accepted(
    cache: &Mutex<MetaCache>,
    toc: &DiscToc,
) -> Result<DiscMeta, rend_meta::lookup::Error> {
    let candidates = candidates_with_cache(cache, toc)?;
    candidates
        .into_iter()
        .find(|c| c.accepted())
        .map(|c| c.meta)
        .ok_or(rend_meta::lookup::Error::NotFound)
}

/// The cover for `release_id`, from the shared cache or a fresh fetch.
///
/// As with [`candidates_with_cache`], the lock never spans the network,
/// and a failed fetch is not cached.
fn cover_art_cached(
    cache: &Mutex<MetaCache>,
    release_id: &str,
) -> Result<Option<Vec<u8>>, rend_meta::lookup::Error> {
    if let Some(art) = cache.lock().unwrap().cover(release_id) {
        return Ok(art.clone());
    }
    let art = cover_art(release_id)?;
    cache
        .lock()
        .unwrap()
        .cover_insert(release_id.to_string(), art.clone());
    Ok(art)
}

/// Looks up the disc's metadata and cover art for the given TOC.
///
/// A disc this run has already looked up is answered from the shared
/// cache. Any failure (no match, no network, …) is reported as a warning
/// and yields `None`, since a missing lookup must not stop a rip.
fn lookup_metadata(
    toc: &Toc,
    prefix: &str,
    cache: &Mutex<MetaCache>,
) -> Option<(DiscMeta, Option<Vec<u8>>)> {
    let lbas: Vec<u32> = toc.audio_tracks().map(|t| t.start_lba).collect();
    if lbas.is_empty() {
        return None;
    }
    let disc = match lookup_accepted(
        cache,
        &DiscToc {
            offsets: lbas,
            leadout: toc.leadout_lba,
        },
    ) {
        Ok(disc) => disc,
        Err(e) => {
            eprintln!("{prefix}metadata lookup failed: {e}");
            return None;
        }
    };
    let art = match cover_art_cached(cache, &disc.release_id) {
        Ok(art) => art,
        Err(e) => {
            eprintln!("{prefix}cover art lookup failed: {e}");
            None
        }
    };
    eprintln!(
        "{prefix}{} — {}{}",
        disc.artist,
        disc.album,
        disc.year
            .as_deref()
            .map(|y| format!(" ({y})"))
            .unwrap_or_default()
    );
    Some((disc, art))
}

/// Resolves the `n`th (1-based) candidate match of the disc to metadata and
/// cover art.
///
/// Unlike [`lookup_metadata`], any failure is an error rather than a
/// warning: an explicitly requested match must not be silently replaced by
/// the default one.
fn resolve_match(
    toc: &Toc,
    n: usize,
    prefix: &str,
    cache: &Mutex<MetaCache>,
) -> Result<(DiscMeta, Option<Vec<u8>>), Error> {
    let lbas: Vec<u32> = toc.audio_tracks().map(|t| t.start_lba).collect();
    let id = disc_id(&lbas);
    let candidates = candidates_with_cache(
        cache,
        &DiscToc {
            offsets: lbas,
            leadout: toc.leadout_lba,
        },
    )
    .map_err(|e| Error::Unexpected(format!("{prefix}{}", lookup_error(e, &id))))?;
    let disc = nth_candidate(&candidates, n)?;
    let art = cover_art_cached(cache, &disc.release_id)
        .map_err(|e| Error::Unexpected(format!("{prefix}cover art lookup failed: {e}")))?;
    eprintln!(
        "{prefix}{} — {}{}",
        disc.artist,
        disc.album,
        disc.year
            .as_deref()
            .map(|y| format!(" ({y})"))
            .unwrap_or_default()
    );
    Ok((disc.clone(), art))
}

fn cmd_eject(device: Option<&str>) -> Result<(), Error> {
    let dev = open_device(device)?;
    dev.eject()?;
    println!("ejected {}", dev.path());
    Ok(())
}

/// Formats a frame count as `m:ss`.
fn fmt_duration(frames: u32) -> String {
    let secs = frames / FRAMES_PER_SECOND;
    format!("{:1}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_format() {
        assert_eq!(fmt_duration(0), "0:00");
        assert_eq!(fmt_duration(FRAMES_PER_SECOND * 75), "1:15");
        assert_eq!(fmt_duration(FRAMES_PER_SECOND * 60 + 37), "1:00");
    }

    fn candidate(release_id: &str) -> Candidate {
        Candidate {
            meta: DiscMeta {
                album: "The Album".into(),
                artist: "The Band".into(),
                album_artist: None,
                year: None,
                release_id: release_id.into(),
                release_artist_id: None,
                disc_number: None,
                disc_count: None,
                tracks: vec![],
            },
            max_diff_ms: 0,
        }
    }

    #[test]
    fn nth_candidate_is_one_based() {
        let candidates = [candidate("r1"), candidate("r2")];
        assert_eq!(nth_candidate(&candidates, 1).unwrap().release_id, "r1");
        assert_eq!(nth_candidate(&candidates, 2).unwrap().release_id, "r2");
        assert!(nth_candidate(&candidates, 0).is_err());
        assert!(nth_candidate(&candidates, 3).is_err());
        let none: [Candidate; 0] = [];
        assert!(nth_candidate(&none, 1).is_err());
    }

    #[test]
    fn a_cached_disc_is_served_without_the_network() {
        let cache = Mutex::new(MetaCache::new());
        let toc = DiscToc {
            offsets: vec![150, 1650],
            leadout: 3300,
        };
        cache
            .lock()
            .unwrap()
            .insert(toc.clone(), vec![candidate("r1")]);

        // A cached disc never touches the network: the candidates come
        // straight from the cache.
        let found = candidates_with_cache(&cache, &toc).unwrap();
        assert_eq!(found[0].meta.release_id, "r1");
        assert!(lookup_accepted(&cache, &toc).unwrap().release_id == "r1");

        // A cached "no match" stays a no-match, not a fetch.
        let other = DiscToc {
            offsets: vec![150, 1650, 2450],
            leadout: 3300,
        };
        cache.lock().unwrap().insert(other.clone(), vec![]);
        assert!(lookup_accepted(&cache, &other).is_err());

        // A cached cover is served the same way, absence included.
        cache
            .lock()
            .unwrap()
            .cover_insert("r1".into(), Some(vec![1, 2]));
        assert_eq!(cover_art_cached(&cache, "r1").unwrap(), Some(vec![1, 2]));
        cache.lock().unwrap().cover_insert("r2".into(), None);
        assert_eq!(cover_art_cached(&cache, "r2").unwrap(), None);
    }

    #[test]
    fn single_device_rejects_multiple() {
        let one = ["/dev/sr0".to_string()];
        assert!(single_device(&one).is_ok());

        let two = ["/dev/sr0".to_string(), "/dev/sr1".to_string()];
        assert!(single_device(&two).is_err());
    }
}
