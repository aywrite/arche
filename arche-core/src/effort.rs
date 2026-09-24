// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The effort instrument: what a rule frees, and where the freed effort
//! goes.
//!
//! The other five instruments each describe one tree. A saving is a
//! difference between two, so no row any of them writes can carry one.
//! Here one command searches the suite twice, once under the default and
//! once under the default with a named switch off, and joins the two runs
//! by the node.
//!
//! The join is possible because the sampling key is a function of the node
//! and nothing about the run. Both sides record under one lane, so wherever
//! both reached a position at a depth they kept it or dropped it alike, and
//! a key one side holds and the other does not is a fact about the trees.
//! A counter over the stream would take two unrelated sets and the join
//! would be empty.
//!
//! A joined key reads one of three ways. `both` is a node in both trees,
//! so the difference in what sat under it is effort the rule moved.
//! `only_off` is a node the baseline reached and the candidate never did,
//! so what sat under it is effort the rule removed. `only_on` is that the
//! other way round, and what sits under it is effort the rule created.
//! Nothing else in the engine reads that last population.
//!
//! The rows are sampled. The per depth node counts beside them are not:
//! every offered event is tallied by depth on each side, so those counts do
//! not move with `every`. The rows attribute and the counts measure.
//!
//! Recording changes nothing, on the reservoir's terms, and here under
//! either of the two configurations, since this is the only instrument that
//! searches under a configuration its caller chose.

use crate::bench::{self, Position};
use crate::board::Board;
use crate::engine::{AlphaBeta, Engine, ScoreBound, SearchConfig, SearchOutcome, SearchParameters};
use crate::limits::Limits;
use crate::misc::Score;
use crate::play::Play;
use crate::recorder::{self, Sampled, Sampler};
use std::fmt;

/// About one record in every this many events, unless the command says
/// otherwise. The census's figure for the census's reason: this instrument
/// offers an event where the census does, so the stream has the same
/// density and the same rate fills a good share of the default cap at the
/// bench's depth. Declared here rather than shared, since a rate is a fact
/// about what an instrument is asking.
pub const DEFAULT_EVERY: u32 = 1_000;

/// The key a node's answer is sampled by, on either side. One lane, used by
/// both: a lane a side would be one instrument sampling two unrelated sets,
/// and the invariant the lanes exist for is about recorders armed at once,
/// which the two sides never are.
pub fn sample_key(position_key: u64, depth: u8) -> u64 {
    recorder::sample_key(position_key, recorder::EFFORT_LANE, depth)
}

/// The `SearchConfig` switches a run may name. Refused against this list
/// rather than given a flag each, which would cost an edit at every arm.
///
/// `taint` is not among them: it is a policy with four values rather than a
/// switch, and `residuals` already takes it.
pub const SWITCHES: [&str; 14] = [
    "reverse_futility",
    "null_move",
    "adaptive_null_move",
    "delta_margin",
    "see_pruning",
    "late_move_reductions",
    "deep_reductions",
    "late_move_pruning",
    "quiet_futility",
    "late_move_count",
    "reduction_table",
    "deep_index_rule",
    "move_memory",
    "aspiration",
];

/// The default with one switch off, which is the baseline side of a run, or
/// none when the name is not one of `SWITCHES`. The two are kept in step by
/// a test rather than by hand.
pub fn without(switch: &str) -> Option<SearchConfig> {
    let mut config = SearchConfig::default();
    match switch {
        "reverse_futility" => config.reverse_futility = false,
        "null_move" => config.null_move = false,
        "adaptive_null_move" => config.adaptive_null_move = false,
        "delta_margin" => config.delta_margin = false,
        "see_pruning" => config.see_pruning = false,
        "late_move_reductions" => config.late_move_reductions = false,
        "deep_reductions" => config.deep_reductions = false,
        "late_move_pruning" => config.late_move_pruning = false,
        "quiet_futility" => config.quiet_futility = false,
        "late_move_count" => config.late_move_count = false,
        "reduction_table" => config.reduction_table = false,
        "deep_index_rule" => config.deep_index_rule = false,
        "move_memory" => config.move_memory = false,
        "aspiration" => config.aspiration = false,
        _ => return None,
    }
    Some(config)
}

/// A compile error when `SearchConfig` gains a switch that `SWITCHES` and
/// `without` do not name.
///
/// The list and the match are held to each other by a test, which cannot
/// see a field in neither: `late_move_count` shipped and was unnameable
/// until this was written. Nothing here has a body to run, since naming
/// every field without a rest pattern is the whole check.
///
/// A new field fails to match. Rustc offers to silence that with a `_`,
/// which defeats the check: add the field to `SWITCHES` and to `without`
/// first, and name it here last.
#[cfg(test)]
const fn _every_switch_is_named(config: &SearchConfig) {
    let SearchConfig {
        taint: _,
        reverse_futility: _,
        null_move: _,
        adaptive_null_move: _,
        delta_margin: _,
        see_pruning: _,
        late_move_reductions: _,
        deep_reductions: _,
        late_move_pruning: _,
        quiet_futility: _,
        late_move_count: _,
        reduction_table: _,
        deep_index_rule: _,
        move_memory: _,
        // a measurement rather than a shortcut, off in the default, so the
        // baseline side has nothing to turn off
        ordering_exploration: _,
        exploration_seed: _,
        aspiration: _,
    } = config;
}

/// Every event offered, by depth, on one side.
///
/// Exact rather than estimated. Counting a node costs an array bump inside
/// a body that is already cold, where dividing the records by the rate
/// would carry sampling error into the headline and make it depend on
/// `every`. A slot for every `u8` so no bound has to be argued: two
/// kilobytes an engine, against a table of sixteen megabytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Depths([u64; 256]);

impl Default for Depths {
    fn default() -> Self {
        Depths([0; 256])
    }
}

impl Depths {
    /// One more event at this depth.
    pub(crate) fn count(&mut self, depth: u8) {
        self.0[usize::from(depth)] += 1;
    }

    /// What stands at one depth.
    pub fn at(&self, depth: u8) -> u64 {
        self.0[usize::from(depth)]
    }

    /// Add one engine's tally in, which is how a run totals its positions.
    fn absorb(&mut self, other: &Depths) {
        for (total, one) in self.0.iter_mut().zip(other.0) {
            *total += one;
        }
    }

