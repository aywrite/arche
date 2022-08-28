//extern crate basic_engine;
use rand::seq::IteratorRandom;

use basic_engine::Board;
use basic_engine::Game;

fn main() {
    let mut game = Board::from_fen(
        "r3k2r/Pppp1ppp/1b3nbN/1PB5/B1P1P3/qn3N2/Pp1P2PP/1R1Q1RK1 b kq - 3 2".to_string(),
    )
    .unwrap();
    game.perft(3);
}

fn play<G: Game>(game: G) {
    println!("{}", game);
}
