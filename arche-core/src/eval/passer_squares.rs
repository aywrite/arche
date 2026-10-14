// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What the squares in front of a passed pawn say about it: where the two
//! kings stand from its stop and queening squares, whether the stop is taken
//! or attacked, and whether anything stands between it and the eighth.
//!
//! The structure term prices a passer by its file and rank alone, so that it
//! can sit behind the pawn key. These read the pieces, so they are computed
//! at the leaf.

use super::pawn_structure;
#[cfg(test)]
use crate::board::Board;
use crate::board::{Position, king_attacks, knight_attacks};
use crate::magic::MAGIC;
use crate::misc::Color;
use crate::psqt::pack;

/// The first relative rank the term reads, counted from zero: the fourth.
/// A passer further back is far from queening, and the term was not fitted
/// for it.
const FIRST_RANK: usize = 3;
/// One bucket for each of the relative fourth to seventh.
const BUCKETS: usize = 4;

/// What each bucket counts, in this order.
const FEATURES: usize = 7;
const OWN_STOP: usize = 0;
const THEIR_STOP: usize = 1;
const OWN_QUEEN: usize = 2;
const THEIR_QUEEN: usize = 3;
const STOP_TAKEN: usize = 4;
const STOP_ATTACKED: usize = 5;
const FREE_PATH: usize = 6;

/// The counts, by relative rank bucket and then feature.
pub(crate) const COUNTS: usize = BUCKETS * FEATURES;

/// The squares a white passer is read on, the fourth to the seventh rank,
/// and a black one, the fifth down to the second.
const WHITE_RANKS: u64 = 0x00ff_ffff_ff00_0000;
const BLACK_RANKS: u64 = 0x0000_00ff_ffff_ff00;

/// What one count is worth, as packed pairs.
///
/// Fitted 2026-10-14 on 5,026,360 quiet positions from 109,141 games with
/// every other weight held, each weight held to the sign chess expects of it.
/// Without that bound the fit put a blockaded passer on the seventh above a
/// free one in the ending and paid an attacked stop in the midgame, which is
/// what a corpus of quiet positions teaches: a free passer on the seventh with
/// its side to move promotes in quiescence and is not kept. The bound took the
/// held-out gain from 0.000440 to 0.000393. `tune.py` has no such bound, so a
/// refit there gives the unbounded weights back. The king distances are in
/// king steps.
///
/// On the seventh the stop is the path, so a free path there is exactly a
/// stop not taken, and the split between those two weights is the bound's
/// rather than the corpus's.
static PASSER_SQUARES: [i32; COUNTS] = [
    // fourth
    pack(0, -8),
    pack(2, 7),
    pack(0, 0),
    pack(0, 1),
    pack(-12, 0),
    pack(0, -14),
    pack(0, 14),
    // fifth
    pack(0, -10),
    pack(1, 5),
    pack(0, 0),
    pack(0, 7),
    pack(-7, 0),
    pack(0, -22),
    pack(14, 13),
    // sixth
    pack(0, -10),
    pack(6, 0),
    pack(0, 0),
    pack(0, 16),
    pack(-13, 0),
    pack(0, -45),
    pack(21, 26),
    // seventh, where the stop is the queening square and the two queening
    // distances would repeat the stop's
    pack(-6, -6),
    pack(17, 8),
    pack(0, 0),
    pack(0, 0),
    pack(0, 0),
    pack(0, -41),
    pack(154, 0),
];

/// The weight of one count, as the packed pair, read through
/// [`super::TERMS`] so that a slot names the live weight rather than a copy.
pub(crate) fn weight(index: usize) -> i32 {
    PASSER_SQUARES[index]
}

/// Whether [`super::sum`] takes this term at the leaf: true while any weight
/// is not zero, so weights put back to zero turn it off and nothing is read.
pub(crate) const SCORED: bool = {
    let mut index = 0;
    let mut any = false;
    while index < COUNTS {
        any |= PASSER_SQUARES[index] != 0;
        index += 1;
    }
    any
};

/// The king distance between two squares: the more of the rank and file
/// steps between them.
const fn steps(a: usize, b: usize) -> u8 {
    let files = (a % 8).abs_diff(b % 8);
    let ranks = (a / 8).abs_diff(b / 8);
    (if files > ranks { files } else { ranks }) as u8
}

