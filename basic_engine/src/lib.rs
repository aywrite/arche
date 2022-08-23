#[macro_use]
extern crate lazy_static;

mod board;
mod misc;

pub use board::Board;
pub use misc::Color;
use std::fmt;

pub trait Game: fmt::Display {
    fn from_fen(fen: String) -> Result<Self, String>
    where
        Self: std::marker::Sized;
}
