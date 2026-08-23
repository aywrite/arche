//! A cache of already-searched positions, used to order moves and to skip
//! re-searching interior nodes. An accelerator: deleting it may slow the
//! search but never changes the answer.

use crate::engine::CHECKMATE_THRESHOLD;
use crate::misc::Score;
use crate::play::Play;
use std::mem;

pub const DEFAULT_TABLE_BYTES: usize = 256 * 1024 * 1024;

/// Convert a score to its transposition table form. Mate scores are stored
/// relative to the node they are stored at (plies-to-mate from this node)
/// rather than relative to the root of the search, so that they remain correct
/// when the entry is reused at a different distance from the root.
pub fn score_to_tt(score: Score, line_ply: usize) -> Score {
    if score > CHECKMATE_THRESHOLD {
        score + line_ply as Score
    } else if score < -CHECKMATE_THRESHOLD {
        score - line_ply as Score
    } else {
        score
    }
}

/// The inverse of score_to_tt: convert a stored mate score back to being
/// relative to the root of the current search.
pub fn score_from_tt(score: Score, line_ply: usize) -> Score {
    if score > CHECKMATE_THRESHOLD {
        score - line_ply as Score
    } else if score < -CHECKMATE_THRESHOLD {
        score + line_ply as Score
    } else {
        score
    }
}

/// How often the transposition table hands back a score that depended on the
/// path taken rather than on the position, which is the graph history
/// interaction error every engine carries and none of them measure.
#[derive(Copy, Clone, Debug, Default)]
pub struct GhiCounters {
    /// Entries stored carrying a draw tainted score.
    pub tainted_stores: u64,
    /// Entries stored in total.
    pub stores: u64,
    /// Probes that returned a score, cutting the search off.
    pub score_cutoffs: u64,
    /// Probes that returned a tainted score, which is the error itself: the
    /// stored draw was reachable by the path that stored it and may not be
    /// reachable by this one.
    pub tainted_score_cutoffs: u64,
}

/// What the search stores about a position and reads back: the entry as the
/// search sees it. The table packs it into an `Entry` of its own.
#[derive(Copy, Clone, Debug)]
pub struct Pv {
    pub play: Play,
    pub score: Score,
    /// True if the score flowed from a repetition or fifty move draw somewhere
    /// below it, so it describes the path taken to this position and not the
    /// position itself. See docs on graph history interaction.
    pub tainted: bool,
    pub depth: u8,
    pub bound: Bound,
}

/// What the stored score means: the searched window decides whether a score
/// is the truth, a ceiling or a floor, and a reader can only use it for a
/// cutoff its kind allows. Two bits of an entry, which the values are.
#[derive(Copy, Clone, Debug)]
#[repr(u8)]
pub enum Bound {
    Exact = 0,
    // fail low nodes are not stored yet, see the known issues in the readme
    Upper = 1,
    Lower = 2,
    Ordering = 3,
}

impl Bound {
    fn from_bits(bits: u8) -> Self {
        match bits & 0b11 {
            0 => Bound::Exact,
            1 => Bound::Upper,
            2 => Bound::Lower,
            _ => Bound::Ordering,
        }
    }
}

/// A slot in the table: sixteen bytes, four to a cache line.
///
/// The key is kept only in part. The index is drawn from the top of the key
/// by multiply-shift, so the bottom of it is what carries anything the index
/// does not already say, and thirty two bits of that leaves one chance in
/// four thousand million per entry compared of taking another position's
/// entry for this one: a probe compares with the four of its bucket, so
/// one probe in about a thousand million. The whole key cost eight bytes
/// a slot and made the slot twenty four.
///
/// The flags byte holds the bound in its low two bits, the taint in the
/// third and the search the entry was stored in, the generation, in the top
/// five; a generation of zero is a slot never written, which is what a
/// cleared table is full of. Two bytes are set aside for the static
/// evaluation, which correction history will want stored beside the score;
/// reserving them now means the layout, and with it every node count,
/// changes once rather than twice.
#[derive(Copy, Clone, Debug)]
#[repr(C)]
struct Entry {
    key: u32,
    play: Play,
    score: Score,
    depth: u8,
    flags: u8,
    #[allow(dead_code)]
    static_eval: i16,
}

