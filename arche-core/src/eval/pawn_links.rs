// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! Which of each side's pawns stand beside or behind another of their own,
//! and what that is worth.
//!
//! Pawns and nothing else are read, as for the pawn structure. Nothing is
//! remembered yet: at zero weight the leaf does not count the term at all.

use super::{scored, weigh};
use crate::board::{Board, pawn_attacks};
use crate::misc::Color;
use crate::psqt::pack;

/// How many counts the term is measured in, in the order [`PAWN_LINKS`] and
/// [`counts_of`] are indexed by: the phalanx pawns, the supported ones, and
/// the pawns that are either.
pub(crate) const COUNTS: usize = 3;

/// What one linked pawn is worth, as the packed pairs the taper is read
/// from: phalanx, supported, connected.
///
/// Fitted 2026-10-04 on 59,049 games beside the rank 8 pair term, with the
/// linear weights held at the joint refit's, and rounded to whole
/// centipawns. A connected pawn that is also a phalanx or supported pawn
/// is paid both ways, which is how the fit priced it.
///
/// No count exceeds eight, the pawns a side has, and `bounds_hold` charges
/// eight of each.
static PAWN_LINKS: [i32; COUNTS] = [pack(1, 3), pack(6, 5), pack(3, 4)];

/// The weight of one count, as the packed pair, read through
/// [`super::TERMS`] so that a slot names the live weight rather than a copy.
pub(crate) const fn weight(index: usize) -> i32 {
    PAWN_LINKS[index]
}

/// Whether [`super::sum`] takes this term at the leaf, for the reason
/// `king_attack::SCORED` gives.
pub(crate) const SCORED: bool = scored(&PAWN_LINKS);

const A_FILE: u64 = 0x0101_0101_0101_0101;
const H_FILE: u64 = 0x8080_8080_8080_8080;

/// This side's pawns in the three counts [`COUNTS`] names.
///
/// A pawn is in a phalanx when a pawn of ours stands on a file beside it on
/// the same rank, and supported when a pawn of ours attacks it, which is a
/// pawn diagonally behind it. Each pawn is counted once per count, so a pawn
/// with a neighbour on both sides is one phalanx pawn, and the third count is
/// the pawns that are either, a pawn that is both counted once.
///
/// Behind is read in the side's own frame through [`pawn_attacks`], so a
/// white pawn is supported from the rank below and a black one from the rank
/// above. A phalanx is the same either way up.
///
/// The evaluation and the tuner's walk both read this, so the hand counts in
/// the tests below are what pin it.
#[inline]
pub(crate) fn counts_of(board: &Board, color: Color) -> [i32; COUNTS] {
    let (ours, _) = board.sides(color);
    let pawns = board.pawns() & ours;
    // the file masks stop a shift carrying a pawn on the h file round to the
    // a file of the next rank, as they do in `pawn_attacks`
    let beside = ((pawns & !A_FILE) >> 1) | ((pawns & !H_FILE) << 1);
    let phalanx = pawns & beside;
    let supported = pawns & pawn_attacks(pawns, color);
    [
        phalanx.count_ones() as i32,
        supported.count_ones() as i32,
        (phalanx | supported).count_ones() as i32,
    ]
}

/// The three counts, written into `into`, which is what [`super::TERMS`]
/// hands the tuner's walk.
pub(crate) fn counts(board: &Board, color: Color, into: &mut [i32]) {
    into.copy_from_slice(&counts_of(board, color));
}