/// [`steps`] for every pair of squares, so the leaf reads a distance rather
/// than working one out.
static DISTANCE: [[u8; 64]; 64] = {
    let mut table = [[0; 64]; 64];
    let mut a = 0;
    while a < 64 {
        let mut b = 0;
        while b < 64 {
            table[a][b] = steps(a, b);
            b += 1;
        }
        a += 1;
    }
    table
};

fn distance(a: u8, b: u8) -> i32 {
    i32::from(DISTANCE[usize::from(a)][usize::from(b)])
}

/// This side's passers on the relative fourth to seventh.
#[inline]
fn read_passers(board: &Position, color: Color) -> u64 {
    let ranks = match color {
        Color::White => WHITE_RANKS,
        Color::Black => BLACK_RANKS,
    };
    pawn_structure::passers(board, color) & ranks
}

/// Both sides' passers on the relative fourth to seventh, in one set: the
/// pawns alone decide it, so [`super::Caches`] keeps it under the pawn key,
/// and the side occupancy tells the two colours apart again.
#[inline]
pub(crate) fn passers(board: &Position) -> u64 {
    read_passers(board, Color::White) | read_passers(board, Color::Black)
}

/// This side's counts, summed over its passers on the fourth to the seventh.
pub(crate) fn counts_of(board: &Position, color: Color) -> [i32; COUNTS] {
    let mut counts = [0; COUNTS];
    let other = !color;
    let occupied = board.occupied();
    let own_king = board.king_index(color);
    let their_king = board.king_index(other);
    let mut passers = read_passers(board, color);
    while passers != 0 {
        let square = passers.trailing_zeros() as u8;
        passers &= passers - 1;
        let (relative, stop, queen) = match color {
            Color::White => (usize::from(square / 8), square + 8, 56 + square % 8),
            Color::Black => (7 - usize::from(square / 8), square - 8, square % 8),
        };
        let base = (relative - FIRST_RANK) * FEATURES;
        counts[base + OWN_STOP] += distance(own_king, stop);
        counts[base + THEIR_STOP] += distance(their_king, stop);
        if relative < 6 {
            counts[base + OWN_QUEEN] += distance(own_king, queen);
            counts[base + THEIR_QUEEN] += distance(their_king, queen);
        }
        counts[base + STOP_TAKEN] += i32::from(occupied & (1u64 << stop) != 0);
        counts[base + STOP_ATTACKED] += i32::from(board.square_attacked(stop, other));
        let path = pawn_structure::file_ahead(square, color);
        counts[base + FREE_PATH] += i32::from(occupied & path == 0);
    }
    counts
}

/// The counts, written into `into`, which is what [`super::TERMS`] hands the
/// tuner's walk.
pub(crate) fn counts(board: &Position, color: Color, into: &mut [i32]) {
    into.copy_from_slice(&counts_of(board, color));
}

/// Whether `by`'s pieces bear on a passer's stop square. `square_attacked`
/// without its pawn test: a pawn of theirs on a file beside the passer and
/// ahead of it would have stopped it being passed, so none can reach its
/// stop.
#[inline]
fn stop_attacked(board: &Position, stop: u8, by: Color, occupied: u64) -> bool {
    let (theirs, _) = board.sides(by);
    if knight_attacks(stop) & board.knights() & theirs != 0
        || king_attacks(stop) & board.kings() & theirs != 0
    {
        return true;
    }
    let queens = board.queens();
    let diagonal = (board.bishops() | queens) & theirs;
    let straight = (board.rooks() | queens) & theirs;
    (diagonal != 0 && MAGIC.get_diagonal_move(stop, occupied) & diagonal != 0)
        || (straight != 0 && MAGIC.get_straight_move(stop, occupied) & straight != 0)
}