// the bench's node counts depend on how many slots a table of a given size
// holds, so a compiler that laid the entry out differently would move every
// one of them: this says why, the moment it happens
const _: () = assert!(mem::size_of::<Entry>() == 16);

/// An entry stored this many searches ago or more is replaced whatever its
/// depth: its score describes repetition and fifty move context the game
/// has moved past, and a slot is worth more to the search under way. The
/// window the ply rule gave before, MAX_DEPTH plus three plies with a side
/// searching every other ply, came to about the same.
const STALE_AFTER_SEARCHES: u8 = 12;

/// The generations run from one to this and round again; zero is never one,
/// so an entry of generation zero reads as empty. Ages are taken modulo
/// this, so an entry from thirty one searches ago or more reads as recent
/// again and holds its slot by depth until it is twelve searches old by
/// that reckoning, or is hit. A hit is keyed, so nothing wrong is read; a
/// slot is held a while longer than it should be, once in forty moves.
const GENERATIONS: u8 = 31;

impl Entry {
    const EMPTY: Entry = Entry {
        key: 0,
        play: Play {
            from: 0,
            to: 0,
            capture: None,
            promote: None,
            en_passant: false,
            castle: false,
        },
        score: 0,
        depth: 0,
        flags: 0,
        static_eval: 0,
    };

    #[inline]
    fn slice(key: u64) -> u32 {
        key as u32
    }

    #[inline]
    fn pack(key: u64, pv: Pv, generation: u8) -> Entry {
        Entry {
            key: Entry::slice(key),
            play: pv.play,
            score: pv.score,
            depth: pv.depth,
            flags: (pv.bound as u8) | (u8::from(pv.tainted) << 2) | (generation << 3),
            static_eval: 0,
        }
    }

    #[inline]
    fn unpack(self) -> Pv {
        Pv {
            play: self.play,
            score: self.score,
            depth: self.depth,
            bound: Bound::from_bits(self.flags),
            tainted: self.flags & 0b100 != 0,
        }
    }

    #[inline]
    fn generation(self) -> u8 {
        self.flags >> 3
    }

    #[inline]
    fn bound(self) -> Bound {
        Bound::from_bits(self.flags)
    }
}

/// Four entries in one cache line. A position is looked for in every entry
/// of its bucket and stored in whichever of them is worth the least, so a
/// table keeps four positions that hash alike where a slot kept one, and a
/// probe still touches one line.
#[derive(Copy, Clone, Debug)]
#[repr(C, align(64))]
struct Bucket {
    entries: [Entry; 4],
}

const BUCKET: usize = 4;

impl Bucket {
    const EMPTY: Bucket = Bucket {
        entries: [Entry::EMPTY; BUCKET],
    };
}

#[derive(Debug)]
pub struct TranspositionTable {
    table: Vec<Bucket>,
    /// The search under way, as the entries it stores are marked.
    generation: u8,
}

impl TranspositionTable {
    /// A table of at least this many entries, rounded up to whole buckets.
    fn with_capacity(capacity: usize) -> Self {
        Self {
            table: vec![Bucket::EMPTY; capacity.div_ceil(BUCKET).max(1)],
            generation: 1,
        }
    }

    pub fn clear(&mut self) {
        self.table.fill(Bucket::EMPTY);
        self.generation = 1;
    }

    pub fn with_capacity_bytes(bytes: usize) -> Self {
        Self::with_capacity(bytes / mem::size_of::<Entry>())
    }

    /// A search is beginning: what it stores is marked as its own, and what
    /// earlier searches stored ages by one.
    pub fn new_search(&mut self) {
        self.generation = self.generation % GENERATIONS + 1;
    }

