// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! How many squares around the enemy king each side's pieces attack, and what
//! that is worth.
//!
//! Nothing here is remembered between positions. The counts read the whole
//! occupancy and every piece of one side, which is the position itself, so
//! the only key a score could sit behind is the position key, which the
//! mobility cache was measured on and turned down.

use super::mobility;
use crate::board::{Board, king_attacks, knight_attacks, pop_lsb};
use crate::magic::MAGIC;
use crate::misc::Color;
use crate::psqt::{eg_value, mg_value, pack};

/// How many counts the term is measured in, which is one per piece that
/// carries a weight. The same four as mobility, in the order
/// [`mobility::PIECES`] names them, so the walk in `eval/mod.rs` that takes
/// both readings at once writes each into the slot the other uses.
pub(crate) const COUNTS: usize = mobility::PIECES.len();

/// What one attacked square of the enemy king's ring is worth to each of
/// [`mobility::PIECES`], as the packed pairs the taper is read from.
///
/// Fitted 2026-09-16 by `scripts/tune.py` over the whole archived strength
/// run, 50,677 games and 2,890,773 quiet rows extracted by `arche terms` at
/// the 820 slot layout (corpus sha256 `a0798d51`, sealed pairs `a723887f`,
/// rows `6001063b`, fitted vector `f65aa57e`), K held at 1.0821, every other
/// weight held, at a ridge of zero. The fit's figure is the sealed group's:
/// opened once at 2026-09-16T10:28:31Z after the vector was frozen, it scores
/// 0.079520 at zero and 0.079377 at these, a paired difference of -0.000143
/// against a standard error of 0.000059 over its 6,985 games at a design
/// factor of 3.2. The selection group read -0.000395 against 0.000052, about
/// 3.2 standard errors away, and by the rule the fit was registered with (a
/// disagreement wider than the mobility refit's 1.85) the sealed figure is
/// the one quoted. Commit d3dcc53 holds the phase split, the column support
/// and the learning curve, and docs/ROADMAP.md what the term leaves out.
///
/// The sealed file names 3,501 pairs over 6,995 games against the 3,498 and
/// 6,985 that reached a row. Three of those pairs reach no row, which
/// accounts for at most six of the ten missing games, and where the other
/// four went is not established.
///
/// The ridge of zero was not overruled: the largest weight is the rook's 27,
/// and the rook column is the best supported of the four.
///
/// [`SCORED`] is true at these weights, so the leaf counts the ring at every
/// evaluation; what that costs is in docs/ROADMAP.md and 7991f40.
///
/// `bounds_hold` charges one piece of each kind two squares of the ring for a
/// knight, three for a bishop, four for a rook and six for a queen, both sides
/// counted. That puts this term at 584 in the midgame and the whole vector's
/// boardful at 9,762, against the 32,767 a half has to stay inside.
static KING_ATTACK: [i32; COUNTS] = [pack(11, 3), pack(20, -2), pack(27, -3), pack(17, 9)];

/// The weight of one piece's count, as the packed pair, read through
/// [`super::TERMS`] so that a slot names the live weight rather than a copy.
pub(crate) const fn weight(index: usize) -> i32 {
    KING_ATTACK[index]
}

/// Whether [`super::sum`] takes this term at the leaf: true when one of the
/// four [`KING_ATTACK`] weights is not zero at either end of the taper.
/// Derived from the weights the way [`mobility::SCORED_KINDS`] is, so weights
/// put back to zero turn the term off with nothing else edited.
///
/// A count at a zero weight is not folded away: llvm leaves the walk over
/// the pieces standing, and the commit that added this term at zero weight
/// (188297f) measured what that cost over the bench.
/// `the_term_is_counted_exactly_when_a_weight_is_not_zero` holds the constant
/// and the weights together.
pub(crate) const SCORED: bool = scored(&KING_ATTACK);

