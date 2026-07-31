use crate::Game;
use crate::board::Board;
use crate::misc::Color;
use crate::play::Play;
use std::fmt;
use std::mem;
use std::time;

const CHECKMATE_SCORE: i64 = 800_000;
const MAX_DEPTH: u8 = 20;
const DEFAULT_TABLE_BYTES: usize = 500 * 1024 * 1024;
// Any score this close to CHECKMATE_SCORE is a forced mate. Regular evals are
// bounded by material values which are orders of magnitude smaller.
const CHECKMATE_THRESHOLD: i64 = CHECKMATE_SCORE - 1000;

/// Convert a score to its transposition table form. Mate scores are stored
/// relative to the node they are stored at (plies-to-mate from this node)
/// rather than relative to the root of the search, so that they remain correct
/// when the entry is reused at a different distance from the root.
fn score_to_tt(score: i64, line_ply: usize) -> i64 {
    if score > CHECKMATE_THRESHOLD {
        score + line_ply as i64
    } else if score < -CHECKMATE_THRESHOLD {
        score - line_ply as i64
    } else {
        score
    }
}

/// The inverse of score_to_tt: convert a stored mate score back to being
/// relative to the root of the current search.
fn score_from_tt(score: i64, line_ply: usize) -> i64 {
    if score > CHECKMATE_THRESHOLD {
        score - line_ply as i64
    } else if score < -CHECKMATE_THRESHOLD {
        score + line_ply as i64
    } else {
        score
    }
}

pub trait Engine {
    fn new(board: Board) -> Self;

    fn parse_fen(&mut self, fen_string: &str) -> Result<(), String>;

    fn should_stop(&self) -> bool;

    fn perft(&mut self);

    fn search(&mut self, depth: u8) -> Option<SearchResult>;

    //fn make_move(&mut self, play: &Play);

    fn make_move_str(&mut self, play: &str) -> bool;

    fn iterative_deepening_search(&mut self, search_options: SearchParameters) -> Option<Play> {
        let mut best_move: Option<Play> = None;
        let max_depth = match search_options.depth {
            Some(depth) => depth,
            None => MAX_DEPTH,
        };
        self.configure(search_options.start_time, search_options.search_duration);

        for depth in 1..=max_depth {
            let search_result = self.search(depth);
            if self.should_stop() {
                // Fall back to the interrupted iteration's move if we ran out
                // of time before the first iteration completed
                return match (best_move, search_result) {
                    (Some(play), _) => Some(play),
                    (None, result) => result.map(|r| r.best_move),
                };
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
        best_move
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
    pub fn with_table_bytes(board: Board, bytes: usize) -> Self {
        Self {
            board,
            nodes: 0,
            score: 0,
            moves: HashTable::with_capacity_bytes(bytes),
            search_depth: 0,
            selective_depth: 0,
            start_time: time::Instant::now(),
            search_duration: None,
            should_stop: false,
        }
    }

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
                let move_key = self.board.key;
                self.board.undo_move().unwrap();
                if self.should_stop {
                    // The search was aborted somewhere below us, so the score
                    // is meaningless and must not be stored or used.
                    // TODO return an error instead
                    return 0;
                }
                if score > alpha {
                    if score >= beta {
                        return beta;
                    }
                    alpha = score;
                    best_move = Some(*m);
                    best_board = Some(move_key);
                }
            }
        }

        if alpha != old_alpha {
            self.moves.set(
                self.board.key,
                Pv {
                    play: best_move.unwrap(),
                    next_key: best_board.unwrap(),
                    score: score_to_tt(alpha, self.board.line_ply),
                    depth: 0, // Never use a quiescence move instead of evaluating, only for move ordering
                    node: Node::Ordering,
                    ply: self.board.ply,
                },
            );
        }
        alpha
    }

