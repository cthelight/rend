//! `rend-wui`: serve the rend web UI and rip through the shared worker.

use std::path::PathBuf;

use clap::Parser;
use rend_encode::{Format, FormatSet};
use rend_meta::{DEFAULT_TEMPLATE, Template};
use rend_wui::api::router;
use rend_wui::state::AppState;
use rend_wui::{ShutdownCause, shutdown_signal};
use tokio::net::TcpListener;

#[derive(Parser)]
#[command(name = "rend-wui", about = "Serve the rend web UI")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    #[arg(long, default_value_t = 3000)]
    port: u16,

    /// Only rip this device.
    #[arg(short = 'd', long)]
    device: Option<String>,

    /// Where to write the ripped files.
    #[arg(short = 'o', long, default_value = ".", env = "REND_WUI_OUTPUT_DIR")]
    output_dir: PathBuf,

    /// Output format. Without it, the best installed default is used.
    #[arg(short = 'F', long, value_parser = Format::parse)]
    format: Option<Format>,

    /// Output quality, e.g. `256`, `256k`, or `q6`.
    #[arg(short = 'q', long)]
    quality: Option<String>,

    /// Filename template, e.g. `<artist>/<album>/<number> <title>`.
    #[arg(short = 'T', long, default_value = DEFAULT_TEMPLATE, env = "REND_WUI_TEMPLATE")]
    template: String,

    /// Rip every drive into its own subdirectory of the output dir.
    ///
    /// Takes an optional `true` or `false` (bare `--drive-dirs` means on).
    /// Without it, drives get their own subdirectory only when several
    /// rip at once.
    #[arg(long, num_args = 0..=1, default_missing_value = "true", env = "REND_WUI_DRIVE_DIRS")]
    drive_dirs: Option<bool>,

    /// Overwrite existing output files.
    #[arg(short = 'f', long)]
    force: bool,

    /// Use the built-in simulated disc instead of real devices.
    #[arg(long)]
    demo: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let template = Template::parse(&cli.template)?;
    let formats = FormatSet::probe();
    let format = formats.resolve(cli.format, cli.quality.as_deref())?;
    let state = AppState::new(
        cli.output_dir,
        cli.device.as_deref(),
        formats,
        format,
        cli.force,
        template,
        cli.drive_dirs,
        cli.demo,
    )?;

    // Bind before starting the background thread so a busy port fails fast.
    let listener = TcpListener::bind((cli.host.as_str(), cli.port)).await?;
    state.start();
    println!("rend-wui listening on http://{}:{}/", cli.host, cli.port);

    let app = router(state.clone());
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            match shutdown_signal().await {
                ShutdownCause::Terminate => println!("rend-wui: received SIGTERM"),
                ShutdownCause::Interrupt => println!("rend-wui: interrupted"),
            }
        })
        .await?;

    state.shutdown();
    Ok(())
}
