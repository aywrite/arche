// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What the search's shortcuts cost in accuracy.
//!
//! A shortcut answers a node from something cheaper than searching it: a
//! margin above beta, or a pass searched shallow. Both are guesses, and the
//! question this module asks is how far off the guesses are.
//!
//! The error that counts is the decision and not the score. A shortcut
//! returns a lower bound and claims it clears beta, so a claim well above
//! what the position is worth is still a sound fail high as long as the
//! reference agrees the node fails high. What is not sound is a reference
//! answer below beta: the node was cut off and should not have been. That is
//! the crossing, the primary label on every row here. The residual, the
//! reference's answer less the claim, stays beside it as the size of the
//! error the crossings are drawn from.
//!
//! The sampler is the recording half. It hangs off an engine as an option,
//! and an engine without one searches exactly the tree it searched before
//! there was a sampler at all, which is what the pinned bench counts say.
//! The replaying half lives beside it and runs after the search, never
//! during: see the module's `run`.

use crate::bench::{self, Position};
use crate::board::Board;
use crate::engine::{AlphaBeta, Engine, SearchConfig, SearchOutcome, SearchParameters};
use crate::misc::Score;
use std::collections::BinaryHeap;
use std::fmt;

/// The shortcut that answered a node, or the shadow lane that watched one
/// it could have.
///
/// The first two are the shortcuts the default configuration turns on.
/// Another shortcut would be one arm here and one call at wherever it
/// returns.
///
/// The shadow kind answers nothing. It records every reverse futility
/// candidate, a node where the eval stood at or above beta with the other
/// gates passed, whether or not the margin test then fired. The live rows
/// cannot price a tighter margin on their own: every one of them stood a
/// whole margin above beta, so the region a tighter margin would newly
/// fire on has no data. The shadow rows are that population whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    /// The node stood far enough above beta by its static evaluation alone.
    ReverseFutility,
    /// A reduced search of the position left by passing came back above beta.
    NullMove,
    /// A reverse futility candidate, fired or not.
    ShadowFutility,
}

impl Shortcut {
    /// The kinds, for a reader summarising by them.
    pub const KINDS: [Shortcut; 3] = [
        Shortcut::ReverseFutility,
        Shortcut::NullMove,
        Shortcut::ShadowFutility,
    ];

    /// The word a row prints. For the live kinds it names the
    /// `SearchConfig` switch that turns the shortcut on; the shadow kind
    /// has no switch and is named for the shortcut it watches.
    pub fn word(self) -> &'static str {
        match self {
            Shortcut::ReverseFutility => "reverse_futility",
            Shortcut::NullMove => "null_move",
            Shortcut::ShadowFutility => "shadow_futility",
        }
    }

    /// What the kind contributes to a sampling key. Arbitrary constants
    /// far apart in their bits, so each kind keeps its own uniform draw
    /// of its own events. The draws are not joint: the salts differ high
    /// in the word, so at any rate coarser than one in four the same
    /// position and depth is never kept under two kinds at once.
    fn salt(self) -> u64 {
        match self {
            Shortcut::ReverseFutility => 0x51ed_2701_c3f8_4d95,
            Shortcut::NullMove => 0xa24b_af09_7d16_e8c3,
            Shortcut::ShadowFutility => 0x38c6_54da_0b9e_7f12,
        }
    }
}

/// The window a node was searched with, as far as a sample can know it.
///
/// A zero width window asks whether the position beats one score; a wider
/// one asks what it is worth. The two are different questions and a shortcut
/// answering them wrongly costs different things, so the rows are filtered
/// by this. It is read from alpha and beta at the sample and nothing else.
/// The fuller pv, cut and all classification needs the node's outcome, which
/// a sample taken at a cutoff cannot know, so this is what is knowable here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    /// Beta is one above alpha: the node was asked a yes or no question.
    Zero,
    /// Anything wider.
    Open,
}

impl Window {
    /// The window a node with these bounds was searched with.
    pub fn of(alpha: Score, beta: Score) -> Self {
        if i32::from(beta) - i32::from(alpha) <= 1 {
            Window::Zero
        } else {
            Window::Open
        }
    }

    /// The word a row prints.
    pub fn word(self) -> &'static str {
        match self {
            Window::Zero => "zw",
            Window::Open => "open",
        }
    }
}

/// The odd multiplier the depth is spread by before it joins the key. Odd,
/// so multiplying by it loses no bits, and the fractional part of the golden
/// ratio, which is the usual choice for a constant with no structure the
/// position key could share.
const DEPTH_SPREAD: u64 = 0x9e37_79b9_7f4a_7c15;

/// The key a shortcut's answer at this node is sampled by.
///
/// A function of the node and nothing about the run, so the same node
/// answered by the same shortcut at the same depth is sampled or not sampled
/// whatever order the search reached it in. That is the whole point: a
/// counter over the stream picks nodes by when they were visited, and a
/// change to the tree then moves the membership in ways that read as a shift
/// in the distribution.
pub fn sample_key(position_key: u64, kind: Shortcut, depth: u8) -> u64 {
    position_key ^ kind.salt() ^ u64::from(depth).wrapping_mul(DEPTH_SPREAD)
}

/// One node a shortcut answered, with enough of the node to search it again.
///
/// Everything is owned. A sample outlives the search that took it, and the
/// board it was taken from has moved on by then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    /// The position, as the board prints one. The fifty move counter travels
    /// in it; the path does not, and see `run` for what that costs.
    pub fen: String,
    /// The depth the node had left to search, which is the depth the
    /// shortcut was trusted over and so the depth the replay searches to.
    pub depth: u8,
    pub kind: Shortcut,
    /// The score the shortcut returned for the node.
    pub claimed: Score,
    /// The beta the shortcut cleared, which is what the reference's answer
    /// is held against. Without it a row says how wrong the claim was and
    /// not whether the cutoff was earned.
    pub beta: Score,
    /// How far the static evaluation stood above beta when the shortcut
    /// fired, which is the margin each of them is really betting on. Widened
    /// past a `Score` because the difference of two of them is not one.
    pub eval_beta: i32,
    pub window: Window,
    /// The fifty move counter at the node. It travels in the fen too, and it
    /// is pulled out here so rows sort and filter on it without anything
    /// parsing a fen to find it: a residual from a position deep in a
    /// shuffle is read differently from one in a middlegame.
    pub halfmove: usize,
}

/// A held sample and the key it was drawn by.
///
/// The key is the sampler's business rather than the node's, so it lives
/// here and not in the `Sample` a reader gets. Ordered by the key alone,
/// which is what makes a heap of these the reservoir below.
#[derive(Clone, Debug)]
struct Kept {
    key: u64,
    sample: Sample,
}

// The four below are key-only on purpose. The reservoir ranks records by
// their key and by nothing else, and two records that key alike are
// interchangeable to it, so the sample they carry is deliberately not part
// of the comparison. They are written out rather than derived because a
// derive would order by the sample too and quietly make the heap depend on
// what a fen sorts like.
impl Ord for Kept {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}

