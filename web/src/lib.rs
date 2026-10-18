//! rend-wui: a small HTTP server that serves rend's ripper behind a
//! single-page frontend.
//!
//! The frontend is plain HTML/CSS/JavaScript embedded in the binary at
//! build time with [`rust-embed`]; progress is pushed to the browser over
//! a server-sent events stream. State and flows mirror the TUI: drives
//! (real devices discovered at startup, or the simulated demo disc),
//! TOC reading, MusicBrainz lookup with cached candidates, cover art,
//! and background ripping through the shared [`rend_rip::spawn`] worker.

pub mod api;
pub mod assets;
pub mod state;
pub mod view;

/// Which signal asked the process to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownCause {
    /// SIGTERM (or the platform equivalent).
    Terminate,
    /// Ctrl-C / SIGINT.
    Interrupt,
}

/// Waits for SIGTERM or Ctrl-C and reports which one arrived.
///
/// Each call registers its own signal handlers, so the server's graceful
/// shutdown and every open SSE stream can wait on it independently; they
/// all resolve when the signal arrives.
pub async fn shutdown_signal() -> ShutdownCause {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                return tokio::select! {
                    _ = sigterm.recv() => ShutdownCause::Terminate,
                    _ = tokio::signal::ctrl_c() => ShutdownCause::Interrupt,
                };
            }
            Err(error) => eprintln!("rend-wui: cannot catch SIGTERM ({error}); using Ctrl-C"),
        }
    }
    let _ = tokio::signal::ctrl_c().await;
    ShutdownCause::Interrupt
}
