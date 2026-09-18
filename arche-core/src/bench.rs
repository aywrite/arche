// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The bench: a fixed suite of positions, each searched to a fixed depth with
//! a fixed table, and what the searches counted.
//!
//! The search is deterministic, so the node count is exact and moves if and
//! only if the tree searched moves. That makes it the signature of a search
//! change, which an engine commit states in its `Bench:` trailer. The speed
//! is what the match tools scale their time controls by, so the clock runs
//! over the search alone and not over allocating the table.

use crate::board::Board;
use crate::engine::{AlphaBeta, Engine, SearchConfig, SearchOutcome, SearchParameters};
use crate::misc::Score;
use crate::play::Play;
use crate::transposition::SignatureCounters;
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

/// The depth every position is searched to. Seven until September 2026 and
/// nine since, so a `Bench:` trailer or a note from before then that says
/// the bench's depth means seven.
pub const DEPTH: u8 = 9;

/// The table every position is searched with. The tree moves with the
/// table, so this is part of what the numbers mean.
pub const TABLE_BYTES: usize = 16 * 1024 * 1024;

const SUITE: &str = include_str!("../bench.epd");

/// A position of the suite, as a full fen and the name the report gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    pub id: String,
    pub fen: String,
    /// The operations the line carried, by opcode, for whichever reader
    /// knows what they mean: the bench reads none, the tactical suite reads
    /// `bm` and the strategic suite `points`.
    pub operations: HashMap<String, String>,
}

/// The suite's positions, in the order the file lists them.
pub fn positions() -> Vec<Position> {
    parse_epd(SUITE)
}

/// Reads epd: the first four fields are the fen, and an `id "..."` operation
/// among those that follow names the position. The clocks an epd leaves out
/// are filled in as zero and one, a position that has just arisen. A line
/// with no id is named by its fen. Blank lines and lines opening with `#`
/// are skipped.
pub fn parse_epd(text: &str) -> Vec<Position> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut words = line.split_whitespace();
            let fen: Vec<&str> = words.by_ref().take(4).collect();
            let fen = format!("{} 0 1", fen.join(" "));
            // an operation with no operands is dropped: it would arrive as
            // an empty string no reader could tell from a missing one. The
            // quotes around an id are epd syntax rather than part of the
            // name, so they come off here
            let operations: HashMap<String, String> = words
                .collect::<Vec<&str>>()
                .join(" ")
                .split(';')
                .map(str::trim)
                .filter_map(|operation| operation.split_once(char::is_whitespace))
                .map(|(opcode, operands)| {
                    (
                        opcode.to_string(),
                        operands.trim().trim_matches('"').to_string(),
                    )
                })
                .collect();
            let id = operations.get("id").cloned().unwrap_or_else(|| fen.clone());
            Position {
                id,
                fen,
                operations,
            }
        })
        .collect()
}

/// What one position's search counted.
#[derive(Debug, Clone)]
pub struct PositionReport {
    pub id: String,
    /// The move the search chose and the score it gave it, from the side to
    /// move: a change that moves either has changed the answer and not only
    /// the tree that found it.
    pub play: Play,
    pub score: Score,
    pub nodes: u64,
    /// The nodes quiescence visited, a part of nodes.
    pub quiescence_nodes: u64,
    /// Probes that cut the search off with a stored score.
    pub tt_cutoffs: u64,
    /// Entries stored in total.
    pub tt_stores: u64,
    /// Entries stored with a draw tainted score, a part of the stores.
    pub tainted_stores: u64,
    /// Cutoffs taken from a draw tainted score. Zero under a configuration
    /// that refuses every one of them; under the default, which refuses only
    /// near the fifty move horizon, it counts the ones taken away from it.
    pub tainted_cutoffs: u64,
    /// Cutoffs refused for their taint and searched instead, which is what
    /// refusing costs; zero under a configuration that trusts them. Under
    /// the rule50 policy this counts its horizon refusals instead.
    pub refused_cutoffs: u64,
    /// Tainted results not stored, under the policy that keeps only clean
    /// scores; zero under every other.
    pub skipped_stores: u64,
    /// What the table's signature audit counted, or none, which is what an
    /// unaudited run reports.
    pub signatures: Option<SignatureCounters>,
    /// The search alone, not the table's allocation.
    pub elapsed: Duration,
}

