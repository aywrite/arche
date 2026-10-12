// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! A weight for every two pawns that stand near each other: on one file, or
//! on files beside each other no more than two ranks apart.
//!
//! The pair term (`factors`) prices every pair of pieces through sixteen
//! shared lanes, and pawn with pawn was a third of what it carried. This
//! gives the near pawn pairs a weight each, outside the lanes, fitted with
//! the factors beside it. It is a function of the pawns alone, so the
//! evaluation reads it with the pawn structure, under the pawn key, and
//! pays for it on a miss only.
//!
//! The table is in white's frame and is a pair's weight less its colour
//! mirror's, so it is antisymmetric under the mirror and its sum over the
//! board's pairs is the white relative term directly. Each pair is read once:
//! a pawn reads its own colour's partners on later squares and, for a white
//! pawn, every black partner.

use super::weigh;
use crate::board::Position;
use crate::misc::Color;
use crate::psqt::pack;

/// The two sums the term is read as: the midgame half and the endgame half,
/// in centipawns. Both are the white side's; the black side's are zero, since
/// the table already holds white's less black's.
pub(crate) const COUNTS: usize = 2;

/// Each sum enters at its own end of the taper at a weight of one, so the
/// tuner's identity holds with the term's numbers in the counts. A fit holds
/// these two (`--hold pawn_pairs`); the table is fitted elsewhere.
static WEIGHTS: [i32; COUNTS] = [pack(1, 0), pack(0, 1)];

/// A row a feature (a white pawn on `s` at `s - 8`, a black pawn at
/// `s + 40`) and a column a feature, padded to 128: the midgame weight in the
/// low sixteen bits as an `i16`, the endgame weight above it, in eighths of a
/// centipawn. The two halves are joined by masking, not added as
/// `psqt::pack` adds them, so an entry is read with `halves` and not with
/// `eg_value`.
static PAIRS: [[i32; 128]; 96] = include!("pawn_pairs_table.rs");

/// The two back ranks, where a feature has no pawn.
const RANK_1: u64 = 0xff;
const RANK_8: u64 = 0xff << 56;

/// The squares a pawn on each square pairs with: its own file, and the files
/// beside it no more than two ranks away.
static NEAR: [u64; 64] = near();

const fn near() -> [u64; 64] {
    let mut out = [0u64; 64];
    let mut s = 0;
    while s < 64 {
        let mut t = 0;
        while t < 64 {
            let df = (s % 8) as i32 - (t % 8) as i32;
            let dr = (s / 8) as i32 - (t / 8) as i32;
            if s != t && (df == 0 || (df.abs() == 1 && dr.abs() <= 2)) {
                out[s] |= 1 << t;
            }
            t += 1;
        }
        s += 1;
    }
    out
}

/// One entry, its two halves widened.
#[inline(always)]
fn halves(entry: i32) -> (i32, i32) {
    (i32::from(entry as i16), entry >> 16)
}

/// The table summed over the board's near pawn pairs, each half divided by
/// eight toward zero: (midgame, endgame) in centipawns, white relative.
#[inline]
fn sums(board: &Position) -> (i32, i32) {
    // a fen can stand a pawn on a back rank, where no feature names it, so
    // the pawns there are left out
    let pawns = board.pawns() & !(RANK_1 | RANK_8);
    let (white, black) = board.sides(Color::White);
    let (w, b) = (pawns & white, pawns & black);
    let (mut mg, mut eg) = (0, 0);
    let mut add = |entry: i32| {
        let (m, e) = halves(entry);
        mg += m;
        eg += e;
    };
    let mut ws = w;
    while ws != 0 {
        let s = ws.trailing_zeros() as usize;
        ws &= ws - 1;
        let row = &PAIRS[s - 8];
        let after = (u64::MAX << s) << 1;
        let mut same = w & NEAR[s] & after;
        while same != 0 {
            add(row[same.trailing_zeros() as usize - 8]);
            same &= same - 1;
        }
        let mut other = b & NEAR[s];
        while other != 0 {
            add(row[other.trailing_zeros() as usize + 40]);
            other &= other - 1;
        }
    }
    let mut bs = b;
    while bs != 0 {
        let s = bs.trailing_zeros() as usize;
        bs &= bs - 1;
        let row = &PAIRS[s + 40];
        let after = (u64::MAX << s) << 1;
        let mut same = b & NEAR[s] & after;
        while same != 0 {
            add(row[same.trailing_zeros() as usize + 40]);
            same &= same - 1;
        }
    }
    (mg / 8, eg / 8)
}

