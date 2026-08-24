//! A cache of already-searched positions, used to order moves and to skip
//! re-searching interior nodes. An accelerator: deleting it may slow the
//! search but never changes the answer.
//!
//! What an entry holds is this module's own business: nothing outside names
//! the entry, its bound, the sixteen byte packing or the form a score is
//! stored in. That is where the room is for what the table does not do yet.
//! Fail low nodes are not stored at all, and a cutoff no move can be
//! attributed to, which is what null move pruning or a static cutoff would
//! want to record, has nowhere to go while an entry must name a play. The two
//! bytes already set aside for the static evaluation are the same bet. Each is
//! a change to this file and to none of its callers.

use crate::board::Board;
use crate::engine::CHECKMATE_THRESHOLD;
use crate::misc::Score;
use crate::play::Play;
use std::mem;

pub const DEFAULT_TABLE_BYTES: usize = 256 * 1024 * 1024;

/// Convert a score to its transposition table form. Mate scores are stored
/// relative to the node they are stored at (plies-to-mate from this node)
/// rather than relative to the root of the search, so that they remain correct
/// when the entry is reused at a different distance from the root.
fn score_to_tt(score: Score, line_ply: usize) -> Score {
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
fn score_from_tt(score: Score, line_ply: usize) -> Score {
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
    /// Entries which landed carrying a draw tainted score.
    pub tainted_stores: u64,
    /// Entries which landed and whose score a probe could later cut on. A
    /// store which loses the replacement contest is not one, and neither is
    /// a quiescence entry: that is kept for the move alone.
    pub stores: u64,
    /// Probes that returned a score, cutting the search off.
    pub score_cutoffs: u64,
    /// Probes that returned a tainted score, which is the error itself: the
    /// stored draw was reachable by the path that stored it and may not be
    /// reachable by this one. Zero while the search refuses them.
    pub tainted_score_cutoffs: u64,
    /// Probes that found a tainted score deep enough to cut and refused it,
    /// handing back the move alone: the cost of refusing, counted in the
    /// branch where a trusting search takes its cutoff, so the two are the
    /// same event under each policy. Not the same count: the refusing
    /// search is the larger tree and probes more. Zero while the search
    /// trusts them.
    pub refused_cutoffs: u64,
}

/// What the search stores about a position and reads back: the entry as the
/// search sees it. The table packs it into an `Entry` of its own.
#[derive(Copy, Clone, Debug)]
struct Pv {
    play: Play,
    score: Score,
    /// True if the score flowed from a repetition or fifty move draw somewhere
    /// below it, so it describes the path taken to this position and not the
    /// position itself. This is the graph history interaction every engine
    /// carries: what the search does with such a score is
    /// `SearchConfig::refuse_tainted_cutoffs`, and which half of the problem
    /// the flag does not cover is in the known limitations in
    /// `docs/ROADMAP.md`.
    tainted: bool,
    depth: u8,
    bound: Bound,
}

/// What the stored score means: the searched window decides whether a score
/// is the truth, a ceiling or a floor, and a reader can only use it for a
/// cutoff its kind allows. Two bits of an entry, which the values are.
#[derive(Copy, Clone, Debug)]
#[repr(u8)]
enum Bound {
    Exact = 0,
    // fail low nodes are not stored yet, see the known limitations in
    // docs/ROADMAP.md
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

/// What a probe found. A stored score is only handed back when the entry is
/// deep enough, its bound allows a cutoff at the window asked about, and it
/// does not describe a draw down somebody else's path.
#[derive(Copy, Clone, Debug)]
pub enum Probe {
    /// Nothing is known about this position.
    Miss,
    /// A move worth trying first, and no score worth trusting.
    Order(Play),
    /// A score the caller may return without searching. It carries whether it
    /// came from a draw tainted entry, because that travels on up.
    Cut { score: Score, tainted: bool },
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

// The layout is load bearing: four entries to a cache line, so a probe reads
// one line and sees all four, and how many slots a table of a given size
// holds is what every pinned node count is counted against. A compiler that
// laid either out differently would move all of them, so it fails the build
// here rather than the suite later.
const _: () = assert!(mem::size_of::<Entry>() == 16);
const _: () = assert!(mem::size_of::<Bucket>() == 64);
const _: () = assert!(mem::align_of::<Bucket>() == 64);

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
    ghi: GhiCounters,
    /// The search under way, as the entries it stores are marked.
    generation: u8,
}

impl TranspositionTable {
    /// A table of at least this many entries, rounded up to whole buckets, or
    /// None if there was not the memory for them. The buckets are asked for
    /// rather than taken, since a size too large for the machine now arrives
    /// over the protocol, and the allocator's answer to a request it cannot
    /// meet is to abort.
    fn with_capacity(capacity: usize) -> Option<Self> {
        let buckets = capacity.div_ceil(BUCKET).max(1);
        let mut table = Vec::new();
        table.try_reserve_exact(buckets).ok()?;
        table.resize(buckets, Bucket::EMPTY);
        Some(Self {
            table,
            ghi: GhiCounters::default(),
            generation: 1,
        })
    }

    pub fn clear(&mut self) {
        self.table.fill(Bucket::EMPTY);
        self.generation = 1;
    }

    pub fn with_capacity_bytes(bytes: usize) -> Option<Self> {
        Self::with_capacity(bytes / mem::size_of::<Entry>())
    }

    /// The table an engine is built with, which is fatal to be unable to
    /// allocate: there is no older table to fall back to and no game under
    /// way to protect.
    pub fn of_bytes(bytes: usize) -> Self {
        Self::with_capacity_bytes(bytes)
            .unwrap_or_else(|| panic!("no memory for a {bytes} byte transposition table"))
    }

    /// The bytes the buckets occupy, which is what was asked for rounded up to
    /// whole ones.
    pub fn bytes(&self) -> usize {
        self.table.len() * mem::size_of::<Bucket>()
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

    fn get(&self, key: u64) -> Option<Pv> {
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

    /// Store unless the slot holds something worth more. Reports whether the
    /// entry landed: a caller counting what the table knows must not count a
    /// store the contest below turned away.
    fn set(&mut self, key: u64, pv: Pv) -> bool {
        let (index, i) = self.slot_for(key);
        let old = self.table[index].entries[i];
        if old.generation() != 0 && self.age(old) < STALE_AFTER_SEARCHES {
            if pv.depth < old.depth {
                return false;
            }
            if pv.depth == old.depth
                && old.key == Entry::slice(key)
                && matches!(old.bound(), Bound::Exact)
                && !matches!(pv.bound, Bound::Exact)
            {
                return false;
            }
        }
        self.table[index].entries[i] = Entry::pack(key, pv, self.generation);
        true
    }

    /// Store without the depth contest above. The root's end-of-iteration
    /// entry is for this: it names the move the search is about to answer
    /// with, and the reported line is read back from its slot, so an entry a
    /// deeper search left there earlier in the game must not outrank it.
    /// When one did, the engine answered one move while its line opened with
    /// another, which is a lie the match tools flag. The leftover's extra
    /// depth is no loss: its score describes repetition and fifty move
    /// context the game has since moved past.
    fn set_always(&mut self, key: u64, pv: Pv) {
        let (index, i) = self.slot_for(key);
        self.table[index].entries[i] = Entry::pack(key, pv, self.generation);
    }

    /// A move worth trying first at this position, learned by a quiescence
    /// search. Never a score to cut on, and never counted against the graph
    /// history figures: quiescence looks at captures and promotions alone, so
    /// what it says a position is worth is not what a full width search would
    /// say, and the figures describe the full width search.
    #[inline]
    pub fn record_ordering(&mut self, board: &Board, play: Play, score: Score) {
        self.set(
            board.key,
            entry(board, play, score, 0, Bound::Ordering, false),
        );
    }

    /// A move which refuted this position: the search failed high on it, so
    /// beta is a floor under what the position is worth and not the worth
    /// itself.
    pub fn record_cutoff(
        &mut self,
        board: &Board,
        play: Play,
        beta: Score,
        depth: u8,
        tainted: bool,
    ) {
        if self.set(
            board.key,
            entry(board, play, beta, depth, Bound::Lower, tainted),
        ) {
            self.count_store(tainted);
        }
    }

    /// The best move found by searching all of them here, with the score it
    /// scored: neither a floor nor a ceiling but the value itself.
    pub fn record_best(
        &mut self,
        board: &Board,
        play: Play,
        score: Score,
        depth: u8,
        tainted: bool,
    ) {
        if self.set(
            board.key,
            entry(board, play, score, depth, Bound::Exact, tainted),
        ) {
            self.count_store(tainted);
        }
    }

    /// The move the engine is about to answer with. Stored past the depth
    /// contest the other verbs hold, for the reason `set_always` gives: this
    /// is the move being played, and the reported line is read back from its
    /// slot.
    pub fn record_answer(
        &mut self,
        board: &Board,
        play: Play,
        score: Score,
        depth: u8,
        tainted: bool,
    ) {
        self.set_always(
            board.key,
            entry(board, play, score, depth, Bound::Exact, tainted),
        );
        self.count_store(tainted);
    }

    #[inline]
    fn count_store(&mut self, tainted: bool) {
        self.ghi.stores += 1;
        self.ghi.tainted_stores += u64::from(tainted);
    }

    /// What the table knows about this position, given the window and depth
    /// the caller is searching to. `refuse_tainted` is the search's, not the
    /// table's: whether to trust a score that came from a draw is a decision
    /// about the search being run, and `SearchConfig` holds it.
    pub fn probe(
        &mut self,
        board: &Board,
        alpha: Score,
        beta: Score,
        depth: u8,
        refuse_tainted: bool,
    ) -> Probe {
        let Some(pv) = self.get(board.key) else {
            return Probe::Miss;
        };
        if pv.depth >= depth {
            let score = score_from_tt(pv.score, board.line_ply);
            let cuts = match pv.bound {
                Bound::Exact => true,
                Bound::Upper => score <= alpha,
                Bound::Lower => score >= beta,
                Bound::Ordering => false,
            };
            if cuts && refuse_tainted && pv.tainted {
                // the stored draw was reachable by the path that stored it and
                // may not be reachable by this one, so the move is still worth
                // ordering by but the score is not worth trusting
                self.ghi.refused_cutoffs += 1;
                return Probe::Order(pv.play);
            }
            if cuts {
                self.ghi.score_cutoffs += 1;
                self.ghi.tainted_score_cutoffs += u64::from(pv.tainted);
                return Probe::Cut {
                    score,
                    tainted: pv.tainted,
                };
            }
        }
        Probe::Order(pv.play)
    }

    /// The move to try first here, whatever wrote it. A quiescence move is fit
    /// for this even though its score is fit for nothing.
    #[inline]
    pub fn ordering_play(&self, board: &Board) -> Option<Play> {
        self.get(board.key).map(|pv| pv.play)
    }

    /// The move the table says is meant here, for reporting a line. Unlike
    /// `ordering_play` this refuses a quiescence entry: that move orders the
    /// next search and does not say what the engine intends to play.
    pub fn intended_play(&self, board: &Board) -> Option<Play> {
        let pv = self.get(board.key)?;
        (!matches!(pv.bound, Bound::Ordering)).then_some(pv.play)
    }

    /// How much of what the table handed back depended on the path taken.
    pub fn ghi(&self) -> GhiCounters {
        self.ghi
    }
}

/// Fold a position and a result into an entry. Converting the score to the
/// table's mate-relative form happens here, so that no caller has to remember
/// to.
#[inline]
fn entry(board: &Board, play: Play, score: Score, depth: u8, bound: Bound, tainted: bool) -> Pv {
    Pv {
        play,
        depth,
        score: score_to_tt(score, board.line_ply),
        bound,
        tainted,
    }
}

#[cfg(test)]
mod tests {
    use super::{Bound, Play, Pv, STALE_AFTER_SEARCHES, TranspositionTable};
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
    fn four_positions_share_a_bucket_and_all_are_kept() {
        let mut table = TranspositionTable::with_capacity(4).expect("a table of a few buckets");
        for key in 1..=4 {
            table.set(key, new_pv(Bound::Exact, key as u8));
        }
        for key in 1..=4 {
            assert_eq!(table.get(key).unwrap().depth, key as u8, "key {key}");
        }
    }

    #[test]
    fn a_fifth_position_evicts_the_shallowest_of_the_bucket() {
        let mut table = TranspositionTable::with_capacity(4).expect("a table of a few buckets");
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
        let mut table = TranspositionTable::with_capacity(4).expect("a table of a few buckets");
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
        let mut table = TranspositionTable::with_capacity(1).expect("a table of a few buckets");
        table.set(1, new_pv(Bound::Exact, 1));
        assert!(table.get(1).is_some());
        assert!(table.get(2).is_none());
    }

    #[test]
    fn an_exact_entry_replaces_a_non_exact_entry() {
        let mut table = TranspositionTable::with_capacity(1).expect("a table of a few buckets");
        table.set(1, new_pv(Bound::Lower, 1));
        table.set(1, new_pv(Bound::Exact, 1));
        assert!(matches!(table.get(1).unwrap().bound, Bound::Exact));
    }

    #[test]
    fn a_deeper_exact_entry_survives_a_shallower_one() {
        let mut table = TranspositionTable::with_capacity(1).expect("a table of a few buckets");
        table.set(1, new_pv(Bound::Exact, 8));
        table.set(1, new_pv(Bound::Exact, 2));
        assert_eq!(table.get(1).unwrap().depth, 8);
    }

    /// A bucket with no room left: four positions at the depth given.
    fn full_bucket(depth: u8) -> TranspositionTable {
        let mut table = TranspositionTable::with_capacity(4).expect("a table of a few buckets");
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
    fn a_store_the_contest_turns_away_says_so() {
        // the graph history figures count what lands, and set's answer is
        // what they ride on: a refused store reporting true would put
        // entries in the count that are not in the table
        let mut table = full_bucket(8);
        assert!(!table.set(5, new_pv(Bound::Lower, 1)));
        assert!(table.set(5, new_pv(Bound::Lower, 9)));
    }

    #[test]
    fn a_turned_away_store_is_not_counted() {
        // the one bucket is already deep, so the shallow cutoff is turned
        // away and must leave the figures alone; deeper, it lands and counts
        let mut table = full_bucket(8);
        let board = crate::board::Board::new();
        let play = Play::new(0, 1, None, None, false, false);
        table.record_cutoff(&board, play, 0, 1, true);
        assert_eq!(table.ghi().stores, 0, "a turned away store was counted");
        assert_eq!(table.ghi().tainted_stores, 0);
        table.record_cutoff(&board, play, 0, 9, true);
        assert_eq!(table.ghi().stores, 1);
        assert_eq!(table.ghi().tainted_stores, 1);
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
        let mut table = TranspositionTable::with_capacity(4).expect("a table of a few buckets");
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
        let mut table = TranspositionTable::with_capacity(4).expect("a table of a few buckets");
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
        let mut table = TranspositionTable::with_capacity(4).expect("a table of a few buckets");
        table.set(1, new_pv(Bound::Exact, 8));
        assert_eq!(table.get(1 + (1 << 32)).unwrap().depth, 8);
        assert!(table.get(2).is_none());
    }

    #[test]
    fn clearing_forgets_everything() {
        let mut table = TranspositionTable::with_capacity(2).expect("a table of a few buckets");
        table.set(1, new_pv(Bound::Exact, 8));
        table.clear();
        assert!(table.get(1).is_none());
    }
}
