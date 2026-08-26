//! The bench: a fixed suite of positions, each searched to a fixed depth with
//! a fixed table, and what the searches counted.
//!
//! The search is deterministic, so the node count is exact and says the same
//! thing on any machine: it moves if and only if the tree searched moves.
//! That makes it the signature of a search change, which a commit that makes
//! one states in its message. The speed is the other half, and it is what
//! the match tools scale their time controls by, so the clock runs over the
//! search alone and not over allocating the table.

use crate::board::Board;
use crate::engine::{AlphaBeta, Engine, SearchConfig, SearchOutcome, SearchParameters};
use crate::misc::Score;
use crate::play::Play;
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

/// The depth every position is searched to.
pub const DEPTH: u8 = 7;

/// The table every position is searched with. The tree moves with the
/// table, so this is part of what the numbers mean.
pub const TABLE_BYTES: usize = 16 * 1024 * 1024;

/// The suite, as epd: a four field fen and an id operation a line.
const SUITE: &str = include_str!("../bench.epd");

/// A position of the suite, as a full fen and the name the report gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    pub id: String,
    pub fen: String,
    /// The operations the line carried, by opcode. `id` is lifted out
    /// above because every reader wants it and a line without one is named
    /// by its fen; the rest are left here for whichever reader knows what
    /// they mean. The bench reads none of them, and the tactical suite
    /// reads `bm`.
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
            // what is left is the operations, each an opcode and its
            // operands and each ended by a semicolon. An operation with no
            // operands is dropped: every opcode read here takes them, and
            // one that does not would arrive as an empty string that no
            // reader could tell from a missing one. The quotes around an id
            // are epd syntax rather than part of the name, so they come off
            // here and nothing below has to know they were ever there
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
    /// The move the search chose, and the score it gave it, from the side
    /// to move. A policy that changes either has changed the answer and not
    /// only the tree that found it.
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
    /// Cutoffs taken from a draw tainted score, which the configuration may
    /// refuse, and the default does, so this stays at zero while it does.
    pub tainted_cutoffs: u64,
    /// Cutoffs refused for their taint and searched instead, which is what
    /// refusing costs; zero under a configuration that trusts them. Under
    /// the rule50 policy this counts its horizon refusals instead.
    pub refused_cutoffs: u64,
    /// Tainted results not stored, under the policy that keeps only clean
    /// scores; zero under every other.
    pub skipped_stores: u64,
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
}

/// Counted in microseconds, so a position searched in well under a
/// millisecond still gets a rate rather than its count times a thousand, and
/// measured as at least one so the rate stays finite and the arithmetic whole.
fn nps(nodes: u64, elapsed: Duration) -> u64 {
    (nodes as u128 * 1_000_000 / elapsed.as_micros().max(1)) as u64
}

/// Runs a suite under the settings given. Each position gets a fresh engine
/// and a fresh table, allocated before its clock starts, and is deepened to
/// the depth the way a game would be, so the table is warm from each
/// iteration to the next. A depth of zero runs no iteration and counts
/// nothing, which is no bench at all, so it is searched as one. The bench
/// the command prints runs the default configuration, the one the engine
/// plays with; the reference is run the same way to pin its own counts.
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
                .unwrap_or_else(|e| panic!("bench position {} does not parse: {}", position.id, e));
            let mut engine = AlphaBeta::with_config(board, table_bytes, config);
            let outcome =
                engine.iterative_deepening_search(SearchParameters::to_depth(depth), |_, _, _| {});
            let result = match outcome {
                SearchOutcome::Complete(result) => result,
                other => panic!(
                    "bench position {} did not complete: {:?}",
                    position.id, other
                ),
            };
            // the search says how long it took, measured over the same
            // interval as the nodes it counted
            let elapsed = result.elapsed;
            PositionReport {
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
                elapsed,
            }
        })
        .collect();
    Report {
        depth,
        table_bytes,
        config,
        positions,
    }
}

fn share(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        100.0 * part as f64 / whole as f64
    }
}

