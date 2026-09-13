// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! How many squares each side's pieces cover, and what a square is worth.
//!
//! The term owns its counts, its weights and its fold. Nothing here is
//! remembered between positions: a piece that moves changes what every slider
//! looking through its square sees, so there is no key a score could sit
//! behind and nothing for `Accumulator::count` to add and take away.

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
/// Refitted 2026-09-13 by `scripts/tune.py` over the whole archived strength
/// run: 259 artifacts across 54 runs, 43,675 games, whose 5,694,775 post-book
/// plies gave 5,355,792 positions and 2,461,322 quiet rows, extracted by
/// `arche terms` at 2105b55. The corpus is sha256 `b37ebf0c` and the rows are
/// sha256 `49aa715a`. K was held at 1.1350 and the games split by pair:
/// 12,743 pairs trained, 4,339 chose the ridge and 4,747 were sealed. The 768
/// table entries, the six material values, the fourteen shelter weights and
/// the sixteen pawn structure ones were all held, so these eight are the only
/// thing that moved. Every number here is of the rounded vector that ships.
///
/// The selection group scores 0.087671 at the old weights and 0.086874 at
/// these, a paired difference of -0.000798 against a standard error of
/// 0.000113 at a design factor of 3.8. The sealed group, opened once
/// afterwards over 4,747 pairs and 9,452 games that no fit and no ridge
/// choice had read, scores 0.080507 and 0.079997, a paired difference of
/// -0.000510 against a standard error of 0.000107 at a design factor of 4.1.
/// Both are outside their intervals, and they are 1.85 standard errors apart,
/// so the selection group overstates this fit by about a third. The king
/// safety fit's two agreed to 0.68 and the pawn structure fit's to 0.15.
///
/// The first fit of this term read 1,812 games and priced one piece. This one
/// reads twenty four times as many and prices all four, which is the whole of
/// what changed: nothing about the term is different, the corpus simply has
/// enough games to say something about a knight. A preliminary fit taken
/// before the pawn structure term existed predicted `[5,6,5,1]` and
/// `[0,2,2,7]`, and it came out `[4,6,4,1]` and `[1,3,4,7]`. Pawn structure
/// took almost nothing away from mobility, despite both terms reading open
/// lines.
///
/// The ridge is zero, which the grid ranked first, and here that is not the
/// trap it was for the pawn structure fit. The vector quantizes identically
/// at zero, 1e-8 and 1e-7, the largest weight is 7, and every one of the
/// eight slots carries a coefficient in 37% to 70% of the training rows with
/// summed coefficients in the tens of millions. There is no low leverage
/// direction here for an unregularised fit to hide error in.
///
/// By phase the sealed reading is -0.000160 at six pieces or fewer,
/// -0.001725 from seven to twelve, and +0.000703 at thirteen or more. The
/// term pays in the middlegame, barely in the ending, and costs something in
/// the opening, which is the phase a mobility count would be expected to earn
/// most. That is worth an ablation and is not one this arm ran.
///
/// Every weight is non-zero, so [`SCORED_KINDS`] has nothing left to skip and
/// all four kinds are counted at every leaf again. What that costs is in the
/// commit that landed this, and it is the larger half of the arm.
///
/// A count is at most twenty seven for a queen and a boardful comes to a few
/// hundred, so a weight in single figures leaves the same order of magnitude
/// in hand that `pack` asks for.
const MOBILITY: [i32; COUNTS] = [pack(4, 1), pack(6, 3), pack(4, 4), pack(1, 7)];

/// The weight of one of the four pieces that carries one, as the packed pair.
/// The tuner's seam asks through [`super::TERMS`], so that a slot names the
/// live weight rather than a copy of it, the way it reads the tables.
pub(crate) const fn weight(index: usize) -> i32 {
    MOBILITY[index]
}

/// A set of [`PIECES`], a bit per index, which is what [`counts_of`] takes. A
/// kind left out of the set is not counted and answers zero.
///
/// This is all four of them. The tuner's walk asks for it whatever the
/// weights hold, because that walk is offline and its coefficients are what
/// lets a later fit price a kind that is worth nothing today.
pub(crate) const ALL_KINDS: u8 = (1 << COUNTS) - 1;

