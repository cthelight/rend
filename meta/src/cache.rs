//! A per-session cache for looked-up disc metadata and cover art.
//!
//! A disc is keyed by its exact layout ([`DiscToc`]) — the same input the
//! lookup is built from — so a second selection of the same drive in the
//! TUI, or a re-rip in the CLI, is answered from memory instead of the
//! network. Cover art is keyed by the release id that [`cover_art`]
//! takes. Only successful lookups are stored: a network failure is not
//! cached, so a later retry can still reach the network. An empty
//! candidate list is cached — "no release matched" is a definitive
//! answer, not a failure.

use std::collections::HashMap;

use crate::lookup::{Candidate, DiscToc};

/// Lookups and covers this session has already fetched.
#[derive(Debug, Default)]
pub struct MetaCache {
    candidates: HashMap<DiscToc, Vec<Candidate>>,
    covers: HashMap<String, Option<Vec<u8>>>,
}

impl MetaCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The candidates already looked up for `toc`, best first, if any.
    pub fn candidates(&self, toc: &DiscToc) -> Option<&[Candidate]> {
        self.candidates.get(toc).map(|c| c.as_slice())
    }

    /// Records the candidates for `toc`, best first. An empty list is
    /// recorded too: "no match" needs no refetch.
    pub fn insert(&mut self, toc: DiscToc, candidates: Vec<Candidate>) {
        self.candidates.insert(toc, candidates);
    }

    /// The cover already fetched for `release_id`, if any — `Some(None)`
    /// when the release was found to have no cover.
    pub fn cover(&self, release_id: &str) -> Option<&Option<Vec<u8>>> {
        self.covers.get(release_id)
    }

    /// Records the cover for `release_id`, `None` included: a release
    /// without a cover needs no refetch either.
    pub fn cover_insert(&mut self, release_id: String, cover: Option<Vec<u8>>) {
        self.covers.insert(release_id, cover);
    }

    /// How many discs have been looked up.
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    /// Whether no disc has been looked up yet.
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::DiscMeta;

    fn toc() -> DiscToc {
        DiscToc {
            offsets: vec![150, 1650],
            leadout: 3300,
        }
    }

    fn candidate(release_id: &str) -> Candidate {
        Candidate {
            meta: DiscMeta {
                album: "The Album".into(),
                artist: "The Band".into(),
                album_artist: None,
                year: None,
                release_id: release_id.into(),
                tracks: vec![],
            },
            max_diff_ms: 0,
        }
    }

    #[test]
    fn candidates_are_keyed_by_the_whole_toc() {
        let mut cache = MetaCache::new();
        assert!(cache.is_empty());
        assert!(cache.candidates(&toc()).is_none());

        cache.insert(toc(), vec![candidate("rel-1")]);
        let found = cache.candidates(&toc()).unwrap();
        assert_eq!(found[0].meta.release_id, "rel-1");
        assert_eq!(cache.len(), 1);

        // A different layout is a different disc, even with a shared
        // prefix.
        let other = DiscToc {
            offsets: vec![150, 1650, 2450],
            leadout: 3300,
        };
        assert!(cache.candidates(&other).is_none());
    }

    #[test]
    fn a_cached_miss_stays_a_hit() {
        let mut cache = MetaCache::new();
        cache.insert(toc(), vec![]);
        assert!(cache.candidates(&toc()).unwrap().is_empty());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn a_later_lookup_replaces_the_earlier_one() {
        let mut cache = MetaCache::new();
        cache.insert(toc(), vec![candidate("rel-1")]);
        cache.insert(toc(), vec![candidate("rel-2"), candidate("rel-1")]);
        let found = cache.candidates(&toc()).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].meta.release_id, "rel-2");
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn covers_are_keyed_by_release_id() {
        let mut cache = MetaCache::new();
        assert!(cache.cover("rel-1").is_none());

        cache.cover_insert("rel-1".into(), Some(vec![1, 2, 3]));
        assert_eq!(cache.cover("rel-1"), Some(&Some(vec![1u8, 2, 3])));

        // A release without a cover is remembered as such.
        cache.cover_insert("rel-2".into(), None);
        assert_eq!(cache.cover("rel-2"), Some(&None));

        assert!(cache.cover("rel-3").is_none());
    }
}
