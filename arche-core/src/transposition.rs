// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! A cache of already-searched positions, used to order moves and to skip
//! re-searching interior nodes. Under the reference configuration it is an
//! accelerator: deleting it may slow the search but never changes the
//! answer. Under the default it can change the answer, since the default
//! taint policy takes the cutoffs a draw tainted entry offers everywhere
//! short of the fifty move horizon (see `TaintPolicy`), and the move it
//! suggests changes the order the shortcuts prune against.
//!
//! Nothing outside names the entry, its bound, the sixteen byte packing or
//! the form a score is stored in. An entry must name a play, so a cutoff
//! no move can be attributed to (null move pruning, a static cutoff) has
//! nowhere to go yet; a node no move raised alpha at names the move that
//! came closest (`record_ceiling`).

use crate::board::Board;
use crate::misc::Score;
use crate::play::Play;
use crate::value::{Value, is_mate};
use std::cell::Cell;
use std::mem;

pub const DEFAULT_TABLE_BYTES: usize = 256 * 1024 * 1024;

/// A score in its table form. A mate score is stored as plies to mate
/// from the node storing it rather than from the root, so that it stays
/// correct when the entry is read at another distance from the root.
fn score_to_tt(score: Score, line_ply: usize) -> Score {
    if !is_mate(score) {
        score
    } else if score > 0 {
        score + line_ply as Score
    } else {
        score - line_ply as Score
    }
}

/// The inverse of `score_to_tt`: a stored mate score made relative to the
/// root of the current search.
fn score_from_tt(score: Score, line_ply: usize) -> Score {
    if !is_mate(score) {
        score
    } else if score > 0 {
        score - line_ply as Score
    } else {
        score + line_ply as Score
    }
}

/// How often the table hands back a score that depended on the path taken
/// rather than on the position: the graph history interaction error. The
/// figures cover the whole search, quiescence included, since quiescence
/// stores real bounds and takes real cutoffs.
#[derive(Copy, Clone, Debug, Default)]
pub struct GhiCounters {
    /// Entries which landed carrying a draw tainted score.
    pub tainted_stores: u64,
    /// Entries which landed and whose score a probe could later cut on. A
    /// store that loses the replacement contest is not one.
    pub stores: u64,
    /// Probes that returned a score, cutting the search off.
    pub score_cutoffs: u64,
    /// Probes that returned a tainted score, which is the error itself: the
    /// stored draw was reachable by the path that stored it and may not be
    /// reachable by this one. Zero while the search refuses them.
    pub tainted_score_cutoffs: u64,
    /// Tainted results the search declined to offer the table under a
    /// policy that keeps only clean scores, counted before the replacement
    /// contest: some would have lost it anyway, so this is the policy's
    /// reach rather than exactly what the table went without.
    pub skipped_stores: u64,
    /// Probes that found a tainted score deep enough to cut and refused it,
    /// handing back the move alone. Counted in the branch where a trusting
    /// search takes its cutoff, so the two are the same event under each
    /// policy, though not the same count: the refusing search is the larger
    /// tree. Zero while the search trusts them; under the rule50 policy
    /// this counts its horizon refusals instead, tainted or not.
    pub refused_cutoffs: u64,
}

