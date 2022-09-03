//extern crate basic_engine;

use basic_engine::Board;
use basic_engine::Game;
use basic_engine::{Engine, AlphaBeta};

fn main() {
    let game = Board::new();
    //let game = Board::from_fen("6k1/3b3r/1p1p4/p1n2p2/1PPNpP1q/P3Q1p1/1R1RB1P1/5K2 b - - 0 1".to_string()).unwrap();
    //let game = Board::from_fen("2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - - 0 1".to_string()).unwrap();
    let mut e = <AlphaBeta as Engine>::new(game);
    loop {
        let m = e.iterative_deepening_search(7);
        e.make_move(&m);
        println!("Played {}:\n{}", m, e.board);
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
    }
}
