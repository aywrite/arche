//extern crate basic_engine;
use rand::seq::IteratorRandom;

use basic_engine::Board;
use basic_engine::Game;

fn main() {
    let mut rng = rand::thread_rng();
    //let game =
    //    Board::from_fen("r3kbnr/pppppppp/8/8/8/8/PPPPPPPP/R3KBNR w KQkq - 0 1".to_string())
    //        .unwrap();
    //let game = Board::from_fen("3k3p/1p4p1/8/8/8/P1P3P1/8/RNBQKBNR w KQkq - 0 1".to_string()).unwrap();
    //let mut game = Board::from_fen(
    //    "rnbqkbnr/pp1ppppp/8/2p5/3Pp3/8/PPPP1PpP/RNBQKB1R b KQkq e5 0 2".to_string(),
    //)
    //.unwrap();
    // THIS IS THE TEST CASE BELOW
    let mut game = Board::from_fen(
        //"r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1".to_string(),
        //"r3k2r/4qpb1/8/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1".to_string(),
        "8/2p5/3p4/KP5r/5R1k/8/4P1P1/8 b - - 0 1".to_string(),
    )
    .unwrap();
    //let game = Board::from_fen("r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10".to_string()).unwrap();
    //let mut game = Board::from_fen("r3k2r/8/8/8/8/8/8/R3K2R b KQkq - 0 1".to_string()).unwrap();
    //let game = Board::from_fen("8/1N6/8/1Q6/8/1n6/8/8 w KQkq - 0 1".to_string()).unwrap();
    println!("Before {}", game);
    let moves = game.generate_moves();
    for m in moves {
        println!("({})", &m);
        if game.make_move(&m) {
            println!("Legal {}", game);
            game.undo_move().unwrap();
        } else {
            println!("Illegal {}", game);
        }

    }
    //let play = moves.iter().choose(&mut rng).unwrap();
    //game.make_move(play);
    //println!("after ({})", play);
    //println!("{}", game);
    ////play(game); // TODO stop this moving
    //let moves = game.generate_moves();
    //let play = moves.iter().choose(&mut rng).unwrap();
    //game.make_move(play);
    //println!("after ({})", play);
    //println!("{}", game);

    //let moves = game.generate_moves();
    //let play = moves.iter().choose(&mut rng).unwrap();
    //game.make_move(play);
    //println!("after ({})", play);
    //println!("{}", game);
}

fn play<G: Game>(game: G) {
    println!("{}", game);
}
