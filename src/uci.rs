use crate::time_control::TimeControl;
use basic_engine::Color;
use basic_engine::Engine;
use basic_engine::SearchParameters;
use regex::Regex;

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
}

/// Reads the value that follows a keyword. The value is matched as digits, so
/// the only way it can fail to parse is by being too large to hold, in which
/// case use the largest value we can. Discarding it would read as the keyword
/// having been absent, which for a clock means searching without a limit.
fn capture(re: &Regex, line: &str) -> Option<u64> {
    let digits = re.captures(line)?.get(1)?.as_str();
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
        loop {
            if let Some(result) = std::io::stdin().lines().next() {
                let line = result.unwrap();
                if line.starts_with("quit") {
                    return;
                } else if line.starts_with("isready") {
                    println!("readyok");
                } else if line.starts_with("ucinewgame") {
                    self.engine.new_game();
                    self.parse_position("position startpos");
                } else if line.starts_with("uci") {
                    println!("id name {} {}", self.name, self.version);
                    println!("id author {}", self.author);
                    println!("uciok");
                } else if line.starts_with("position") {
                    self.parse_position(&line);
                } else if line.starts_with("display") {
                    self.engine.display_board();
                } else if line.starts_with("go") {
                    self.parse_go(&line);
                } else if line.starts_with("perft") {
                    self.engine.perft();
                } else {
                    println!("Failed to parse line: {}", line);
                }
            };
        }
    }

    fn parse_position(&mut self, line: &str) {
        let position_string = line.strip_prefix("position").unwrap().trim();
        let (start, move_list) = match position_string.split_once("moves") {
            Some((s, m)) => (s.trim(), Some(m)),
            None => (position_string, None),
        };
        if start.starts_with("startpos") {
            self.engine
                .parse_fen(START_FEN)
                .expect("parse of start fen should never fail");
        } else if let Some(fen) = start.strip_prefix("fen") {
            self.engine.parse_fen(fen.trim()).unwrap();
        } else {
            panic!("Unexpected position: {}", start);
        }

        if let Some(moves) = move_list {
            for m in moves.split_whitespace() {
                assert!(
                    self.engine.make_move_str(m.trim()),
                    "Failed to parse/play {}",
                    m
                );
            }
        }
    }

    fn parse_go(&mut self, line: &str) {
        let mut sp = SearchParameters::new();
        sp.print_info = true;

        sp.search_duration = time_control_from(line, self.engine.active_color()).budget();
        sp.depth = capture(&DEPTH_RE, line).map(|depth| depth.try_into().unwrap_or(u8::MAX));

        match self.engine.iterative_deepening_search(sp) {
            // 0000 is the null move, used to report that there is no move to make
            Some(play) => println!("bestmove {}", play),
            None => println!("bestmove 0000"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
