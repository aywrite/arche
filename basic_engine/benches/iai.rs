use basic_engine::{AlphaBeta, Board, Engine, Game, SearchParameters};
use iai;

// TODO share these with criterion benches
const TEST_POSITIONS: [&str; 4] = [
    "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1", // initial
    "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1", // Kiwipete
    "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 10 10",              // position 3
    "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1", // position 4
];

fn iai_alpha_beta() {
    for fen in TEST_POSITIONS {
        let b = iai::black_box(Board::from_fen(fen).unwrap());
        let mut e = <AlphaBeta as Engine>::new(b.clone());
        e.clear_cache();
        e.iterative_deepening_search(SearchParameters::new_with_depth(5));
    }
}

iai::main!(iai_alpha_beta);
