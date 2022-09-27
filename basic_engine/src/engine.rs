use crate::board::Board;
use crate::misc::Color;
use crate::play::Play;
use crate::Game;
use std::fmt;
use std::mem;
use std::time;

const CHECKMATE_SCORE: i64 = 800_000;
const MAX_DEPTH: u8 = 20;

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
                            "info depth {} seldepth {} nodes {} score mate {} pv {}",
                            depth,
                            m.selective_depth,
                            m.nodes,
                            mate_in,
                            self.pv_line(),
                        );
                    } else {
                        println!(
                            "info depth {} seldepth {} nodes {} score cp {} pv {}",
                            depth,
                            m.selective_depth,
                            m.nodes,
                            m.score,
                            self.pv_line(),
                            // TODO add search time to this
                            // TODO add nodes per second
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
    selective_depth: u8,
    // search parameters
    search_depth: u8,
    // search state
    start_time: time::Instant,
    search_duration: Option<time::Duration>,
    should_stop: bool,
}

impl AlphaBeta {
    fn eval(&self) -> i64 {
        self.board.eval()
    }

    pub fn clear_cache(&mut self) {
        self.moves.clear();
    }

    fn check_if_should_stop(&mut self) {
        if let Some(search_time) = self.search_duration {
            self.should_stop = self.start_time.elapsed() >= search_time;
        }
    }

    fn quiescence(&mut self, mut alpha: i64, beta: i64) -> i64 {
        self.selective_depth = self.selective_depth.max(self.board.line_ply as u8);
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
        let mut moves = self.board.generate_captures();
        moves.sort_by_cached_key(|m| {
            let mut score = m.mmv_lva(&self.board);
            if let Some(pv) = pv_line {
                if pv.play == *m {
                    score += 100000;
                }
            };
            -(score as i64)
        });

        for m in &moves {
            if self.board.make_move(m) {
                score = -self.quiescence(-beta, -alpha);
                if self.should_stop {
                    // TODO return an error instead
                    self.board.undo_move().unwrap();
                    return 0;
                }
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
            }
        }

        if alpha != old_alpha {
            self.moves.set(
                self.board.key,
                Pv {
                    play: best_move.unwrap(),
                    next_key: best_board.unwrap(),
                    score: alpha,
                    depth: 0, // Never use a quiescence move instead of evaluating, only for move ordering
                    node: Node::Ordering,
                },
            );
        }
        alpha
    }

    fn alpha_beta(&mut self, mut alpha: i64, beta: i64, mut depth: u8) -> i64 {
        if self.nodes % 3000 == 0 {
            self.check_if_should_stop();
        }
        self.selective_depth = self.selective_depth.max(self.board.line_ply as u8);
        self.nodes += 1;

        if self.board.fifty_move_rule >= 100 || self.board.is_repetition() {
            return 0;
        }
        let in_check = self.board.is_king_attacked();
        if in_check {
            depth += 1;
        }

        if depth == 0 {
            if self.search_depth >= 4 {
                return self.quiescence(alpha, beta);
            }
            return self.eval();
        }

        let old_alpha = alpha;
        let mut score: i64;
        let mut found_legal_move = false;
        let mut best_move: Option<&Play> = None;
        let mut best_board: Option<u64> = None;
        let (pv_line, cutoff) = self.get_transposition(self.board.key, alpha, beta, depth);
        if cutoff {
            return pv_line.unwrap().score;
        }

        let mut moves = self.board.generate_moves();
        moves.sort_by_cached_key(|m| {
            let mut score = m.mmv_lva(&self.board);
            if let Some(pv) = pv_line {
                if pv.play == *m {
                    score += 100_000;
                }
            };
            -(score as i64)
        });

        for m in &moves {
            if self.board.make_move(m) {
                found_legal_move = true;
                score = -self.alpha_beta(-beta, -alpha, depth - 1);
                if self.should_stop {
                    // TODO return an error instead
                    self.board.undo_move().unwrap();
                    return 0;
                }
                if score > alpha {
                    best_move = Some(m);
                    best_board = Some(self.board.key);
                    if score >= beta {
                        self.board.undo_move().unwrap();
                        self.moves.set(
                            self.board.key,
                            Pv {
                                play: *best_move.unwrap(),
                                next_key: best_board.unwrap(),
                                depth: depth as usize,
                                score: beta,
                                node: Node::Beta,
                            },
                        );
                        return beta;
                    }
                    alpha = score;
                }
                self.board.undo_move().unwrap();
            }
        }

        if !found_legal_move {
            if in_check {
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
                    depth: depth as usize,
                    score: alpha,
                    node: Node::Exact,
                },
            );
        } else if let Some(&bm) = best_move {
            self.moves.set(
                self.board.key,
                Pv {
                    play: bm,
                    next_key: best_board.unwrap(),
                    depth: depth as usize,
                    score: alpha,
                    node: Node::Alpha,
                },
            );
        }
        alpha
    }

    fn get_transposition(&self, key: u64, alpha: i64, beta: i64, depth: u8) -> (Option<&Pv>, bool) {
        let pv = self.moves.get(key);
        if let Some(pv) = pv {
            if pv.depth >= depth.into() {
                match pv.node {
                    Node::Exact => return (Some(pv), true),
                    Node::Alpha => {
                        if pv.score <= alpha {
                            return (Some(pv), true);
                        }
                    }
                    Node::Beta => {
                        if pv.score >= beta {
                            return (Some(pv), true);
                        }
                    }
                    Node::Ordering => {
                        return (None, false);
                    }
                }
            }
        }
        (None, false)
    }
}

