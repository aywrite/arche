// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

pub mod bench;
mod bitboard;
mod board;
mod engine;
mod limits;
mod magic;
mod misc;
mod ordering;
mod play;
mod psqt;
pub mod tactics;
mod transposition;
mod value;
mod zobrist;

pub use board::Board;
pub use engine::{
    AlphaBeta, Engine, MAX_PLY, PvLine, SearchConfig, SearchOutcome, SearchParameters, SearchResult,
};
pub use limits::{Clock, Limits};
pub use misc::{Color, Score};
pub use play::Play;
pub use transposition::DEFAULT_TABLE_BYTES;

/// The starting position, so that nothing setting a board up from scratch has
/// to carry its own copy of the fen.
pub const STARTING_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
