// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The strategic suite: a fixed set of quiet positions, each searched to a
//! fixed depth with a fixed table, and how many of the points on offer the
//! moves it chose were worth.
//!
//! The tactical suite says whether the search still finds the move. Nearly
//! every position in it has one answer, and a change that makes the engine
//! worse at quiet play can leave its count exactly where it was. This says
//! whether the evaluation still prefers the same kind of position, which is
//! the other half of the question, and a change that moves one and not the
//! other is worth being able to see.
//!
//! It grades rather than passes or fails. Each position carries up to ten
//! moves with a score out of a hundred, so a move that is second best is
//! worth most of the points and the total moves by a little where a count of
//! solved positions would not move at all.
//!
//! Deterministic for the reason the tactical suite is: a fixed depth and a
//! fixed table make the total exact and the same on any machine, which is
//! what lets it gate rather than only report.

use crate::bench::{Position, parse_epd};
use crate::engine::SearchConfig;
use crate::tactics;
use std::fmt;

/// The depth every position is searched to.
///
/// The same six the tactical suite uses, and part of what the total below
/// means the way the bench's depth is part of what its node count means. The
/// suite takes 57.4, 60.3, 61.9 and 63.3 percent of its points at depths four
/// to seven, so it discriminates at any of them; what decides is the clock.
/// Six takes about thirteen seconds here, where seven takes thirty five for
/// another one and a half points, and the two fifths of the points it leaves
/// are already plenty of room for the total to move in either direction.
///
/// Five times the positions of the tactical suite for not much more than its
/// time: a quiet middlegame cuts off far sooner than a Win At Chess tactic
/// does.
pub const DEPTH: u8 = 6;

/// The table every position is searched with, part of the total for the same
/// reason.
pub const TABLE_BYTES: usize = 16 * 1024 * 1024;

/// How many of the suite's points the search takes at that depth with that
/// table.
///
/// Exact, not a floor. A change that raises it has to update this number in
/// the same commit, which is what puts the improvement in the diff rather
/// than leaving it to be noticed later or not at all.
pub const EXPECTED_POINTS: u32 = 92352;

const SUITE: &str = include_str!("../strategy.epd");

/// The suite's positions, in the order the file lists them.
pub fn positions() -> Vec<Position> {
    parse_epd(SUITE)
}

/// The theme a position belongs to, which is its id up to the number within
/// the theme: `Undermine.001` is `Undermine`.
pub fn theme(id: &str) -> &str {
    id.rsplit_once('.').map_or(id, |(theme, _)| theme)
}

/// A position's graded moves and what each is worth, in the order the file
/// lists them, which is the source's order of preference.
///
/// The file is generated and committed, so a line that does not read is a
/// broken suite rather than an input to be handled: it panics the way the
/// tactical suite's reader does.
pub fn points(position: &Position) -> Vec<(&str, u32)> {
    let operand = position
        .operations
        .get("points")
        .unwrap_or_else(|| panic!("strategy position {} has no points", position.id));
    operand
        .split_whitespace()
        .map(|entry| {
            let (play, score) = entry.split_once('=').unwrap_or_else(|| {
                panic!(
                    "strategy position {} has {} for a score",
                    position.id, entry
                )
            });
            let score = score.parse().unwrap_or_else(|e| {
                panic!(
                    "strategy position {}: {} is not a score: {}",
                    position.id, entry, e
                )
            });
            (play, score)
        })
        .collect()
}

/// What one theme's hundred positions came to.
#[derive(Debug, Clone)]
pub struct ThemeReport {
    pub theme: String,
    pub positions: usize,
    /// The points the moves the search chose were worth.
    pub scored: u32,
    /// The points on offer, which is the top score of each position.
    pub available: u32,
    /// How many of the positions the search played a top scoring move in,
    /// which is what the tactical suite's pass count measures.
    pub top_moves: usize,
}

impl ThemeReport {
    /// The share of the theme's points taken, as a percentage.
    pub fn share(&self) -> f64 {
        if self.available == 0 {
            0.0
        } else {
            100.0 * f64::from(self.scored) / f64::from(self.available)
        }
    }
}

/// The suite's themes in the order the file lists them, and the totals.
#[derive(Debug, Clone)]
pub struct Report {
    pub depth: u8,
    /// The table the run used, which is part of what the totals mean and so
    /// is carried rather than read back off the constant.
    pub table_bytes: usize,
    pub themes: Vec<ThemeReport>,
}

impl Report {
    pub fn scored(&self) -> u32 {
        self.themes.iter().map(|theme| theme.scored).sum()
    }

    pub fn available(&self) -> u32 {
        self.themes.iter().map(|theme| theme.available).sum()
    }

    pub fn positions(&self) -> usize {
        self.themes.iter().map(|theme| theme.positions).sum()
    }

    pub fn top_moves(&self) -> usize {
        self.themes.iter().map(|theme| theme.top_moves).sum()
    }

