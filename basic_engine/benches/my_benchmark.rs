use criterion::{black_box, criterion_group, criterion_main, Criterion};

use basic_engine::{Board, Color, Game};

pub fn criterion_benchmark(c: &mut Criterion) {
    let b = black_box(
        Board::from_fen("3k3p/1p4p1/8/8/8/P1P3P1/8/RNBQKBNR w KQkq - 0 1".to_string()).unwrap(),
    );
    c.bench_function("square_attacked", |d| {
        d.iter(|| {
            for index in 0..64 {
                b.square_attacked(index, Color::Black);
            }
        })
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
