use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::result::Result;

use clap::Parser;
use rend_core::{CddaStream, Device, Error, FRAME_SIZE, FRAMES_PER_SECOND, Track};

mod wav;
use wav::WavWriter;

#[derive(Parser)]
#[command(name = "rend", version, about = "Rip audio CDs from the command line")]
struct Cli {
    /// CD-ROM device to use (default: first device found).
    #[arg(short, long, global = true)]
    device: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// List CD-ROM devices.
    Drives,
    /// Show the table of contents of the disc.
    Toc,
    /// Rip audio tracks to WAV files.
    Rip {
        /// Directory to write WAV files to.
        #[arg(short, long, default_value = ".")]
        output_dir: PathBuf,
        /// Only rip the given track number (repeatable; default: all audio tracks).
        #[arg(short = 't', long = "track")]
        tracks: Vec<u8>,
        /// Overwrite existing files.
        #[arg(short, long)]
        force: bool,
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
        Command::Toc => cmd_toc(cli.device.as_deref()),
        Command::Rip {
            output_dir,
            tracks,
            force,
        } => cmd_rip(cli.device.as_deref(), &output_dir, &tracks, force),
        Command::Eject => cmd_eject(cli.device.as_deref()),
    }
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

fn cmd_rip(device: Option<&str>, output_dir: &Path, only: &[u8], force: bool) -> Result<(), Error> {
    let mut dev = open_device(device)?;
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

    std::fs::create_dir_all(output_dir)?;
    dev.spin_up().ok();

    let mut failures = 0usize;
    for track in &selected {
        let end = toc.end_lba(track.number).unwrap_or(toc.leadout_lba);
        let frames = track.frames(end);
        let path = output_dir.join(format!("track{:02}.wav", track.number));

        if path.exists() && !force {
            eprintln!(
                "rend: {} already exists (use --force to overwrite)",
                path.display()
            );
            failures += 1;
            continue;
        }

        match rip_track(&mut dev, track, frames, &path) {
            Ok(()) => eprintln!("track {:02}: wrote {}", track.number, path.display()),
            Err(e) => {
                std::fs::remove_file(&path).ok();
                eprintln!("track {:02}: {e}", track.number);
                failures += 1;
            }
        }
    }

    dev.spin_down().ok();

    if failures > 0 {
        return Err(Error::Unexpected(format!(
            "{failures} of {} track(s) failed",
            selected.len()
        )));
    }
    Ok(())
}

/// Rips a single track to a WAV file, reporting progress on stderr.
fn rip_track(dev: &mut Device, track: &Track, frames: u32, path: &Path) -> io::Result<()> {
    let mut stream = CddaStream::new(dev, track.start_lba, frames);
    let mut wav = WavWriter::create(path)?;
    let total = stream.total_bytes();
    let mut buf = vec![0u8; FRAMES_PER_SECOND as usize * FRAME_SIZE];
    let mut done = 0u64;

    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        wav.write(&buf[..n])?;
        done += n as u64;
        eprint!(
            "\rtrack {:02}: {:3}%  ",
            track.number,
            done * 100 / total as u64
        );
    }

    eprint!("\r");
    wav.finish()
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
}
