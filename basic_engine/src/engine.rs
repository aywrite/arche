use crate::board::Board;
use crate::misc::Color;
use crate::play::Play;
use rand::seq::SliceRandom;
use std::collections::HashMap;

const CHECKMATE_SCORE: i64 = 888000;

pub trait Engine {
    fn new(board: Board) -> Self;

    fn search(&mut self, depth: u8) -> Option<Play>;

    fn make_move(&mut self, play: &Play);

    fn iterative_deepening_search(&mut self, max_depth: u8) -> Play {
        // TODO add init search call which sets nodes to 0
        let mut best_move: Option<Play> = None;
        let mut best_score: i64;

        for depth in 1..=max_depth {
            best_move = self.search(depth);
            //println!("Depth: {} Move: {}", depth, best_move.unwrap());
            //println!(
            //    "Depth: {} Score: {}, Move: {}, Nodes {}",
            //    depth,
            //    best_score,
            //    best_move.unwrap(),
            //    self.nodes
            //);
        }
        best_move.unwrap()
    }
}

pub struct RandomEngine {
    pub board: Board,
}

impl Engine for RandomEngine {
    fn new(board: Board) -> Self {
        Self { board }
    }

    fn search(&mut self, depth: u8) -> Option<Play> {
        // TODO make this not mut
        let mut moves = self.board.generate_moves();
        moves.shuffle(&mut rand::thread_rng());
        for m in moves.iter() {
            if self.board.make_move(m) {
                self.board.undo_move().unwrap();
                return Some(*m);
            }
        }
        None
    }

    fn make_move(&mut self, play: &Play) {
        self.board.make_move(play);
    }
}

pub struct SimpleEngine {
    pub board: Board,
}

impl Engine for SimpleEngine {
    fn new(board: Board) -> Self {
        Self { board }
    }

    fn search(&mut self, depth: u8) -> Option<Play> {
        // TODO make this not mut
        let mut best_score = 0;
        let mut best_move: Option<&Play> = None;

        let moves = self.board.generate_moves();
        for m in moves.iter() {
            if self.board.make_move(m) {
                // TODO switch on color instead of using abs
                if self.board.white_value >= best_score {
                    best_score = self.board.white_value;
                    best_move = Some(m);
                }
                self.board.undo_move().unwrap();
            }
        }
        if let Some(play) = best_move {
            return Some(*play);
        }
        None
    }

    fn make_move(&mut self, play: &Play) {
        self.board.make_move(play);
    }
}

pub struct AlphaBeta {
    pub board: Board,
    nodes: u64,
    score: i64,
    moves: HashMap<Board, Pv>,
}

impl AlphaBeta {
    //fn iterative_deepening_search(&mut self) -> Play {
    //    self.nodes = 0;
    //    let mut best_move: Option<&Play> = None;
    //    let mut best_score: i64;

    //    for depth in 1..=AlphaBeta::MAX_DEPTH {
    //        best_score = self.alpha_beta(i64::MIN + 1, i64::MAX - 1, depth);
    //        best_move = Some(self.moves.get(&self.board).unwrap());
    //        //println!(
    //        //    "Depth: {} Score: {}, Move: {}, Nodes {}",
    //        //    depth,
    //        //    best_score,
    //        //    best_move.unwrap(),
    //        //    self.nodes
    //        //);
    //    }
    //    *best_move.unwrap()
    //}

    fn eval(&self) -> i64 {
        self.board.white_value as i64 - self.board.black_value as i64
    }

    fn alpha_beta(&mut self, old_alpha: i64, beta: i64, depth: u8) -> i64 {
        if depth == 0 {
            self.nodes += 1;
            match self.board.active_color {
                Color::White => return self.eval(),
                Color::Black => return -self.eval(),
            }
        }
        self.nodes += 1;

        let mut alpha = old_alpha;
        let mut score: i64;
        let mut found_legal_move = false;
        let mut best_move: Option<&Play> = None;
        let mut moves = self.board.generate_moves();
        let mut best_board: Option<Board> = None;
        moves.sort_unstable_by_key(|m| m.mmv_lva(&self.board));

        for m in moves.iter().rev() {
            if self.board.make_move(m) {
                found_legal_move = true;
                score = -self.alpha_beta(-beta, -alpha, depth - 1);
                let next_board = Some(self.board.clone());
                self.board.undo_move().unwrap();
                if score > alpha {
                    if score >= beta {
                        return beta;
                    }
                    alpha = score;
                    best_move = Some(m);
                    best_board = next_board;
                }
            }
        }

        if !found_legal_move {
            if self.board.is_king_attacked() {
                return -CHECKMATE_SCORE + self.board.ply as i64;
            }
            return 0;
        }

        if alpha != old_alpha {
            self.moves.insert(
                self.board.clone(),
                Pv {
                    play: *best_move.unwrap(),
                    next_board: best_board.unwrap(),
                },
            );
        }
        alpha
    }

    fn get_line(&self) {
        let mut pv = self.moves.get(&self.board).unwrap();
        print!("PV: {}", pv.play);
        loop {
            if let Some(next) = self.moves.get(&pv.next_board) {
                print!("->{}", next.play);
                pv = next;
            } else {
                println!();
                break;
            }
        }
    }
}

struct Pv {
    next_board: Board,
    play: Play,
}

impl Engine for AlphaBeta {
    fn new(board: Board) -> Self {
        Self {
            board,
            nodes: 0,
            score: 0,
            moves: HashMap::new(),
        }
    }

    fn search(&mut self, depth: u8) -> Option<Play> {
        self.nodes = 0;
        self.score = self.alpha_beta(i64::MIN + 1, i64::MAX - 1, depth);
        let pv = self.moves.get(&self.board).unwrap();
        println!(
            "Depth: {} Score: {}, Move: {}, Nodes {}",
            depth, self.score, pv.play, self.nodes
        );
        self.get_line();
        Some(pv.play)
    }

    fn make_move(&mut self, play: &Play) {
        self.board.make_move(play);
    }
}
