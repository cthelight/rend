//! Disc metadata for rend.
//!
//! Given the track layout of a disc, this crate figures out what the disc
//! is — [`disc_id`] computes the CDDB id, [`lookup_disc`] submits it to
//! MusicBrainz, and [`cover_art`] fetches the release's front cover — and
//! writes the result into the encoded audio files with [`apply`].

pub mod discid;
pub mod lookup;
pub mod tag;

pub use discid::disc_id;
pub use lookup::{DiscMeta, TrackMeta, cover_art, lookup_disc};
pub use tag::{TrackTags, apply};