/// One side's passers' squares as a packed pair: [`counts_of`] weighed, a
/// passer at a time, so the leaf builds no count array.
#[inline]
fn side_score(board: &Position, color: Color, mut passers: u64, occupied: u64) -> i32 {
    let other = !color;
    let own_king = board.king_index(color);
    let their_king = board.king_index(other);
    let mut score = 0;
    while passers != 0 {
        let square = passers.trailing_zeros() as u8;
        passers &= passers - 1;
        let (relative, stop, queen) = match color {
            Color::White => (usize::from(square / 8), square + 8, 56 + square % 8),
            Color::Black => (7 - usize::from(square / 8), square - 8, square % 8),
        };
        // the set holds only the fourth to seventh, and the mask says so to
        // the bounds checks
        let base = ((relative - FIRST_RANK) & (BUCKETS - 1)) * FEATURES;
        let weight = |feature: usize| PASSER_SQUARES[base + feature];
        // own king to the queening square is fitted at zero in every bucket,
        // so it is not read here, and the other king's is read on the seventh
        // too, where its weight is zero; `the_skipped_counts_are_worth_nothing`
        // holds both
        score += weight(OWN_STOP) * distance(own_king, stop)
            + weight(THEIR_STOP) * distance(their_king, stop)
            + weight(THEIR_QUEEN) * distance(their_king, queen);
        if occupied & (1u64 << stop) != 0 {
            score += weight(STOP_TAKEN);
        } else if occupied & pawn_structure::file_ahead(square, color) == 0 {
            // the stop is on the path, so a taken stop is never a free path
            score += weight(FREE_PATH);
        }
        if stop_attacked(board, stop, other, occupied) {
            score += weight(STOP_ATTACKED);
        }
    }
    score
}