    /// How many searches ago an entry was stored.
    #[inline]
    fn age(&self, entry: Entry) -> u8 {
        (self.generation + GENERATIONS - entry.generation()) % GENERATIONS
    }

    #[inline]
    fn index_for(&self, key: u64) -> usize {
        // multiply-shift: maps key uniformly onto 0..len without a 64 bit
        // division on every probe
        (((key as u128) * (self.table.len() as u128)) >> 64) as usize
    }

    pub fn get(&self, key: u64) -> Option<Pv> {
        let slice = Entry::slice(key);
        self.table[self.index_for(key)]
            .entries
            .iter()
            // the key first: in a warm table nearly every entry has a
            // generation, and the key is what rejects three of the four
            .find(|entry| entry.key == slice && entry.generation() != 0)
            .map(|entry| entry.unpack())
    }

    /// Where in its bucket a position goes: its own entry if it has one,
    /// else an empty one, else one from a search long enough ago to be
    /// stale, else the shallowest. Which of the four comes back is the whole
    /// of the replacement policy, and the depth contest in `set` is the
    /// rest.
    #[inline]
    fn slot_for(&self, key: u64) -> (usize, usize) {
        let index = self.index_for(key);
        let slice = Entry::slice(key);
        let bucket = &self.table[index].entries;
        // its own entry first, wherever it sits, so a position is never in
        // a bucket twice
        if let Some(i) = bucket
            .iter()
            .position(|entry| entry.key == slice && entry.generation() != 0)
        {
            return (index, i);
        }
        let mut victim = 0;
        for (i, entry) in bucket.iter().enumerate() {
            if entry.generation() == 0 || self.age(*entry) >= STALE_AFTER_SEARCHES {
                return (index, i);
            }
            if entry.depth < bucket[victim].depth {
                victim = i;
            }
        }
        (index, victim)
    }

    pub fn set(&mut self, key: u64, pv: Pv) {
        let (index, i) = self.slot_for(key);
        let old = self.table[index].entries[i];
        if old.generation() != 0 && self.age(old) < STALE_AFTER_SEARCHES {
            if pv.depth < old.depth {
                return;
            }
            if pv.depth == old.depth
                && old.key == Entry::slice(key)
                && matches!(old.bound(), Bound::Exact)
                && !matches!(pv.bound, Bound::Exact)
            {
                return;
            }
        }
        self.table[index].entries[i] = Entry::pack(key, pv, self.generation);
    }

    /// Store without the depth contest above. The root's end-of-iteration
    /// entry is for this: it names the move the search is about to answer
    /// with, and the reported line is read back from its slot, so an entry a
    /// deeper search left there earlier in the game must not outrank it.
    /// When one did, the engine answered one move while its line opened with
    /// another, which is a lie the match tools flag. The leftover's extra
    /// depth is no loss: its score describes repetition and fifty move
    /// context the game has since moved past.
    pub fn set_always(&mut self, key: u64, pv: Pv) {
        let (index, i) = self.slot_for(key);
        self.table[index].entries[i] = Entry::pack(key, pv, self.generation);
    }
}

#[cfg(test)]
mod tests {
    use super::{Bound, Bucket, Entry, Play, Pv, STALE_AFTER_SEARCHES, TranspositionTable};
    use crate::engine::MAX_DEPTH;
    use crate::misc::{Piece, PromotePiece};
    use pretty_assertions::assert_eq;
    use std::mem;

    fn new_pv(bound: Bound, depth: u8) -> Pv {
        Pv {
            play: Play::new(0, 1, None, None, false, false),
            score: 0,
            depth,
            bound,
            tainted: false,
        }
    }

    #[test]
    fn an_entry_is_sixteen_bytes() {
        // four to a cache line, and the bench's node counts depend on how
        // many a table of a given size holds
        assert_eq!(mem::size_of::<Entry>(), 16);
    }

    #[test]
    fn a_bucket_is_one_cache_line() {
        // a probe reads the line once and sees all four entries in it
        assert_eq!(mem::size_of::<Bucket>(), 64);
        assert_eq!(mem::align_of::<Bucket>(), 64);
    }

