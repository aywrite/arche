// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The table a remembered leaf term is kept in.
//!
//! One cache type, not one table: a term names its key, its fold and its
//! width, and [`super::Caches`] holds an instance of this per term.

/// One remembered score, under the whole key rather than a tag, since a
/// wrong hit would be a silently wrong evaluation. The whole key costs sixteen
/// bytes an entry against eight once the alignment is paid.
#[derive(Copy, Clone)]
struct Entry {
    key: u64,
    packed: i32,
}

/// What one term has already worked out, `1 << BITS` scores of it.
///
/// Direct mapped and never cleared. An entry is only ever read against the
/// key that wrote it, so a stale one is a miss rather than a wrong answer.
/// The cache changes how a score is arrived at and not what it is, so the
/// node counts are the ones the weights alone produce.
///
/// Owned by the searcher rather than by the board, because it is scratch and
/// not position.
pub(super) struct Cache<const BITS: usize> {
    entries: Box<[Entry]>,
}

impl<const BITS: usize> Default for Cache<BITS> {
    fn default() -> Self {
        // an empty entry is key zero holding zero. A pawnless board's pawn
        // key is zero and its pawn structure scores zero, so it reads the
        // entry correctly; any other key of zero is the same sixty four bit
        // coincidence a wrong hit needs anywhere else in the table
        Cache {
            entries: vec![Entry { key: 0, packed: 0 }; Self::SLOTS].into_boxed_slice(),
        }
    }
}

impl<const BITS: usize> Cache<BITS> {
    /// How many scores the table holds. A power of two, so the index is a
    /// mask rather than a remainder.
    pub(super) const SLOTS: usize = 1 << BITS;

    /// What `key` stands for, read back where the table holds it and folded
    /// where it does not.
    #[inline]
    pub(super) fn get(&mut self, key: u64, fold: impl FnOnce() -> i32) -> i32 {
        let slot = (key as usize) & (Self::SLOTS - 1);
        let entry = &mut self.entries[slot];
        if entry.key == key {
            return entry.packed;
        }
        let packed = fold();
        *entry = Entry { key, packed };
        packed
    }

    /// What the table holds under `key`, and nothing where the slot stands
    /// for another key. For the tests in [`super`], which read the tables
    /// rather than the score.
    #[cfg(test)]
    pub(super) fn stored(&self, key: u64) -> Option<i32> {
        let entry = &self.entries[(key as usize) & (Self::SLOTS - 1)];
        (entry.key == key).then_some(entry.packed)
    }
}

#[cfg(test)]
mod tests {
    use super::Cache;
    use pretty_assertions::assert_eq;

    /// A miss folds and stores, a hit reads back, and a key landing on a
    /// taken slot folds and writes the entry over. What the cache answers
    /// does not say which of those happened, so the fold counts its calls.
    ///
    /// Sixteen slots, so the two keys below share one: the entry keeps the
    /// whole of the key that wrote it, and a table that kept the index bits
    /// alone would hand the second key the first one's score.
    ///
    /// A fresh table is key zero holding zero, which is asserted rather than
    /// assumed: the pawn structure reads that entry as its own for a board
    /// with no pawns, whose key is zero and whose counts are zero.
    #[test]
    fn a_score_is_folded_once_and_read_back_under_the_key_that_wrote_it() {
        const BITS: usize = 4;
        let mut cache: Cache<BITS> = Cache::default();
        assert_eq!(cache.entries.len(), Cache::<BITS>::SLOTS);
        let key = 0x0123_4567_89ab_cdefu64;
        let shares = key ^ (1 << 63);
        let slot = (key as usize) & (Cache::<BITS>::SLOTS - 1);
        assert_eq!((shares as usize) & (Cache::<BITS>::SLOTS - 1), slot);

        assert_eq!(cache.entries[slot].key, 0, "the slot starts empty");
        assert_eq!(cache.entries[slot].packed, 0, "holding nothing");
        assert_eq!(cache.stored(key), None);
        assert_eq!(cache.stored(0), Some(0), "and a key of zero reads it");

        let mut folded = 0;
        assert_eq!(
            cache.get(key, || {
                folded += 1;
                11
            }),
            11
        );
        assert_eq!(folded, 1);
        assert_eq!(cache.entries[slot].key, key);

        assert_eq!(
            cache.get(key, || {
                folded += 1;
                22
            }),
            11
        );
        assert_eq!(folded, 1);

        assert_eq!(
            cache.get(shares, || {
                folded += 1;
                33
            }),
            33
        );
        assert_eq!(folded, 2);
        assert_eq!(cache.entries[slot].key, shares);

        // and the key that was written over is a miss rather than a hit on
        // the score standing in its slot
        assert_eq!(
            cache.get(key, || {
                folded += 1;
                44
            }),
            44
        );
        assert_eq!(folded, 3);
    }
}
