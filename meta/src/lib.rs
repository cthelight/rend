//! Disc metadata for rend.
//!
//! Given the track layout of a disc, this crate figures out what the disc
//! is — [`disc_id`] computes the CDDB id, [`lookup_disc`] submits the
//! disc's layout to MusicBrainz (exact disc id first, then a fuzzy
//! duration match), and [`cover_art`] fetches the release's front cover —
//! and writes the result into the encoded audio files with [`apply`].

pub mod discid;
pub mod lookup;
pub mod tag;

pub use discid::{disc_id, mb_discid};
pub use lookup::{DiscMeta, DiscToc, TrackMeta, cover_art, lookup_disc};
pub use tag::{TrackTags, apply};