#[derive(Copy, Clone, Debug)]
struct Pv {
    next_key: u64,
    play: Play,
    score: i64,
    depth: usize,
    node: Node,
}

#[derive(Copy, Clone, Debug)]
// TODO better name for this
enum Node {
    Exact,
    Alpha,
    Beta,
    Ordering,
}

#[derive(Debug)]
struct HashTable {
    table: Vec<Option<Pv>>,
    capacity: usize,
}

impl HashTable {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            table: vec![None; capacity as usize],
            capacity,
        }
    }

    fn clear(&mut self) {
        self.table = vec![None; self.capacity as usize];
    }

    fn with_capacity_bytes(bytes: usize) -> Self {
        let entry_size = mem::size_of::<Pv>();
        Self::with_capacity(bytes / entry_size)
    }

    fn get(&self, index: u64) -> Option<&Pv> {
        let key = (index % self.capacity as u64) as usize;
        (&self.table[key]).as_ref()
    }

    fn clear_key(&mut self, index: u64) {
        let key = (index % self.capacity as u64) as usize;
        self.table[key] = None;
    }

    fn set(&mut self, index: u64, pv: Pv) {
        let key = (index % self.capacity as u64) as usize;
        if let Some(old_pv) = self.table[key] {
            if matches!(old_pv.node, Node::Exact) && !matches!(pv.node, Node::Exact) {
                return;
            }
        }
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

#[derive(Debug)]
pub struct SearchResult {
    nodes: u64,          // The number of results examined as part of the search
    selective_depth: u8, // Selective search depth in plies
    best_move: Play,     // The best move found as part of the search
    score: i64,          // The estimated score for the best move if played
}

impl SearchResult {
    fn checkmate_in(&self) -> Option<i64> {
        if (CHECKMATE_SCORE - self.score.abs()) < 300 {
            let mut mate = (CHECKMATE_SCORE - self.score.abs() + 1) / 2;
            if self.score < 0 {
                mate = -mate;
            };
            return Some(mate);
        }
        None
    }
}

#[cfg(test)]
mod test_search {
    use super::AlphaBeta;
    use super::Board;
    use super::Engine;
    use super::Game;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_checkmate_in_2_white() {
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - - 0 0").unwrap();
        let mut e = <AlphaBeta as Engine>::new(game);
        let result = e.search(4).unwrap();
        assert_eq!(result.checkmate_in(), Some(2));
        assert_eq!(format!("{}", result.best_move), "g3g6");
    }

    #[test]
    fn test_checkmate_in_1_black() {
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbNQp/3pN3/2pP4/2P5/PPB4P/R4RK1 b - - 1 1").unwrap();
        let mut e = <AlphaBeta as Engine>::new(game);
        let result = e.search(4).unwrap();
        assert_eq!(result.checkmate_in(), Some(-1));
    }

    #[test]
    fn test_fifty_move_rule_play_for_draw() {
        // white is down material in this position so should play for fifty move draw
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 99 112").unwrap();
        let mut e = <AlphaBeta as Engine>::new(game);
        let result = e.search(3).unwrap();
        assert_eq!(result.score, 0);
    }

    #[test]
    fn test_fifty_move_rule_no_legal_moves() {
        // The fifty move rules has been triggered - there should not be any legal moves
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 100 112").unwrap();
        let mut e = <AlphaBeta as Engine>::new(game);
        let result = e.search(3);
        assert!(result.is_none());
    }
}

impl Engine for AlphaBeta {
    fn new(board: Board) -> Self {
        Self {
            board,
            nodes: 0,
            score: 0,
            moves: HashTable::with_capacity_bytes(16 * 1024 * 1024),
            search_depth: 0,
            selective_depth: 0,
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
        self.selective_depth = depth;
        self.board.line_ply = 0;
        self.score = self.alpha_beta(i64::MIN + 1, i64::MAX - 1, depth);
        if let Some(best_move) = self.moves.get(self.board.key) {
            assert!(
                matches!(best_move.node, Node::Exact),
                "played best move from non exact node {:?}",
                best_move.node
            );
            return Some(SearchResult {
                nodes: self.nodes,
                score: self.score,
                selective_depth: self.selective_depth,
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
                let result = self.board.make_move(&p);
                self.moves.clear_key(self.board.key); // TODO this is a hack to try to fix bad
                                                      // cache hits, particularly for draws
                return result; // TODO change this to return Result
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
