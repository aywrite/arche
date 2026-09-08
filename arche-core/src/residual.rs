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
//! the crossing, the primary label on every row here. The second label is
//! the overstatement, a claim above the reference's answer: the value is
//! wrong even where the decision was right, and it is read wherever a
//! fail-soft value is read. The residual, the reference's answer less the
//! claim, stays beside both as the size of the error the labels are drawn
//! from.
//!
//! The recording half is the reservoir in `recorder`, armed with the
//! `Sample` this module defines. The replaying half is here, and runs after
//! the search and never during: see the module's `run`.

use crate::bench::Position;
use crate::board::Board;
use crate::engine::{AlphaBeta, Engine, SearchConfig, SearchOutcome, SearchParameters};
use crate::misc::Score;
use crate::recorder::{self, DEFAULT_CAP, DEPTH_SPREAD, Window};
use crate::value::Value;
use std::fmt;

/// The shortcut that answered a node, or the shadow lane that watched one
/// it could have.
///
/// The first two are the shortcuts that answer a whole node, which is what
/// gives a reference something to be asked about. The default turns on
/// three others: the delta margin and the losing capture skip pass over a
/// move in quiescence rather than answering a node, and what trusting the
/// late move reduction's scout costs is the reduction ledger's question.
/// Another shortcut answering a node would be one arm here and one call at
/// wherever it returns.
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
    /// found, which is `overstated`. It is the size of the error and not the
    /// error itself: a shortcut returns a lower bound, so claiming more than
    /// the position is worth costs the node nothing as long as it still
    /// fails high. What it costs when the node does not is `crossed`.
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

    /// Whether the claim is higher than the reference's answer, which is
    /// the value the shortcut got wrong: the bound it handed the parent
    /// was more than the position is worth.
    ///
    /// Independent of `crossed`. A shortcut can clear beta rightly and still
    /// overstate, and the overstatement then costs nothing at the node. The
    /// parent reads it: a child's claim arrives at or below the parent's
    /// alpha, so it never raises alpha, but it is the parent's fail-soft
    /// best when no move does better, and the ceiling the parent stores is
    /// then one too low. A claim can also sit under a reference that still
    /// crossed, when the cutoff was wrong and the value was not. On a
    /// shadow row nothing was handed up, and the label says what the
    /// margin would have overstated had it fired.
    ///
    /// Strictly above. A claim equal to the reference is the bound holding
    /// exactly.
    ///
    /// A mate reference is labelled like any other row. The distance the
    /// replay reports is counted from the wrong root, as `reference_is_mate`
    /// says, but no claim is a mate score, and an eval claim is above every
    /// mated score and below every mating score whatever the distance, so
    /// the label survives.
    pub fn overstated(&self) -> bool {
        self.claimed > self.reference
    }

    /// The word a row prints for the overstatement, said either way like
    /// the crossing: the bound held, or it did not.
    pub fn overstating_word(&self) -> &'static str {
        if self.overstated() {
            "overstated"
        } else {
            "held"
        }
    }

    /// Whether the reference answered with a forced mate.
    ///
    /// Such a row answers the two labels and nothing else. Its delta is not
    /// a number of pawns, and the mate distance the reference reports counts
    /// from the root of the replay while the claim's counts from the root of
    /// the recorded search, so the two are not even measured from the same
    /// place. The labels survive all of that: a mate score is above every
    /// eval or below every eval, and which of the two is all that comparing
    /// it with beta, or with an eval claim, asks.
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
    /// Rows whose claim is higher than the reference's answer. Counted over
    /// every row of the pair, the mates included, the way the crossings
    /// are; the two counts overlap without either containing the other.
    pub overstated: usize,
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
    /// The file the positions were read from, or none when they are the
    /// bench's own. Stated in the header for the cap's reason: a report
    /// says how to run it again, and a distribution measured over another
    /// suite is not the bench's.
    pub suite: Option<String>,
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
    /// fen did not parse. Counted rather than scored, so the rows below are
    /// all comparisons and the count says how many were not. A position
    /// with no move to make is not one of these. It has a value by rule,
    /// and it gets a row.
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
///
/// `suite` names the file the positions came from, for the header alone.
/// None is the bench's own suite, and nothing here reads the positions any
/// differently either way.
pub fn run(
    positions: &[Position],
    suite: Option<&str>,
    depth: u8,
    every: u32,
    cap: usize,
    config: SearchConfig,
) -> Report {
    let depth = depth.max(1);
    // the rate the sampler will really keep to, so that the header states
    // the run that happened rather than the words it was asked in
    let every = every.max(1);
    let sampled = recorder::record(positions, depth, every, cap, config);
    let (rows, unplayable) = replay(&sampled.taken);
    Report {
        depth,
        every,
        cap,
        suite: suite.map(str::to_string),
        config,
        positions: positions.len(),
        events: sampled.events,
        overflowed: sampled.overflowed,
        unplayable,
        rows,
    }
}

