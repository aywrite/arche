use crate::time_control::TimeControl;
use basic_engine::Color;
use basic_engine::Engine;
use basic_engine::SearchOutcome;
use basic_engine::SearchParameters;
use basic_engine::{DEFAULT_TABLE_MB, MAX_TABLE_MB, MIN_TABLE_MB};
use basic_engine::{PvLine, SearchResult};
use regex::Regex;
use std::io::BufRead;
use std::time::Duration;

const START_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

lazy_static! {
    static ref WTIME_RE: Regex = Regex::new(r"wtime (\d+)").unwrap();
    static ref BTIME_RE: Regex = Regex::new(r"btime (\d+)").unwrap();
    static ref WINC_RE: Regex = Regex::new(r"winc (\d+)").unwrap();
    static ref BINC_RE: Regex = Regex::new(r"binc (\d+)").unwrap();
    static ref MOVES_TO_GO_RE: Regex = Regex::new(r"movestogo (\d+)").unwrap();
    static ref MOVE_TIME: Regex = Regex::new(r"movetime (\d+)").unwrap();
    static ref DEPTH_RE: Regex = Regex::new(r"depth (\d+)").unwrap();
    static ref INFINITE_RE: Regex = Regex::new(r"infinite").unwrap();
    static ref PERFT_RE: Regex = Regex::new(r"perft (\d+)").unwrap();
    // an option name may contain spaces, so take everything up to the value
    static ref SETOPTION_RE: Regex =
        Regex::new(r"(?i)^\s*setoption\s+name\s+(.+?)\s+value\s+(.*?)\s*$").unwrap();
}

/// Reads the value that follows a keyword. The value is matched as digits, so
/// the only way it can fail to parse is by being too large to hold, in which
/// case use the largest value we can. Discarding it would read as the keyword
/// having been absent, which for a clock means searching without a limit.
fn capture(re: &Regex, line: &str) -> Option<u64> {
    let digits = re.captures(line)?.get(1)?.as_str();
    Some(digits.parse().unwrap_or(u64::MAX))
}

/// Tells the interface about anything we could not act on. A bad line is the
/// interface's problem to fix, so say so and carry on reading.
fn report(result: Result<(), String>) {
    if let Err(error) = result {
        println!("info string {}", error);
    }
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
        infinite: INFINITE_RE.is_match(line),
    }
}

pub struct UCI<T: Engine> {
    author: String,
    name: String,
    version: String,

    engine: T,
}

impl<T: Engine> UCI<T> {
    pub fn new_with_engine(engine: T) -> Self {
        Self {
            author: env!("CARGO_PKG_AUTHORS").to_string(),
            name: env!("CARGO_PKG_NAME").to_string(), // TODO change based on engine?
            version: env!("CARGO_PKG_VERSION").to_string(),
            engine,
        }
    }

