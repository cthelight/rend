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