/// What the entry's thirty two bit key slice costs. A probe accepts an
/// entry when the slice matches, so two positions sharing a slice and an
/// index make the search read a stranger's entry as its own, about one
/// probe in a thousand million.
///
/// A run of the bench's size expects no false accepts at all, so the count
/// alone cannot tell a working instrument from a dead one. `comparisons`
/// gives the expectation, and `narrow_accepts` counts what narrower
/// signatures would have accepted over the same comparisons. A narrow
/// figure sitting on its own expectation says the rate scales as two to
/// the minus the width on this workload, which is what lets the thirty two
/// bit expectation be believed.
///
/// The audit is off in every path a game plays, so a run that was not
/// asked for one has nothing to report rather than zeroes.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SignatureCounters {
    /// Keyed lookups asked of the table, whatever asked for them.
    pub probes: u64,
    /// Probes the slice accepted, which is a hit as the table sees one.
    pub hits: u64,
    /// Live entries a probe compared its slice against whose full key
    /// belonged to another position, over the entries it really looked at.
    /// One chance in two to the width apiece: the denominator every
    /// expectation is drawn from.
    pub comparisons: u64,
    /// Accepted probes whose full key differed, so the entry belonged to
    /// another position.
    pub false_accepts: u64,
    /// False accepts the search then took a score from, which is the half
    /// that costs anything: a foreign move is turned away by the legality
    /// check, and a foreign score cuts a subtree that was never searched.
    pub false_accept_cutoffs: u64,
    /// Comparisons a narrower signature would have accepted and this one
    /// refused, one figure per width in `NARROW_WIDTHS`. A signature of
    /// that width would take these and the false accepts both, so its rate
    /// is the two added.
    ///
    /// The widths are cumulative, not a partition: an entry agreeing on
    /// twenty four low bits is counted under sixteen too.
    ///
    /// Counted and never acted on. A search running one of these widths
    /// would have stopped its scan at the first entry it accepted, which is
    /// a different tree.
    pub narrow_accepts: [u64; NARROW_WIDTHS.len()],
    /// Stores whose slice matched a foreign full key, so the store replaced
    /// another position's entry as this position's. Landed stores only: a
    /// store the depth contest turns away after comparing against a foreign
    /// entry's depth is a related cost, but nothing was evicted.
    pub aliased_evictions: u64,
}

/// The chances in which the signature the table runs accepts a foreign
/// entry: two to its thirty two bits.
const WIDE: f64 = 4_294_967_296.0;

/// The narrower signatures the audit counts beside the one it runs, in bits
/// and smallest first. Sixteen has enough counts at the bench's scale to be
/// read on its own; twenty four and twenty eight are what the signature
/// would keep if eight or four of its bits went to other metadata.
pub const NARROW_WIDTHS: [u32; 3] = [16, 24, 28];

impl SignatureCounters {
    /// Add another table's figures to these.
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
    /// produced over the comparisons this run made.
    pub fn expected_false_accepts(&self) -> f64 {
        self.comparisons as f64 / WIDE
    }

    /// The same for a narrow width, less the chance the whole slice agrees
    /// too.
    pub fn expected_narrow_accepts(&self, width: u32) -> f64 {
        // a width of thirty two or more would shift the one off the end of
        // the u64 as well as meaning nothing
        debug_assert!(width < 32, "a narrow width is under thirty two: {width}");
        let narrow = (1u64 << width) as f64;
        self.comparisons as f64 * (1.0 / narrow - 1.0 / WIDE)
    }

    /// Each narrow width with what it counted and what it expected, in
    /// `NARROW_WIDTHS` order.
    pub fn narrow(&self) -> impl Iterator<Item = (u32, u64, f64)> + '_ {
        NARROW_WIDTHS
            .into_iter()
            .zip(self.narrow_accepts)
            .map(|(width, counted)| (width, counted, self.expected_narrow_accepts(width)))
    }
}

/// The ground truth the table does not keep: the full key of every entry,
/// written whenever one lands. Allocated only when the bench asks for the
/// audit, so an ordinary search carries a null pointer and one predictable
/// branch a probe. The unused `static_eval` bytes are no use for this:
/// sixteen more bits of signature would still alias where sixty four
/// cannot.
#[derive(Debug)]
struct Audit {
    /// One key an entry, at the bucket's index times four plus the entry
    /// within the bucket. A slot never written holds zero, which nothing
    /// reads: the entry beside it has generation zero.
    keys: Box<[u64]>,
    /// A cell because a probe reads the table through a shared reference.
    /// It costs the table its `Sync`, which nothing asks of it: the search
    /// thread needs only `Send`, asserted below.
    counters: Cell<SignatureCounters>,
}