impl PartialOrd for Kept {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Kept {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for Kept {}

/// What a sampler collected, taken away from it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sampled {
    pub taken: Vec<Sample>,
    /// Every node offered to the sampler, kept or not. The denominator: a
    /// run that recorded no crossings has said nothing until this says how
    /// many chances it had to record one.
    pub events: u64,
    /// Samples the buffer had no room for. Counted rather than kept, so a
    /// long run says how much of itself it is not describing.
    pub overflowed: u64,
}

/// Records about one node in every n a shortcut answers, picked by the key
/// of the node rather than by its place in the stream.
///
/// Deterministic twice over. Two runs of the same search record the same
/// nodes, so a distribution can be reproduced from the command that printed
/// it; and the nodes recorded do not depend on the order the search reached
/// them in, so a change that reorders the tree without changing which nodes
/// are in it samples the same nodes.
#[derive(Clone, Debug)]
pub struct Sampler {
    /// The largest key kept, which is the rate in the form the events are
    /// tested against. A key is spread over the whole range, so a share of
    /// one in `every` of them sits at or below this. The rate itself is not
    /// held: the report prints the one the run was asked for.
    threshold: u64,
    /// The most samples the buffer will hold. A cap rather than a growing
    /// vector: a run at a low rate over a deep search would otherwise ask
    /// for gigabytes of fens.
    cap: usize,
    /// What is held, as a heap on the key so the largest is the one at hand
    /// to give up. See `event` for why the largest is the one to give up.
    kept: BinaryHeap<Kept>,
    /// Every node offered, whatever became of it.
    events: u64,
    overflowed: u64,
}

impl Sampler {
    /// What a sampler holds when nothing says otherwise. Ten thousand fens
    /// is a megabyte or so; a calibration run that wants more of the tree
    /// than that asks the residuals command for a larger cap.
    pub const DEFAULT_CAP: usize = 10_000;

    /// Records about one node in every `every`.
    pub fn every(every: u32) -> Self {
        Self::with_cap(every, Self::DEFAULT_CAP)
    }

    /// The same, holding at most `cap` samples. One sampler is meant to be
    /// carried across a whole run of searches, so the cap it is built with
    /// bounds the run rather than any one search in it, and what survives
    /// the cap is drawn from the whole run rather than from its start.
    pub fn with_cap(every: u32, cap: usize) -> Self {
        Self {
            // held at one or more, so a rate of zero records everything
            // rather than dividing by nothing. At or below the threshold
            // rather than below it, so that a rate of one really keeps every
            // event rather than every event but the one key
            threshold: u64::MAX / u64::from(every.max(1)),
            cap,
            kept: BinaryHeap::new(),
            events: 0,
            overflowed: 0,
        }
    }

    /// Offer one node, keyed. The key decides whether it is wanted at all,
    /// and then whether it beats what the cap is already holding.
    ///
    /// At the cap the record with the largest key is the one given up, so
    /// what is left at the end is the `cap` smallest keys of the run. Those
    /// are a uniform draw from the whole run: the key says nothing about
    /// when the node was reached, so taking the smallest of them is taking
    /// an arbitrary fixed share, and it is the same share whichever order
    /// the events arrived in. Keeping the first arrivals instead would
    /// describe the first position of a suite and call it the suite.
    ///
    /// That holds up to ties, and the ties are not rare. A key names a
    /// position, a kind and a depth, and a deepening search revisits all
    /// three, so a run keys many events alike; the samples behind them are
    /// not interchangeable, since the beta and the window at a revisit are
    /// the node's second answer and not its first. Which member of a tied
    /// group survives the cap is whichever the heap happens to surface, not
    /// the earliest, and a run that offers the same events in another order
    /// can keep a different member of the same tie. The set of keys is
    /// order-independent; the samples behind a tied key are not.
    ///
    /// The sample arrives as a closure because building one prints a fen,
    /// and that is not worth doing for an event that is not kept.
    pub fn event(&mut self, key: u64, describe: impl FnOnce() -> Sample) {
        self.events += 1;
        if key > self.threshold {
            return;
        }
        if self.kept.len() < self.cap {
            self.kept.push(Kept {
                key,
                sample: describe(),
            });
            return;
        }
        // past the cap every wanted event costs one of them, whether it is
        // the new one or the one it displaces, so the count is the wanted
        // events the report is not describing
        self.overflowed += 1;
        let largest = match self.kept.peek() {
            Some(held) => held.key,
            // a cap of zero holds nothing and displaces nothing
            None => return,
        };
        // strictly smaller, so an equal key does not displace. That keeps
        // the set of keys right; which of a tied group is held is the heap's
        // business either way, and the doc above says so
        if key >= largest {
            return;
        }
        self.kept.pop();
        self.kept.push(Kept {
            key,
            sample: describe(),
        });
    }

    /// How many samples are held.
    pub fn len(&self) -> usize {
        self.kept.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kept.is_empty()
    }

    /// Everything collected, in key order, leaving the sampler empty and
    /// ready to record again. The event and overflow counts go with it: they
    /// describe the samples handed over and not the sampler.
    ///
    /// Key order rather than the order the run met them, because a heap does
    /// not remember the latter. It is an order and not a ranking: a key is a
    /// hash of the node, so a reader gets the rows shuffled. Two records
    /// that key alike come out in no order worth relying on, which is the
    /// one thing here that is not a property of the events alone.
    pub fn drain(&mut self) -> Sampled {
        Sampled {
            taken: std::mem::take(&mut self.kept)
                .into_sorted_vec()
                .into_iter()
                .map(|held| held.sample)
                .collect(),
            events: std::mem::replace(&mut self.events, 0),
            overflowed: std::mem::replace(&mut self.overflowed, 0),
        }
    }
}

/// One sample of the recording run with the reference's answer beside the
/// shortcut's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub kind: Shortcut,
    pub depth: u8,
    pub window: Window,
    pub halfmove: usize,
    pub beta: Score,
    pub eval_beta: i32,
    pub claimed: Score,
    /// What the reference search said about the position, searched to the
    /// depth the shortcut was trusted over.
    pub reference: Score,
    pub fen: String,
}

impl Row {
    /// The residual: the reference's answer less what the shortcut claimed.
    ///
    /// Negative is the shortcut claiming more than the search behind it
    /// found. It is the size of the error and not the error itself: a
    /// shortcut returns a lower bound, so claiming more than the position is
    /// worth costs nothing as long as the node still fails high. What it
    /// costs when the node does not is `crossed`.
    pub fn delta(&self) -> i32 {
        i32::from(self.reference) - i32::from(self.claimed)
    }

    /// Whether the reference's answer fell below the beta the shortcut
    /// cleared, which is the decision the shortcut got wrong: the node was
    /// answered with a cutoff the search behind it does not support.
    ///
    /// Strictly below. A reference answer equal to beta is a fail high the
    /// shortcut was entitled to take.
    pub fn crossed(&self) -> bool {
        self.reference < self.beta
    }

    /// The word a row prints for the crossing: what the reference did with
    /// the beta, said either way rather than by an empty column.
    pub fn crossing_word(&self) -> &'static str {
        if self.crossed() { "crossed" } else { "clear" }
    }

    /// Whether the reference answered with a forced mate.
    ///
    /// Such a row is a crossing question and nothing else. Its delta is not
    /// a number of pawns, and the mate distance the reference reports counts
    /// from the root of the replay while the claim's counts from the root of
    /// the recorded search, so the two are not even measured from the same
    /// place. The crossing survives all of that: a mate score is above every
    /// eval or below every eval, and which of the two is all the comparison
    /// with beta asks.
    pub fn reference_is_mate(&self) -> bool {
        crate::value::is_mate(self.reference)
    }
}

