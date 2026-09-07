// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The reduction ledger: what each sampled reduced scout decided, and
//! whether a fail low it was trusted on threw a move away.
//!
//! The late move reduction trusts a scout. A quiet move searched late is
//! asked a zero width question a ply shallower, and a scout that fails low
//! answers for the move at the node's full depth: the move is never
//! searched at the depth the node has. The census records what the
//! ordering earned; this ledger records what trusting the scout decided,
//! one event per sampled reduced scout, taken in `windowed` where the
//! scout's answer comes back. The features on a row are the ones the
//! decision could have read cheaply, which is what a reduction policy
//! would be fit on.
//!
//! The label comes from a replay. A fail low is the trusted answer, so the
//! replay asks the counterfactual the trust skipped: the reference search,
//! on the position the move left, to the full depth the move was denied,
//! over the full window. The row is `harmful` when that answer, seen from
//! the node that reduced, stands above the alpha the scout was read
//! against: the full search would have raised alpha on a move the scout
//! wrote off. A fail high is not a decision the search trusted, so it
//! carries no label; its cost is the wasted scout, which the row prices in
//! nodes. The high rows stay in the stream at the same rate all the same:
//! they are the denominator a policy's propensities are read against.
//!
//! What the label alone cannot price is another reduction. A row says what
//! one ply decided and the reference says what the move was worth, and
//! between them sits every reduction the search did not take. So the
//! replay asks those too: the same zero width question, from the same
//! position, at each reduction the node's depth leaves room for, under the
//! configuration the engine plays with. That is the trials column, and it
//! turns a row from one observation into the whole action-to-outcome table
//! at the node. A reduction's harm is then read off the rows it writes off,
//! and the band a deeper reduction newly reaches is read off the rows it
//! writes off that the shallower one does not.
//!
//! The recorder hangs off an engine the way the census does, and an engine
//! without one searches exactly the tree it searched before there was a
//! ledger at all, which is what the pinned bench counts say.

use crate::bench::{self, Position};
use crate::board::Board;
use crate::census;
use crate::engine::{AlphaBeta, Engine, SearchConfig, SearchOutcome, SearchParameters};
use crate::misc::Score;
use crate::play::Play;
use crate::residual::{self, DEPTH_SPREAD, Sampler, Window};
use crate::value::Value;
use std::fmt;

/// What the ledger contributes to a sampling key: an arbitrary constant
/// under the census's rule. The five salts now in use differ within their
/// top three bits, so at any rate coarser than one in eight a node kept
/// here is not one the census or the shortcut kinds keep.
const SALT: u64 = 0x6d84_3b2f_51c9_07ea;

/// The key a scout's answer is sampled by: the position the scout judged,
/// the node's depth, and nothing about the run, exactly as the census
/// builds one, so two runs of the same search record the same scouts.
pub fn sample_key(position_key: u64, depth: u8) -> u64 {
    position_key ^ SALT ^ u64::from(depth).wrapping_mul(DEPTH_SPREAD)
}

/// About one record in every this many events, unless the command says
/// otherwise. The scouts run sparser than the census's events, since only
/// a late quiet move at depth offers one, and every fail low kept is a
/// reference search in the replay; this rate keeps a run at the bench's
/// depth to minutes, and a run that wants a stratum whole lowers it.
pub const DEFAULT_EVERY: u32 = 1_000;

/// Under this many replayed rows a cell prints its counts and no rate: a
/// percentage over a handful of rows reads as a finding and is noise.
const THIN: usize = 30;

/// How many reductions the replay tries on a row, counted from none at
/// all. Five, so a row at the bench's deepest reducing node still has a
/// trial past the reduction its policy would choose; a node has room for
/// the reduction `r` only where its depth is at least `r + 2`, the floor
/// the live reduction keeps, so the shallow rows fill fewer of them.
pub const TRIALS: usize = 5;

/// What the zero width scout answered, against the alpha it was asked
/// about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scout {
    /// At or under alpha: the answer the reduction trusts, and the one the
    /// replay checks.
    Low,
    /// Above alpha: the move earned the full depth and was re-searched, so
    /// there is no decision left to check, only the scout's cost.
    High,
}

impl Scout {
    /// The word a row prints.
    pub fn word(self) -> &'static str {
        match self {
            Scout::Low => "low",
            Scout::High => "high",
        }
    }

    /// How a scout's answer reads against the alpha it was asked about.
    /// At or under alpha is the fail low, as `windowed` reads it.
    pub fn of(score: Score, alpha: Score) -> Self {
        if score <= alpha {
            Scout::Low
        } else {
            Scout::High
        }
    }
}

/// The move loop's half of an event: what the node knew about the reduced
/// move at the moment it decided to scout it. Staged on the engine while
/// the ledger is armed, and finished by `windowed` when the scout answers.
#[derive(Clone, Copy, Debug)]
pub struct Staged {
    /// The move, kept so the recorder can step it back for the node's own
    /// evaluation and replay it.
    pub play: Play,
    /// The move's place among the searched moves, the census's count: the
    /// table's move, when it was searched, is 0.
    pub index: usize,
    /// The moves the node generated, as the census records it.
    pub generated: usize,
    /// The history table's score for the move at the decision. Every
    /// reduced move is quiet, so there is no class to price it by instead.
    pub history: u32,
    /// The largest history score among the node's generated quiets, the
    /// denominator `history` is read against.
    pub history_max: u32,
    /// Whether the move stood in one of the node's killer slots.
    pub killer: bool,
    /// What the node's table probe had given it, the census's three-state.
    pub tt: census::Table,
}

