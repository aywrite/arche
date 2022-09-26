use basic_engine::Color;
use basic_engine::Engine;
use basic_engine::SearchParameters;
use regex::Regex;
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
                    self.parse_position("position startpos");
                } else if line.starts_with("uci") {
                    println!("id name {} {}", self.name, self.version);
                    println!("author {}", self.author);
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

        let mut time = match self.engine.active_color() {
            Color::White => {
                if let Some(wtime) = WTIME_RE.captures(line) {
                    Some(wtime.get(1).unwrap().as_str().parse::<u64>().unwrap())
                } else {
                    None
                }
            }
            Color::Black => {
                if let Some(btime) = BTIME_RE.captures(line) {
                    Some(btime.get(1).unwrap().as_str().parse::<u64>().unwrap())
                } else {
                    None
                }
            }
        };
        let increment = match self.engine.active_color() {
            Color::White => {
                if let Some(winc) = WINC_RE.captures(line) {
                    Some(winc.get(1).unwrap().as_str().parse::<u64>().unwrap())
                } else {
                    None
                }
            }
            Color::Black => {
                if let Some(binc) = BINC_RE.captures(line) {
                    Some(binc.get(1).unwrap().as_str().parse::<u64>().unwrap())
                } else {
                    None
                }
            }
        };
        if let Some(move_time) = MOVE_TIME.captures(line) {
            time = Some(move_time.get(1).unwrap().as_str().parse::<u64>().unwrap());
        }

        sp.depth = if let Some(depth_str) = DEPTH_RE.captures(line) {
            Some(depth_str.get(1).unwrap().as_str().parse::<u8>().unwrap())
        } else {
            None
        };

        // TODO what if inc is set but not time?
        if let Some(time) = time {
            let mut duration = if let Some(inc) = increment {
                (time / 40) + inc
            } else {
                time / 40
            };
            duration -= (duration / 10).min(50); // Buffer to be sure we don't run out of time
            sp.search_duration = Some(Duration::from_millis(duration));
        }

        if INFINITE_RE.is_match(line) {
            sp.search_duration = None;
        }

        println!("bestmove {}", self.engine.iterative_deepening_search(sp));
    }
}
