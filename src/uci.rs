use crate::time_control::TimeControl;
use basic_engine::Color;
use basic_engine::Engine;
use basic_engine::SearchOutcome;
use basic_engine::SearchParameters;
use basic_engine::{PvLine, SearchResult};
use regex::Regex;
use std::io::{BufRead, Stdout, Write};
use std::sync::LazyLock;
use std::time::Duration;

const START_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

static WTIME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"wtime (-?\d+)").unwrap());
static BTIME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"btime (-?\d+)").unwrap());
static WINC_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"winc (-?\d+)").unwrap());
static BINC_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"binc (-?\d+)").unwrap());
static MOVES_TO_GO_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"movestogo (\d+)").unwrap());
static MOVE_TIME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"movetime (\d+)").unwrap());
static DEPTH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"depth (\d+)").unwrap());
static NODES_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"nodes (\d+)").unwrap());
static PERFT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"perft (\d+)").unwrap());

/// Reads the value that follows a keyword. A clock below zero, which the match
/// tools send once their time margin has been eaten into, reads as an empty
/// one. Otherwise the value is digits, so the only way it can fail to parse is
/// by being too large to hold, in which case use the largest value we can.
/// Discarding either would read as the keyword having been absent, which for
/// a clock means searching without a limit.
fn capture(re: &Regex, line: &str) -> Option<u64> {
    let digits = re.captures(line)?.get(1)?.as_str();
    if digits.starts_with('-') {
        return Some(0);
    }
    Some(digits.parse().unwrap_or(u64::MAX))
}

fn time_control_from(line: &str, color: Color) -> TimeControl {
    TimeControl {
        time: match color {
            Color::White => capture(&WTIME_RE, line),
            Color::Black => capture(&BTIME_RE, line),
        },
        increment: match color {
            Color::White => capture(&WINC_RE, line),
            Color::Black => capture(&BINC_RE, line),
        },
        moves_to_go: capture(&MOVES_TO_GO_RE, line),
        move_time: capture(&MOVE_TIME, line),
        infinite: line.contains("infinite"),
    }
}

pub struct UCI<T: Engine, W: Write> {
    author: String,
    name: String,
    version: String,

    engine: T,
    out: W,
}

impl<T: Engine> UCI<T, Stdout> {
    pub fn new_with_engine(engine: T) -> Self {
        Self::with_output(engine, std::io::stdout())
    }
}

impl<T: Engine, W: Write> UCI<T, W> {
    /// Separate from new_with_engine so that what is said can be captured.
    fn with_output(engine: T, out: W) -> Self {
        Self {
            author: env!("CARGO_PKG_AUTHORS").to_string(),
            name: env!("CARGO_PKG_NAME").to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            engine,
            out,
        }
    }

    pub fn read_loop(&mut self) {
        self.run(std::io::stdin().lock());
    }

    /// Writes one line to the interface. A failed write means the interface
    /// itself is gone, which leaves no one to tell, so the error is dropped.
    fn say(&mut self, line: std::fmt::Arguments) {
        let _ = writeln!(self.out, "{}", line);
    }

