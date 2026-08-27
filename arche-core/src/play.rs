// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use crate::misc::index_to_coordinate;
use crate::misc::{Piece, PromotePiece};
use std::fmt;

/// A move. Six bytes, which is what the fields happen to come to and what the
/// search is tuned around: a move is copied into a move list, out of it, into
/// the history and into the transposition table, several times per node.
/// Widening it has been measured twice and was slower both times.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Play {
    pub from: u8,
    pub to: u8,
    pub capture: Option<Piece>,
    pub promote: Option<PromotePiece>,

    pub en_passant: bool, // True if an en_passant move was played
    pub castle: bool,     // True if the move was a castling
}

impl Play {
    pub fn new(
        from: u8,
        to: u8,
        capture: Option<Piece>,
        promote: Option<PromotePiece>,
        en_passant: bool,
        castle: bool,
    ) -> Self {
        Play {
            from,
            to,
            capture,
            promote,
            en_passant,
            castle,
        }
    }
}

impl fmt::Display for Play {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (from_rank, from_file) = index_to_coordinate(self.from);
        let (to_rank, to_file) = index_to_coordinate(self.to);
        write!(f, "{}{}", from_file, from_rank)?;
        write!(f, "{}{}", to_file, to_rank)?;
        if let Some(promote) = &self.promote {
            write!(f, "{}", char::from(promote))?;
        }
        Ok(())
    }
}
