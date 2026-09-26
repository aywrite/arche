// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! A factorization machine over the piece square features: a weight for every
//! pair of pieces on the board, as the inner product of two short vectors.
//!
//! A feature is a piece of one colour on one square, seen from one side:
//! `(0 if own else 384) + piece × 64 + square`, the square flipped by rank
//! for black. Each has a row of `RANK` factors. Summed over the pieces
//! standing, a perspective's pair sum collapses (Rendle 2010) to
//!
//! ```text
//!   sum    <q_i, q_j>  =  ( ‖s‖² - D ) / 2,   s = sum q_i,   D = sum ‖q_i‖²
//!  i < j
//! ```
//!
//! so the board keeps `s` and `D` for each perspective and the leaf reads two
//! sums of squares. White's perspective less black's is the term, which makes
//! it white relative like the rest of the accumulator. It is not tapered.
//!
//! `RANK` is the switch. At 0 every array here has no length, every loop has
//! nothing to walk and [`Machine::score`] answers 0 before reading anything,
//! so the term compiles away.

use crate::misc::{Color, Piece};

/// How many factors a feature has. 0 turns the term off.
pub(crate) const RANK: usize = 0;

/// The table's scale: a factor of `v` is stored as `v × Q`, so a product of
/// two is `Q²` too large and the term divides by it.
pub(crate) const Q: i64 = 1;

/// Two perspectives times six pieces times sixty four squares.
pub(crate) const FEATURES: usize = 2 * 6 * 64;

/// The most pieces a legal position stands, which is what bounds a lane.
const MOST_PIECES: usize = 32;

/// 1 when the term is on and 0 when it is off, so the diagonal sums are
/// sized to vanish with it rather than kept and read at rank 0.
const LIVE: usize = (RANK != 0) as usize;

/// Each feature's factors, at scale `Q`.
static FACTORS: [[i16; RANK]; FEATURES] = [[0; RANK]; FEATURES];

/// Each feature's `‖q_i‖²`, worked out from the table when it compiles.
static DIAGONAL: [[i32; LIVE]; FEATURES] = diagonal();

const fn diagonal() -> [[i32; LIVE]; FEATURES] {
    let mut out = [[0; LIVE]; FEATURES];
    let mut feature = 0;
    while feature < FEATURES {
        let mut lane = 0;
        let mut total = 0;
        while lane != RANK {
            let factor = FACTORS[feature][lane] as i32;
            total += factor * factor;
            lane += 1;
        }
        let mut slot = 0;
        while slot != LIVE {
            out[feature][slot] = total;
            slot += 1;
        }
        feature += 1;
    }
    out
}

/// The largest each lane of `s` can reach: the sum of its 32 largest
/// magnitudes, kept as a sorted top 32 per lane in one pass over the table.
/// Picking the 32 by a scan each measured past the compiler's limit on a
/// constant's evaluation at rank 32.
const fn lane_bounds() -> [i64; RANK] {
    let mut top = [[0_i64; MOST_PIECES]; RANK];
    let mut feature = 0;
    while feature != FEATURES {
        let mut lane = 0;
        while lane != RANK {
            let magnitude = (FACTORS[feature][lane] as i64).abs();
            let kept = &mut top[lane];
            // ascending, so the smallest kept is first and a larger one
            // replaces it and sinks to its place
            if magnitude > kept[0] {
                kept[0] = magnitude;
                let mut at = 1;
                while at != MOST_PIECES && kept[at] < kept[at - 1] {
                    let smaller = kept[at];
                    kept[at] = kept[at - 1];
                    kept[at - 1] = smaller;
                    at += 1;
                }
            }
            lane += 1;
        }
        feature += 1;
    }
    let mut bounds = [0; RANK];
    let mut lane = 0;
    while lane != RANK {
        let mut at = 0;
        while at != MOST_PIECES {
            bounds[lane] += top[lane][at];
            at += 1;
        }
        lane += 1;
    }
    bounds
}

/// Whether no legal position can overflow the arithmetic: every lane fits
/// i16, and a perspective's sum of squares fits i32. `D` fits with it, since
/// `Σ_i q_i,r²` is at most `(Σ_i |q_i,r|)²` in every lane.
const fn in_range() -> bool {
    let bounds = lane_bounds();
    let mut squares = 0;
    let mut lane = 0;
    while lane != RANK {
        if bounds[lane] > i16::MAX as i64 {
            return false;
        }
        squares += bounds[lane] * bounds[lane];
        lane += 1;
    }
    squares <= i32::MAX as i64
}

const _: () = assert!(in_range(), "a legal position could overflow the factors");

/// The feature a piece sets, seen from `perspective`.
#[inline(always)]
pub(crate) const fn feature(perspective: Color, index: u8, piece: Piece, color: Color) -> usize {
    let square = match perspective {
        Color::White => index,
        Color::Black => index ^ 56,
    } as usize;
    let side = if color as usize == perspective as usize {
        0
    } else {
        384
    };
    side + piece as usize * 64 + square
}

/// One feature's factors, for the tuner's walk.
pub(crate) fn row(feature: usize) -> &'static [i16; RANK] {
    &FACTORS[feature]
}