/// One reduced scout answering.
///
/// Everything is owned, as a census event is: an event outlives the search
/// that took it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// The position the reduced move left, as the board prints one, last
    /// on the row. The side to move in it is the side the move was played
    /// against, so a score searched from it is negated before it is read
    /// beside `alpha`.
    pub fen: String,
    /// The depth of the node that reduced, the check extension included.
    /// The move was denied a search at `depth - 1`, which is the depth the
    /// replay searches the fen to.
    pub depth: u8,
    /// The window the node stood in at the scout, read from its bounds the
    /// way the census reads them.
    pub window: Window,
    /// The move's place among the searched moves. Never under the late
    /// move threshold, which is what made the move late.
    pub index: usize,
    /// The moves searched with this one among them: `index + 1` by
    /// construction, recorded beside it all the same, so a row is read
    /// without re-deriving it.
    pub searched: usize,
    /// The moves the node generated.
    pub generated: usize,
    /// The history table's score for the reduced move at the decision.
    pub history: u32,
    /// The largest history score among the node's generated quiets.
    pub history_max: u32,
    /// Whether the reduced move stood in one of the node's killer slots.
    pub killer: bool,
    /// What the node's table probe had given it.
    pub tt: census::Table,
    /// The node's static evaluation less its beta. The eval is the
    /// reducing node's own, taken at record time for kept events alone by
    /// stepping the move back and replaying it; computing it for every
    /// scout to fill a column is what the census's precedent refuses.
    pub eval_beta: i32,
    /// The node's alpha less the same evaluation: how far the eval stood
    /// from the bound the scout was actually asked about.
    pub alpha_gap: i32,
    /// The alpha the scout was read against, which is the bar the replay's
    /// answer is held to.
    pub alpha: Score,
    /// What the scout answered.
    pub scout: Scout,
    /// The nodes the scout spent.
    pub cost: u64,
}

impl Event {
    /// The depth the reduced move was denied: what the replay searches the
    /// fen to. Held to one for rows built by hand; a recorded row's depth
    /// is never under the reduction's own floor.
    pub fn replay_depth(&self) -> u8 {
        self.depth.saturating_sub(1).max(1)
    }

    /// The depth a trial at this reduction searches the fen to, or none
    /// where the node has no room for it. The room is the live
    /// reduction's own floor, a full width ply under the scout, so a node
    /// of depth `d` offers the reductions up to `d - 2`. A reduction of
    /// none is the move searched whole and is offered wherever the row is.
    pub fn trial_depth(&self, reduction: usize) -> Option<u8> {
        let reduction = u8::try_from(reduction).ok()?;
        if self.depth < reduction + 2 {
            return None;
        }
        Some(self.depth - 1 - reduction)
    }
}

/// An event with the replay's answers beside it: what the move was really
/// worth, and how the same scout would have answered at each reduction the
/// node had room for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub event: Event,
    /// The reference's answer to the fen at the full depth, from the side
    /// to move in the fen, so it is negated before it is read beside the
    /// event's alpha. None where the replay was not run.
    pub reference: Option<Score>,
    /// What a scout at each reduction answered, the index the reduction in
    /// plies. None where the node's depth had no room for it, and none
    /// throughout where the replay was not run.
    pub trials: [Option<Scout>; TRIALS],
}

impl Row {
    /// Whether the move the scout was asked about really beats the alpha
    /// it was read against: the reference's answer, seen from the node
    /// that reduced, stands above that alpha, strictly. An answer equal to
    /// alpha raises nothing. This is the oracle every trial is read
    /// against, and it does not depend on which reduction is being
    /// priced. None where nothing was replayed.
    pub fn raises_alpha(&self) -> Option<bool> {
        self.reference
            .map(|reference| -i32::from(reference) > i32::from(self.event.alpha))
    }

    /// Whether trusting the scout the search ran threw a move away: it
    /// failed low and the move raises alpha. A fail high was never
    /// trusted, so it carries no label however the replay answered.
    pub fn harmful(&self) -> Option<bool> {
        if self.event.scout == Scout::High {
            return None;
        }
        self.raises_alpha()
    }

    /// Whether a reduction of this many plies would have thrown the move
    /// away: its trial failed low and the move raises alpha. None where
    /// the node had no room for the reduction or nothing was replayed.
    pub fn harmful_at(&self, reduction: usize) -> Option<bool> {
        let trial = (*self.trials.get(reduction)?)?;
        Some(trial == Scout::Low && self.raises_alpha()?)
    }

    /// The word a row prints for the label, or a `-` on a row the search
    /// trusted nothing on.
    pub fn label_word(&self) -> &'static str {
        match self.harmful() {
            Some(true) => "harmful",
            Some(false) => "harmless",
            None => "-",
        }
    }

    /// The trials as a row prints them: one word a reduction, comma
    /// separated so the column holds no space, and a `-` for a reduction
    /// the node had no room for.
    pub fn trials_word(&self) -> String {
        self.trials
            .iter()
            .map(|trial| trial.map_or("-", Scout::word))
            .collect::<Vec<&str>>()
            .join(",")
    }
}

/// A whole run: what it was asked for and what it recorded, a row an
/// event.
#[derive(Clone, Debug)]
pub struct Report {
    pub depth: u8,
    pub every: u32,
    /// The most events the run would keep. Stated in the header only when
    /// it is not the default, the way the census header states its own.
    pub cap: usize,
    /// The file the positions were read from, or none when they are the
    /// bench's own. Stated in the header for the cap's reason, and on the
    /// residuals report's: a policy fitted on the bench's positions and
    /// read back on the same positions has checked nothing.
    pub suite: Option<String>,
    /// Positions of the suite the recording run searched.
    pub positions: usize,
    /// Every scout offered, kept or not: the denominator the rows are read
    /// against.
    pub events: u64,
    /// Events the buffer had no room for.
    pub overflowed: u64,
    /// Events the replay could not put an answer on, because the fen did
    /// not parse. Counted rather than labelled, so every label below has a
    /// search behind it.
    pub unplayable: usize,
    pub rows: Vec<Row>,
}

