use crate::Game;
use crate::board::Board;
use crate::misc::{Color, Score};
use crate::play::Play;
use std::fmt;
use std::mem;
use std::time;

const CHECKMATE_SCORE: Score = 30_000;
const MAX_DEPTH: u8 = 20;
const DEFAULT_TABLE_BYTES: usize = 500 * 1024 * 1024;
// Any score this close to CHECKMATE_SCORE is a forced mate. Regular evals are
// bounded by the material on the board, which cannot come near it.
const CHECKMATE_THRESHOLD: Score = CHECKMATE_SCORE - 1000;

/// Convert a score to its transposition table form. Mate scores are stored
/// relative to the node they are stored at (plies-to-mate from this node)
/// rather than relative to the root of the search, so that they remain correct
/// when the entry is reused at a different distance from the root.
fn score_to_tt(score: Score, line_ply: usize) -> Score {
    if score > CHECKMATE_THRESHOLD {
        score + line_ply as Score
    } else if score < -CHECKMATE_THRESHOLD {
        score - line_ply as Score
    } else {
        score
    }
}

/// The inverse of score_to_tt: convert a stored mate score back to being
/// relative to the root of the current search.
fn score_from_tt(score: Score, line_ply: usize) -> Score {
    if score > CHECKMATE_THRESHOLD {
        score - line_ply as Score
    } else if score < -CHECKMATE_THRESHOLD {
        score + line_ply as Score
    } else {
        score
    }
}

pub trait Engine {
    fn new(board: Board) -> Self;

    fn parse_fen(&mut self, fen_string: &str) -> Result<(), String>;

    /// Forget what was learned from the game just finished. Stored scores do not
    /// account for repetition or the fifty move counter, so a position that
    /// comes up again in a new game would otherwise be scored from a line that
    /// no longer applies to it.
    fn new_game(&mut self);

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

impl Default for SearchParameters {
    fn default() -> Self {
        Self::new()
    }
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
    score: Score,
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

