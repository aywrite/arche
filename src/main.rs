//extern crate basic_engine;

use basic_engine::Board;
use basic_engine::Game;

fn main() {
    //let game =
    //    Board::from_fen("r3kbnr/pppppppp/8/8/8/8/PPPPPPPP/R3KBNR w KQkq - 0 1".to_string())
    //        .unwrap();
    //let game = Board::from_fen("3k3p/1p4p1/8/8/8/P1P3P1/8/RNBQKBNR w KQkq - 0 1".to_string()).unwrap();
    let game = Board::from_fen("rnbqkbnr/pp1ppppp/8/2p5/3Pp3/8/PPPP1PpP/RNBQKB1R b KQkq e5 0 2".to_string()).unwrap();
    //let game = Board::from_fen("r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10".to_string()).unwrap();
    //let game = Board::from_fen("8/8/8/8/3Q4/8/8/8 w KQkq - 0 1".to_string()).unwrap();
    //let game = Board::from_fen("8/1N6/8/1Q6/8/1n6/8/8 w KQkq - 0 1".to_string()).unwrap();
    for play in game.generate_moves() {
        println!("{}", play)
    }
    play(game); // TODO stop this moving
}

fn play<G: Game>(game: G) {
    println!("{}", game);
}
