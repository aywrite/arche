use crate::board::Board;
use crate::misc::Color;
use crate::play::Play;
use crate::Game;
use std::fmt;
use std::mem;
use std::time;

const CHECKMATE_SCORE: i64 = 800_000;
const MAX_DEPTH: u8 = 16;

pub trait Engine {
    fn new(board: Board) -> Self;

    fn parse_fen(&mut self, fen_string: &str) -> Result<(), String>;

    fn should_stop(&self) -> bool;

    fn perft(&mut self);

    fn search(&mut self, depth: u8) -> Option<SearchResult>;

    //fn make_move(&mut self, play: &Play);

    fn make_move_str(&mut self, play: &str) -> bool;

    fn iterative_deepening_search(&mut self, search_options: SearchParameters) -> Play {
        let mut best_move: Option<Play> = None;
        let max_depth = match search_options.depth {
            Some(depth) => depth,
            None => MAX_DEPTH,
        };
        self.configure(search_options.start_time, search_options.search_duration);

        for depth in 1..=max_depth {
            let search_result = self.search(depth);
            if self.should_stop() {
                return best_move.unwrap();
            }
            if let Some(m) = &search_result {
                best_move = Some(m.best_move);
                if search_options.print_info {
                    if let Some(mate_in) = m.checkmate_in() {
                        println!(
                            "info depth {} nodes {} score mate {} pv {}",
                            depth,
                            m.nodes,
                            mate_in,
                            self.pv_line(),
                        );
                    } else {
                        println!(
                            "info depth {} nodes {} score cp {} pv {}",
                            depth,
                            m.nodes,
                            m.score,
                            self.pv_line(),
                            // TODO add search time to this
                            // TODO add nodes per second
                            // TODO add selective depth (qui)
                        );
                    }
                }
            } else {
                println!("info string no legal moves identified");
            }
        }
        best_move.unwrap()
    }

    fn configure(&mut self, start_time: time::Instant, search_duration: Option<time::Duration>);

    fn display_board(&self);

    fn pv_line(&self) -> PvLine;

    fn active_color(&self) -> Color;
}

pub struct SearchParameters {
    pub depth: Option<u8>,
    pub search_duration: Option<time::Duration>,
    pub start_time: time::Instant,
    pub print_info: bool,
}

impl SearchParameters {
    pub fn new() -> Self {
        Self {
            depth: None,
            search_duration: None,
            start_time: time::Instant::now(),
            print_info: false,
        }
    }

    pub fn new_with_depth(depth: u8) -> Self {
        Self {
            depth: Some(depth),
            search_duration: None,
            start_time: time::Instant::now(),
            print_info: false,
        }
    }
}

pub struct AlphaBeta {
    pub board: Board,
    nodes: u64,
    score: i64,
    moves: HashTable,
    // search parameters
    search_depth: u8,
    // search state
    start_time: time::Instant,
    search_duration: Option<time::Duration>,
    should_stop: bool,
}

impl AlphaBeta {
    fn eval(&self) -> i64 {
        let eval = i64::from(self.board.white_value) - i64::from(self.board.black_value);

        let mut score = 0i64;
        for i in 0..64u8 {
            score += self.board.piece_value(i) as i64;
        }
        let eval = eval + score;

        match self.board.active_color {
            Color::White => eval,
            Color::Black => -eval,
        }
    }

    fn check_if_should_stop(&mut self) {
        if let Some(search_time) = self.search_duration {
            self.should_stop = self.start_time.elapsed() >= search_time;
        }
    }

    fn quiescence(&mut self, mut alpha: i64, beta: i64) -> i64 {
        if self.board.line_ply >= MAX_DEPTH.into() {
            return self.eval();
        }
        if self.nodes % 3000 == 0 {
            self.check_if_should_stop();
        }
        self.nodes += 1;

        let score = self.eval();
        if score >= beta {
            return beta;
        } else if score >= alpha {
            alpha = score;
        }

        let mut best_move: Option<Play> = None;
        let mut best_board: Option<u64> = None;
        let old_alpha = alpha;
        let mut score: i64;
        let pv_line = self.moves.get(self.board.key);
        //let moves = self.board.generate_moves();
        let mut moves = self.board.generate_captures();
        moves.sort_by_cached_key(|m| {
            let mut score = m.mmv_lva(&self.board);
            if let Some(pv) = pv_line {
                if pv.play == *m {
                    score += 100000;
                }
            };
            score
        });

        for m in moves.iter().rev() {
            if self.board.make_move(m) {
                score = -self.quiescence(-beta, -alpha);
                if score > alpha {
                    if score >= beta {
                        self.board.undo_move().unwrap();
                        return beta;
                    }
                    alpha = score;
                    best_move = Some(*m);
                    best_board = Some(self.board.key);
                }
                self.board.undo_move().unwrap();
                if self.should_stop {
                    // TODO return an error instead
                    return 0;
                }
            }
        }

        if alpha != old_alpha {
            self.moves.set(
                self.board.key,
                Pv {
                    play: best_move.unwrap(),
                    next_key: best_board.unwrap(),
                },
            );
        }
        alpha
    }