    pub fn read_loop(&mut self) {
        self.run(std::io::stdin().lock());
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
                    println!("info string could not read input: {}", error);
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
            println!("readyok");
        } else if line.starts_with("ucinewgame") {
            self.engine.new_game();
            let result = self.parse_position("position startpos");
            report(result);
        } else if line.starts_with("setoption") {
            let result = self.parse_setoption(line);
            report(result);
        } else if line.starts_with("uci") {
            println!("id name {} {}", self.name, self.version);
            println!("id author {}", self.author);
            println!(
                "option name Hash type spin default {} min {} max {}",
                DEFAULT_TABLE_MB, MIN_TABLE_MB, MAX_TABLE_MB
            );
            println!("uciok");
        } else if line.starts_with("position") {
            let result = self.parse_position(line);
            report(result);
        } else if line.starts_with("display") {
            self.engine.display_board();
        } else if line.starts_with("go") {
            self.parse_go(line);
        } else if line.starts_with("perft") {
            let depth = perft_depth(line);
            let nodes = self.engine.perft(depth);
            println!("info string perft depth {} nodes {}", depth, nodes);
        } else {
            println!("info string unrecognised command: {}", line);
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

    /// An option we never advertised is said something about and otherwise left
    /// alone, which is what the protocol asks for. There is nothing an engine
    /// can usefully do about an interface offering it a setting it has not got.
    fn parse_setoption(&mut self, line: &str) -> Result<(), String> {
        let captures = SETOPTION_RE
            .captures(line)
            .ok_or_else(|| format!("could not read an option from: {}", line))?;
        let name = captures.get(1).unwrap().as_str().trim();
        let value = captures.get(2).unwrap().as_str().trim();

        if !name.eq_ignore_ascii_case("hash") {
            return Err(format!("no option named {}", name));
        }
        let requested: usize = value
            .parse()
            .map_err(|_| format!("Hash wants a whole number of megabytes, got {}", value))?;
        let actual = self.engine.set_table_mb(requested);
        if actual != requested {
            println!(
                "info string Hash {} is outside {} to {}, using {}",
                requested, MIN_TABLE_MB, MAX_TABLE_MB, actual
            );
        }
        Ok(())
    }

    fn parse_go(&mut self, line: &str) {
        let mut sp = SearchParameters::new();
        sp.search_duration = time_control_from(line, self.engine.active_color()).budget();
        sp.depth = capture(&DEPTH_RE, line).map(|depth| depth.try_into().unwrap_or(u8::MAX));

        let start = sp.start_time;
        let outcome = self
            .engine
            .iterative_deepening_search(sp, |depth, result, pv| {
                println!("{}", format_info(depth, result, &pv, start.elapsed()));
            });
        match outcome {
            SearchOutcome::Complete(result) | SearchOutcome::Aborted(Some(result)) => {
                println!("bestmove {}", result.best_move);
            }
            SearchOutcome::GameOver => {
                println!("info string no legal moves identified");
                // 0000 is the null move, used to report that there is no move
                // to make
                println!("bestmove 0000");
            }
            SearchOutcome::Aborted(None) => println!("bestmove 0000"),
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

    /// A table small enough that a test can afford one per case.
    fn uci() -> UCI<AlphaBeta> {
        UCI::new_with_engine(AlphaBeta::with_table_bytes(Board::new(), 8 * 1024))
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
    fn a_new_game_resets_the_position() {
        let mut uci = uci();
        uci.parse_position("position startpos moves e2e4").unwrap();
        assert_eq!(uci.engine.active_color(), Color::Black);
        assert!(uci.handle("ucinewgame"));
        assert_eq!(uci.engine.active_color(), Color::White);
    }

    #[test]
    fn the_hash_option_is_advertised_and_accepted() {
        let mut uci = uci();
        assert_eq!(uci.parse_setoption("setoption name Hash value 16"), Ok(()));
        // an interface echoes back what we advertised, but be forgiving anyway
        assert_eq!(uci.parse_setoption("setoption name hash value 32"), Ok(()));
        assert_eq!(uci.parse_setoption("SETOPTION NAME HASH VALUE 8"), Ok(()));
    }

    #[test]
    fn the_hash_option_resizes_the_table() {
        let mut uci = uci();
        // small sizes only, a test has no business allocating the maximum
        for mb in [1, 2, 4] {
            assert!(
                uci.parse_setoption(&format!("setoption name Hash value {}", mb))
                    .is_ok()
            );
            assert!(uci.handle("go depth 3"), "still usable at {}MB", mb);
        }
    }

    #[test]
    fn a_hash_size_out_of_range_is_reported_rather_than_refused() {
        // only the small end is asked for here, the other would allocate four
        // gigabytes to show a clamp basic_engine already covers
        let mut uci = uci();
        assert_eq!(uci.parse_setoption("setoption name Hash value 0"), Ok(()));
    }

    #[test]
    fn malformed_options_are_reported_rather_than_fatal() {
        let mut uci = uci();
        for line in [
            "setoption",
            "setoption name",
            "setoption name Hash",
            "setoption name Hash value",
            "setoption name Hash value wibble",
            "setoption name Hash value 16.5",
            "setoption name Threads value 4",
            "setoption value 16",
        ] {
            assert!(
                uci.parse_setoption(line).is_err(),
                "expected an error: {}",
                line
            );
        }
    }

    #[test]
    fn setoption_does_not_quit_the_loop() {
        let mut uci = uci();
        assert!(uci.handle("setoption name Hash value 4"));
        assert!(uci.handle("setoption name Nonsense value 1"));
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