/// The whole bench: the settings it ran with and what each position counted.
#[derive(Debug, Clone)]
pub struct Report {
    pub depth: u8,
    pub table_bytes: usize,
    pub config: SearchConfig,
    pub positions: Vec<PositionReport>,
}

impl Report {
    pub fn nodes(&self) -> u64 {
        self.positions.iter().map(|p| p.nodes).sum()
    }

    pub fn elapsed(&self) -> Duration {
        self.positions.iter().map(|p| p.elapsed).sum()
    }

    /// Nodes a second over the whole bench.
    pub fn nps(&self) -> u64 {
        nps(self.nodes(), self.elapsed())
    }

    /// What the signature audit counted over the whole suite, summed over
    /// the positions' tables, or none when the run was not audited.
    pub fn signatures(&self) -> Option<SignatureCounters> {
        self.positions
            .iter()
            .filter_map(|p| p.signatures)
            .reduce(|mut total, counted| {
                total.absorb(counted);
                total
            })
    }
}

/// Counted in microseconds, so a position searched in well under a
/// millisecond still gets a rate, and over at least one so the rate stays
/// finite.
fn nps(nodes: u64, elapsed: Duration) -> u64 {
    (nodes as u128 * 1_000_000 / elapsed.as_micros().max(1)) as u64
}

/// Runs a suite under the settings given. Each position gets a fresh engine
/// and a fresh table, allocated before its clock starts, and is deepened to
/// the depth the way a game would be. A depth of zero runs no iteration and
/// counts nothing, so it is searched as one.
pub fn run_suite(
    positions: &[Position],
    depth: u8,
    table_bytes: usize,
    config: SearchConfig,
) -> Report {
    run(positions, depth, table_bytes, config, false)
        .expect("an unaudited run asks for no keys and so cannot fail to get them")
}

/// The same suite, with each table keeping the full key of every entry so
/// that the report can say how often the thirty two bit signature accepted
/// another position's entry. The audit counts and does nothing else, so the
/// tree and the node counts are `run_suite`'s.
///
/// None if there was not the memory for the keys, which are half a table's
/// size again, rather than a report with no audit in it.
pub fn run_audited_suite(
    positions: &[Position],
    depth: u8,
    table_bytes: usize,
    config: SearchConfig,
) -> Option<Report> {
    run(positions, depth, table_bytes, config, true)
}

fn run(
    positions: &[Position],
    depth: u8,
    table_bytes: usize,
    config: SearchConfig,
    audit: bool,
) -> Option<Report> {
    let depth = depth.max(1);
    let positions = positions
        .iter()
        .map(|position| {
            let board = Board::from_fen(&position.fen)
                .unwrap_or_else(|e| panic!("bench position {} does not parse: {}", position.id, e));
            let mut engine = AlphaBeta::with_config(board, table_bytes, config);
            if audit && !engine.audit_signatures() {
                return None;
            }
            let outcome = engine
                .iterative_deepening_search(SearchParameters::to_depth(depth), |_, _, _, _| {});
            let result = match outcome {
                SearchOutcome::Complete(result, _) => result,
                other => panic!(
                    "bench position {} did not complete: {:?}",
                    position.id, other
                ),
            };
            // measured by the search over the same interval as the nodes it
            // counted
            let elapsed = result.elapsed;
            Some(PositionReport {
                id: position.id.clone(),
                play: result.best_move,
                score: result.score,
                nodes: result.nodes,
                quiescence_nodes: engine.quiescence_nodes(),
                tt_cutoffs: engine.ghi().score_cutoffs,
                tt_stores: engine.ghi().stores,
                tainted_stores: engine.ghi().tainted_stores,
                tainted_cutoffs: engine.ghi().tainted_score_cutoffs,
                refused_cutoffs: engine.ghi().refused_cutoffs,
                skipped_stores: engine.ghi().skipped_stores,
                signatures: engine.signatures(),
                elapsed,
            })
        })
        .collect::<Option<Vec<PositionReport>>>()?;
    Some(Report {
        depth,
        table_bytes,
        config,
        positions,
    })
}

