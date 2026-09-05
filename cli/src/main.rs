use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::result::Result;
use std::thread;

use clap::Parser;
use rend_core::{CddaStream, Device, Error, FRAME_SIZE, FRAMES_PER_SECOND, Toc, Track};
use rend_encode::{Format, ffmpeg_available};
use rend_meta::{
    DiscMeta, DiscToc, TrackTags, apply, cover_art, disc_dir, disc_id, lookup_disc,
    lookup_disc_all, track_path,
};

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
    Drives,
    /// Show the table of contents of the disc.
    Toc,
    /// Rip audio tracks to FLAC (default) or WAV files (across several drives in parallel).
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
    /// Output format: flac (default, transcoded with ffmpeg) or wav.
    #[arg(short = 'F', long = "format", default_value = "flac", value_parser = Format::parse)]
    format: Format,
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
        Command::Drives => cmd_drives(),
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

fn cmd_drives() -> Result<(), Error> {
    let devices = Device::discover()?;
    if devices.is_empty() {
        return Err(Error::NoDevices);
    }
    for dev in &devices {
        let info = dev.info()?;
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

    if let Some(n) = r#match {
        let candidates =
            lookup_disc_all(&toc_).map_err(|e| Error::Unexpected(lookup_error(e, &id)))?;
        let disc = nth_candidate(&candidates, n)?;
        println!("disc id    {id}");
        print_match(disc, None);
        return Ok(());
    }
    if matches {
        let candidates =
            lookup_disc_all(&toc_).map_err(|e| Error::Unexpected(lookup_error(e, &id)))?;
        println!("disc id    {id}");
        for (i, disc) in candidates.iter().enumerate() {
            print_match(disc, Some((i + 1, candidates.len())));
        }
        return Ok(());
    }

    let disc = match lookup_disc(&toc_) {
        Ok(disc) => disc,
        Err(rend_meta::lookup::Error::NotFound) => {
            // The disc may still have a close-but-distant candidate the
            // default lookup is not willing to accept.
            let n = lookup_disc_all(&toc_).map(|c| c.len()).unwrap_or(0);
            let hint = if n > 0 {
                format!("; `rend info --matches` lists {n} close candidate(s)")
            } else {
                " (is it a commercial disc?)".into()
            };
            return Err(Error::Unexpected(format!(
                "no release matched disc {id}{hint}"
            )));
        }
        Err(e) => return Err(Error::Unexpected(lookup_error(e, &id))),
    };
    println!("disc id    {id}");
    print_match(&disc, None);
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
fn nth_candidate(candidates: &[DiscMeta], n: usize) -> Result<&DiscMeta, Error> {
    let Some(i) = n.checked_sub(1) else {
        return Err(Error::Unexpected("--match numbers start at 1".into()));
    };
    candidates.get(i).ok_or_else(|| {
        Error::Unexpected(format!(
            "no candidate {n}: the disc has {count} candidate match(es); see `rend info --matches`",
            count = candidates.len()
        ))
    })
}

/// The options shared by a drive's rip worker and its per-track rips.
struct RipOptions {
    format: Format,
    force: bool,
    no_metadata: bool,
    r#match: Option<usize>,
    prefix: String,
    progress: bool,
}

fn cmd_rip(devices: &[String], args: &RipArgs) -> Result<(), Error> {
    let RipArgs {
        output_dir,
        format,
        tracks,
        force,
        all,
        no_metadata,
        r#match,
    } = args;
    if format.requires_ffmpeg() && !ffmpeg_available() {
        return Err(Error::Unexpected(
            "ffmpeg not found in PATH — install it for FLAC output, or rip with --format wav"
                .into(),
        ));
    }
    let resolved = rip_devices(devices, *all)?;

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
                format: *format,
                force: *force,
                no_metadata: *no_metadata,
                r#match: *r#match,
                prefix,
                progress: !multi,
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

/// Rips one drive's selected tracks to `out_dir` in the given format,
/// returning the number of tracks attempted and how many of them failed.
fn rip_device(
    mut dev: Device,
    out_dir: &Path,
    only: &[u8],
    opts: &RipOptions,
) -> Result<(usize, usize), Error> {
    let prefix = opts.prefix.as_str();
    dev.require_disc()?;
    let toc = dev.toc()?;

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

    let selected: Vec<&Track> = toc
        .audio_tracks()
        .filter(|t| only.is_empty() || only.contains(&t.number))
        .collect();
    if selected.is_empty() {
        return Err(Error::NoAudioTracks {
            path: dev.path().into(),
        });
    }

    let total_audio = toc.audio_tracks().count();
    let metadata = if opts.no_metadata {
        None
    } else if let Some(n) = opts.r#match {
        Some(resolve_match(&toc, n, prefix)?)
    } else {
        lookup_metadata(&toc, prefix)
    };

    // With looked-up metadata the tracks live under an artist subdirectory;
    // without it they stay flat in the output dir.
    let base_dir = match &metadata {
        Some((disc, _)) => disc_dir(out_dir, disc),
        None => out_dir.to_path_buf(),
    };

    std::fs::create_dir_all(&base_dir)?;
    dev.spin_up().ok();

    let mut failed = 0usize;
    for track in &selected {
        let end = toc.end_lba(track.number).unwrap_or(toc.leadout_lba);
        let frames = track.frames(end);
        let path = match &metadata {
            Some((disc, _)) => {
                let position = track_position(&toc, track.number).unwrap_or(1);
                track_path(
                    out_dir,
                    disc,
                    track.number,
                    position,
                    opts.format.extension(),
                )
            }
            None => out_dir.join(format!(
                "track{:02}.{}",
                track.number,
                opts.format.extension()
            )),
        };

        if path.exists() && !opts.force {
            eprintln!(
                "{prefix}{} already exists (use --force to overwrite)",
                path.display()
            );
            failed += 1;
            continue;
        }

        match rip_track(&mut dev, track, frames, &path, opts) {
            Ok(()) => {
                eprintln!(
                    "{prefix}track {:02}: wrote {}",
                    track.number,
                    path.display()
                );
                if let Some((disc, art)) = &metadata {
                    tag_track(&toc, &path, disc, art, track.number, total_audio, prefix);
                }
            }
            Err(e) => {
                std::fs::remove_file(&path).ok();
                eprintln!("{prefix}track {:02}: {e}", track.number);
                failed += 1;
            }
        }
    }

    dev.spin_down().ok();
    Ok((selected.len(), failed))
}

/// Rips a single track to an output file in the given format, reporting
/// progress on stderr when enabled (only safe for a single drive — parallel
/// drives would clobber each other's `\r` progress line).
fn rip_track(
    dev: &mut Device,
    track: &Track,
    frames: u32,
    path: &Path,
    opts: &RipOptions,
) -> io::Result<()> {
    let prefix = opts.prefix.as_str();
    let progress = opts.progress;
    let mut stream = CddaStream::new(dev, track.start_lba, frames);
    let mut file = opts.format.create_file(path)?;
    let total = stream.total_bytes();
    let mut buf = vec![0u8; FRAMES_PER_SECOND as usize * FRAME_SIZE];
    let mut done = 0u64;

    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write(&buf[..n])?;
        done += n as u64;
        if progress {
            eprint!(
                "\r{prefix}track {:02}: {:3}%  ",
                track.number,
                done * 100 / total as u64
            );
        }
    }

    if progress {
        eprint!("\r");
    }
    file.finish()
}