impl Audit {
    /// The keys for a table of this many entries, or none if there was not
    /// the memory.
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

/// The entry as the search sees it. The table packs it into an `Entry`.
#[derive(Copy, Clone, Debug)]
struct Pv {
    play: Play,
    score: Score,
    /// True if the score flowed from a repetition or fifty move draw below
    /// it, so it describes the path taken to this position and not the
    /// position. What the flag does not cover is under known limitations in
    /// `docs/ROADMAP.md`.
    tainted: bool,
    depth: u8,
    bound: Bound,
}

/// What the stored score means: the truth, a ceiling or a floor.
#[derive(Copy, Clone, Debug)]
#[repr(u8)]
enum Bound {
    Exact = 0,
    Upper = 1,
    Lower = 2,
    /// Written by nothing since quiescence began storing real bounds. The
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

/// What a probe found.
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
/// The key is kept only in part. The index is drawn from the top of the
/// key by multiply-shift, so the bottom carries what the index does not,
/// and thirty two bits of it leave one chance in four thousand million per
/// entry compared of taking another position's entry for this one: about
/// one probe in a thousand million. The whole key made the slot twenty four
/// bytes.
///
/// The flags byte holds the bound in its low two bits, the taint in the
/// third and the generation in the top five; generation zero is a slot
/// never written. Two bytes are set aside for the static evaluation, which
/// the shelved correction history arm (docs/ROADMAP.md) would store, so the
/// layout and every node count change once rather than twice.
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

// A probe reads one cache line for all four entries, and every pinned node
// count depends on how many slots a table of a given size holds.
const _: () = assert!(mem::size_of::<Entry>() == 16);
const _: () = assert!(mem::size_of::<Bucket>() == 64);
const _: () = assert!(mem::align_of::<Bucket>() == 64);

// The audit's cell is not `Sync`, and the table is not asked to be; the
// protocol moves an engine whole to the search thread, which needs `Send`.
const fn assert_send<T: Send>() {}
const _: () = assert_send::<TranspositionTable>();

/// An entry stored this many searches ago or more is replaced whatever its
/// depth: its score describes repetition and fifty move context the game
/// has moved past. Twelve is about the window the old ply rule gave: the
/// twenty ply cap plus three, with a side searching every other ply.
const STALE_AFTER_SEARCHES: u8 = 12;

/// The generations run from one to this and round again, so generation
/// zero reads as empty. Ages are taken modulo this, so an entry from
/// thirty one searches ago or more reads as recent again and holds its slot
/// by depth until it ages out again. Nothing wrong is read, since a hit is
/// keyed; a slot is only held longer than it should be.
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

/// Four entries in one cache line, so four positions that hash alike are
/// kept and a probe still touches one line.
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

/// Asking the kernel to back the table with huge pages.
///
/// Every probe that misses the translation buffer pays a page walk before
/// its cache miss. A 256MB table is 65,536 pages of 4KiB against a second
/// level buffer of about fifteen hundred entries, so nearly every probe
/// walks; at 2MiB a page it is 128 entries. The commit that added this has
/// the measurements. The advice is given once, when the table is
/// allocated.
///
/// Linux only. macOS has no advice for this and Windows wants a privilege
/// the process does not hold, so `advise` is nothing there.
#[cfg(target_os = "linux")]
mod huge_pages {
    use super::Bucket;
    use std::ffi::c_void;
    use std::mem;

    /// A huge page on a kernel with 4KiB base pages, which is what the linux
    /// builds here run on. A kernel with larger base pages has larger huge
    /// pages too; the advice still lands there, since the start is base page
    /// aligned, and only the trimming below is at the wrong granularity.
    const HUGE_PAGE: usize = 2 * 1024 * 1024;

    /// The reservation the advice is not given below. glibc answers a request
    /// under its mmap threshold out of the general heap, and that threshold
    /// rises as far as 32MB on its own as large blocks are freed. Advising a
    /// table served from the heap marks the heap, which stays marked after
    /// the table is dropped. Above the threshold the table has a mapping of
    /// its own. An operator who raises the threshold by hand can put a larger
    /// table on the heap, and the advice then marks heap memory the process
    /// still maps, which is untidy and not unsound. The bound gives up
    /// nothing measurable: the change at 16MB was inside the spread, and the
    /// table an engine plays with is `DEFAULT_TABLE_BYTES`.
    const ADVISE_ABOVE: usize = 32 * 1024 * 1024;

    /// The advice, declared here rather than through the `libc` crate: std
    /// already links libc on linux, the signature is POSIX, and the value is
    /// `asm-generic/mman-common.h`'s, which every architecture rust targets
    /// uses (parisc had its own until 6.2, and rust has no parisc target).
    const MADV_HUGEPAGE: i32 = 14;
    unsafe extern "C" {
        fn madvise(addr: *mut c_void, length: usize, advice: i32) -> i32;
    }

