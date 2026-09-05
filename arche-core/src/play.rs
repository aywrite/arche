// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use crate::misc::index_to_coordinate;
use crate::misc::{Piece, PromotePiece};
use std::fmt;

/// A move. Six bytes, which is what the fields happen to come to and what the
/// search is tuned around: a move is copied into a move list, out of it, into
/// the history and into the transposition table, several times per node.
/// Widening it has been measured twice and was slower both times.
///
/// Outside the crate a move is a value: it comes out of `generate_moves`, a
/// search result or a pv line, goes into `try_make`, and prints in coordinate
/// notation. The fields stay in the crate so that a move handed to the board
/// is one this crate generated.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Play {
    pub(crate) from: u8,
    pub(crate) to: u8,
    pub(crate) capture: Option<Piece>,
    pub(crate) promote: Option<PromotePiece>,

    /// The two moves that displace a second piece, and so cannot be read off
    /// the from and to squares alone: the pawn taken en passant does not
    /// stand on the to square, and a castle moves a rook as well as the king.
    pub(crate) en_passant: bool,
    pub(crate) castle: bool,
}

impl Play {
    pub(crate) fn new(
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
