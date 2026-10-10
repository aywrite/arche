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
//! White's perspective less black's is the term, which makes it white
//! relative like the rest of the accumulator. It is not tapered. The two sums
//! of squares are read as one product, `‖s_w‖² - ‖s_b‖² = (s_w + s_b)·(s_w -
//! s_b)`, so the board keeps the sum and the difference of the two
//! perspectives' `s`, and `D_w - D_b`, and the leaf reads one dot product
//! where it read two.
//!
//! At a `RANK` of 0 every array here has no length and [`Machine::score`]
//! answers 0 before reading anything, so the term compiles away. The shipped
//! table is sixteen wide, so turning the term off takes an empty table in
//! place of `factors16.rs` as well as the rank.

use crate::misc::{Color, Piece};

/// How many factors a feature has. 16 is the rank chosen on 2026-09-26
/// against a cost bar fixed before it was measured: ranks 8, 16 and 32 cost
/// 6.2%, 4.4% and 8.2% of the bench's nodes a second, and 16 bought the most
/// held out loss for what it cost.
#[cfg(not(feature = "machine-test"))]
pub(crate) const RANK: usize = 16;

/// The table's scale: a factor of `v` is stored as `v × Q`, so a product of
/// two is `Q²` too large and the term divides by it. 128 is the largest power
/// of two at which [`in_range`] holds for this table; the i32 sum of squares
/// is what binds.
#[cfg(not(feature = "machine-test"))]
pub(crate) const Q: i64 = 128;

/// The rank the tests run the term at, on the seeded table the
/// `machine-test` feature swaps in.
#[cfg(feature = "machine-test")]
pub(crate) const RANK: usize = 8;

#[cfg(feature = "machine-test")]
pub(crate) const Q: i64 = 64;

/// Own and other side, six pieces, sixty four squares.
pub(crate) const FEATURES: usize = 2 * 6 * 64;

/// The most pieces a legal position stands, which is what bounds a lane.
const MOST_PIECES: usize = 32;

/// 1 when the term is on and 0 when it is off, so the diagonal sums are
/// sized to vanish with it rather than kept and read at rank 0.
pub(crate) const LIVE: usize = (RANK != 0) as usize;

/// Each feature's factors, at scale `Q`.
#[cfg(not(feature = "machine-test"))]
pub(crate) static FACTORS: [[i16; RANK]; FEATURES] = include!("factors16.rs");

#[cfg(feature = "machine-test")]
pub(crate) static FACTORS: [[i16; RANK]; FEATURES] = seeded();

/// A fixed table for the tests: every factor drawn from -96 to 96 by a linear
/// congruential generator, which puts the term's spread near the fitted
/// one's.
#[cfg(feature = "machine-test")]
const fn seeded() -> [[i16; RANK]; FEATURES] {
    let mut table = [[0; RANK]; FEATURES];
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut feature = 0;
    while feature != FEATURES {
        let mut lane = 0;
        while lane != RANK {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            table[feature][lane] = ((state >> 33) % 193) as i16 - 96;
            lane += 1;
        }
        feature += 1;
    }
    table
}

/// Each feature's `‖q_i‖²`, worked out from the table when it compiles.
pub(crate) static DIAGONAL: [[i32; LIVE]; FEATURES] = diagonal();

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

