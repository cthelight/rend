//! Disc metadata for rend.
//!
//! Given the track layout of a disc, this crate figures out what the disc
//! is — [`disc_id`] computes the CDDB id, [`lookup_disc`] submits the
//! disc's layout to MusicBrainz (exact disc id first, then a fuzzy
//! duration match) while [`lookup_disc_all`] lists every candidate, and
//! [`cover_art`] fetches the release's front cover — keeps lookups of a
//! known disc in memory for the session in a [`MetaCache`], names the
//! output files after the result ([`Template`]), and writes it into the
//! encoded audio files with [`apply`].

pub mod cache;
pub mod discid;
pub mod lookup;
pub mod naming;
pub mod tag;
pub mod throttle;

pub use cache::MetaCache;
pub use discid::{disc_id, mb_discid};
pub use lookup::{
    Candidate, DiscMeta, DiscToc, TrackMeta, cover_art, lookup_candidates, lookup_disc,
    lookup_disc_all, register_disc_id_url,
};
pub use naming::{DEFAULT_TEMPLATE, ParseError, Template, sanitize};
pub use tag::{TrackTags, apply};
pub use throttle::Throttle;
