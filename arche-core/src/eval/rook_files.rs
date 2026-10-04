// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! Which of each side's rooks stand on a file free of pawns, and what that
//! is worth.
//!
//! The rooks and both sides' pawns are read, so a rook move changes it and
//! the pawn key does not cover it. Nothing is remembered: at zero weight the
//! leaf does not count the term at all.

use super::pawn_structure::{files_of, spread};
use super::{scored, weigh};
use crate::board::Board;
use crate::misc::Color;
use crate::psqt::pack;

/// How many counts the term is measured in, in the order [`ROOK_FILES`] and
/// [`counts_of`] are indexed by: the rooks on open files, then the rooks on
/// half open ones.
pub(crate) const COUNTS: usize = 2;

/// What one rook on such a file is worth, as the packed pairs the taper is
/// read from: an open file, then a half open one.
///
/// Fitted 2026-10-04 with the pawn links, as their comment says. The fit
/// puts the open file slightly below nothing in the ending.
///
/// A side has two rooks and eight pawns that could promote, so no count
/// exceeds ten, and `bounds_hold` charges ten of each.
static ROOK_FILES: [i32; COUNTS] = [pack(7, -2), pack(6, 3)];

/// The weight of one count, as the packed pair, read through
/// [`super::TERMS`] so that a slot names the live weight rather than a copy.
pub(crate) const fn weight(index: usize) -> i32 {
    ROOK_FILES[index]
}

/// Whether [`super::sum`] takes this term at the leaf, for the reason
/// `king_attack::SCORED` gives.
pub(crate) const SCORED: bool = scored(&ROOK_FILES);

/// This side's rooks in the two counts [`COUNTS`] names.
///
/// A file is open when no pawn of either colour stands on it, and half open
/// for this side when no pawn of ours stands on it and a pawn of theirs
/// does. Each rook is counted once, so two rooks doubled on an open file are
/// two and a file holding two enemy pawns is no more half open than one
/// holding one. A queen on the same file is not counted.
///
/// The shelter reads the same two kinds of file round the king. The
/// evaluation and the tuner's walk both read this, so the hand counts in the
/// tests below are what pin it.
#[inline]
pub(crate) fn counts_of(board: &Board, color: Color) -> [i32; COUNTS] {
    let (ours, theirs) = board.sides(color);
    let pawns = board.pawns();
    let our_files = files_of(pawns & ours);
    let their_files = files_of(pawns & theirs);
    let rooks = board.rooks() & ours;
    [
        (rooks & spread(!(our_files | their_files))).count_ones() as i32,
        (rooks & spread(!our_files & their_files)).count_ones() as i32,
    ]
}

/// The two counts, written into `into`, which is what [`super::TERMS`] hands
/// the tuner's walk.
pub(crate) fn counts(board: &Board, color: Color, into: &mut [i32]) {
    into.copy_from_slice(&counts_of(board, color));
}

/// What white's rooks on open and half open files stand ahead by, as a
/// packed pair on the scale the piece square pair is on.
#[inline]
pub(crate) fn fold(board: &Board) -> i32 {
    weigh(
        &ROOK_FILES,
        counts_of(board, Color::White),
        counts_of(board, Color::Black),
    )
}

/// The same fold against weights named by the caller, since the shipped
/// fold is nothing wherever it is read.
#[cfg(test)]
pub(crate) fn fold_with(board: &Board, weights: &[i32; COUNTS]) -> i32 {
    weigh(
        weights,
        counts_of(board, Color::White),
        counts_of(board, Color::Black),
    )
}

#[cfg(test)]
mod tests {
    use super::{Board, COUNTS, Color, ROOK_FILES, SCORED, counts_of, fold_with};
    use crate::psqt::{eg_value, mg_value, pack};
    use pretty_assertions::assert_eq;

