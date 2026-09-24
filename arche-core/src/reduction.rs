// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The reduction ledger: what each sampled reduced scout decided, and
//! whether a fail low it was trusted on threw a move away.
//!
//! The late move reduction trusts a scout: a quiet move searched late is
//! asked a zero width question a ply shallower, and a scout that fails
//! low answers for the move at the node's full depth. This ledger records
//! what trusting the scout decided, one event per sampled reduced scout,
//! taken in `windowed` where the scout's answer comes back. The features
//! on a row are the ones the decision could have read cheaply, which is
//! what a reduction policy would be fit on.
//!
//! The label comes from a replay of the counterfactual the trust skipped:
//! the reference search on the position the move left, to the full depth
//! the move was denied, over the full window. The row is `harmful` when
//! that answer, seen from the node that reduced, stands above the alpha
//! the scout was read against. A fail high needs no replay, since the
//! mechanism already re-searched the move; its cost is the wasted scout,
//! priced in nodes. The high rows stay in the stream at the same rate as
//! the denominator a policy's propensities are read against.
//!
//! Late move pruning adds a third outcome. A sampled skip is recorded
//! where the loop passes the move over: the same features, no cost, and
//! the outcome word `skipped`. A skipped move that turns out illegal when
//! the recorder makes it is not recorded, since the skip denied it
//! nothing. The replay treats a skipped row as a fail low.
//!
//! An engine without a ledger searches exactly the tree it searched before
//! there was one, which is what the pinned bench counts say.

use crate::bench::Position;
use crate::census;
use crate::engine::SearchConfig;
use crate::late_move;
use crate::misc::Score;
use crate::play::Play;
use crate::recorder::{self, Window, share};
use crate::residual;
use std::fmt;

/// The key a scout's answer is sampled by: the position the scout judged,
/// the node's depth, and nothing about the run, as the census builds one.
pub fn sample_key(position_key: u64, depth: u8) -> u64 {
    recorder::sample_key(position_key, recorder::LEDGER_LANE, depth)
}

/// About one record in every this many events, unless the command says
/// otherwise. Every fail low kept is a reference search in the replay;
/// this rate keeps a run at the bench's depth to minutes, and a run that
/// wants a stratum whole lowers it.
pub const DEFAULT_EVERY: u32 = 1_000;

/// Under this many replayed rows a cell prints its counts and no rate: a
/// percentage over a handful of rows reads as a finding and is noise.
const THIN: usize = 30;

/// What the zero width scout answered against the alpha it was asked
/// about, or that no scout was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scout {
    /// At or under alpha: the answer the reduction trusts, and the one the
    /// replay checks.
    Low,
    /// Above alpha: the move was re-searched, so there is no decision left
    /// to check, only the scout's cost.
    High,
    /// Never asked: late move pruning dropped the move. Replayed exactly as
    /// a fail low, since what was denied is the same full depth search.
    Skipped,
}

impl Scout {
    /// The word a row prints.
    pub fn word(self) -> &'static str {
        match self {
            Scout::Low => "low",
            Scout::High => "high",
            Scout::Skipped => "skipped",
        }
    }
}

/// The move loop's half of an event: the reduced move and what the node
/// knew about it when it decided to scout it. Built while the ledger is
/// armed and handed to `windowed`, which finishes the event when the scout
/// answers. The features are `late_move::features`'s, the derivation the
/// gate scored, so a row cannot say something the score did not.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Staged {
    /// The move, kept so the recorder can step it back for the node's own
    /// evaluation and replay it.
    pub(crate) play: Play,
    /// What the node knew about the move, in the units the columns below
    /// are printed in.
    pub(crate) features: late_move::Features,
}