/// The residuals of one kind at one depth, as the run reports them.
///
/// By depth and not pooled over them. The shortcuts risk a margin that grows
/// with the depth left, and the depths are reached in wildly different
/// numbers, so a pooled rate is the shallowest depth's rate wearing every
/// depth's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    pub kind: Shortcut,
    pub depth: u8,
    /// Every row of the pair, which is what the crossing rate is a share of.
    pub count: usize,
    /// Rows whose reference answer fell below the beta that was cleared.
    pub crossed: usize,
    /// Rows the reference answered with a mate. Counted here and left out of
    /// the percentiles below, for the reasons `Row::reference_is_mate` gives.
    pub mates: usize,
    /// The delta percentiles over the rows that are not mates, or none when
    /// every row of the pair is one.
    pub deltas: Option<Deltas>,
}

/// What the residuals of one kind at one depth ran to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deltas {
    pub min: i32,
    pub median: i32,
    pub p90: i32,
    pub p99: i32,
    pub max: i32,
}

impl Summary {
    /// The share of the pair's rows that crossed, as a percentage. The count
    /// is never zero: a pair with no rows has no summary at all.
    pub fn crossing_rate(&self) -> f64 {
        100.0 * self.crossed as f64 / self.count as f64
    }
}

/// A whole run: what it was asked for, what it recorded, and a row a sample.
#[derive(Clone, Debug)]
pub struct Report {
    pub depth: u8,
    pub every: u32,
    /// The most samples the run would keep. Stated in the header only when
    /// it is not the default, the way the overflow it governs is.
    pub cap: usize,
    pub config: SearchConfig,
    /// Positions of the suite the recording run searched.
    pub positions: usize,
    /// Every node the shortcuts answered, whether or not the rate wanted it.
    /// The denominator the rows are read against: a crossing rate of zero
    /// over four hundred records and one over four hundred thousand events
    /// are not the same statement, and only this tells them apart.
    pub events: u64,
    /// Samples the buffer had no room for.
    pub overflowed: u64,
    /// Samples the replay could not put a reference value on, because the
    /// position as a diagram has no move to make. Counted rather than
    /// scored, so the rows below are all comparisons and the count says how
    /// many were not.
    pub unplayable: usize,
    pub rows: Vec<Row>,
}

/// About one sample in every this many events, unless the command says
/// otherwise. Low enough that a run at the bench's depth finishes in minutes
/// and high enough that it describes the whole suite rather than its first
/// position.
pub const DEFAULT_EVERY: u32 = 1_000;

/// The table the replay searches with. Small on purpose: it belongs to the
/// replay engine alone, it never sees the measured search's entries and the
/// measured search never sees its, and each record is a shallow search that
/// would not fill a larger one.
pub const REPLAY_TABLE_BYTES: usize = 4 * 1024 * 1024;

/// Record, then replay. Never both at once.
///
/// The recording run searches the suite under the configuration given, which
/// is the one the engine plays with, and samples the nodes its shortcuts
/// answer. The replay then asks the reference search what each of those
/// positions is worth. Re-searching during the first run would write
/// reference entries into the table the measured search is reading, and
/// change the very play being measured, so the two phases never overlap and
/// the replay owns its own engine and its own table.
pub fn run(
    positions: &[Position],
    depth: u8,
    every: u32,
    cap: usize,
    config: SearchConfig,
) -> Report {
    let depth = depth.max(1);
    // the rate the sampler will really keep to, so that the header states
    // the run that happened rather than the words it was asked in
    let every = every.max(1);
    let sampled = record(positions, depth, every, cap, config);
    let (rows, unplayable) = replay(&sampled.taken);
    Report {
        depth,
        every,
        cap,
        config,
        positions: positions.len(),
        events: sampled.events,
        overflowed: sampled.overflowed,
        unplayable,
        rows,
    }
}

/// The recording phase on its own: the suite searched under the
/// configuration given, with the nodes its shortcuts answered sampled.
///
/// One sampler for the whole suite, carried from each position's engine to
/// the next, so that the cap describes the run and not each position of it.
/// The rate would survive a sampler built afresh per position, since a key
/// decides on its own node, but the cap would not: eighteen caps of ten
/// thousand hold the first ten thousand of each position, and a reservoir
/// over the whole run holds a share of all of it.
pub fn record(
    positions: &[Position],
    depth: u8,
    every: u32,
    cap: usize,
    config: SearchConfig,
) -> crate::residual::Sampled {
    let mut sampler = Sampler::with_cap(every, cap);
    for position in positions {
        let board = Board::from_fen(&position.fen)
            .unwrap_or_else(|e| panic!("residual position {} does not parse: {}", position.id, e));
        let mut engine = AlphaBeta::with_config(board, bench::TABLE_BYTES, config);
        engine.sample_shortcuts(sampler);
        engine.iterative_deepening_search(SearchParameters::to_depth(depth), |_, _, _, _| {});
        sampler = engine
            .take_sampler()
            .expect("the sampler just handed to the engine comes back");
    }
    sampler.drain()
}

/// The engine the replay asks its questions of.
///
/// The reference and not the default, which is the premise the whole module
/// rests on: a shortcut cannot be the judge of what it cost, so the search
/// that answers the sampled positions is the one with both shortcuts off.
/// Built here rather than inline so a test can hold the replay to it.
fn replay_engine() -> AlphaBeta {
    AlphaBeta::with_config(Board::new(), REPLAY_TABLE_BYTES, SearchConfig::reference())
}

/// What the reference search says about each sampled position, and how many
/// of them it had nothing to say about.
///
/// One engine for the whole replay, with a table of its own that is cleared
/// before every sample. A probe compares the whole key, so an entry left by
/// a record about some other position is never read; an entry about this one
/// is, and that is what the clear is for. The same position sampled twice at
/// two depths would otherwise have its shallower record answered from the
/// deeper record's entry, and a reference value that depends on what the
/// replay searched before it is not a reference value. The cost is a table
/// wipe a sample, four megabytes of it, against a search that is orders
/// dearer.
///
/// The limitation, known and accepted: a fen carries the fifty move counter
/// and not the path. The replay cannot see a repetition that needs moves
/// made before the node, so what is measured is the reference's answer to
/// the position as a diagram. That is the simplification every epd suite
/// makes, and it is why a residual on a position deep in a shuffle is read
/// with more care than one from a middlegame.
pub fn replay(samples: &[Sample]) -> (Vec<Row>, usize) {
    let mut engine = replay_engine();
    let mut rows = Vec::with_capacity(samples.len());
    let mut unplayable = 0;
    for sample in samples {
        if engine.parse_fen(&sample.fen).is_err() {
            unplayable += 1;
            continue;
        }
        // cold for every sample, so no sample's answer is another's
        engine.clear_transpositions();
        let outcome = engine
            .iterative_deepening_search(SearchParameters::to_depth(sample.depth), |_, _, _, _| {});
        let SearchOutcome::Complete(result) = outcome else {
            // the position has no move to make: mate, stalemate, or drawn
            // already by the counter its fen carries. There is no reference
            // value to compare, so it is counted instead of scored
            unplayable += 1;
            continue;
        };
        rows.push(Row {
            kind: sample.kind,
            depth: sample.depth,
            window: sample.window,
            halfmove: sample.halfmove,
            beta: sample.beta,
            eval_beta: sample.eval_beta,
            claimed: sample.claimed,
            reference: result.score,
            fen: sample.fen.clone(),
        });
    }
    (rows, unplayable)
}

