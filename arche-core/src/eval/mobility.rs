// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! How many squares each side's pieces cover, and what a square is worth.
//!
//! The term owns its counts, its weights and its fold. Nothing here is
//! remembered between positions: a piece that moves changes what every slider
//! looking through its square sees, so there is no key a score could sit
//! behind and nothing for `Accumulator::count` to add and take away.

use super::weigh;
use crate::board::{Board, knight_attacks, pawn_attacks, pop_lsb};
use crate::magic::MAGIC;
use crate::misc::{Color, Piece};
use crate::psqt::{eg_value, mg_value, pack};

/// The pieces that carry a mobility weight, in the order [`MOBILITY`] and
/// [`counts`] are indexed by. The pawn and the king carry none.
pub(crate) const PIECES: [Piece; 4] = [Piece::Knight, Piece::Bishop, Piece::Rook, Piece::Queen];

/// How many counts the term is measured in, which is one per piece that
/// carries a weight.
pub(crate) const COUNTS: usize = PIECES.len();

/// What one square of scope is worth to each of [`PIECES`], as the packed
/// pairs the taper is read from.
///
/// Refitted 2026-09-26 with every weight but material, in the joint refit the tables' comment in `psqt.rs` describes.
///
/// Before that, refitted 2026-09-13 by `scripts/tune.py` over the whole
/// archived strength run, 43,675 games and 2,461,322 quiet rows extracted by `arche terms` at
/// 2105b55 (corpus sha256 `b37ebf0c`, rows `49aa715a`), K held at 1.1350,
/// every other weight held, at a ridge of zero. The sealed group, opened once
/// afterwards over 9,452 games, scores 0.080507 at the old weights and
/// 0.079997 at these, a paired difference of -0.000510 against a standard
/// error of 0.000107 at a design factor of 4.1; the selection group read
/// -0.000798 against 0.000113, 1.85 standard errors away, so it overstates
/// the fit by about a third. Commit 7e6ddd7 holds the rest.
///
/// The first fit read 1,812 games and priced one piece; the refits since
/// price all four, so [`SCORED_KINDS`] skips nothing and every kind is
/// counted at every leaf. The 2026-09-13 refit's ridge of zero was not the
/// trap it was for the pawn structure fit: its vector quantized identically
/// at zero, 1e-8 and 1e-7, and every slot carried a coefficient in 37% to 70%
/// of the training rows. The largest weight was 7 then and is 7 now.
///
/// By phase the 2026-09-13 sealed reading was -0.000160 at six pieces or fewer,
/// -0.001725 from seven to twelve, and +0.000703 at thirteen or more, so the
/// term costs something in the opening, the phase a mobility count would be
/// expected to earn most. That is worth an ablation and is not one this arm
/// ran.
///
/// A count is at most twenty seven for a queen and a boardful comes to a few
/// hundred, so a weight in single figures leaves the order of magnitude in
/// hand that `pack` asks for.
const MOBILITY: [i32; COUNTS] = [pack(6, 0), pack(7, 2), pack(4, 4), pack(2, 7)];

/// The weight of one piece's count, as the packed pair, read through
/// [`super::TERMS`] so that a slot names the live weight rather than a copy.
pub(crate) const fn weight(index: usize) -> i32 {
    MOBILITY[index]
}

/// A set of [`PIECES`], a bit per index, which is what [`counts_of`] takes. A
/// kind left out of the set is not counted and answers zero. This is all
/// four, which the tuner's walk asks for whatever the weights hold, since a
/// coefficient for a kind worth nothing today is what lets a later fit price
/// it.
pub(crate) const ALL_KINDS: u8 = (1 << COUNTS) - 1;

/// The kinds [`super::eval`] counts: the ones whose [`MOBILITY`] weight is not
/// zero at one end of the taper or the other, derived from the weights so a
/// refit changes the set with nothing else edited. The 2026-09-13 refit
/// priced all eight halves, so this is all four kinds.
/// `eval_counts_a_kind_exactly_when_its_weight_is_not_zero` holds the two
/// together.
pub(crate) const SCORED_KINDS: u8 = scored_kinds();

/// Whether `kinds` names the piece at `index` in [`PIECES`].
pub(crate) const fn counted(kinds: u8, index: usize) -> bool {
    kinds & (1 << index) != 0
}

/// [`SCORED_KINDS`], read off the weights at compile time. Both halves are
/// asked about: a weight worth nothing in the midgame and something in the
/// ending is still a weight and still has to be counted.
const fn scored_kinds() -> u8 {
    let mut kinds = 0;
    let mut index = 0;
    while index < COUNTS {
        let weight = weight(index);
        if mg_value(weight) != 0 || eg_value(weight) != 0 {
            kinds |= 1 << index;
        }
        index += 1;
    }
    kinds
}

