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
/// leaving it to be noticed later or not at all. A change that lowers it has
/// the same duty and a heavier one: this is the tactical tripwire, and it had
/// moved by one in each direction across every evaluation arm before the
/// 2026-09-13 mobility refit took it down seven at once. What that bought is
/// in the refit's commit, and the suite is not a gate. The 2026-09-16 king
/// attack fit took it up thirteen.
pub const EXPECTED_PASSES: usize = 237;

/// The count the suite may not go under, whatever a commit says it meant to
/// spend.
///
/// Held apart from `EXPECTED_PASSES` and moved only on its own account. The
/// exact count is a tripwire: it says the suite moved, and a change that
/// meant to move it updates the number and the tripwire is rearmed one notch
/// lower. Nothing in that stops the number walking down a few positions at a
/// time until the suite says nothing at all, because each step is small and
/// each is argued for on its own.
///
/// Two hundred and ten was fourteen under where the count stood when the
/// floor was set, and is eleven under the lowest it has been gated at (221, on
/// the model gated two ply reduction). Since the suite was first gated the
/// count has been 243, 241, 240, 237, 236, 229, 221, 226, 227, 228, 231, 224
/// and 237, and the largest single step down in that list is eight. So one
/// change spending fourteen is spending most of two of the largest steps ever
/// taken, and this fails rather than being written down and rearmed. Lowering
/// the floor is a commit whose whole subject is lowering the floor.
pub const FLOOR: usize = 210;

// The snapshot cannot be set under the floor without moving the floor, and
// the build says so rather than the suite run, which is a job of its own and
// runs on three platforms. Strictly under, not equal: a floor standing on
// the snapshot would fail on the next arm that spends one position, and
// whoever raised it would raise both, which is the ratchet the floor is
// there to refuse.
const _: () = assert!(EXPECTED_PASSES > FLOOR);

/// A position the engine has knowingly given up, with what bought it and the
/// version its acceptance runs out at.
///
/// This is what tells a change that spent seven positions and said which
/// from a change that spent seventy and said nothing. It is not a second
/// floor and it does not enter the count: the suite still has to match
/// `EXPECTED_PASSES` exactly and still has to clear `FLOOR`. What the list
/// adds is that a loss taken on purpose is named, and that naming it does
/// not settle the matter for ever.
#[derive(Debug, Clone, Copy)]
pub struct AcceptedLoss {
    /// The suite id, as `tactics.epd` writes it.
    pub id: &'static str,
    /// What the position was spent on.
    pub why: &'static str,
    /// The first version at which the acceptance no longer holds. At that
    /// version the tests fail until someone either wins the position back or
    /// writes down a fresh reason and a later version.
    pub until: &'static str,
}

/// The losses accepted so far.
///
/// Empty. The suite misses sixty three positions at the pinned depth and
/// none of them has been read one at a time, so there is nothing here that
/// would be a record rather than a guess. The next change that lowers
/// `EXPECTED_PASSES` is the first that writes here, naming what it spent.
pub const ACCEPTED_LOSSES: &[AcceptedLoss] = &[];

const SUITE: &str = include_str!("../tactics.epd");