    #[test]
    fn four_positions_share_a_bucket_and_all_are_kept() {
        let mut table = TranspositionTable::with_capacity(4);
        for key in 1..=4 {
            table.set(key, new_pv(Bound::Exact, key as u8));
        }
        for key in 1..=4 {
            assert_eq!(table.get(key).unwrap().depth, key as u8, "key {key}");
        }
    }

    #[test]
    fn a_fifth_position_evicts_the_shallowest_of_the_bucket() {
        let mut table = TranspositionTable::with_capacity(4);
        for (key, depth) in [(1, 8), (2, 3), (3, 5), (4, 7)] {
            table.set(key, new_pv(Bound::Exact, depth));
        }
        table.set(5, new_pv(Bound::Lower, 4));
        assert!(
            table.get(2).is_none(),
            "the depth three entry should have gone"
        );
        assert_eq!(table.get(5).unwrap().depth, 4);
        for key in [1, 3, 4] {
            assert!(table.get(key).is_some(), "key {key} should have stayed");
        }
    }

    #[test]
    fn every_field_survives_the_round_trip() {
        // the entry packs what it stores; each field has to come back as it
        // went in, at the edges of its range, for every kind of bound
        let mut table = TranspositionTable::with_capacity(4);
        for (key, bound) in [
            (1u64, Bound::Exact),
            (2, Bound::Upper),
            (3, Bound::Lower),
            (4, Bound::Ordering),
        ] {
            let pv = Pv {
                play: Play::new(
                    63,
                    7,
                    Some(Piece::Queen),
                    Some(PromotePiece::Knight),
                    false,
                    false,
                ),
                score: -29_999,
                depth: MAX_DEPTH,
                bound,
                tainted: true,
            };
            table.set(key, pv);
            let read = table.get(key).expect("stored");
            assert_eq!(read.play, pv.play);
            assert_eq!(read.score, pv.score);
            assert_eq!(read.depth, pv.depth);
            assert!(read.tainted);
            assert!(
                mem::discriminant(&read.bound) == mem::discriminant(&bound),
                "{bound:?} came back as {:?}",
                read.bound
            );
        }
    }

    #[test]
    fn get_compares_the_key_not_just_the_slot() {
        // two different keys which map to the same slot must not be confused for each other
        let mut table = TranspositionTable::with_capacity(1);
        table.set(1, new_pv(Bound::Exact, 1));
        assert!(table.get(1).is_some());
        assert!(table.get(2).is_none());
    }

    #[test]
    fn an_exact_entry_replaces_a_non_exact_entry() {
        let mut table = TranspositionTable::with_capacity(1);
        table.set(1, new_pv(Bound::Lower, 1));
        table.set(1, new_pv(Bound::Exact, 1));
        assert!(matches!(table.get(1).unwrap().bound, Bound::Exact));
    }

    #[test]
    fn a_deeper_exact_entry_survives_a_shallower_one() {
        let mut table = TranspositionTable::with_capacity(1);
        table.set(1, new_pv(Bound::Exact, 8));
        table.set(1, new_pv(Bound::Exact, 2));
        assert_eq!(table.get(1).unwrap().depth, 8);
    }

    /// A bucket with no room left: four positions at the depth given.
    fn full_bucket(depth: u8) -> TranspositionTable {
        let mut table = TranspositionTable::with_capacity(4);
        for key in 1..=4 {
            table.set(key, new_pv(Bound::Exact, depth));
        }
        table
    }

    fn kept(table: &TranspositionTable, keys: std::ops::RangeInclusive<u64>) -> usize {
        keys.filter(|key| table.get(*key).is_some()).count()
    }

    #[test]
    fn a_deeper_entry_replaces_an_exact_entry_for_another_position() {
        let mut table = full_bucket(1);
        table.set(5, new_pv(Bound::Lower, 8));
        assert!(table.get(5).is_some());
        assert_eq!(kept(&table, 1..=4), 3);
    }

