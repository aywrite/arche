// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What the search's shortcuts cost in accuracy.
//!
//! A shortcut answers a node from something cheaper than searching it: a
//! margin above beta, or a pass searched shallow. Both are guesses, and the
//! question this module asks is how far off the guesses are. The residual is
//! the reference search's answer to the node less the score the shortcut
//! claimed for it, and a distribution of those, by kind and by depth, is what
//! a later change to either shortcut has to be argued from.
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
use std::fmt;

/// The shortcut that answered a node.
///
/// Two of them today, which are the two the default configuration turns on.
/// A third would be one arm here and one call at wherever it returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    /// The node stood far enough above beta by its static evaluation alone.
    ReverseFutility,
    /// A reduced search of the position left by passing came back above beta.
    NullMove,
}

impl Shortcut {
    /// The kinds, for a reader summarising by them.
    pub const KINDS: [Shortcut; 2] = [Shortcut::ReverseFutility, Shortcut::NullMove];

    /// The word a row prints, which names the `SearchConfig` switch that
    /// turns the shortcut on.
    pub fn word(self) -> &'static str {
        match self {
            Shortcut::ReverseFutility => "reverse_futility",
            Shortcut::NullMove => "null_move",
        }
    }
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
    /// How far the static evaluation stood above beta when the shortcut
    /// fired, which is the margin each of them is really betting on. Widened
    /// past a `Score` because the difference of two of them is not one.
    pub eval_beta: i32,
}

/// What a sampler collected, taken away from it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sampled {
    pub taken: Vec<Sample>,
    /// Samples the buffer had no room for. Counted rather than kept, so a
    /// long run says how much of itself it is not describing.
    pub overflowed: u64,
}

/// Records every nth node a shortcut answers.
///
/// Deterministic: a countdown, not a draw, so two runs of the same search
/// record the same nodes and a distribution can be reproduced from the
/// command that printed it.
#[derive(Clone, Debug)]
pub struct Sampler {
    /// One sample every this many events. Held at one or more, so a rate of
    /// zero records everything rather than dividing by nothing.
    every: u32,
    /// Events still to go before the next sample.
    countdown: u32,
    /// The most samples the buffer will hold. A cap rather than a growing
    /// vector: a run at a low rate over a deep search would otherwise ask
    /// for gigabytes of fens.
    cap: usize,
    taken: Vec<Sample>,
    overflowed: u64,
}

impl Sampler {
    /// What a sampler holds when nothing says otherwise. Ten thousand fens
    /// is a megabyte or so, and a run wanting more of the tree than that
    /// should lower its rate rather than raise this.
    pub const DEFAULT_CAP: usize = 10_000;

    /// Records one node in every `every`.
    pub fn every(every: u32) -> Self {
        Self::with_cap(every, Self::DEFAULT_CAP)
    }

    /// The same, holding at most `cap` samples. One sampler is meant to be
    /// carried across a whole run of searches, so the cap it is built with
    /// bounds the run rather than any one search in it.
    pub fn with_cap(every: u32, cap: usize) -> Self {
        let every = every.max(1);
        Self {
            every,
            countdown: every,
            cap,
            taken: Vec::new(),
            overflowed: 0,
        }
    }

    /// Count one event, and on the nth of them ask for the sample.
    ///
    /// The sample arrives as a closure because building one prints a fen and
    /// evaluates a position, and neither is worth doing on the events that
    /// are not kept.
    pub fn event(&mut self, describe: impl FnOnce() -> Sample) {
        self.countdown -= 1;
        if self.countdown > 0 {
            return;
        }
        self.countdown = self.every;
        if self.taken.len() >= self.cap {
            self.overflowed += 1;
            return;
        }
        self.taken.push(describe());
    }

    /// How many samples are held.
    pub fn len(&self) -> usize {
        self.taken.len()
    }

    pub fn is_empty(&self) -> bool {
        self.taken.is_empty()
    }

