mod basic_engine;
mod lib;
use crate::basic_engine::Board;
use crate::lib::Game;

fn main() {
    let game =
        Board::from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1".to_string())
            .unwrap();
    play(game);
}

fn play<G: Game>(game: G) {
    game.debug_print();
}
