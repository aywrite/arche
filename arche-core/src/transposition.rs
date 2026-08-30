// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

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
use crate::misc::Score;
use crate::play::Play;
use crate::value::{Value, is_mate};
use std::cell::Cell;
use std::mem;

pub const DEFAULT_TABLE_BYTES: usize = 256 * 1024 * 1024;

/// Convert a score to its transposition table form. Mate scores are stored
/// relative to the node they are stored at (plies-to-mate from this node)
/// rather than relative to the root of the search, so that they remain correct
/// when the entry is reused at a different distance from the root.
fn score_to_tt(score: Score, line_ply: usize) -> Score {
    if !is_mate(score) {
        score
    } else if score > 0 {
        score + line_ply as Score
    } else {
        score - line_ply as Score
    }
}

/// The inverse of score_to_tt: convert a stored mate score back to being
/// relative to the root of the current search.
fn score_from_tt(score: Score, line_ply: usize) -> Score {
    if !is_mate(score) {
        score
    } else if score > 0 {
        score - line_ply as Score
    } else {
        score + line_ply as Score
    }
}

/// How often the transposition table hands back a score that depended on the
/// path taken rather than on the position, which is the graph history
/// interaction error every engine carries and none of them measure. The
/// figures describe the whole search, quiescence included, since quiescence
/// stores real bounds and takes real cutoffs like any other node.
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
    /// Tainted results the search declined to offer the table because its
    /// policy keeps only clean scores, counted before the replacement
    /// contest: some of these would have lost it anyway, so this is the
    /// policy's reach rather than exactly what the table went without.
    pub skipped_stores: u64,
    /// Probes that found a tainted score deep enough to cut and refused it,
    /// handing back the move alone: the cost of refusing, counted in the
    /// branch where a trusting search takes its cutoff, so the two are the
    /// same event under each policy. Not the same count: the refusing
    /// search is the larger tree and probes more. Zero while the search
    /// trusts them; under the rule50 policy this counts its horizon
    /// refusals instead, tainted or not.
    pub refused_cutoffs: u64,
}

/// What the entry's thirty two bit key slice costs. A probe accepts an entry
/// when the slice matches, and two positions sharing a slice and an index
/// make the search read a stranger's entry as its own. The entry's comment
/// puts that at about one probe in a thousand million; these are the same
/// figure counted rather than reasoned about.
///
/// A run of the size the bench is expects no false accepts at all, so the
/// observation on its own cannot tell a working instrument from a dead one.
/// That is what `comparisons` and `narrow_accepts` are for. The first gives
/// the expectation the observation is read against, and the second counts
/// what a narrower signature would have accepted over the same comparisons,
/// at each of the widths in `NARROW_WIDTHS`. The narrowest of those is large
/// enough at this scale to be compared with its own expectation. A narrow
/// figure sitting on its expectation says the rate really does scale by two
/// to the minus the width on this workload, which is what lets the thirty
/// two bit expectation be believed where the observation cannot say
/// anything, and the widths near thirty two are what a claimant on the
/// signature's bits would be costing.
///
/// The audit is off in every path a game plays, so a run that was not asked
/// for one has nothing to report rather than zeroes.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SignatureCounters {
    /// Keyed lookups asked of the table, whatever asked for them.
    pub probes: u64,
    /// Probes the slice accepted, which is a hit as the table sees one.
    pub hits: u64,
    /// Live entries the probes compared their slice against whose full key
    /// belonged to another position, over the entries each probe really
    /// looked at. One chance in two to the width apiece, so this is the
    /// denominator every expectation is drawn from.
    pub comparisons: u64,
    /// Accepted probes whose full key differed, so the entry belonged to
    /// another position.
    pub false_accepts: u64,
    /// False accepts the search then took a score from, which is the half
    /// that costs anything: a foreign move is turned away by the legality
    /// check and costs a little order, and a foreign score cuts a subtree
    /// that was never searched.
    pub false_accept_cutoffs: u64,
    /// Comparisons a narrower signature would have accepted and this one
    /// refused, one figure for each width in `NARROW_WIDTHS` and in that
    /// order: the low bits of the width agree where the whole slice does
    /// not. A signature of that width would take these and the false
    /// accepts both, so its rate is the two added.
    ///
    /// The widths are counted independently, and are cumulative by
    /// construction: an entry agreeing on twenty four low bits agrees on
    /// sixteen as well and is counted under both. So each figure is read
    /// against its own expectation, and the figures are not a partition to
    /// be added up.
    ///
    /// Counted and never acted on. A search really running sixteen bits
    /// would have stopped its scan at the first of them, which is a
    /// different tree, and this measures the comparisons the search that
    /// ran actually made.
    pub narrow_accepts: [u64; NARROW_WIDTHS.len()],
    /// Stores whose slice matched a foreign full key, so the store took
    /// another position's entry for this position's and replaced it.
    ///
    /// Landed stores only. A store the depth contest turns away after
    /// comparing itself against a foreign entry's depth is a related cost,
    /// since the comparison was against the wrong position and this
    /// position's result went unstored, but nothing was evicted and it is
    /// not counted here.
    pub aliased_evictions: u64,
}