/// The report as the command prints it: a header naming the settings, a row
/// a position, a total, and last the one line the match tools read, which is
/// `<nodes> nodes <nps> nps` and nothing else.
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
        write!(f, "{} nodes {} nps", self.nodes(), self.nps())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            // the fen and the id, which is what this one is about; the
            // operations differ between the two lines and have a test of
            // their own below
            assert_eq!(parsed[0].id, "bare kings", "{line:?}");
            assert_eq!(parsed[0].fen, "4k3/8/8/8/8/8/8/4K3 w - - 0 1", "{line:?}");
        }
    }

    /// The bench reads none of these, so nothing else would notice if the
    /// reader started dropping them. The tactical suite reads bm out of the
    /// same map.
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
        // the move and score are what a policy changes when it changes
        // anything that matters, and the refusals are what the default
        // policy costs, so all three are columns of the report: two bench
        // outputs diffed say whether the root moved, not only the tree.
        // The same suite searched trusting tainted scores states that
        // policy in its header, refuses nothing, and may choose otherwise
        let suite = parse_epd("8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - id \"rook and pawns\";");
        let refusing = run_suite(&suite, 7, 1 << 20, SearchConfig::reference());
        let text = refusing.to_string();
        let position = &refusing.positions[0];
        assert!(position.refused_cutoffs > 0, "{}", text);
        // the columns are read by position, the way the header names them,
        // so a value printed in the wrong column fails rather than being
        // found somewhere else on the line
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
        // a depth of zero runs no iteration and finds nothing, which is not
        // a bench: there is nothing to count below one
        let suite = parse_epd("4k3/8/8/8/8/8/8/4K3 w - - id \"bare kings\";");
        let report = run_suite(&suite, 0, 1 << 20, SearchConfig::default());
        assert_eq!(report.depth, 1);
        assert!(report.positions[0].nodes > 0);
    }

    /// The search is deterministic, so how many nodes the bench visits is an
    /// exact figure rather than a timing, and it says the same thing on any
    /// machine. It moves whenever move ordering, quiescence, the transposition
    /// table or any pruning changes, including the many such changes that
    /// leave the move finally played untouched, which is what makes it worth
    /// pinning.
    ///
    /// A deliberate change to the search is expected to move these. Update
    /// them in the same commit, from `arche bench`: the diff is then a
    /// statement of how much less, or more, of the tree the engine now looks
    /// at, position by position.
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
                ("start", 1_006_821),
                ("italian", 3_843_640),
                ("ruy lopez", 1_380_882),
                ("kiwipete", 854_315),
                ("perft 4", 267_122),
                ("promotions", 174_760),
                ("middlegame", 609_385),
                ("sharp middlegame", 5_427_753),
                ("bratko kopec 1", 602_178),
                ("wac 4", 2_085_574),
                ("rook and pawns", 152_774),
                ("tarrasch", 229_988),
                ("lucena", 499_790),
                ("philidor", 777_641),
                ("minor endgame", 330_155),
                ("queen endgame", 1_928_594),
                ("king and pawn", 7_227),
                ("trebuchet", 3_504),
            ]
        );
    }

    /// The reference search's counts, pinned apart from the default's. The
    /// two are separate trees now that the default prunes: a change that
    /// moves both touched the search they share, move ordering or the
    /// table, say, and one that moves the default's alone is a shortcut,
    /// which is what this pin standing still through one says. Pinned
    /// shallower than the bench, which is cheaper and coarser: a twentieth
    /// of the time, with a table under half full, so a change to what the
    /// table keeps shows here less than it does at the bench's depth. The
    /// pin stays where it is when that depth is raised.
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
                ("start", 25_191),
                ("italian", 209_297),
                ("ruy lopez", 137_865),
                ("kiwipete", 213_513),
                ("perft 4", 202_962),
                ("promotions", 112_313),
                ("middlegame", 199_843),
                ("sharp middlegame", 518_385),
                ("bratko kopec 1", 57_952),
                ("wac 4", 99_357),
                ("rook and pawns", 21_869),
                ("tarrasch", 26_797),
                ("lucena", 23_675),
                ("philidor", 41_634),
                ("minor endgame", 21_922),
                ("queen endgame", 112_925),
                ("king and pawn", 1_983),
                ("trebuchet", 710),
            ]
        );
    }
}
