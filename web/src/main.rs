//! `rend-wui`: serve the rend web UI and rip through the shared worker.

use std::path::PathBuf;

use clap::Parser;
use rend_core::{DriveNames, DriveOrder};
use rend_encode::FormatSet;
use rend_meta::{DEFAULT_TEMPLATE, Template};
use rend_wui::api::router;
use rend_wui::state::AppState;
use rend_wui::{ShutdownCause, shutdown_signal};
use tokio::net::TcpListener;

#[derive(Parser)]
#[command(name = "rend-wui", about = "Serve the rend web UI")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1", env = "REND_WUI_HOST")]
    host: String,

    #[arg(long, default_value_t = 3000, env = "REND_WUI_PORT")]
    port: u16,

    /// Only rip this device.
    #[arg(short = 'd', long)]
    device: Option<String>,

    /// Where to write the ripped files.
    #[arg(short = 'o', long, default_value = ".", env = "REND_WUI_OUTPUT_DIR")]
    output_dir: PathBuf,

    /// Output format, repeatable. Each value may be `format` or `format:quality`.
    /// Without it, `REND_WUI_FORMATS` or the best installed default is used.
    #[arg(short = 'F', long)]
    formats: Vec<String>,

    /// Output quality for selected formats that do not include an explicit quality.
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
    #[arg(short, long)]
    force: bool,

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

    /// The order to list the drives in (repeatable; `sr0` or `/dev/sr0`):
    /// the listed drives first, in the given order, the rest after them.
    /// Also `REND_DRIVE_ORDER`, comma-separated.
    #[arg(
        long = "drive-order",
        value_name = "DRIVE",
        value_delimiter = ',',
        env = "REND_DRIVE_ORDER"
    )]
    drive_order: Vec<String>,

    /// Use the built-in simulated disc instead of real devices.
    #[arg(long)]
    demo: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let template = Template::parse(&cli.template)?;
    let formats = FormatSet::probe();
    let selected_formats = if !cli.formats.is_empty() {
        formats.resolve_many(&cli.formats, cli.quality.as_deref())?
    } else if let Ok(raw) = std::env::var("REND_WUI_FORMATS") {
        let selections: Vec<String> = raw
            .split(',')
            .map(|part| part.trim().to_string())
            .filter(|part| !part.is_empty())
            .collect();
        if selections.is_empty() {
            vec![formats.default_format()]
        } else {
            match formats.resolve_many(&selections, None) {
                Ok(selected) => selected,
                Err(message) => {
                    eprintln!("rend-wui: ignoring REND_WUI_FORMATS: {message}");
                    vec![formats.default_format()]
                }
            }
        }
    } else {
        vec![formats.default_format()]
    };
    let drive_names = DriveNames::from_pairs(&cli.drive_names)?;
    let drive_order = DriveOrder::from_parts(&cli.drive_order);
    let state = AppState::new(
        cli.output_dir,
        cli.device.as_deref(),
        formats,
        selected_formats,
        cli.force,
        template,
        drive_names,
        drive_order,
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