    /// Look up the current position in the transposition table.
    ///
    /// Returns the stored best move (if any) which is always safe to use for
    /// move ordering, and a score when the stored entry is deep enough and its
    /// bound allows a cutoff at the current alpha/beta window.
    fn get_transposition(
        &self,
        key: u64,
        alpha: i64,
        beta: i64,
        depth: u8,
    ) -> (Option<Play>, Option<i64>) {
        let pv = self.moves.get(key);
        if let Some(pv) = pv {
            if pv.depth >= depth.into() {
                let score = score_from_tt(pv.score, self.board.line_ply);
                match pv.node {
                    Node::Exact => return (Some(pv.play), Some(score)),
                    Node::Alpha => {
                        if score <= alpha {
                            return (Some(pv.play), Some(score));
                        }
                    }
                    Node::Beta => {
                        if score >= beta {
                            return (Some(pv.play), Some(score));
                        }
                    }
                    Node::Ordering => (),
                }
            }
            return (Some(pv.play), None);
        }
        (None, None)
    }

    fn alpha_beta(&mut self, mut alpha: i64, beta: i64, mut depth: u8) -> i64 {
        if self.nodes % 3000 == 0 {
            self.check_if_should_stop();
        }
        self.selective_depth = self.selective_depth.max(self.board.line_ply as u8);
        self.nodes += 1;

        // a repetition at the root is not a finished game, the engine still has to move, so only
        // score it as a draw further down the line
        if self.board.fifty_move_rule >= 100
            || (self.board.line_ply > 0 && self.board.is_repetition())
        {
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
        let (pv_play, tt_score) = self.get_transposition(self.board.key, alpha, beta, depth);
        if let Some(tt_score) = tt_score {
            return tt_score;
        }

        let mut moves = self.board.generate_moves();
        moves.sort_by_cached_key(|m| {
            let mut score = m.mmv_lva(&self.board);
            if let Some(pv) = pv_play {
                if pv == *m {
                    score += 100_000;
                }
            };
            -(score as i64)
        });

        for m in &moves {
            if self.board.make_move(m) {
                found_legal_move = true;
                score = -self.alpha_beta(-beta, -alpha, depth - 1);
                let move_key = self.board.key;
                self.board.undo_move().unwrap();
                if self.should_stop {
                    // The search was aborted somewhere below us, so the score
                    // is meaningless and must not be stored or used.
                    // TODO return an error instead
                    return 0;
                }
                if score > alpha {
                    best_move = Some(m);
                    best_board = Some(move_key);
                    if score >= beta {
                        self.moves.set(
                            self.board.key,
                            Pv {
                                play: *m,
                                next_key: move_key,
                                depth: depth as usize,
                                score: score_to_tt(beta, self.board.line_ply),
                                node: Node::Beta,
                                ply: self.board.ply,
                            },
                        );
                        return beta;
                    }
                    alpha = score;
                }
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
                    score: score_to_tt(alpha, self.board.line_ply),
                    node: Node::Exact,
                    ply: self.board.ply,
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
    score: i64,
    depth: usize,
    node: Node,
    ply: usize,
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
    table: Vec<Option<(Pv, u64)>>,
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
        let entry_size = mem::size_of::<u64>() + mem::size_of::<Pv>();
        Self::with_capacity(bytes / entry_size)
    }

    fn get(&self, key: u64) -> Option<&Pv> {
        let index = (key % self.capacity as u64) as usize;
        if let Some((pv, k)) = &self.table[index] {
            if *k == key {
                return Some(pv);
            }
        }
        None
    }

    fn clear_key(&mut self, key: u64) {
        let index = (key % self.capacity as u64) as usize;
        self.table[index] = None;
    }

    fn set(&mut self, key: u64, pv: Pv) {
        let index = (key % self.capacity as u64) as usize;
        if let Some((old_pv, old_key)) = self.table[index] {
            // entries left over from an earlier point in the game are always replaced
            let stale = (pv.ply as isize - old_pv.ply as isize) > (MAX_DEPTH as isize + 3);
            if !stale {
                if pv.depth < old_pv.depth {
                    return;
                }
                if pv.depth == old_pv.depth
                    && old_key == key
                    && matches!(old_pv.node, Node::Exact)
                    && !matches!(pv.node, Node::Exact)
                {
                    return;
                }
            }
        }
        self.table[index] = Some((pv, key));
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
    use std::time;

    #[test]
    fn test_regression_bad_cache() {
        // This is a losing position but running a search on a previous position then the losing
        // position seems to cause hash/cache collisions in some cases.
        let game =
            Board::from_fen("r4rk1/pppb1ppp/4pn2/6N1/3P4/2qBP3/P4PPP/3R1R1K w - - 2 16").unwrap();
        let mut e = <AlphaBeta as Engine>::new(game);
        let result = e.search(7).unwrap();
        assert!(
            result.score < -800,
            "expect bad score (first) got {}",
            result.score
        );

        let game =
            Board::from_fen("r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12")
                .unwrap();
        let mut e = <AlphaBeta as Engine>::new(game);
        e.search(7).unwrap();
        let _ = e.parse_fen("r4rk1/pppb1ppp/4pn2/6N1/3P4/2qBP3/P4PPP/3R1R1K w - - 2 16");
        let result = e.search(7).unwrap();
        assert!(result.score < -800, "expect bad score got {}", result.score);
    }

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
    fn test_checkmate_in_2_white_warm_cache() {
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - - 0 0").unwrap();
        let mut e = <AlphaBeta as Engine>::new(game);
        let result = e.search(4).unwrap();
        assert_eq!(result.checkmate_in(), Some(2));
        // searching again deeper with a warm cache reuses mate scores stored
        // at different plies, the reported mate distance must not change
        let result = e.search(6).unwrap();
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

    #[test]
    fn test_warm_cache_matches_cold_search() {
        // Searching a position with a cache warmed by unrelated positions must give the same
        // result as searching it with an empty cache
        let fens = [
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 10 10",
            "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
        ];
        let mut warm = <AlphaBeta as Engine>::new(Board::new());
        for fen in fens {
            warm.parse_fen(fen).unwrap();
            warm.search(5).unwrap();
        }
        for fen in fens {
            let game = Board::from_fen(fen).unwrap();
            let mut cold = <AlphaBeta as Engine>::new(game);
            let expected = cold.search(5).unwrap();
            warm.parse_fen(fen).unwrap();
            let result = warm.search(5).unwrap();
            assert_eq!(result.score, expected.score, "score differs for {}", fen);
            assert_eq!(
                format!("{}", result.best_move),
                format!("{}", expected.best_move),
                "best move differs for {}",
                fen
            );
        }
    }

    #[test]
    fn test_small_table_matches_large_table() {
        // a table small enough to force constant collisions must not change the result
        let fen = "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12";
        let mut big = <AlphaBeta as Engine>::new(Board::from_fen(fen).unwrap());
        let expected = big.search(5).unwrap();
        let mut small = AlphaBeta::with_table_bytes(Board::from_fen(fen).unwrap(), 8 * 1024);
        let result = small.search(5).unwrap();
        assert_eq!(result.score, expected.score);
    }

    #[test]
    fn test_search_at_repetition_returns_a_move() {
        let game = Board::from_fen(
            "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 b - - 3 19",
        )
        .unwrap();
        let mut e = <AlphaBeta as Engine>::new(game);
        for m in ["a8b8", "a1b1", "b8a8", "b1a1", "a8b8", "a1b1", "b8a8", "b1a1"] {
            assert!(e.make_move_str(m), "failed to play {}", m);
        }
        assert!(e.board.is_repetition());
        assert!(e.search(3).is_some());
    }

    #[test]
    fn test_pv_line_without_cache_entry() {
        let game = Board::new();
        let e = <AlphaBeta as Engine>::new(game);
        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn test_stopped_search_does_not_poison_cache() {
        let fen = "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12";
        let game = Board::from_fen(fen).unwrap();
        let mut cold = <AlphaBeta as Engine>::new(game);
        let expected = cold.search(6).unwrap();

        // a search with no time budget stops immediately, it must not leave partial results in
        // the hash table which change the outcome of the next search
        let game = Board::from_fen(fen).unwrap();
        let mut e = <AlphaBeta as Engine>::new(game);
        e.configure(time::Instant::now(), Some(time::Duration::ZERO));
        e.search(6);
        assert!(e.should_stop());

        e.configure(time::Instant::now(), None);
        let result = e.search(6).unwrap();
        assert_eq!(result.score, expected.score);
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
        );
    }
}

#[cfg(test)]
mod test_hash_table {
    use super::{HashTable, Node, Play, Pv};
    use pretty_assertions::assert_eq;

    fn new_pv(node: Node, depth: usize, ply: usize) -> Pv {
        Pv {
            next_key: 0,
            play: Play::new(0, 1, None, None, false, false),
            score: 0,
            depth,
            node,
            ply,
        }
    }

    #[test]
    fn test_get_compares_key_not_just_slot() {
        // two different keys which map to the same slot must not be confused for each other
        let mut table = HashTable::with_capacity(1);
        table.set(1, new_pv(Node::Exact, 1, 1));
        assert!(table.get(1).is_some());
        assert!(table.get(2).is_none());
    }

    #[test]
    fn test_exact_entry_replaces_non_exact_entry() {
        let mut table = HashTable::with_capacity(1);
        table.set(1, new_pv(Node::Beta, 1, 1));
        table.set(1, new_pv(Node::Exact, 1, 1));
        assert!(matches!(table.get(1).unwrap().node, Node::Exact));
    }

    #[test]
    fn test_deeper_exact_entry_survives_shallower_exact_entry() {
        let mut table = HashTable::with_capacity(1);
        table.set(1, new_pv(Node::Exact, 8, 1));
        table.set(1, new_pv(Node::Exact, 2, 1));
        assert_eq!(table.get(1).unwrap().depth, 8);
    }

    #[test]
    fn test_deeper_entry_replaces_exact_entry_for_another_position() {
        let mut table = HashTable::with_capacity(1);
        table.set(1, new_pv(Node::Exact, 1, 1));
        table.set(2, new_pv(Node::Beta, 8, 1));
        assert!(table.get(2).is_some());
        assert!(table.get(1).is_none());
    }

    #[test]
    fn test_shallower_entry_does_not_evict_deeper_entry_for_another_position() {
        let mut table = HashTable::with_capacity(1);
        table.set(1, new_pv(Node::Beta, 8, 1));
        table.set(2, new_pv(Node::Exact, 1, 1));
        assert_eq!(table.get(1).unwrap().depth, 8);
    }

    #[test]
    fn test_quiescence_entry_does_not_evict_searched_entry() {
        let mut table = HashTable::with_capacity(1);
        table.set(1, new_pv(Node::Exact, 5, 1));
        table.set(2, new_pv(Node::Ordering, 0, 1));
        assert_eq!(table.get(1).unwrap().depth, 5);
    }

    #[test]
    fn test_stale_entry_is_replaced_regardless_of_depth() {
        let mut table = HashTable::with_capacity(1);
        table.set(1, new_pv(Node::Exact, 8, 1));
        table.set(2, new_pv(Node::Beta, 1, 100));
        assert!(table.get(2).is_some());
    }
}

impl Engine for AlphaBeta {
    fn new(board: Board) -> Self {
        AlphaBeta::with_table_bytes(board, DEFAULT_TABLE_BYTES)
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
        let mut line = Vec::new();
        let mut key = self.board.key;
        while let Some(pv) = self.moves.get(key) {
            line.push(pv.play);
            key = pv.next_key;
            if line.len() >= 16 {
                break; // TODO resolve hash colisions to prevent errors here
            }
        }
        PvLine { line }
    }
}