/// The kinds [`super::eval`] counts: the ones whose [`MOBILITY`] weight is not
/// zero at one end of the taper or the other. The 2026-09-13 refit priced all
/// eight weights, so this is now all four kinds and skips nothing.
///
/// Derived from the weights rather than written out, which is what made the
/// refit start counting three kinds again rather than leaving them ignored at
/// every leaf. It cost what the refit's commit records.
/// `eval_counts_a_kind_exactly_when_its_weight_is_not_zero` is what holds the
/// two together.
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
/// count per piece kind in that order, which is the order [`PIECES`] names
/// them in. `KINDS` says which of the four to count; the rest are not looked
/// at and answer zero.
///
/// A piece's count is its attack set over the real occupancy, less the
/// squares this side stands on, less the squares an enemy pawn attacks. So
/// a friendly piece blocks a slider rather than being seen through, and a
/// square an enemy pawn covers is not somewhere a piece goes. An enemy
/// piece standing on a square keeps that square in the count, because
/// attacking it is the point. Pins are ignored: a pinned bishop counts its
/// squares, and the search is what knows it cannot move.
///
/// The enemy pawns are taken as one span rather than probed a square at a
/// time. The king and the pawn have no count of their own: a king's is a
/// danger signal rather than a scope one, and a pawn's is move generation.
///
/// Both [`super::eval`] and the tuner's walk read this, and they may ask for
/// different kinds. The walk always asks for all four, because it is offline
/// and its coefficients are what prices a kind; the evaluation asks for the
/// kinds whose weight is not zero, because a count multiplied by zero is not
/// worth the leaf it is taken at. Since the refit priced all four the two
/// sets are equal today. What keeps them honest whether or not they are is
/// that the difference is exactly the zero weights, which
/// `eval_counts_a_kind_exactly_when_its_weight_is_not_zero` pins. Within one
/// set of kinds the counts are still one answer rather than two, so a second
/// implementation of them would still be two chances to be wrong rather than a
/// check on one, and the hand counts below are what pins them. That is the
/// exception to the rule `tune.rs` states in its header, which names it.
///
/// `KINDS` is a compile time set, so a kind left out of it costs nothing:
/// its loop is not compiled rather than skipped.
///
/// Inlined by force. Left to itself llvm keeps this out of line even under
/// link time optimisation, and the evaluation asks for it twice at every leaf
/// and every quiescence node. That call was three fifths of what the term cost
/// over the bench: 4.30 billion instructions without the attribute against
/// 3.76 billion with it.
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
/// piece square pair is on.
///
/// Only [`SCORED_KINDS`] are counted. A kind whose weight is zero contributes
/// nothing however many squares it covers, so counting it is work no score
/// can see. The rule is what is written down, not a list: when the first fit
/// left six of the eight weights at zero, leaving three of the four kinds out
/// took a bit over a third off what the term cost. The refit priced all four,
/// so today this counts every kind and the skip is waiting for a weight to
/// round to nothing again.
#[inline]
pub(crate) fn fold(board: &Board) -> i32 {
    fold_with::<SCORED_KINDS>(board, &MOBILITY)
}

/// The same fold over the kinds `KINDS` names, against weights named by the
/// caller. A kind outside `KINDS` counts zero and so has to be worth zero.
///
/// The live weights are the fit's now and no two of the four pairs agree, so
/// the sign of this term, the order of the four pieces and the packing all
/// show in an evaluation the engine prints. The tests supply weights of their
/// own through here anyway, over all four kinds, because what they pin is the
/// fold rather than the fit: a permuted [`MOBILITY`] would be a different
/// evaluation and not a wrong one.
#[inline]
fn fold_with<const KINDS: u8>(board: &Board, weights: &[i32; COUNTS]) -> i32 {
    let white = counts_of::<KINDS>(board, Color::White);
    let black = counts_of::<KINDS>(board, Color::Black);
    let mut packed = 0;
    for ((weight, white), black) in weights.iter().zip(white).zip(black) {
        packed += weight * (white - black);
    }
    packed
}

#[cfg(test)]
mod tests {
    use super::{ALL_KINDS, Board, COUNTS, Color, MOBILITY, PIECES, SCORED_KINDS, counts_of, fold};
    use crate::board::fens;
    use crate::psqt::{eg_value, mg_value, pack};
    use pretty_assertions::assert_eq;

    /// The counts by hand, square by square, because nothing else pins them.
    /// The tuner's identity folds a row against the live weights, and `eval`
    /// and the walk read this same function, so the two sides of the identity
    /// move together whatever it answers. These cases are the only check this
    /// term has.
    ///
    /// Each case names what the count is made of. The two kings stand in
    /// opposite corners and out of the way, so that nothing here is a count of
    /// theirs and no piece is placed giving check.
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
            // past it, so the file gives d5 and d6 beside the rank. This is
            // the case that says the magic lookup is asked about the whole
            // occupancy and not about this side's half of it
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

    /// What the fold does with weights that are not the shipped ones.
    ///
    /// The shipped weights would do here now that no two of them agree.
    /// Weights of this test's own are kept anyway, because the shipped ones
    /// are the fit's and will move again. This hands the fold four pairs that
    /// differ from each other at both ends and asserts the packed pair against
    /// the arithmetic: white's count less black's, piece by piece, each half
    /// of the pair summed on its own.
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
    /// contract between it and the tuner's walk.
    ///
    /// The walk counts all four kinds and the evaluation counts
    /// [`SCORED_KINDS`], so the two no longer read one answer. What makes that
    /// safe is the size of the difference and nothing else: a count multiplied
    /// by zero adds nothing, so a kind worth zero can go uncounted without
    /// moving a score, and any other kind cannot. So this asserts the
    /// difference is exactly that, kind by kind, and then that the two fold to
    /// the same packed pair at the shipped weights.
    ///
    /// A later fit that puts one of the four kinds back at zero fails here,
    /// rather than being silently counted at every leaf for nothing.
    ///
    /// The position has to give every kind of both colours something to cover,
    /// or a kind that is skipped and a kind that covers nothing read the same
    /// and the test passes without having looked at anything.
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
                     {}, and eval counted {} of its {} squares",
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