/// The weight of one of the two counts, as the packed pair, read through
/// [`super::TERMS`].
pub(crate) const fn weight(index: usize) -> i32 {
    WEIGHTS[index]
}

/// The two sums for white and nothing for black, which is what
/// [`super::TERMS`] hands the tuner's walk.
pub(crate) fn counts(board: &Position, color: Color, into: &mut [i32]) {
    let (mg, eg) = match color {
        Color::White => sums(board),
        Color::Black => (0, 0),
    };
    into.copy_from_slice(&[mg, eg]);
}

/// The term as a packed pair, white relative.
#[inline]
pub(crate) fn fold(board: &Position) -> i32 {
    let (mg, eg) = sums(board);
    weigh(&WEIGHTS, [mg, eg], [0, 0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;

    /// The sum for a position whose near pairs are listed by hand: white e4
    /// and e5 (one file) and black d5 (beside both, a rank from e4 and on
    /// e5's rank). A white pawn on `s` is feature `s - 8`, a black one
    /// `s + 40`, so e4 is 20, e5 is 28 and d5 is 75.
    #[test]
    fn the_sum_is_the_listed_pairs_entries() {
        let board = Board::from_fen("4k3/8/8/3pP3/4P3/8/8/4K3 w - - 0 1").unwrap();
        let entries = [PAIRS[20][28], PAIRS[20][75], PAIRS[28][75]];
        let mg: i32 = entries.iter().map(|&e| halves(e).0).sum();
        let eg: i32 = entries.iter().map(|&e| halves(e).1).sum();
        assert!(
            entries.iter().all(|&e| e != 0),
            "a listed pair has no weight"
        );
        assert_eq!(sums(&board), (mg / 8, eg / 8));
    }

    /// The whole table, entry by entry: a pair's weight read from either of
    /// its two features is the same, the colour mirror of a pair carries the
    /// negated weight, only near pairs carry one, and the padding is zero.
    #[test]
    fn the_table_is_symmetric_antisymmetric_near_and_padded() {
        let square = |f: usize| if f < 48 { f + 8 } else { f - 40 };
        let white = |f: usize| f < 48;
        let mirror = |f: usize| {
            let s = square(f) ^ 56;
            if white(f) { s + 40 } else { s - 8 }
        };
        for a in 0..96 {
            for b in 0..128 {
                if b >= 96 {
                    assert_eq!(PAIRS[a][b], 0, "padding at {a}, {b}");
                    continue;
                }
                assert_eq!(PAIRS[a][b], PAIRS[b][a], "{a} and {b}");
                let (mg, eg) = halves(PAIRS[a][b]);
                let (m, e) = halves(PAIRS[mirror(a)][mirror(b)]);
                assert_eq!((mg, eg), (-m, -e), "{a} and {b} against the mirror");
                if PAIRS[a][b] != 0 {
                    assert!(
                        NEAR[square(a)] & 1 << square(b) != 0,
                        "{a} and {b} are not near"
                    );
                }
            }
        }
    }

    /// The colour mirror reads the negated sums, since the table is a pair's
    /// weight less its mirror's.
    #[test]
    fn a_mirrored_position_reads_the_negated_sums() {
        for (white, black) in [
            (
                "4k3/8/8/3pP3/4P3/8/8/4K3 w - - 0 1",
                "4k3/8/8/4p3/3Pp3/8/8/4K3 b - - 0 1",
            ),
            (
                "rnbqkbnr/pppp1ppp/8/4p3/2P1P3/8/PP1P1PPP/RNBQKBNR w KQkq - 0 1",
                "rnbqkbnr/pp1p1ppp/8/2p1p3/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1",
            ),
        ] {
            let (mg, eg) = sums(&Board::from_fen(white).unwrap());
            assert_eq!(
                sums(&Board::from_fen(black).unwrap()),
                (-mg, -eg),
                "{white}"
            );
        }
    }
}