    /// The huge page aligned interior of a range starting at `address` and
    /// running `bytes`, as an offset from that address and a length, or
    /// none when the range holds no whole huge page. `madvise` reads an
    /// unaligned start as the page it falls in, so the whole range would
    /// name a page the table does not own; a `Vec`'s buffer is aligned to
    /// its element and to nothing the kernel cares about. A 256MB table
    /// gives up under 2MiB of itself this way.
    fn interior(address: usize, bytes: usize) -> Option<(usize, usize)> {
        let first = address.checked_next_multiple_of(HUGE_PAGE)?;
        let end = address.checked_add(bytes)? / HUGE_PAGE * HUGE_PAGE;
        (end > first).then(|| (first - address, end - first))
    }

    /// Advise the reservation at `buffer`, `buckets` long. Called after the
    /// buffer is reserved and before it is written, so the pages fault in
    /// huge rather than being collapsed later. Failure is not an error: a
    /// kernel built without transparent huge pages, or one with them set to
    /// `never`, refuses the advice and nothing changes. With `defrag` set to
    /// `madvise` as well a fragmented host may compact before the fault, so
    /// a large table can be slower to come up there.
    pub(super) fn advise(buffer: *mut Bucket, buckets: usize) {
        let bytes = buckets * mem::size_of::<Bucket>();
        if bytes <= ADVISE_ABOVE {
            return;
        }
        let Some((offset, length)) = interior(buffer.addr(), bytes) else {
            return;
        };
        // SAFETY: the offset and the length stay inside the `bytes` the
        // caller reserved, which the test
        // `the_advised_range_stays_inside_the_allocation` holds, so `add`
        // stays inside one allocation. `madvise`
        // neither reads nor writes the range and cannot move it, so the
        // buffer is the same buffer afterwards and the `Vec` still owns it.
        // A refused advice leaves the mapping alone, so the answer is not
        // read.
        unsafe {
            madvise(
                buffer.cast::<u8>().add(offset).cast(),
                length,
                MADV_HUGEPAGE,
            );
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{HUGE_PAGE, interior};

        /// The arithmetic the advice is built on, which is the half of it
        /// a test can check.
        #[test]
        fn the_advised_range_is_the_aligned_interior() {
            // an aligned range of whole huge pages is asked for entire
            assert_eq!(interior(HUGE_PAGE, 4 * HUGE_PAGE), Some((0, 4 * HUGE_PAGE)));
            // an unaligned one gives up the part page at each end
            assert_eq!(
                interior(HUGE_PAGE + 64, 4 * HUGE_PAGE),
                Some((HUGE_PAGE - 64, 3 * HUGE_PAGE))
            );
            // a range too short to hold a whole page has no interior,
            // aligned or not
            assert_eq!(interior(HUGE_PAGE, HUGE_PAGE - 1), None);
            assert_eq!(interior(HUGE_PAGE + 64, HUGE_PAGE), None);
            assert_eq!(interior(0, 0), None);
            // and neither end is allowed to wrap
            assert_eq!(interior(usize::MAX - 63, 64), None);
            assert_eq!(interior(HUGE_PAGE, usize::MAX), None);
        }

        /// What the SAFETY note on the call claims: every advised range lies
        /// inside the range it was asked about, and starts on a huge page.
        #[test]
        fn the_advised_range_stays_inside_the_allocation() {
            for address in [0, 1, 64, HUGE_PAGE - 1, HUGE_PAGE, 3 * HUGE_PAGE + 4096] {
                for bytes in [0, 1, 4096, HUGE_PAGE, 5 * HUGE_PAGE + 17] {
                    let Some((offset, length)) = interior(address, bytes) else {
                        continue;
                    };
                    assert!(
                        offset + length <= bytes,
                        "{address}+{bytes} advised {offset}+{length}"
                    );
                    assert_eq!((address + offset) % HUGE_PAGE, 0);
                }
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod huge_pages {
    pub(super) fn advise(_buffer: *mut super::Bucket, _buckets: usize) {}
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

/// The sizes `up_to_bytes` tries, largest first: the size asked for, then
/// half of it each time, ending at one bucket, since `with_capacity` rounds
/// every smaller size up to one.
fn halving(bytes: usize) -> impl Iterator<Item = usize> {
    std::iter::successors(Some(bytes), |&bytes| {
        (bytes > mem::size_of::<Bucket>()).then_some(bytes / 2)
    })
}

/// What `build` made from the first size in the chain it answered to, and
/// `bytes` again when that was not the size asked for. Whether the chain
/// stepped down is read off the size it stopped at, since whole buckets
/// round an odd ask up.
///
/// Separate from `up_to_bytes` so that the step down can be tested without
/// the allocator: halving a size no host can meet reaches one the kernel
/// grants on paper long before one the machine has, and the test would be
/// killed writing the buckets rather than failing.
fn largest<T>(
    bytes: usize,
    mut build: impl FnMut(usize) -> Option<T>,
) -> Option<(T, Option<usize>)> {
    halving(bytes)
        .find_map(|size| build(size).map(|built| (built, (size < bytes).then_some(bytes))))
}

impl TranspositionTable {
    /// A table of at least this many entries, rounded up to whole buckets,
    /// or None if there was not the memory. The buckets are asked for
    /// rather than taken, because a size too large for the machine can
    /// arrive over the protocol and the allocator's answer to that is to
    /// abort.
    fn with_capacity(capacity: usize) -> Option<Self> {
        let buckets = capacity.div_ceil(BUCKET).max(1);
        let mut table = Vec::new();
        table.try_reserve_exact(buckets).ok()?;
        huge_pages::advise(table.as_mut_ptr(), table.capacity());
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
    /// signature accepted can be held against the position it stands for.
    /// Only the bench family calls it.
    ///
    /// The table is cleared as the keys go on, since an entry stored before
    /// the audit has no key on the side and would read as a stranger's.
    ///
    /// Eight bytes an entry beside the entry's sixteen. False if there was
    /// not the memory, in which case the table is left as it was and
    /// unaudited. No probe or store reads a different entry for having
    /// been counted.
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

    /// The table an engine is built with when a bench, an instrument or a
    /// test names a size. Failing to allocate it is fatal, because the size
    /// is part of what the run means and a smaller one would answer a
    /// different question. A size set over the protocol goes through
    /// `with_capacity_bytes` instead, and a refusal keeps the old table.
    pub fn of_bytes(bytes: usize) -> Self {
        Self::with_capacity_bytes(bytes)
            .unwrap_or_else(|| panic!("no memory for a {bytes} byte transposition table"))
    }

    /// The largest table up to `bytes` the host will give, which is the
    /// table a session starts with, and `bytes` again when that is not the
    /// size it got. The protocol cannot shrink the table until it exists,
    /// so without this a host with less memory than the default would not
    /// start at all. The ask is halved until the allocator answers, down to
    /// a single bucket.
    ///
    /// Where the kernel overcommits, a size larger than the host has is
    /// granted here and the process killed later as the buckets are
    /// written. This does not catch that.
    pub fn up_to_bytes(bytes: usize) -> (Self, Option<usize>) {
        largest(bytes, Self::with_capacity_bytes).expect("a table of one bucket")
    }

    /// The bytes the buckets occupy: the ask rounded up to whole buckets.
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
        // multiply-shift: onto 0..len without a 64 bit division
        (((key as u128) * (self.table.len() as u128)) >> 64) as usize
    }

    fn get(&self, key: u64) -> Option<Pv> {
        self.get_audited(key).0
    }

    /// The same lookup, saying as well whether the entry it accepted
    /// belongs to another position. Always false without the audit.
    #[inline(always)]
    fn get_audited(&self, key: u64) -> (Option<Pv>, bool) {
        let index = self.index_for(key);
        let slice = Entry::slice(key);
        let bucket = &self.table[index].entries;
        let found = bucket
            .iter()
            .enumerate()
            // the key first: in a warm table nearly every entry has a
            // generation, and the key is what rejects the others
            .find(|(_, entry)| entry.key == slice && entry.generation() != 0);
        let pv = found.map(|(_, entry)| entry.unpack());
        let Some(audit) = self.audit.as_deref() else {
            return (pv, false);
        };
        // the entries the scan looked at, up to and including the one it
        // took, rather than an assumed four
        let examined = found.map_or(BUCKET, |(i, _)| i + 1);
        let mut comparisons = 0;
        let mut narrow = [0; NARROW_WIDTHS.len()];
        for (i, entry) in bucket[..examined].iter().enumerate() {
            if entry.generation() == 0 || audit.keys[index * BUCKET + i] == key {
                continue;
            }
            comparisons += 1;
            // the agreeing low bits are those below the first that differs,
            // so one count settles every width
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
    /// Counted and nothing else.
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
    /// else an empty one, else a stale one, else the shallowest. This and
    /// the depth contest in `set` are the whole replacement policy.
    #[inline]
    fn slot_for(&self, key: u64) -> (usize, usize) {
        let index = self.index_for(key);
        let slice = Entry::slice(key);
        let bucket = &self.table[index].entries;
        // its own entry first, so a position is never in a bucket twice
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

    /// Store unless the slot holds something worth more. Reports whether
    /// the entry landed.
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

    /// Write the entry, and under the audit record its full key and count
    /// an aliased eviction.
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

    /// Store without the depth contest. For the root's end-of-iteration
    /// entry: it names the move about to be answered with, and the reported
    /// line is read back from its slot, so an entry a deeper search left
    /// there earlier in the game must not outrank it. When one did, the
    /// engine answered one move while its line opened with another.
    fn set_always(&mut self, key: u64, pv: Pv) {
        let (index, i) = self.slot_for(key);
        self.store(index, i, key, pv);
    }

    /// A move the search failed high on: the score is a floor under the
    /// position's worth, fail soft, so at least as tight as the beta it
    /// crossed.
    pub fn record_cutoff(&mut self, board: &Board, play: Play, floor: Value, depth: u8) {
        if self.set(board.key, entry(board, play, floor, depth, Bound::Lower)) {
            self.count_store(floor.tainted);
        }
    }

    /// Every move here fell short of the window: the score is a ceiling,
    /// and the move is the one that came closest, worth trying first next
    /// time though it proved nothing.
    pub fn record_ceiling(&mut self, board: &Board, play: Play, ceiling: Value, depth: u8) {
        if self.set(board.key, entry(board, play, ceiling, depth, Bound::Upper)) {
            self.count_store(ceiling.tainted);
        }
    }

    /// The best move found by searching all of them here, with its exact
    /// score.
    pub fn record_best(&mut self, board: &Board, play: Play, score: Value, depth: u8) {
        if self.set(board.key, entry(board, play, score, depth, Bound::Exact)) {
            self.count_store(score.tainted);
        }
    }

    /// The move the engine is about to answer with, stored past the depth
    /// contest for the reason `set_always` gives.
    pub fn record_answer(&mut self, board: &Board, play: Play, score: Value, depth: u8) {
        self.set_always(board.key, entry(board, play, score, depth, Bound::Exact));
        self.count_store(score.tainted);
    }

    /// The move a root iteration failed high on, stored past the depth
    /// contest as the floor it is, so the wider re-search orders it first.
    pub fn record_floor_answer(&mut self, board: &Board, play: Play, floor: Value, depth: u8) {
        self.set_always(board.key, entry(board, play, floor, depth, Bound::Lower));
        self.count_store(floor.tainted);
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

    /// What the table knows about this position, given the window and
    /// depth the caller is searching to. A score is handed back only when
    /// the entry is deep enough, its bound allows a cutoff at the window,
    /// and the policy (`refuse_tainted`, `guard_rule50`) trusts it.
    #[inline(always)]
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
                // or not
                self.ghi.refused_cutoffs += 1;
                return Probe::Order(pv.play);
            }
            if cuts && refuse_tainted && pv.tainted {
                // the stored draw may not be reachable by this path
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

    /// The move to try first here, whatever wrote it, quiescence included.
    #[inline]
    pub fn ordering_play(&self, board: &Board) -> Option<Play> {
        self.get(board.key).map(|pv| pv.play)
    }

    /// The move the table says is meant here, for reporting a line. Unlike
    /// `ordering_play` this refuses a depth zero entry: quiescence wrote it
    /// from a tree of captures alone, and it does not say what the engine
    /// intends to play.
    pub fn intended_play(&self, board: &Board) -> Option<Play> {
        let pv = self.get(board.key)?;
        (pv.depth > 0 && !matches!(pv.bound, Bound::Ordering)).then_some(pv.play)
    }

    /// How much of what the table handed back depended on the path taken.
    pub fn ghi(&self) -> GhiCounters {
        self.ghi
    }
}

/// Fold a position and a result into an entry, converting the score to the
/// table's form so no caller has to.
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
    use super::{
        Bound, Bucket, DEFAULT_TABLE_BYTES, NARROW_WIDTHS, Play, Pv, STALE_AFTER_SEARCHES, Score,
        TranspositionTable, Value, halving, largest,
    };
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
    fn the_sizes_a_session_falls_back_through_end_at_one_bucket() {
        let bucket = mem::size_of::<Bucket>();
        assert_eq!(
            halving(8 * bucket).collect::<Vec<_>>(),
            vec![512, 256, 128, 64]
        );
        // an ask already at a bucket or under it has nowhere to step down to
        assert_eq!(halving(bucket).collect::<Vec<_>>(), vec![64]);
        assert_eq!(halving(0).collect::<Vec<_>>(), vec![0]);

        // the default is the only size anything asks for this way
        let sizes: Vec<usize> = halving(DEFAULT_TABLE_BYTES).collect();
        assert_eq!(sizes[0], DEFAULT_TABLE_BYTES);
        assert_eq!(sizes[1], DEFAULT_TABLE_BYTES / 2);
        assert_eq!(sizes.last(), Some(&bucket));
    }

    #[test]
    fn a_size_the_host_refuses_is_halved_until_one_is_taken() {
        // the step down without an allocator, which is why `largest` takes
        // the builder: this host refuses everything above a kibibyte
        let mut tried = Vec::new();
        let took = largest(8 * 1024, |bytes| {
            tried.push(bytes);
            (bytes <= 1024).then_some(bytes)
        });
        // what it took, and the ask it could not have
        assert_eq!(took, Some((1024, Some(8 * 1024))));
        assert_eq!(tried, vec![8 * 1024, 4 * 1024, 2 * 1024, 1024]);

        // a host that refuses nothing steps nowhere and has nothing to say
        assert_eq!(largest(8 * 1024, Some), Some((8 * 1024, None)));

        // a host that refuses everything, which `up_to_bytes` says cannot
        // happen for the last size in the chain
        assert_eq!(largest(8 * 1024, |_| None::<usize>), None);
    }

    #[test]
    fn a_session_table_is_the_size_asked_for_when_the_host_has_it() {
        let (table, asked) = TranspositionTable::up_to_bytes(1024 * 1024);
        assert_eq!(table.bytes(), 1024 * 1024);
        assert_eq!(asked, None);

        // under a bucket is still a table, and still the size it asked for
        let (table, asked) = TranspositionTable::up_to_bytes(0);
        assert!(table.bytes() > 0);
        assert_eq!(asked, None);
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
        // each field at the edge of its range, for every kind of bound
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
        // a probe whose alpha the ceiling cannot reach cuts on it, and one
        // whose window the ceiling sits inside must still search
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
    fn a_mate_score_is_stored_relative_to_its_own_node() {
        // every other test here works at ply zero, where both conversions
        // are the identity, so flipping the sign of `score_from_tt` would
        // leave them all passing. A mate stored nine plies down the line
        // and read back two plies down has to come back seven plies nearer
        // the root, and an ordinary score is not touched either way
        use super::Probe;
        const STORED_AT: usize = 9;
        const PROBED_AT: usize = 2;
        const TO_MATE: usize = 3;

        let play = Play::new(0, 1, None, None, false, false);
        for (stored, wanted, what) in [
            (
                Value::mated(STORED_AT + TO_MATE),
                Value::mated(PROBED_AT + TO_MATE),
                "a mate against the side to move",
            ),
            (
                -Value::mated(STORED_AT + TO_MATE),
                -Value::mated(PROBED_AT + TO_MATE),
                "a mate for the side to move",
            ),
            (
                Value::clean(50),
                Value::clean(50),
                "an ordinary score, which no ply moves",
            ),
        ] {
            // a table of its own, so the three stores never meet in a slot
            let mut table = TranspositionTable::with_capacity(4).expect("a table of a few buckets");
            let mut board = crate::board::Board::new();

            board.line_ply = STORED_AT;
            table.record_best(&board, play, stored, 5);

            board.line_ply = PROBED_AT;
            match table.probe(&board, Score::MIN + 1, Score::MAX - 1, 5, false, false) {
                Probe::Cut(value) => assert_eq!(value.score, wanted.score, "{what}"),
                other => panic!("{what} did not cut: {other:?}"),
            }
        }
    }

    #[test]
    fn the_rule50_guard_refuses_any_cutoff_at_the_horizon() {
        // a deep clean entry cuts from a fresh position and is refused
        // move-only once the counter stands at the guard, with the refusal
        // counted
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
        // the graph history figures count what lands, and ride on set's
        // answer
        let mut table = full_bucket(8);
        assert!(!table.set(5, new_pv(Bound::Lower, 1)));
        assert!(table.set(5, new_pv(Bound::Lower, 9)));
    }

    #[test]
    fn a_turned_away_store_is_not_counted() {
        // the shallow cutoff is turned away and must leave the figures
        // alone; deeper, it lands and counts
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
        // a bucket filled in the last generation before the wrap is one
        // search old in the first after it, not thirty
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
        // the accepted imprecision: in a one bucket table every key shares
        // the index, so one differing only above the slice is a hit
        let mut table = TranspositionTable::with_capacity(4).expect("a table of a few buckets");
        table.set(1, new_pv(Bound::Exact, 8));
        assert_eq!(table.get(1 + (1 << 32)).unwrap().depth, 8);
        assert!(table.get(2).is_none());
    }

    /// A table with no audit has nothing to report rather than zeroes.
    #[test]
    fn a_table_keeps_no_keys_until_the_audit_asks_for_them() {
        let mut table = TranspositionTable::with_capacity(4).expect("a table of one bucket");
        table.set(1, new_pv(Bound::Exact, 4));
        assert!(table.get(1).is_some());
        assert!(table.signatures().is_none());
        assert!(table.audit_signatures());
        assert_eq!(table.signatures().expect("audited"), Default::default());
        // an entry stored before the audit has no key on the side, so the
        // audit starts the table empty
        assert!(table.get(1).is_none(), "the audit left an entry behind");
        assert_eq!(table.signatures().expect("audited").probes, 1);
        assert_eq!(table.signatures().expect("audited").false_accepts, 0);
    }

    /// Two keys agreeing on the slice (the low thirty two bits) and, in a
    /// one bucket table, the index. The probe is a hit the table cannot
    /// tell from a real one, and the store that follows takes the first
    /// position's entry for the second's.
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
        // this signature took the entry, so it belongs to the false accepts
        // and to no narrow width: a reader adding the two must not count it
        // twice
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

    /// The same index with a slice of its own: the probe misses and nothing
    /// is counted against the signature. The comparison is still counted,
    /// since the expectation is drawn from comparisons against live foreign
    /// entries.
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

    /// Keys agreeing on the low bits of a width and differing on the first
    /// bit above them: the probe misses, and the key counts under the width
    /// it was built for and every narrower one, and under no wider one.
    /// Each width is tried from both sides, since a key agreeing on one bit
    /// fewer has to be refused, which puts the boundary where the width
    /// says.
    #[test]
    fn an_acceptance_a_narrower_signature_would_have_made_is_counted() {
        for built_for in NARROW_WIDTHS {
            let mut table = TranspositionTable::with_capacity(4).expect("a table of one bucket");
            assert!(table.audit_signatures());
            table.set(1, new_pv(Bound::Exact, 4));
            // the agreement stops exactly at the width
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
            // one bit short of the width: this width has to refuse it and
            // every narrower one has to take it
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

    /// One key agreeing on each width in turn, so the widest is counted
    /// once, the middle twice and the narrowest three times: the ordering
    /// is checked on three distinct counts rather than three zeroes.
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
        // a width takes every probe built for it or for a wider one
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
        // the counts differ, so the ordering below is not three zeroes
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

    /// A foreign entry deep enough to cut is a subtree answered from
    /// another position's score, and the probe hands it back exactly as it
    /// would without the audit. Counting it is all detection does.
    #[test]
    fn a_score_taken_from_a_foreign_entry_is_counted_as_a_cutoff() {
        use super::Probe;
        let mut table = TranspositionTable::with_capacity(4).expect("a table of one bucket");
        assert!(table.audit_signatures());
        let board = crate::board::Board::new();
        let play = Play::new(0, 1, None, None, false, false);
        table.record_best(&board, play, Value::clean(20), 5);
        // a key differing above the slice, sharing the one bucket's index
        let mut twin = board.clone();
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
