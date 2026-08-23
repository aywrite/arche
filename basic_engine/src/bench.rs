//! The bench: a fixed suite of positions, each searched to a fixed depth with
//! a fixed table, and what the searches counted.
//!
//! The search is deterministic, so the node count is exact and says the same
//! thing on any machine: it moves if and only if the tree searched moves.
//! That makes it the signature of a search change, which a commit that makes
//! one states in its message. The speed is the other half, and it is what
//! the match tools scale their time controls by, so the clock runs over the
//! search alone and not over allocating the table.

use crate::Game;
use crate::board::Board;
use crate::engine::{AlphaBeta, Engine, SearchOutcome, SearchParameters};
use std::fmt;
use std::time::{Duration, Instant};

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
fn parse_epd(text: &str) -> Vec<Position> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut words = line.split_whitespace();
            let fen: Vec<&str> = words.by_ref().take(4).collect();
            let fen = format!("{} 0 1", fen.join(" "));
            // what is left is the operations, each ended by a semicolon
            let operations = words.collect::<Vec<&str>>().join(" ");
            let id = operations
                .split(';')
                .map(str::trim)
                .find_map(|operation| operation.strip_prefix("id "))
                .map(|name| name.trim().trim_matches('"').to_string())
                .unwrap_or_else(|| fen.clone());
            Position { id, fen }
        })
        .collect()
}

/// What one position's search counted.
#[derive(Debug, Clone)]
pub struct PositionReport {
    pub id: String,
    pub nodes: u64,
    /// The nodes quiescence visited, a part of nodes.
    pub quiescence_nodes: u64,
    /// Probes that cut the search off with a stored score.
    pub tt_cutoffs: u64,
    /// Entries stored in total.
    pub tt_stores: u64,
    /// Entries stored with a draw tainted score, a part of the stores.
    pub tainted_stores: u64,
    /// Cutoffs taken from a draw tainted score, which the search refuses, so
    /// this stays at zero while it does.
    pub tainted_cutoffs: u64,
    /// The search alone, not the table's allocation.
    pub elapsed: Duration,
}