fn share(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        100.0 * part as f64 / whole as f64
    }
}

/// The report as the command prints it: a header naming the settings, a row
/// a position, a total, the two signature audit lines when the run was
/// audited, and last the one line the match tools read, `<nodes> nodes
/// <nps> nps` and nothing else.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "bench depth {} hash {}MB positions {} taint {}",
            self.depth,
            self.table_bytes / (1024 * 1024),
            self.positions.len(),
            self.config.taint_word()
        )?;
        // the name column is as wide as the widest name, so a suite of
        // fen-named positions still lines up
        let width = self
            .positions
            .iter()
            .map(|p| p.id.len())
            .max()
            .unwrap_or(0)
            .max("position".len());
        writeln!(
            f,
            "{:<width$} {:>5} {:>6} {:>10} {:>7} {:>9} {:>9} {:>8} {:>6} {:>7} {:>7} {:>6} {:>10}",
            "position",
            "move",
            "score",
            "nodes",
            "qs%",
            "cutoffs",
            "stores",
            "tainted",
            "tcuts",
            "refused",
            "skipped",
            "ms",
            "nps"
        )?;
        // the total row has no move or score, so those two arrive as text
        let row = |f: &mut fmt::Formatter<'_>,
                   name: &str,
                   play: &str,
                   score: &str,
                   nodes: u64,
                   quiescence: u64,
                   cutoffs: u64,
                   stores: u64,
                   tainted: u64,
                   tainted_cutoffs: u64,
                   refused_cutoffs: u64,
                   skipped_stores: u64,
                   elapsed: Duration| {
            writeln!(
                f,
                "{:<width$} {:>5} {:>6} {:>10} {:>6.1}% {:>9} {:>9} {:>8} {:>6} {:>7} {:>7} {:>6} {:>10}",
                name,
                play,
                score,
                nodes,
                share(quiescence, nodes),
                cutoffs,
                stores,
                tainted,
                tainted_cutoffs,
                refused_cutoffs,
                skipped_stores,
                elapsed.as_millis(),
                nps(nodes, elapsed)
            )
        };
        for p in &self.positions {
            row(
                f,
                &p.id,
                &p.play.to_string(),
                &p.score.to_string(),
                p.nodes,
                p.quiescence_nodes,
                p.tt_cutoffs,
                p.tt_stores,
                p.tainted_stores,
                p.tainted_cutoffs,
                p.refused_cutoffs,
                p.skipped_stores,
                p.elapsed,
            )?;
        }
        let sum = |field: fn(&PositionReport) -> u64| self.positions.iter().map(field).sum::<u64>();
        row(
            f,
            "total",
            "",
            "",
            self.nodes(),
            sum(|p| p.quiescence_nodes),
            sum(|p| p.tt_cutoffs),
            sum(|p| p.tt_stores),
            sum(|p| p.tainted_stores),
            sum(|p| p.tainted_cutoffs),
            sum(|p| p.refused_cutoffs),
            sum(|p| p.skipped_stores),
            self.elapsed(),
        )?;
        // only when the run was audited, and above the last line because the
        // last line is the one the match tools read
        if let Some(counted) = self.signatures() {
            // the thirty two bit observation is a zero at this scale whether
            // or not the instrument works, so the narrow widths are what say
            // it does
            writeln!(
                f,
                "signature audit: probes {}, hits {}, comparisons {}, \
                 false accepts {} ({:.3} expected), \
                 false accept cutoffs {}, aliased evictions {}",
                counted.probes,
                counted.hits,
                counted.comparisons,
                counted.false_accepts,
                counted.expected_false_accepts(),
                counted.false_accept_cutoffs,
                counted.aliased_evictions,
            )?;
            writeln!(f, "narrow signature: {}", narrow_widths(&counted))?;
        }
        write!(f, "{} nodes {} nps", self.nodes(), self.nps())
    }
}