/// The term's incremental state, one per perspective, indexed by `Color`.
///
/// The lanes wrap rather than check: a position with more pieces than a
/// legal one can stand (a fen can state one) scores wrongly rather than
/// panicking in a debug build.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct Machine {
    sums: [[i16; RANK]; 2],
    diagonal: [[i32; LIVE]; 2],
}

impl Machine {
    pub(crate) const EMPTY: Self = Self {
        sums: [[0; RANK]; 2],
        diagonal: [[0; LIVE]; 2],
    };

    /// A piece counted on to or off of a square, in both perspectives.
    #[inline(always)]
    pub(crate) fn count<const SET: bool>(&mut self, index: u8, piece: Piece, color: Color) {
        // the loops below walk nothing at rank 0, but the table's index is
        // still bounds checked, which costs at every move
        if RANK == 0 {
            return;
        }
        for perspective in [Color::Black, Color::White] {
            let feature = feature(perspective, index, piece, color);
            let at = perspective as usize;
            for (sum, &factor) in self.sums[at].iter_mut().zip(&FACTORS[feature]) {
                *sum = if SET {
                    sum.wrapping_add(factor)
                } else {
                    sum.wrapping_sub(factor)
                };
            }
            for (sum, &diagonal) in self.diagonal[at].iter_mut().zip(&DIAGONAL[feature]) {
                *sum = if SET {
                    sum.wrapping_add(diagonal)
                } else {
                    sum.wrapping_sub(diagonal)
                };
            }
        }
    }

    /// A piece moving between two squares.
    #[inline(always)]
    pub(crate) fn relocate(&mut self, from: u8, to: u8, piece: Piece, color: Color) {
        if RANK == 0 {
            return;
        }
        for perspective in [Color::Black, Color::White] {
            let left = feature(perspective, from, piece, color);
            let arrived = feature(perspective, to, piece, color);
            let at = perspective as usize;
            for ((sum, &off), &on) in self.sums[at]
                .iter_mut()
                .zip(&FACTORS[left])
                .zip(&FACTORS[arrived])
            {
                *sum = sum.wrapping_add(on.wrapping_sub(off));
            }
            for ((sum, &off), &on) in self.diagonal[at]
                .iter_mut()
                .zip(&DIAGONAL[left])
                .zip(&DIAGONAL[arrived])
            {
                *sum = sum.wrapping_add(on.wrapping_sub(off));
            }
        }
    }

    /// The state the pieces deserve, summed from the table directly rather
    /// than through `count`, for `Accumulator::recomputed`.
    pub(crate) fn of(pieces: impl Iterator<Item = (u8, Piece, Color)>) -> Self {
        let mut sums = [[0_i16; RANK]; 2];
        let mut diagonal = [[0_i32; LIVE]; 2];
        for (index, piece, color) in pieces {
            for perspective in [Color::Black, Color::White] {
                let feature = feature(perspective, index, piece, color);
                let at = perspective as usize;
                for (sum, &factor) in sums[at].iter_mut().zip(&FACTORS[feature]) {
                    *sum = sum.wrapping_add(factor);
                }
                let square: i32 = FACTORS[feature]
                    .iter()
                    .map(|&factor| i32::from(factor) * i32::from(factor))
                    .sum();
                for total in &mut diagonal[at] {
                    *total = total.wrapping_add(square);
                }
            }
        }
        Self { sums, diagonal }
    }

    /// The term, white relative, in centipawns.
    #[inline(always)]
    pub(crate) fn score(&self) -> i32 {
        if RANK == 0 {
            return 0;
        }
        let doubled = |at: usize| {
            let squares = self.sums[at].iter().fold(0_i32, |total, &sum| {
                total.wrapping_add(i32::from(sum) * i32::from(sum))
            });
            let diagonal: i32 = self.diagonal[at].iter().sum();
            i64::from(squares) - i64::from(diagonal)
        };
        // truncated toward zero, so a mirrored position scores the exact
        // negation of its mirror
        ((doubled(Color::White as usize) - doubled(Color::Black as usize)) / (2 * Q * Q)) as i32
    }
}

#[cfg(test)]
mod features {
    use super::feature;
    use crate::misc::{Color, Piece};

    /// Worked by hand from the definition the factors were fitted against,
    /// since every path in the engine and the tuner reads this one function
    /// and would agree with each other about a wrong index.
    #[test]
    fn a_feature_is_the_side_the_piece_and_the_square_seen_from_a_perspective() {
        // e8 is 60 and e1 is 4
        assert_eq!(
            feature(Color::Black, 60, Piece::King, Color::Black),
            5 * 64 + 4
        );
        assert_eq!(
            feature(Color::White, 60, Piece::King, Color::Black),
            384 + 5 * 64 + 60
        );
        assert_eq!(
            feature(Color::White, 4, Piece::King, Color::White),
            5 * 64 + 4
        );
        assert_eq!(
            feature(Color::Black, 4, Piece::King, Color::White),
            384 + 5 * 64 + 60
        );
        // a white pawn on a2 from each side
        assert_eq!(feature(Color::White, 8, Piece::Pawn, Color::White), 8);
        assert_eq!(
            feature(Color::Black, 8, Piece::Pawn, Color::White),
            384 + 48
        );
    }
}