/// One reduced scout answering. Everything is owned: an event outlives the
/// search that took it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// The position the reduced move left, as the board prints one. Its
    /// side to move is the side the move was played against, so a score
    /// searched from it is negated before it is read beside `alpha`.
    pub fen: String,
    /// The depth of the node that reduced, the check extension included.
    /// The move was denied a search at `depth - 1`, which the replay
    /// searches the fen to.
    pub depth: u8,
    /// The window the node stood in at the scout, read from its bounds.
    pub window: Window,
    /// The move's place among the searched moves. Never under the late
    /// move threshold on a scouted row; a shallow skip records its own,
    /// which is never under one.
    pub index: usize,
    /// The moves searched: `index + 1` on a scouted row, whose move is
    /// among them, and `index` on a skipped row, whose move never was.
    /// Recorded rather than re-derived, so a row says which it is.
    pub searched: usize,
    /// The moves the node generated.
    pub generated: usize,
    /// The history table's score for the reduced move at the decision,
    /// signed.
    pub history: i32,
    /// The largest history score among the node's generated quiets,
    /// clamped at zero.
    pub history_max: i32,
    /// Whether the reduced move stood in one of the node's killer slots.
    pub killer: bool,
    /// What the node's table probe had given it.
    pub tt: census::Table,
    /// The reducing node's static evaluation less its beta, taken at
    /// record time for kept events alone by stepping the move back;
    /// computing it for every scout to fill a column is what the census's
    /// precedent refuses.
    pub eval_beta: i32,
    /// The node's alpha less the same evaluation: how far the eval stood
    /// from the bound the scout was asked about.
    pub alpha_gap: i32,
    /// The alpha the scout was read against, which the replay's answer is
    /// held to.
    pub alpha: Score,
    pub scout: Scout,
    /// The nodes the scout spent: zero on a skipped row.
    pub cost: u64,
    /// How many plies shallower the scout ran: what the reduction table,
    /// the model gate and the clamp under them settled on, and zero on a
    /// skipped row. The label does not read it; the counterfactual is the
    /// full depth answer whichever was trusted.
    pub reduction: u8,
}

impl Event {
    /// The depth the reduced move was denied: what the replay searches the
    /// fen to. Floored at one, which is what a depth one skip asks for: the
    /// counterfactual is then a search of the same depth rather than
    /// quiescence, so the label over-states what the skip denied by a ply
    /// there rather than reading nothing at all.
    pub fn replay_depth(&self) -> u8 {
        self.depth.saturating_sub(1).max(1)
    }
}

/// An event with the replay's answer beside it, or without one on a fail
/// high, which is never replayed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub event: Event,
    /// The reference's answer to the fen at the full depth, from the side
    /// to move in the fen, so it is negated before it is read beside the
    /// event's alpha. None on a fail high.
    pub reference: Option<Score>,
}

impl Row {
    /// Whether trusting the scout threw a move away: the reference's
    /// answer, seen from the node that reduced, stands strictly above the
    /// alpha the scout was read against. An answer equal to alpha is a
    /// fail low the scout was right about. None where nothing was
    /// replayed.
    pub fn harmful(&self) -> Option<bool> {
        self.reference
            .map(|reference| -i32::from(reference) > i32::from(self.event.alpha))
    }

    /// The word a row prints for the label, or a `-` on a row with no
    /// replay behind it.
    pub fn label_word(&self) -> &'static str {
        match self.harmful() {
            Some(true) => "harmful",
            Some(false) => "harmless",
            None => "-",
        }
    }
}

/// A whole run: what it was asked for and what it recorded, a row an
/// event.
#[derive(Clone, Debug)]
pub struct Report {
    pub depth: u8,
    pub every: u32,
    /// The most events the run would keep. Stated in the header only when
    /// it is not the default.
    pub cap: usize,
    /// The file the positions came from, or none for the bench's own.
    pub suite: Option<String>,
    /// Positions of the suite the recording run searched.
    pub positions: usize,
    /// Every scout offered, kept or not: the denominator.
    pub events: u64,
    /// Events the buffer had no room for.
    pub overflowed: u64,
    /// Replayable rows whose fen did not parse, counted rather than
    /// labelled, so every label has a search behind it.
    pub unplayable: usize,
    pub rows: Vec<Row>,
}

/// Record, then replay, for `residual::run`'s reason. `suite` names the
/// file the positions came from, for the header alone; None is the
/// bench's own suite.
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
    let sampled = recorder::record(positions, depth, every, cap, SearchConfig::default());
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