/// A version as three numbers, for holding an expiry against the crate's
/// own. Anything after the patch number is dropped, so an acceptance that
/// runs out at 0.6.0 has run out by the time 0.6.0-rc1 is built. A part that
/// is not a number reads as zero, which means a mistyped expiry has already
/// run out rather than lasting for ever: the direction to fail in.
fn version_triple(version: &str) -> (u32, u32, u32) {
    let mut parts = version
        .split(['.', '-', '+'])
        .map(|part| part.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// What is wrong with the allowlist, read against the suite it names
/// positions in and the version in force. Empty when nothing is.
///
/// Cheap: it runs no search, so the default test run holds the list to this
/// rather than leaving it to the suite job.
pub fn accepted_loss_faults(
    accepted: &[AcceptedLoss],
    suite: &[Position],
    version: &str,
) -> Vec<String> {
    let now = version_triple(version);
    let mut faults = Vec::new();
    for (at, loss) in accepted.iter().enumerate() {
        if !suite.iter().any(|position| position.id == loss.id) {
            faults.push(format!("{} is not a position in the suite", loss.id));
        }
        if accepted[..at].iter().any(|earlier| earlier.id == loss.id) {
            faults.push(format!("{} is accepted twice", loss.id));
        }
        if loss.why.trim().is_empty() {
            faults.push(format!("{} is accepted for no stated reason", loss.id));
        }
        if now >= version_triple(loss.until) {
            faults.push(format!(
                "{} was accepted until {} and this is {}: win it back or write down a new reason",
                loss.id, loss.until, version
            ));
        }
    }
    faults
}

/// Accepted losses the run found passing. An acceptance nothing is spending
/// any more comes off the list rather than sitting on it, since a list that
/// keeps entries it no longer needs stops being read.
pub fn stale_acceptances<'a>(
    accepted: &'a [AcceptedLoss],
    report: &Report,
) -> Vec<&'a AcceptedLoss> {
    accepted
        .iter()
        .filter(|loss| {
            report
                .positions
                .iter()
                .any(|position| position.id == loss.id && position.passed)
        })
        .collect()
}

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
/// the table is warm from each iteration to the next, the same shape the
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
            let board = Board::from_fen(&position.fen)
                .unwrap_or_else(|e| panic!("suite position {} does not parse: {}", position.id, e));
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
                "suite position {} has no bm operation",
                position.id
            );
            let mut engine = AlphaBeta::with_config(board, table_bytes, config);
            let found = match engine
                .iterative_deepening_search(SearchParameters::to_depth(depth), |_, _, _, _| {})
            {
                SearchOutcome::Complete(result) => result.best_move.to_string(),
                other => panic!(
                    "suite position {} did not complete: {:?}",
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
            // the file names its moves the way the engine writes its own, and
            // `run_suite` decides a pass by comparing those strings. A `bm` the
            // generator can never emit is therefore a position that can never
            // pass, folded silently into `EXPECTED_PASSES` as though the search
            // had missed it. The suite is converted from san by a script, so
            // that is a conversion slip rather than a hypothetical. The
            // strategic suite has carried this check since it was written.
            let board = Board::from_fen(&position.fen).unwrap();
            let generated: Vec<String> = board
                .generate_moves()
                .iter()
                .map(|play| play.to_string())
                .collect();
            for wanted in moves.split_whitespace() {
                assert!(
                    generated.iter().any(|generated| generated == wanted),
                    "{} names {}, which it does not offer",
                    position.id,
                    wanted
                );
            }
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
        // the floor first, and with a message of its own: under the floor
        // is a different event from off the snapshot, and the number to
        // reach for is not the same number
        assert!(
            report.passes() >= FLOOR,
            "the suite is under its floor of {}: {} of {} at depth {}. This is not a snapshot to update. Missed:\n{}",
            FLOOR,
            report.passes(),
            report.positions.len(),
            DEPTH,
            missed.join("\n")
        );
        assert_eq!(
            report.passes(),
            EXPECTED_PASSES,
            "the suite moved: {} of {} at depth {}. Missed:\n{}",
            report.passes(),
            report.positions.len(),
            DEPTH,
            missed.join("\n")
        );
        let stale = stale_acceptances(ACCEPTED_LOSSES, &report);
        assert!(
            stale.is_empty(),
            "the suite finds these again, so they are not losses to accept: {}",
            stale
                .iter()
                .map(|loss| loss.id)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    #[test]
    fn the_accepted_losses_name_real_positions_and_have_not_run_out() {
        // the half of the allowlist that costs no search, so the default run
        // holds it rather than the suite job, which is ignored and runs on
        // three platforms
        let faults = accepted_loss_faults(ACCEPTED_LOSSES, &positions(), env!("CARGO_PKG_VERSION"));
        assert!(faults.is_empty(), "{}", faults.join("\n"));
    }

    #[test]
    fn a_fault_in_the_allowlist_is_named() {
        // the check above passes over an empty list, so this is what says it
        // can fail: one made up list carrying each fault once
        let suite = positions();
        let accepted = [
            AcceptedLoss {
                id: "no.such.position",
                why: "a typo, or a position the suite no longer carries",
                until: "99.0.0",
            },
            AcceptedLoss {
                id: "WAC.001",
                why: "",
                until: "99.0.0",
            },
            AcceptedLoss {
                id: "WAC.001",
                why: "the same position named twice",
                until: "99.0.0",
            },
            AcceptedLoss {
                id: "WAC.002",
                why: "accepted against a version already past",
                until: "0.0.1",
            },
        ];
        let faults = accepted_loss_faults(&accepted, &suite, "0.4.4");
        assert_eq!(faults.len(), 4, "{}", faults.join("\n"));
        assert!(faults[0].contains("not a position in the suite"));
        assert!(faults[1].contains("no stated reason"));
        assert!(faults[2].contains("accepted twice"));
        assert!(faults[3].contains("write down a new reason"));

        // and a sound entry against a version it outlives raises nothing
        let sound = [AcceptedLoss {
            id: "WAC.003",
            why: "spent on something the commit names",
            until: "99.0.0",
        }];
        assert!(accepted_loss_faults(&sound, &suite, "0.4.4").is_empty());
    }

    #[test]
    fn an_expiry_is_read_to_the_patch_number_and_no_further() {
        // a pre-release of the version an acceptance runs out at has run out
        assert_eq!(version_triple("0.4.4"), (0, 4, 4));
        assert_eq!(version_triple("0.6.0-rc1"), (0, 6, 0));
        assert!(version_triple("0.6.0-rc1") >= version_triple("0.6.0"));
        assert!(version_triple("0.5.9") < version_triple("0.6.0"));
    }
}