    /// Everything collected, leaving the sampler empty and ready to record
    /// again. The overflow count goes with it: it describes the samples
    /// handed over and not the sampler.
    pub fn drain(&mut self) -> Sampled {
        Sampled {
            taken: std::mem::take(&mut self.taken),
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
    /// found, which is the direction that loses games. Positive is a
    /// shortcut that gave away less than it could have, since what it
    /// returns is a lower bound and a bound below the truth is sound.
    pub fn delta(&self) -> i32 {
        i32::from(self.reference) - i32::from(self.claimed)
    }
}

/// One kind's residuals, as the run reports them. The percentiles are the
/// product; the rows are the evidence for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    pub kind: Shortcut,
    pub count: usize,
    pub min: i32,
    pub median: i32,
    pub p90: i32,
    pub p99: i32,
    pub max: i32,
}

/// A whole run: what it was asked for, what it recorded, and a row a sample.
#[derive(Clone, Debug)]
pub struct Report {
    pub depth: u8,
    pub every: u32,
    pub config: SearchConfig,
    /// Positions of the suite the recording run searched.
    pub positions: usize,
    /// Samples the buffer had no room for.
    pub overflowed: u64,
    /// Samples the replay could not put a reference value on, because the
    /// position as a diagram has no move to make. Counted rather than
    /// scored, so the rows below are all comparisons and the count says how
    /// many were not.
    pub unplayable: usize,
    pub rows: Vec<Row>,
}

/// One sample every this many events, unless the command says otherwise.
/// Low enough that a run at the bench's depth finishes in minutes and high
/// enough that it describes the whole suite rather than its first position.
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
pub fn run(positions: &[Position], depth: u8, every: u32, config: SearchConfig) -> Report {
    let depth = depth.max(1);
    // the rate the sampler will really keep to, so that the header states
    // the run that happened rather than the words it was asked in
    let every = every.max(1);
    let sampled = record(positions, depth, every, config);
    let (rows, unplayable) = replay(&sampled.taken);
    Report {
        depth,
        every,
        config,
        positions: positions.len(),
        overflowed: sampled.overflowed,
        unplayable,
        rows,
    }
}

/// The recording phase on its own: the suite searched under the
/// configuration given, with the nodes its shortcuts answered sampled.
///
/// One sampler for the whole suite, carried from each position's engine to
/// the next, so that the countdown and the cap describe the run and not each
/// position of it. A sampler built afresh per position would restart the
/// countdown eighteen times: a position with fewer events than the rate
/// would contribute nothing at all, and the remainder of every other one
/// would be dropped, which weights the distribution towards the positions
/// with the most shortcuts rather than sampling the suite evenly. What this
/// takes is every nth event of the whole run, in the order the suite lists
/// its positions.
pub fn record(
    positions: &[Position],
    depth: u8,
    every: u32,
    config: SearchConfig,
) -> crate::residual::Sampled {
    let mut sampler = Sampler::with_cap(every, Sampler::DEFAULT_CAP);
    for position in positions {
        let board = Board::from_fen(&position.fen)
            .unwrap_or_else(|e| panic!("residual position {} does not parse: {}", position.id, e));
        let mut engine = AlphaBeta::with_config(board, bench::TABLE_BYTES, config);
        engine.sample_shortcuts(sampler);
        engine.iterative_deepening_search(SearchParameters::to_depth(depth), |_, _, _| {});
        sampler = engine
            .take_sampler()
            .expect("the sampler just handed to the engine comes back");
    }
    sampler.drain()
}

/// What the reference search says about each sampled position, and how many
/// of them it had nothing to say about.
///
/// One engine for the whole replay, with a table of its own that starts cold
/// and is not cleared between records. A probe compares the whole key, so an
/// entry left by a record about some other position is not read at all. An
/// entry about this one can be, though, and that is the cost of not clearing:
/// a record replayed at depth three after the same position was replayed at
/// depth five is answered from the deeper entry, so a reference value depends
/// in principle on what the replay happened to search before it. It was
/// measured not to bite at the sizes this is run at, where repeats of a
/// position at different depths are rare and the rows they produced were
/// unchanged either way. Accepted for a table that starts cold once a run:
/// clearing per record would pay a full table wipe for every sample to fix
/// something the measurement does not see.
///
/// The limitation, known and accepted: a fen carries the fifty move counter
/// and not the path. The replay cannot see a repetition that needs moves
/// made before the node, so what is measured is the reference's answer to
/// the position as a diagram. That is the simplification every epd suite
/// makes, and it is why a residual on a position deep in a shuffle is read
/// with more care than one from a middlegame.
pub fn replay(samples: &[Sample]) -> (Vec<Row>, usize) {
    let mut engine =
        AlphaBeta::with_config(Board::new(), REPLAY_TABLE_BYTES, SearchConfig::reference());
    let mut rows = Vec::with_capacity(samples.len());
    let mut unplayable = 0;
    for sample in samples {
        if engine.parse_fen(&sample.fen).is_err() {
            unplayable += 1;
            continue;
        }
        let outcome = engine
            .iterative_deepening_search(SearchParameters::to_depth(sample.depth), |_, _, _| {});
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
    /// One kind's residuals, or none when the run recorded none of that
    /// kind. A kind can go missing at a shallow depth, where the gates in
    /// front of the pass turn away every node the countdown would have
    /// picked.
    pub fn summary(&self, kind: Shortcut) -> Option<Summary> {
        let mut deltas: Vec<i32> = self
            .rows
            .iter()
            .filter(|row| row.kind == kind)
            .map(Row::delta)
            .collect();
        if deltas.is_empty() {
            return None;
        }
        deltas.sort_unstable();
        Some(Summary {
            kind,
            count: deltas.len(),
            min: deltas[0],
            median: percentile(&deltas, 50),
            p90: percentile(&deltas, 90),
            p99: percentile(&deltas, 99),
            max: deltas[deltas.len() - 1],
        })
    }
}

/// The report as the command prints it: a header naming what the run was
/// asked for and what it collected, a row a sample, and a summary a kind.
///
/// A row is whitespace separated with the fen last, so it parses left to
/// right and the field that can hold spaces holds the rest of the line.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "residuals depth {} every {} taint {} positions {} records {}",
            self.depth,
            self.every,
            self.config.taint_word(),
            self.positions,
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
                "{} {} {} {} {} {} {}",
                row.kind.word(),
                row.depth,
                row.eval_beta,
                row.claimed,
                row.reference,
                row.delta(),
                row.fen,
            )?;
        }
        writeln!(f)?;
        writeln!(f, "summary")?;
        for kind in Shortcut::KINDS {
            match self.summary(kind) {
                Some(s) => writeln!(
                    f,
                    "{} {} min {} median {} p90 {} p99 {} max {}",
                    kind.word(),
                    s.count,
                    s.min,
                    s.median,
                    s.p90,
                    s.p99,
                    s.max
                )?,
                // said rather than left out: a kind that recorded nothing is
                // a fact about the run
                None => writeln!(f, "{} 0", kind.word())?,
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
            eval_beta: 40,
        }
    }

    #[test]
    fn one_event_in_every_n_is_kept() {
        let mut sampler = Sampler::every(3);
        for event in 0..9 {
            sampler.event(|| sample(&event.to_string()));
        }
        let sampled = sampler.drain();
        let fens: Vec<&str> = sampled.taken.iter().map(|s| s.fen.as_str()).collect();
        assert_eq!(fens, vec!["2", "5", "8"]);
        assert_eq!(sampled.overflowed, 0);
    }

    #[test]
    fn a_rate_of_zero_keeps_every_event() {
        let mut sampler = Sampler::every(0);
        for event in 0..3 {
            sampler.event(|| sample(&event.to_string()));
        }
        assert_eq!(sampler.len(), 3);
    }

    #[test]
    fn a_full_buffer_stops_recording_and_counts_the_rest() {
        let mut sampler = Sampler::with_cap(1, 2);
        for event in 0..7 {
            sampler.event(|| sample(&event.to_string()));
        }
        let sampled = sampler.drain();
        assert_eq!(sampled.taken.len(), 2);
        assert_eq!(sampled.overflowed, 5);
    }

    #[test]
    fn draining_leaves_the_sampler_ready_to_record_again() {
        let mut sampler = Sampler::with_cap(1, 1);
        sampler.event(|| sample("first"));
        sampler.event(|| sample("dropped"));
        let first = sampler.drain();
        assert_eq!(first.taken.len(), 1);
        assert_eq!(first.overflowed, 1);
        assert!(sampler.is_empty());
        sampler.event(|| sample("second"));
        let second = sampler.drain();
        assert_eq!(second.taken[0].fen, "second");
        assert_eq!(second.overflowed, 0);
    }

    #[test]
    fn the_kinds_print_the_switches_that_turn_them_on() {
        assert_eq!(Shortcut::ReverseFutility.word(), "reverse_futility");
        assert_eq!(Shortcut::NullMove.word(), "null_move");
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
            eval_beta: 1,
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
            eval_beta: 0,
        };
        let (rows, unplayable) = replay(&[mated]);
        assert!(rows.is_empty());
        assert_eq!(unplayable, 1);
    }

    #[test]
    fn a_run_records_and_replays_the_suite() {
        let report = run(&suite(), 4, 25, SearchConfig::default());
        assert_eq!(report.positions, 2);
        assert_eq!(report.depth, 4);
        assert_eq!(report.every, 25);
        assert!(!report.rows.is_empty(), "nothing was recorded");
        for row in &report.rows {
            Board::from_fen(&row.fen).unwrap_or_else(|e| panic!("{}: {}", row.fen, e));
            assert!(Shortcut::KINDS.contains(&row.kind));
            assert!(row.depth >= 1 && row.depth <= 5, "{:?}", row);
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
        let sampled = run(&suite, 4, 1, SearchConfig::default());
        let watched = bench::run_suite(&suite, 4, bench::TABLE_BYTES, SearchConfig::default());
        assert!(!sampled.rows.is_empty());
        assert_eq!(plain.nodes(), watched.nodes());
    }

    /// The countdown runs over the suite and not over each position of it.
    /// Asked of the recording phase alone, which is where the property lives
    /// and which costs a search rather than a search and a replay.
    ///
    /// Both halves matter. The count says the remainder of each position is
    /// not thrown away, which is what a counter restarting per position does
    /// to it. The sequence says which events were taken, which is the
    /// property itself: every nth of the whole stream, in suite order.
    #[test]
    fn the_countdown_runs_over_the_whole_suite() {
        const EVERY: usize = 7;
        let suite = suite();
        let all = record(&suite, 4, 1, SearchConfig::default());
        assert_eq!(all.overflowed, 0, "the cap got in the way of the count");
        assert!(all.taken.len() > 3 * EVERY, "{} events", all.taken.len());
        let sampled = record(&suite, 4, EVERY as u32, SearchConfig::default());
        assert_eq!(sampled.overflowed, 0);
        assert_eq!(sampled.taken.len(), all.taken.len() / EVERY);
        let expected: Vec<&Sample> = all.taken.iter().skip(EVERY - 1).step_by(EVERY).collect();
        assert_eq!(sampled.taken.iter().collect::<Vec<&Sample>>(), expected);
    }

    /// The sampler holds a rate of zero at one, so the header says one:
    /// two runs that behaved identically print identical headers.
    #[test]
    fn a_rate_of_zero_is_reported_as_the_rate_that_ran() {
        let report = run(&suite(), 2, 0, SearchConfig::default());
        assert_eq!(report.every, 1);
        assert!(
            report.to_string().starts_with("residuals depth 2 every 1 "),
            "{}",
            report
        );
    }

    #[test]
    fn the_report_names_its_settings_and_ends_in_a_summary() {
        let report = run(&suite(), 3, 20, SearchConfig::default());
        let text = report.to_string();
        assert!(
            text.starts_with(&format!(
                "residuals depth 3 every 20 taint rule50 positions 2 records {}\n",
                report.rows.len()
            )),
            "{}",
            text
        );
        let lines: Vec<&str> = text.lines().collect();
        // a line a kind, whether or not the kind recorded anything
        let summary_at = lines.iter().position(|l| *l == "summary").expect("summary");
        assert_eq!(lines.len() - summary_at - 1, Shortcut::KINDS.len());
        for (kind, line) in Shortcut::KINDS.iter().zip(&lines[summary_at + 1..]) {
            assert!(line.starts_with(kind.word()), "{}", line);
        }
    }

    /// The row's fields, in the order the header of docs/DEVELOPMENT.md
    /// names them, with the fen last so a row parses left to right.
    #[test]
    fn a_row_reads_left_to_right_with_the_fen_last() {
        let report = Report {
            depth: 4,
            every: 10,
            config: SearchConfig::default(),
            positions: 1,
            overflowed: 0,
            unplayable: 0,
            rows: vec![Row {
                kind: Shortcut::NullMove,
                depth: 3,
                eval_beta: 140,
                claimed: 200,
                reference: 150,
                fen: "4k3/8/8/8/8/8/8/4K3 w - - 0 1".to_string(),
            }],
        };
        let text = report.to_string();
        let row = text.lines().nth(1).expect("a row");
        let mut words = row.splitn(7, ' ');
        assert_eq!(words.next(), Some("null_move"));
        assert_eq!(words.next(), Some("3"));
        assert_eq!(words.next(), Some("140"));
        assert_eq!(words.next(), Some("200"));
        assert_eq!(words.next(), Some("150"));
        assert_eq!(words.next(), Some("-50"));
        assert_eq!(words.next(), Some("4k3/8/8/8/8/8/8/4K3 w - - 0 1"));
        assert!(text.contains("null_move 1 min -50 median -50 p90 -50 p99 -50 max -50"));
        // a kind with nothing recorded says so rather than going missing
        assert!(text.contains("\nreverse_futility 0\n"));
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
            config: SearchConfig::default(),
            positions: 1,
            overflowed: 0,
            unplayable: 0,
            rows: Vec::new(),
        };
        let quiet = report.to_string();
        assert!(!quiet.contains("overflow"), "{}", quiet);
        assert!(!quiet.contains("unplayable"), "{}", quiet);
        report.overflowed = 12;
        report.unplayable = 3;
        let loud = report.to_string();
        assert!(loud.starts_with(
            "residuals depth 4 every 1 taint rule50 positions 1 records 0 overflow 12 unplayable 3\n"
        ), "{}", loud);
    }
}
