// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The tactical suite: a fixed set of positions, each searched to a fixed
//! depth with a fixed table, and how many of them the search found the move
//! in.
//!
//! The bench says how much of the tree the search looked at. It moves for any
//! change to the search, including one that changes nothing about how the
//! engine plays, so it says a great deal about what happened and nothing about
//! whether it was good. This says whether the search still finds the move,
//! which such a change should leave exactly where it was. A change that moves
//! one and not the other is worth being able to see.
//!
//! Deterministic for the reason the bench is: a fixed depth and a fixed table
//! make the count exact and the same on any machine, which is what lets it
//! gate rather than only report.

use crate::bench::{Position, parse_epd};
use crate::board::Board;
use crate::engine::{AlphaBeta, Engine, SearchConfig, SearchOutcome, SearchParameters};

/// The depth every position is searched to.
///
/// Chosen from a measurement and then frozen, because it is part of what the
/// count below means the way the bench's depth is part of what its node count
/// means. The suite solves 193, 232, 243, 258 and 276 of its three hundred at
/// depths four to eight, so it discriminates at any of them; what decides is
/// the clock. Six takes about eleven seconds here and a minute and a half on
/// a runner, where seven takes three times that for fifteen more positions,
/// and the fifty seven it does not solve are already plenty of room for the
/// count to move in either direction.
pub const DEPTH: u8 = 6;

/// The table every position is searched with, part of the count for the same
/// reason.
pub const TABLE_BYTES: usize = 16 * 1024 * 1024;

/// How many of the suite the search finds at that depth with that table.
///
/// Exact, not a floor. A change that raises it has to update this number in
/// the same commit, which is what puts the improvement in the diff rather than
/// leaving it to be noticed later or not at all.
pub const EXPECTED_PASSES: usize = 229;

const SUITE: &str = include_str!("../tactics.epd");

/// The suite's positions, in the order the file lists them.
pub fn positions() -> Vec<Position> {
    parse_epd(SUITE)
}

/// What one position's search found, and whether that was a move the suite
/// accepts.
#[derive(Debug, Clone)]
pub struct PositionReport {
    pub id: String,
    /// The move the search chose, in the notation the suite names its own in.
    pub found: String,
    /// Every move that counts as finding it. More than one is common: a
    /// position can have two ways to win and the suite takes either.
    pub wanted: Vec<String>,
    pub passed: bool,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub depth: u8,
    pub positions: Vec<PositionReport>,
}

impl Report {
    pub fn passes(&self) -> usize {
        self.positions.iter().filter(|p| p.passed).count()
    }

    /// The ones it did not find, for a run that has to say which moved.
    pub fn failures(&self) -> Vec<&PositionReport> {
        self.positions.iter().filter(|p| !p.passed).collect()
    }
}

/// Runs a suite under the settings given. Each position gets a fresh engine
/// and a fresh table and is deepened to the depth the way a game would be, so
/// the table is warm from each iteration to the next — the same shape the
/// bench runs in, because a position searched differently is a position
/// answered differently.
pub fn run_suite(
    positions: &[Position],
    depth: u8,
    table_bytes: usize,
    config: SearchConfig,
) -> Report {
    let depth = depth.max(1);
    let positions = positions
        .iter()
        .map(|position| {
            let board = Board::from_fen(&position.fen).unwrap_or_else(|e| {
                panic!("tactics position {} does not parse: {}", position.id, e)
            });
            // whitespace separated whole tokens rather than a fixed width:
            // the generator writes a promotion as five characters, and a
            // matcher that sliced four would read e7e8q as e7e8 and call a
            // queen and a knight the same move
            let wanted: Vec<String> = position
                .operations
                .get("bm")
                .map(|moves| moves.split_whitespace().map(String::from).collect())
                .unwrap_or_default();
            assert!(
                !wanted.is_empty(),
                "tactics position {} has no bm operation",
                position.id
            );
            let mut engine = AlphaBeta::with_config(board, table_bytes, config);
            let found = match engine
                .iterative_deepening_search(SearchParameters::to_depth(depth), |_, _, _, _| {})
            {
                SearchOutcome::Complete(result) => result.best_move.to_string(),
                other => panic!(
                    "tactics position {} did not complete: {:?}",
                    position.id, other
                ),
            };
            PositionReport {
                passed: wanted.contains(&found),
                id: position.id.clone(),
                found,
                wanted,
            }
        })
        .collect();
    Report { depth, positions }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_position_parses_and_names_a_move() {
        let positions = positions();
        assert_eq!(positions.len(), 300, "the suite is not the size it was");
        for position in &positions {
            assert!(
                Board::from_fen(&position.fen).is_ok(),
                "{} does not parse",
                position.id
            );
            let moves = position
                .operations
                .get("bm")
                .unwrap_or_else(|| panic!("{} has no bm", position.id));
            assert!(!moves.trim().is_empty(), "{} has an empty bm", position.id);
        }
    }

    /// Ignored because it searches all three hundred positions. A job of its
    /// own runs it in ci, and `cargo test --workspace --release -- --ignored`
    /// runs it by hand; leaving it in the default run would spend those minutes on three
    /// platforms that would agree with each other every time.
    #[test]
    #[ignore]
    fn the_suite_finds_what_it_found_before() {
        let report = run_suite(&positions(), DEPTH, TABLE_BYTES, SearchConfig::default());
        let missed: Vec<String> = report
            .failures()
            .iter()
            .map(|p| {
                format!(
                    "  {} played {} not {}",
                    p.id,
                    p.found,
                    p.wanted.join(" or ")
                )
            })
            .collect();
        assert_eq!(
            report.passes(),
            EXPECTED_PASSES,
            "the suite moved: {} of {} at depth {}. Missed:\n{}",
            report.passes(),
            report.positions.len(),
            DEPTH,
            missed.join("\n")
        );
    }
}
