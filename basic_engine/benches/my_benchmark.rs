use criterion::{black_box, criterion_group, criterion_main, Criterion};

use basic_engine::{Board, Color, Game};

pub fn attacked_benchmark(c: &mut Criterion) {
    let b = black_box(
        Board::from_fen("3k3p/1p4p1/8/8/8/P1P3P1/8/RNBQKBNR w KQkq - 0 1".to_string()).unwrap(),
    );
    c.bench_function("square_attacked_1", |d| {
        d.iter(|| {
            for index in 0..64 {
                b.square_attacked(index, Color::Black);
            }
        })
    });
}

pub fn generate_moves_benchmark(c: &mut Criterion) {
    let b = black_box(
        Board::from_fen("3k3p/1p4p1/8/8/8/P1P3P1/8/RNBQKBNR w KQkq - 0 1".to_string()).unwrap(),
    );
    c.bench_function("generate_moves_1", |d| {
        d.iter(|| {
            b.generate_moves();
        })
    });
}

criterion_group!(benches, attacked_benchmark, generate_moves_benchmark);
criterion_main!(benches);
