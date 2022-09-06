mod uci;

pub use uci::UCI;

#[macro_use]
extern crate lazy_static;

use basic_engine::Board;
use basic_engine::{AlphaBeta, Engine};

fn main() {
    let game = Board::new();
    let e = <AlphaBeta as Engine>::new(game);
    UCI::new_with_engine(e).read_loop();
}