/// The counterfactual on every fail low and every skip: what the full
/// search would have said about the move the search wrote off. The engine
/// is the residuals replay's, cleared before every sample, and the fen
/// carries the counter and not the path, for the reasons and with the
/// costs `residual::replay` gives. A fail high is passed through
/// unreplayed.
pub fn replay(events: &[Event]) -> (Vec<Row>, usize) {
    let mut engine = residual::replay_engine();
    let mut rows = Vec::with_capacity(events.len());
    let mut unplayable = 0;
    for event in events {
        if event.scout == Scout::High {
            rows.push(Row {
                event: event.clone(),
                reference: None,
            });
            continue;
        }
        let Some(reference) =
            residual::reference_answer(&mut engine, &event.fen, event.replay_depth())
        else {
            unplayable += 1;
            continue;
        };
        rows.push(Row {
            event: event.clone(),
            reference: Some(reference),
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

/// The scouts of one depth, as the summary line prints them. By depth and
/// not pooled, since the depths are reached in wildly different numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    pub depth: u8,
    /// The scouted rows at this depth: a fail low or a fail high each.
    pub scouts: usize,
    /// The fail lows among them: the share the reduction trusts.
    pub low: usize,
    /// The skipped rows, beside the scouted ones; every one is replayed.
    pub skipped: usize,
    /// Rows with a reference answer (the fail lows and the skips): the
    /// denominator of every rate below.
    pub replayed: usize,
    pub harmful: usize,
    /// The replayed rows split by the move's index: 4 to 7, 8 to 15, and
    /// 16 and past.
    pub index_bands: [Band; 3],
    /// The replayed rows split by `history` over `history_max`: exactly
    /// zero, under a tenth, under half, half and up, and below zero. The
    /// last is appended rather than put at the foot, so a summary line
    /// printed before the history went signed reads the same in its first
    /// four cells.
    pub history_bands: [Band; 5],
}

fn index_band(index: usize) -> usize {
    match index {
        0..=7 => 0,
        8..=15 => 1,
        _ => 2,
    }
}

/// A marked down move and a move with no history are each their own band
/// whatever the denominator; past that the fraction is read in integers,
/// so no rounding sits under a boundary.
fn history_band(history: i32, history_max: i32) -> usize {
    if history < 0 {
        return 4;
    }
    if history == 0 {
        return 0;
    }
    let history = i64::from(history);
    let max = i64::from(history_max);
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
            scouts: 0,
            low: 0,
            skipped: 0,
            replayed: 0,
            harmful: 0,
            index_bands: [Band::default(); 3],
            history_bands: [Band::default(); 5],
        };
        for row in rows {
            match row.event.scout {
                Scout::Low => {
                    counted.scouts += 1;
                    counted.low += 1;
                }
                Scout::High => counted.scouts += 1,
                Scout::Skipped => counted.skipped += 1,
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

/// A cell's harmful rate, or its bare counts when the cell is thin.
fn cell(band: Band) -> String {
    if band.replayed >= THIN {
        share(band.harmful, band.replayed)
    } else {
        format!("{}/{}", band.harmful, band.replayed)
    }
}

/// The report as the command prints it: a header naming what the run was
/// asked for and what it collected, a row an event, and a summary line a
/// depth.
///
/// A row is `depth window index searched generated history history_max
/// killer tt eval_beta alpha_gap alpha scout cost reference label
/// reduction fen`, whitespace separated with the fen last, so it parses
/// left to right and the field that can hold spaces holds the rest of the
/// line. The two fields a fail high has no value for print `-` rather
/// than moving the columns. A skipped row prints `skipped` in the scout
/// column, zero for its cost and its reduction, and carries a reference
/// and a label the way a fail low does.
///
/// The header always states the events beside the records: a distribution
/// says nothing until the reader knows how many chances there were to be
/// in it.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "reductions depth {} every {}", self.depth, self.every)?;
        if self.cap != recorder::DEFAULT_CAP {
            write!(f, " cap {}", self.cap)?;
        }
        // the bench's own suite reads as absent, the way the cap does. A
        // run over another suite says so, since a threshold chosen on one
        // set of positions and read back on the same set has checked
        // nothing
        if let Some(suite) = &self.suite {
            write!(f, " epd {}", suite)?;
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
                e.reduction,
                e.fen,
            )?;
        }
        writeln!(f)?;
        writeln!(f, "summary")?;
        let summaries = self.summaries();
        // a run that kept nothing is a fact about the run
        if summaries.is_empty() {
            writeln!(f, "records 0")?;
        }
        for s in summaries {
            write!(
                f,
                "depth {} scouts {} low {} share {} skipped {} replayed {} harmful {} rate {}",
                s.depth,
                s.scouts,
                s.low,
                share(s.low, s.scouts),
                s.skipped,
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
            for (label, band) in ["hist0", "hist<0.1", "hist<0.5", "hist0.5+", "hist<0"]
                .iter()
                .zip(s.history_bands)
            {
                write!(f, " {} {}", label, cell(band))?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;
    use crate::late_move::{
        DEEP_REDUCTION_MIN_DEPTH, LATE_MOVE_MIN_DEPTH, LATE_MOVE_THRESHOLD, SHALLOW_MAX_DEPTH,
    };
    use crate::recorder::fixtures::{recording_leaves_the_search_where_it_was, suite};
    use crate::recorder::{DEFAULT_CAP, Sampler};

    /// An event made up.
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
            reduction: 1,
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

    /// The same node keys differently under the ledger, the census and
    /// every shortcut kind.
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

    /// The sign every label rests on: the replay's answer is from the side
    /// the move was played against, so it is negated before it meets
    /// alpha. A side left facing a bare queen is lost, so the scout that
    /// wrote the move off threw a winning move away. This fen and the
    /// next differ only in which side holds the queen.
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

    /// The same ask where the side to move holds the queen: the full search
    /// agrees with the scout.
    #[test]
    fn a_fail_low_the_full_search_agrees_with_is_harmless() {
        let events = vec![made_up("7k/8/8/8/8/8/1q6/7K b - - 0 1", 3, 0, Scout::Low)];
        let (rows, _) = replay(&events);
        let reference = rows[0].reference.expect("a fail low is replayed");
        assert!(reference > 0, "the side to move is winning: {}", reference);
        assert_eq!(rows[0].harmful(), Some(false));
        assert_eq!(rows[0].label_word(), "harmless");
    }

    /// An answer exactly at alpha raises nothing: the label is strict.
    #[test]
    fn an_answer_landing_on_alpha_is_harmless() {
        let row = Row {
            event: made_up("7k/8/8/8/8/8/8/Q6K b - - 0 1", 3, 25, Scout::Low),
            reference: Some(-25),
        };
        assert_eq!(row.harmful(), Some(false));
        let raised = Row {
            event: made_up("7k/8/8/8/8/8/8/Q6K b - - 0 1", 3, 24, Scout::Low),
            reference: Some(-25),
        };
        assert_eq!(raised.harmful(), Some(true));
    }

    /// A skipped move is replayed on the fail low's terms. The bare queen
    /// fens are the two fail low tests', and the labels land the same way.
    #[test]
    fn a_skipped_move_the_full_search_would_raise_alpha_on_is_harmful() {
        let mut event = made_up("7k/8/8/8/8/8/8/1Q5K b - - 0 1", 4, 0, Scout::Skipped);
        event.cost = 0;
        event.reduction = 0;
        event.searched = event.index;
        let (rows, unplayable) = replay(&[event]);
        assert_eq!(unplayable, 0);
        assert_eq!(rows.len(), 1);
        let reference = rows[0].reference.expect("a skip is replayed");
        assert!(reference < 0, "the side to move is lost: {}", reference);
        assert_eq!(rows[0].harmful(), Some(true));
        assert_eq!(rows[0].label_word(), "harmful");
    }

    /// The same ask where the skip was right.
    #[test]
    fn a_skipped_move_the_full_search_agrees_with_is_harmless() {
        let events = vec![made_up(
            "7k/8/8/8/8/8/1q6/7K b - - 0 1",
            4,
            0,
            Scout::Skipped,
        )];
        let (rows, _) = replay(&events);
        let reference = rows[0].reference.expect("a skip is replayed");
        assert!(reference > 0, "the side to move is winning: {}", reference);
        assert_eq!(rows[0].harmful(), Some(false));
        assert_eq!(rows[0].label_word(), "harmless");
    }

    /// A fail high is kept and never replayed: its fen is unreadable, so a
    /// replay that touched it would show in the unplayable count.
    #[test]
    fn a_fail_high_is_recorded_and_not_replayed() {
        let events = vec![made_up("not a position", 3, 0, Scout::High)];
        let (rows, unplayable) = replay(&events);
        assert_eq!(unplayable, 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reference, None);
        assert_eq!(rows[0].harmful(), None);
        assert_eq!(rows[0].label_word(), "-");
    }

    /// A fail low whose fen does not parse is counted rather than
    /// labelled.
    #[test]
    fn an_unreadable_fail_low_is_counted_not_labelled() {
        let events = vec![made_up("not a position", 3, 0, Scout::Low)];
        let (rows, unplayable) = replay(&events);
        assert!(rows.is_empty());
        assert_eq!(unplayable, 1);
    }

    /// A stalemated side to move is a draw by rule, and a draw at or under
    /// alpha is harmless.
    #[test]
    fn a_replayed_stalemate_is_scored_by_rule() {
        let events = vec![made_up("7k/5Q2/6K1/8/8/8/8/8 b - - 0 1", 3, 10, Scout::Low)];
        let (rows, unplayable) = replay(&events);
        assert_eq!(unplayable, 0);
        assert_eq!(rows[0].reference, Some(0));
        assert_eq!(rows[0].harmful(), Some(false));
    }

    /// The row's fields in the order the printer's comment names them, and
    /// a fail high printing `-` rather than moving the columns.
    #[test]
    fn a_row_reads_left_to_right_with_the_fen_last() {
        let low = Row {
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
                reduction: 2,
            },
            reference: Some(-40),
        };
        let high = Row {
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
                reduction: 1,
            },
            reference: None,
        };
        let skipped = Row {
            event: Event {
                fen: "4k3/8/8/8/8/8/8/4K3 b - - 0 1".to_string(),
                depth: 5,
                window: Window::Zero,
                index: 9,
                searched: 9,
                generated: 28,
                history: 0,
                history_max: 12,
                killer: false,
                tt: census::Table::Miss,
                eval_beta: -210,
                alpha_gap: 185,
                alpha: 30,
                scout: Scout::Skipped,
                cost: 0,
                reduction: 0,
            },
            reference: Some(-31),
        };
        let report = report_of(vec![low, high, skipped]);
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
                "2",
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
                "-",
                "-",
                "1",
                "4k3/8/8/8/8/8/8/4K3 w - - 0 1",
            ]
        );
        let row = text.lines().nth(3).expect("a skipped row");
        let words: Vec<&str> = row.splitn(18, ' ').collect();
        assert_eq!(
            words,
            vec![
                "5",
                "zw",
                "9",
                "9",
                "28",
                "0",
                "12",
                "plain",
                "miss",
                "-210",
                "185",
                "30",
                "skipped",
                "0",
                "-31",
                "harmful",
                "0",
                "4k3/8/8/8/8/8/8/4K3 b - - 0 1",
            ]
        );
        assert!(
            text.starts_with("reductions depth 5 every 10 positions 1 events 300 records 3\n"),
            "{}",
            text
        );
    }

    /// A replayed row for the summary tests, with alpha 0.
    fn replayed(depth: u8, index: usize, history: i32, harmful: bool) -> Row {
        let mut event = made_up("4k3/8/8/8/8/8/8/4K3 b - - 0 1", depth, 0, Scout::Low);
        event.index = index;
        event.searched = index + 1;
        event.history = history;
        event.history_max = 40;
        Row {
            event,
            // a positive answer negated stays under alpha and a negative
            // one crosses it
            reference: Some(if harmful { -50 } else { 50 }),
        }
    }

    /// The summary's counts, pinned against rows made up to land one in
    /// each band.
    #[test]
    fn the_summary_counts_the_scouts_and_where_the_harm_fell() {
        let mut high = made_up("4k3/8/8/8/8/8/8/4K3 b - - 0 1", 5, 0, Scout::High);
        high.index = 20;
        let report = report_of(vec![
            replayed(5, 4, 0, true),
            replayed(5, 9, 3, false),
            replayed(5, 17, 8, false),
            replayed(5, 5, 30, true),
            replayed(5, 6, -9, true),
            Row {
                event: high,
                reference: None,
            },
        ]);
        let summary = report.summary(5).expect("six rows at depth five");
        assert_eq!(summary.scouts, 6);
        assert_eq!(summary.low, 5);
        assert_eq!(summary.replayed, 5);
        assert_eq!(summary.harmful, 3);
        // indices 4, 5 and 6 in the first band, 9 in the second, 17 in the
        // third; the unreplayed 20 in none
        assert_eq!(
            summary.index_bands,
            [
                Band {
                    harmful: 3,
                    replayed: 3
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
        // under half, 30 of 40 half and up, and the marked down -9 in the
        // band appended after them
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
                Band {
                    harmful: 1,
                    replayed: 1
                },
            ]
        );
        // every cell is thin, so the line prints counts and no rate
        assert!(
            report.to_string().contains(
                "depth 5 scouts 6 low 5 share 83.33% skipped 0 replayed 5 harmful 3 rate - \
                 index4-7 3/3 index8-15 0/1 index16+ 0/1 \
                 hist0 1/1 hist<0.1 0/1 hist<0.5 0/1 hist0.5+ 1/1 hist<0 1/1"
            ),
            "{}",
            report
        );
    }

    /// Past `THIN` replayed rows a cell earns its rate, and the thin cells
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
                "depth 4 scouts 41 low 41 share 100.00% skipped 0 replayed 41 harmful 11 rate 26.83% \
                 index4-7 25.00% index8-15 1/1 index16+ 0/0"
            ),
            "{}",
            text
        );
    }

    /// A skipped row is counted beside the scouts, out of the low share's
    /// denominator, and its replay lands in the same bands a fail low's
    /// does.
    #[test]
    fn a_skipped_row_is_counted_beside_the_scouts() {
        let mut skip = made_up("4k3/8/8/8/8/8/8/4K3 b - - 0 1", 5, 0, Scout::Skipped);
        skip.cost = 0;
        skip.reduction = 0;
        skip.searched = skip.index;
        let report = report_of(vec![
            replayed(5, 4, 0, false),
            Row {
                event: skip,
                reference: Some(-50),
            },
        ]);
        let summary = report.summary(5).expect("two rows at depth five");
        assert_eq!(summary.scouts, 1);
        assert_eq!(summary.low, 1);
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.replayed, 2);
        assert_eq!(summary.harmful, 1);
        assert!(
            report
                .to_string()
                .contains("depth 5 scouts 1 low 1 share 100.00% skipped 1 replayed 2 harmful 1"),
            "{}",
            report
        );
    }

    /// A tenth and a half land in the bands their names say.
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
        // a marked down move is the appended band whatever the denominator
        assert_eq!(history_band(-1, 40), 4);
        assert_eq!(history_band(-8192, 0), 4);
    }

    /// A line a depth, and the depth with no rows is not invented.
    #[test]
    fn each_depth_is_summarised_on_its_own() {
        let report = report_of(vec![replayed(3, 4, 0, true), replayed(5, 4, 0, false)]);
        let depths: Vec<u8> = report.summaries().iter().map(|s| s.depth).collect();
        assert_eq!(depths, vec![3, 5]);
        assert!(report.summary(4).is_none());
    }

    /// The header states a cap off the default, an overflow and an
    /// unplayable count, and none of them on an ordinary run.
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

    /// A run over the suite: every row holds together, and the two scout
    /// outcomes a run of this size holds appear.
    #[test]
    fn a_run_records_rows_that_hold_together() {
        // depth six rather than five, because a fail high is the rare
        // outcome and depth five leaves none at any rate. Since the
        // aspiration window it is rarer than this run can hold. Measured
        // with the window on: 0 of the 2,049 rows one event in five gives
        // at depth six, and 0 to 3 of the 10,000 a reservoir takes when
        // every event is offered, over depths six to ten. That is about
        // one gate event in ten thousand, and a sample large enough to
        // hold one costs more than this test is worth. So the outcome is
        // not asserted here. What pins it is
        // `a_scout_that_fails_high_is_recorded_as_high` in `engine.rs`,
        // which builds the scout by hand, and the shape assertion in the
        // loop below, which holds wherever one does turn up
        let report = run(&suite(), None, 6, 5, DEFAULT_CAP);
        assert_eq!(report.positions, 2);
        assert!(!report.rows.is_empty(), "nothing was recorded");
        assert!(report.events >= report.rows.len() as u64);
        assert_eq!(report.unplayable, 0);
        for row in &report.rows {
            let e = &row.event;
            assert!(Board::from_fen(&e.fen).is_ok(), "{} does not parse", e.fen);
            assert!(e.searched <= e.generated + 1, "{:?}", row);
            assert!(e.history <= e.history_max, "{:?}", row);
            assert!(e.depth >= 1, "{:?}", row);
            if e.scout == Scout::Skipped {
                // no scout ran and the move is not among the searched.
                // Three rules skip, and the model's never meets the other
                // two at a depth: it stands on the reduction's floors,
                // while quiet futility and the late move count take a move
                // after the node's first at a depth under the model's. The
                // row does not say which of those two took it
                assert_eq!(e.searched, e.index, "{:?}", row);
                assert_eq!(e.cost, 0, "{:?}", row);
                assert_eq!(e.reduction, 0, "{:?}", row);
                if e.depth >= DEEP_REDUCTION_MIN_DEPTH {
                    assert!(e.index >= LATE_MOVE_THRESHOLD, "{:?}", row);
                } else {
                    assert!(e.depth <= SHALLOW_MAX_DEPTH, "{:?}", row);
                    assert!(e.index >= 1, "{:?}", row);
                }
            } else {
                assert!(e.index >= LATE_MOVE_THRESHOLD, "{:?}", row);
                assert!(e.depth >= LATE_MOVE_MIN_DEPTH, "{:?}", row);
                assert_eq!(e.searched, e.index + 1, "{:?}", row);
                assert!(e.cost >= 1, "{:?}", row);
                // never nothing, and never so much that the scout gives up
                // its full width ply: the clamp the amount is read under
                assert!(e.reduction >= 1, "{:?}", row);
                assert!(e.reduction <= e.depth - 2, "{:?}", row);
            }
            assert_eq!(row.reference.is_some(), e.scout != Scout::High, "{:?}", row);
        }
        assert!(report.rows.iter().any(|row| row.event.scout == Scout::Low));
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.event.scout == Scout::Skipped)
        );
    }

    /// The ledger's contract, at depth five so the armed runs reach skip
    /// events: the recorder makes and unmakes a skipped move on the live
    /// board, and this says the search did not notice.
    #[test]
    fn recording_leaves_the_measured_search_where_it_was() {
        let skipped = std::cell::Cell::new(0usize);
        recording_leaves_the_search_where_it_was(
            5,
            SearchConfig::default(),
            |engine| engine.arm(Sampler::<Event>::with_cap(1, DEFAULT_CAP)),
            |engine| {
                let taken = engine
                    .disarm::<Event>()
                    .expect("the sampler comes back")
                    .drain()
                    .taken;
                skipped.set(
                    skipped.get() + taken.iter().filter(|e| e.scout == Scout::Skipped).count(),
                );
                taken.len()
            },
        );
        assert!(skipped.get() > 0, "the armed runs never reached a skip");
    }

    /// A rate of zero and a depth of zero run as one, and the header says
    /// so.
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

    /// A run over a suite of its own says so in the header.
    #[test]
    fn the_header_names_a_suite_that_is_not_the_benchs() {
        let named = run(&suite(), Some("held_out.epd"), 2, 0, DEFAULT_CAP);
        assert!(
            named
                .to_string()
                .starts_with("reductions depth 2 every 1 epd held_out.epd positions"),
            "{}",
            named
        );
        let bench = run(&suite(), None, 2, 0, DEFAULT_CAP);
        assert!(
            bench
                .to_string()
                .starts_with("reductions depth 2 every 1 positions"),
            "{}",
            bench
        );
    }
}
