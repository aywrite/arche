#[macro_use]
extern crate lazy_static;

mod bitboard;
mod board;
mod engine;
mod magic;
mod misc;
mod play;
mod pvt;
mod zorbrist;

pub use board::Board;
pub use engine::{
    AlphaBeta, DEFAULT_TABLE_MB, Engine, MAX_TABLE_MB, MIN_TABLE_MB, PvLine, SearchOutcome,
    SearchParameters, SearchResult, table_mb_for,
};
pub use misc::{Color, Score};
pub use play::Play;
use std::fmt;

pub trait Game: fmt::Display {
    fn from_fen(fen: &str) -> Result<Self, String>
    where
        Self: std::marker::Sized;
}