/// Whether `weights` prices anything, read at compile time. Both halves are
/// asked about: a weight worth nothing in the midgame and something in the
/// ending is still a weight and still has to be counted.
const fn scored(weights: &[i32; COUNTS]) -> bool {
    let mut index = 0;
    while index < COUNTS {
        if mg_value(weights[index]) != 0 || eg_value(weights[index]) != 0 {
            return true;
        }
        index += 1;
    }
    false
}

/// How many squares of the other king's ring this side's knights, bishops,
/// rooks and queens attack, a count per piece kind in the order
/// [`mobility::PIECES`] names them.
///
/// The ring is the eight squares a king attacks from where it stands, five on
/// the edge and three in a corner. The king's own square is not in it: no
/// quiet position carries an attack on it, since the side to move out of
/// check is one of the three conditions a tuned row meets and the other side
/// cannot be in check at all, so a column counting it is one no fit could
/// price. The rank beyond the ring is not in it either; the shelter already
/// counts the pawns standing there.
///
/// A piece's count is its attack set over the real occupancy with nothing
/// taken out, which is where this parts company with
/// [`mobility::counts_of`]: a rook bears on the pawn in front of the king
/// whether or not a bishop guards it, and a knight standing in the ring is a
/// square the rook behind it bears on. So a slider stops at the first piece
/// of either colour, counts that square if the ring holds it, and sees
/// nothing past it. Pins are ignored, as mobility ignores them.
///
/// A ring square two pieces attack is counted twice, once in each piece's
/// popcount. How many attackers bear on the king is the signal, and a union
/// per kind would lose it.
///
/// Pawns and kings are left out. The storm already reads the enemy pawns on
/// the three ranks in front of the king, and a king bearing on the other
/// king's ring is the opposition, which the endgame table prices.
///
/// The tuner's walk reads this and the evaluation reads the shared walk in
/// `eval/mod.rs`, which `the_shared_walk_counts_what_each_term_counts_alone`
/// holds to this function, so neither the identity nor that test can see a
/// wrong count here; the hand counts in the tests below are what pin it.
///
/// Inlined by force, for the reason `mobility::counts_of` gives.
#[inline(always)]
pub(crate) fn counts_of(board: &Board, color: Color) -> [i32; COUNTS] {
    let occupied = board.occupied();
    let (ours, _) = board.sides(color);
    let ring = king_attacks(board.king_index(!color));
    let magic = &MAGIC;
    let mut counts = [0; COUNTS];
    let mut knights = board.knights() & ours;
    while knights != 0 {
        let from = pop_lsb(&mut knights);
        counts[0] += (knight_attacks(from) & ring).count_ones() as i32;
    }
    let mut bishops = board.bishops() & ours;
    while bishops != 0 {
        let from = pop_lsb(&mut bishops);
        counts[1] += (magic.get_diagonal_move(from, occupied) & ring).count_ones() as i32;
    }
    let mut rooks = board.rooks() & ours;
    while rooks != 0 {
        let from = pop_lsb(&mut rooks);
        counts[2] += (magic.get_straight_move(from, occupied) & ring).count_ones() as i32;
    }
    let mut queens = board.queens() & ours;
    while queens != 0 {
        let from = pop_lsb(&mut queens);
        let attacks =
            magic.get_straight_move(from, occupied) | magic.get_diagonal_move(from, occupied);
        counts[3] += (attacks & ring).count_ones() as i32;
    }
    counts
}

/// The four counts, written into `into`, which is what [`super::TERMS`] hands
/// the tuner's walk.
pub(crate) fn counts(board: &Board, color: Color, into: &mut [i32]) {
    into.copy_from_slice(&counts_of(board, color));
}

/// What white's bearing on the black king stands ahead by, as a packed pair on
/// the scale the piece square pair is on, given each side's counts. The sum
/// calls it only while [`SCORED`] is true, with counts from the walk in
/// `eval/mod.rs` that probes each attack set once for both terms.
#[inline]
pub(crate) fn fold_counts(white: [i32; COUNTS], black: [i32; COUNTS]) -> i32 {
    weigh(&KING_ATTACK, white, black)
}

