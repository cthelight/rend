//! rend-rip: the rip worker shared by rend's frontends.
//!
//! Given a source of CDDA frames (a real drive, or the simulated disc)
//! and a set of tracks, a background worker thread reads the audio,
//! encodes each track, embeds the looked-up metadata and cover art, and
//! reports progress as [`RipEvent`]s over an mpsc channel. Frontends
//! (the CLI, the TUI, the web UI) drive it with [`spawn`] and translate
//! the events into their own progress displays.

pub mod demo;
mod worker;

pub use worker::{RipEvent, RipJob, RipSource, output_paths, spawn};
