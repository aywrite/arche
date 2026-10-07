// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! A sample of the positions the search evaluates, for a corpus.
//!
//! The tuner's corpus is the positions games reached, and the evaluation is
//! read mostly at positions no game reaches: the stand pat of a capture
//! search, and the nodes of a full width search below the root. This keeps
//! one in `n` of those two kinds by key, as the other recorders do, turns
//! away what the corpus's own quiet test turns away, and can ask the
//! reference what each kept position is worth to a fixed depth. That is a
//! label with no pruning in it.
//!
//! The roots are searched to a depth or a node budget, whichever comes
//! first, on the bench's table, so the tree a root grows is the one a short
//! search from it grows.

use crate::bench::{self, Position};
use crate::board::Board;
use crate::engine::{AlphaBeta, Engine, SearchConfig, SearchParameters};
use crate::limits::Limits;
use crate::misc::Score;
use crate::recorder::{self, POSITIONS_LANE, Sampled, Sampler};
use crate::residual;
use crate::tune::{Filter, Quiet};
use std::fmt;

/// One position kept in this many offered, when the line does not say.
pub const DEFAULT_EVERY: u32 = 1_000;

/// Where the search read the evaluation of a sampled node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A node of the full width search that the table did not answer.
    Full,
    /// A capture search node not in check, where the side to move stands
    /// pat.
    Quiescence,
    /// A position of the suite itself, offered without a search, for a run
    /// that labels positions it was handed.
    Given,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Full => "full",
            Kind::Quiescence => "quiescence",
            Kind::Given => "given",
        }
    }
}

/// A node offered to the lane and kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: Kind,
    /// The depth left at the node, zero in quiescence.
    pub depth: u8,
    /// The root's place in the suite, counted from zero. A place and not
    /// the epd's id, since an id can hold spaces and a row is read left to
    /// right.
    pub root: usize,
    /// The key the node was drawn by. Rows from runs over different roots
    /// are merged by it and cut at a count, which keeps the smallest keys
    /// of the merged run as the cap does within one.
    pub key: u64,
    pub fen: String,
}

/// What an engine carries while the lane is armed.
#[derive(Debug)]
pub(crate) struct Arm {
    pub(crate) sampler: Sampler<Event>,
    pub(crate) root: usize,
}

impl Arm {
    pub(crate) fn offer(&mut self, kind: Kind, depth: u8, board: &Board) {
        let key = recorder::sample_key(board.key, POSITIONS_LANE, depth);
        let root = self.root;
        self.sampler.event(key, || Event {
            kind,
            depth,
            root,
            key,
            fen: board.to_fen(),
        });
    }
}

/// A kept event and, where the run labels, the reference's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub event: Event,
    pub reference: Option<Score>,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub depth: u8,
    pub every: u32,
    pub cap: usize,
    pub suite: Option<String>,
    pub budget: Option<u64>,
    /// The depth the reference labels to, or none for a run that only
    /// samples.
    pub label: Option<u8>,
    /// Whether the suite's positions were offered as given rather than
    /// searched.
    pub given: bool,
    pub roots: usize,
    pub events: u64,
    pub overflowed: u64,
    /// The samples the quiet test turned away, by its reason.
    pub in_check: usize,
    pub unsettled: usize,
    pub drawn: usize,
    pub rows: Vec<Row>,
}

/// Search every root with the lane armed, then judge what it kept and
/// label what survives. Never both at once: the filter and the reference
/// run on engines and tables of their own.
// one argument past clippy's limit: each is a setting the line names
#[allow(clippy::too_many_arguments)]
pub fn run(
    positions: &[Position],
    suite: Option<&str>,
    depth: u8,
    every: u32,
    cap: usize,
    budget: Option<u64>,
    label: Option<u8>,
    given: bool,
) -> Report {
    let depth = depth.max(1);
    let every = every.max(1);
    let sampled = if given {
        offer_given(positions, every, cap)
    } else {
        record(positions, depth, every, cap, budget)
    };
    let mut report = Report {
        depth,
        every,
        cap,
        suite: suite.map(str::to_string),
        budget,
        label: label.map(|depth| depth.max(1)),
        given,
        roots: positions.len(),
        events: sampled.events,
        overflowed: sampled.overflowed,
        in_check: 0,
        unsettled: 0,
        drawn: 0,
        rows: Vec::new(),
    };
    let mut filter = Filter::new();
    let mut reference = residual::replay_engine();
    for event in sampled.taken {
        let board = Board::from_fen(&event.fen).expect("a sampled node prints a fen it reads");
        let turned_away = match filter.judge(&board) {
            Quiet::Kept => None,
            Quiet::InCheck => Some(&mut report.in_check),
            Quiet::Unsettled => Some(&mut report.unsettled),
            Quiet::Drawn => Some(&mut report.drawn),
        };
        if let Some(count) = turned_away {
            *count += 1;
            continue;
        }
        let reference = report.label.map(|depth| {
            residual::reference_answer(&mut reference, &event.fen, depth)
                .expect("a sampled node prints a fen it reads")
        });
        report.rows.push(Row { event, reference });
    }
    report
}