/// The fold with the counts taken by [`counts_of`], which is what the sum
/// answered before it shared a walk with mobility. The tests hold the sum to
/// it.
#[cfg(test)]
pub(crate) fn fold(board: &Board) -> i32 {
    fold_with(board, &KING_ATTACK)
}

/// The same fold against weights named by the caller.
#[cfg(test)]
fn fold_with(board: &Board, weights: &[i32; COUNTS]) -> i32 {
    weigh(
        weights,
        counts_of(board, Color::White),
        counts_of(board, Color::Black),
    )
}

/// White's counts less black's, piece by piece, against `weights`.
#[inline]
fn weigh(weights: &[i32; COUNTS], white: [i32; COUNTS], black: [i32; COUNTS]) -> i32 {
    let mut packed = 0;
    for ((weight, white), black) in weights.iter().zip(white).zip(black) {
        packed += weight * (white - black);
    }
    packed
}

#[cfg(test)]
mod tests {
    use super::{Board, COUNTS, Color, SCORED, counts_of, king_attacks, scored};
    use crate::psqt::{eg_value, mg_value, pack};
    use pretty_assertions::assert_eq;

    /// The counts by hand, because nothing else pins them: the tuner reads
    /// this function and `eval` reads a walk held to it, so the identity
    /// between them moves with whatever it answers.
    ///
    /// The black king stands on e8 unless the case says otherwise, so its ring
    /// is d7, e7, f7, d8 and f8. The white king stands off every line the case
    /// is about, except in the row that is about a king. Each row names the
    /// colour whose pieces are counted, since one of them reads black's.
    #[test]
    fn a_piece_bears_on_what_a_hand_count_says_it_does() {
        for (fen, color, counts, why) in [
            (
                "4k3/8/2N5/8/8/8/8/7K w - - 0 1",
                Color::White,
                [2, 0, 0, 0],
                "a knight on c6 has d8 and e7 of its eight",
            ),
            (
                "4k3/8/8/2B5/8/8/8/7K w - - 0 1",
                Color::White,
                [0, 2, 0, 0],
                "a bishop on c5 has e7 and f8 on one diagonal and nothing on the other three",
            ),
            (
                "4k3/R7/8/8/8/8/8/7K w - - 0 1",
                Color::White,
                [0, 0, 3, 0],
                "a rook on a7 has d7, e7 and f7 along the rank",
            ),
            // a queen is the queen column and not a rook's plus a bishop's
            (
                "4k3/8/8/8/7Q/8/8/K7 w - - 0 1",
                Color::White,
                [0, 0, 0, 2],
                "a queen on h4 has e7 and d8 on the diagonal",
            ),
            // the rook stops at its own knight, and d7 is still a square it
            // bears on. The knight counts f8 from inside the ring
            (
                "4k3/R2N4/8/8/8/8/8/7K w - - 0 1",
                Color::White,
                [1, 0, 1, 0],
                "a rook on a7 behind its own knight on d7",
            ),
            // the defender's pawn stops the rook and its square is attacked
            (
                "4k3/R2p4/8/8/8/8/8/7K w - - 0 1",
                Color::White,
                [0, 0, 1, 0],
                "a rook on a7 against a pawn on d7",
            ),
            // d7, e7 and f7 read by both pieces, so the rank is worth three
            // to each of the two columns rather than three between them
            (
                "4k3/R6Q/8/8/8/8/8/K7 w - - 0 1",
                Color::White,
                [0, 0, 3, 3],
                "a rook on a7 and a queen on h7 doubled along the rank",
            ),
            // the same rank read by two pieces of one kind, which is the case
            // a union per kind would answer three to
            (
                "4k3/R6R/8/8/8/8/8/K7 w - - 0 1",
                Color::White,
                [0, 0, 6, 0],
                "two rooks on a7 and h7, each with d7, e7 and f7",
            ),
            // a piece standing in the ring bears on the ring from inside it,
            // and is a square the piece behind it bears on
            (
                "4k3/5N2/8/8/8/8/8/K4R2 w - - 0 1",
                Color::White,
                [1, 0, 1, 0],
                "a knight on f7 has d8 and a rook on f1 has the knight's square",
            ),
            // the bishop cannot move and still bears on the two squares. What
            // knows it cannot move is the search
            (
                "2r1k3/8/8/2B5/8/8/8/2K5 w - - 0 1",
                Color::White,
                [0, 2, 0, 0],
                "a bishop on c5 pinned against the king on c1",
            ),
            // a corner ring is g7, g8 and h7, and the king's own square is
            // not in it however the queen bears on it. Black is to move
            // because the queen gives check along the file
            (
                "7k/8/8/8/8/8/8/K6Q b - - 0 1",
                Color::White,
                [0, 0, 0, 1],
                "a queen on h1 against a king in the corner",
            ),
            // e3 is covered by the white pawn on d2 and is attacked all the
            // same. Mobility's scope would drop it
            (
                "k7/8/8/8/4K3/8/3P4/3n4 w - - 0 1",
                Color::Black,
                [1, 0, 0, 0],
                "a knight on d1 against a king on e4 behind a pawn on d2",
            ),
            // a pawn covering the ring carries no column of its own
            (
                "4k3/8/2P5/8/8/8/8/7K w - - 0 1",
                Color::White,
                [0, 0, 0, 0],
                "a pawn on c6 covering d7",
            ),
            // nor does a king, however much of the ring it stands against.
            // The two are two ranks apart, which is as close as they come
            (
                "4k3/8/4K3/8/8/8/8/8 w - - 0 1",
                Color::White,
                [0, 0, 0, 0],
                "a king on e6 covering d7, e7 and f7",
            ),
            (
                "4k3/8/8/8/8/8/8/4K3 w - - 0 1",
                Color::White,
                [0, 0, 0, 0],
                "the two kings alone",
            ),
        ] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(counts_of(&board, color), counts, "{}", why);
            super::super::evaluate::the_shared_walk_agrees(&board, why);
        }
    }

    /// The ring is the squares a king attacks and never the one it stands on,
    /// so it is eight squares in the middle, five on the edge and three in a
    /// corner.
    #[test]
    fn every_king_square_names_the_ring_around_it() {
        for square in 0..64u8 {
            let ring = king_attacks(square);
            let edges = u32::from(square / 8 == 0 || square / 8 == 7)
                + u32::from(square % 8 == 0 || square % 8 == 7);
            let size = match edges {
                0 => 8,
                1 => 5,
                _ => 3,
            };
            assert_eq!(ring.count_ones(), size, "the ring of {}", square);
            assert_eq!(ring & (1 << square), 0, "{} is in its own ring", square);
        }
    }

    /// Black's count of a position is white's count of its reflection, so the
    /// two colours are read the same way round and each reads the other king's
    /// ring rather than its own. Each row is held to a count of its own first:
    /// two reflections named the wrong way round both answer nothing, which an
    /// equality alone would take for agreement.
    #[test]
    fn the_two_colours_count_the_same_squares() {
        for (white, black, why) in [
            (
                "4k3/R6Q/8/8/8/8/8/K7 w - - 0 1",
                "k7/8/8/8/8/8/r6q/4K3 b - - 0 1",
                "a rook and a queen doubled on the rank in front of the king",
            ),
            (
                "3N4/3p4/8/4k3/8/8/8/K7 b - - 0 1",
                "k7/8/8/8/4K3/8/3P4/3n4 w - - 0 1",
                "a knight on a defended square of a centred king's ring",
            ),
        ] {
            let white = Board::from_fen(white).unwrap();
            let black = Board::from_fen(black).unwrap();
            let counts = counts_of(&white, Color::White);
            assert_ne!(
                counts, [0; COUNTS],
                "{} is counted from the side without the pieces",
                why
            );
            assert_eq!(counts, counts_of(&black, Color::Black), "{}", why);
        }
    }

    /// The position the fold test below is read against, and what each side
    /// bears on, worked out square by square rather than read back off
    /// [`counts_of`].
    ///
    /// Black's king on g8 has the ring f7, g7, h7, f8 and h8. White's knight
    /// on e6 has f8 and g7. Its bishop on d3 has h7, through e4, f5 and g6. Its
    /// rook on a7 has d7 and e7 and f7 along the rank, of which f7 alone is in
    /// the ring, and then g7 and h7 as well, which is three. Its queen on h6
    /// has h7 and h8 up the file and g7 and f8 up the diagonal, which is four.
    ///
    /// White's king on g1 has the ring f1, h1, f2, g2 and h2. Black's knight
    /// on b6 reaches none of them. Its bishop on g3 has h2 and f2. Its rook on
    /// a8 is shut in by its own king and reaches none. Its queen on d2 has f2,
    /// g2 and h2 along the rank.
    ///
    /// The four differences are 2, -1, 3 and 1. None is zero and no two agree,
    /// so a permutation of either array shows in the assertion below.
    const BEARING: &str = "r5k1/R7/1n2N2Q/8/8/3B2b1/3q4/6K1 w - - 0 1";
    const WHITE_BEARS: [i32; COUNTS] = [2, 1, 3, 4];
    const BLACK_BEARS: [i32; COUNTS] = [0, 2, 0, 3];

    /// Four weights that differ from each other at both ends of the taper, so
    /// that a pair read into the wrong piece's slot lands on a different
    /// number. The four differences between the halves are 9, -20, 8 and -12,
    /// which differ from each other too.
    const TRIAL: [i32; COUNTS] = [pack(11, 2), pack(-7, 13), pack(3, -5), pack(29, 41)];

    /// What the fold does with weights that are not the shipped ones, which
    /// nothing holds apart from each other: white's count less black's, piece
    /// by piece, each half of the pair summed on its own.
    #[test]
    fn the_king_attack_fold_reads_white_less_black_piece_by_piece() {
        let board = Board::from_fen(BEARING).unwrap();
        assert_eq!(counts_of(&board, Color::White), WHITE_BEARS);
        assert_eq!(counts_of(&board, Color::Black), BLACK_BEARS);
        let midgame: i32 = (0..COUNTS)
            .map(|i| mg_value(TRIAL[i]) * (WHITE_BEARS[i] - BLACK_BEARS[i]))
            .sum();
        let endgame: i32 = (0..COUNTS)
            .map(|i| eg_value(TRIAL[i]) * (WHITE_BEARS[i] - BLACK_BEARS[i]))
            .sum();
        assert_ne!(
            midgame, endgame,
            "the two halves would not tell a swap apart"
        );
        let packed = super::fold_with(&board, &TRIAL);
        assert_ne!(midgame, 0, "black less white would answer the same here");
        assert_eq!((mg_value(packed), eg_value(packed)), (midgame, endgame));
    }

    /// What the leaf is allowed to leave out, which is the whole of the
    /// contract between it and the tuner's walk: [`SCORED`] is the weights
    /// and nothing else, read here off the array a second way, and then a
    /// weight put back into each half of each column in turn.
    #[test]
    fn the_term_is_counted_exactly_when_a_weight_is_not_zero() {
        let priced = (0..COUNTS).any(|index| {
            let weight = super::weight(index);
            mg_value(weight) != 0 || eg_value(weight) != 0
        });
        assert_eq!(SCORED, priced);
        assert!(!scored(&[pack(0, 0); COUNTS]));
        for index in 0..COUNTS {
            let mut midgame = [pack(0, 0); COUNTS];
            midgame[index] = pack(1, 0);
            assert!(
                scored(&midgame),
                "a midgame weight on {} is a weight",
                index
            );
            let mut ending = [pack(0, 0); COUNTS];
            ending[index] = pack(0, -1);
            assert!(
                scored(&ending),
                "an endgame weight on {} is a weight",
                index
            );
        }
    }
}