/// The whole bench: the settings it ran with and what each position counted.
#[derive(Debug, Clone)]
pub struct Report {
    pub depth: u8,
    pub table_bytes: usize,
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
/// nothing, which is no bench at all, so it is searched as one.
pub fn run_suite(positions: &[Position], depth: u8, table_bytes: usize) -> Report {
    let depth = depth.max(1);
    let positions = positions
        .iter()
        .map(|position| {
            let board = Board::from_fen(&position.fen)
                .unwrap_or_else(|e| panic!("bench position {} does not parse: {}", position.id, e));
            let mut engine = AlphaBeta::with_table_bytes(board, table_bytes);
            let start = Instant::now();
            let outcome = engine
                .iterative_deepening_search(SearchParameters::new_with_depth(depth), |_, _, _| {});
            let elapsed = start.elapsed();
            let nodes = match outcome {
                SearchOutcome::Complete(result) => result.nodes,
                other => panic!(
                    "bench position {} did not complete: {:?}",
                    position.id, other
                ),
            };
            PositionReport {
                id: position.id.clone(),
                nodes,
                quiescence_nodes: engine.quiescence_nodes(),
                tt_cutoffs: engine.ghi.score_cutoffs,
                tt_stores: engine.ghi.stores,
                tainted_stores: engine.ghi.tainted_stores,
                tainted_cutoffs: engine.ghi.tainted_score_cutoffs,
                elapsed,
            }
        })
        .collect();
    Report {
        depth,
        table_bytes,
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
            "bench depth {} hash {}MB positions {}",
            self.depth,
            self.table_bytes / (1024 * 1024),
            self.positions.len()
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
            "{:<width$} {:>10} {:>7} {:>9} {:>9} {:>8} {:>6} {:>6} {:>10}",
            "position", "nodes", "qs%", "cutoffs", "stores", "tainted", "tcuts", "ms", "nps"
        )?;
        let row = |f: &mut fmt::Formatter<'_>,
                   name: &str,
                   nodes: u64,
                   quiescence: u64,
                   cutoffs: u64,
                   stores: u64,
                   tainted: u64,
                   tainted_cutoffs: u64,
                   elapsed: Duration| {
            writeln!(
                f,
                "{:<width$} {:>10} {:>6.1}% {:>9} {:>9} {:>8} {:>6} {:>6} {:>10}",
                name,
                nodes,
                share(quiescence, nodes),
                cutoffs,
                stores,
                tainted,
                tainted_cutoffs,
                elapsed.as_millis(),
                nps(nodes, elapsed)
            )
        };
        for p in &self.positions {
            row(
                f,
                &p.id,
                p.nodes,
                p.quiescence_nodes,
                p.tt_cutoffs,
                p.tt_stores,
                p.tainted_stores,
                p.tainted_cutoffs,
                p.elapsed,
            )?;
        }
        let sum = |field: fn(&PositionReport) -> u64| self.positions.iter().map(field).sum::<u64>();
        row(
            f,
            "total",
            self.nodes(),
            sum(|p| p.quiescence_nodes),
            sum(|p| p.tt_cutoffs),
            sum(|p| p.tt_stores),
            sum(|p| p.tainted_stores),
            sum(|p| p.tainted_cutoffs),
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
        let parsed = parse_epd("4k3/8/8/8/8/8/8/4K3 w - - id \"bare kings\";");
        assert_eq!(
            parsed,
            vec![Position {
                id: "bare kings".to_string(),
                fen: "4k3/8/8/8/8/8/8/4K3 w - - 0 1".to_string(),
            }]
        );
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
        let report = run_suite(&suite, 2, 1 << 20);
        let text = report.to_string();
        let last = text.lines().last().unwrap();
        let words: Vec<&str> = last.split(' ').collect();
        assert_eq!(words.len(), 4, "{}", last);
        assert_eq!(words[1], "nodes");
        assert_eq!(words[3], "nps");
        assert_eq!(words[0].parse::<u64>().unwrap(), report.nodes());
        assert_eq!(words[2].parse::<u64>().unwrap(), report.nps());
        assert!(text.starts_with("bench depth 2 hash 1MB positions 1\n"));
    }

    #[test]
    fn the_report_counts_every_position() {
        let suite = parse_epd(
            "4k3/8/8/8/8/8/8/4K3 w - - id \"bare kings\";\n\
             rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - id \"start\";",
        );
        let report = run_suite(&suite, 3, 1 << 16);
        assert_eq!(report.positions.len(), 2);
        assert!(report.positions.iter().all(|p| p.nodes > 0));
        assert!(report.positions[1].quiescence_nodes <= report.positions[1].nodes);
        assert_eq!(
            report.nodes(),
            report.positions.iter().map(|p| p.nodes).sum::<u64>()
        );
    }

    #[test]
    fn fields_may_be_separated_by_any_whitespace_and_the_id_need_not_come_first() {
        let parsed = parse_epd("4k3/8/8/8/8/8/8/4K3\tw  - -  c0 \"a note\"; id \"bare kings\";");
        assert_eq!(parsed[0].id, "bare kings");
        assert_eq!(parsed[0].fen, "4k3/8/8/8/8/8/8/4K3 w - - 0 1");
    }

    #[test]
    fn a_depth_of_zero_is_searched_as_one() {
        // a depth of zero runs no iteration and finds nothing, which is not
        // a bench: there is nothing to count below one
        let suite = parse_epd("4k3/8/8/8/8/8/8/4K3 w - - id \"bare kings\";");
        let report = run_suite(&suite, 0, 1 << 20);
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
        let report = run_suite(&positions(), DEPTH, TABLE_BYTES);
        let counted: Vec<(&str, u64)> = report
            .positions
            .iter()
            .map(|p| (p.id.as_str(), p.nodes))
            .collect();
        assert_eq!(
            counted,
            vec![
                ("start", 1_195_758),
                ("italian", 4_426_337),
                ("ruy lopez", 3_099_801),
                ("kiwipete", 4_641_935),
                ("perft 4", 4_069_374),
                ("promotions", 2_617_360),
                ("middlegame", 4_007_180),
                ("sharp middlegame", 11_195_520),
                ("bratko kopec 1", 718_574),
                ("wac 4", 2_302_115),
                ("rook and pawns", 173_755),
                ("tarrasch", 434_137),
                ("lucena", 523_941),
                ("philidor", 913_576),
                ("minor endgame", 142_796),
                ("queen endgame", 2_373_132),
                ("king and pawn", 9_196),
                ("trebuchet", 3_264),
            ]
        );
    }
}