/// The suite's own positions, each offered once at depth zero under the
/// lane's key, so `every` and `cap` keep a share of the suite as they keep
/// a share of a tree.
fn offer_given(positions: &[Position], every: u32, cap: usize) -> Sampled<Event> {
    let mut sampler = Sampler::with_cap(every, cap);
    for (root, position) in positions.iter().enumerate() {
        let board = position.board("positions");
        let key = recorder::sample_key(board.key, POSITIONS_LANE, 0);
        sampler.event(key, || Event {
            kind: Kind::Given,
            depth: 0,
            root,
            key,
            fen: board.to_fen(),
        });
    }
    sampler.drain()
}

/// The roots searched one after another with one reservoir, so the cap
/// describes the whole run.
fn record(
    positions: &[Position],
    depth: u8,
    every: u32,
    cap: usize,
    budget: Option<u64>,
) -> Sampled<Event> {
    let mut sampler = Sampler::with_cap(every, cap);
    for (root, position) in positions.iter().enumerate() {
        let mut engine = AlphaBeta::with_config(
            position.board("positions"),
            bench::TABLE_BYTES,
            SearchConfig::default(),
        );
        engine.arm_positions(Arm { sampler, root });
        let limits = match budget {
            None => Limits::unlimited(),
            Some(nodes) => Limits::starting_now(None, Some(nodes)),
        };
        // a root with no move answers at once and offers nothing
        engine.iterative_deepening_search(
            SearchParameters::new(Some(depth), limits),
            |_, _, _, _| {},
        );
        sampler = engine
            .disarm_positions()
            .expect("the lane just handed to the engine comes back")
            .sampler;
    }
    sampler.drain()
}

