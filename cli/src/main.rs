use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::result::Result;
use std::thread;

use clap::Parser;
use rend_core::{CddaStream, Device, Error, FRAME_SIZE, FRAMES_PER_SECOND, Track};
use rend_encode::{Format, ffmpeg_available};

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
    },
    /// Eject the disc.
    Eject,
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
        Command::Rip {
            output_dir,
            format,
            tracks,
            force,
            all,
        } => cmd_rip(&cli.devices, all, &output_dir, format, &tracks, force),
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

fn cmd_rip(
    devices: &[String],
    all: bool,
    output_dir: &Path,
    format: Format,
    only: &[u8],
    force: bool,
) -> Result<(), Error> {
    if format.requires_ffmpeg() && !ffmpeg_available() {
        return Err(Error::Unexpected(
            "ffmpeg not found in PATH — install it for FLAC output, or rip with --format wav"
                .into(),
        ));
    }
    let resolved = rip_devices(devices, all)?;

    // One drive keeps the flat layout; several get a subdirectory each, named
    // after the device (e.g. `sr0`), so track files never collide.
    let multi = resolved.len() > 1;
    let jobs: Vec<(Device, PathBuf, String)> = resolved
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
            (dev, dir, prefix)
        })
        .collect();

    // One worker thread per drive; they all run at the same time.
    let mut handles = Vec::new();
    for (dev, dir, prefix) in jobs {
        let only = only.to_vec();
        let progress = !multi;
        let path = dev.path().to_string();
        let handle = thread::Builder::new()
            .name(format!("rend-rip-{path}"))
            .spawn(move || rip_device(dev, &dir, &only, format, force, &prefix, progress))
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
    format: Format,
    force: bool,
    prefix: &str,
    progress: bool,
) -> Result<(usize, usize), Error> {
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

    std::fs::create_dir_all(out_dir)?;
    dev.spin_up().ok();

    let mut failed = 0usize;
    for track in &selected {
        let end = toc.end_lba(track.number).unwrap_or(toc.leadout_lba);
        let frames = track.frames(end);
        let path = out_dir.join(format!("track{:02}.{}", track.number, format.extension()));

        if path.exists() && !force {
            eprintln!(
                "{prefix}{} already exists (use --force to overwrite)",
                path.display()
            );
            failed += 1;
            continue;
        }

        match rip_track(&mut dev, track, frames, &path, format, prefix, progress) {
            Ok(()) => eprintln!(
                "{prefix}track {:02}: wrote {}",
                track.number,
                path.display()
            ),
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
/// progress on stderr when `progress` is set (only safe for a single drive —
/// parallel drives would clobber each other's `\r` progress line).
fn rip_track(
    dev: &mut Device,
    track: &Track,
    frames: u32,
    path: &Path,
    format: Format,
    prefix: &str,
    progress: bool,
) -> io::Result<()> {
    let mut stream = CddaStream::new(dev, track.start_lba, frames);
    let mut file = format.create_file(path)?;
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

    #[test]
    fn single_device_rejects_multiple() {
        let one = ["/dev/sr0".to_string()];
        assert!(single_device(&one).is_ok());

        let two = ["/dev/sr0".to_string(), "/dev/sr1".to_string()];
        assert!(single_device(&two).is_err());
    }
}