/// Whether no legal position can overflow the arithmetic: every lane of the
/// two perspectives' sum and difference fits i16 (each is at most twice a
/// perspective's lane), and a perspective's sum of squares fits i32. `D` fits
/// with it, since `Σ_i q_i,r²` is at most `(Σ_i |q_i,r|)²` in every lane. The
/// product of the sum and the difference is the difference of two sums of
/// squares that each fit i32, so it fits i32 too, and so does `D_w - D_b`.
const fn in_range() -> bool {
    let bounds = lane_bounds();
    let mut squares = 0;
    let mut lane = 0;
    while lane != RANK {
        if 2 * bounds[lane] > i16::MAX as i64 {
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

/// The term's incremental state: the two perspectives' sums added and
/// subtracted (white's less black's), and their diagonals subtracted the
/// same way.
///
/// The lanes wrap rather than check: a position with more pieces than a
/// legal one can stand (a fen can state one) scores wrongly rather than
/// panicking in a debug build.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct Machine {
    sums: [[i16; RANK]; 2],
    diagonal: [i32; LIVE],
}

impl Machine {
    pub(crate) const EMPTY: Self = Self {
        sums: [[0; RANK]; 2],
        diagonal: [0; LIVE],
    };

    /// A piece counted on to or off of a square, from the piece's one row.
    #[inline(always)]
    pub(crate) fn count<const SET: bool>(&mut self, row: &super::Row) {
        // at rank 0 a row holds no lanes and the loops below walk nothing;
        // the return says so rather than leaving it to the optimiser
        if RANK == 0 {
            return;
        }
        for at in 0..2 {
            for (sum, &factor) in self.sums[at].iter_mut().zip(&row.lanes[at]) {
                *sum = if SET {
                    sum.wrapping_add(factor)
                } else {
                    sum.wrapping_sub(factor)
                };
            }
        }
        for (sum, &diagonal) in self.diagonal.iter_mut().zip(&row.diagonal) {
            *sum = if SET {
                sum.wrapping_add(diagonal)
            } else {
                sum.wrapping_sub(diagonal)
            };
        }
    }

    /// A piece moving between two squares, from the two squares' rows.
    #[inline(always)]
    pub(crate) fn relocate(&mut self, left: &super::Row, arrived: &super::Row) {
        // as in `count`
        if RANK == 0 {
            return;
        }
        for at in 0..2 {
            for ((sum, &off), &on) in self.sums[at]
                .iter_mut()
                .zip(&left.lanes[at])
                .zip(&arrived.lanes[at])
            {
                *sum = sum.wrapping_add(on.wrapping_sub(off));
            }
        }
        for ((sum, &off), &on) in self
            .diagonal
            .iter_mut()
            .zip(&left.diagonal)
            .zip(&arrived.diagonal)
        {
            *sum = sum.wrapping_add(on.wrapping_sub(off));
        }
    }

    /// The state the pieces deserve, summed from the table directly rather
    /// than through `count`, for `Accumulator::recomputed`.
    // index loops on purpose: the iterator form clippy asks for changed
    // how llvm compiled the search, 0.2% to 0.8% more of its instructions
    #[allow(clippy::needless_range_loop)]
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
        let (white, black) = (Color::White as usize, Color::Black as usize);
        let mut kept = Self::EMPTY;
        for lane in 0..RANK {
            kept.sums[0][lane] = sums[white][lane].wrapping_add(sums[black][lane]);
            kept.sums[1][lane] = sums[white][lane].wrapping_sub(sums[black][lane]);
        }
        for slot in 0..LIVE {
            kept.diagonal[slot] = diagonal[white][slot].wrapping_sub(diagonal[black][slot]);
        }
        kept
    }

    /// What `relocate` with these two rows would do to [`Machine::score`],
    /// before it is done: with `e` the rows' difference, the numerator
    /// moves by `u·e1 + v·e0 + e0·e1` less the diagonals' difference, where
    /// `u` and `v` are the sum and the difference this keeps. Read in i64,
    /// and truncated once, so it can differ from the change in the
    /// truncated score by one.
    #[inline(always)]
    pub(crate) fn moved(&self, left: &super::Row, arrived: &super::Row) -> i32 {
        if RANK == 0 {
            return 0;
        }
        let mut total = 0_i64;
        for lane in 0..RANK {
            let e0 = i64::from(arrived.lanes[0][lane]) - i64::from(left.lanes[0][lane]);
            let e1 = i64::from(arrived.lanes[1][lane]) - i64::from(left.lanes[1][lane]);
            total += i64::from(self.sums[0][lane]) * e1 + i64::from(self.sums[1][lane]) * e0;
            total += e0 * e1;
        }
        for (&on, &off) in arrived.diagonal.iter().zip(&left.diagonal) {
            total -= i64::from(on) - i64::from(off);
        }
        (total / (2 * Q * Q)) as i32
    }

    /// The term, white relative, in centipawns.
    #[inline(always)]
    pub(crate) fn score(&self) -> i32 {
        if RANK == 0 {
            return 0;
        }
        // `‖s_w‖² - ‖s_b‖²`, which fits i32 (see `in_range`), so the wrapping
        // sum lands on it
        let squares = self.sums[0]
            .iter()
            .zip(&self.sums[1])
            .fold(0_i32, |total, (&sum, &difference)| {
                total.wrapping_add(i32::from(sum) * i32::from(difference))
            });
        let diagonal: i32 = self.diagonal.iter().sum();
        // truncated toward zero, so a mirrored position scores the exact
        // negation of its mirror
        ((i64::from(squares) - i64::from(diagonal)) / (2 * Q * Q)) as i32
    }
}

#[cfg(test)]
mod features {
    use super::{Machine, RANK, feature};
    use crate::bench;
    use crate::board::{Board, fens};
    use crate::eval::{eval, pieces_of, suite_fens};
    use crate::misc::{Color, Piece};
    use pretty_assertions::assert_eq;

    /// The colour mirror of a fen.
    fn mirrored(fen: &str) -> String {
        let fields: Vec<&str> = fen.split(' ').collect();
        let swap = |c: char| {
            if c.is_ascii_uppercase() {
                c.to_ascii_lowercase()
            } else {
                c.to_ascii_uppercase()
            }
        };
        let board: Vec<String> = fields[0]
            .split('/')
            .rev()
            .map(|rank| rank.chars().map(swap).collect())
            .collect();
        let side = if fields[1] == "w" { "b" } else { "w" };
        let castling = if fields[2] == "-" {
            "-".to_string()
        } else {
            let mut rights: Vec<char> = fields[2].chars().map(swap).collect();
            rights.sort_by_key(|c| "KQkq".find(*c));
            rights.into_iter().collect()
        };
        let passant = match fields[3].as_bytes() {
            [file, b'3'] => format!("{}6", *file as char),
            [file, b'6'] => format!("{}3", *file as char),
            _ => fields[3].to_string(),
        };
        let mut out = vec![board.join("/"), side.to_string(), castling, passant];
        out.extend(fields[4..].iter().map(|f| f.to_string()));
        out.join(" ")
    }

    /// A position and its colour mirror score the same over every suite
    /// position, which needs the divide to truncate toward zero.
    #[test]
    fn a_mirrored_position_scores_the_same_across_the_suites() {
        for fen in suite_fens() {
            let board = Board::from_fen(&fen).unwrap();
            let mirror = Board::from_fen(&mirrored(&fen)).unwrap();
            assert_eq!(eval(&board), eval(&mirror), "{}", fen);
        }
    }

    /// Two plies deep from every bench and core position, each unmake gives
    /// the accumulator back. The board's unmake tests compare it as part of
    /// the whole board; this reaches a ply further over more positions.
    #[test]
    fn unmaking_a_move_restores_the_accumulator() {
        let mut fens: Vec<String> = fens::CORE.iter().map(|f| f.to_string()).collect();
        fens.extend(bench::positions().into_iter().map(|p| p.fen));
        for fen in fens {
            let mut board = Board::from_fen(&fen).unwrap();
            let root = board.eval;
            for play in &board.generate_moves() {
                if !board.make_move(play) {
                    continue;
                }
                let after = board.eval;
                for reply in &board.generate_moves() {
                    if board.make_move(reply) {
                        board.undo_move();
                        assert_eq!(board.eval, after, "{} then {}", fen, play);
                    }
                }
                board.undo_move();
                assert_eq!(board.eval, root, "{} then {}", fen, play);
            }
        }
    }

    /// The tests above would pass on a term that scored nothing, so at any
    /// rank but 0 it has to be seen to score.
    #[test]
    fn the_term_scores_something() {
        if RANK == 0 {
            return;
        }
        let scored = suite_fens()
            .iter()
            .map(|fen| {
                let board = Board::from_fen(fen).unwrap();
                Machine::of(pieces_of(&board)).score()
            })
            .filter(|&score| score != 0)
            .count();
        assert!(scored > 1_000, "the term scored {} positions", scored);
    }

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
