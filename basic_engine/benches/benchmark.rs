use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use basic_engine::{AlphaBeta, Board, Color, Engine, Game, SearchParameters};

const TEST_POSITIONS: [&str; 4] = [
    "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1", // initial
    "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1", // Kiwipete
    "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 10 10",              // position 3
    "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1", // position 4
];

macro_rules! bench_board_fen {
    ($func:ident, $b:ident, $f:block) => {
        pub fn $func(c: &mut Criterion) {
            let mut group = c.benchmark_group(stringify!($func));
            for fen in TEST_POSITIONS {
                #[allow(unused_mut)] // Some callers will need mut, others will not
                let mut $b = black_box(Board::from_fen(fen).unwrap());
                group.bench_with_input(BenchmarkId::from_parameter(fen), fen, |d, _fen| {
                    d.iter(|| $f)
                });
            }
            group.finish();
        }
    };
}

bench_board_fen!(square_attacked, b, {
    for index in 0..64 {
        b.square_attacked(index, Color::Black);
    }
});

bench_board_fen!(generate_moves, b, {
    b.generate_moves();
});

bench_board_fen!(perft_3, b, {
    b.perft(3);
});

macro_rules! bench_engine_fen {
    ($func:ident, $e:ident, $f:block) => {
        pub fn $func(c: &mut Criterion) {
            let mut group = c.benchmark_group(stringify!($func));
            for fen in TEST_POSITIONS {
                let b = black_box(Board::from_fen(fen).unwrap());
                let mut $e = <AlphaBeta as Engine>::new(b.clone());
                group.significance_level(0.05).sample_size(50);
                group.bench_with_input(BenchmarkId::from_parameter(fen), fen, |d, _fen| {
                    d.iter(|| $f)
                });
            }
            group.finish();
        }
    };
}

bench_engine_fen!(alpha_beta_5, engine, {
    engine.clear_cache();
    engine.iterative_deepening_search(SearchParameters::new_with_depth(5))
});

criterion_group!(board_benches, square_attacked, generate_moves,);
criterion_group!(perft_benches, perft_3);
criterion_group!(search_benches, alpha_beta_5);
criterion_main!(board_benches, perft_benches, search_benches);