/// The chances in which the signature the table runs accepts a foreign
/// entry: two to its thirty two bits.
const WIDE: f64 = 4_294_967_296.0;

/// The narrower signatures the audit counts beside the one it runs, in bits
/// and smallest first. Sixteen is the width with enough counts at the
/// bench's scale to be read on its own; twenty four and twenty eight are
/// what the signature would be left with if four or eight of its bits went
/// to other metadata, and are there so the scaling claim rests on three
/// measured points rather than one.
pub const NARROW_WIDTHS: [u32; 3] = [16, 24, 28];

impl SignatureCounters {
    /// Add another table's figures to these, for a caller totalling a suite
    /// of searches with a table each.
    pub fn absorb(&mut self, other: SignatureCounters) {
        self.probes += other.probes;
        self.hits += other.hits;
        self.comparisons += other.comparisons;
        self.false_accepts += other.false_accepts;
        self.false_accept_cutoffs += other.false_accept_cutoffs;
        for (total, counted) in self.narrow_accepts.iter_mut().zip(other.narrow_accepts) {
            *total += counted;
        }
        self.aliased_evictions += other.aliased_evictions;
    }

    /// The false accepts the thirty two bit signature is expected to have
    /// produced over the comparisons this run made, which is what the
    /// observation beside it means anything against.
    pub fn expected_false_accepts(&self) -> f64 {
        self.comparisons as f64 / WIDE
    }

    /// The same for a narrow counter: the low bits of the width agreeing
    /// while the whole slice does not, which is one chance in two to the
    /// width less the chance the whole slice agrees too.
    pub fn expected_narrow_accepts(&self, width: u32) -> f64 {
        // narrower than the signature the table runs, which is what makes it
        // a narrow width. A width of thirty two or more would shift the one
        // off the end of the u64 as well as meaning nothing
        debug_assert!(width < 32, "a narrow width is under thirty two: {width}");
        let narrow = (1u64 << width) as f64;
        self.comparisons as f64 * (1.0 / narrow - 1.0 / WIDE)
    }

    /// Each narrow width with what it counted and what it expected, in the
    /// order `NARROW_WIDTHS` gives, for a caller printing the line or
    /// reading one figure against another.
    pub fn narrow(&self) -> impl Iterator<Item = (u32, u64, f64)> + '_ {
        NARROW_WIDTHS
            .into_iter()
            .zip(self.narrow_accepts)
            .map(|(width, counted)| (width, counted, self.expected_narrow_accepts(width)))
    }
}