    /// Tells the interface about anything we could not act on. A bad line is
    /// the interface's problem to fix, so say so and carry on reading.
    fn report(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.say(format_args!("info string {}", error));
        }
    }

    /// Handles input until it is exhausted or `quit` arrives. Separate from
    /// read_loop so that it can be driven without stdin.
    fn run<R: BufRead>(&mut self, input: R) {
        for line in input.lines() {
            match line {
                Ok(line) => {
                    if !self.handle(&line) {
                        return;
                    }
                }
                // there is nothing left to read and no one to tell, so stop
                // rather than sitting in the loop asking again
                Err(error) => {
                    self.say(format_args!("info string could not read input: {}", error));
                    return;
                }
            }
        }
    }

    /// Returns false once the engine has been asked to quit.
    fn handle(&mut self, line: &str) -> bool {
        if line.starts_with("quit") {
            return false;
        }
        if line.starts_with("isready") {
            self.say(format_args!("readyok"));
        } else if line.starts_with("ucinewgame") {
            self.engine.new_game();
            let result = self.parse_position("position startpos");
            self.report(result);
        } else if line.starts_with("uci") {
            // written to the field directly: say borrows all of self, and
            // these lines also read from it
            let _ = writeln!(self.out, "id name {} {}", self.name, self.version);
            let _ = writeln!(self.out, "id author {}", self.author);
            self.say(format_args!("uciok"));
        } else if line.starts_with("position") {
            let result = self.parse_position(line);
            self.report(result);
        } else if line.starts_with("display") {
            self.engine.display_board();
        } else if line.starts_with("go") {
            self.parse_go(line);
        } else if line.starts_with("perft") {
            let depth = perft_depth(line);
            let nodes = self.engine.perft(depth);
            self.say(format_args!(
                "info string perft depth {} nodes {}",
                depth, nodes
            ));
        } else {
            self.say(format_args!("info string unrecognised command: {}", line));
        }
        true
    }

    /// A move that cannot be played leaves the position at the last one that
    /// could be, since the interface is expected to send the whole line again
    /// rather than to carry on from a position we rejected.
    fn parse_position(&mut self, line: &str) -> Result<(), String> {
        let position_string = line.strip_prefix("position").unwrap_or(line).trim();
        let (start, move_list) = match position_string.split_once("moves") {
            Some((s, m)) => (s.trim(), Some(m)),
            None => (position_string, None),
        };
        if start.starts_with("startpos") {
            self.engine.parse_fen(START_FEN)?;
        } else if let Some(fen) = start.strip_prefix("fen") {
            self.engine.parse_fen(fen.trim())?;
        } else {
            return Err(format!("unrecognised position: {}", start));
        }

        if let Some(moves) = move_list {
            for m in moves.split_whitespace() {
                if !self.engine.make_move_str(m.trim()) {
                    return Err(format!("could not play {}", m));
                }
            }
        }
        Ok(())
    }

    fn parse_go(&mut self, line: &str) {
        let mut sp = SearchParameters::new();
        sp.search_duration = time_control_from(line, self.engine.active_color()).budget();
        sp.depth = capture(&DEPTH_RE, line).map(|depth| depth.try_into().unwrap_or(u8::MAX));
        sp.nodes = capture(&NODES_RE, line);

        let start = sp.start_time;
        // the closure writes while the engine is borrowed for the search, so
        // it goes to the writer directly rather than through say
        let out = &mut self.out;
        let outcome = self
            .engine
            .iterative_deepening_search(sp, |depth, result, pv| {
                let _ = writeln!(out, "{}", format_info(depth, result, &pv, start.elapsed()));
            });
        match outcome {
            SearchOutcome::Complete(result) | SearchOutcome::Aborted(Some(result)) => {
                self.say(format_args!("bestmove {}", result.best_move));
            }
            SearchOutcome::GameOver => {
                self.say(format_args!("info string no legal moves identified"));
                // 0000 is the null move, used to report that there is no move
                // to make
                self.say(format_args!("bestmove 0000"));
            }
            SearchOutcome::Aborted(None) => self.say(format_args!("bestmove 0000")),
        }
    }
}

/// The depth asked of a perft command. A bare `perft` counts to depth one,
/// which is what the command did before it took a depth at all.
fn perft_depth(line: &str) -> u8 {
    capture(&PERFT_RE, line)
        .map(|depth| depth.try_into().unwrap_or(u8::MAX))
        .unwrap_or(1)
}

