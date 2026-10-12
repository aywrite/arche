// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! Endgame scaling: how much of an advantage the material can convert.
//!
//! Every term scores the side ahead as if being ahead were enough. In two
//! families of ending it usually is not, and the score is cut to a fraction
//! of itself, out of [`FULL`]:
//!
//! - bishops of opposite colours with nothing else but pawns, where a pawn or
//!   two up is often a draw because the defending bishop holds the squares
//!   the attacker's cannot reach;
//! - the side ahead having no pawns and at most a bishop's worth more in
//!   pieces, as rook against a minor, where the extra material cannot mate
//!   and has nothing to promote.
//!
//! The fractions are Stockfish's for the same signatures, except the
//! opposite bishops, where Stockfish counts passed pawns and this takes a
//! half. A position matching neither is scored in full.
//!
//! Which side is ahead is read off the score itself, so the side with the
//! extra rook is scaled and a defender with pawns that is winning is not.
//! Material that cannot mate at all reads zero before any of this
//! (`Position::drawn_by_material`).

use crate::board::{LIGHT_SQUARES, Position};
use crate::misc::{Color, Piece, Score};

/// The scale of a position matching no signature.
pub(crate) const FULL: i32 = 64;

/// Bishops of opposite colours and pawns, nothing else.
const OPPOSITE_BISHOPS: i32 = 32;

/// The side ahead has fewer pieces than a rook's worth, which is a single
/// minor. It cannot mate without promoting and has nothing to promote.
const NO_MATE: i32 = 0;

/// The side ahead has at least a rook and the defender at most a minor.
const AGAINST_A_MINOR: i32 = 4;

/// The side ahead has at least a rook and the defender more than a minor.
const AGAINST_PIECES: i32 = 14;

/// What a bishop is worth on the material scale, which is the most the side
/// ahead may lead by in pieces for its pawnless advantage to be scaled.
const BISHOP: i32 = super::MATERIAL[Piece::Bishop as usize] as i32;
const ROOK: i32 = super::MATERIAL[Piece::Rook as usize] as i32;

/// One side's pieces other than pawns and its king, on the material scale.
fn pieces(board: &Position, side: u64) -> i32 {
    let count =
        |kind: u64, piece: Piece| (kind & side).count_ones() as i32 * super::material(piece) as i32;
    count(board.knights(), Piece::Knight)
        + count(board.bishops(), Piece::Bishop)
        + count(board.rooks(), Piece::Rook)
        + count(board.queens(), Piece::Queen)
}

/// The scale of `strong`'s advantage, out of [`FULL`].
pub(crate) fn factor(board: &Position, strong: Color) -> i32 {
    let (ours, theirs) = board.sides(strong);
    let bishops = board.bishops();
    if board.knights() | board.rooks() | board.queens() == 0
        && (bishops & ours).count_ones() == 1
        && (bishops & theirs).count_ones() == 1
        && (bishops & LIGHT_SQUARES).count_ones() == 1
    {
        return OPPOSITE_BISHOPS;
    }
    if board.pawns() & ours != 0 {
        return FULL;
    }
    let (strong, weak) = (pieces(board, ours), pieces(board, theirs));
    if strong - weak > BISHOP {
        FULL
    } else if strong < ROOK {
        NO_MATE
    } else if weak <= BISHOP {
        AGAINST_A_MINOR
    } else {
        AGAINST_PIECES
    }
}

/// `score`, from the side to move's point of view, scaled by the factor of
/// whichever side it favours. The divide truncates toward zero, so the same
/// position from the other side to move scales to the negative.
#[inline]
pub(crate) fn scaled(board: &Position, score: Score) -> Score {
    let strong = match score {
        0 => return 0,
        1.. => board.active_color,
        _ => !board.active_color,
    };
    let factor = factor(board, strong);
    if factor == FULL {
        return score;
    }
    (i32::from(score) * factor / FULL) as Score
}

#[cfg(test)]
mod tests {
    use super::{FULL, factor};
    use crate::board::Board;
    use crate::eval::eval;
    use crate::misc::Color;
    use pretty_assertions::assert_eq;

    /// Each signature at its factor, for the side ahead, and the positions
    /// beside them that look alike and are scored in full. The figures in the
    /// comments are what the evaluation read before the scale.
    #[test]
    fn each_signature_scales_the_side_ahead() {
        for (fen, strong, expected, why) in [
            (
                "8/5k2/8/3b4/8/2B2PP1/5K2/8 w - - 0 1",
                Color::White,
                32,
                "opposite bishops two pawns up, +232",
            ),
            (
                "8/5k2/8/2b5/8/2B2PP1/5K2/8 w - - 0 1",
                Color::White,
                FULL,
                "bishops on one colour, +206",
            ),
            (
                "r5k1/5ppp/8/3b4/8/2B2P2/5PPP/R5K1 w - - 0 1",
                Color::White,
                FULL,
                "opposite bishops with a rook each",
            ),
            (
                "8/8/8/8/8/4k3/8/n3K2R w - - 0 1",
                Color::White,
                4,
                "rook against knight, +232",
            ),
            (
                "8/8/8/8/8/4k3/8/b3K2R w - - 0 1",
                Color::White,
                4,
                "rook against bishop, +177",
            ),
            (
                "8/8/8/8/3k4/8/2r5/R1B1K3 w - - 0 1",
                Color::White,
                14,
                "rook and bishop against rook, +275",
            ),
            (
                "8/8/8/8/3k4/8/2r5/R1Q1K3 w - - 0 1",
                Color::White,
                FULL,
                "rook and queen against rook",
            ),
            (
                "8/5k2/2n5/8/3K4/1R6/1P6/8 w - - 0 1",
                Color::White,
                FULL,
                "rook and pawn against knight",
            ),
            (
                "8/5k2/8/8/3K4/8/1N3pp1/8 w - - 0 1",
                Color::White,
                0,
                "a knight against pawns",
            ),
            (
                "8/5k2/8/8/3K4/8/1N3pp1/8 w - - 0 1",
                Color::Black,
                FULL,
                "the pawns against a knight",
            ),
            (
                "8/8/3k4/8/8/2B5/1K6/1N6 w - - 0 1",
                Color::White,
                FULL,
                "bishop and knight, which mate",
            ),
        ] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(factor(&board, strong), expected, "{}: {}", why, fen);
        }
    }

    /// The scale reaches the score through the evaluation, and from both
    /// sides of the board.
    #[test]
    fn the_evaluation_reads_the_scale() {
        for (fen, at_most) in [
            ("8/8/8/8/8/4k3/8/n3K2R w - - 0 1", 20),
            ("8/8/8/8/8/4k3/8/n3K2R b - - 0 1", 20),
            ("8/5k2/8/3b4/8/2B2PP1/5K2/8 w - - 0 1", 130),
            ("8/5k2/8/3b4/8/2B2PP1/5K2/8 b - - 0 1", 130),
        ] {
            let board = Board::from_fen(fen).unwrap();
            let score = i32::from(eval(&board)).abs();
            assert!(score <= at_most, "{} scored {}", fen, score);
        }
        let ahead = Board::from_fen("8/8/8/8/8/4k3/8/n3K2R w - - 0 1").unwrap();
        assert!(eval(&ahead) > 0);
    }
}
