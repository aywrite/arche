pub mod bench;
mod bitboard;
mod board;
mod engine;
mod limits;
mod magic;
mod misc;
mod play;
mod psqt;
mod transposition;
mod value;
mod zobrist;

pub use board::Board;
pub use engine::{
    AlphaBeta, Engine, PvLine, SearchConfig, SearchOutcome, SearchParameters, SearchResult,
};
pub use limits::Limits;
pub use misc::{Color, Score};
pub use play::Play;
pub use transposition::DEFAULT_TABLE_BYTES;

/// The starting position, so that nothing setting a board up from scratch has
/// to carry its own copy of the fen.
pub const STARTING_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