    fn eval(&self) -> Score {
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

    fn quiescence(&mut self, mut alpha: Score, beta: Score) -> Score {
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
        let old_alpha = alpha;
        let mut score: Score;
        let pv_line = self.moves.get(self.board.key);
        let mut moves = self.board.generate_captures();
        moves.sort_by_cached_key(|m| {
            let mut score = m.mmv_lva(&self.board);
            if let Some(pv) = pv_line {
                if pv.play == *m {
                    score += 100000;
                }
            };
            -score
        });

        for m in &moves {
            if self.board.make_move(m) {
                score = -self.quiescence(-beta, -alpha);
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
                }
            }
        }

        if alpha != old_alpha {
            self.moves.set(
                self.board.key,
                Pv {
                    play: best_move.unwrap(),
                    score: score_to_tt(alpha, self.board.line_ply),
                    depth: 0, // Never use a quiescence move instead of evaluating, only for move ordering
                    node: Node::Ordering,
                    ply: self.board.ply as u16,
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
        alpha: Score,
        beta: Score,
        depth: u8,
    ) -> (Option<Play>, Option<Score>) {
        let pv = self.moves.get(key);
        if let Some(pv) = pv {
            if pv.depth >= depth {
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

    fn alpha_beta(&mut self, mut alpha: Score, beta: Score, mut depth: u8) -> Score {
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
        let mut score: Score;
        let mut found_legal_move = false;
        let mut best_move: Option<&Play> = None;
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
            -score
        });

        for m in &moves {
            if self.board.make_move(m) {
                found_legal_move = true;
                score = -self.alpha_beta(-beta, -alpha, depth - 1);
                self.board.undo_move().unwrap();
                if self.should_stop {
                    // The search was aborted somewhere below us, so the score
                    // is meaningless and must not be stored or used.
                    // TODO return an error instead
                    return 0;
                }
                if score > alpha {
                    best_move = Some(m);
                    if score >= beta {
                        self.moves.set(
                            self.board.key,
                            Pv {
                                play: *m,
                                depth,
                                score: score_to_tt(beta, self.board.line_ply),
                                node: Node::Beta,
                                ply: self.board.ply as u16,
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
                return -CHECKMATE_SCORE + (self.board.line_ply as Score);
            }
            return 0;
        }

        if alpha != old_alpha {
            self.moves.set(
                self.board.key,
                Pv {
                    play: *best_move.unwrap(),
                    depth,
                    score: score_to_tt(alpha, self.board.line_ply),
                    node: Node::Exact,
                    ply: self.board.ply as u16,
                },
            );
        }
        alpha
    }
}

#[derive(Copy, Clone, Debug)]
struct Pv {
    play: Play,
    score: Score,
    // a depth cannot exceed MAX_DEPTH and a ply is bounded by the history
    // array, so neither needs a word. Both sit in the padding the key leaves
    // behind, which is why widening ply to u16 costs nothing.
    depth: u8,
    node: Node,
    ply: u16,
}

#[derive(Copy, Clone, Debug)]
// TODO better name for this
enum Node {
    Exact,
    // fail low nodes are not stored yet, see the known issues in the readme
    #[allow(dead_code)]
    Alpha,
    Beta,
    Ordering,
}

/// A slot in the table. The key is kept alongside the entry so that a probe can
/// tell a real hit from another position landing on the same index.
type Entry = Option<(Pv, u64)>;

#[derive(Debug)]
struct HashTable {
    table: Vec<Entry>,
    capacity: usize,
}

impl HashTable {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            table: vec![None; capacity],
            capacity,
        }
    }

    fn clear(&mut self) {
        self.table.fill(None);
    }

    fn with_capacity_bytes(bytes: usize) -> Self {
        // ask for the size of what is actually stored. Adding up the fields
        // gives the same answer today only because Pv happens to need no
        // padding on the end of it, and would quietly over allocate if that
        // stopped being true.
        Self::with_capacity(bytes / mem::size_of::<Entry>())
    }

    #[inline]
    fn index_for(&self, key: u64) -> usize {
        // multiply-shift: maps key uniformly onto 0..capacity without a 64 bit
        // division on every probe
        (((key as u128) * (self.capacity as u128)) >> 64) as usize
    }

    fn get(&self, key: u64) -> Option<&Pv> {
        let index = self.index_for(key);
        if let Some((pv, k)) = &self.table[index] {
            if *k == key {
                return Some(pv);
            }
        }
        None
    }

    fn clear_key(&mut self, key: u64) {
        let index = self.index_for(key);
        self.table[index] = None;
    }

    fn set(&mut self, key: u64, pv: Pv) {
        let index = self.index_for(key);
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
    score: Score,        // The estimated score for the best move if played
}

impl SearchResult {
    fn checkmate_in(&self) -> Option<Score> {
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
    use super::{Node, Play, Pv};
    use pretty_assertions::assert_eq;
    use std::time;

    /// The default table is half a gigabyte, which is the right size to play
    /// with and the wrong size to test with: one per test dominated both the
    /// memory and the run time of the suite. This is still far larger than
    /// anything here searches deeply enough to fill.
    const TABLE_BYTES: usize = 16 * 1024 * 1024;

    fn engine(board: Board) -> AlphaBeta {
        AlphaBeta::with_table_bytes(board, TABLE_BYTES)
    }

    /// The move of this name in this position, so that a test can name a line
    /// the way the rest of the world does.
    fn play_named(board: &Board, name: &str) -> Play {
        *board
            .generate_moves()
            .iter()
            .find(|m| format!("{}", m) == name)
            .unwrap_or_else(|| panic!("{} is not a move here", name))
    }

    /// An entry of the kind a completed search leaves behind.
    fn searched(play: Play, ply: usize) -> Pv {
        Pv {
            play,
            score: 0,
            depth: 5,
            node: Node::Exact,
            ply: ply as u16,
        }
    }

    #[test]
    fn test_regression_bad_cache() {
        // This is a losing position but running a search on a previous position then the losing
        // position seems to cause hash/cache collisions in some cases.
        let game =
            Board::from_fen("r4rk1/pppb1ppp/4pn2/6N1/3P4/2qBP3/P4PPP/3R1R1K w - - 2 16").unwrap();
        let mut e = engine(game);
        let result = e.search(7).unwrap();
        assert!(
            result.score < -800,
            "expect bad score (first) got {}",
            result.score
        );

        let game =
            Board::from_fen("r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12")
                .unwrap();
        let mut e = engine(game);
        e.search(7).unwrap();
        let _ = e.parse_fen("r4rk1/pppb1ppp/4pn2/6N1/3P4/2qBP3/P4PPP/3R1R1K w - - 2 16");
        let result = e.search(7).unwrap();
        assert!(result.score < -800, "expect bad score got {}", result.score);
    }

    #[test]
    fn test_checkmate_in_2_white() {
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - - 0 0").unwrap();
        let mut e = engine(game);
        let result = e.search(4).unwrap();
        assert_eq!(result.checkmate_in(), Some(2));
        assert_eq!(format!("{}", result.best_move), "g3g6");
    }

    #[test]
    fn test_checkmate_in_2_white_warm_cache() {
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - - 0 0").unwrap();
        let mut e = engine(game);
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
        let mut e = engine(game);
        let result = e.search(4).unwrap();
        assert_eq!(result.checkmate_in(), Some(-1));
    }

    #[test]
    fn test_fifty_move_rule_play_for_draw() {
        // white is down material in this position so should play for fifty move draw
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 99 112").unwrap();
        let mut e = engine(game);
        let result = e.search(3).unwrap();
        assert_eq!(result.score, 0);
    }

    #[test]
    fn test_fifty_move_rule_no_legal_moves() {
        // The fifty move rules has been triggered - there should not be any legal moves
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 100 112").unwrap();
        let mut e = engine(game);
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
        let mut warm = engine(Board::new());
        for fen in fens {
            warm.parse_fen(fen).unwrap();
            warm.search(5).unwrap();
        }
        for fen in fens {
            let game = Board::from_fen(fen).unwrap();
            let mut cold = engine(game);
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
        let mut big = engine(Board::from_fen(fen).unwrap());
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
        let mut e = engine(game);
        for m in [
            "a8b8", "a1b1", "b8a8", "b1a1", "a8b8", "a1b1", "b8a8", "b1a1",
        ] {
            assert!(e.make_move_str(m), "failed to play {}", m);
        }
        assert!(e.board.is_repetition());
        assert!(e.search(3).is_some());
    }

    #[test]
    fn test_new_game_forgets_the_previous_game() {
        let fen = "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12";
        let mut e = engine(Board::from_fen(fen).unwrap());
        e.search(4).unwrap();
        assert!(e.moves.get(e.board.key).is_some(), "nothing was stored");
        assert_ne!(format!("{}", e.pv_line()), "");

        e.new_game();
        assert!(e.moves.get(e.board.key).is_none());
        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn test_pv_line_without_cache_entry() {
        let game = Board::new();
        let e = engine(game);
        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn test_pv_line_stops_at_a_repetition() {
        // a shuffle both sides are content with leaves the table holding a line
        // that goes round for ever. The game is over the third time the
        // position comes up, so the line has to stop there rather than report a
        // continuation nobody would be allowed to play.
        let game = Board::from_fen(
            "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 b - - 3 19",
        )
        .unwrap();
        let mut e = engine(game);
        let cycle = ["a8b8", "a1b1", "b8a8", "b1a1"];
        let mut board = e.board;
        for name in cycle.iter().cycle().take(16) {
            let play = play_named(&board, name);
            e.moves.set(board.key, searched(play, board.ply));
            assert!(board.make_move(&play), "failed to play {}", name);
        }

        assert_eq!(
            format!("{}", e.pv_line()),
            "a8b8 a1b1 b8a8 b1a1 a8b8 a1b1 b8a8 b1a1"
        );
    }

    #[test]
    fn test_pv_line_stops_when_the_fifty_move_counter_runs_out() {
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 99 112").unwrap();
        let mut e = engine(game);
        let mut board = e.board;
        for name in ["c3d4", "f8g8"] {
            let play = play_named(&board, name);
            e.moves.set(board.key, searched(play, board.ply));
            assert!(board.make_move(&play), "failed to play {}", name);
        }
        assert_eq!(board.fifty_move_rule, 101);

        // the first move draws by the fifty move rule, so the reply the table
        // holds is one the game never gets to
        assert_eq!(format!("{}", e.pv_line()), "c3d4");
    }

    #[test]
    fn test_pv_line_does_not_follow_a_move_which_is_illegal_here() {
        // two positions which hash to the same key share an entry, so the move
        // a probe comes back with is not always a move of the position asked
        // about
        let mut e = engine(Board::new());
        let a2 = 8;
        let a5 = 32;
        let colliding = Play::new(a2, a5, None, None, false, false);
        e.moves.set(e.board.key, searched(colliding, e.board.ply));

        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn test_pv_line_does_not_follow_a_quiescence_entry() {
        // quiescence looks at captures alone, so its move is fit for ordering
        // the next search and not for saying what the engine means to play
        let mut e = engine(Board::new());
        let play = play_named(&e.board, "e2e4");
        e.moves.set(
            e.board.key,
            Pv {
                play,
                score: 0,
                depth: 0,
                node: Node::Ordering,
                ply: 0,
            },
        );

        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn test_pv_line_does_not_follow_a_move_which_leaves_the_king_in_check() {
        // moves are generated pseudo legally, so a pinned piece's move is in
        // the list for this position and still cannot be played. Asking whether
        // the move belongs to this position is not enough on its own, which is
        // why the walk goes on to check that making it succeeds.
        let board = Board::from_fen("4r2k/8/8/8/8/8/4N3/4K3 w - - 0 1").unwrap();
        let mut e = engine(board);
        let pinned = play_named(&e.board, "e2d4");
        e.moves.set(e.board.key, searched(pinned, e.board.ply));

        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn test_pv_line_is_bounded_by_the_search_depth() {
        // pawns only, so no position comes up twice and the fifty move counter
        // keeps being reset: nothing stops this line but the bound
        let mut names = Vec::new();
        for file in "abcdefgh".chars() {
            names.push(format!("{file}2{file}3"));
            names.push(format!("{file}7{file}6"));
        }
        for file in "abcdefgh".chars() {
            names.push(format!("{file}3{file}4"));
            names.push(format!("{file}6{file}5"));
        }

        let mut e = engine(Board::new());
        let mut board = e.board;
        for name in &names {
            let play = play_named(&board, name);
            e.moves.set(board.key, searched(play, board.ply));
            assert!(board.make_move(&play), "failed to play {}", name);
        }

        assert!(names.len() > super::MAX_DEPTH as usize);
        assert_eq!(e.pv_line().line.len(), super::MAX_DEPTH as usize);
    }

    #[test]
    fn test_stopped_search_does_not_poison_cache() {
        let fen = "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12";
        let game = Board::from_fen(fen).unwrap();
        let mut cold = engine(game);
        let expected = cold.search(6).unwrap();

        // a search with no time budget stops immediately, it must not leave partial results in
        // the hash table which change the outcome of the next search
        let game = Board::from_fen(fen).unwrap();
        let mut e = engine(game);
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

    fn new_pv(node: Node, depth: u8, ply: u16) -> Pv {
        Pv {
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

    fn new_game(&mut self) {
        self.clear_cache();
    }

    fn search(&mut self, depth: u8) -> Option<SearchResult> {
        self.nodes = 0;
        self.search_depth = depth;
        self.selective_depth = depth;
        self.board.line_ply = 0;
        self.score = self.alpha_beta(Score::MIN + 1, Score::MAX - 1, depth);
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

    /// Replay the line the table holds on a copy of the board, one stored move
    /// at a time. Walking the positions rather than following a key from one
    /// entry to the next is what lets the line be checked as it is built: the
    /// board says whether a stored move is legal here, and whether the line has
    /// reached a position it would be a draw to play on from.
    fn pv_line(&self) -> PvLine {
        let mut line = Vec::new();
        // Board is Copy, so the search's own board is untouched by this.
        let mut board = self.board;
        while line.len() < MAX_DEPTH as usize {
            let Some(pv) = self.moves.get(board.key) else {
                break;
            };
            if matches!(pv.node, Node::Ordering) {
                // written by quiescence, which looks at captures alone, so the
                // move is fit for ordering the next search and not for telling
                // anyone what the engine intends to play
                break;
            }
            let play = pv.play;
            // a probe compares the whole key, so what still gets through is
            // another position which hashed to the same one. Its move belongs
            // to that position, and playing it here would print a line the
            // rules do not allow
            if !board.generate_moves().contains(&play) {
                break;
            }
            if !board.make_move(&play) {
                break;
            }
            line.push(play);
            // the line is a draw from here, so whatever the table says comes
            // next is a continuation that would never be played
            if board.fifty_move_rule >= 100 || board.is_repetition() {
                break;
            }
        }
        PvLine { line }
    }
}

#[cfg(test)]
mod test_node_counts {
    use super::AlphaBeta;
    use super::Board;
    use super::Engine;
    use super::Game;
    use pretty_assertions::assert_eq;

    /// Pinned, because how often the transposition table collides decides how
    /// much of the tree is searched again.
    const TABLE_BYTES: usize = 1 << 20;

    /// Positions chosen to reach different parts of the search: a quiet
    /// opening, a tactical middlegame, a pawn endgame, a position full of
    /// captures, and one with castling and promotions available.
    const POSITIONS: [(&str, &str, u8); 5] = [
        (
            "opening",
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
            6,
        ),
        (
            "kiwipete",
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            5,
        ),
        (
            "pawn endgame",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
            7,
        ),
        (
            "promotions",
            "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
            5,
        ),
        (
            "middlegame",
            "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
            5,
        ),
    ];

    /// Widens the way the engine does when it plays, so the table is warm from
    /// the previous iteration the way it is in a real search, and totals what
    /// each iteration visited.
    fn nodes(fen: &str, depth: u8) -> u64 {
        let mut engine = AlphaBeta::with_table_bytes(Board::from_fen(fen).unwrap(), TABLE_BYTES);
        (1..=depth)
            .map(|d| engine.search(d).map_or(0, |result| result.nodes))
            .sum()
    }

    /// The search is deterministic, so how many nodes it visits is an exact
    /// figure rather than a timing, and it says the same thing on any machine.
    /// It moves whenever move ordering, quiescence, the transposition table or
    /// any pruning changes, including the many such changes that leave the move
    /// finally played untouched, which is what makes it worth pinning.
    ///
    /// A deliberate change to the search is expected to move these. Update them
    /// in the same commit: the diff is then a statement of how much less, or
    /// more, of the tree the engine now looks at.
    #[test]
    fn test_node_counts_have_not_moved() {
        let counted: Vec<(&str, u64)> = POSITIONS
            .iter()
            .map(|(name, fen, depth)| (*name, nodes(fen, *depth)))
            .collect();
        assert_eq!(
            counted,
            vec![
                ("opening", 171_858),
                ("kiwipete", 218_290),
                ("pawn endgame", 180_219),
                ("promotions", 120_717),
                ("middlegame", 199_263),
            ]
        );
    }
}
