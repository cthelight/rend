use std::process::ExitCode;
use std::result::Result;

use clap::Parser;
use rend_core::{Device, Error, FRAMES_PER_SECOND};

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