/// The value at a share of the sorted deltas, by nearest rank: the first one
/// at or past that share of them. The median is the fiftieth by the same
/// rule, which on an even count is the lower of the middle pair rather than
/// the mean of them, so every figure printed is a residual that happened.
fn percentile(sorted: &[i32], share: u64) -> i32 {
    let rank = ((sorted.len() as u64 * share).div_ceil(100)).max(1) as usize;
    sorted[rank.min(sorted.len()) - 1]
}

impl Report {
    /// One kind's residuals at one depth, or none when the run recorded no
    /// row of that pair.
    pub fn summary(&self, kind: Shortcut, depth: u8) -> Option<Summary> {
        let of_pair = || {
            self.rows
                .iter()
                .filter(|row| row.kind == kind && row.depth == depth)
        };
        let count = of_pair().count();
        if count == 0 {
            return None;
        }
        // the mates are counted and then set aside: a mate distance is not a
        // residual, and at a small count a handful of them own every
        // percentile above the median
        let mut deltas: Vec<i32> = of_pair()
            .filter(|row| !row.reference_is_mate())
            .map(Row::delta)
            .collect();
        deltas.sort_unstable();
        Some(Summary {
            kind,
            depth,
            count,
            crossed: of_pair().filter(|row| row.crossed()).count(),
            mates: of_pair().filter(|row| row.reference_is_mate()).count(),
            deltas: (!deltas.is_empty()).then(|| Deltas {
                min: deltas[0],
                median: percentile(&deltas, 50),
                p90: percentile(&deltas, 90),
                p99: percentile(&deltas, 99),
                max: deltas[deltas.len() - 1],
            }),
        })
    }

    /// Every summary the run has, a kind at a time and shallowest depth
    /// first. A kind with no rows at all contributes none, which is what the
    /// report prints its bare zero line from.
    pub fn summaries(&self, kind: Shortcut) -> Vec<Summary> {
        let mut depths: Vec<u8> = self
            .rows
            .iter()
            .filter(|row| row.kind == kind)
            .map(|row| row.depth)
            .collect();
        depths.sort_unstable();
        depths.dedup();
        depths
            .into_iter()
            .filter_map(|depth| self.summary(kind, depth))
            .collect()
    }
}