/// How many squares this side's knights, bishops, rooks and queens cover, a
/// count per piece kind in the order [`PIECES`] names them. `KINDS` is a
/// compile time set of the kinds to count; a kind left out has its loop not
/// compiled and answers zero.
///
/// A piece's count is its attack set over the real occupancy, less the
/// squares this side stands on, less the squares an enemy pawn attacks. So a
/// friendly piece blocks a slider rather than being seen through, a square an
/// enemy pawn covers is not somewhere a piece goes, and an enemy piece's
/// square stays in the count because attacking it is the point. Pins are
/// ignored: the search is what knows a pinned bishop cannot move. The king
/// and the pawn have no count of their own: a king's is a danger signal
/// rather than a scope one, and a pawn's is move generation.
///
/// The tuner's walk reads this, always for all four kinds, because it is
/// offline and its coefficients are what lets a later fit price a kind. The
/// evaluation reads the shared walk in `eval/mod.rs` over [`SCORED_KINDS`],
/// which `the_shared_walk_counts_what_each_term_counts_alone` holds to this
/// function kind by kind and
/// `eval_counts_a_kind_exactly_when_its_weight_is_not_zero` holds to the
/// weights. A count wrong the same way in both would pass both and the
/// tuner's identity, so the hand counts below are what pin the counts
/// themselves; `tune.rs` names this as the exception to the rule its header
/// states.
///
/// Inlined by force. Left to itself llvm keeps this out of line even under
/// link time optimisation, and the evaluation asked for it twice at every
/// leaf and every quiescence node: callgrind over the bench at 57f2116 read
/// 4.30 billion instructions without the attribute against 3.76 billion with
/// it. The shared walk is inlined by force for the same reason.
#[inline(always)]
pub(crate) fn counts_of<const KINDS: u8>(board: &Board, color: Color) -> [i32; COUNTS] {
    let occupied = board.occupied();
    let (ours, theirs) = board.sides(color);
    let scope = !(ours | pawn_attacks(board.pawns() & theirs, !color));
    let magic = &MAGIC;
    let mut counts = [0; COUNTS];
    if counted(KINDS, 0) {
        let mut knights = board.knights() & ours;
        while knights != 0 {
            let from = pop_lsb(&mut knights);
            counts[0] += (knight_attacks(from) & scope).count_ones() as i32;
        }
    }
    if counted(KINDS, 1) {
        let mut bishops = board.bishops() & ours;
        while bishops != 0 {
            let from = pop_lsb(&mut bishops);
            counts[1] += (magic.get_diagonal_move(from, occupied) & scope).count_ones() as i32;
        }
    }
    if counted(KINDS, 2) {
        let mut rooks = board.rooks() & ours;
        while rooks != 0 {
            let from = pop_lsb(&mut rooks);
            counts[2] += (magic.get_straight_move(from, occupied) & scope).count_ones() as i32;
        }
    }
    if counted(KINDS, 3) {
        let mut queens = board.queens() & ours;
        while queens != 0 {
            let from = pop_lsb(&mut queens);
            let attacks =
                magic.get_straight_move(from, occupied) | magic.get_diagonal_move(from, occupied);
            counts[3] += (attacks & scope).count_ones() as i32;
        }
    }
    counts
}

/// All four counts, written into `into`, which is what [`super::TERMS`] hands
/// the tuner's walk.
pub(crate) fn counts(board: &Board, color: Color, into: &mut [i32]) {
    into.copy_from_slice(&counts_of::<ALL_KINDS>(board, color));
}

/// What white's mobility stands ahead by, as a packed pair on the scale the
/// piece square pair is on, given each side's counts from the walk in
/// `eval/mod.rs`, which counts only [`SCORED_KINDS`]: a kind whose weight is
/// zero contributes nothing however many squares it covers. When the first
/// fit left six of the eight weights at zero, leaving three kinds out took a
/// bit over a third off what the term cost (7b0f38b).
#[inline]
pub(crate) fn fold_counts(white: [i32; COUNTS], black: [i32; COUNTS]) -> i32 {
    weigh(&MOBILITY, white, black)
}

/// The fold with the counts taken by [`counts_of`] over [`SCORED_KINDS`],
/// which is what the sum answered before it shared a walk with the king
/// attack zone. The tests hold the sum to it.
#[cfg(test)]
pub(crate) fn fold(board: &Board) -> i32 {
    fold_with::<SCORED_KINDS>(board, &MOBILITY)
}