    /// The counts by hand, in the helper's order: open, then half open. Each
    /// row names the colour whose rooks are counted.
    #[test]
    fn rooks_stand_on_the_files_a_hand_count_says_they_do() {
        for (fen, color, counts, why) in [
            (
                "4r1k1/5pp1/8/3PP3/2P5/8/8/R3K2R w - - 0 1",
                Color::White,
                [2, 0],
                "rooks on a1 and h1, and no pawn on either file",
            ),
            (
                "4r1k1/5pp1/8/3PP3/2P5/8/8/R3K2R w - - 0 1",
                Color::Black,
                [0, 1],
                "a rook on e8, and a white pawn on e5",
            ),
            // the rook's own pawns close the file however many there are
            (
                "4k3/8/8/8/4P3/4P3/8/4R1K1 w - - 0 1",
                Color::White,
                [0, 0],
                "a rook on e1 behind its doubled pawns",
            ),
            // and the other side's are one half open file, not two
            (
                "4k3/4p3/4p3/8/8/8/8/4R1K1 w - - 0 1",
                Color::White,
                [0, 1],
                "a rook on e1 against doubled pawns on e6 and e7",
            ),
            // a pawn of each colour closes it for both
            (
                "4k3/4p3/8/8/4P3/8/8/4R1K1 w - - 0 1",
                Color::White,
                [0, 0],
                "a rook on e1, and a pawn of each colour on the e file",
            ),
            // each rook is counted
            (
                "k7/8/8/8/8/8/4R3/4R1K1 w - - 0 1",
                Color::White,
                [2, 0],
                "rooks doubled on e1 and e2",
            ),
            // a queen is not a rook
            (
                "4k3/8/8/8/8/8/8/Q3K3 w - - 0 1",
                Color::White,
                [0, 0],
                "a queen on a1",
            ),
            // a pawn on the h file says nothing about the a file
            (
                "4k3/8/8/8/8/8/7P/R3K3 w - - 0 1",
                Color::White,
                [1, 0],
                "a rook on a1 and a pawn on h2",
            ),
            // black's files are read against black's pawns
            (
                "r2rk3/8/8/8/3P4/8/8/4K3 w - - 0 1",
                Color::Black,
                [1, 1],
                "black rooks on a8 and d8, and a white pawn on d4",
            ),
            (
                "3rk3/3p4/8/8/8/8/8/4K3 w - - 0 1",
                Color::Black,
                [0, 0],
                "a black rook on d8 behind its pawn on d7",
            ),
        ] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(counts_of(&board, color), counts, "{}", why);
        }
    }

    /// Black's count of a position is white's count of its reflection.
    #[test]
    fn the_two_colours_count_the_same_way() {
        let white = Board::from_fen("4r1k1/5pp1/8/3PP3/2P5/8/8/R3K2R w - - 0 1").unwrap();
        let black = Board::from_fen("r3k2r/8/8/2p5/3pp3/8/5PP1/4R1K1 b - - 0 1").unwrap();
        assert_eq!(
            counts_of(&white, Color::White),
            counts_of(&black, Color::Black)
        );
        assert_eq!(
            counts_of(&white, Color::Black),
            counts_of(&black, Color::White)
        );
    }

    /// White's rook on a1 has an open file and the two on f1 and h1 face
    /// black pawns on f7 and h7, so the differences are 1 and 2.
    const ROOKS: &str = "4k3/5p1p/8/8/4P3/2PP4/8/R4RKR w - - 0 1";

    /// Weights that differ at both ends of the taper and from each other.
    const TRIAL: [i32; COUNTS] = [pack(13, 4), pack(-6, 17)];

    /// White's count less black's, count by count, each half on its own.
    #[test]
    fn the_rook_files_fold_reads_white_less_black_count_by_count() {
        let board = Board::from_fen(ROOKS).unwrap();
        let (white, black) = ([1, 2], [0, 0]);
        assert_eq!(counts_of(&board, Color::White), white);
        assert_eq!(counts_of(&board, Color::Black), black);
        let midgame: i32 = (0..COUNTS)
            .map(|i| mg_value(TRIAL[i]) * (white[i] - black[i]))
            .sum();
        let endgame: i32 = (0..COUNTS)
            .map(|i| eg_value(TRIAL[i]) * (white[i] - black[i]))
            .sum();
        assert_ne!(midgame, endgame);
        let packed = fold_with(&board, &TRIAL);
        assert_eq!((mg_value(packed), eg_value(packed)), (midgame, endgame));
    }

    /// [`SCORED`] is the weights and nothing else.
    #[test]
    fn the_term_is_counted_exactly_when_a_weight_is_not_zero() {
        let priced = ROOK_FILES
            .iter()
            .any(|weight| mg_value(*weight) != 0 || eg_value(*weight) != 0);
        assert_eq!(SCORED, priced);
    }
}
