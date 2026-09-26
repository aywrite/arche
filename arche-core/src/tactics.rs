// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The tactical suite: a fixed set of positions, each searched to a fixed
//! depth with a fixed table, and how many of them the search found the move
//! in.
//!
//! The bench says how much of the tree the search looked at; this says
//! whether it still finds the move. A fixed depth and a fixed table make the
//! count exact and the same on any machine, which is what lets it gate rather
//! than only report.

use crate::bench::{Position, parse_epd};
use crate::board::Board;
use crate::engine::{AlphaBeta, Engine, SearchConfig, SearchOutcome, SearchParameters};

/// The depth every position is searched to, chosen from a measurement and
/// then frozen, since it is part of what the count below means.
///
/// When the suite was first gated (6f11ca4) it solved 193, 232, 243, 258
/// and 276 of its three hundred at depths four to eight, so it discriminates
/// at any of them; what decided was the clock. Six took about eleven seconds
/// locally and a minute and a half on a runner, and seven three times that
/// for fifteen more positions.
pub const DEPTH: u8 = 6;

/// The table every position is searched with, part of the count for the same
/// reason.
pub const TABLE_BYTES: usize = 16 * 1024 * 1024;

/// How many of the suite the search finds at that depth with that table.
///
/// Exact, not a floor: a tripwire that says the suite moved, where `FLOOR`
/// is the gate. A change that moves it in either direction updates this
/// number in the same commit, and a change that lowers it says what the
/// positions were spent on.
pub const EXPECTED_PASSES: usize = 235;

/// The count the suite may not go under, whatever a commit says it meant to
/// spend.
///
/// Held apart from `EXPECTED_PASSES` and moved only on its own account:
/// otherwise each change rearms the tripwire a notch lower and nothing stops
/// the count walking down until the suite says nothing at all.
///
/// Two hundred and ten was fourteen under the count when the floor was set
/// (224) and is eleven under the lowest the suite has been gated at (221).
/// The largest single step down since the suite was first gated is eight
/// (634083f), so one change spending fourteen fails here rather than being
/// written down and rearmed. Lowering the floor is a commit whose whole
/// subject is lowering the floor.
pub const FLOOR: usize = 210;

// Checked by the build rather than the ignored suite run. Strictly under: a
// floor standing on the snapshot would fail on the next change that spends
// one position, and whoever raised it would raise both, which is the ratchet
// the floor is there to refuse.
const _: () = assert!(EXPECTED_PASSES > FLOOR);

/// A position the engine has knowingly given up, with what bought it and the
/// version its acceptance runs out at.
///
/// Not a second floor, and not part of the count: the suite still has to
/// match `EXPECTED_PASSES` exactly and clear `FLOOR`. The list names a loss
/// taken on purpose, and the expiry stops that naming settling it for ever.
#[derive(Debug, Clone, Copy)]
pub struct AcceptedLoss {
    /// The suite id, as `tactics.epd` writes it.
    pub id: &'static str,
    /// What the position was spent on.
    pub why: &'static str,
    /// The first version at which the acceptance no longer holds. From then
    /// the tests fail until someone wins the position back or writes down a
    /// fresh reason and a later version.
    pub until: &'static str,
}