    /// Every depth anything stands at, shallowest first.
    fn reached(&self) -> Vec<u8> {
        (0..=u8::MAX).filter(|depth| self.at(*depth) > 0).collect()
    }
}

/// One node's move loop answering, on one side of the run. Everything is
/// owned: an event outlives the search that took it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// The key it was drawn by, kept because the join is by the key and the
    /// reservoir hands its records back without them.
    pub key: u64,
    /// The position, as the board prints one.
    pub fen: String,
    /// The node's depth with its check extension, the other half of the
    /// key.
    pub depth: u8,
    /// Whether the node cut, rather than its move loop running out.
    pub cut: bool,
    /// Nodes spent under this node: the counter at its answer less the
    /// counter at its entry. Quiescence is in it, because the node counter
    /// counts quiescence.
    pub cost: u64,
}

/// The part of a fen the sampling key covers: the placement, the side to
/// move, the castling rights and the en passant file. The fifty move
/// counter and the move number are the two fields after, and the key covers
/// neither, so two visits at one key may differ there without being two
/// positions.
fn signature(fen: &str) -> &str {
    match fen.match_indices(' ').nth(3) {
        Some((at, _)) => &fen[..at],
        None => fen,
    }
}

/// One side's visits to one node at one depth, folded.
///
/// A deepening search revisits a position at a depth, so a run keys many
/// events alike. They are folded rather than printed one a visit: a row a
/// visit would need the iteration in the key, which would stop a node
/// joining at all wherever the rule changed how many iterations reached it,
/// and that is one of the things being measured.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Folded {
    key: u64,
    depth: u8,
    fen: String,
    visits: usize,
    cuts: usize,
    cost: u64,
    /// Whether the visits folded here disagreed about the node, which means
    /// two positions took one key.
    collided: bool,
}

/// One side's records folded by key. They arrive in key order, so the
/// visits of one node stand together.
fn fold(taken: Vec<Event>) -> Vec<Folded> {
    let mut folded: Vec<Folded> = Vec::new();
    for event in taken {
        match folded.last_mut() {
            Some(last) if last.key == event.key => {
                last.collided |=
                    last.depth != event.depth || signature(&last.fen) != signature(&event.fen);
                last.visits += 1;
                last.cuts += usize::from(event.cut);
                last.cost += event.cost;
            }
            _ => folded.push(Folded {
                key: event.key,
                depth: event.depth,
                visits: 1,
                cuts: usize::from(event.cut),
                cost: event.cost,
                fen: event.fen,
                collided: false,
            }),
        }
    }
    folded
}

/// Which of the two sides reached a joined node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Both sides reached it. The difference in what sat under it is effort
    /// the rule moved inside a node both trees hold.
    Both,
    /// The candidate reached it and the baseline never did: effort the rule
    /// created.
    OnlyOn,
    /// The baseline reached it and the candidate never did: effort the rule
    /// removed outright.
    OnlyOff,
}

impl Outcome {
    /// The word a row prints.
    pub fn word(self) -> &'static str {
        match self {
            Outcome::Both => "both",
            Outcome::OnlyOn => "only_on",
            Outcome::OnlyOff => "only_off",
        }
    }
}

/// One joined key: what each side did at this node at this depth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub depth: u8,
    pub outcome: Outcome,
    /// How many times that side's move loop answered this position at this
    /// depth over the whole deepening. A rule that changes how often a node
    /// is re-reached changes these, and that change is itself reallocation.
    pub visits_on: usize,
    pub visits_off: usize,
    /// How many of those visits ended in a cutoff rather than in the loop
    /// running out, so a rule that turns held nodes into cut ones is
    /// readable without a second run of the census.
    pub cuts_on: usize,
    pub cuts_off: usize,
    /// Nodes spent under the node, summed over that side's visits. The
    /// absent side of an `only_` row is 0 rather than the `-` the census
    /// prints for a column with no value: that side really spent nothing
    /// there, and 0 is what lets the column be summed.
    pub cost_on: u64,
    pub cost_off: u64,
    /// The candidate side's where the row has one and the baseline's
    /// otherwise. The key covers neither the fifty move counter nor the
    /// move number, so two sides at one key can carry different ones and
    /// the row's is one side's.
    pub fen: String,
}

impl Row {
    /// `cost_on - cost_off`, signed. Derivable and printed anyway, for the
    /// reason `census.rs` prints `index`: a row is read without re-deriving
    /// it.
    pub fn delta(&self) -> i64 {
        self.cost_on as i64 - self.cost_off as i64
    }
}

/// One position of the suite, read exactly on both sides. Not sampled: the
/// two node counts are each side's whole search, so at the same depth and
/// configuration the candidate's is the bench's, position by position.
#[derive(Clone, Debug)]
pub struct Reached {
    pub id: String,
    /// The deepest iteration that landed inside its window. Under a budget
    /// the two sides can differ, which is the whole of that mode's reading.
    pub depth_on: u8,
    pub depth_off: u8,
    /// The move the search would play and what it made of it.
    ///
    /// Under a budget these can come from an iteration deeper than
    /// `depth`: an iteration cut short answers with a move that beat its
    /// alpha, and the search answers with that rather than with the
    /// completed depth's. So the move is what the side would play and the
    /// depth is the deepest it finished, and the two are not always the
    /// same iteration's. The score is then a floor rather than a value.
    /// At equal depth, which is the default, no iteration is cut short and
    /// the question does not arise.
    pub best_on: Play,
    pub best_off: Play,
    pub score_on: Score,
    pub score_off: Score,
    pub nodes_on: u64,
    pub nodes_off: u64,
}

/// A whole run: what it was asked for, what each side counted, and a row a
/// joined key.
#[derive(Clone, Debug)]
pub struct Report {
    pub depth: u8,
    pub every: u32,
    /// The most events either side would keep. Stated in the header only
    /// when it is not the default.
    pub cap: usize,
    /// The file the suite was read from, or none for the bench's own.
    pub suite: Option<String>,
    /// The switch the baseline side turned off, or none for the null run.
    pub off: Option<String>,
    /// The node budget both sides searched under, or none for equal depth.
    pub budget: Option<u64>,
    pub positions: Vec<Reached>,
    /// Every event offered on each side, kept or not: the denominator the
    /// rows are read against.
    pub events_on: u64,
    pub events_off: u64,
    pub overflowed_on: u64,
    pub overflowed_off: u64,
    /// The exact per depth event counts, which are the measurement.
    pub nodes_on: Depths,
    pub nodes_off: Depths,
    /// The key the trim dropped from, or none where neither side
    /// overflowed and there was nothing to trim to.
    pub bound: Option<u64>,
    /// Joined keys the trim dropped.
    pub trimmed: usize,
    /// Joined keys two positions took, dropped.
    pub collisions: usize,
    pub rows: Vec<Row>,
}