/// The same fold over the kinds `KINDS` names, against weights named by the
/// caller. A kind outside `KINDS` counts zero and so has to be worth zero.
/// The tests supply weights of their own because what they pin is the fold
/// rather than the fit: a permuted [`MOBILITY`] would be a different
/// evaluation and not a wrong one.
#[cfg(test)]
fn fold_with<const KINDS: u8>(board: &Board, weights: &[i32; COUNTS]) -> i32 {
    weigh(
        weights,
        counts_of::<KINDS>(board, Color::White),
        counts_of::<KINDS>(board, Color::Black),
    )
}

#[cfg(test)]
mod tests {
    use super::{ALL_KINDS, Board, COUNTS, Color, MOBILITY, PIECES, SCORED_KINDS, counts_of, fold};
    use crate::board::fens;
    use crate::psqt::{eg_value, mg_value, pack};
    use pretty_assertions::assert_eq;

    /// The counts by hand, because nothing else pins them: the tuner reads
    /// this function and `eval` reads a walk held to it, so the identity
    /// between them moves with whatever it answers. The two kings stand in
    /// opposite corners, out of the way and out of check.
    #[test]
    fn a_piece_covers_what_a_hand_count_says_it_does() {
        for (fen, counts, why) in [
            // a knight in the corner has two squares and one in the middle
            // has all eight
            (
                "k7/8/8/8/8/8/8/N6K w - - 0 1",
                [2, 0, 0, 0],
                "a knight on a1",
            ),
            (
                "k7/8/8/8/3N4/8/8/7K w - - 0 1",
                [8, 0, 0, 0],
                "a knight on d4",
            ),
            // rays of three, four, three and three
            (
                "k7/8/8/8/3B4/8/8/7K w - - 0 1",
                [0, 13, 0, 0],
                "a bishop on d4",
            ),
            // a rank and a file, less the square it stands on
            (
                "k7/8/8/8/3R4/8/8/7K w - - 0 1",
                [0, 0, 14, 0],
                "a rook on d4",
            ),
            (
                "k7/8/8/8/3Q4/8/8/7K w - - 0 1",
                [0, 0, 0, 27],
                "a queen on d4",
            ),
            // the friendly pawn on d6 is not scope and is not seen through
            // either, so the file gives d5, d3, d2 and d1 beside the rank
            (
                "k7/8/3P4/8/3R4/8/8/7K w - - 0 1",
                [0, 0, 11, 0],
                "a rook on d4 behind its own pawn",
            ),
            // the pawn on b7 covers c6, which is one of the knight's eight
            (
                "k7/1p6/8/8/3N4/8/8/7K w - - 0 1",
                [7, 0, 0, 0],
                "a knight on d4 against a pawn on b7",
            ),
            // the enemy rook stands on one of the same eight and keeps it:
            // a square with something to take on it is still scope
            (
                "k7/8/2r5/8/3N4/8/8/7K w - - 0 1",
                [8, 0, 0, 0],
                "a knight on d4 against a rook on c6",
            ),
            // the enemy pawn stops the file at d6 and the ray does not carry
            // past it, so the file gives d5 and d6 beside the rank: the magic
            // lookup is asked about the whole occupancy, not this side's half
            (
                "8/2k5/3p4/8/3R4/8/8/6K1 w - - 0 1",
                [0, 0, 12, 0],
                "a rook on d4 in front of an enemy pawn",
            ),
        ] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(
                counts_of::<{ ALL_KINDS }>(&board, Color::White),
                counts,
                "{}",
                why
            );
            super::super::evaluate::the_shared_walk_agrees(&board, why);
        }
    }

    /// Black's count of a position is white's count of its reflection, so the
    /// two colours are read the same way round.
    #[test]
    fn the_two_colours_count_the_same_squares() {
        let white = Board::from_fen("7k/1p6/8/8/3N4/8/8/7K w - - 0 1").unwrap();
        let black = Board::from_fen("7k/8/8/3n4/8/8/1P6/7K b - - 0 1").unwrap();
        assert_eq!(
            counts_of::<{ ALL_KINDS }>(&white, Color::White),
            counts_of::<{ ALL_KINDS }>(&black, Color::Black)
        );
        assert_eq!(
            counts_of::<{ ALL_KINDS }>(&white, Color::Black),
            [0, 0, 0, 0]
        );
    }

    /// The position the two tests below are read against, and what each side
    /// covers in it, worked out square by square rather than read back off
    /// [`counts_of`].
    ///
    /// White's knight on b1 has a3, c3 and d2. Its bishop on f1 has g2 and h3,
    /// the pawn on e2 standing in the way of the other diagonal. Its rook on
    /// a1 has the a file, the knight beside it being neither scope nor
    /// something to see through. Its queen on d1 has c1, then c2, b3 and a4,
    /// then the d file up to the black queen it takes, less d6, which the
    /// black pawn on e7 covers.
    ///
    /// Black has no knight. Its bishop on c8 has b7 and a6 one way and d7 out
    /// to h3 the other. Its rook on h8 has the h file and g8 and f8. Its queen
    /// on d8 has c7, b6 and a5, then the d file down to the white queen, less
    /// d3, which the white pawn on e2 covers.
    const COUNTED: &str = "2bqk2r/4p3/8/8/8/8/4P3/RN1QKB2 w - - 0 1";
    const WHITE_COVERS: [i32; COUNTS] = [3, 2, 7, 10];
    const BLACK_COVERS: [i32; COUNTS] = [0, 7, 9, 9];

    /// Four weights that differ from each other at both ends of the taper, so
    /// that a pair read into the wrong piece's slot lands on a different
    /// number. The four differences are 3, -5, -2 and 1, which differ from
    /// each other too, so a permutation of either array shows.
    const TRIAL: [i32; COUNTS] = [pack(11, 2), pack(-7, 13), pack(3, -5), pack(29, 41)];

    /// What the fold does with weights that are not the shipped ones, which
    /// are the fit's and will move again: white's count less black's, piece
    /// by piece, each half of the pair summed on its own.
    #[test]
    fn the_mobility_fold_reads_white_less_black_piece_by_piece() {
        let board = Board::from_fen(COUNTED).unwrap();
        assert_eq!(
            counts_of::<{ ALL_KINDS }>(&board, Color::White),
            WHITE_COVERS
        );
        assert_eq!(
            counts_of::<{ ALL_KINDS }>(&board, Color::Black),
            BLACK_COVERS
        );
        let midgame: i32 = (0..COUNTS)
            .map(|i| mg_value(TRIAL[i]) * (WHITE_COVERS[i] - BLACK_COVERS[i]))
            .sum();
        let endgame: i32 = (0..COUNTS)
            .map(|i| eg_value(TRIAL[i]) * (WHITE_COVERS[i] - BLACK_COVERS[i]))
            .sum();
        assert_ne!(
            midgame, endgame,
            "the two halves would not tell a swap apart"
        );
        let packed = super::fold_with::<{ ALL_KINDS }>(&board, &TRIAL);
        assert_ne!(midgame, 0, "black less white would answer the same here");
        assert_eq!((mg_value(packed), eg_value(packed)), (midgame, endgame));
    }

    /// What the evaluation is allowed to leave out, which is the whole of the
    /// contract between it and the tuner's walk: the difference between
    /// `counts_of::<ALL_KINDS>` and `counts_of::<SCORED_KINDS>` is exactly the
    /// kinds whose weight is zero, kind by kind, and the two fold to the same
    /// packed pair at the shipped weights. A later fit that puts a kind back
    /// at zero fails here rather than being counted at every leaf for nothing.
    ///
    /// The position has to give every kind of both colours something to
    /// cover, or a skipped kind and a kind that covers nothing read the same.
    #[test]
    fn eval_counts_a_kind_exactly_when_its_weight_is_not_zero() {
        let board = Board::from_fen(fens::KIWIPETE).unwrap();
        for color in [Color::White, Color::Black] {
            let all = counts_of::<{ ALL_KINDS }>(&board, color);
            let scored = counts_of::<{ SCORED_KINDS }>(&board, color);
            for (index, piece) in PIECES.into_iter().enumerate() {
                assert_ne!(
                    all[index], 0,
                    "{:?} covers nothing here, so this position cannot tell a skipped kind \
                     from a counted one",
                    piece
                );
                let weight = super::weight(index);
                let priced = mg_value(weight) != 0 || eg_value(weight) != 0;
                assert_eq!(
                    scored[index],
                    if priced { all[index] } else { 0 },
                    "{:?} is worth {} in the midgame and {} in the ending, so it should be \
                     {}, and counts_of::<SCORED_KINDS> counted {} of its {} squares",
                    piece,
                    mg_value(weight),
                    eg_value(weight),
                    if priced { "counted" } else { "skipped" },
                    scored[index],
                    all[index]
                );
            }
        }
        let whole = super::fold_with::<{ ALL_KINDS }>(&board, &MOBILITY);
        assert_ne!(
            whole, 0,
            "mobility is level here, so this position says nothing about the fold"
        );
        assert_eq!(fold(&board), whole);
    }
}