/// One completed depth as a UCI info line. The elapsed time arrives as a
/// parameter rather than being read from a clock here, so that tests can pin
/// the whole line.
fn format_info(depth: u8, result: &SearchResult, pv: &PvLine, elapsed: Duration) -> String {
    let millis = elapsed.as_millis();
    // measure a search faster than a millisecond as one, so the rate stays
    // finite and the arithmetic stays whole
    let nps = (result.nodes as u128 * 1000 / millis.max(1)) as u64;
    match result.checkmate_in() {
        Some(mate_in) => format!(
            "info depth {} seldepth {} nodes {} time {} nps {} score mate {} pv {}",
            depth, result.selective_depth, result.nodes, millis, nps, mate_in, pv
        ),
        None => format!(
            "info depth {} seldepth {} nodes {} time {} nps {} score cp {} pv {}",
            depth, result.selective_depth, result.nodes, millis, nps, result.score, pv
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use basic_engine::{AlphaBeta, Board};
    use std::io::Cursor;

    /// A table small enough that a test can afford one per case, speaking
    /// into a buffer so that what the interface would see can be asserted.
    fn uci() -> UCI<AlphaBeta, Vec<u8>> {
        UCI::with_output(
            AlphaBeta::with_table_bytes(Board::new(), 8 * 1024),
            Vec::new(),
        )
    }

    /// Everything said so far, as one string.
    fn said(uci: &UCI<AlphaBeta, Vec<u8>>) -> String {
        String::from_utf8(uci.out.clone()).unwrap()
    }

    #[test]
    fn a_position_can_be_set_from_the_start_or_from_a_fen() {
        let mut uci = uci();
        assert_eq!(uci.parse_position("position startpos"), Ok(()));
        assert_eq!(uci.engine.active_color(), Color::White);

        let fen = "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R b KQ - 9 12";
        assert_eq!(uci.parse_position(&format!("position fen {}", fen)), Ok(()));
        assert_eq!(uci.engine.active_color(), Color::Black);
    }

    #[test]
    fn moves_after_a_position_are_played() {
        let mut uci = uci();
        assert_eq!(
            uci.parse_position("position startpos moves e2e4 e7e5 g1f3"),
            Ok(())
        );
        assert_eq!(uci.engine.active_color(), Color::Black);
    }

    #[test]
    fn malformed_positions_are_reported_rather_than_fatal() {
        // each of these used to panic, taking the engine down mid game
        for line in [
            "position",
            "position wibble",
            "position fen",
            "position fen not a fen at all",
            "position fen 8/8/8/8/8/8/8/8 w - - 0 1 moves e2e4",
            // both of these parsed, and then took the engine down on the search
            "position fen 8/8/8/8/8/8/8/8 w - - 0 1",
            "position fen 4k3/8/8/8/8/8/8/4R1K1 w - - 0 1",
            "position startpos moves e2e4 zzzz",
            "position startpos moves e2e4 e2e4",
        ] {
            let mut uci = uci();
            assert!(
                uci.parse_position(line).is_err(),
                "expected an error: {}",
                line
            );
        }
    }

    #[test]
    fn unrecognised_and_empty_commands_are_survivable() {
        let mut uci = uci();
        for line in ["", "   ", "wibble", "positional", "isready"] {
            assert!(uci.handle(line), "{} should not have quit", line);
        }
    }

    #[test]
    fn quit_stops_the_loop_and_leaves_the_rest_unread() {
        let mut uci = uci();
        uci.run(Cursor::new(
            "position startpos moves e2e4\nquit\nposition startpos\n",
        ));
        // the reset after quit must not have been acted on
        assert_eq!(uci.engine.active_color(), Color::Black);
    }

    #[test]
    fn the_loop_ends_when_the_input_does() {
        // without a quit the old loop asked a closed stdin for another line for
        // ever, so reaching the end of this call is the assertion
        let mut uci = uci();
        uci.run(Cursor::new("uci\nisready\nposition startpos\ngo depth 1\n"));
    }

    #[test]
    fn the_handshake_identifies_the_engine_and_ends_with_uciok() {
        let mut uci = uci();
        uci.handle("uci");
        let said = said(&uci);
        let lines: Vec<&str> = said.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("id name arche "));
        assert!(lines[1].starts_with("id author "));
        assert_eq!(lines[2], "uciok");
    }

    #[test]
    fn isready_is_answered_with_readyok_alone() {
        let mut uci = uci();
        uci.handle("isready");
        assert_eq!(said(&uci), "readyok\n");
    }

    #[test]
    fn a_search_reports_each_depth_and_then_its_move() {
        let mut uci = uci();
        uci.run(Cursor::new("position startpos\ngo depth 3\n"));
        let said = said(&uci);
        let lines: Vec<&str> = said.lines().collect();
        assert_eq!(lines.len(), 4, "a depth three search speaks four lines");
        for line in &lines[..3] {
            assert!(
                line.starts_with("info depth "),
                "not an info line: {}",
                line
            );
        }
        assert!(lines[3].starts_with("bestmove "));
    }

    #[test]
    fn a_search_on_a_negative_clock_still_answers_with_a_move() {
        let mut uci = uci();
        uci.run(Cursor::new("position startpos\ngo wtime -5 btime -5\n"));
        let said = said(&uci);
        let last = said.lines().last().unwrap_or("");
        assert!(last.starts_with("bestmove "), "{}", said);
        assert_ne!(last, "bestmove 0000", "{}", said);
    }

    #[test]
    fn a_position_with_no_legal_moves_reports_the_null_move() {
        for fen in [
            "7k/6Q1/6K1/8/8/8/8/8 b - - 0 1", // checkmate
            "7k/5Q2/6K1/8/8/8/8/8 b - - 0 1", // stalemate
        ] {
            let mut uci = uci();
            uci.run(Cursor::new(format!("position fen {}\ngo depth 1\n", fen)));
            let said = said(&uci);
            assert!(
                said.contains("info string no legal moves identified"),
                "{}",
                fen
            );
            assert!(said.ends_with("bestmove 0000\n"), "{}: {}", fen, said);
        }
    }

    #[test]
    fn what_could_not_be_acted_on_is_reported_as_an_info_string() {
        let mut uci = uci();
        uci.run(Cursor::new("position wibble\nwobble\n"));
        let said = said(&uci);
        assert!(said.contains("info string unrecognised position: wibble"));
        assert!(said.contains("info string unrecognised command: wobble"));
    }

    #[test]
    fn a_perft_command_reports_its_count() {
        let mut uci = uci();
        uci.run(Cursor::new("position startpos\nperft 2\n"));
        assert_eq!(said(&uci), "info string perft depth 2 nodes 400\n");
    }

    #[test]
    fn a_new_game_resets_the_position() {
        let mut uci = uci();
        uci.parse_position("position startpos moves e2e4").unwrap();
        assert_eq!(uci.engine.active_color(), Color::Black);
        assert!(uci.handle("ucinewgame"));
        assert_eq!(uci.engine.active_color(), Color::White);
    }

    #[test]
    fn each_colour_reads_its_own_clock() {
        let line = "go wtime 111 btime 222 winc 333 binc 444 movestogo 5";
        assert_eq!(
            time_control_from(line, Color::White),
            TimeControl {
                time: Some(111),
                increment: Some(333),
                moves_to_go: Some(5),
                move_time: None,
                infinite: false,
            }
        );
        assert_eq!(
            time_control_from(line, Color::Black),
            TimeControl {
                time: Some(222),
                increment: Some(444),
                moves_to_go: Some(5),
                move_time: None,
                infinite: false,
            }
        );
    }

    #[test]
    fn a_missing_clock_for_our_colour_is_not_taken_from_the_other() {
        let control = time_control_from("go btime 222 binc 444", Color::White);
        assert_eq!(control.time, None);
        assert_eq!(control.increment, None);
    }

    #[test]
    fn move_time_and_infinite_are_read() {
        assert_eq!(
            time_control_from("go movetime 500", Color::White).move_time,
            Some(500)
        );
        assert!(time_control_from("go infinite", Color::White).infinite);
        assert!(!time_control_from("go wtime 1000", Color::White).infinite);
    }

    #[test]
    fn a_clock_too_large_to_hold_is_not_read_as_absent() {
        let line = "go wtime 99999999999999999999999";
        assert_eq!(
            time_control_from(line, Color::White).time,
            Some(u64::MAX),
            "an unreadable clock must not turn into an unlimited search"
        );
    }

    #[test]
    fn a_negative_clock_is_read_as_empty() {
        // cutechess and fastchess send a clock below zero once their time
        // margin has been eaten into. Not reading it would leave the budget
        // unset and search without a limit, at the moment there is the least
        // time to spare
        let control = time_control_from("go wtime -5 btime -5", Color::White);
        assert_eq!(control.time, Some(0));
        let control = time_control_from("go wtime -5 btime -5 winc -1 binc -1", Color::Black);
        assert_eq!(control.time, Some(0));
        assert_eq!(control.increment, Some(0));
    }

    #[test]
    fn a_node_limit_is_read() {
        assert_eq!(capture(&NODES_RE, "go nodes 1234"), Some(1234));
        assert_eq!(capture(&NODES_RE, "go depth 3"), None);
    }

    #[test]
    fn a_node_limit_is_honoured_end_to_end() {
        let mut uci = uci();
        uci.run(Cursor::new("position startpos\ngo nodes 5000\n"));
        let said = said(&uci);
        let lines: Vec<&str> = said.lines().collect();
        assert!(lines.last().unwrap().starts_with("bestmove "), "{}", said);
        let info = lines[lines.len() - 2];
        let nodes: u64 = info
            .split_whitespace()
            .skip_while(|word| *word != "nodes")
            .nth(1)
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no node count in {}", info));
        assert!(nodes <= 5000, "{}", said);
    }

    #[test]
    fn an_oversized_depth_does_not_panic() {
        assert_eq!(capture(&DEPTH_RE, "go depth 999"), Some(999));
    }

    #[test]
    fn a_perft_command_reads_its_depth() {
        assert_eq!(perft_depth("perft 3"), 3);
        assert_eq!(perft_depth("perft"), 1, "a bare perft counts to depth one");
        assert_eq!(perft_depth("perft 999"), u8::MAX);
    }

    #[test]
    fn a_perft_command_counts_without_disturbing_the_position() {
        let mut uci = uci();
        uci.parse_position("position startpos").unwrap();
        assert_eq!(uci.engine.perft(2), 400);
        // counting is make/undo all the way down, the position must survive it
        assert_eq!(uci.engine.active_color(), Color::White);
    }

    use basic_engine::Play;

    /// The move of this name in the starting position, for building the
    /// synthetic results the format tests pin.
    fn play_named(name: &str) -> Play {
        *Board::new()
            .generate_moves()
            .iter()
            .find(|m| format!("{}", m) == name)
            .unwrap_or_else(|| panic!("{} is not a move here", name))
    }

    #[test]
    fn an_info_line_reports_a_centipawn_score() {
        let result = SearchResult {
            nodes: 2000,
            selective_depth: 7,
            best_move: play_named("e2e4"),
            score: 25,
        };
        let pv = PvLine::new(vec![play_named("e2e4"), play_named("g1f3")]);
        assert_eq!(
            format_info(5, &result, &pv, Duration::from_millis(500)),
            "info depth 5 seldepth 7 nodes 2000 time 500 nps 4000 score cp 25 pv e2e4 g1f3"
        );
    }

    #[test]
    fn an_info_line_reports_a_mate_score_in_moves() {
        // three plies from checkmate reads as mate in two moves
        let result = SearchResult {
            nodes: 1500,
            selective_depth: 4,
            best_move: play_named("e2e4"),
            score: 30_000 - 3,
        };
        let pv = PvLine::new(vec![play_named("e2e4")]);
        assert_eq!(
            format_info(4, &result, &pv, Duration::from_millis(20)),
            "info depth 4 seldepth 4 nodes 1500 time 20 nps 75000 score mate 2 pv e2e4"
        );
    }

    #[test]
    fn a_search_faster_than_a_millisecond_still_reports_a_rate() {
        let result = SearchResult {
            nodes: 300,
            selective_depth: 1,
            best_move: play_named("e2e4"),
            score: 0,
        };
        let pv = PvLine::new(vec![]);
        assert_eq!(
            format_info(1, &result, &pv, Duration::ZERO),
            "info depth 1 seldepth 1 nodes 300 time 0 nps 300000 score cp 0 pv "
        );
    }
}
