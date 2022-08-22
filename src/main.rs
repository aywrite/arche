#[macro_use]
extern crate lazy_static;

mod basic_engine;
mod lib;
use crate::basic_engine::Board;
use crate::lib::Game;

fn main() {
    //let game =
    //    Board::from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1".to_string())
    //        .unwrap();
    //let game = Board::from_fen("3k3p/1p4p1/8/8/8/P1P3P1/8/RNBQKBNR w KQkq - 0 1".to_string()).unwrap();
    //let game = Board::from_fen("8/8/8/8/3Q4/8/8/8 w KQkq - 0 1".to_string()).unwrap();
    let game = Board::from_fen("8/1N6/8/1Q6/8/1n6/8/8 w KQkq - 0 1".to_string()).unwrap();
    play(game);
}

fn play<G: Game>(game: G) {
    game.debug_print();
}
