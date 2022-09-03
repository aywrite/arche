#[macro_use]
extern crate lazy_static;

mod board;
mod misc;
mod play;
mod engine;

pub use board::Board;
pub use misc::Color;
pub use engine::{RandomEngine, Engine, SimpleEngine, AlphaBeta};
use std::fmt;

pub trait Game: fmt::Display {
    fn from_fen(fen: String) -> Result<Self, String>
    where
        Self: std::marker::Sized;
}
