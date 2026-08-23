pub mod bench;
mod bitboard;
mod board;
mod engine;
mod magic;
mod misc;
mod play;
mod psqt;
mod zobrist;

pub use board::Board;
pub use engine::{
    AlphaBeta, Engine, PvLine, SearchConfig, SearchOutcome, SearchParameters, SearchResult,
};
pub use misc::{Color, Score};
pub use play::Play;
use std::fmt;

pub trait Game: fmt::Display {
    fn from_fen(fen: &str) -> Result<Self, String>
    where
        Self: std::marker::Sized;
}