/// The report as the command prints it: a header naming what the run was
/// asked for and what it collected, a row a sample, and a summary line for
/// each kind at each depth.
///
/// A row is `kind depth window halfmove beta eval_beta claimed reference
/// delta crossed fen`, whitespace separated with the fen last, so it parses
/// left to right and the field that can hold spaces holds the rest of the
/// line.
///
/// The header states the events as well as the records, always. A run that
/// recorded no crossing has measured nothing until the reader knows how many
/// chances it had, and a rate low enough to finish in minutes can leave the
/// two counts three orders apart.
///
/// Three columns a reader will ask for are not here, each waiting on
/// something outside this module. Whether the side to move is improving
/// wants the per-ply eval stack a reduction scheme will bring. The table
/// entry's bound and depth at the node want probe plumbing that should be
/// designed once for whoever needs it rather than twice. Whether the node
/// stood on the line to the root wants lineage tracking, which is not a
/// thing to build on the chance a column wants it.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "residuals depth {} every {}", self.depth, self.every)?;
        // the default on every header would describe nothing; a run asked
        // for another cap states it, so the header still says how to rerun
        // the run it heads
        if self.cap != Sampler::DEFAULT_CAP {
            write!(f, " cap {}", self.cap)?;
        }
        write!(
            f,
            " taint {} positions {} events {} records {}",
            self.config.taint_word(),
            self.positions,
            self.events,
            self.rows.len(),
        )?;
        // both are absent from an ordinary run, and naming them anyway would
        // put two zeroes on every header to describe nothing
        if self.overflowed > 0 {
            write!(f, " overflow {}", self.overflowed)?;
        }
        if self.unplayable > 0 {
            write!(f, " unplayable {}", self.unplayable)?;
        }
        writeln!(f)?;
        for row in &self.rows {
            writeln!(
                f,
                "{} {} {} {} {} {} {} {} {} {} {}",
                row.kind.word(),
                row.depth,
                row.window.word(),
                row.halfmove,
                row.beta,
                row.eval_beta,
                row.claimed,
                row.reference,
                row.delta(),
                row.crossing_word(),
                row.fen,
            )?;
        }
        writeln!(f)?;
        writeln!(f, "summary")?;
        for kind in Shortcut::KINDS {
            let summaries = self.summaries(kind);
            // said rather than left out: a kind that recorded nothing is a
            // fact about the run
            if summaries.is_empty() {
                writeln!(f, "{} 0", kind.word())?;
            }
            for s in summaries {
                write!(
                    f,
                    "{} depth {} {} crossed {} rate {:.2}% mates {}",
                    kind.word(),
                    s.depth,
                    s.count,
                    s.crossed,
                    s.crossing_rate(),
                    s.mates,
                )?;
                // absent when every row of the pair was a mate, since there
                // is then no residual to take a percentile of
                if let Some(d) = s.deltas {
                    write!(
                        f,
                        " min {} median {} p90 {} p99 {} max {}",
                        d.min, d.median, d.p90, d.p99, d.max
                    )?;
                }
                writeln!(f)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(fen: &str) -> Sample {
        Sample {
            fen: fen.to_string(),
            depth: 3,
            kind: Shortcut::NullMove,
            claimed: 12,
            beta: 10,
            eval_beta: 40,
            window: Window::Zero,
            halfmove: 0,
        }
    }

    /// The fens of what a sampler is holding, which is how every test below
    /// says which events survived.
    fn held(sampled: &Sampled) -> Vec<&str> {
        sampled.taken.iter().map(|s| s.fen.as_str()).collect()
    }

    /// Scores inside the mate window, one either side of it: the reference
    /// answers the tests give a row when the row is to be a mate.
    const MATING: Score = crate::value::CHECKMATE_THRESHOLD + 1;
    const MATED: Score = -(crate::value::CHECKMATE_THRESHOLD + 1);

    /// A key a share of the way up the range, so a test can say where an
    /// event sits against a rate without writing sixteen hex digits out.
    fn key_at(share: f64) -> u64 {
        (u64::MAX as f64 * share) as u64
    }

    #[test]
    fn only_the_keys_under_the_rate_are_kept() {
        let mut sampler = Sampler::every(10);
        // a tenth of the range is kept, so the first two of these are in and
        // the rest are out
        for share in [0.0, 0.09, 0.11, 0.5, 0.99] {
            sampler.event(key_at(share), || sample(&share.to_string()));
        }
        let sampled = sampler.drain();
        assert_eq!(held(&sampled), vec!["0", "0.09"]);
        assert_eq!(sampled.overflowed, 0);
        // every event offered is counted, kept or not, which is what makes
        // two records out of five a rate rather than a number
        assert_eq!(sampled.events, 5);
    }

    /// The event count is the denominator, so it counts what was offered and
    /// not what survived: the rate turns some away, the cap turns more away,
    /// and the count is deaf to both. It goes with the samples when they are
    /// drained, leaving the sampler counting afresh.
    #[test]
    fn every_event_offered_is_counted_whatever_became_of_it() {
        let mut sampler = Sampler::with_cap(4, 1);
        for key in [1, 2, 3, u64::MAX] {
            sampler.event(key, || sample(&key.to_string()));
        }
        let sampled = sampler.drain();
        assert_eq!(sampled.events, 4);
        // one held, two wanted and displaced, one the rate never wanted
        assert_eq!(sampled.taken.len(), 1);
        assert_eq!(sampled.overflowed, 2);
        sampler.event(1, || sample("after"));
        assert_eq!(sampler.drain().events, 1);
    }

    #[test]
    fn a_rate_of_zero_keeps_every_event() {
        let mut sampler = Sampler::every(0);
        for key in [0, u64::MAX / 2, u64::MAX] {
            sampler.event(key, || sample(&key.to_string()));
        }
        assert_eq!(sampler.len(), 3);
    }

    /// The key decides on the node alone, so the same node keys the same way
    /// twice and two runs sample the same set. Depth and kind are in it
    /// because the same position answered at another depth, or by the other
    /// shortcut, is another measurement.
    #[test]
    fn a_key_is_the_node_and_nothing_about_the_run() {
        let position = 0x0123_4567_89ab_cdef;
        let key = sample_key(position, Shortcut::NullMove, 4);
        assert_eq!(key, sample_key(position, Shortcut::NullMove, 4));
        assert_ne!(key, sample_key(position, Shortcut::NullMove, 5));
        assert_ne!(key, sample_key(position, Shortcut::ReverseFutility, 4));
        assert_ne!(key, sample_key(position ^ 1, Shortcut::NullMove, 4));
    }

    /// The reservoir: past the cap the largest keys are the ones given up,
    /// so what survives is the smallest keys of everything offered.
    #[test]
    fn the_cap_keeps_the_smallest_keys_and_counts_the_rest() {
        let mut sampler = Sampler::with_cap(1, 3);
        for key in [50, 10, 90, 20, 70, 30, 60] {
            sampler.event(key, || sample(&key.to_string()));
        }
        let sampled = sampler.drain();
        // in key order, which is the order drain hands them over in
        assert_eq!(held(&sampled), vec!["10", "20", "30"]);
        assert_eq!(sampled.overflowed, 4);
    }

    /// Keys tie, and the tie is where the order-independence above stops.
    ///
    /// A key names a position, a kind and a depth, all three of which a
    /// deepening search revisits, so a run offers the same key more than
    /// once and the samples behind those offers differ: the beta and the
    /// window at a revisit are the node's second answer. This pins what the
    /// cap does with a tie for one fixed order, which is all that is
    /// promised. An equal key does not displace, so the record already held
    /// is the one that survives here, and a run that offered these three in
    /// another order could keep the other.
    #[test]
    fn a_tie_at_the_cap_is_settled_the_same_way_every_run() {
        let run = || {
            let mut sampler = Sampler::with_cap(1, 2);
            sampler.event(1, || sample("small"));
            sampler.event(5, || sample("first of the tie"));
            sampler.event(5, || sample("second of the tie"));
            sampler.drain()
        };
        let sampled = run();
        assert_eq!(held(&sampled), vec!["small", "first of the tie"]);
        assert_eq!(sampled.overflowed, 1);
        assert_eq!(sampled, run());
    }

    /// What the reservoir is for. The retained set is a property of the
    /// events and not of when they arrived, so a change that reorders the
    /// tree without changing what is in it samples the same nodes.
    #[test]
    fn two_orderings_of_the_same_events_keep_the_same_set() {
        let keys = [50u64, 10, 90, 20, 70, 30, 60];
        let run = |order: &[u64]| {
            let mut sampler = Sampler::with_cap(1, 4);
            for key in order {
                sampler.event(*key, || sample(&key.to_string()));
            }
            sampler.drain()
        };
        let forward = run(&keys);
        let mut backward: Vec<u64> = keys.to_vec();
        backward.reverse();
        assert_eq!(forward, run(&backward));
        let mut shuffled = vec![90u64, 20, 60, 50, 30, 70, 10];
        assert_eq!(forward, run(&shuffled));
        shuffled.sort_unstable();
        assert_eq!(forward, run(&shuffled));
        assert_eq!(held(&forward), vec!["10", "20", "30", "50"]);
    }

    #[test]
    fn draining_leaves_the_sampler_ready_to_record_again() {
        let mut sampler = Sampler::with_cap(1, 1);
        sampler.event(1, || sample("first"));
        sampler.event(2, || sample("dropped"));
        let first = sampler.drain();
        assert_eq!(held(&first), vec!["first"]);
        assert_eq!(first.overflowed, 1);
        assert!(sampler.is_empty());
        sampler.event(9, || sample("second"));
        let second = sampler.drain();
        assert_eq!(held(&second), vec!["second"]);
        assert_eq!(second.overflowed, 0);
    }

    /// A cap of nothing holds nothing, rather than reaching into an empty
    /// heap for something to give up.
    #[test]
    fn a_cap_of_nothing_counts_every_event_and_keeps_none() {
        let mut sampler = Sampler::with_cap(1, 0);
        for key in [3, 1, 2] {
            sampler.event(key, || sample(&key.to_string()));
        }
        let sampled = sampler.drain();
        assert!(sampled.taken.is_empty());
        assert_eq!(sampled.overflowed, 3);
    }

    /// The live kinds print the switches that turn them on; the shadow
    /// kind has no switch and prints the shortcut it watches.
    #[test]
    fn the_kinds_print_their_words() {
        assert_eq!(Shortcut::ReverseFutility.word(), "reverse_futility");
        assert_eq!(Shortcut::NullMove.word(), "null_move");
        assert_eq!(Shortcut::ShadowFutility.word(), "shadow_futility");
    }

    /// The window is read from the bounds and says which of the two
    /// questions the node was asked.
    #[test]
    fn a_window_one_wide_is_a_zero_width_one() {
        assert_eq!(Window::of(10, 11), Window::Zero);
        assert_eq!(Window::of(-1, 0), Window::Zero);
        assert_eq!(Window::of(10, 12), Window::Open);
        assert_eq!(Window::of(Score::MIN + 1, Score::MAX - 1), Window::Open);
        assert_eq!(Window::Zero.word(), "zw");
        assert_eq!(Window::Open.word(), "open");
    }

    /// Two positions, enough for a run to have something to record and
    /// little enough for the replay to finish in a test.
    fn suite() -> Vec<Position> {
        bench::parse_epd(
            "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - id \"sharp\";\n\
             r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - id \"kiwipete\";",
        )
    }

    /// The delta's sign is the whole reading of a residual, so it is pinned
    /// against a claim made up to be wrong in a known direction. The replay
    /// is driven straight, with no recording run in front of it, which is
    /// what lets the claimed value be anything at all.
    #[test]
    fn the_delta_is_the_reference_answer_less_what_was_claimed() {
        let fen = "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12";
        let claimed = Sample {
            fen: fen.to_string(),
            depth: 3,
            kind: Shortcut::ReverseFutility,
            // far above anything the position is worth, so the reference
            // must come back below it
            claimed: 20_000,
            beta: 20_000,
            eval_beta: 1,
            window: Window::Zero,
            halfmove: 9,
        };
        let modest = Sample {
            // and far below, so it must come back above
            claimed: -20_000,
            ..claimed.clone()
        };
        let (rows, unplayable) = replay(&[claimed, modest]);
        assert_eq!(unplayable, 0);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].delta() < 0, "{:?}", rows[0]);
        assert!(rows[1].delta() > 0, "{:?}", rows[1]);
        // and both saw the same position, so they agree on what it is worth
        assert_eq!(rows[0].reference, rows[1].reference);
        assert_eq!(
            rows[0].delta(),
            i32::from(rows[0].reference) - i32::from(rows[0].claimed)
        );
    }

    /// A position with no move to make has no reference value, so it is
    /// counted rather than scored and no row claims a comparison that was
    /// never made.
    #[test]
    fn a_position_with_no_move_is_counted_and_not_scored() {
        let mated = Sample {
            fen: "7k/6Q1/6K1/8/8/8/8/8 b - - 0 1".to_string(),
            depth: 2,
            kind: Shortcut::NullMove,
            claimed: 0,
            beta: 0,
            eval_beta: 0,
            window: Window::Zero,
            halfmove: 0,
        };
        let (rows, unplayable) = replay(&[mated]);
        assert!(rows.is_empty());
        assert_eq!(unplayable, 1);
    }

    #[test]
    fn a_run_records_and_replays_the_suite() {
        let report = run(
            &suite(),
            4,
            25,
            Sampler::DEFAULT_CAP,
            SearchConfig::default(),
        );
        assert_eq!(report.positions, 2);
        assert_eq!(report.depth, 4);
        assert_eq!(report.every, 25);
        assert!(!report.rows.is_empty(), "nothing was recorded");
        for row in &report.rows {
            let board = Board::from_fen(&row.fen).unwrap_or_else(|e| panic!("{}: {}", row.fen, e));
            assert!(Shortcut::KINDS.contains(&row.kind));
            assert!(row.depth >= 1 && row.depth <= 5, "{:?}", row);
            // the column and the fen say the same thing, which is what
            // lets a reader filter on the column and trust it
            assert_eq!(row.halfmove, board.halfmove_clock(), "{:?}", row);
        }
    }

    /// Where the purity argument is written down rather than where it is
    /// tested. Each engine owns its table and there is nothing global for
    /// one to reach the other through, so the node counts agreeing is what
    /// the types already guarantee and this cannot fail while that holds.
    /// It is kept because the day someone shares a table between the two
    /// phases, this is the test whose name says what was given up. What it
    /// really asserts is the line above the counts: the sampled run recorded
    /// something, so the comparison is between two runs that happened.
    #[test]
    fn recording_leaves_the_measured_search_where_it_was() {
        let suite = suite();
        let plain = bench::run_suite(&suite, 4, bench::TABLE_BYTES, SearchConfig::default());
        let sampled = run(&suite, 4, 1, Sampler::DEFAULT_CAP, SearchConfig::default());
        let watched = bench::run_suite(&suite, 4, bench::TABLE_BYTES, SearchConfig::default());
        assert!(!sampled.rows.is_empty());
        assert_eq!(plain.nodes(), watched.nodes());
    }

    /// A rate takes a subset of what rate one takes, and takes it by the key
    /// rather than by the count. Asked of the recording phase alone, which
    /// is where the property lives and which costs a search rather than a
    /// search and a replay.
    ///
    /// The membership is the property. A run at rate seven records the
    /// events of the rate one run whose keys fall in the first seventh of
    /// the range, wherever in the suite they happened, so nothing about
    /// which position the search was on or how far into it decides.
    #[test]
    fn a_rate_keeps_the_events_its_keys_choose() {
        const EVERY: u32 = 7;
        let suite = suite();
        let all = record(&suite, 4, 1, Sampler::DEFAULT_CAP, SearchConfig::default());
        assert_eq!(all.overflowed, 0, "the cap got in the way of the count");
        assert!(all.taken.len() > 30, "{} events", all.taken.len());
        let sampled = record(
            &suite,
            4,
            EVERY,
            Sampler::DEFAULT_CAP,
            SearchConfig::default(),
        );
        assert_eq!(sampled.overflowed, 0);
        let threshold = u64::MAX / u64::from(EVERY);
        // as multisets: the two runs agree on which events they kept, and
        // the order equal keys come out in is not a property of either
        let listed = |samples: &[Sample]| {
            let mut lines: Vec<String> = samples.iter().map(|s| format!("{:?}", s)).collect();
            lines.sort();
            lines
        };
        let expected: Vec<Sample> = all
            .taken
            .iter()
            .filter(|s| {
                Board::from_fen(&s.fen)
                    .is_ok_and(|b| sample_key(b.key, s.kind, s.depth) <= threshold)
            })
            .cloned()
            .collect();
        assert!(!expected.is_empty(), "the rate turned everything away");
        assert!(
            expected.len() < all.taken.len(),
            "the rate kept everything, so it chose nothing"
        );
        assert_eq!(listed(&sampled.taken), listed(&expected));
    }

    /// The crossing, which is the row's primary label. Driven through the
    /// replay so that the reference value is a real one, with the beta made
    /// up on either side of it.
    #[test]
    fn the_reference_crosses_a_beta_it_falls_below_and_not_one_it_meets() {
        let fen = "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12";
        let node = Sample {
            fen: fen.to_string(),
            depth: 2,
            kind: Shortcut::NullMove,
            claimed: 0,
            beta: 0,
            eval_beta: 0,
            window: Window::Zero,
            halfmove: 9,
        };
        let (rows, _) = replay(&[node]);
        let reference = rows[0].reference;
        let at = Row {
            beta: reference,
            ..rows[0].clone()
        };
        let above = Row {
            beta: reference + 1,
            ..rows[0].clone()
        };
        let below = Row {
            beta: reference - 1,
            ..rows[0].clone()
        };
        assert!(above.crossed(), "{:?}", above);
        // a reference answer equal to beta is a fail high the shortcut was
        // entitled to take
        assert!(!at.crossed(), "{:?}", at);
        assert!(!below.crossed(), "{:?}", below);
        assert_eq!(above.crossing_word(), "crossed");
        assert_eq!(at.crossing_word(), "clear");
    }

    /// What the reference search on its own says about a position at a
    /// depth, which is what every row here claims to hold.
    fn reference_answer(fen: &str, depth: u8) -> Score {
        let mut engine = replay_engine();
        engine.parse_fen(fen).expect("the fen parses");
        let outcome =
            engine.iterative_deepening_search(SearchParameters::to_depth(depth), |_, _, _, _| {});
        let SearchOutcome::Complete(result) = outcome else {
            panic!("{} at depth {} has no move to make", fen, depth);
        };
        result.score
    }

    /// The replay starts cold for every sample, so no sample's reference
    /// answer is another's, and each answer is the one a search driven
    /// straight at that position and that depth gives.
    ///
    /// The same position at two depths is the case that bites: the deeper
    /// record leaves entries the shallower one's children would be answered
    /// from, and the shallow row would then quietly hold the deep search's
    /// answer. The first assertion is that the two depths really do disagree
    /// here, without which the rest of the test would pass on a table that
    /// was never cleared. The last two pin the depth as well as the table: a
    /// replay that searched a sample one ply out would still be internally
    /// consistent and would still be wrong.
    #[test]
    fn a_sample_is_replayed_on_a_cold_table() {
        let fen = "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12";
        let node = |depth: u8| Sample {
            fen: fen.to_string(),
            depth,
            kind: Shortcut::NullMove,
            claimed: 0,
            beta: 0,
            eval_beta: 0,
            window: Window::Zero,
            halfmove: 9,
        };
        let (alone, _) = replay(&[node(5)]);
        let (shallow, _) = replay(&[node(2)]);
        assert_ne!(
            alone[0].reference, shallow[0].reference,
            "the depths agree, so this test could not tell a warm table from a cold one"
        );
        // the deep record first, so its entries are there to be read
        let (both, _) = replay(&[node(5), node(2)]);
        assert_eq!(both[0].reference, alone[0].reference);
        assert_eq!(both[1].reference, shallow[0].reference);
        assert_eq!(both[0].reference, reference_answer(fen, 5));
        assert_eq!(both[1].reference, reference_answer(fen, 2));
    }

    /// The premise of the module, asserted rather than left to the reader of
    /// `replay_engine`. A replay searching the default configuration would
    /// be asking the shortcuts what the shortcuts cost, and every crossing
    /// rate it printed would be a shortcut agreeing with itself.
    #[test]
    fn the_replay_searches_the_reference_and_not_the_default() {
        let engine = replay_engine();
        assert_eq!(engine.config(), SearchConfig::reference());
        assert_ne!(SearchConfig::reference(), SearchConfig::default());
        // and it owns a table rather than borrowing the measured search's
        assert!(engine.table_bytes() >= REPLAY_TABLE_BYTES);
    }

    /// The sampler holds a rate of zero at one, so the header says one:
    /// two runs that behaved identically print identical headers.
    #[test]
    fn a_rate_of_zero_is_reported_as_the_rate_that_ran() {
        let report = run(
            &suite(),
            2,
            0,
            Sampler::DEFAULT_CAP,
            SearchConfig::default(),
        );
        assert_eq!(report.every, 1);
        assert!(
            report.to_string().starts_with("residuals depth 2 every 1 "),
            "{}",
            report
        );
    }

    /// The cap the run was asked for reaches the sampler: a run capped at
    /// two keeps two records of everything it offered and counts the rest.
    #[test]
    fn the_cap_asked_for_bounds_the_run() {
        let report = run(&suite(), 3, 1, 2, SearchConfig::default());
        assert_eq!(report.cap, 2);
        // each kept sample is replayed into a row or counted unplayable
        assert_eq!(report.rows.len() + report.unplayable, 2);
        assert!(report.overflowed > 0, "{}", report);
        assert!(report.to_string().contains(" cap 2 "), "{}", report);
    }

    /// The cap is a setting like the rate, so a header states one that is
    /// not the default and a run can be rerun from what it printed. The
    /// default on every header would describe nothing, the same rule the
    /// overflow and unplayable counts follow.
    #[test]
    fn a_cap_off_the_default_is_stated_in_the_header() {
        let mut report = report_of(Vec::new());
        assert!(
            report
                .to_string()
                .starts_with("residuals depth 4 every 1 taint "),
            "{}",
            report
        );
        report.cap = 25;
        assert!(
            report
                .to_string()
                .starts_with("residuals depth 4 every 1 cap 25 taint "),
            "{}",
            report
        );
    }

    #[test]
    fn the_report_names_its_settings_and_ends_in_a_summary() {
        let report = run(
            &suite(),
            3,
            20,
            Sampler::DEFAULT_CAP,
            SearchConfig::default(),
        );
        let text = report.to_string();
        assert!(
            text.starts_with(&format!(
                "residuals depth 3 every 20 taint rule50 positions 2 events {} records {}\n",
                report.events,
                report.rows.len()
            )),
            "{}",
            text
        );
        // the events are the denominator, so a run that sampled at all
        // offered more than it kept
        assert!(report.events > report.rows.len() as u64, "{}", text);
        let lines: Vec<&str> = text.lines().collect();
        let summary_at = lines.iter().position(|l| *l == "summary").expect("summary");
        let summary = &lines[summary_at + 1..];
        // a line a kind at a depth, in kind order and shallowest first
        let expected: usize = Shortcut::KINDS
            .iter()
            .map(|kind| report.summaries(*kind).len().max(1))
            .sum();
        assert_eq!(summary.len(), expected, "{:?}", summary);
        let mut at = 0;
        for kind in Shortcut::KINDS {
            let mut last = 0;
            for s in report.summaries(kind) {
                let line = summary[at];
                assert!(
                    line.starts_with(&format!("{} depth {} ", kind.word(), s.depth)),
                    "{}",
                    line
                );
                assert!(s.depth > last, "{:?} is out of order", summary);
                last = s.depth;
                at += 1;
            }
            if report.summaries(kind).is_empty() {
                assert_eq!(summary[at], format!("{} 0", kind.word()));
                at += 1;
            }
        }
    }

    /// The row's fields, in the order the header of docs/DEVELOPMENT.md
    /// names them, with the fen last so a row parses left to right.
    #[test]
    fn a_row_reads_left_to_right_with_the_fen_last() {
        let report = Report {
            depth: 4,
            every: 10,
            cap: Sampler::DEFAULT_CAP,
            config: SearchConfig::default(),
            positions: 1,
            events: 40,
            overflowed: 0,
            unplayable: 0,
            rows: vec![Row {
                kind: Shortcut::NullMove,
                depth: 3,
                window: Window::Zero,
                halfmove: 12,
                // above the reference below it, so the row reads crossed
                beta: 180,
                eval_beta: 140,
                claimed: 200,
                reference: 150,
                fen: "4k3/8/8/8/8/8/8/4K3 w - - 0 1".to_string(),
            }],
        };
        let text = report.to_string();
        let row = text.lines().nth(1).expect("a row");
        let mut words = row.splitn(11, ' ');
        assert_eq!(words.next(), Some("null_move"));
        assert_eq!(words.next(), Some("3"));
        assert_eq!(words.next(), Some("zw"));
        assert_eq!(words.next(), Some("12"));
        assert_eq!(words.next(), Some("180"));
        assert_eq!(words.next(), Some("140"));
        assert_eq!(words.next(), Some("200"));
        assert_eq!(words.next(), Some("150"));
        assert_eq!(words.next(), Some("-50"));
        assert_eq!(words.next(), Some("crossed"));
        assert_eq!(words.next(), Some("4k3/8/8/8/8/8/8/4K3 w - - 0 1"));
        // the header states the denominator whether or not anything else
        // needed saying
        assert!(
            text.starts_with(
                "residuals depth 4 every 10 taint rule50 positions 1 events 40 records 1\n"
            ),
            "{}",
            text
        );
        assert!(
            text.contains(
                "null_move depth 3 1 crossed 1 rate 100.00% mates 0 \
                 min -50 median -50 p90 -50 p99 -50 max -50"
            ),
            "{}",
            text
        );
        // a kind with nothing recorded says so rather than going missing
        assert!(text.contains("\nreverse_futility 0\n"));
        assert!(text.contains("\nshadow_futility 0\n"));
    }

    /// A row made up, so a test can put a reference where it wants one.
    fn made_up(depth: u8, reference: Score) -> Row {
        Row {
            kind: Shortcut::ReverseFutility,
            depth,
            window: Window::Open,
            halfmove: 0,
            beta: 100,
            eval_beta: 30,
            claimed: 100,
            reference,
            fen: "4k3/8/8/8/8/8/8/4K3 w - - 0 1".to_string(),
        }
    }

    /// A report over rows made up, for the tests that pin what the summary
    /// says about them.
    fn report_of(rows: Vec<Row>) -> Report {
        Report {
            depth: 4,
            every: 1,
            cap: Sampler::DEFAULT_CAP,
            config: SearchConfig::default(),
            positions: 1,
            events: 1_000,
            overflowed: 0,
            unplayable: 0,
            rows,
        }
    }

    /// The crossing count and the rate are what the summary is read for, so
    /// they are pinned against rows made up to give a rate that is neither
    /// nothing nor everything.
    #[test]
    fn the_summary_counts_the_crossings_and_says_their_share() {
        // one below beta, one at it, and two above
        let report = report_of(vec![
            made_up(2, 60),
            made_up(2, 100),
            made_up(2, 140),
            made_up(2, 180),
        ]);
        let summary = report
            .summary(Shortcut::ReverseFutility, 2)
            .expect("four rows of the pair");
        assert_eq!(summary.count, 4);
        assert_eq!(summary.crossed, 1);
        assert_eq!(summary.mates, 0);
        assert_eq!(summary.crossing_rate(), 25.0);
        assert!(
            report.to_string().contains(
                "reverse_futility depth 2 4 crossed 1 rate 25.00% mates 0 \
                 min -40 median 0 p90 80 p99 80 max 80"
            ),
            "{}",
            report
        );
    }

    /// A line a depth, not one line over all of them. The depths are reached
    /// in wildly different numbers and the margin risked grows with the
    /// depth, so a pooled rate is the shallowest depth's rate under every
    /// depth's name.
    #[test]
    fn each_depth_of_a_kind_is_summarised_on_its_own() {
        let report = report_of(vec![
            // three at depth one, none of which crossed
            made_up(1, 140),
            made_up(1, 150),
            made_up(1, 160),
            // and one at depth three that did
            made_up(3, 40),
        ]);
        let shallow = report
            .summary(Shortcut::ReverseFutility, 1)
            .expect("three rows at depth one");
        let deep = report
            .summary(Shortcut::ReverseFutility, 3)
            .expect("one row at depth three");
        assert_eq!((shallow.count, shallow.crossed), (3, 0));
        assert_eq!((deep.count, deep.crossed), (1, 1));
        assert_eq!(shallow.crossing_rate(), 0.0);
        assert_eq!(deep.crossing_rate(), 100.0);
        // the depth with no rows is not invented
        assert!(report.summary(Shortcut::ReverseFutility, 2).is_none());
        let text = report.to_string();
        assert!(
            text.contains("reverse_futility depth 1 3 crossed 0 rate 0.00% mates 0 "),
            "{}",
            text
        );
        assert!(
            text.contains("reverse_futility depth 3 1 crossed 1 rate 100.00% mates 0 "),
            "{}",
            text
        );
        // and the pooled quarter, which is what this replaces, is nowhere
        assert!(!text.contains("rate 25.00%"), "{}", text);
        assert_eq!(
            report
                .summaries(Shortcut::ReverseFutility)
                .iter()
                .map(|s| s.depth)
                .collect::<Vec<u8>>(),
            vec![1, 3]
        );
    }

    /// A mate reference is a crossing question and nothing else. Its delta
    /// is not a number of pawns, and the distance it encodes is counted from
    /// the replay's root rather than the recorded search's, so it is left
    /// out of the percentiles and counted in a column of its own. It still
    /// answers to beta, which is the label that matters.
    #[test]
    fn a_mate_reference_is_counted_and_kept_out_of_the_percentiles() {
        let report = report_of(vec![
            made_up(2, 140),
            made_up(2, 160),
            made_up(2, MATING),
            made_up(2, MATED),
        ]);
        let summary = report
            .summary(Shortcut::ReverseFutility, 2)
            .expect("four rows of the pair");
        assert_eq!(summary.count, 4);
        assert_eq!(summary.mates, 2);
        // the mate below beta crossed, and the count and the rate are over
        // every row of the pair rather than over the two left after the
        // mates were set aside
        assert_eq!(summary.crossed, 1);
        assert_eq!(summary.crossing_rate(), 25.0);
        // the percentiles are the two ordinary rows and nothing else
        let deltas = summary.deltas.expect("two rows that are not mates");
        assert_eq!((deltas.min, deltas.max), (40, 60));
        assert!(
            report
                .to_string()
                .contains("reverse_futility depth 2 4 crossed 1 rate 25.00% mates 2 min 40 "),
            "{}",
            report
        );
    }

    /// A pair whose every row is a mate has no residual to take a
    /// percentile of, so the line stops after the counts rather than
    /// printing a number that means nothing.
    #[test]
    fn a_pair_of_nothing_but_mates_prints_no_percentiles() {
        let report = report_of(vec![made_up(2, MATING)]);
        let summary = report
            .summary(Shortcut::ReverseFutility, 2)
            .expect("one row of the pair");
        assert_eq!((summary.count, summary.mates), (1, 1));
        assert!(summary.deltas.is_none());
        let text = report.to_string();
        assert!(
            text.contains("\nreverse_futility depth 2 1 crossed 0 rate 0.00% mates 1\n"),
            "{}",
            text
        );
        assert!(!text.contains("median"), "{}", text);
    }

    /// Nearest rank, so every figure printed is one of the residuals rather
    /// than an average of two of them.
    #[test]
    fn a_percentile_is_one_of_the_values() {
        let sorted: Vec<i32> = (1..=10).collect();
        assert_eq!(percentile(&sorted, 50), 5);
        assert_eq!(percentile(&sorted, 90), 9);
        assert_eq!(percentile(&sorted, 99), 10);
        // and a single value is every percentile of itself
        assert_eq!(percentile(&[7], 50), 7);
        assert_eq!(percentile(&[7], 99), 7);
    }

    #[test]
    fn the_header_says_when_the_buffer_or_the_replay_dropped_something() {
        let mut report = Report {
            depth: 4,
            every: 1,
            cap: Sampler::DEFAULT_CAP,
            config: SearchConfig::default(),
            positions: 1,
            events: 0,
            overflowed: 0,
            unplayable: 0,
            rows: Vec::new(),
        };
        let quiet = report.to_string();
        assert!(!quiet.contains("overflow"), "{}", quiet);
        assert!(!quiet.contains("unplayable"), "{}", quiet);
        // the events are stated whatever they are, including none: a run
        // that offered nothing is a fact the header owes the reader
        assert!(
            quiet.starts_with(
                "residuals depth 4 every 1 taint rule50 positions 1 events 0 records 0\n"
            ),
            "{}",
            quiet
        );
        report.events = 900;
        report.overflowed = 12;
        report.unplayable = 3;
        let loud = report.to_string();
        assert!(loud.starts_with(
            "residuals depth 4 every 1 taint rule50 positions 1 events 900 records 0 overflow 12 unplayable 3\n"
        ), "{}", loud);
    }
}