    pub fn share(&self) -> f64 {
        if self.available() == 0 {
            0.0
        } else {
            100.0 * f64::from(self.scored()) / f64::from(self.available())
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // the name column is as wide as the widest theme, one of which runs
        // to three words joined by slashes
        let width = self
            .themes
            .iter()
            .map(|theme| theme.theme.len())
            .max()
            .unwrap_or(0)
            .max("theme".len());
        writeln!(
            f,
            "strategy depth {} hash {}MB positions {}",
            self.depth,
            self.table_bytes / (1024 * 1024),
            self.positions()
        )?;
        writeln!(
            f,
            "{:<width$} {:>9} {:>8} {:>8} {:>7} {:>5}",
            "theme", "positions", "points", "of", "share", "top"
        )?;
        for theme in &self.themes {
            writeln!(
                f,
                "{:<width$} {:>9} {:>8} {:>8} {:>6.1}% {:>5}",
                theme.theme,
                theme.positions,
                theme.scored,
                theme.available,
                theme.share(),
                theme.top_moves
            )?;
        }
        write!(
            f,
            "{:<width$} {:>9} {:>8} {:>8} {:>6.1}% {:>5}",
            "total",
            self.positions(),
            self.scored(),
            self.available(),
            self.share(),
            self.top_moves()
        )
    }
}

/// Runs the suite under the settings given.
///
/// The search itself is the tactical suite's runner, which already deepens
/// each position the way a game would and reports the move it settled on.
/// With `bm` holding the top scoring moves its `passed` says the search
/// played one of them, so all that is left here is to look the move it played
/// up in the position's points, which is nothing when it played something the
/// source did not grade.
pub fn run_suite(
    positions: &[Position],
    depth: u8,
    table_bytes: usize,
    config: SearchConfig,
) -> Report {
    let found = tactics::run_suite(positions, depth, table_bytes, config);
    debug_assert_eq!(
        positions.len(),
        found.positions.len(),
        "the runner answered a different suite"
    );
    let mut themes: Vec<ThemeReport> = Vec::new();
    for (position, report) in positions.iter().zip(&found.positions) {
        let graded = points(position);
        let available = graded.iter().map(|&(_, score)| score).max().unwrap_or(0);
        let scored = graded
            .iter()
            .find(|&&(play, _)| play == report.found)
            .map_or(0, |&(_, score)| score);
        let name = theme(&position.id);
        let theme = match themes.iter_mut().find(|theme| theme.theme == name) {
            Some(theme) => theme,
            None => {
                themes.push(ThemeReport {
                    theme: name.to_string(),
                    positions: 0,
                    scored: 0,
                    available: 0,
                    top_moves: 0,
                });
                themes.last_mut().expect("just pushed")
            }
        };
        theme.positions += 1;
        theme.scored += scored;
        theme.available += available;
        theme.top_moves += usize::from(report.passed);
    }
    Report {
        depth: found.depth,
        table_bytes,
        themes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;
    use std::collections::HashSet;

    /// The points on offer, which is what the shares this suite prints are
    /// taken out of and what the development notes publish beside the total.
    /// A line whose top score changed would move it and leave the total
    /// where it was, so it is pinned here rather than only printed.
    const AVAILABLE: u32 = 149_703;

    #[test]
    fn every_position_parses_and_names_a_move() {
        for position in &positions() {
            let board = Board::from_fen(&position.fen)
                .unwrap_or_else(|e| panic!("{} does not parse: {}", position.id, e));
            let graded = points(position);
            assert!(!graded.is_empty(), "{} grades no moves", position.id);
            // the file names its moves the way the engine writes its own, so
            // a move the position does not offer is a file the search can
            // never score against, and castling is where the two notations
            // would part company first
            let generated: Vec<String> = board
                .generate_moves()
                .iter()
                .map(|play| play.to_string())
                .collect();
            for &(play, _) in &graded {
                assert!(
                    generated.iter().any(|generated| generated == play),
                    "{} grades {}, which it does not offer",
                    position.id,
                    play
                );
            }
            let top = graded
                .iter()
                .map(|&(_, score)| score)
                .max()
                .expect("a graded move");
            let best = position
                .operations
                .get("bm")
                .unwrap_or_else(|| panic!("{} has no bm", position.id));
            let named: Vec<&str> = best.split_whitespace().collect();
            let scoring: Vec<&str> = graded
                .iter()
                .filter(|&&(_, score)| score == top)
                .map(|&(play, _)| play)
                .collect();
            // exactly the moves at the top score, no more and no less: one
            // dropped from a tie would call a best move a miss
            assert_eq!(
                named,
                scoring,
                "{} names {} as best and grades {} at the top",
                position.id,
                named.join(" "),
                scoring.join(" ")
            );
        }
    }

    #[test]
    fn the_suite_is_the_themes_and_the_points_it_was() {
        let positions = positions();
        assert_eq!(positions.len(), 1500, "the suite is not the size it was");
        let mut seen = HashSet::new();
        let mut themes: Vec<(&str, usize)> = Vec::new();
        let mut available = 0;
        for position in &positions {
            assert!(
                seen.insert(position.id.as_str()),
                "{} is named twice",
                position.id
            );
            available += points(position)
                .iter()
                .map(|&(_, score)| score)
                .max()
                .unwrap_or(0);
            let name = theme(&position.id);
            match themes.iter_mut().find(|(theme, _)| *theme == name) {
                Some((_, counted)) => *counted += 1,
                None => themes.push((name, 1)),
            }
        }
        assert_eq!(themes.len(), 15, "the suite is not fifteen themes");
        for (theme, counted) in &themes {
            assert_eq!(*counted, 100, "{theme} is not a hundred positions");
        }
        assert_eq!(available, AVAILABLE, "the points on offer moved");
    }

    /// Ignored because it searches all fifteen hundred positions. A job of
    /// its own runs it in ci, and `cargo test --workspace --release --
    /// --ignored` runs it by hand; leaving it in the default run would spend
    /// those minutes on three platforms that would agree with each other
    /// every time.
    #[test]
    #[ignore]
    fn the_strategy_suite_scores_what_it_scored_before() {
        let report = run_suite(&positions(), DEPTH, TABLE_BYTES, SearchConfig::default());
        assert_eq!(
            report.scored(),
            EXPECTED_POINTS,
            "the suite moved: {} of {} at depth {}. By theme:\n{}",
            report.scored(),
            report.available(),
            DEPTH,
            report
        );
    }
}
