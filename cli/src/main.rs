use clap::Parser;

#[derive(Parser)]
#[command(name = "rend", version, about = "Rip audio CDs from the command line")]
struct Cli {
    /// CD-ROM device to use (default: first device found).
    #[arg(short, long, global = true)]
    device: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// List CD-ROM devices.
    Drives,
    /// Show the table of contents of the disc.
    Toc,
    /// Rip audio tracks to WAV files.
    Rip,
    /// Eject the disc.
    Eject,
}

fn main() {
    let _cli = Cli::parse();
    // Implementation in later commits.
}
