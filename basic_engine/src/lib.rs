mod bitboard;
mod board;
mod engine;
mod magic;
mod misc;
mod play;
mod pvt;
mod zobrist;

pub use board::Board;
pub use engine::{AlphaBeta, Engine, PvLine, SearchOutcome, SearchParameters, SearchResult};
pub use misc::{Color, Score};
pub use play::Play;
use std::fmt;

pub trait Game: fmt::Display {
    fn from_fen(fen: &str) -> Result<Self, String>
    where
        Self: std::marker::Sized;
}