/// The engine the replay asks its questions of.
///
/// The reference and not the default, which is the premise the whole module
/// rests on: a shortcut cannot be the judge of what it cost, so the search
/// that answers the sampled positions is the one with every shortcut off.
/// Built here rather than inline so a test can hold the replay to it.
fn replay_engine() -> AlphaBeta {
    AlphaBeta::with_config(Board::new(), REPLAY_TABLE_BYTES, SearchConfig::reference())
}

/// What the reference search says about each sampled position, and how many
/// of them it could not read.
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
        let reference = match outcome {
            SearchOutcome::Complete(result) => result.score,
            // the position has no move to make: mate, stalemate, or drawn
            // already by the counter its fen carries. The rules fix what
            // it is worth, so it is scored the way the search scores the
            // same position a ply down: a mate on the hundredth half move
            // is still a mate, and everything else is a draw. From a real
            // run only the stalemate arrives here, since a sampled node is
            // never in check and never past the counter
            SearchOutcome::GameOver => {
                if engine.board.in_check() && !engine.board.has_legal_move() {
                    Value::mated(0).score
                } else {
                    0
                }
            }
            // the replay runs to a depth and never on a clock, so the one
            // abort left is a depth of zero, which the sampler never records
            SearchOutcome::Aborted(_) => {
                unreachable!("a sample is searched to a depth of at least one")
            }
        };
        rows.push(Row {
            kind: sample.kind,
            depth: sample.depth,
            window: sample.window,
            halfmove: sample.halfmove,
            beta: sample.beta,
            eval_beta: sample.eval_beta,
            claimed: sample.claimed,
            reference,
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
            overstated: of_pair().filter(|row| row.overstated()).count(),
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
/// delta crossed overstated fen`, whitespace separated with the fen last, so
/// it parses left to right and the field that can hold spaces holds the rest
/// of the line.
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
        if self.cap != DEFAULT_CAP {
            write!(f, " cap {}", self.cap)?;
        }
        // the bench's own suite is the default and reads as absent, the way
        // the cap does. A run over another suite says so, because a
        // distribution measured over other positions is another
        // distribution
        if let Some(suite) = &self.suite {
            write!(f, " epd {}", suite)?;
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
                "{} {} {} {} {} {} {} {} {} {} {} {}",
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
                row.overstating_word(),
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
                    "{} depth {} {} crossed {} rate {:.2}% overstated {} mates {}",
                    kind.word(),
                    s.depth,
                    s.count,
                    s.crossed,
                    s.crossing_rate(),
                    s.overstated,
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
    use crate::recorder::Sampler;
    use crate::recorder::fixtures::{recording_leaves_the_search_where_it_was, suite};

    /// Scores inside the mate window, one either side of it: the reference
    /// answers the tests give a row when the row is to be a mate.
    const MATING: Score = crate::value::CHECKMATE_THRESHOLD + 1;
    const MATED: Score = -(crate::value::CHECKMATE_THRESHOLD + 1);

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

    /// The live kinds print the switches that turn them on; the shadow
    /// kind has no switch and prints the shortcut it watches.
    #[test]
    fn the_kinds_print_their_words() {
        assert_eq!(Shortcut::ReverseFutility.word(), "reverse_futility");
        assert_eq!(Shortcut::NullMove.word(), "null_move");
        assert_eq!(Shortcut::ShadowFutility.word(), "shadow_futility");
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

    /// A sample whose position is a finished game as a diagram. The search
    /// that recorded it stood a ply short of the end, and the fen carries
    /// only the position.
    fn finished(fen: &str, halfmove: usize) -> Sample {
        Sample {
            fen: fen.to_string(),
            depth: 2,
            kind: Shortcut::NullMove,
            claimed: 0,
            beta: 0,
            eval_beta: 0,
            window: Window::Zero,
            halfmove,
        }
    }

    /// A mated root is worth the mated score, the value the search gives
    /// the same position at an interior node. The row is a mate row like
    /// any other the reference answers with, so it is counted under mates
    /// and kept out of the percentiles. The second sample is mated on the
    /// hundredth half move, which the search scores as a mate and not as
    /// the draw the counter would claim.
    #[test]
    fn a_mated_root_is_scored_as_mated() {
        let samples = [
            finished("7k/6Q1/6K1/8/8/8/8/8 b - - 0 1", 0),
            finished("k6R/8/1K6/8/8/8/8/8 b - - 100 100", 100),
        ];
        let (rows, unplayable) = replay(&samples);
        assert_eq!(unplayable, 0);
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert_eq!(row.reference, Value::mated(0).score, "{:?}", row);
            assert!(row.reference_is_mate(), "{:?}", row);
        }
    }

    /// A stalemated root is a draw, so its row says 0 rather than the
    /// sample being dropped. This is the one terminal position a real run
    /// reaches: a claim above beta on a side that turns out to have no
    /// move, which the old count hid.
    #[test]
    fn a_stalemated_root_is_scored_as_a_draw() {
        let (rows, unplayable) = replay(&[finished("k7/8/1Q6/8/8/8/8/7K b - - 0 1", 0)]);
        assert_eq!(unplayable, 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reference, 0, "{:?}", rows[0]);
        assert!(!rows[0].reference_is_mate());
    }

    /// A root the fifty move counter has already drawn scores 0 as well.
    /// The second sample is in check on the hundredth half move with a
    /// square to run to, so being in check alone does not make a mate.
    #[test]
    fn a_root_the_fifty_move_rule_has_drawn_is_scored_as_a_draw() {
        let samples = [
            finished("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/8/8/8 w - - 100 112", 100),
            finished("k6R/8/2K5/8/8/8/8/8 b - - 100 100", 100),
        ];
        let (rows, unplayable) = replay(&samples);
        assert_eq!(unplayable, 0);
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert_eq!(row.reference, 0, "{:?}", row);
        }
    }

    /// The one sample the replay cannot score: a fen it cannot read. That
    /// is what the unplayable count is, and nothing else goes into it.
    #[test]
    fn a_fen_that_does_not_parse_is_counted_unplayable() {
        let (rows, unplayable) = replay(&[finished("not a position", 0)]);
        assert!(rows.is_empty());
        assert_eq!(unplayable, 1);
    }

    #[test]
    fn a_run_records_and_replays_the_suite() {
        let report = run(&suite(), None, 4, 25, DEFAULT_CAP, SearchConfig::default());
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

    /// The sampler's contract, asked the way `fixtures` asks all three.
    #[test]
    fn recording_leaves_the_measured_search_where_it_was() {
        recording_leaves_the_search_where_it_was(
            4,
            |engine| engine.arm(Sampler::<Sample>::with_cap(1, DEFAULT_CAP)),
            |engine| {
                engine
                    .disarm::<Sample>()
                    .expect("the sampler comes back")
                    .drain()
                    .taken
                    .len()
            },
        );
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
        let all = recorder::record::<Sample>(&suite, 4, 1, DEFAULT_CAP, SearchConfig::default());
        assert_eq!(all.overflowed, 0, "the cap got in the way of the count");
        assert!(all.taken.len() > 30, "{} events", all.taken.len());
        let sampled = recorder::record(&suite, 4, EVERY, DEFAULT_CAP, SearchConfig::default());
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

    /// The overstatement, the row's second label: the claim is higher than
    /// what the reference found. Strictly above, since a claim equal to the
    /// reference is the bound holding exactly.
    #[test]
    fn a_claim_above_the_reference_overstates_it_and_one_at_or_below_does_not() {
        let above = claiming(2, 150, 120);
        let at = claiming(2, 120, 120);
        let below = claiming(2, 90, 120);
        assert!(above.overstated(), "{:?}", above);
        assert!(!at.overstated(), "{:?}", at);
        assert!(!below.overstated(), "{:?}", below);
        assert_eq!(above.overstating_word(), "overstated");
        assert_eq!(at.overstating_word(), "held");
        assert_eq!(below.overstating_word(), "held");
    }

    /// The two labels answer different questions, so each is found with the
    /// other absent. A claim under the reference still crosses when the
    /// reference is under beta: the cutoff was wrong and the value was not.
    /// A claim over a reference that clears beta overstates without
    /// crossing: the cutoff was right and the value handed up was too high.
    #[test]
    fn a_row_can_cross_without_overstating_and_overstate_without_crossing() {
        // beta is 100 in every made up row
        let crossed_only = claiming(2, 70, 90);
        let overstated_only = claiming(2, 150, 120);
        let both = claiming(2, 100, 60);
        let neither = claiming(2, 100, 140);
        assert!(crossed_only.crossed() && !crossed_only.overstated());
        assert!(!overstated_only.crossed() && overstated_only.overstated());
        assert!(both.crossed() && both.overstated());
        assert!(!neither.crossed() && !neither.overstated());
    }

    /// A mate reference is labelled like any other row. A claim above a
    /// mated reference overstated it, and a claim below a mating reference
    /// did not; the mate distance is counted from the wrong root, and that
    /// moves neither answer.
    #[test]
    fn a_mate_reference_is_overstated_by_a_claim_above_it() {
        let over_a_mated = claiming(2, 100, MATED);
        let under_a_mating = claiming(2, 100, MATING);
        assert!(over_a_mated.overstated(), "{:?}", over_a_mated);
        assert!(!under_a_mating.overstated(), "{:?}", under_a_mating);
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
        let report = run(&suite(), None, 2, 0, DEFAULT_CAP, SearchConfig::default());
        assert_eq!(report.every, 1);
        assert!(
            report.to_string().starts_with("residuals depth 2 every 1 "),
            "{}",
            report
        );
    }

    /// A run over a suite of its own says so in the header, so a
    /// distribution measured over other positions is never read as the
    /// bench's.
    #[test]
    fn the_header_names_a_suite_that_is_not_the_benchs() {
        let named = run(
            &suite(),
            Some("held_out.epd"),
            2,
            0,
            DEFAULT_CAP,
            SearchConfig::default(),
        );
        assert!(
            named
                .to_string()
                .starts_with("residuals depth 2 every 1 epd held_out.epd taint"),
            "{}",
            named
        );
        let bench = run(&suite(), None, 2, 0, DEFAULT_CAP, SearchConfig::default());
        assert!(
            bench
                .to_string()
                .starts_with("residuals depth 2 every 1 taint"),
            "{}",
            bench
        );
    }

    /// The cap the run was asked for reaches the sampler: a run capped at
    /// two keeps two records of everything it offered and counts the rest.
    #[test]
    fn the_cap_asked_for_bounds_the_run() {
        let report = run(&suite(), None, 3, 1, 2, SearchConfig::default());
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
        let report = run(&suite(), None, 3, 20, DEFAULT_CAP, SearchConfig::default());
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
            cap: DEFAULT_CAP,
            suite: None,
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
        let mut words = row.splitn(12, ' ');
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
        // and the claim of 200 is above the reference of 150
        assert_eq!(words.next(), Some("overstated"));
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
                "null_move depth 3 1 crossed 1 rate 100.00% overstated 1 mates 0 \
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

    /// The same row with the claim made up as well, for the tests that put
    /// the claim and the reference on either side of each other.
    fn claiming(depth: u8, claimed: Score, reference: Score) -> Row {
        Row {
            claimed,
            ..made_up(depth, reference)
        }
    }

    /// A report over rows made up, for the tests that pin what the summary
    /// says about them.
    fn report_of(rows: Vec<Row>) -> Report {
        Report {
            depth: 4,
            every: 1,
            cap: DEFAULT_CAP,
            suite: None,
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
                "reverse_futility depth 2 4 crossed 1 rate 25.00% overstated 1 mates 0 \
                 min -40 median 0 p90 80 p99 80 max 80"
            ),
            "{}",
            report
        );
    }

    /// The overstatements are counted beside the crossings and not inside
    /// them: the rows are made up so that the two counts differ, and the
    /// line prints the count right after the crossing rate.
    #[test]
    fn the_summary_counts_the_overstatements_apart_from_the_crossings() {
        // beta is 100 in every made up row: one crossed and overstated,
        // one overstated with beta cleared, one crossed with the claim
        // under the reference, and one neither
        let report = report_of(vec![
            claiming(2, 100, 60),
            claiming(2, 150, 120),
            claiming(2, 70, 90),
            claiming(2, 100, 140),
        ]);
        let summary = report
            .summary(Shortcut::ReverseFutility, 2)
            .expect("four rows of the pair");
        assert_eq!(summary.count, 4);
        assert_eq!(summary.crossed, 2);
        assert_eq!(summary.overstated, 2);
        assert!(
            report.to_string().contains(
                "reverse_futility depth 2 4 crossed 2 rate 50.00% overstated 2 mates 0 \
                 min -40 median -30 p90 40 p99 40 max 40"
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
            text.contains("reverse_futility depth 1 3 crossed 0 rate 0.00% overstated 0 mates 0 "),
            "{}",
            text
        );
        assert!(
            text.contains(
                "reverse_futility depth 3 1 crossed 1 rate 100.00% overstated 1 mates 0 "
            ),
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
        // the mated reference is the one row the claim of 100 is above,
        // and it is counted: a mate row is set aside from the percentiles
        // and from neither label
        assert_eq!(summary.overstated, 1);
        // the percentiles are the two ordinary rows and nothing else
        let deltas = summary.deltas.expect("two rows that are not mates");
        assert_eq!((deltas.min, deltas.max), (40, 60));
        assert!(
            report.to_string().contains(
                "reverse_futility depth 2 4 crossed 1 rate 25.00% overstated 1 mates 2 min 40 "
            ),
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
            text.contains(
                "\nreverse_futility depth 2 1 crossed 0 rate 0.00% overstated 0 mates 1\n"
            ),
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
            cap: DEFAULT_CAP,
            suite: None,
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