/// The ground truth the table does not keep: the full key of every entry,
/// held beside the entries and written whenever one lands.
///
/// Allocated only when the bench asks for the audit, so an ordinary search
/// carries a null pointer and one predictable branch a probe. The dead
/// `static_eval` bytes are no use for this. They are the bits the entry
/// layout is being argued over, and sixteen more of signature would still
/// alias where sixty four cannot.
#[derive(Debug)]
struct Audit {
    /// One key an entry, in the table's own order: the bucket's index times
    /// four, plus the entry within the bucket. A slot never written holds
    /// zero, which nothing reads: the entry beside it has generation zero
    /// and no probe gets that far.
    keys: Box<[u64]>,
    /// Counted through a cell because a probe reads the table through a
    /// shared reference, and an instrument must not turn `get` into a
    /// mutation for the sake of its own bookkeeping.
    ///
    /// The cell costs the table its `Sync`, which nothing asks of it: a
    /// table belongs to one engine, and the protocol moves the engine whole
    /// to the search thread rather than sharing it. `Send` is what that move
    /// needs, and it is asserted below.
    counters: Cell<SignatureCounters>,
}

impl Audit {
    /// The keys for a table of this many entries, or none if there was not
    /// the memory. Asked for rather than taken, the way the table asks for
    /// its own buckets: a size that arrives over the protocol may be one the
    /// machine cannot hold, and the allocator's answer to that is to abort.
    fn of_entries(entries: usize) -> Option<Self> {
        let mut keys = Vec::new();
        keys.try_reserve_exact(entries).ok()?;
        keys.resize(entries, 0);
        Some(Self {
            keys: keys.into_boxed_slice(),
            counters: Cell::new(SignatureCounters::default()),
        })
    }