/// What white's linked pawns stand ahead by, as a packed pair on the scale
/// the piece square pair is on.
#[inline]
pub(crate) fn fold(board: &Board) -> i32 {
    weigh(
        &PAWN_LINKS,
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
    use super::{Board, COUNTS, Color, PAWN_LINKS, SCORED, counts_of, fold_with};
    use crate::psqt::{eg_value, mg_value, pack};
    use pretty_assertions::assert_eq;

    /// The counts by hand, in the helper's order: phalanx, supported,
    /// connected. Each row names the colour whose pawns are counted.
    #[test]
    fn pawns_link_as_a_hand_count_says_they_do() {
        for (fen, color, counts, why) in [
            (
                "4r1k1/5pp1/8/3PP3/2P5/8/8/R3K2R w - - 0 1",
                Color::White,
                [2, 1, 2],
                "d5 and e5 side by side, and c4 behind d5",
            ),
            (
                "4r1k1/5pp1/8/3PP3/2P5/8/8/R3K2R w - - 0 1",
                Color::Black,
                [2, 0, 2],
                "f7 and g7 side by side",
            ),
            // a pawn with a neighbour on each side is one pawn
            (
                "4k3/8/8/8/2PPP3/8/8/4K3 w - - 0 1",
                Color::White,
                [3, 0, 3],
                "c4, d4 and e4 in a row",
            ),
            // d4 is both, and connected once. c3 behind it is neither
            (
                "4k3/8/8/8/3PP3/2P5/8/4K3 w - - 0 1",
                Color::White,
                [2, 1, 2],
                "d4 and e4 side by side, and c3 behind d4",
            ),
            // a pawn on its own file behind another does not support it
            (
                "4k3/8/8/8/4P3/4P3/8/4K3 w - - 0 1",
                Color::White,
                [0, 0, 0],
                "a doubled pawn on e3 and e4",
            ),
            // the edge files link inwards like any other
            (
                "4k3/8/8/8/8/8/PP4PP/4K3 w - - 0 1",
                Color::White,
                [4, 0, 4],
                "a2 and b2, g2 and h2",
            ),
            (
                "4k3/8/8/8/8/P6P/1P4P1/4K3 w - - 0 1",
                Color::White,
                [0, 2, 2],
                "a3 behind b2 and h3 behind g2",
            ),
            // h4 and a5 are neighbours as bits and not as squares
            (
                "4k3/8/8/P7/7P/8/8/4K3 w - - 0 1",
                Color::White,
                [0, 0, 0],
                "a5 and h4 either side of the wrap",
            ),
            // and h3 would attack a5 across it
            (
                "4k3/8/8/P7/8/7P/8/4K3 w - - 0 1",
                Color::White,
                [0, 0, 0],
                "a5 and h3 either side of the wrap",
            ),
            // the same for black, down the board: a4 would attack h2 across
            // the wrap, and h5 would attack a5
            (
                "4k3/8/8/p6p/p7/8/7p/4K3 w - - 0 1",
                Color::Black,
                [0, 0, 0],
                "black pawns on a4, a5, h2 and h5",
            ),
        ] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(counts_of(&board, color), counts, "{}", why);
        }
    }

    /// A black pawn is supported from the rank above it. d6 supports c5 and
    /// e5, which is two; read up the board instead, c5 and e5 would support
    /// d6, which is one.
    #[test]
    fn a_black_pawn_is_supported_from_above() {
        let board = Board::from_fen("4k3/8/3p4/2p1p3/8/8/8/4K3 w - - 0 1").unwrap();
        assert_eq!(counts_of(&board, Color::Black), [0, 2, 2]);
        assert_eq!(counts_of(&board, Color::White), [0, 0, 0]);
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

    /// Three white pawns linked three ways and no black pawn linked at all:
    /// c3 and d3 side by side and e4 in front of d3, so the differences are
    /// 2, 1 and 3.
    const LINKED: &str = "4k3/5p1p/8/8/4P3/2PP4/8/R4RKR w - - 0 1";

    /// Weights that differ at both ends of the taper and from each other.
    const TRIAL: [i32; COUNTS] = [pack(7, -3), pack(-11, 19), pack(5, 23)];

    /// White's count less black's, count by count, each half on its own.
    #[test]
    fn the_pawn_links_fold_reads_white_less_black_count_by_count() {
        let board = Board::from_fen(LINKED).unwrap();
        let (white, black) = ([2, 1, 3], [0, 0, 0]);
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
        let priced = PAWN_LINKS
            .iter()
            .any(|weight| mg_value(*weight) != 0 || eg_value(*weight) != 0);
        assert_eq!(SCORED, priced);
    }
}