/// White's passers' squares less black's, as a packed pair, from the set
/// [`passers`] gives. Most positions have no passer on the fourth to seventh
/// for either side, and they cost the set and nothing else.
#[inline]
pub(crate) fn fold(board: &Position, passers: u64) -> i32 {
    if passers == 0 {
        return 0;
    }
    let occupied = board.occupied();
    let (white, black) = board.sides(Color::White);
    side_score(board, Color::White, passers & white, occupied)
        - side_score(board, Color::Black, passers & black, occupied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::weigh;

    fn feature(counts: &[i32; COUNTS], relative: usize, feature: usize) -> i32 {
        counts[(relative - FIRST_RANK) * FEATURES + feature]
    }

    #[test]
    fn a_free_passer_on_the_sixth_counts_its_squares() {
        // white pawn e6, white king g1, black king a8: stop e7, queen e8
        let board = Board::from_fen("k7/8/4P3/8/8/8/8/6K1 w - - 0 1").unwrap();
        let white = counts_of(&board, Color::White);
        assert_eq!(feature(&white, 5, OWN_STOP), 6);
        assert_eq!(feature(&white, 5, THEIR_STOP), 4);
        assert_eq!(feature(&white, 5, OWN_QUEEN), 7);
        assert_eq!(feature(&white, 5, THEIR_QUEEN), 4);
        assert_eq!(feature(&white, 5, STOP_TAKEN), 0);
        assert_eq!(feature(&white, 5, STOP_ATTACKED), 0);
        assert_eq!(feature(&white, 5, FREE_PATH), 1);
        assert_eq!(counts_of(&board, Color::Black), [0; COUNTS]);
    }

    #[test]
    fn a_blockaded_passer_on_the_seventh_reads_its_stop() {
        // white pawn e7, black rook e8: the stop is taken, and the path with
        // it. The rook stands on the stop rather than attacking it, and the
        // king on g8 does not reach e8, so the stop is not attacked
        let board = Board::from_fen("4r1k1/4P3/8/8/8/8/8/6K1 w - - 0 1").unwrap();
        let white = counts_of(&board, Color::White);
        assert_eq!(feature(&white, 6, OWN_STOP), 7);
        assert_eq!(feature(&white, 6, THEIR_STOP), 2);
        assert_eq!(feature(&white, 6, OWN_QUEEN), 0);
        assert_eq!(feature(&white, 6, THEIR_QUEEN), 0);
        assert_eq!(feature(&white, 6, STOP_TAKEN), 1);
        assert_eq!(feature(&white, 6, STOP_ATTACKED), 0);
        assert_eq!(feature(&white, 6, FREE_PATH), 0);
    }

    #[test]
    fn a_black_passer_is_read_down_the_board() {
        // black pawn d3 is on its relative sixth; stop d2 attacked by the
        // white king on e1
        let board = Board::from_fen("7k/8/8/8/8/3p4/8/4K3 w - - 0 1").unwrap();
        let black = counts_of(&board, Color::Black);
        assert_eq!(feature(&black, 5, OWN_STOP), 6);
        assert_eq!(feature(&black, 5, THEIR_STOP), 1);
        assert_eq!(feature(&black, 5, OWN_QUEEN), 7);
        assert_eq!(feature(&black, 5, THEIR_QUEEN), 1);
        assert_eq!(feature(&black, 5, STOP_TAKEN), 0);
        assert_eq!(feature(&black, 5, STOP_ATTACKED), 1);
        assert_eq!(feature(&black, 5, FREE_PATH), 1);
    }

    #[test]
    fn a_passer_behind_the_fourth_or_on_the_back_rank_is_not_read() {
        for fen in [
            "k7/8/8/8/8/4P3/8/6K1 w - - 0 1",
            // from_fen accepts a pawn on the eighth; it has no stop
            "k3P3/8/8/8/8/8/8/6K1 w - - 0 1",
        ] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(counts_of(&board, Color::White), [0; COUNTS], "{fen}");
            assert_eq!(fold(&board, passers(&board)), 0, "{fen}");
        }
    }

    /// The fold is the counts weighed, white less black, including where it
    /// returns early.
    #[test]
    fn the_fold_weighs_white_less_black() {
        for fen in [
            "k7/8/4P3/8/8/8/8/6K1 w - - 0 1",
            "4r1k1/4P3/8/8/8/8/8/4R1K1 w - - 0 1",
            "6k1/5ppp/8/P7/8/8/5PPP/r4RK1 w - - 0 1",
            "8/1p3k2/p1p5/P1P1p3/4P1p1/6P1/5K2/8 w - - 0 1",
            "r1bq1rk1/pp3ppp/2n5/8/8/2N5/PPP2PP1/R1BQK2R w KQ - 0 1",
            // passers of both colours with a king beside a stop, a knight on
            // one and a bishop bearing on another
            "8/1P4k1/3K4/2n5/5p2/1b6/6p1/8 w - - 0 1",
        ] {
            let board = Board::from_fen(fen).unwrap();
            let expected = weigh(
                &PASSER_SQUARES,
                counts_of(&board, Color::White),
                counts_of(&board, Color::Black),
            );
            assert_eq!(fold(&board, passers(&board)), expected, "{fen}");
        }
    }

    /// The leaf skips the own king's distance to the queening square, and
    /// reads the other king's on the seventh where [`counts_of`] counts
    /// nothing. Both are right only while those weights are zero.
    #[test]
    fn the_skipped_counts_are_worth_nothing() {
        for bucket in 0..BUCKETS {
            assert_eq!(PASSER_SQUARES[bucket * FEATURES + OWN_QUEEN], 0, "{bucket}");
        }
        assert_eq!(PASSER_SQUARES[(BUCKETS - 1) * FEATURES + THEIR_QUEEN], 0);
    }

    #[test]
    fn the_distance_table_counts_king_steps() {
        assert_eq!(distance(0, 63), 7);
        assert_eq!(distance(6, 52), 6);
        assert_eq!(distance(62, 52), 2);
        assert_eq!(distance(12, 12), 0);
        assert_eq!(distance(4, 11), 1);
    }

    /// The fitted weights, read as chess: the other king's distance and a
    /// free path never count against the passer, and its own king's
    /// distance, a taken stop and an attacked one never count for it.
    #[test]
    fn every_weight_has_the_sign_it_was_fitted_under() {
        use crate::psqt::{eg_value, mg_value};
        for (index, weight) in PASSER_SQUARES.iter().enumerate() {
            let feature = index % FEATURES;
            let favours = matches!(feature, THEIR_STOP | THEIR_QUEEN | FREE_PATH);
            for half in [mg_value(*weight), eg_value(*weight)] {
                assert!(
                    if favours { half >= 0 } else { half <= 0 },
                    "count {index} has the wrong sign"
                );
            }
        }
    }
}