/// Looks up the disc's metadata and cover art for the given TOC.
///
/// Any failure (no match, no network, …) is reported as a warning and yields
/// `None`, since a missing lookup must not stop a rip.
fn lookup_metadata(toc: &Toc, prefix: &str) -> Option<(DiscMeta, Option<Vec<u8>>)> {
    let lbas: Vec<u32> = toc.audio_tracks().map(|t| t.start_lba).collect();
    if lbas.is_empty() {
        return None;
    }
    let disc = match lookup_disc(&DiscToc {
        offsets: lbas,
        leadout: toc.leadout_lba,
    }) {
        Ok(disc) => disc,
        Err(e) => {
            eprintln!("{prefix}metadata lookup failed: {e}");
            return None;
        }
    };
    let art = match cover_art(&disc.release_id) {
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
fn resolve_match(toc: &Toc, n: usize, prefix: &str) -> Result<(DiscMeta, Option<Vec<u8>>), Error> {
    let lbas: Vec<u32> = toc.audio_tracks().map(|t| t.start_lba).collect();
    let id = disc_id(&lbas);
    let candidates = lookup_disc_all(&DiscToc {
        offsets: lbas,
        leadout: toc.leadout_lba,
    })
    .map_err(|e| Error::Unexpected(format!("{prefix}{}", lookup_error(e, &id))))?;
    let disc = nth_candidate(&candidates, n)?;
    let art = cover_art(&disc.release_id)
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

/// Applies the looked-up metadata to one ripped track file, warning (rather
/// than failing) if the tags cannot be written.
fn tag_track(
    toc: &Toc,
    path: &Path,
    disc: &DiscMeta,
    art: &Option<Vec<u8>>,
    number: u8,
    total: usize,
    prefix: &str,
) {
    let Some(position) = track_position(toc, number) else {
        return;
    };
    let Some(tags) = TrackTags::for_track(disc, position, total) else {
        return;
    };
    if let Err(e) = apply(path, &tags, art.as_deref()) {
        eprintln!("{prefix}track {number:02}: warning: could not write tags: {e}");
    }
}

/// The 1-based position of the given track number among the disc's audio tracks.
fn track_position(toc: &Toc, number: u8) -> Option<usize> {
    toc.audio_tracks()
        .position(|t| t.number == number)
        .map(|i| i + 1)
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
    use rend_core::TrackType;

    #[test]
    fn duration_format() {
        assert_eq!(fmt_duration(0), "0:00");
        assert_eq!(fmt_duration(FRAMES_PER_SECOND * 75), "1:15");
        assert_eq!(fmt_duration(FRAMES_PER_SECOND * 60 + 37), "1:00");
    }

    #[test]
    fn nth_candidate_is_one_based() {
        let one = DiscMeta {
            album: "A".into(),
            artist: "B".into(),
            album_artist: None,
            year: None,
            release_id: "r1".into(),
            tracks: vec![],
        };
        let two = DiscMeta {
            album: "C".into(),
            artist: "B".into(),
            album_artist: None,
            year: None,
            release_id: "r2".into(),
            tracks: vec![],
        };
        let candidates = [one.clone(), two.clone()];
        assert_eq!(nth_candidate(&candidates, 1).unwrap().release_id, "r1");
        assert_eq!(nth_candidate(&candidates, 2).unwrap().release_id, "r2");
        assert!(nth_candidate(&candidates, 0).is_err());
        assert!(nth_candidate(&candidates, 3).is_err());
        let none: [DiscMeta; 0] = [];
        assert!(nth_candidate(&none, 1).is_err());
    }

    #[test]
    fn single_device_rejects_multiple() {
        let one = ["/dev/sr0".to_string()];
        assert!(single_device(&one).is_ok());

        let two = ["/dev/sr0".to_string(), "/dev/sr1".to_string()];
        assert!(single_device(&two).is_err());
    }

    #[test]
    fn track_position_is_one_based_among_audio_tracks() {
        // Track 2 is a data track, so the audio tracks are numbered 1 and 3.
        let toc = Toc {
            tracks: vec![
                Track {
                    number: 1,
                    kind: TrackType::Audio,
                    start_lba: 0,
                },
                Track {
                    number: 2,
                    kind: TrackType::Data,
                    start_lba: 1_000,
                },
                Track {
                    number: 3,
                    kind: TrackType::Audio,
                    start_lba: 2_000,
                },
            ],
            leadout_lba: 3_000,
        };
        assert_eq!(track_position(&toc, 1), Some(1));
        assert_eq!(track_position(&toc, 3), Some(2));
        assert_eq!(track_position(&toc, 2), None);
        assert_eq!(track_position(&toc, 9), None);
    }
}