/// The narrow accepts as one clause a width: `16 bit accepts 89 (92.965
/// expected), 24 bit accepts 0 (0.362 expected)` and so on. The widths are
/// cumulative, so each figure is read against the expectation beside it and
/// never added to another's.
fn narrow_widths(counted: &SignatureCounters) -> String {
    counted
        .narrow()
        .map(|(width, accepts, expected)| {
            format!("{width} bit accepts {accepts} ({expected:.3} expected)")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transposition::NARROW_WIDTHS;
    use pretty_assertions::assert_eq;

    #[test]
    fn an_epd_line_yields_its_fen_and_id() {
        // the fields may be separated by any whitespace and the id need not
        // be the first operation
        for line in [
            "4k3/8/8/8/8/8/8/4K3 w - - id \"bare kings\";",
            "4k3/8/8/8/8/8/8/4K3\tw  - -  c0 \"a note\"; id \"bare kings\";",
        ] {
            let parsed = parse_epd(line);
            assert_eq!(parsed.len(), 1, "{line:?}");
            assert_eq!(parsed[0].id, "bare kings", "{line:?}");
            assert_eq!(parsed[0].fen, "4k3/8/8/8/8/8/8/4K3 w - - 0 1", "{line:?}");
        }
    }

    /// The bench reads none of these, so nothing else here would notice if
    /// the reader started dropping them.
    #[test]
    fn every_operation_on_a_line_is_kept() {
        let parsed =
            parse_epd("8/8/8/8/8/8/8/K1k5 w - - id \"two ops\"; bm a1b1 a1a2; c0 \"a comment\";");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "two ops");
        assert_eq!(
            parsed[0].operations.get("bm"),
            Some(&"a1b1 a1a2".to_string())
        );
        assert_eq!(
            parsed[0].operations.get("c0"),
            Some(&"a comment".to_string())
        );
    }

    #[test]
    fn an_operation_with_no_operands_is_dropped() {
        let parsed = parse_epd("8/8/8/8/8/8/8/K1k5 w - - id \"lonely\"; hmvc;");
        assert_eq!(parsed[0].id, "lonely");
        assert!(!parsed[0].operations.contains_key("hmvc"));
    }

    #[test]
    fn a_line_without_an_id_is_named_by_its_fen() {
        let parsed = parse_epd("# a comment\n\n4k3/8/8/8/8/8/8/4K3 w - -\n");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "4k3/8/8/8/8/8/8/4K3 w - - 0 1");
    }

    #[test]
    fn the_suite_parses_into_distinct_legal_positions() {
        let positions = positions();
        assert!(positions.len() >= 16, "{} positions", positions.len());
        let mut ids: Vec<&str> = positions.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), positions.len(), "an id repeats");
        for position in &positions {
            assert!(!position.id.is_empty());
            Board::from_fen(&position.fen).unwrap_or_else(|e| panic!("{}: {}", position.id, e));
        }
    }

    #[test]
    fn the_report_ends_with_the_line_the_match_tools_read() {
        let suite = parse_epd("4k3/8/8/8/8/8/8/4K3 w - - id \"bare kings\";");
        let report = run_suite(&suite, 2, 1 << 20, SearchConfig::default());
        let text = report.to_string();
        let last = text.lines().last().unwrap();
        let words: Vec<&str> = last.split(' ').collect();
        assert_eq!(words.len(), 4, "{}", last);
        assert_eq!(words[1], "nodes");
        assert_eq!(words[3], "nps");
        assert_eq!(words[0].parse::<u64>().unwrap(), report.nodes());
        assert_eq!(words[2].parse::<u64>().unwrap(), report.nps());
        assert!(text.starts_with("bench depth 2 hash 1MB positions 1 taint rule50\n"));
    }

    #[test]
    fn the_report_says_what_each_search_chose_and_refused() {
        // two bench outputs diffed say whether the root moved, not only the
        // tree, and the same suite searched trusting tainted scores states
        // that policy in its header and refuses nothing
        let suite = parse_epd("8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - id \"rook and pawns\";");
        let refusing = run_suite(&suite, 7, 1 << 20, SearchConfig::reference());
        let text = refusing.to_string();
        let position = &refusing.positions[0];
        assert!(position.refused_cutoffs > 0, "{}", text);
        // the columns are read by position, so a value printed in the wrong
        // column fails rather than being found elsewhere on the line
        let columns = |line: &str| {
            line.split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let header = columns(text.lines().nth(1).unwrap());
        let row = columns(
            text.lines()
                .find(|line| line.starts_with("rook and pawns"))
                .unwrap(),
        );
        let column = |name: &str| {
            let at = header
                .iter()
                .position(|h| h == name)
                .unwrap_or_else(|| panic!("no {name} column"));
            // the name is three words; the header has one word a column
            row[at + 2].clone()
        };
        assert_eq!(column("move"), position.play.to_string());
        assert_eq!(column("score"), position.score.to_string());
        assert_eq!(column("refused"), position.refused_cutoffs.to_string());

        let trusting = SearchConfig::with_taint("trust").expect("trust is a policy");
        let trusted = run_suite(&suite, 7, 1 << 20, trusting);
        assert!(
            trusted
                .to_string()
                .starts_with("bench depth 7 hash 1MB positions 1 taint trust\n")
        );
        assert_eq!(trusted.positions[0].refused_cutoffs, 0);
        assert!(trusted.positions[0].tainted_cutoffs > 0);
    }

    #[test]
    fn the_report_counts_every_position() {
        let suite = parse_epd(
            "4k3/8/8/8/8/8/8/4K3 w - - id \"bare kings\";\n\
             rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - id \"start\";",
        );
        let report = run_suite(&suite, 3, 1 << 16, SearchConfig::default());
        assert_eq!(report.positions.len(), 2);
        assert!(report.positions.iter().all(|p| p.nodes > 0));
        assert!(report.positions[1].quiescence_nodes <= report.positions[1].nodes);
        assert_eq!(
            report.nodes(),
            report.positions.iter().map(|p| p.nodes).sum::<u64>()
        );
    }

    #[test]
    fn a_depth_of_zero_is_searched_as_one() {
        let suite = parse_epd("4k3/8/8/8/8/8/8/4K3 w - - id \"bare kings\";");
        let report = run_suite(&suite, 0, 1 << 20, SearchConfig::default());
        assert_eq!(report.depth, 1);
        assert!(report.positions[0].nodes > 0);
    }

    /// Two positions: enough for an audited run to count something, few
    /// enough to search twice inside a test.
    fn small_suite() -> Vec<Position> {
        parse_epd(
            "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - id \"sharp\";\n\
             r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - id \"kiwipete\";",
        )
    }

    /// The shadow keys are allocated only when the bench asks for them, so
    /// an ordinary run prints what it always printed.
    #[test]
    fn a_run_that_was_not_audited_keeps_no_keys() {
        for config in [SearchConfig::default(), SearchConfig::reference()] {
            let report = run_suite(&small_suite(), 3, 1 << 20, config);
            assert!(report.signatures().is_none());
            assert!(report.positions.iter().all(|p| p.signatures.is_none()));
            assert!(!report.to_string().contains("signature audit"));
        }
    }

    /// The audit counts and does nothing else, so the tree is the tree the
    /// unaudited run searched.
    #[test]
    fn an_audited_run_searches_the_same_tree() {
        let suite = small_suite();
        let plain = run_suite(&suite, 4, 4 << 20, SearchConfig::default());
        let audited =
            run_audited_suite(&suite, 4, 4 << 20, SearchConfig::default()).expect("the keys");
        assert_eq!(plain.nodes(), audited.nodes());
        let played = |report: &Report| {
            report
                .positions
                .iter()
                .map(|p| (p.play, p.score, p.nodes))
                .collect::<Vec<_>>()
        };
        assert_eq!(played(&plain), played(&audited));
    }

    /// The summary block, with each counter inside the one it is a part of,
    /// above the last line the match tools read.
    ///
    /// The false accepts are pinned at zero rather than bounded. The
    /// expectation at this scale is about a ten thousandth of one, so a
    /// count above zero is the instrument reading its own keys wrongly, not
    /// the search finding a collision. A shadow key dropped on store, or
    /// index arithmetic a slot off, makes every probe read as foreign and
    /// would pass a bound.
    #[test]
    fn an_audited_run_prints_a_summary_that_holds_together() {
        let report =
            run_audited_suite(&small_suite(), 4, 4 << 20, SearchConfig::default()).expect("keys");
        let counted = report.signatures().expect("an audited run counted");
        assert!(counted.probes > 0, "nothing was probed");
        assert!(counted.hits <= counted.probes);
        assert_eq!(
            counted.false_accepts, 0,
            "a false accept at this scale is the audit reading its own keys wrongly"
        );
        assert!(counted.false_accept_cutoffs <= counted.false_accepts);
        assert!(counted.comparisons > 0, "nothing was compared");
        // the widths are cumulative, so the counts fall as the width rises.
        // A suite this small usually counts nothing at any width; the
        // property is checked against real counts by
        // `the_counts_fall_as_the_width_rises`
        assert!(
            counted
                .narrow_accepts
                .windows(2)
                .all(|widths| widths[0] >= widths[1]),
            "a wider signature accepted more than a narrower one: {:?}",
            counted.narrow_accepts
        );
        let text = report.to_string();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[lines.len() - 3],
            format!(
                "signature audit: probes {}, hits {}, comparisons {}, \
                 false accepts {} ({:.3} expected), \
                 false accept cutoffs {}, aliased evictions {}",
                counted.probes,
                counted.hits,
                counted.comparisons,
                counted.false_accepts,
                counted.expected_false_accepts(),
                counted.false_accept_cutoffs,
                counted.aliased_evictions,
            )
        );
        let clauses: Vec<String> = NARROW_WIDTHS
            .into_iter()
            .zip(counted.narrow_accepts)
            .map(|(width, accepts)| {
                format!(
                    "{width} bit accepts {accepts} ({:.3} expected)",
                    counted.expected_narrow_accepts(width)
                )
            })
            .collect();
        assert_eq!(
            lines[lines.len() - 2],
            format!("narrow signature: {}", clauses.join(", "))
        );
        assert!(lines[lines.len() - 1].ends_with(" nps"));
    }

    /// The expectations are the comparisons over two to the width, which is
    /// arithmetic rather than a measurement and is pinned as such.
    #[test]
    fn an_expectation_is_the_comparisons_over_two_to_the_width() {
        let counted = SignatureCounters {
            comparisons: 1 << 32,
            ..Default::default()
        };
        assert_eq!(counted.expected_false_accepts(), 1.0);
        // less the chance the whole slice agreed as well, which the wide
        // signature takes for itself
        assert_eq!(counted.expected_narrow_accepts(16), 65_536.0 - 1.0);
        assert_eq!(counted.expected_narrow_accepts(24), 256.0 - 1.0);
        assert_eq!(counted.expected_narrow_accepts(28), 16.0 - 1.0);
        assert_eq!(
            counted
                .narrow()
                .map(|(width, _, _)| width)
                .collect::<Vec<_>>(),
            NARROW_WIDTHS.to_vec(),
            "the widths are reported in the order they are declared"
        );
        let none = SignatureCounters::default();
        assert_eq!(none.expected_false_accepts(), 0.0);
        assert!(none.narrow().all(|(_, _, expected)| expected == 0.0));
    }

    /// The instrument is as deterministic as the search it watches.
    #[test]
    fn two_audited_runs_count_the_same() {
        let suite = small_suite();
        let first = run_audited_suite(&suite, 4, 4 << 20, SearchConfig::default()).expect("keys");
        let second = run_audited_suite(&suite, 4, 4 << 20, SearchConfig::default()).expect("keys");
        assert_eq!(first.signatures(), second.signatures());
    }

    /// The node count is exact and the same on any machine, and it moves
    /// whenever move ordering, quiescence, the transposition table or any
    /// pruning changes, including the changes that leave the move played
    /// untouched. A deliberate change to the search is expected to move
    /// these: update them in the same commit, from `arche bench`, so the
    /// diff states how much of each tree the engine now looks at.
    #[test]
    fn node_counts_have_not_moved() {
        let report = run_suite(&positions(), DEPTH, TABLE_BYTES, SearchConfig::default());
        let counted: Vec<(&str, u64)> = report
            .positions
            .iter()
            .map(|p| (p.id.as_str(), p.nodes))
            .collect();
        assert_eq!(
            counted,
            vec![
                ("start", 438_221),
                ("italian", 668_066),
                ("ruy lopez", 549_138),
                ("kiwipete", 1_201_992),
                ("perft 4", 396_852),
                ("promotions", 217_976),
                ("middlegame", 516_335),
                ("sharp middlegame", 710_175),
                ("bratko kopec 1", 7_334_664),
                ("wac 4", 38_242_037),
                ("rook and pawns", 84_226),
                ("tarrasch", 191_107),
                ("lucena", 60_887),
                ("philidor", 191_053),
                ("minor endgame", 110_997),
                ("queen endgame", 303_124),
                ("king and pawn", 11_658),
                ("trebuchet", 7_946),
            ]
        );
    }

    /// The reference search's counts, pinned apart from the default's: a
    /// change that moves both touched the search the two share, and one that
    /// moves the default's alone is a shortcut. Pinned shallower than the
    /// bench, which is cheaper and coarser (a twentieth of the time, with a
    /// table under half full, so a change to what the table keeps shows here
    /// less). The pin stayed at this depth when the bench's was raised from
    /// seven to nine.
    #[test]
    fn reference_node_counts_have_not_moved() {
        const REFERENCE_DEPTH: u8 = 5;
        let report = run_suite(
            &positions(),
            REFERENCE_DEPTH,
            TABLE_BYTES,
            SearchConfig::reference(),
        );
        let counted: Vec<(&str, u64)> = report
            .positions
            .iter()
            .map(|p| (p.id.as_str(), p.nodes))
            .collect();
        assert_eq!(
            counted,
            vec![
                ("start", 33_729),
                ("italian", 120_260),
                ("ruy lopez", 118_515),
                ("kiwipete", 224_244),
                ("perft 4", 255_435),
                ("promotions", 95_210),
                ("middlegame", 164_325),
                ("sharp middlegame", 241_920),
                ("bratko kopec 1", 52_545),
                ("wac 4", 89_428),
                ("rook and pawns", 22_615),
                ("tarrasch", 48_576),
                ("lucena", 24_290),
                ("philidor", 29_763),
                ("minor endgame", 30_627),
                ("queen endgame", 175_117),
                ("king and pawn", 1_601),
                ("trebuchet", 968),
            ]
        );
    }
}