/// One side's whole run: what its reservoir kept, its exact per depth
/// tally, and what each position's search answered.
struct Side {
    sampled: Sampled<Event>,
    depths: Depths,
    positions: Vec<Answered>,
}

/// One position's search on one side.
struct Answered {
    depth: u8,
    best: Play,
    score: Score,
    nodes: u64,
}

/// Search the suite once with the effort reservoir armed, under the
/// configuration given.
///
/// The engine, the table and the deepening are `bench::run_suite`'s, which
/// is what makes a position's node count the bench's at the same depth and
/// configuration. One reservoir for the whole suite, carried from each
/// position's engine to the next, as the other three recorders do.
fn side(
    positions: &[Position],
    depth: u8,
    every: u32,
    cap: usize,
    config: SearchConfig,
    budget: Option<u64>,
) -> Side {
    let mut sampler = Sampler::with_cap(every, cap);
    let mut depths = Depths::default();
    let mut answered = Vec::with_capacity(positions.len());
    for position in positions {
        let board = Board::from_fen(&position.fen)
            .unwrap_or_else(|e| panic!("effort position {} does not parse: {}", position.id, e));
        let mut engine = AlphaBeta::with_config(board, bench::TABLE_BYTES, config);
        engine.arm(sampler);
        // the deepest iteration that landed inside its window. A budget can
        // stop one partway, and the depth that reports is a floor under the
        // position rather than an answer to it
        let mut reached = 0;
        let outcome = engine.iterative_deepening_search(
            SearchParameters::new(
                Some(depth),
                match budget {
                    None => Limits::unlimited(),
                    Some(nodes) => Limits::starting_now(None, Some(nodes)),
                },
            ),
            |at, _, _, bound| {
                if bound == ScoreBound::Exact {
                    reached = at;
                }
            },
        );
        sampler = engine
            .disarm()
            .expect("the reservoir just handed to the engine comes back");
        depths.absorb(engine.effort_tally());
        let result = match outcome {
            SearchOutcome::Complete(result, _) | SearchOutcome::Aborted(Some(result)) => result,
            // a budget too small to finish depth one, or a root with no
            // move. Neither is a reading, and both say so on their own row
            other => panic!("effort position {} answered {:?}", position.id, other),
        };
        answered.push(Answered {
            depth: reached,
            best: result.best_move,
            score: result.score,
            nodes: result.nodes,
        });
    }
    Side {
        sampled: sampler.drain(),
        depths,
        positions: answered,
    }
}

/// The largest key a side's retained set reaches to, or none where it
/// reaches as far as the rate allows.
///
/// A side that overflowed gave up its largest keys, so it holds every key
/// it was offered at or below the largest it kept and nothing above. A side
/// that did not overflow kept every key the rate wanted, so it bounds
/// nothing and the trim below has nothing to do.
///
/// A side that overflowed and kept nothing reaches no key at all, so it
/// bounds at zero and the join keeps none. That is a cap of nothing, where
/// both sides are empty and there is nothing to mis-attribute either way,
/// but the answer is written down rather than left to the caller: read as
/// unbounded it would pass every row of the other side through as an
/// `only_` one.
fn retained_to(sampled: &Sampled<Event>) -> Option<u64> {
    if sampled.overflowed == 0 {
        return None;
    }
    Some(sampled.taken.last().map_or(0, |event| event.key))
}

/// Walk the two sides' folded rows together and say what each key reads as.
///
/// Both lists are in key order, so this is a merge. A key at or above the
/// trim bound is dropped from both sides before it is read: a key kept on
/// one side and dropped on the other by the buffer alone would read as
/// `only_on` or `only_off`, which is the instrument's own finding
/// manufactured. A key two positions took is dropped after that, since a
/// row that is two positions is not a reading and a run of minutes is not
/// worth aborting over one.
fn join(on: Vec<Folded>, off: Vec<Folded>, bound: Option<u64>) -> (Vec<Row>, usize, usize) {
    /// Whichever side reached the key, the candidate first. One of the two
    /// is always there, since a key is here because a side held it.
    fn seen<'a>(candidate: &'a Option<Folded>, baseline: &'a Option<Folded>) -> &'a Folded {
        candidate
            .as_ref()
            .or(baseline.as_ref())
            .expect("one side of a joined key is always there")
    }

    let mut rows = Vec::new();
    let mut trimmed = 0;
    let mut collisions = 0;
    let mut on = on.into_iter().peekable();
    let mut off = off.into_iter().peekable();
    loop {
        let (left, right) = (on.peek().map(|f| f.key), off.peek().map(|f| f.key));
        let (candidate, baseline) = match (left, right) {
            (None, None) => break,
            (Some(_), None) => (on.next(), None),
            (None, Some(_)) => (None, off.next()),
            (Some(a), Some(b)) if a < b => (on.next(), None),
            (Some(a), Some(b)) if a > b => (None, off.next()),
            _ => (on.next(), off.next()),
        };
        let key = seen(&candidate, &baseline).key;
        if bound.is_some_and(|bound| key >= bound) {
            trimmed += 1;
            continue;
        }
        let agree = match (&candidate, &baseline) {
            (Some(a), Some(b)) => a.depth == b.depth && signature(&a.fen) == signature(&b.fen),
            _ => true,
        };
        if !agree
            || candidate.as_ref().is_some_and(|f| f.collided)
            || baseline.as_ref().is_some_and(|f| f.collided)
        {
            collisions += 1;
            continue;
        }
        let outcome = match (&candidate, &baseline) {
            (Some(_), Some(_)) => Outcome::Both,
            (Some(_), None) => Outcome::OnlyOn,
            _ => Outcome::OnlyOff,
        };
        let row = Row {
            depth: seen(&candidate, &baseline).depth,
            outcome,
            visits_on: candidate.as_ref().map_or(0, |f| f.visits),
            visits_off: baseline.as_ref().map_or(0, |f| f.visits),
            cuts_on: candidate.as_ref().map_or(0, |f| f.cuts),
            cuts_off: baseline.as_ref().map_or(0, |f| f.cuts),
            cost_on: candidate.as_ref().map_or(0, |f| f.cost),
            cost_off: baseline.as_ref().map_or(0, |f| f.cost),
            // moved rather than copied: the fold's row is dropped here, and
            // a whole population run joins millions of keys
            fen: match candidate {
                Some(f) => f.fen,
                None => {
                    baseline
                        .expect("one side of a joined key is always there")
                        .fen
                }
            },
        };
        rows.push(row);
    }
    (rows, trimmed, collisions)
}