/// The losses accepted so far.
///
/// WAC.082 and WAC.260 are the reduction table's (1f805b2): a move it never
/// touches or reaches late, whose continuation is quiet and is now scouted
/// further back than the depth can afford.
///
/// WAC.023 to WAC.280 are the late move count's (5bc4e91), which refuses a
/// quiet a node of depth one to three has reached past four moves a ply, so
/// a winning line with such a quiet in it is not searched at those depths.
/// Each names what depth six answers with instead and the depth the suite's
/// move comes back at, read one position at a time on that commit. WAC.022
/// was one of them until 188f2f6 found it again through the suite's other
/// move c4a2.
///
/// WAC.150 is mate distance pruning's (c7730f1). The position holds no mate
/// inside depth six, so what moved it is the reordering of a subtree deeper
/// down that does, and it is borderline either way: that commit answers
/// d6e5 at six and at seven, and the suite's d6a3 at eight.
pub const ACCEPTED_LOSSES: &[AcceptedLoss] = &[
    AcceptedLoss {
        id: "WAC.082",
        why: "the quiet queen lift and rook swing behind the h7 sacrifice are \
              scouted further back, so the sacrifice never comes back above \
              alpha at depth six",
        until: "0.6.0",
    },
    AcceptedLoss {
        id: "WAC.260",
        why: "the three quiet moves of the mate in five behind the queen check \
              are scouted further back, so depth six answers with a centipawn \
              score instead",
        until: "0.6.0",
    },
    AcceptedLoss {
        id: "WAC.023",
        why: "depth six answers d4f4 at 301 and the pawn push g2g4 comes \
              back at depth seven at 387",
        until: "0.6.0",
    },
    AcceptedLoss {
        id: "WAC.069",
        why: "depth six answers e6e8 at 370 and the quiet luft f2f3 comes \
              back at depth seven at the same 370",
        until: "0.6.0",
    },
    AcceptedLoss {
        id: "WAC.167",
        why: "depth six answers f2f1 at 184 and depth seven answers f2g2 \
              with a mate in five",
        until: "0.6.0",
    },
    AcceptedLoss {
        id: "WAC.232",
        why: "depth six answers a6b7 at 19 and the rook trade b8e8 comes \
              back at depth seven at 311",
        until: "0.6.0",
    },
    AcceptedLoss {
        id: "WAC.239",
        why: "depth six answers f2e2 at -61 and f2f1 comes back at depth \
              eleven at 0, the furthest of the count's losses",
        until: "0.6.0",
    },
    AcceptedLoss {
        id: "WAC.242",
        why: "depth six answers b1a2 at 33 and the rook to the seventh \
              comes back at depth seven at 353",
        until: "0.6.0",
    },
    AcceptedLoss {
        id: "WAC.243",
        why: "depth six answers h2h3 at 145 and the queen step f2e2 comes \
              back at depth seven at 144",
        until: "0.6.0",
    },
    AcceptedLoss {
        id: "WAC.266",
        why: "depth six answers f2g3 at 11 and depth seven answers h8h2 \
              with a mate in six",
        until: "0.6.0",
    },
    AcceptedLoss {
        id: "WAC.280",
        why: "depth six answers c2e2 at 76 and the bishop to a3 comes back \
              at depth seven at 94",
        until: "0.6.0",
    },
    AcceptedLoss {
        id: "WAC.150",
        why: "depth six answers d6e5 at 529 and the bishop retreat d6a3 comes \
              back at depth eight at 420",
        until: "0.6.0",
    },
];

const SUITE: &str = include_str!("../tactics.epd");

/// A version as three numbers, for holding an expiry against the crate's
/// own. Anything after the patch number is dropped, so an acceptance that
/// runs out at 0.6.0 has run out by the time 0.6.0-rc1 is built. A part that
/// is not a number reads as zero, so a mistyped expiry has already run out
/// rather than lasting for ever.
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
/// positions in and the version in force. Empty when nothing is. Runs no
/// search, so the default test run holds the list to it.
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

/// Accepted losses the run found passing, which come off the list.
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
    /// Every move that counts as finding it; a position with two ways to
    /// win names both.
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

/// Runs a suite under the settings given, in the shape the bench runs in:
/// each position gets a fresh engine and a fresh table and is deepened to
/// the depth the way a game would be.
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
            // whole tokens rather than a fixed width: a promotion is five
            // characters, and a matcher that sliced four would call a queen
            // and a knight the same move
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
                SearchOutcome::Complete(result, _) => result.best_move.to_string(),
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
            // a pass is a string comparison, so a `bm` the generator can
            // never emit is a position that can never pass, folded silently
            // into `EXPECTED_PASSES`. The suite is converted from san by a
            // script, so that is a conversion slip rather than a hypothetical
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

    /// Ignored because it searches the whole suite. A job of its own runs it
    /// in ci; see docs/DEVELOPMENT.md for running it by hand.
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
        // the floor first, with a message of its own: under the floor is a
        // different event from off the snapshot
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
        let faults = accepted_loss_faults(ACCEPTED_LOSSES, &positions(), env!("CARGO_PKG_VERSION"));
        assert!(faults.is_empty(), "{}", faults.join("\n"));
    }

    #[test]
    fn a_fault_in_the_allowlist_is_named() {
        // the check above passes over an empty list, so this is what says it
        // can fail: a made up list carrying each fault once
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
        assert_eq!(version_triple("0.4.4"), (0, 4, 4));
        assert_eq!(version_triple("0.6.0-rc1"), (0, 6, 0));
        assert!(version_triple("0.6.0-rc1") >= version_triple("0.6.0"));
        assert!(version_triple("0.5.9") < version_triple("0.6.0"));
    }
}
