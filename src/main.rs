mod params;
mod time_control;
mod uci;

pub use uci::UCI;

use basic_engine::AlphaBeta;
use basic_engine::Board;
use basic_engine::SearchConfig;
use basic_engine::bench;
use std::process::ExitCode;

fn main() -> ExitCode {
    // the one argument the binary takes: `arche bench [depth]` prints the
    // bench and exits, which is how the match tools measure an engine's
    // speed and how a commit states what its search change did to the tree.
    // No argument starts the uci loop; anything else is a mistake, and a
    // mistake that started the uci loop would sit waiting for input in
    // silence
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {
            let game = Board::new();
            let e = AlphaBeta::new(game);
            UCI::new_with_engine(e).read_loop();
            ExitCode::SUCCESS
        }
        Some("bench") => match uci::bench_depth(&params::Params::of(&args.join(" "))) {
            Ok(depth) => {
                println!(
                    "{}",
                    bench::run_suite(
                        &bench::positions(),
                        depth,
                        bench::TABLE_BYTES,
                        SearchConfig::default()
                    )
                );
                ExitCode::SUCCESS
            }
            Err(word) => {
                eprintln!("unrecognised bench depth: {}", word);
                ExitCode::from(2)
            }
        },
        Some(other) => {
            eprintln!("unrecognised argument: {}", other);
            ExitCode::from(2)
        }
    }
}
