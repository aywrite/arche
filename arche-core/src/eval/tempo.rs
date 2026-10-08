// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The tempo: what having the move is worth.
//!
//! Every other term scores the pieces and none says whose turn it is, so a
//! position and the same position with the other side to move scored as each
//! other's negatives. Fitted on the corpus with the rest of the evaluation
//! held as it plays, the side to move was underrated by about nine
//! centipawns, a little more with the pieces on than off.

use super::weigh;
#[cfg(test)]
use crate::board::Board;
use crate::board::Position;
use crate::misc::Color;
use crate::psqt::pack;

/// One count: whether this side is the one to move.
pub(crate) const COUNTS: usize = 1;

/// The fit gave 10.47 in the midgame and 8.28 in the ending, the folds
/// within half a centipawn of each other, rounded here. `tune.py::bounds_hold`
/// charges it for both colours as it charges every term, which is loose on
/// the safe side, since only one side has the move.
static TEMPO: [i32; COUNTS] = [pack(10, 8)];

/// The weight of the count, as the packed pair, read through
/// [`super::TERMS`] so that a slot names the live weight rather than a copy.
pub(crate) const fn weight(index: usize) -> i32 {
    TEMPO[index]
}

fn counts_of(board: &Position, color: Color) -> [i32; COUNTS] {
    [i32::from(board.active_color == color)]
}

/// The count, written into `into`, which is what [`super::TERMS`] hands the
/// tuner's walk.
pub(crate) fn counts(board: &Position, color: Color, into: &mut [i32]) {
    into.copy_from_slice(&counts_of(board, color));
}

/// White's tempo less black's, as a packed pair: the weight for white to
/// move and its negative for black, before the score turns to the mover.
#[inline]
pub(crate) fn fold(board: &Position) -> i32 {
    weigh(
        &TEMPO,
        counts_of(board, Color::White),
        counts_of(board, Color::Black),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_side_to_move_counts() {
        let white = Board::from_fen("4k3/8/8/8/8/8/8/R3K3 w - - 0 1").unwrap();
        let black = Board::from_fen("4k3/8/8/8/8/8/8/R3K3 b - - 0 1").unwrap();
        assert_eq!(counts_of(&white, Color::White), [1]);
        assert_eq!(counts_of(&white, Color::Black), [0]);
        assert_eq!(counts_of(&black, Color::White), [0]);
        assert_eq!(counts_of(&black, Color::Black), [1]);
        assert_eq!(fold(&white), TEMPO[0]);
        assert_eq!(fold(&black), -TEMPO[0]);
    }
}