/// Search the suite twice and join the two runs by the node.
///
/// The candidate side is the default. The baseline is the default with
/// `off` set false, or the default again when nothing is named, which is
/// the null run: every joined key must then read `both` with a delta of
/// zero, and an instrument that fails that is measuring its own buffer.
/// `off` has been refused against `SWITCHES` by the caller, since a run
/// takes minutes and a misspelling read as the null would spend them saying
/// nothing.
#[allow(clippy::too_many_arguments)]
pub fn run(
    positions: &[Position],
    suite: Option<&str>,
    depth: u8,
    every: u32,
    cap: usize,
    off: Option<&str>,
    budget: Option<u64>,
) -> Report {
    let depth = depth.max(1);
    // the rate the reservoirs will really keep to, so the header states the
    // run that happened
    let every = every.max(1);
    let baseline = match off {
        None => SearchConfig::default(),
        Some(switch) => {
            without(switch).unwrap_or_else(|| panic!("{switch} is not a search switch"))
        }
    };
    let on = side(
        positions,
        depth,
        every,
        cap,
        SearchConfig::default(),
        budget,
    );
    let off_side = side(positions, depth, every, cap, baseline, budget);
    let bound = match (retained_to(&on.sampled), retained_to(&off_side.sampled)) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (bound, None) | (None, bound) => bound,
    };
    let (rows, trimmed, collisions) =
        join(fold(on.sampled.taken), fold(off_side.sampled.taken), bound);
    Report {
        depth,
        every,
        cap,
        suite: suite.map(str::to_string),
        off: off.map(str::to_string),
        budget,
        positions: positions
            .iter()
            .zip(on.positions)
            .zip(off_side.positions)
            .map(|((position, on), off)| Reached {
                id: position.id.clone(),
                depth_on: on.depth,
                depth_off: off.depth,
                best_on: on.best,
                best_off: off.best,
                score_on: on.score,
                score_off: off.score,
                nodes_on: on.nodes,
                nodes_off: off.nodes,
            })
            .collect(),
        events_on: on.sampled.events,
        events_off: off_side.sampled.events,
        overflowed_on: on.sampled.overflowed,
        overflowed_off: off_side.sampled.overflowed,
        nodes_on: on.depths,
        nodes_off: off_side.depths,
        bound,
        trimmed,
        collisions,
        rows,
    }
}

/// One depth's line. By depth and not pooled, as the census's is: the
/// depths are reached in wildly different numbers, so a pooled figure is
/// the shallowest depth's wearing every depth's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    pub depth: u8,
    /// Exact, from the tallies rather than from the rows.
    pub nodes_on: u64,
    pub nodes_off: u64,
    /// Sampled, from the rows.
    pub records: usize,
    pub both: usize,
    pub only_on: usize,
    pub only_off: usize,
    pub cost_on: u64,
    pub cost_off: u64,
}

impl Report {
    /// One depth's summary, or none where neither side offered an event at
    /// it and no row stands there.
    pub fn summary(&self, depth: u8) -> Option<Summary> {
        let rows = self.rows.iter().filter(|row| row.depth == depth);
        let mut counted = Summary {
            depth,
            nodes_on: self.nodes_on.at(depth),
            nodes_off: self.nodes_off.at(depth),
            records: 0,
            both: 0,
            only_on: 0,
            only_off: 0,
            cost_on: 0,
            cost_off: 0,
        };
        for row in rows {
            counted.records += 1;
            match row.outcome {
                Outcome::Both => counted.both += 1,
                Outcome::OnlyOn => counted.only_on += 1,
                Outcome::OnlyOff => counted.only_off += 1,
            }
            counted.cost_on += row.cost_on;
            counted.cost_off += row.cost_off;
        }
        if counted.records == 0 && counted.nodes_on == 0 && counted.nodes_off == 0 {
            return None;
        }
        Some(counted)
    }

    /// Every summary the run has, shallowest depth first. The tallies are
    /// exact, so a depth either side reached has a line even where the
    /// sampling kept no row of it.
    pub fn summaries(&self) -> Vec<Summary> {
        let mut depths = self.nodes_on.reached();
        depths.extend(self.nodes_off.reached());
        depths.extend(self.rows.iter().map(|row| row.depth));
        depths.sort_unstable();
        depths.dedup();
        depths
            .into_iter()
            .filter_map(|depth| self.summary(depth))
            .collect()
    }
}

/// A signed change as a share of what the baseline spent, under the same
/// rule as `recorder::share`: a figure with no denominator is not a zero.
fn signed_share(delta: i64, of: u64) -> String {
    if of == 0 {
        "-".to_string()
    } else {
        format!("{:+.2}%", 100.0 * delta as f64 / of as f64)
    }
}

