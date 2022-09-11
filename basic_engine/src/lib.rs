#[macro_use]
extern crate lazy_static;

mod board;
mod engine;
mod misc;
mod play;
mod pvt;
mod zorbrist;

pub use board::Board;
pub use engine::{AlphaBeta, Engine, SearchParameters};
pub use misc::Color;
use std::fmt;

pub trait Game: fmt::Display {
    fn from_fen(fen: &str) -> Result<Self, String>
    where
        Self: std::marker::Sized;
}