    #[test]
    fn a_shallower_entry_does_not_evict_a_deeper_one_for_another_position() {
        let mut table = full_bucket(8);
        table.set(5, new_pv(Bound::Exact, 1));
        assert!(table.get(5).is_none());
        assert_eq!(kept(&table, 1..=4), 4);
    }

    #[test]
    fn a_quiescence_entry_does_not_evict_a_searched_entry() {
        let mut table = full_bucket(5);
        table.set(5, new_pv(Bound::Ordering, 0));
        assert!(table.get(5).is_none());
        assert_eq!(kept(&table, 1..=4), 4);
    }

    #[test]
    fn an_entry_from_searches_ago_is_replaced_regardless_of_depth() {
        // the table remembers which search stored an entry, and one from
        // long enough ago loses to anything: its depth is no longer worth
        // the slot, and its score describes a game the clock has moved past
        let mut table = full_bucket(8);
        for _ in 0..STALE_AFTER_SEARCHES - 1 {
            table.new_search();
        }
        table.set(5, new_pv(Bound::Lower, 1));
        assert!(table.get(5).is_none(), "still recent enough to keep");
        table.new_search();
        table.set(5, new_pv(Bound::Lower, 1));
        assert!(table.get(5).is_some());
        assert_eq!(kept(&table, 1..=4), 3);
    }

    #[test]
    fn the_root_store_takes_a_slot_whatever_the_bucket_holds() {
        let mut table = full_bucket(20);
        table.set_always(5, new_pv(Bound::Exact, 1));
        assert_eq!(table.get(5).unwrap().depth, 1);
        assert_eq!(kept(&table, 1..=4), 3);
    }

    #[test]
    fn the_search_counter_wraps_and_ages_are_taken_round_it() {
        // five bits of generation: a bucket filled in the last generation
        // before the wrap is one search old in the first after it, not
        // thirty, and is stale twelve searches on as any other would be
        let mut table = TranspositionTable::with_capacity(4);
        for _ in 0..30 {
            table.new_search();
        }
        for key in 1..=4 {
            table.set(key, new_pv(Bound::Exact, 8));
        }
        table.new_search();
        table.set(5, new_pv(Bound::Lower, 1));
        assert!(table.get(5).is_none(), "one search old, kept by depth");
        for _ in 0..STALE_AFTER_SEARCHES - 1 {
            table.new_search();
        }
        table.set(5, new_pv(Bound::Lower, 1));
        assert!(table.get(5).is_some(), "twelve searches old, replaced");
        assert_eq!(kept(&table, 1..=4), 3);
    }

    #[test]
    fn the_root_store_overwrites_the_positions_own_entry_not_the_shallowest() {
        let mut table = TranspositionTable::with_capacity(4);
        for (key, depth) in [(1, 9), (2, 1), (3, 9), (4, 9)] {
            table.set(key, new_pv(Bound::Exact, depth));
        }
        table.set_always(3, new_pv(Bound::Exact, 2));
        assert_eq!(table.get(3).unwrap().depth, 2);
        assert_eq!(
            table.get(2).unwrap().depth,
            1,
            "the shallowest was left alone"
        );
        assert_eq!(kept(&table, 1..=4), 4);
    }

    #[test]
    fn a_key_agreeing_on_the_slice_and_the_index_is_taken_for_the_same_position() {
        // the accepted imprecision, stated: in a one bucket table every key
        // shares the index, so one differing only above the slice is a hit
        let mut table = TranspositionTable::with_capacity(4);
        table.set(1, new_pv(Bound::Exact, 8));
        assert_eq!(table.get(1 + (1 << 32)).unwrap().depth, 8);
        assert!(table.get(2).is_none());
    }

    #[test]
    fn clearing_forgets_everything() {
        let mut table = TranspositionTable::with_capacity(2);
        table.set(1, new_pv(Bound::Exact, 8));
        table.clear();
        assert!(table.get(1).is_none());
    }
}