/// Record, then replay, the residuals discipline: a reference search run
/// inside the measured one would write into the table the measured search
/// is reading, so the replay waits for the suite and owns an engine and a
/// table of its own.
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
) -> Report {
    let depth = depth.max(1);
    // the rate the sampler will really keep to, so the header states the
    // run that happened
    let every = every.max(1);
    let mut sampler = Sampler::with_cap(every, cap);
    for position in positions {
        let board = Board::from_fen(&position.fen)
            .unwrap_or_else(|e| panic!("ledger position {} does not parse: {}", position.id, e));
        let mut engine = AlphaBeta::with_config(board, bench::TABLE_BYTES, SearchConfig::default());
        engine.sample_reductions(sampler);
        engine.iterative_deepening_search(SearchParameters::to_depth(depth), |_, _, _, _| {});
        sampler = engine
            .take_reductions()
            .expect("the sampler just handed to the engine comes back");
    }
    let sampled = sampler.drain();
    let (rows, unplayable) = replay(&sampled.taken);
    Report {
        depth,
        every,
        cap,
        suite: suite.map(str::to_string),
        positions: positions.len(),
        events: sampled.events,
        overflowed: sampled.overflowed,
        unplayable,
        rows,
    }
}

/// The counterfactual on every row: what the full search would have said
/// about the move, and how a scout at each reduction the node had room for
/// would have answered.
///
/// Two engines, because the two questions are asked of two searches. The
/// oracle is the residuals replay's exactly: the reference, with a table of
/// its own cleared before every sample and no clock, for the reasons
/// `residual::replay` gives. The trials are the default, since what they
/// price is the scout the engine itself would run, and they put its own
/// zero width question at the alpha the row carries.
///
/// Every row is replayed now, the fail highs among them. A fail high is
/// still no decision the search trusted, so it takes no label; what it is
/// replayed for is the trials, where a reduction deeper than the one the
/// search took writes off moves the search did not. Those rows are the band
/// a deeper reduction newly reaches, and without the oracle on them the
/// band cannot be priced at all.
///
/// The fen carries the fifty move counter and not the path, with everything
/// the residuals section says that costs. The trials carry one thing more:
/// they run on a cold table from a bare position, where the scout they
/// stand for ran inside a search with its killers, its history and its
/// table warm. The `trials` column at the reduction the search really took
/// is what says how much that costs, since the `scout` column beside it is
/// the same question answered in the tree.
pub fn replay(events: &[Event]) -> (Vec<Row>, usize) {
    let mut oracle = AlphaBeta::with_config(
        Board::new(),
        residual::REPLAY_TABLE_BYTES,
        SearchConfig::reference(),
    );
    let mut trialist = AlphaBeta::with_config(
        Board::new(),
        residual::REPLAY_TABLE_BYTES,
        SearchConfig::default(),
    );
    let mut rows = Vec::with_capacity(events.len());
    let mut unplayable = 0;
    for event in events {
        if oracle.parse_fen(&event.fen).is_err() {
            unplayable += 1;
            continue;
        }
        // cold for every sample, so no sample's answer is another's
        oracle.clear_transpositions();
        let outcome = oracle.iterative_deepening_search(
            SearchParameters::to_depth(event.replay_depth()),
            |_, _, _, _| {},
        );
        let reference = match outcome {
            SearchOutcome::Complete(result) => result.score,
            // no move to make: the rules fix what the position is worth,
            // as the residuals replay scores the same case
            SearchOutcome::GameOver => {
                if oracle.board.in_check() && !oracle.board.has_legal_move() {
                    Value::mated(0).score
                } else {
                    0
                }
            }
            SearchOutcome::Aborted(_) => {
                unreachable!("a replay is searched to a depth of at least one")
            }
        };
        let mut trials = [None; TRIALS];
        for (reduction, trial) in trials.iter_mut().enumerate() {
            let Some(depth) = event.trial_depth(reduction) else {
                continue;
            };
            trialist
                .parse_fen(&event.fen)
                .expect("the oracle read this fen already");
            trialist.clear_transpositions();
            *trial = Some(Scout::of(
                trialist.scout_again(event.alpha, depth),
                event.alpha,
            ));
        }
        rows.push(Row {
            event: event.clone(),
            reference: Some(reference),
            trials,
        });
    }
    (rows, unplayable)
}

/// One cell of the summary's split: the harmful rows over the replayed
/// rows that fell in it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Band {
    pub harmful: usize,
    pub replayed: usize,
}

/// One reduction priced over the rows of one depth: what it would write
/// off, what that would cost, and the band it reaches that a reduction a
/// ply shallower does not.
///
/// The band is the whole of why the trials are run. A reduction's own
/// harmful rate is pooled over every row it writes off, and the great
/// majority of those are rows any reduction writes off; the rate that
/// prices a step from one reduction to the next is the rate in the rows
/// the step newly writes off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Trial {
    /// The reduction in plies, none at all being the move searched whole.
    pub reduction: usize,
    /// Rows whose depth left room for this reduction: the denominator.
    pub offered: usize,
    /// Of those, the ones whose trial failed low, which are the moves this
    /// reduction writes off.
    pub low: usize,
    /// Of those, the ones the reference would have raised alpha on.
    pub harmful: usize,
    /// Rows this reduction writes off that a ply less does not, among the
    /// rows offered both.
    pub band: usize,
    /// The harmful rows in that band.
    pub band_harmful: usize,
}

