pub mod bench;
mod bitboard;
mod board;
mod engine;
mod magic;
mod misc;
mod play;
mod psqt;
mod transposition;
mod zobrist;

pub use board::Board;
pub use engine::{
    AlphaBeta, Engine, PvLine, SearchConfig, SearchOutcome, SearchParameters, SearchResult,
};
pub use misc::{Color, Score};
pub use play::Play;
use std::fmt;

/// The starting position, so that nothing setting a board up from scratch has
/// to carry its own copy of the fen.
pub const STARTING_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

pub trait Game: fmt::Display {
    fn from_fen(fen: &str) -> Result<Self, String>
    where
        Self: std::marker::Sized;
}
