//! rend-tui: an interactive TUI for rend.
//!
//! Browse CD-ROM drives, inspect the table of contents, and watch tracks
//! get ripped in real time. Supports the keyboard and the mouse
//! (click, double-click, and scroll wheel). With `--demo` it runs against
//! a simulated disc, so the behavior can be visualized without hardware.

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::event;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::event::{Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

mod app;
mod demo;
mod rip;
mod ui;

use app::App;

#[derive(Parser)]
#[command(
    name = "rend-tui",
    version,
    about = "Interactive TUI for rend: browse drives, inspect TOCs, and watch rips"
)]
struct Cli {
    /// CD-ROM device to use (default: first device found).
    #[arg(short, long)]
    device: Option<String>,

    /// Directory to write ripped WAV files to.
    #[arg(short, long, default_value = ".")]
    output_dir: PathBuf,

    /// Overwrite existing files.
    #[arg(short, long)]
    force: bool,

    /// Run against a simulated disc (no hardware required).
    #[arg(long)]
    demo: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rend-tui: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> io::Result<()> {
    let mut app = App::new(
        cli.device.as_deref(),
        cli.output_dir.clone(),
        cli.force,
        cli.demo,
    );
    let mut terminal = setup_terminal()?;
    let result = event_loop(&mut terminal, &mut app);
    app.shutdown();
    let teardown = tear_down(&mut terminal);
    match (result, teardown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(e), _) => Err(e),
        (Ok(()), Err(e)) => Err(e),
    }
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    terminal.hide_cursor()?;
    Ok(terminal)
}

fn tear_down(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    terminal.show_cursor()?;
    let stdout = terminal.backend_mut();
    execute!(stdout, LeaveAlternateScreen, DisableMouseCapture)?;
    disable_raw_mode()?;
    Ok(())
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> io::Result<()> {
    let mut status_poll = Instant::now();
    while app.running {
        app.drain_rip_events();
        if status_poll.elapsed() >= Duration::from_millis(1000) {
            status_poll = Instant::now();
            app.refresh_drives();
        }
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key),
                Event::Mouse(mouse) => app.handle_mouse(mouse),
                _ => {}
            }
        }
        terminal.draw(|f| ui::draw(f, app))?;
    }
    Ok(())
}