/// The scouts of one depth, counted the way the summary line prints them.
/// By depth and not pooled, for the census's reason: the depths are
/// reached in wildly different numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    pub depth: u8,
    /// Every row at this depth: one scout each.
    pub scouts: usize,
    /// The fail lows among them, which is the share the reduction trusts.
    pub low: usize,
    /// Fail lows with a reference answer, the denominator of every rate
    /// below.
    pub replayed: usize,
    pub harmful: usize,
    /// The replayed rows split by the move's index: 4 to 7, 8 to 15, and
    /// 16 and past.
    pub index_bands: [Band; 3],
    /// The replayed rows split by the history fraction, `history` over
    /// `history_max`: exactly zero, under a tenth, under half, and half
    /// and up.
    pub history_bands: [Band; 4],
    /// Each reduction the replay tried, priced over every row of this
    /// depth rather than over the fail lows alone: a reduction the search
    /// did not take is asked of the whole population.
    pub trials: [Trial; TRIALS],
}

/// Which index band a row falls in.
fn index_band(index: usize) -> usize {
    match index {
        0..=7 => 0,
        8..=15 => 1,
        _ => 2,
    }
}

/// Which history fraction band a row falls in. A move with no history is
/// its own band whatever the denominator; past that the fraction is read
/// in integers, so no rounding sits under a boundary.
fn history_band(history: u32, history_max: u32) -> usize {
    if history == 0 {
        return 0;
    }
    let history = u64::from(history);
    let max = u64::from(history_max);
    if history * 10 < max {
        1
    } else if history * 2 < max {
        2
    } else {
        3
    }
}

impl Report {
    /// One depth's summary, or none when the run kept no row of it.
    pub fn summary(&self, depth: u8) -> Option<Summary> {
        let rows: Vec<&Row> = self
            .rows
            .iter()
            .filter(|row| row.event.depth == depth)
            .collect();
        if rows.is_empty() {
            return None;
        }
        let mut counted = Summary {
            depth,
            scouts: rows.len(),
            low: 0,
            replayed: 0,
            harmful: 0,
            index_bands: [Band::default(); 3],
            history_bands: [Band::default(); 4],
            trials: [Trial::default(); TRIALS],
        };
        for (reduction, trial) in counted.trials.iter_mut().enumerate() {
            trial.reduction = reduction;
        }
        for row in &rows {
            for (reduction, trial) in counted.trials.iter_mut().enumerate() {
                let Some(harmful) = row.harmful_at(reduction) else {
                    continue;
                };
                trial.offered += 1;
                if row.trials[reduction] != Some(Scout::Low) {
                    continue;
                }
                trial.low += 1;
                trial.harmful += usize::from(harmful);
                // the band is what this reduction writes off and a ply
                // less does not, so a row the shallower reduction was
                // never offered stands in neither
                if reduction > 0 && row.trials[reduction - 1] == Some(Scout::High) {
                    trial.band += 1;
                    trial.band_harmful += usize::from(harmful);
                }
            }
        }
        for row in rows {
            if row.event.scout == Scout::Low {
                counted.low += 1;
            }
            let Some(harmful) = row.harmful() else {
                continue;
            };
            counted.replayed += 1;
            counted.harmful += usize::from(harmful);
            let by_index = &mut counted.index_bands[index_band(row.event.index)];
            by_index.replayed += 1;
            by_index.harmful += usize::from(harmful);
            let by_history =
                &mut counted.history_bands[history_band(row.event.history, row.event.history_max)];
            by_history.replayed += 1;
            by_history.harmful += usize::from(harmful);
        }
        Some(counted)
    }

    /// Every summary the run has, shallowest depth first.
    pub fn summaries(&self) -> Vec<Summary> {
        let mut depths: Vec<u8> = self.rows.iter().map(|row| row.event.depth).collect();
        depths.sort_unstable();
        depths.dedup();
        depths
            .into_iter()
            .filter_map(|depth| self.summary(depth))
            .collect()
    }
}

/// A share as the summary prints one, or a `-` when nothing stands under
/// it: a figure with no denominator is not a zero.
fn share(part: usize, of: usize) -> String {
    if of == 0 {
        "-".to_string()
    } else {
        format!("{:.2}%", 100.0 * part as f64 / of as f64)
    }
}

/// A cell's harmful rate, or its bare counts when the cell is thin: a rate
/// over a handful of rows is not a rate.
fn cell(band: Band) -> String {
    if band.replayed >= THIN {
        share(band.harmful, band.replayed)
    } else {
        format!("{}/{}", band.harmful, band.replayed)
    }
}

/// A rate over a denominator, held to the same rule as a cell: past thirty
/// rows a percentage, under it a `-`, since a percentage over a handful of
/// rows reads as a finding and is noise.
fn rate(part: usize, of: usize) -> String {
    if of >= THIN {
        share(part, of)
    } else {
        "-".to_string()
    }
}