/// The report as the command prints it: a header, a row a joined key, a
/// summary line a depth and a line a position.
///
/// A row is `depth outcome visits_on visits_off cuts_on cuts_off cost_on
/// cost_off delta fen`, whitespace separated with the fen last, so a row
/// parses left to right. No column prints `-`: the absent side of an
/// `only_` row spent nothing rather than having no value.
///
/// The header extends the three searching instruments' rather than
/// matching it. It names the switch, states an events count a side, and
/// says what the trim and the collision guard dropped, none of which the
/// other three have anything to say about.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "effort depth {} every {}", self.depth, self.every)?;
        if self.cap != recorder::DEFAULT_CAP {
            write!(f, " cap {}", self.cap)?;
        }
        if let Some(suite) = &self.suite {
            write!(f, " epd {}", suite)?;
        }
        // stated either way, because `off none` is the null run and the
        // null run is a reading rather than an accident of the parser
        write!(f, " off {}", self.off.as_deref().unwrap_or("none"))?;
        if let Some(budget) = self.budget {
            write!(f, " budget {}", budget)?;
        }
        write!(
            f,
            " positions {} events on {} off {} records {}",
            self.positions.len(),
            self.events_on,
            self.events_off,
            self.rows.len(),
        )?;
        if let Some(bound) = self.bound {
            write!(f, " bound {}", bound)?;
        }
        write!(
            f,
            " trimmed {} collisions {}",
            self.trimmed, self.collisions
        )?;
        if self.overflowed_on > 0 || self.overflowed_off > 0 {
            write!(
                f,
                " overflow on {} off {}",
                self.overflowed_on, self.overflowed_off
            )?;
        }
        writeln!(f)?;
        for row in &self.rows {
            writeln!(
                f,
                "{} {} {} {} {} {} {} {} {} {}",
                row.depth,
                row.outcome.word(),
                row.visits_on,
                row.visits_off,
                row.cuts_on,
                row.cuts_off,
                row.cost_on,
                row.cost_off,
                row.delta(),
                row.fen,
            )?;
        }
        writeln!(f)?;
        writeln!(f, "summary")?;
        let summaries = self.summaries();
        // said rather than left out: a run that reached nothing is a fact
        if summaries.is_empty() {
            writeln!(f, "records 0")?;
        }
        for s in summaries {
            let nodes = s.nodes_on as i64 - s.nodes_off as i64;
            // the node counts add down the depths and the costs do not: a
            // depth three node's cost holds its depth one descendants'. So
            // the counts are the headline and the costs stand beside them
            writeln!(
                f,
                "depth {} nodes on {} off {} delta {} {} records {} both {} only_on {} \
                 only_off {} cost on {} off {} delta {}",
                s.depth,
                s.nodes_on,
                s.nodes_off,
                nodes,
                signed_share(nodes, s.nodes_off),
                s.records,
                s.both,
                s.only_on,
                s.only_off,
                s.cost_on,
                s.cost_off,
                s.cost_on as i64 - s.cost_off as i64,
            )?;
        }
        // the id last for the fen's reason: an epd id is whatever stood in
        // the quotes, and one of the bench's is six fields long
        for p in &self.positions {
            writeln!(
                f,
                "position reached on {} off {} best on {} off {} agree {} score on {} off {} \
                 nodes on {} off {} {}",
                p.depth_on,
                p.depth_off,
                p.best_on,
                p.best_off,
                if p.best_on == p.best_off { "yes" } else { "no" },
                p.score_on,
                p.score_off,
                p.nodes_on,
                p.nodes_off,
                p.id,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::DEFAULT_CAP;
    use crate::recorder::fixtures::{recording_leaves_the_search_where_it_was, suite};
    use pretty_assertions::assert_eq;

    /// The key decides on the node alone, and the same node keys
    /// differently here from under every other lane.
    #[test]
    fn a_key_is_the_node_and_nothing_about_the_run() {
        let position = 0x0123_4567_89ab_cdef;
        let key = sample_key(position, 4);
        assert_eq!(key, sample_key(position, 4));
        assert_ne!(key, sample_key(position, 5));
        assert_ne!(key, sample_key(position ^ 1, 4));
        assert_ne!(key, crate::census::sample_key(position, 4));
        assert_ne!(key, crate::reduction::sample_key(position, 4));
        for kind in crate::residual::Shortcut::KINDS {
            assert_ne!(key, crate::residual::sample_key(position, kind, 4));
        }
    }

    /// Every name the command refuses against turns a switch off, and no
    /// two names turn the same one off. Written as a loop so the list and
    /// the match cannot drift: a name added to one and not the other fails
    /// here.
    #[test]
    fn every_switch_named_turns_one_of_its_own_off() {
        let default = SearchConfig::default();
        let mut seen: Vec<SearchConfig> = Vec::new();
        for switch in SWITCHES {
            let config = without(switch).unwrap_or_else(|| panic!("{switch} is not taken"));
            assert_ne!(config, default, "{switch} turned nothing off");
            assert!(!seen.contains(&config), "{switch} repeats another switch");
            seen.push(config);
        }
        assert_eq!(without("taint"), None);
        assert_eq!(without("quiet_futilty"), None);
        assert_eq!(without(""), None);
    }

    /// The four fields the key covers, and the two after it that it does
    /// not.
    #[test]
    fn a_signature_is_the_fen_the_key_covers_and_no_more() {
        let one = "r3k2r/8/8/8/8/8/8/R3K2R w KQkq e3 0 1";
        let counted = "r3k2r/8/8/8/8/8/8/R3K2R w KQkq e3 47 63";
        assert_eq!(signature(one), "r3k2r/8/8/8/8/8/8/R3K2R w KQkq e3");
        assert_eq!(signature(one), signature(counted));
        assert_ne!(
            signature(one),
            signature("r3k2r/8/8/8/8/8/8/R3K2R b KQkq e3 0 1")
        );
        // a fen with fewer fields than four is nobody's, and reads whole
        assert_eq!(signature("8/8/8/8"), "8/8/8/8");
    }

    fn event(key: u64, depth: u8, cut: bool, cost: u64, fen: &str) -> Event {
        Event {
            key,
            fen: fen.to_string(),
            depth,
            cut,
            cost,
        }
    }

    const ONE: &str = "4k3/8/8/8/8/8/8/4K3 w - - 0 1";
    const TWO: &str = "4k3/8/8/8/8/8/8/4K3 b - - 0 1";

    /// A deepening search revisits a node, so the visits of one key fold
    /// into one row with the cost summed and the cuts counted.
    #[test]
    fn a_keys_visits_fold_into_one_row() {
        let folded = fold(vec![
            event(10, 3, true, 40, ONE),
            event(10, 3, false, 60, ONE),
            event(20, 2, true, 7, TWO),
        ]);
        assert_eq!(folded.len(), 2);
        assert_eq!(
            (folded[0].visits, folded[0].cuts, folded[0].cost),
            (2, 1, 100)
        );
        assert!(!folded[0].collided);
        assert_eq!(
            (folded[1].visits, folded[1].cuts, folded[1].cost),
            (1, 1, 7)
        );
    }

    /// Two positions on one key inside a side fold together, which is the
    /// same defect as two sides disagreeing and gets the same answer.
    #[test]
    fn two_positions_folded_under_one_key_are_a_collision() {
        let folded = fold(vec![
            event(10, 3, true, 40, ONE),
            event(10, 3, false, 60, TWO),
        ]);
        assert_eq!(folded.len(), 1);
        assert!(folded[0].collided);
        let (rows, trimmed, collisions) = join(folded, Vec::new(), None);
        assert!(rows.is_empty());
        assert_eq!((trimmed, collisions), (0, 1));
    }

    /// The three outcomes, off a merge of two sides that part company in
    /// both directions.
    #[test]
    fn a_joined_key_reads_as_one_of_three_outcomes() {
        let on = fold(vec![
            event(10, 3, true, 40, ONE),
            event(30, 2, false, 5, TWO),
        ]);
        let off = fold(vec![
            event(10, 3, false, 90, ONE),
            event(20, 1, true, 8, TWO),
        ]);
        let (rows, trimmed, collisions) = join(on, off, None);
        assert_eq!((trimmed, collisions), (0, 0));
        let read: Vec<(Outcome, u64, u64, i64)> = rows
            .iter()
            .map(|row| (row.outcome, row.cost_on, row.cost_off, row.delta()))
            .collect();
        assert_eq!(
            read,
            vec![
                (Outcome::Both, 40, 90, -50),
                (Outcome::OnlyOff, 0, 8, -8),
                (Outcome::OnlyOn, 5, 0, 5),
            ]
        );
        // the absent side prints a nothing it really spent
        assert_eq!((rows[1].visits_on, rows[1].cuts_on), (0, 0));
    }

    /// Two sides at one key that are two positions are dropped rather than
    /// read, and counted where a reader can see them.
    #[test]
    fn a_key_two_positions_took_is_dropped() {
        let on = fold(vec![event(10, 3, true, 40, ONE)]);
        let off = fold(vec![event(10, 3, true, 40, TWO)]);
        let (rows, _, collisions) = join(on, off, None);
        assert!(rows.is_empty());
        assert_eq!(collisions, 1);
    }

    /// The trim drops from the bound up on both sides, so a key one side's
    /// buffer gave up cannot read as a rule removing a node.
    #[test]
    fn the_trim_drops_every_key_at_or_above_the_bound() {
        let on = fold(vec![
            event(10, 3, true, 40, ONE),
            event(50, 2, true, 9, ONE),
        ]);
        let off = fold(vec![event(10, 3, true, 40, ONE)]);
        let (rows, trimmed, _) = join(on, off, Some(50));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, Outcome::Both);
        assert_eq!(trimmed, 1);
    }

    /// A side that did not overflow kept everything the rate wanted, so it
    /// bounds nothing and a run where neither did trims nothing.
    #[test]
    fn only_an_overflowing_side_bounds_the_trim() {
        let full = Sampled {
            taken: vec![event(10, 3, true, 40, ONE)],
            events: 100,
            overflowed: 7,
        };
        let roomy = Sampled {
            taken: vec![event(90, 3, true, 40, ONE)],
            events: 100,
            overflowed: 0,
        };
        assert_eq!(retained_to(&full), Some(10));
        assert_eq!(retained_to(&roomy), None);
    }

    fn row(depth: u8, outcome: Outcome, cost_on: u64, cost_off: u64) -> Row {
        Row {
            depth,
            outcome,
            visits_on: 1,
            visits_off: 1,
            cuts_on: 1,
            cuts_off: 0,
            cost_on,
            cost_off,
            fen: ONE.to_string(),
        }
    }

    fn report_of(rows: Vec<Row>) -> Report {
        let mut nodes_on = Depths::default();
        let mut nodes_off = Depths::default();
        for _ in 0..1000 {
            nodes_on.count(3);
        }
        for _ in 0..1040 {
            nodes_off.count(3);
        }
        Report {
            depth: 4,
            every: 10,
            cap: DEFAULT_CAP,
            suite: None,
            off: Some("quiet_futility".to_string()),
            budget: None,
            positions: Vec::new(),
            events_on: 400,
            events_off: 410,
            overflowed_on: 0,
            overflowed_off: 0,
            nodes_on,
            nodes_off,
            bound: None,
            trimmed: 0,
            collisions: 0,
            rows,
        }
    }

    /// The row's fields in the order the `Display` doc names them, with the
    /// fen last and the delta printed rather than left to the reader.
    #[test]
    fn a_row_reads_left_to_right_with_the_fen_last() {
        let report = report_of(vec![row(3, Outcome::OnlyOn, 214, 0)]);
        let text = report.to_string();
        let words: Vec<&str> = text
            .lines()
            .nth(1)
            .expect("a row")
            .splitn(10, ' ')
            .collect();
        assert_eq!(
            words,
            vec!["3", "only_on", "1", "1", "1", "0", "214", "0", "214", ONE]
        );
    }

    /// The header names the switch, states an events count a side, and says
    /// what the two guards dropped.
    #[test]
    fn the_header_states_the_switch_and_both_sides_events() {
        let mut report = report_of(Vec::new());
        assert!(
            report.to_string().starts_with(
                "effort depth 4 every 10 off quiet_futility positions 0 \
                 events on 400 off 410 records 0 trimmed 0 collisions 0\n"
            ),
            "{}",
            report
        );
        // the null run says so rather than leaving the word out
        report.off = None;
        report.cap = 25;
        report.budget = Some(4_000_000);
        report.bound = Some(77);
        report.trimmed = 3;
        report.collisions = 1;
        report.overflowed_on = 12;
        assert!(
            report.to_string().starts_with(
                "effort depth 4 every 10 cap 25 off none budget 4000000 positions 0 \
                 events on 400 off 410 records 0 bound 77 trimmed 3 collisions 1 \
                 overflow on 12 off 0\n"
            ),
            "{}",
            report
        );
    }

    /// The depth line's node counts come from the exact tallies and its
    /// records from the sampled rows, and the two are printed apart.
    #[test]
    fn a_depth_line_prints_the_exact_counts_beside_the_sampled_rows() {
        let report = report_of(vec![
            row(3, Outcome::Both, 40, 90),
            row(3, Outcome::OnlyOff, 0, 8),
        ]);
        let summary = report.summary(3).expect("rows at depth three");
        assert_eq!((summary.nodes_on, summary.nodes_off), (1000, 1040));
        assert_eq!(
            (
                summary.records,
                summary.both,
                summary.only_on,
                summary.only_off
            ),
            (2, 1, 0, 1)
        );
        assert!(
            report.to_string().contains(
                "depth 3 nodes on 1000 off 1040 delta -40 -3.85% records 2 both 1 only_on 0 \
                 only_off 1 cost on 40 off 98 delta -58"
            ),
            "{}",
            report
        );
    }

    /// A depth either side offered an event at has a line even where the
    /// rate kept no row of it, since the tallies are the measurement.
    #[test]
    fn a_depth_with_no_rows_is_still_counted() {
        let report = report_of(Vec::new());
        let depths: Vec<u8> = report.summaries().iter().map(|s| s.depth).collect();
        assert_eq!(depths, vec![3]);
        assert!(report.summary(2).is_none());
        assert!(
            report
                .to_string()
                .contains("depth 3 nodes on 1000 off 1040 "),
            "{}",
            report
        );
    }

    /// A depth the baseline never reached has no denominator, so the share
    /// is a dash rather than an invented figure.
    #[test]
    fn a_share_with_no_baseline_under_it_is_a_dash() {
        assert_eq!(signed_share(0, 0), "-");
        assert_eq!(signed_share(-40, 1040), "-3.85%");
        assert_eq!(signed_share(0, 1040), "+0.00%");
        assert_eq!(signed_share(52, 1040), "+5.00%");
    }

    /// The null run: two identical configurations reach the same nodes and
    /// spend the same effort under them, so every joined key reads `both`
    /// with a zero delta and the per depth counts are equal. An instrument
    /// that fails this is reading the reservoir rather than the tree.
    #[test]
    fn the_null_run_reads_both_at_every_key_with_no_delta() {
        let report = run(&suite(), None, 4, 1, DEFAULT_CAP, None, None);
        assert!(!report.rows.is_empty(), "nothing was recorded");
        assert_eq!(report.collisions, 0);
        assert_eq!(report.trimmed, 0);
        assert_eq!(report.events_on, report.events_off);
        for row in &report.rows {
            assert_eq!(row.outcome, Outcome::Both, "{:?}", row);
            assert_eq!(row.delta(), 0, "{:?}", row);
            assert_eq!(row.visits_on, row.visits_off, "{:?}", row);
            assert_eq!(row.cuts_on, row.cuts_off, "{:?}", row);
        }
        for s in report.summaries() {
            assert_eq!(s.nodes_on, s.nodes_off, "depth {}", s.depth);
            assert_eq!(s.only_on, 0, "depth {}", s.depth);
            assert_eq!(s.only_off, 0, "depth {}", s.depth);
        }
        for p in &report.positions {
            assert_eq!(p.nodes_on, p.nodes_off, "{}", p.id);
            assert_eq!(p.best_on, p.best_off, "{}", p.id);
            assert_eq!(p.depth_on, p.depth_off, "{}", p.id);
        }
    }

    /// The per depth counts are exact counters rather than a share of the
    /// records, so they do not move with the rate where the record count
    /// does.
    #[test]
    fn the_node_counts_do_not_move_with_the_rate() {
        let dense = run(&suite(), None, 4, 1, DEFAULT_CAP, None, None);
        let sparse = run(&suite(), None, 4, 50, DEFAULT_CAP, None, None);
        assert_eq!(dense.events_on, sparse.events_on);
        for depth in 1..=4 {
            assert_eq!(
                dense.nodes_on.at(depth),
                sparse.nodes_on.at(depth),
                "depth {depth}"
            );
        }
        assert!(sparse.rows.len() < dense.rows.len());
    }

    /// What makes a depth line exact: the per depth tallies are a partition
    /// of the events the header states, so they add to it. A fifth offer
    /// site added without a matching tally, or a tally bumped twice, breaks
    /// this and nothing else would say so.
    #[test]
    fn the_depth_tallies_add_to_the_events_offered() {
        let report = run(
            &suite(),
            None,
            5,
            50,
            DEFAULT_CAP,
            Some("quiet_futility"),
            None,
        );
        let total = |depths: &Depths| -> u64 { (0..=u8::MAX).map(|d| depths.at(d)).sum() };
        assert_eq!(total(&report.nodes_on), report.events_on);
        assert_eq!(total(&report.nodes_off), report.events_off);
        assert!(report.events_on > 0);
    }

    /// The rows are the census's population. The two instruments offer an
    /// event at the same three places, so armed at the same depth over the
    /// same suite they count the same events, and the docs say as much.
    #[test]
    fn the_events_are_the_censuss_own_population() {
        let census = crate::census::run(&suite(), 5, 1000, DEFAULT_CAP);
        let report = run(&suite(), None, 5, 1000, DEFAULT_CAP, None, None);
        assert_eq!(report.events_on, census.events);
        assert_eq!(report.events_off, census.events);
    }

    /// Two runs of the same command print the same rows: the key is the
    /// node's and the reservoir is order independent.
    #[test]
    fn two_runs_of_one_command_print_the_same_rows() {
        let once = run(&suite(), None, 4, 10, DEFAULT_CAP, None, None);
        let again = run(&suite(), None, 4, 10, DEFAULT_CAP, None, None);
        assert_eq!(once.rows, again.rows);
        assert_eq!(once.events_on, again.events_on);
    }

    /// A switch off moves the tree, which is what the instrument is for.
    /// Reverse futility is the switch here because it answers whole nodes,
    /// so the two sides hold different ones at a depth a test can afford.
    #[test]
    fn a_switch_off_parts_the_two_sides() {
        let report = run(
            &suite(),
            None,
            4,
            1,
            DEFAULT_CAP,
            Some("reverse_futility"),
            None,
        );
        assert!(
            report.positions.iter().any(|p| p.nodes_on < p.nodes_off),
            "the switch removed nothing"
        );
        let mut outcomes = (0, 0, 0);
        for row in &report.rows {
            match row.outcome {
                Outcome::Both => outcomes.0 += 1,
                Outcome::OnlyOn => outcomes.1 += 1,
                Outcome::OnlyOff => outcomes.2 += 1,
            }
        }
        // all three, the created population included
        assert!(
            outcomes.0 > 0 && outcomes.1 > 0 && outcomes.2 > 0,
            "{outcomes:?}"
        );
        assert!(report.rows.iter().any(|row| row.delta() != 0));
    }

    /// The other half of a reading, and the reason the cost columns are not
    /// derivable from the outcome. The quiet futility margin decides at
    /// `SHALLOW_MAX_DEPTH` and under, so it parts the two sides there: a
    /// quiet child the rule cut is a node the candidate never reached, and
    /// that row reads `only_off`. Above that depth the two sides hold the
    /// same nodes and the tree the rule removed sits under them, so those
    /// rows read `both` and the effort still moved. A run that read
    /// outcomes alone would call that no change.
    ///
    /// The rows are read at the depths the rule leaves alone for that
    /// reason. Read over every depth the assertion is about the shallow
    /// rows as well, where a parted side is the instrument working.
    #[test]
    fn a_rule_can_move_effort_without_moving_a_node() {
        let report = run(
            &suite(),
            None,
            4,
            1,
            DEFAULT_CAP,
            Some("quiet_futility"),
            None,
        );
        let above: Vec<&Row> = report
            .rows
            .iter()
            .filter(|row| row.depth > crate::late_move::SHALLOW_MAX_DEPTH)
            .collect();
        // without this the three below hold over an empty set
        assert!(
            !above.is_empty(),
            "no row above the depths the rule decides at"
        );
        assert!(above.iter().all(|row| row.outcome == Outcome::Both));
        assert!(
            above.iter().any(|row| row.delta() != 0),
            "the switch moved no effort either"
        );
        assert!(report.positions.iter().any(|p| p.nodes_on < p.nodes_off));
    }

    /// The per position node counts are each side's whole search, so the
    /// candidate's are `bench::run_suite`'s at the same depth and
    /// configuration. That is what the bench delta is read against.
    #[test]
    fn a_positions_node_count_is_the_benchs_own() {
        let positions = bench::positions();
        let report = run(&positions, None, 4, 1000, recorder::DEFAULT_CAP, None, None);
        let bench = bench::run_suite(&positions, 4, bench::TABLE_BYTES, SearchConfig::default());
        let counted: Vec<(&str, u64)> = report
            .positions
            .iter()
            .map(|p| (p.id.as_str(), p.nodes_on))
            .collect();
        let expected: Vec<(&str, u64)> = bench
            .positions
            .iter()
            .map(|p| (p.id.as_str(), p.nodes))
            .collect();
        assert_eq!(counted, expected);
    }

    /// A budget holds both sides to the same nodes, so what is left to read
    /// is the depth each reached and the move each chose.
    #[test]
    fn a_budget_stops_both_sides_at_the_same_spending() {
        const BUDGET: u64 = 20_000;
        // a real switch, so the two sides are configurations that differ and
        // the assertions below are about both of them rather than about one
        // run read twice
        let report = run(
            &suite(),
            None,
            9,
            1000,
            DEFAULT_CAP,
            Some("quiet_futility"),
            Some(BUDGET),
        );
        for p in &report.positions {
            // exact, not approximate: the budget is checked on the node, and
            // an iteration given up part way counts towards it
            assert_eq!(p.nodes_on, BUDGET, "{}", p.id);
            assert_eq!(p.nodes_off, BUDGET, "{}", p.id);
            assert!(p.depth_on >= 1 && p.depth_off >= 1, "{}", p.id);
            assert!(
                p.depth_on < 9 && p.depth_off < 9,
                "{} reached the depth inside the budget",
                p.id
            );
        }
    }

    /// The recorders' shared contract, asked of this instrument under each
    /// of its two configurations, since it is the only one that searches
    /// under a configuration its caller chose.
    #[test]
    fn recording_leaves_the_measured_search_where_it_was() {
        recording_leaves_the_search_where_it_was(
            4,
            SearchConfig::default(),
            |engine| engine.arm(Sampler::<Event>::with_cap(1, DEFAULT_CAP)),
            |engine| {
                engine
                    .disarm::<Event>()
                    .expect("the reservoir comes back")
                    .drain()
                    .taken
                    .len()
            },
        );
    }

    /// The same, under the baseline side of a real arm. There is no pinned
    /// count for a configuration with a switch off and there must not be
    /// one: an instrument that pinned a count for every switch would be
    /// edited by every arm. What is asserted is armed equals disarmed under
    /// whatever configuration it is handed.
    #[test]
    fn recording_changes_nothing_under_the_baseline_configuration_either() {
        let config = without("quiet_futility").expect("a switch of the list");
        let mut kept = 0;
        for position in suite() {
            let board = Board::from_fen(&position.fen).unwrap();
            let searched = |engine: &mut AlphaBeta| match engine
                .iterative_deepening_search(SearchParameters::to_depth(4), |_, _, _, _| {})
            {
                SearchOutcome::Complete(result, _) => result.nodes,
                other => panic!("{}: {:?}", position.id, other),
            };
            let mut plain = AlphaBeta::with_config(board.clone(), bench::TABLE_BYTES, config);
            let plain_nodes = searched(&mut plain);
            let mut armed = AlphaBeta::with_config(board, bench::TABLE_BYTES, config);
            armed.arm(Sampler::<Event>::with_cap(1, DEFAULT_CAP));
            assert_eq!(searched(&mut armed), plain_nodes, "{}", position.id);
            kept += armed
                .disarm::<Event>()
                .expect("the reservoir comes back")
                .drain()
                .taken
                .len();
        }
        assert!(kept > 0, "the armed runs recorded nothing");
    }

    /// A rate of zero and a depth of zero are held to one, and the header
    /// states the run that happened.
    #[test]
    fn a_rate_of_zero_is_reported_as_the_rate_that_ran() {
        let report = run(&suite(), None, 0, 0, 50, None, None);
        assert_eq!(report.depth, 1);
        assert_eq!(report.every, 1);
        assert!(
            report
                .to_string()
                .starts_with("effort depth 1 every 1 cap 50 off none positions 2 "),
            "{}",
            report
        );
    }
}