    /// Change the counters, which a cell hands over by value.
    #[inline]
    fn count(&self, change: impl FnOnce(&mut SignatureCounters)) {
        let mut counters = self.counters.get();
        change(&mut counters);
        self.counters.set(counters);
    }
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
    /// carries: what the search does with such a score is its
    /// `TaintPolicy`, and which half of the problem the flag does not
    /// cover is in the known limitations in `docs/ROADMAP.md`.
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
    Upper = 1,
    Lower = 2,
    /// Written by nothing since quiescence began storing real bounds; the
    /// two flag bits decode totally, so the value keeps a name, and a
    /// probe treats it as a move with no score worth trusting.
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
    /// A score the caller may return without searching, carrying where it
    /// came from, because that travels on up.
    Cut(Value),
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

// The audit's cell is not `Sync`, and the table is not asked to be: the
// protocol moves an engine whole to the search thread. Moving is `Send`, and
// losing that would be found at that move rather than here without this.
const fn assert_send<T: Send>() {}
const _: () = assert_send::<TranspositionTable>();

/// An entry stored this many searches ago or more is replaced whatever its
/// depth: its score describes repetition and fifty move context the game
/// has moved past, and a slot is worth more to the search under way. The
/// window the ply rule gave before, the twenty ply cap that then bounded a
/// search plus three plies with a side searching every other ply, came to
/// about the same.
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
    /// The full keys of the entries, or none, which is what every table an
    /// engine plays with holds. `audit_signatures` fills it in.
    audit: Option<Box<Audit>>,
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
            audit: None,
        })
    }

    pub fn clear(&mut self) {
        self.table.fill(Bucket::EMPTY);
        self.generation = 1;
        if let Some(audit) = self.audit.as_deref_mut() {
            audit.keys.fill(0);
        }
    }

    /// Keep the full key of every entry beside it, so that a probe the
    /// signature accepted can be held against the position it really stands
    /// for. Off until this is called, and only the bench family calls it: a
    /// session that plays games never allocates the keys.
    ///
    /// The table is cleared as the keys go on. There is no key on the side
    /// for an entry stored before the audit began, and a probe reading one
    /// would find a stranger's key where the entry is really its own, so an
    /// audited table starts empty and every entry it reports on was stored
    /// under the audit.
    ///
    /// Eight bytes an entry, beside the entry's sixteen, so an audited table
    /// costs half its own size again. False if there was not the memory for
    /// them, in which case the table is left as it was and unaudited: the
    /// size the keys are asked for comes from whatever the caller sized the
    /// table with. Nothing about the search changes either way, and no probe
    /// or store reads a different entry for having been counted.
    #[must_use]
    pub fn audit_signatures(&mut self) -> bool {
        let Some(audit) = Audit::of_entries(self.table.len() * BUCKET) else {
            return false;
        };
        self.audit = Some(Box::new(audit));
        self.clear();
        true
    }

    /// What the signature audit counted, or none when the table is not
    /// keeping the keys.
    pub fn signatures(&self) -> Option<SignatureCounters> {
        self.audit.as_deref().map(|audit| audit.counters.get())
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
        self.get_audited(key).0
    }

    /// The same lookup, saying as well whether the entry it accepted belongs
    /// to another position. The answer is always false without the audit,
    /// which is where the search's own callers get it from, and a caller
    /// that takes a score from the entry passes it back to
    /// `count_false_accept_cutoff`.
    fn get_audited(&self, key: u64) -> (Option<Pv>, bool) {
        let index = self.index_for(key);
        let slice = Entry::slice(key);
        let bucket = &self.table[index].entries;
        let found = bucket
            .iter()
            .enumerate()
            // the key first: in a warm table nearly every entry has a
            // generation, and the key is what rejects three of the four
            .find(|(_, entry)| entry.key == slice && entry.generation() != 0);
        let pv = found.map(|(_, entry)| entry.unpack());
        let Some(audit) = self.audit.as_deref() else {
            return (pv, false);
        };
        // the entries the scan above really looked at, which is up to and
        // including the one it took. The chances of a false accept are the
        // foreign entries among those and no others, so the denominator is
        // read off the probe that happened rather than assumed to be four
        let examined = found.map_or(BUCKET, |(i, _)| i + 1);
        let mut comparisons = 0;
        let mut narrow = [0; NARROW_WIDTHS.len()];
        for (i, entry) in bucket[..examined].iter().enumerate() {
            if entry.generation() == 0 || audit.keys[index * BUCKET + i] == key {
                continue;
            }
            comparisons += 1;
            // the low bits agreeing where the whole slice does not: an
            // acceptance a narrower signature would have made and this one
            // refused. The low bits that agree are the ones below the first
            // that differs, so one count a width settles every width at
            // once, and a width is counted whenever the agreement reaches
            // it: an entry agreeing on twenty four agrees on sixteen too
            if entry.key != slice {
                let agreeing = (entry.key ^ slice).trailing_zeros();
                for (count, width) in narrow.iter_mut().zip(NARROW_WIDTHS) {
                    *count += u64::from(agreeing >= width);
                }
            }
        }
        let foreign = found.is_some_and(|(i, _)| audit.keys[index * BUCKET + i] != key);
        audit.count(|counters| {
            counters.probes += 1;
            counters.hits += u64::from(found.is_some());
            counters.comparisons += comparisons;
            counters.false_accepts += u64::from(foreign);
            for (total, counted) in counters.narrow_accepts.iter_mut().zip(narrow) {
                *total += counted;
            }
        });
        (pv, foreign)
    }

    /// A score just handed back came from an entry the audit found foreign.
    /// Counted and nothing else: what the search does with a false accept is
    /// what it would have done without the audit, which is the whole of the
    /// instrument's claim to change nothing.
    #[inline]
    fn count_false_accept_cutoff(&self, foreign: bool) {
        if !foreign {
            return;
        }
        if let Some(audit) = self.audit.as_deref() {
            audit.count(|counters| counters.false_accept_cutoffs += 1);
        }
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
        self.store(index, i, key, pv);
        true
    }

    /// Write the entry, and under the audit record the full key it stands
    /// for. A slot the position was given because the slice matched, holding
    /// a different full key, is another position's entry taken for this
    /// one and replaced without anything noticing.
    #[inline]
    fn store(&mut self, index: usize, i: usize, key: u64, pv: Pv) {
        let old = self.table[index].entries[i];
        self.table[index].entries[i] = Entry::pack(key, pv, self.generation);
        let Some(audit) = self.audit.as_deref_mut() else {
            return;
        };
        let at = index * BUCKET + i;
        if old.generation() != 0 && old.key == Entry::slice(key) && audit.keys[at] != key {
            audit.count(|counters| counters.aliased_evictions += 1);
        }
        audit.keys[at] = key;
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
        self.store(index, i, key, pv);
    }

    /// A move which refuted this position: the search failed high on it, so
    /// the score is a floor under what the position is worth and not the
    /// worth itself. The search runs fail soft, so the floor recorded is the
    /// best score it saw, at least as tight as the beta it crossed.
    pub fn record_cutoff(&mut self, board: &Board, play: Play, floor: Value, depth: u8) {
        if self.set(board.key, entry(board, play, floor, depth, Bound::Lower)) {
            self.count_store(floor.tainted);
        }
    }

    /// Every move here fell short of the window: the score is a ceiling
    /// over what the position is worth, and the move is the one that came
    /// closest, worth trying first next time even though it proved nothing.
    pub fn record_ceiling(&mut self, board: &Board, play: Play, ceiling: Value, depth: u8) {
        if self.set(board.key, entry(board, play, ceiling, depth, Bound::Upper)) {
            self.count_store(ceiling.tainted);
        }
    }

    /// The best move found by searching all of them here, with the score it
    /// scored: neither a floor nor a ceiling but the value itself.
    pub fn record_best(&mut self, board: &Board, play: Play, score: Value, depth: u8) {
        if self.set(board.key, entry(board, play, score, depth, Bound::Exact)) {
            self.count_store(score.tainted);
        }
    }

    /// The move the engine is about to answer with. Stored past the depth
    /// contest the other verbs hold, for the reason `set_always` gives: this
    /// is the move being played, and the reported line is read back from its
    /// slot.
    pub fn record_answer(&mut self, board: &Board, play: Play, score: Value, depth: u8) {
        self.set_always(board.key, entry(board, play, score, depth, Bound::Exact));
        self.count_store(score.tainted);
    }

    /// The search declined a store under its taint policy; see
    /// `GhiCounters::skipped_stores`.
    #[inline]
    pub fn count_skipped_store(&mut self) {
        self.ghi.skipped_stores += 1;
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
        guard_rule50: bool,
    ) -> Probe {
        let (found, foreign) = self.get_audited(board.key);
        let Some(pv) = found else {
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
            if cuts && guard_rule50 && board.fifty_move_near_expiry() {
                // near the horizon every stored score is suspect, tainted
                // or not, so the whole cutoff is given up rather than the
                // tainted ones alone
                self.ghi.refused_cutoffs += 1;
                return Probe::Order(pv.play);
            }
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
                self.count_false_accept_cutoff(foreign);
                return Probe::Cut(Value::with_taint(score, pv.tainted));
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
    /// `ordering_play` this refuses a depth zero entry: quiescence wrote it,
    /// its move orders the next search from a tree of captures alone, and it
    /// does not say what the engine intends to play.
    pub fn intended_play(&self, board: &Board) -> Option<Play> {
        let pv = self.get(board.key)?;
        (pv.depth > 0 && !matches!(pv.bound, Bound::Ordering)).then_some(pv.play)
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
fn entry(board: &Board, play: Play, value: Value, depth: u8, bound: Bound) -> Pv {
    Pv {
        play,
        depth,
        score: score_to_tt(value.score, board.line_ply),
        bound,
        tainted: value.tainted,
    }
}

#[cfg(test)]
mod tests {
    use super::{Bound, NARROW_WIDTHS, Play, Pv, STALE_AFTER_SEARCHES, TranspositionTable, Value};
    use crate::engine::MAX_PLY;
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
                depth: MAX_PLY,
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
        // quiescence writes at depth zero, so the depth contest is what
        // keeps its entries from displacing a searched position's
        let mut table = full_bucket(5);
        table.set(5, new_pv(Bound::Lower, 0));
        assert!(table.get(5).is_none());
        assert_eq!(kept(&table, 1..=4), 4);
    }

    #[test]
    fn a_recorded_ceiling_cuts_below_its_score_and_only_orders_above_it() {
        // a fail low store is only worth having if a later probe whose
        // alpha the ceiling cannot reach may cut on it, and a probe whose
        // window the ceiling sits inside must still search
        use super::Probe;
        let mut table = TranspositionTable::with_capacity(4).expect("a table of a few buckets");
        let board = crate::board::Board::new();
        let play = Play::new(0, 1, None, None, false, false);
        table.record_ceiling(&board, play, Value::clean(-50), 5);
        match table.probe(&board, -10, 10, 5, true, false) {
            Probe::Cut(value) => assert_eq!(value, Value::clean(-50)),
            other => panic!("a ceiling under alpha did not cut: {other:?}"),
        }
        assert!(matches!(
            table.probe(&board, -100, 10, 5, true, false),
            Probe::Order(_)
        ));
    }

    #[test]
    fn the_rule50_guard_refuses_any_cutoff_at_the_horizon() {
        // under the guard a deep clean entry cuts from a fresh position
        // and is refused move-only once the counter stands at the guard,
        // with the refusal counted as the policy's cost
        use super::Probe;
        let mut table = TranspositionTable::with_capacity(4).expect("a table of a few buckets");
        let fresh = crate::board::Board::from_fen("4k3/8/8/8/8/8/8/4K3 w - - 0 1").unwrap();
        let near = crate::board::Board::from_fen("4k3/8/8/8/8/8/8/4K3 w - - 96 112").unwrap();
        let play = Play::new(0, 1, None, None, false, false);
        table.record_best(&fresh, play, Value::clean(0), 5);
        assert!(matches!(
            table.probe(&fresh, -10, 10, 5, false, true),
            Probe::Cut { .. }
        ));
        assert!(matches!(
            table.probe(&near, -10, 10, 5, false, true),
            Probe::Order(_)
        ));
        assert_eq!(table.ghi().refused_cutoffs, 1);
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
        table.record_cutoff(&board, play, Value::tainted(0), 1);
        assert_eq!(table.ghi().stores, 0, "a turned away store was counted");
        assert_eq!(table.ghi().tainted_stores, 0);
        table.record_cutoff(&board, play, Value::tainted(0), 9);
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

    /// The instrument is off unless it is asked for, which is what makes it
    /// free: a table with no audit has nothing to report rather than zeroes.
    #[test]
    fn a_table_keeps_no_keys_until_the_audit_asks_for_them() {
        let mut table = TranspositionTable::with_capacity(4).expect("a table of one bucket");
        table.set(1, new_pv(Bound::Exact, 4));
        assert!(table.get(1).is_some());
        assert!(table.signatures().is_none());
        assert!(table.audit_signatures());
        assert_eq!(table.signatures().expect("audited"), Default::default());
        // an entry stored before the audit has no key on the side, so a
        // probe would read it as a stranger's. The audit starts the table
        // empty rather than reporting on entries it never saw stored
        assert!(table.get(1).is_none(), "the audit left an entry behind");
        assert_eq!(table.signatures().expect("audited").probes, 1);
        assert_eq!(table.signatures().expect("audited").false_accepts, 0);
    }

    /// Two keys agreeing on the slice and the index, built rather than
    /// looked for: the slice is the low thirty two bits, and a one bucket
    /// table gives every key the same index. The probe is a hit the table
    /// cannot tell from a real one, and the store that follows takes the
    /// first position's entry for the second's.
    #[test]
    fn a_probe_and_a_store_on_a_shared_slice_are_counted() {
        let mut table = TranspositionTable::with_capacity(4).expect("a table of one bucket");
        assert!(table.audit_signatures());
        let twin = 1 + (1 << 32);
        table.set(1, new_pv(Bound::Exact, 4));
        assert!(table.get(twin).is_some(), "the slice accepted the twin");
        let counted = table.signatures().expect("audited");
        assert_eq!(counted.probes, 1);
        assert_eq!(counted.hits, 1);
        assert_eq!(counted.false_accepts, 1);
        assert_eq!(counted.false_accept_cutoffs, 0, "no score was taken");
        // the narrow counters are the acceptances a narrower signature would
        // have made and this one refused. This one took the entry, so it
        // belongs to the false accepts and to no width beside them: a reader
        // adding a narrow figure to the false accepts must not count it twice
        assert_eq!(
            counted.narrow_accepts,
            [0; NARROW_WIDTHS.len()],
            "a real false accept is not a narrow accept"
        );
        assert_eq!(
            counted.aliased_evictions, 0,
            "the first store found an empty slot"
        );
        table.set(twin, new_pv(Bound::Exact, 4));
        assert_eq!(table.signatures().expect("audited").aliased_evictions, 1);
    }

    /// The same table and the same index with a slice of its own: the probe
    /// misses, the store finds a slot of its own, and nothing is counted
    /// against the signature. The comparison is still counted, because a
    /// live entry belonging to another position is what a false accept
    /// would have come from and the expectation is drawn from those.
    #[test]
    fn a_key_with_a_slice_of_its_own_is_counted_against_nothing() {
        let mut table = TranspositionTable::with_capacity(4).expect("a table of one bucket");
        assert!(table.audit_signatures());
        table.set(1, new_pv(Bound::Exact, 4));
        assert!(table.get(2).is_none());
        table.set(2, new_pv(Bound::Exact, 4));
        let counted = table.signatures().expect("audited");
        assert_eq!(counted.probes, 1);
        assert_eq!(counted.hits, 0);
        assert_eq!(counted.comparisons, 1);
        assert_eq!(counted.false_accepts, 0);
        assert_eq!(counted.narrow_accepts, [0; NARROW_WIDTHS.len()]);
        assert_eq!(counted.aliased_evictions, 0);
    }

    /// The hypothetical the contest wants, one width at a time: keys
    /// agreeing on the low bits of the width and differing on the first bit
    /// above them. The probe misses, as it must, and the counter says a
    /// signature of that width would have taken the entry. The widths are
    /// cumulative, so such a key counts under the width it was built for
    /// and under every narrower one, and under no wider one.
    ///
    /// Each width is tried from both sides. A key agreeing on exactly the
    /// width has to be counted, and a key agreeing on one bit fewer has to
    /// be refused, which is what puts the boundary where the width says
    /// rather than a bit to either side of it.
    #[test]
    fn an_acceptance_a_narrower_signature_would_have_made_is_counted() {
        for built_for in NARROW_WIDTHS {
            let mut table = TranspositionTable::with_capacity(4).expect("a table of one bucket");
            assert!(table.audit_signatures());
            table.set(1, new_pv(Bound::Exact, 4));
            // the low bits of one, and the first bit above the width set, so
            // the slice differs and the agreement stops exactly at the width
            let narrow_twin = 1 | (1u64 << built_for);
            assert!(table.get(narrow_twin).is_none(), "the slice refused it");
            let counted = table.signatures().expect("audited");
            assert_eq!(counted.comparisons, 1);
            assert_eq!(counted.false_accepts, 0);
            let at_the_width: Vec<u64> = NARROW_WIDTHS
                .iter()
                .map(|width| u64::from(*width <= built_for))
                .collect();
            assert_eq!(
                counted.narrow_accepts.to_vec(),
                at_the_width,
                "a key agreeing on {built_for} low bits"
            );
            // one bit short of the width. This width has to refuse it and
            // every narrower one has to take it, so a counter reading one bit
            // too few is caught here rather than overstating the whole run
            let one_short = 1 | (1u64 << (built_for - 1));
            assert!(table.get(one_short).is_none(), "the slice refused it");
            let under_the_width: Vec<u64> = NARROW_WIDTHS
                .iter()
                .zip(&at_the_width)
                .map(|(width, counted)| counted + u64::from(*width < built_for))
                .collect();
            assert_eq!(
                table.signatures().expect("audited").narrow_accepts.to_vec(),
                under_the_width,
                "a key agreeing on {} low bits",
                built_for - 1
            );
            // and a key sharing none of the widths is counted against none
            assert!(table.get(0x0001_0002).is_none());
            let after = table.signatures().expect("audited");
            assert_eq!(after.narrow_accepts.to_vec(), under_the_width);
            assert_eq!(after.comparisons, 3);
        }
    }

    /// The counts fall as the width rises, on a probe sequence built to make
    /// them fall: one key agreeing on each width in turn, so the widest is
    /// counted once, the middle twice and the narrowest three times. The
    /// ordering is what a reader leans on when comparing one width's figure
    /// with another's, and three distinct counts are what it takes to say
    /// the ordering holds rather than to watch three zeroes agree.
    #[test]
    fn the_counts_fall_as_the_width_rises() {
        let mut table = TranspositionTable::with_capacity(4).expect("a table of one bucket");
        assert!(table.audit_signatures());
        table.set(1, new_pv(Bound::Exact, 4));
        for width in NARROW_WIDTHS {
            assert!(
                table.get(1 | (1u64 << width)).is_none(),
                "the slice refused it"
            );
        }
        let counted = table.signatures().expect("audited");
        assert_eq!(counted.comparisons, NARROW_WIDTHS.len() as u64);
        assert_eq!(counted.false_accepts, 0);
        // a width takes every probe built for it or for a wider one, which
        // for three widths is three, two and one
        let wanted: Vec<u64> = NARROW_WIDTHS
            .iter()
            .map(|width| {
                NARROW_WIDTHS
                    .iter()
                    .filter(|probed| *probed >= width)
                    .count() as u64
            })
            .collect();
        assert_eq!(counted.narrow_accepts.to_vec(), wanted, "one probe a width");
        // the counts differ, so the ordering below is read against data
        // rather than against zeroes agreeing with each other
        assert!(counted.narrow_accepts[0] > counted.narrow_accepts[NARROW_WIDTHS.len() - 1]);
        assert!(
            counted
                .narrow_accepts
                .windows(2)
                .all(|widths| widths[0] >= widths[1]),
            "a wider signature accepted more than a narrower one: {:?}",
            counted.narrow_accepts
        );
    }

    /// The dangerous half. A foreign entry deep enough to cut is a subtree
    /// answered from a score belonging to another position, and the probe
    /// hands it back exactly as it would without the audit. Counting it is
    /// all that detection does.
    #[test]
    fn a_score_taken_from_a_foreign_entry_is_counted_as_a_cutoff() {
        use super::Probe;
        let mut table = TranspositionTable::with_capacity(4).expect("a table of one bucket");
        assert!(table.audit_signatures());
        let board = crate::board::Board::new();
        let play = Play::new(0, 1, None, None, false, false);
        table.record_best(&board, play, Value::clean(20), 5);
        // a key differing above the slice, which one bucket leaves sharing
        // the index as well
        let mut twin = board;
        twin.key ^= 1 << 63;
        match table.probe(&twin, -10, 10, 5, true, false) {
            Probe::Cut(value) => assert_eq!(value, Value::clean(20)),
            other => panic!("the foreign entry did not cut: {other:?}"),
        }
        let counted = table.signatures().expect("audited");
        assert_eq!(counted.false_accepts, 1);
        assert_eq!(counted.false_accept_cutoffs, 1);
    }

    #[test]
    fn clearing_forgets_everything() {
        let mut table = TranspositionTable::with_capacity(2).expect("a table of a few buckets");
        table.set(1, new_pv(Bound::Exact, 8));
        table.clear();
        assert!(table.get(1).is_none());
    }
}