/// The report as the command prints it: a header naming what the run was
/// asked for and what it collected, a row an event, and a summary line a
/// depth.
///
/// A row is `depth window index searched generated history history_max
/// killer tt eval_beta alpha_gap alpha scout cost reference label trials
/// fen`, whitespace separated with the fen last, so it parses left to right
/// and the field that can hold spaces holds the rest of the line. The label
/// a fail high has no value for prints `-` rather than moving the columns,
/// and the trials column holds one word a reduction, comma separated, with
/// a `-` where the node had no room for one.
///
/// The header states the events beside the records, always, for the
/// census header's reason: a distribution says nothing until the reader
/// knows how many chances there were to be in it.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "reductions depth {} every {}", self.depth, self.every)?;
        if self.cap != residual::DEFAULT_CAP {
            write!(f, " cap {}", self.cap)?;
        }
        // the bench's own suite is the default and reads as absent, the
        // way the cap does, and the residuals header names one for the
        // same reason
        if let Some(suite) = &self.suite {
            write!(f, " epd {suite}")?;
        }
        write!(
            f,
            " positions {} events {} records {}",
            self.positions,
            self.events,
            self.rows.len(),
        )?;
        if self.overflowed > 0 {
            write!(f, " overflow {}", self.overflowed)?;
        }
        if self.unplayable > 0 {
            write!(f, " unplayable {}", self.unplayable)?;
        }
        writeln!(f)?;
        for row in &self.rows {
            let e = &row.event;
            writeln!(
                f,
                "{} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
                e.depth,
                e.window.word(),
                e.index,
                e.searched,
                e.generated,
                e.history,
                e.history_max,
                if e.killer { "killer" } else { "plain" },
                e.tt.word(),
                e.eval_beta,
                e.alpha_gap,
                e.alpha,
                e.scout.word(),
                e.cost,
                match row.reference {
                    Some(reference) => reference.to_string(),
                    None => "-".to_string(),
                },
                row.label_word(),
                row.trials_word(),
                e.fen,
            )?;
        }
        writeln!(f)?;
        writeln!(f, "summary")?;
        let summaries = self.summaries();
        // said rather than left out: a run that kept nothing is a fact
        if summaries.is_empty() {
            writeln!(f, "records 0")?;
        }
        for s in summaries {
            write!(
                f,
                "depth {} scouts {} low {} share {} replayed {} harmful {} rate {}",
                s.depth,
                s.scouts,
                s.low,
                share(s.low, s.scouts),
                s.replayed,
                s.harmful,
                if s.replayed >= THIN {
                    share(s.harmful, s.replayed)
                } else {
                    "-".to_string()
                },
            )?;
            for (label, band) in ["index4-7", "index8-15", "index16+"]
                .iter()
                .zip(s.index_bands)
            {
                write!(f, " {} {}", label, cell(band))?;
            }
            for (label, band) in ["hist0", "hist<0.1", "hist<0.5", "hist0.5+"]
                .iter()
                .zip(s.history_bands)
            {
                write!(f, " {} {}", label, cell(band))?;
            }
            writeln!(f)?;
        }
        writeln!(f)?;
        writeln!(f, "trials")?;
        for s in self.summaries() {
            for trial in s.trials {
                // a reduction no row of this depth had room for is left
                // out rather than printed as a row of zeroes
                if trial.offered == 0 {
                    continue;
                }
                writeln!(
                    f,
                    "depth {} r {} offered {} low {} share {} harmful {} rate {} \
                     band {} bandharmful {} bandrate {}",
                    s.depth,
                    trial.reduction,
                    trial.offered,
                    trial.low,
                    share(trial.low, trial.offered),
                    trial.harmful,
                    rate(trial.harmful, trial.low),
                    trial.band,
                    trial.band_harmful,
                    rate(trial.band_harmful, trial.band),
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::residual::DEFAULT_CAP;
    use crate::residual::fixtures::{recording_leaves_the_search_where_it_was, suite};

    /// An event made up, for the tests that drive the replay and the
    /// printer on rows the test chose.
    fn made_up(fen: &str, depth: u8, alpha: Score, scout: Scout) -> Event {
        Event {
            fen: fen.to_string(),
            depth,
            window: Window::Zero,
            index: 5,
            searched: 6,
            generated: 30,
            history: 4,
            history_max: 40,
            killer: false,
            tt: census::Table::Miss,
            eval_beta: -60,
            alpha_gap: 55,
            alpha,
            scout,
            cost: 12,
        }
    }

    /// A row with the reference given and no trial on it: the printer and
    /// the summary tests choose their labels, and the trials are driven by
    /// the tests that run the replay.
    fn row_of(event: Event, reference: Option<Score>) -> Row {
        Row {
            event,
            reference,
            trials: [None; TRIALS],
        }
    }

    fn report_of(rows: Vec<Row>) -> Report {
        Report {
            depth: 5,
            every: 10,
            cap: DEFAULT_CAP,
            suite: None,
            positions: 1,
            events: 300,
            overflowed: 0,
            unplayable: 0,
            rows,
        }
    }

    /// The key decides on the node alone and the ledger draws apart from
    /// the census and the shortcut kinds: the same node keys differently
    /// under every one of them.
    #[test]
    fn a_key_is_the_node_and_nothing_about_the_run() {
        let position = 0x0123_4567_89ab_cdef;
        let key = sample_key(position, 4);
        assert_eq!(key, sample_key(position, 4));
        assert_ne!(key, sample_key(position, 5));
        assert_ne!(key, sample_key(position ^ 1, 4));
        assert_ne!(key, census::sample_key(position, 4));
        for kind in crate::residual::Shortcut::KINDS {
            assert_ne!(key, crate::residual::sample_key(position, kind, 4));
        }
    }

    /// The sign every label rests on. The fen on a row is the position
    /// the reduced move left, its side to move the side the move was
    /// played against, so the replay's answer is negated before it meets
    /// alpha. A side left facing a bare queen is lost, the answer negated
    /// is well above any alpha, and the scout that wrote the move off
    /// threw a winning move away: harmful. The two fens differ only in
    /// which side holds the queen.
    #[test]
    fn a_fail_low_the_full_search_would_raise_alpha_on_is_harmful() {
        let events = vec![made_up("7k/8/8/8/8/8/8/1Q5K b - - 0 1", 3, 0, Scout::Low)];
        let (rows, unplayable) = replay(&events);
        assert_eq!(unplayable, 0);
        assert_eq!(rows.len(), 1);
        let reference = rows[0].reference.expect("a fail low is replayed");
        assert!(reference < 0, "the side to move is lost: {}", reference);
        assert_eq!(rows[0].harmful(), Some(true));
        assert_eq!(rows[0].label_word(), "harmful");
    }

    /// The same ask where the side to move holds the queen: the answer
    /// negated is far under alpha, the full search agrees with the scout,
    /// and the row is harmless.
    #[test]
    fn a_fail_low_the_full_search_agrees_with_is_harmless() {
        let events = vec![made_up("7k/8/8/8/8/8/1q6/7K b - - 0 1", 3, 0, Scout::Low)];
        let (rows, _) = replay(&events);
        let reference = rows[0].reference.expect("a fail low is replayed");
        assert!(reference > 0, "the side to move is winning: {}", reference);
        assert_eq!(rows[0].harmful(), Some(false));
        assert_eq!(rows[0].label_word(), "harmless");
    }

    /// An answer exactly at alpha raises nothing, so the boundary is
    /// harmless: the label is strict, as the crossing is in residuals.
    #[test]
    fn an_answer_landing_on_alpha_is_harmless() {
        let row = row_of(
            made_up("7k/8/8/8/8/8/8/Q6K b - - 0 1", 3, 25, Scout::Low),
            Some(-25),
        );
        assert_eq!(row.harmful(), Some(false));
        let raised = row_of(
            made_up("7k/8/8/8/8/8/8/Q6K b - - 0 1", 3, 24, Scout::Low),
            Some(-25),
        );
        assert_eq!(raised.harmful(), Some(true));
    }

    /// A fail high is replayed for its trials and takes no label. The
    /// search never trusted it, so there is no decision of its own to
    /// check; what the row is replayed for is the reductions deeper than
    /// the one that answered, which do write the move off.
    #[test]
    fn a_fail_high_is_replayed_and_takes_no_label() {
        let events = vec![made_up("7k/8/8/8/8/8/8/1Q5K b - - 0 1", 4, 0, Scout::High)];
        let (rows, unplayable) = replay(&events);
        assert_eq!(unplayable, 0);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].reference.is_some(), "{:?}", rows[0]);
        assert_eq!(rows[0].raises_alpha(), Some(true));
        assert_eq!(rows[0].harmful(), None);
        assert_eq!(rows[0].label_word(), "-");
        // a depth of four leaves room for a reduction of none, one and
        // two, and none for the two past them
        assert!(rows[0].trials[0].is_some(), "{:?}", rows[0]);
        assert!(rows[0].trials[2].is_some(), "{:?}", rows[0]);
        assert_eq!(rows[0].trials[3], None);
    }

    /// A row whose fen does not parse is counted rather than labelled, the
    /// residuals rule, and a fail high is no exception now that one is
    /// replayed.
    #[test]
    fn an_unreadable_row_is_counted_not_labelled() {
        for scout in [Scout::Low, Scout::High] {
            let events = vec![made_up("not a position", 3, 0, scout)];
            let (rows, unplayable) = replay(&events);
            assert!(rows.is_empty());
            assert_eq!(unplayable, 1);
        }
    }

    /// A position with no move to make is scored by rule, as the residuals
    /// replay scores the same case: the side to move is stalemated, the
    /// answer is a draw, and a draw at or under alpha is harmless.
    #[test]
    fn a_replayed_stalemate_is_scored_by_rule() {
        let events = vec![made_up("7k/5Q2/6K1/8/8/8/8/8 b - - 0 1", 3, 10, Scout::Low)];
        let (rows, unplayable) = replay(&events);
        assert_eq!(unplayable, 0);
        assert_eq!(rows[0].reference, Some(0));
        assert_eq!(rows[0].harmful(), Some(false));
    }

    /// The row's fields in the order the printer's comment names them,
    /// with the fen last, and a fail high printing `-` for the label
    /// rather than moving the columns.
    #[test]
    fn a_row_reads_left_to_right_with_the_fen_last() {
        let low = Row {
            trials: [
                Some(Scout::High),
                Some(Scout::Low),
                Some(Scout::Low),
                Some(Scout::Low),
                None,
            ],
            event: Event {
                fen: "4k3/8/8/8/8/8/8/4K3 b - - 0 1".to_string(),
                depth: 6,
                window: Window::Zero,
                index: 7,
                searched: 8,
                generated: 31,
                history: 16,
                history_max: 25,
                killer: true,
                tt: census::Table::Move,
                eval_beta: -37,
                alpha_gap: 12,
                alpha: 21,
                scout: Scout::Low,
                cost: 214,
            },
            reference: Some(-40),
        };
        let high = Row {
            trials: [Some(Scout::High), Some(Scout::High), None, None, None],
            event: Event {
                fen: "4k3/8/8/8/8/8/8/4K3 w - - 0 1".to_string(),
                depth: 4,
                window: Window::Open,
                index: 4,
                searched: 5,
                generated: 20,
                history: 0,
                history_max: 9,
                killer: false,
                tt: census::Table::Miss,
                eval_beta: 5,
                alpha_gap: -3,
                alpha: -2,
                scout: Scout::High,
                cost: 9,
            },
            reference: Some(3),
        };
        let report = report_of(vec![low, high]);
        let text = report.to_string();
        let row = text.lines().nth(1).expect("a replayed row");
        let words: Vec<&str> = row.splitn(18, ' ').collect();
        assert_eq!(
            words,
            vec![
                "6",
                "zw",
                "7",
                "8",
                "31",
                "16",
                "25",
                "killer",
                "move",
                "-37",
                "12",
                "21",
                "low",
                "214",
                "-40",
                "harmful",
                "high,low,low,low,-",
                "4k3/8/8/8/8/8/8/4K3 b - - 0 1",
            ]
        );
        let row = text.lines().nth(2).expect("a fail high row");
        let words: Vec<&str> = row.splitn(18, ' ').collect();
        assert_eq!(
            words,
            vec![
                "4",
                "open",
                "4",
                "5",
                "20",
                "0",
                "9",
                "plain",
                "miss",
                "5",
                "-3",
                "-2",
                "high",
                "9",
                "3",
                "-",
                "high,high,-,-,-",
                "4k3/8/8/8/8/8/8/4K3 w - - 0 1",
            ]
        );
        assert!(
            text.starts_with("reductions depth 5 every 10 positions 1 events 300 records 2\n"),
            "{}",
            text
        );
    }

    /// A replayed row for the summary tests, landed in the band the test
    /// names by its index and its history.
    fn replayed(depth: u8, index: usize, history: u32, harmful: bool) -> Row {
        let mut event = made_up("4k3/8/8/8/8/8/8/4K3 b - - 0 1", depth, 0, Scout::Low);
        event.index = index;
        event.searched = index + 1;
        event.history = history;
        event.history_max = 40;
        // alpha is 0, so a positive answer negated stays under it and a
        // negative one crosses it
        row_of(event, Some(if harmful { -50 } else { 50 }))
    }

    /// The summary's counts, pinned against rows made up to land one in
    /// each band: the low share, the replayed count, the harmful rate,
    /// and the two splits.
    #[test]
    fn the_summary_counts_the_scouts_and_where_the_harm_fell() {
        let mut high = made_up("4k3/8/8/8/8/8/8/4K3 b - - 0 1", 5, 0, Scout::High);
        high.index = 20;
        let report = report_of(vec![
            replayed(5, 4, 0, true),
            replayed(5, 9, 3, false),
            replayed(5, 17, 8, false),
            replayed(5, 5, 30, true),
            row_of(high, None),
        ]);
        let summary = report.summary(5).expect("five rows at depth five");
        assert_eq!(summary.scouts, 5);
        assert_eq!(summary.low, 4);
        assert_eq!(summary.replayed, 4);
        assert_eq!(summary.harmful, 2);
        // indices 4 and 5 in the first band, 9 in the second, 17 in the
        // third; the unreplayed 20 in none
        assert_eq!(
            summary.index_bands,
            [
                Band {
                    harmful: 2,
                    replayed: 2
                },
                Band {
                    harmful: 0,
                    replayed: 1
                },
                Band {
                    harmful: 0,
                    replayed: 1
                },
            ]
        );
        // history 0 in the zero band, 3 of 40 under a tenth, 8 of 40
        // under half, 30 of 40 half and up
        assert_eq!(
            summary.history_bands,
            [
                Band {
                    harmful: 1,
                    replayed: 1
                },
                Band {
                    harmful: 0,
                    replayed: 1
                },
                Band {
                    harmful: 0,
                    replayed: 1
                },
                Band {
                    harmful: 1,
                    replayed: 1
                },
            ]
        );
        // every cell is thin, so the line prints counts and no rate
        assert!(
            report.to_string().contains(
                "depth 5 scouts 5 low 4 share 80.00% replayed 4 harmful 2 rate - \
                 index4-7 2/2 index8-15 0/1 index16+ 0/1 \
                 hist0 1/1 hist<0.1 0/1 hist<0.5 0/1 hist0.5+ 1/1"
            ),
            "{}",
            report
        );
    }

    /// Past thirty replayed rows a cell earns its rate, and the thin cells
    /// beside it keep their counts.
    #[test]
    fn a_cell_prints_a_rate_only_past_thirty_rows() {
        let mut rows = Vec::new();
        for i in 0..40 {
            rows.push(replayed(4, 4, 0, i < 10));
        }
        rows.push(replayed(4, 9, 0, true));
        let report = report_of(rows);
        let text = report.to_string();
        assert!(
            text.contains(
                "depth 4 scouts 41 low 41 share 100.00% replayed 41 harmful 11 rate 26.83% \
                 index4-7 25.00% index8-15 1/1 index16+ 0/0"
            ),
            "{}",
            text
        );
    }

    /// The history fraction's boundaries, read in integers: a tenth and a
    /// half land in the bands their names say.
    #[test]
    fn the_history_bands_split_where_their_names_say() {
        assert_eq!(history_band(0, 40), 0);
        assert_eq!(history_band(3, 40), 1);
        assert_eq!(history_band(4, 40), 2);
        assert_eq!(history_band(19, 40), 2);
        assert_eq!(history_band(20, 40), 3);
        assert_eq!(history_band(40, 40), 3);
        // no history at all is the zero band, with nothing divided
        assert_eq!(history_band(0, 0), 0);
    }

    /// A line a depth, and the depth with no rows is not invented.
    #[test]
    fn each_depth_is_summarised_on_its_own() {
        let report = report_of(vec![replayed(3, 4, 0, true), replayed(5, 4, 0, false)]);
        let depths: Vec<u8> = report.summaries().iter().map(|s| s.depth).collect();
        assert_eq!(depths, vec![3, 5]);
        assert!(report.summary(4).is_none());
    }

    /// A row with the trials given, for the trial summary's tests: the
    /// oracle says whether the move raises alpha and each trial says
    /// whether that reduction would have written it off.
    fn tried(depth: u8, raises: bool, trials: [Option<Scout>; TRIALS]) -> Row {
        Row {
            event: made_up("4k3/8/8/8/8/8/8/4K3 b - - 0 1", depth, 0, Scout::Low),
            reference: Some(if raises { -50 } else { 50 }),
            trials,
        }
    }

    /// A reduction is priced over every row its depth offered it, and the
    /// band beside it over the rows it writes off that a ply less does
    /// not. The pooled rate and the band rate are different numbers, and
    /// the band is the one a step from one reduction to the next is read
    /// by.
    #[test]
    fn a_trial_prices_a_reduction_and_the_band_it_reaches() {
        let low = Some(Scout::Low);
        let high = Some(Scout::High);
        let report = report_of(vec![
            // written off at both, and harmless: the bulk of the
            // population, and what a pooled rate is mostly made of
            tried(4, false, [low, low, low, None, None]),
            tried(4, false, [low, low, low, None, None]),
            // held at one ply and written off at two, on a move the full
            // search would have raised alpha on: the band, and the harm
            // in it
            tried(4, true, [high, high, low, None, None]),
            // held at every reduction the depth allows
            tried(4, false, [high, high, high, None, None]),
            // a shallower node, which offers no reduction of two at all
            tried(3, true, [low, low, None, None, None]),
        ]);
        let summary = report.summary(4).expect("four rows at depth four");
        assert_eq!(
            summary.trials[1],
            Trial {
                reduction: 1,
                offered: 4,
                low: 2,
                harmful: 0,
                band: 0,
                band_harmful: 0,
            }
        );
        assert_eq!(
            summary.trials[2],
            Trial {
                reduction: 2,
                offered: 4,
                low: 3,
                harmful: 1,
                band: 1,
                band_harmful: 1,
            }
        );
        // the depth three rows offer the first two reductions and no more
        let shallow = report.summary(3).expect("a row at depth three");
        assert_eq!(shallow.trials[1].offered, 1);
        assert_eq!(shallow.trials[2].offered, 0);

        let text = report.to_string();
        assert!(
            text.contains(
                "depth 4 r 2 offered 4 low 3 share 75.00% harmful 1 rate - \
                 band 1 bandharmful 1 bandrate -"
            ),
            "{}",
            text
        );
        // a reduction no row of the depth had room for is left out rather
        // than printed as a row of zeroes
        assert!(!text.contains("depth 3 r 2 "), "{}", text);
    }

    /// The header states a suite of its own, the way the residuals header
    /// does, so a distribution measured over other positions is never read
    /// as the bench's.
    #[test]
    fn the_header_names_a_suite_that_is_not_the_benchs() {
        let mut report = report_of(Vec::new());
        report.suite = Some("held_out.epd".to_string());
        assert!(
            report
                .to_string()
                .starts_with("reductions depth 5 every 10 epd held_out.epd positions 1"),
            "{}",
            report
        );
    }

    /// The header states a cap off the default, an overflow and an
    /// unplayable count, the way the residuals header does, and none of
    /// them on an ordinary run.
    #[test]
    fn the_header_says_when_the_run_was_capped_or_dropped_something() {
        let mut report = report_of(Vec::new());
        let quiet_run = report.to_string();
        assert!(
            quiet_run.starts_with("reductions depth 5 every 10 positions 1 events 300 records 0\n"),
            "{}",
            quiet_run
        );
        assert!(!quiet_run.contains("overflow"), "{}", quiet_run);
        assert!(quiet_run.contains("\nrecords 0\n"), "{}", quiet_run);
        report.cap = 25;
        report.overflowed = 12;
        report.unplayable = 3;
        assert!(
            report.to_string().starts_with(
                "reductions depth 5 every 10 cap 25 positions 1 events 300 records 0 \
                 overflow 12 unplayable 3\n"
            ),
            "{}",
            report
        );
    }

    /// A run over the suite: every row holds together. The fen parses,
    /// the index is past the late move threshold and inside the searched
    /// and generated counts, the history fits its denominator, every row
    /// carries the replay's answer, and the trials fill exactly the
    /// reductions the node's depth left room for.
    #[test]
    fn a_run_records_rows_that_hold_together() {
        let report = run(&suite(), None, 5, 1, DEFAULT_CAP);
        assert_eq!(report.positions, 2);
        assert!(!report.rows.is_empty(), "nothing was recorded");
        assert!(report.events >= report.rows.len() as u64);
        assert_eq!(report.unplayable, 0);
        for row in &report.rows {
            let e = &row.event;
            assert!(Board::from_fen(&e.fen).is_ok(), "{} does not parse", e.fen);
            assert!(e.index >= 4, "{:?}", row);
            assert_eq!(e.searched, e.index + 1, "{:?}", row);
            assert!(e.searched <= e.generated + 1, "{:?}", row);
            assert!(e.history <= e.history_max, "{:?}", row);
            assert!(e.depth >= 3, "{:?}", row);
            assert!(e.cost >= 1, "{:?}", row);
            assert!(row.reference.is_some(), "{:?}", row);
            for (reduction, trial) in row.trials.iter().enumerate() {
                assert_eq!(
                    trial.is_some(),
                    e.trial_depth(reduction).is_some(),
                    "reduction {} of {:?}",
                    reduction,
                    row
                );
            }
            // the label is the search's own decision, so a fail high
            // keeps none of it however the replay answered
            assert_eq!(row.harmful().is_some(), e.scout == Scout::Low, "{:?}", row);
        }
        // both answers are in the stream: the lows are what the replay
        // labels and the highs are the propensity denominator
        assert!(report.rows.iter().any(|row| row.event.scout == Scout::Low));
        assert!(report.rows.iter().any(|row| row.event.scout == Scout::High));
    }

    /// The ledger's contract, asked the way `fixtures` asks all three.
    #[test]
    fn recording_leaves_the_measured_search_where_it_was() {
        recording_leaves_the_search_where_it_was(
            |engine| engine.sample_reductions(Sampler::with_cap(1, DEFAULT_CAP)),
            |engine| {
                engine
                    .take_reductions()
                    .expect("the sampler comes back")
                    .drain()
                    .taken
                    .len()
            },
        );
    }

    /// The rate of zero and the depth of zero are held to one, so the
    /// header states the run that happened.
    #[test]
    fn a_rate_of_zero_is_reported_as_the_rate_that_ran() {
        let report = run(&suite(), None, 0, 0, 50);
        assert_eq!(report.depth, 1);
        assert_eq!(report.every, 1);
        assert!(
            report
                .to_string()
                .starts_with("reductions depth 1 every 1 cap 50 "),
            "{}",
            report
        );
    }
}
