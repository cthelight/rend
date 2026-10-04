//! `rend-web`: serve the rend web UI and rip through the shared worker.

use std::path::PathBuf;

use clap::Parser;
use rend_encode::Format;
use rend_meta::{DEFAULT_TEMPLATE, Template};
use rend_web::api::router;
use rend_web::state::AppState;
use tokio::net::TcpListener;

#[derive(Parser)]
#[command(name = "rend-web", about = "Serve the rend web UI")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    #[arg(long, default_value_t = 3000)]
    port: u16,

    /// Only rip this device.
    #[arg(short = 'd', long)]
    device: Option<String>,

    /// Where to write the ripped files.
    #[arg(short = 'o', long, default_value = ".")]
    output_dir: PathBuf,

    /// Output format: flac or wav.
    #[arg(short = 'F', long, default_value = "flac", value_parser = Format::parse)]
    format: Format,

    /// Filename template, e.g. `<artist>/<album>/<number> <title>`.
    #[arg(short = 'T', long, default_value = DEFAULT_TEMPLATE)]
    template: String,

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
    let state = AppState::new(
        cli.output_dir,
        cli.device.as_deref(),
        cli.format,
        cli.force,
        template,
        cli.demo,
    )?;

    // Bind before starting the background thread so a busy port fails fast.
    let listener = TcpListener::bind((cli.host.as_str(), cli.port)).await?;
    state.start();
    println!("rend-web listening on http://{}:{}/", cli.host, cli.port);

    let app = router(state.clone());
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    state.shutdown();
    Ok(())
}

/// Waits for SIGTERM or Ctrl-C so the process can shut down cleanly.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = sigterm.recv() => println!("rend-web: received SIGTERM"),
                    _ = tokio::signal::ctrl_c() => println!("rend-web: interrupted"),
                }
                return;
            }
            Err(error) => eprintln!("rend-web: cannot catch SIGTERM ({error}); using Ctrl-C"),
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