    fn alpha_beta(&mut self, mut alpha: i64, beta: i64, depth: u8) -> i64 {
        if self.nodes % 3000 == 0 {
            self.check_if_should_stop();
        }
        self.nodes += 1;

        if depth == 0 {
            if self.search_depth >= 4 {
                return self.quiescence(alpha, beta);
            }
            return self.eval();
        }

        if self.board.fifty_move_rule >= 100 || self.board.is_repetition() {
            return 0;
        }

        let old_alpha = alpha;
        let mut score: i64;
        let mut found_legal_move = false;
        let mut best_move: Option<&Play> = None;
        let mut best_board: Option<u64> = None;
        let pv_line = self.moves.get(self.board.key);

        let mut moves = self.board.generate_moves();
        moves.sort_by_cached_key(|m| {
            let mut score = m.mmv_lva(&self.board);
            if let Some(pv) = pv_line {
                if pv.play == *m {
                    score += 100_000;
                }
            };
            score
        });

        for m in moves.iter().rev() {
            if self.board.make_move(m) {
                found_legal_move = true;
                score = -self.alpha_beta(-beta, -alpha, depth - 1);
                if score > alpha {
                    if score >= beta {
                        self.board.undo_move().unwrap();
                        return beta;
                    }
                    alpha = score;
                    best_move = Some(m);
                    best_board = Some(self.board.key);
                }
                self.board.undo_move().unwrap();
                if self.should_stop {
                    // TODO return an error instead
                    return 0;
                }
            }
        }

        if !found_legal_move {
            if self.board.is_king_attacked() {
                return -CHECKMATE_SCORE + (self.board.line_ply as i64);
            }
            return 0;
        }

        if alpha != old_alpha {
            self.moves.set(
                self.board.key,
                Pv {
                    play: *best_move.unwrap(),
                    next_key: best_board.unwrap(),
                },
            );
        }
        alpha
    }
}

#[derive(Copy, Clone, Debug)]
struct Pv {
    next_key: u64,
    play: Play,
}

#[derive(Debug)]
struct HashTable {
    table: Vec<Option<Pv>>,
    capacity: usize,
}

impl HashTable {
    // TODO make generic
    fn new() -> Self {
        HashTable::with_capacity(10000)
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            table: vec![None; capacity as usize],
            capacity,
        }
    }

    fn with_capacity_bytes(bytes: usize) -> Self {
        let entry_size = mem::size_of::<u64>() + mem::size_of::<Pv>();
        Self::with_capacity(bytes / entry_size)
    }

    fn get(&self, index: u64) -> Option<&Pv> {
        let key = (index % self.capacity as u64) as usize;
        (&self.table[key]).as_ref()
    }

    fn set(&mut self, index: u64, pv: Pv) {
        let key = (index % self.capacity as u64) as usize;
        self.table[key] = Some(pv);
    }
}

pub struct PvLine {
    line: Vec<Play>,
}

impl fmt::Display for PvLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let out: Vec<String> = self.line.iter().map(|p| format!("{}", p)).collect();
        let new = out.join(" ");
        write!(f, "{}", new)?;
        Ok(())
    }
}

pub struct SearchResult {
    nodes: u64,      // The number of results examined as part of the search
    best_move: Play, // The best move found as part of the search
    score: i64,      // The estimated score for the best move if played
}

impl SearchResult {
    fn checkmate_in(&self) -> Option<i64> {
        if (CHECKMATE_SCORE - self.score.abs()) < 300 {
            let mut mate = ((CHECKMATE_SCORE - self.score.abs()) / 2) + 1;
            if self.score < 0 {
                mate = -mate;
            };
            return Some(mate);
        }
        None
    }
}

impl Engine for AlphaBeta {
    fn new(board: Board) -> Self {
        Self {
            board,
            nodes: 0,
            score: 0,
            moves: HashTable::with_capacity_bytes(500 * 1024 * 1024),
            search_depth: 0,
            start_time: time::Instant::now(),
            search_duration: None,
            should_stop: false,
        }
    }

    fn perft(&mut self) {
        // TODO add a param
        self.board.perft(1);
    }

    fn configure(&mut self, start_time: time::Instant, search_duration: Option<time::Duration>) {
        self.start_time = start_time;
        self.search_duration = search_duration;
        self.should_stop = false;
    }

    fn active_color(&self) -> Color {
        self.board.active_color
    }

    fn should_stop(&self) -> bool {
        self.should_stop
    }

    fn parse_fen(&mut self, fen_string: &str) -> Result<(), String> {
        self.nodes = 0;
        self.score = 0;
        self.board = Board::from_fen(fen_string)?;
        Ok(())
    }

    fn search(&mut self, depth: u8) -> Option<SearchResult> {
        self.nodes = 0;
        self.search_depth = depth;
        self.board.line_ply = 0;
        self.score = self.alpha_beta(i64::MIN + 1, i64::MAX - 1, depth);
        if let Some(best_move) = self.moves.get(self.board.key) {
            return Some(SearchResult {
                nodes: self.nodes,
                score: self.score,
                best_move: best_move.play,
            });
        }
        None
    }

    //fn make_move(&mut self, play: &Play) {
    //    self.board.make_move(play);
    //}

    fn make_move_str(&mut self, play: &str) -> bool {
        for p in self.board.generate_moves() {
            let play_str = format!("{}", p).to_lowercase();
            if play == play_str {
                return self.board.make_move(&p); // TODO change this to return Result
            };
        }
        false
    }

    fn display_board(&self) {
        println!("{}", self.board);
    }

    fn pv_line(&self) -> PvLine {
        let mut pv_line = Vec::new();
        let mut pv = self.moves.get(self.board.key).unwrap();
        pv_line.push(pv.play);
        while let Some(next) = self.moves.get(pv.next_key) {
            pv_line.push(next.play);
            pv = next;
            if pv_line.len() >= 16 {
                break; // TODO resolve hash colisions to prevent errors here
            }
        }
        PvLine { line: pv_line }
    }
}