/// A header naming the run, then a row a kept position:
/// `kind depth root key reference fen`, the key in hex and the reference
/// `-` where the run did not label. The header's counts are the sampler's
/// and the filter's, so `events`, `records` and the three reasons say what
/// share of the search the rows describe.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        recorder::write_settings(
            f,
            "positions",
            self.depth,
            self.every,
            self.cap,
            self.suite.as_deref(),
        )?;
        if let Some(budget) = self.budget {
            write!(f, " budget {}", budget)?;
        }
        if let Some(label) = self.label {
            write!(f, " label {}", label)?;
        }
        if self.given {
            write!(f, " given")?;
        }
        let records = self.in_check + self.unsettled + self.drawn + self.rows.len();
        writeln!(
            f,
            " roots {} events {} records {} overflowed {} in_check {} unsettled {} drawn {} kept {}",
            self.roots,
            self.events,
            records,
            self.overflowed,
            self.in_check,
            self.unsettled,
            self.drawn,
            self.rows.len(),
        )?;
        for row in &self.rows {
            write!(
                f,
                "{} {} {} {:016x} ",
                row.event.kind.word(),
                row.event.depth,
                row.event.root,
                row.event.key
            )?;
            match row.reference {
                Some(score) => write!(f, "{}", score)?,
                None => write!(f, "-")?,
            }
            writeln!(f, " {}", row.event.fen)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::fixtures::{recording_leaves_the_search_where_it_was, suite};

    #[test]
    fn a_run_keeps_both_kinds_and_states_what_it_turned_away() {
        let report = run(&suite(), None, 4, 20, 1_000, None, None, false);
        let kinds = |kind| {
            report
                .rows
                .iter()
                .filter(|row| row.event.kind == kind)
                .count()
        };
        assert!(kinds(Kind::Full) > 0, "no full width node kept");
        assert!(kinds(Kind::Quiescence) > 0, "no quiescence node kept");
        assert!(report.unsettled > 0, "a tactical suite settles everywhere");
        assert!(report.rows.iter().all(|row| row.reference.is_none()));
    }

    #[test]
    fn every_kept_rows_fen_reads_back_as_a_position_terms_keeps() {
        let report = run(&suite(), None, 4, 20, 1_000, None, None, false);
        let positions: Vec<Position> = report
            .rows
            .iter()
            .map(|row| Position {
                id: row.event.fen.clone(),
                fen: row.event.fen.clone(),
                operations: Default::default(),
            })
            .collect();
        let terms = crate::tune::run(&positions, None);
        assert_eq!(terms.rows.len(), positions.len());
    }

    #[test]
    fn a_label_is_the_reference_answer_to_its_depth() {
        let report = run(&suite(), None, 3, 50, 20, None, Some(3), false);
        assert!(!report.rows.is_empty());
        // each on an engine of its own and in the other order, so a label
        // that leaned on what the run's shared engine searched before it
        // would differ
        for row in report.rows.iter().rev() {
            assert_eq!(
                row.reference,
                residual::reference_answer(&mut residual::replay_engine(), &row.event.fen, 3),
                "{}",
                row.event.fen
            );
        }
    }

    #[test]
    fn a_row_names_its_root_by_place_and_its_key_is_the_lane_key() {
        let roots = suite();
        // dense, since the sharp root's nodes are mostly unsettled
        let report = run(&roots, None, 4, 2, 1_000_000, None, None, false);
        for row in &report.rows {
            assert!(row.event.root < roots.len());
            let board = Board::from_fen(&row.event.fen).unwrap();
            assert_eq!(
                row.event.key,
                recorder::sample_key(board.key, POSITIONS_LANE, row.event.depth)
            );
        }
        let roots_seen: std::collections::HashSet<usize> =
            report.rows.iter().map(|row| row.event.root).collect();
        assert_eq!(roots_seen.len(), roots.len(), "a root offered nothing");
    }

    #[test]
    fn a_budget_stops_a_root_before_its_depth() {
        let deep = run(&suite(), None, 8, 1, 1_000_000, None, None, false);
        let budgeted = run(&suite(), None, 8, 1, 1_000_000, Some(5_000), None, false);
        assert!(budgeted.events < deep.events);
    }

    #[test]
    fn the_header_states_the_run_and_a_row_reads_left_to_right() {
        let report = run(
            &suite(),
            Some("roots.epd"),
            3,
            50,
            20,
            Some(9_000),
            Some(2),
            false,
        );
        let text = report.to_string();
        let mut lines = text.lines();
        let header = lines.next().unwrap();
        assert!(
            header.starts_with("positions depth 3 every 50 cap 20 epd roots.epd budget 9000 label 2 roots 2 events "),
            "{}",
            header
        );
        let row = lines.next().unwrap();
        let words: Vec<&str> = row.split_whitespace().collect();
        assert!(words[0] == "full" || words[0] == "quiescence", "{}", row);
        assert_eq!(words[3].len(), 16, "{}", row);
        assert!(words[4].parse::<Score>().is_ok(), "{}", row);
        assert_eq!(words.len(), 5 + 6, "{}", row);
    }

    #[test]
    fn a_given_run_offers_each_position_of_the_suite_and_labels_it() {
        let roots = suite();
        let report = run(&roots, None, 9, 1, 1_000, None, Some(3), true);
        assert_eq!(report.events, roots.len() as u64);
        let records = report.in_check + report.unsettled + report.drawn + report.rows.len();
        assert_eq!(records, roots.len());
        let mut engine = residual::replay_engine();
        for row in &report.rows {
            assert_eq!(row.event.kind, Kind::Given);
            assert_eq!(row.event.fen, roots[row.event.root].board("test").to_fen());
            assert_eq!(
                row.reference,
                residual::reference_answer(&mut engine, &row.event.fen, 3)
            );
        }
        assert!(
            report
                .to_string()
                .lines()
                .next()
                .unwrap()
                .contains(" label 3 given roots 2 ")
        );
    }

    #[test]
    fn arming_the_lane_leaves_the_search_where_it_was() {
        recording_leaves_the_search_where_it_was(
            5,
            |engine| {
                engine.arm_positions(Arm {
                    sampler: Sampler::with_cap(1, recorder::DEFAULT_CAP),
                    root: 0,
                })
            },
            |engine| {
                engine
                    .disarm_positions()
                    .expect("the lane comes back")
                    .sampler
                    .drain()
                    .taken
                    .len()
            },
        );
    }
}
