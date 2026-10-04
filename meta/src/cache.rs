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
//!
//! Each table is bounded. When it is full, the oldest entry is evicted to
//! make room; re-recording an existing key refreshes it, moving it to the
//! front. A long-running session therefore never grows without limit, and
//! the discs a user keeps coming back to stay cached.

use std::collections::{HashMap, VecDeque};

use crate::lookup::{Candidate, DiscToc};

/// How many distinct disc layouts may be remembered.
const MAX_CANDIDATES: usize = 256;

/// How many distinct covers may be remembered.
const MAX_COVERS: usize = 64;

/// Lookups and covers this session has already fetched.
#[derive(Debug, Default)]
pub struct MetaCache {
    candidates: HashMap<DiscToc, Vec<Candidate>>,
    /// Insertion order of `candidates`, oldest first, for eviction.
    candidate_order: VecDeque<DiscToc>,
    covers: HashMap<String, Option<Vec<u8>>>,
    /// Insertion order of `covers`, oldest first, for eviction.
    cover_order: VecDeque<String>,
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
    /// recorded too: "no match" needs no refetch. Re-recording a known
    /// disc refreshes it; when the table is full the oldest disc is
    /// evicted to make room.
    pub fn insert(&mut self, toc: DiscToc, candidates: Vec<Candidate>) {
        if self.candidates.contains_key(&toc) {
            self.candidate_order.retain(|t| t != &toc);
        } else if self.candidate_order.len() >= MAX_CANDIDATES {
            if let Some(oldest) = self.candidate_order.pop_front() {
                self.candidates.remove(&oldest);
            }
        }
        self.candidate_order.push_back(toc.clone());
        self.candidates.insert(toc, candidates);
    }

    /// The cover already fetched for `release_id`, if any — `Some(None)`
    /// when the release was found to have no cover.
    pub fn cover(&self, release_id: &str) -> Option<&Option<Vec<u8>>> {
        self.covers.get(release_id)
    }

    /// Records the cover for `release_id`, `None` included: a release
    /// without a cover needs no refetch either. Re-recording a known
    /// release refreshes it; when the table is full the oldest release is
    /// evicted to make room.
    pub fn cover_insert(&mut self, release_id: String, cover: Option<Vec<u8>>) {
        if self.covers.contains_key(&release_id) {
            self.cover_order.retain(|id| id != &release_id);
        } else if self.cover_order.len() >= MAX_COVERS {
            if let Some(oldest) = self.cover_order.pop_front() {
                self.covers.remove(&oldest);
            }
        }
        self.cover_order.push_back(release_id.clone());
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
                release_artist_id: None,
                disc_number: None,
                disc_count: None,
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

    fn toc_at(i: u32) -> DiscToc {
        DiscToc {
            offsets: vec![150, 1650, i],
            leadout: 3300 + i,
        }
    }

    #[test]
    fn the_oldest_disc_is_evicted_when_the_cache_is_full() {
        let mut cache = MetaCache::new();
        for i in 0..MAX_CANDIDATES as u32 {
            cache.insert(toc_at(i), vec![candidate("rel-1")]);
        }
        assert_eq!(cache.len(), MAX_CANDIDATES);
        assert!(cache.candidates(&toc_at(0)).is_some());

        // One more disc pushes the oldest (index 0) out.
        cache.insert(toc_at(999), vec![candidate("rel-1")]);
        assert_eq!(cache.len(), MAX_CANDIDATES);
        assert!(cache.candidates(&toc_at(0)).is_none());
        assert!(cache.candidates(&toc_at(1)).is_some());
        assert!(cache.candidates(&toc_at(999)).is_some());
    }

    #[test]
    fn a_rerecorded_disc_moves_to_the_front() {
        let mut cache = MetaCache::new();
        for i in 0..MAX_CANDIDATES as u32 {
            cache.insert(toc_at(i), vec![candidate("rel-1")]);
        }

        // Touching disc 0 makes it the most recent, so evicting now drops
        // disc 1 instead.
        cache.insert(toc_at(0), vec![candidate("rel-2")]);
        cache.insert(toc_at(999), vec![candidate("rel-1")]);

        assert!(cache.candidates(&toc_at(0)).is_some());
        assert!(cache.candidates(&toc_at(1)).is_none());
        assert_eq!(
            cache.candidates(&toc_at(0)).unwrap()[0].meta.release_id,
            "rel-2"
        );
    }

    #[test]
    fn the_oldest_cover_is_evicted_when_the_cache_is_full() {
        let mut cache = MetaCache::new();
        for i in 0..MAX_COVERS {
            cache.cover_insert(format!("rel-{i}"), Some(vec![i as u8]));
        }
        assert_eq!(cache.cover("rel-0"), Some(&Some(vec![0u8])));

        // One more release pushes the oldest (rel-0) out.
        cache.cover_insert("rel-new".into(), Some(vec![255]));
        assert!(cache.cover("rel-0").is_none());
        assert_eq!(cache.cover("rel-1"), Some(&Some(vec![1u8])));
        assert_eq!(cache.cover("rel-new"), Some(&Some(vec![255u8])));
    }
}
