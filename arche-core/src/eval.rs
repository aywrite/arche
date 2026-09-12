// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The evaluation: what a position scores, and every number that opinion is
//! built from. The board hosts an [`Accumulator`] and tells it about each
//! piece placed, removed and moved from one square to another; the search
//! asks [`eval`] for the score. A term cheap enough to keep incrementally
//! belongs in the accumulator; one computed at the leaf belongs in [`eval`],
//! reading the board, which is where mobility is.

use crate::board::Board;
use crate::misc::{Color, Piece, Score};
use crate::psqt::{PieceSquareTables, eg_value, mg_value, pack};

static PIECE_SQUARE_TABLES: PieceSquareTables = PieceSquareTables::TABLES;

/// What each piece leaves on the board, in `Piece` order, on the scale the
/// two halves of a tapered score are interpolated on. A queen counts for four,
/// a rook two and a minor one, so the opening's complement of pieces comes to
/// `TOTAL_PHASE` and a bare king and pawns to nothing. Pawns count for nothing
/// because an ending is an ending whether or not there are pawns in it.
static PHASE_WEIGHTS: [i32; 6] = [0, 1, 1, 2, 4, 0];
/// What the opening's pieces add up to under `PHASE_WEIGHTS`.
pub(crate) const TOTAL_PHASE: i32 = 24;

/// A table rather than a match. The match compiled to a jump table, and
/// once the piece arrives as a load from the board's square array the
/// target is data the predictor cannot see through: most of the search's
/// indirect mispredicts were this dispatch inside the accumulator's count.
/// Indexed the way the piece square tables and `Zobrist` already index by
/// piece; the assertion in `misc` pins the discriminants.
const MATERIAL: [u32; 6] = [100, 310, 320, 500, 900, 10000];

/// The material weight of one piece, for the board's own seeding walk.
pub(crate) fn material(piece: Piece) -> u32 {
    MATERIAL[piece as usize]
}

/// The pieces that carry a mobility weight, in the order [`MOBILITY`] and
/// `Board::mobility_counts` are indexed by. The pawn and the king carry none.
pub(crate) const MOBILE_PIECES: [Piece; 4] =
    [Piece::Knight, Piece::Bishop, Piece::Rook, Piece::Queen];

/// What one square of scope is worth to each of [`MOBILE_PIECES`], as the
/// packed pairs the taper is read from.
///
/// Fitted 2026-09-11 by `scripts/tune.py` over the 1,812 archived
/// strength-run games at 10+0.1 the tables were fitted on, whose 229,018
/// post-book plies gave 220,369 positions and 101,046 quiet rows in 1,808 of
/// them, extracted by `arche terms` at 2ff6a25, with K held at 1.4834 and the
/// games split 1,065 that trained, 372 that chose the ridge of 1e-4 and 371
/// that were sealed. The 768 table entries and the six material values were
/// held where they stand, so these eight weights are the only thing that
/// moved. Every number here is of the rounded vector that ships. The
/// selection group scores 0.093203 at zero and 0.093176 at these, and the
/// sealed group 0.083462 and 0.083407, a paired difference of -0.000055
/// against a standard error of 0.000157 over its 371 games. Both intervals
/// cover zero, so the loss favours the fit and settles nothing; the match is
/// what settles it.
///
/// Six of the eight rounded to nothing. Before rounding, the rook stood at
/// 0.667 and 0.828 centipawns a square and no other piece reached half of
/// one either end, so the term ships as a rook count. The other three are
/// not counted at the leaf: `SCORED_KINDS` reads that off the weights.
///
/// The rook's two halves are equal, so its contribution does not taper: a
/// square of its scope is worth the same at either end of the game. That is a
/// fact about this fit rather than about the term, so nothing here takes
/// advantage of it. The taper is what a later fit needs.
///
/// A count is at most twenty seven for a queen and a boardful comes to a few
/// hundred, so a weight in single figures leaves the same order of magnitude
/// in hand that `pack` asks for.
const MOBILITY: [i32; MOBILE_PIECES.len()] = [pack(0, 0), pack(0, 0), pack(1, 1), pack(0, 0)];

/// The mobility weight of one of the four pieces that carries one, as the
/// packed pair. The tuner's seam asks, so that a slot names the live weight
/// rather than a copy of it, the way it reads the tables.
pub(crate) const fn mobility_weight(index: usize) -> i32 {
    MOBILITY[index]
}

/// A set of [`MOBILE_PIECES`], a bit per index, which is what
/// `Board::mobility_counts` takes. A kind left out of the set is not counted
/// and answers zero.
///
/// This is all four of them. The tuner's walk asks for it whatever the
/// weights hold, because that walk is offline and its coefficients are what
/// lets a later fit price a kind that is worth nothing today.
pub(crate) const ALL_KINDS: u8 = (1 << MOBILE_PIECES.len()) - 1;

/// The kinds [`eval`] counts: the ones whose [`MOBILITY`] weight is not zero
/// at one end of the taper or the other. Six of the eight weights are zero
/// today, which leaves the rook.
///
/// Derived from the weights rather than written out, so a refit that prices a
/// kind starts counting it again instead of having it ignored at every leaf.
/// `eval_counts_a_kind_exactly_when_its_weight_is_not_zero` is what holds the
/// two together.
pub(crate) const SCORED_KINDS: u8 = scored_kinds();

/// Whether `kinds` names the piece at `index` in [`MOBILE_PIECES`].
pub(crate) const fn counted(kinds: u8, index: usize) -> bool {
    kinds & (1 << index) != 0
}

/// [`SCORED_KINDS`], read off the weights at compile time. Both halves are
/// asked about: a weight worth nothing in the midgame and something in the
/// ending is still a weight and still has to be counted.
const fn scored_kinds() -> u8 {
    let mut kinds = 0;
    let mut index = 0;
    while index < MOBILE_PIECES.len() {
        let weight = mobility_weight(index);
        if mg_value(weight) != 0 || eg_value(weight) != 0 {
            kinds |= 1 << index;
        }
        index += 1;
    }
    kinds
}

/// How many counts the king's shelter is measured in, and so how many weights
/// it carries at each end of the taper. `Board::shelter_counts` prints them in
/// this order: this side's pawns one rank in front of its king, its pawns two
/// ranks in front, the king's files with no pawn of either colour on them, the
/// king's files holding an enemy pawn and none of ours, and then the enemy
/// pawns one, two and three ranks in front of the king.
pub(crate) const SHELTER_TERMS: usize = 7;

/// What one of those seven counts is worth, as the packed pairs the taper is
/// read from.
///
/// Fitted 2026-09-12 by `scripts/tune.py` over the whole archived strength
/// run: 28,675 games (27,133 at 10+0.1, 1,500 at 30+0.3 and 42 at 2+0.02),
/// whose 3,711,074 post-book plies gave 3,536,193 positions and 1,623,149
/// quiet rows in 28,618 of them, extracted by `arche terms` at 4e5bd7d, with
/// K held at 1.2071 and the games split 17,192 that trained, 5,654 that chose
/// the ridge of 1e-8 and 5,772 that were sealed. The 768 table entries, the
/// six material values and the eight mobility weights were all held where
/// they stand, so these fourteen are the only thing that moved. Every number
/// here is of the rounded vector that ships. The selection group scores
/// 0.090395 at zero and 0.089746 at these, a paired difference of -0.000649
/// against a standard error of 0.000116 over its 5,654 games, at a design
/// factor of 3.9. That is outside its interval, which the mobility fit's
/// reading was not.
///
/// The sealed group has not been opened. What it is for is one reading of a
/// final vector, and the match is what says whether this is one, so a seal
/// spent here on a vector the games then reject could not be spent again.
///
/// Not one of the fourteen rounded to nothing, so every count is priced and
/// none can be left uncounted at the leaf the way `SCORED_KINDS` leaves a
/// mobility kind. All seven are read at every leaf, and what that costs is
/// over what a leaf term is allowed.
///
/// Two of the storm's three signs are not what the term was named for. An
/// enemy pawn one rank in front of the king reads 11 and 35, and three ranks
/// out reads 14, so the fit likes the near storm where the term expected it
/// to fear it, and only the middle rank is negative. Our own cover is worth
/// 21 in the midgame and -26 in the ending, which reads as a king that wants
/// to be active rather than covered. The corpus is the first place to look
/// and not the term: 66.4% of its appearances have six or fewer pieces left
/// on the board and 6.0% have thirteen or more of the fourteen, so the
/// midgame half of the taper, which is the half king safety is about, is
/// fitted on the thinnest slice of the games. The count with the oddest
/// weight is also the thinnest supported: an enemy pawn one rank in front of
/// a king carries a coefficient in 4.65% of the rows against 47.83% for the
/// near cover, because a king usually takes such a pawn and the position is
/// then not quiet. The loss says these fourteen score the corpus better than
/// zero did, and that is all it says.
///
/// A side's seven counts come to eighteen at the very most. Five of them are
/// at most three pawns each, and the other two share three files between them
/// rather than reaching three each. Against these weights the largest total
/// any legal set of counts reaches on one side is 168 in the midgame half and
/// -207 in the ending half, so a boardful of both colours leaves the sixteen
/// bits `pack` gives each half a long way off. Six of the fourteen are past
/// single figures, which the paragraph here said they would not be while they
/// were all zero.
static SHELTER: [i32; SHELTER_TERMS] = [
    pack(10, -11),
    pack(21, -26),
    pack(-11, -26),
    pack(-8, -1),
    pack(11, 35),
    pack(-18, 2),
    pack(14, -6),
];

/// The shelter weight of one of the seven counts, as the packed pair. The
/// tuner's seam asks, so that a slot names the live weight rather than a copy
/// of it, the way it reads the tables.
pub(crate) fn shelter_weight(index: usize) -> i32 {
    SHELTER[index]
}

/// What one piece leaves on the board, on the scale the taper is read at.
/// The tuner's walk asks, because a position's phase decides what its
/// coefficients are and a copy of the table there would be a second opinion
/// about the taper.
pub(crate) fn phase_weight(piece: Piece) -> i32 {
    PHASE_WEIGHTS[piece as usize]
}

/// The score of the position from the side to move's point of view.
///
/// Everything incremental is read off the board's accumulator; a term
/// computed at the leaf is added here, from the board itself.
#[inline]
pub(crate) fn eval(board: &Board) -> Score {
    board
        .eval
        .score(board.active_color, mobility(board) + shelter(board))
}

/// What white's mobility stands ahead by, as a packed pair on the scale the
/// piece square pair is on.
///
/// Read off the board rather than accumulated. A piece that moves changes what
/// every slider looking through its square sees, so there is nothing here for
/// `Accumulator::count` to add and take away.
///
/// Only [`SCORED_KINDS`] are counted. A kind whose weight is zero contributes
/// nothing however many squares it covers, so counting it is work no score can
/// see. Leaving three of the four out took a bit over a third off what the
/// term cost.
#[inline]
fn mobility(board: &Board) -> i32 {
    mobility_with::<SCORED_KINDS>(board, &MOBILITY)
}

/// The same fold over the kinds `KINDS` names, against weights named by the
/// caller. A kind outside `KINDS` counts zero and so has to be worth zero.
///
/// Six of the eight live weights are zero and the other two are equal, so a
/// swapped pair of halves scores every position exactly as the right answer
/// does and a permuted [`MOBILITY`] shows only on the one piece that carries a
/// weight. The tests supply weights of their own through here, over all four
/// kinds, which is what says the sign, the order and the packing are right
/// whatever the fit holds.
#[inline]
fn mobility_with<const KINDS: u8>(board: &Board, weights: &[i32; MOBILE_PIECES.len()]) -> i32 {
    let white = board.mobility_counts::<KINDS>(Color::White);
    let black = board.mobility_counts::<KINDS>(Color::Black);
    let mut packed = 0;
    for ((weight, white), black) in weights.iter().zip(white).zip(black) {
        packed += weight * (white - black);
    }
    packed
}

/// What white's king shelter stands ahead by, as a packed pair on the scale
/// the piece square pair is on.
///
/// Read off the board rather than accumulated. All seven counts are read off
/// the king's square, so a king move rewrites the side's whole reading, and a
/// pawn move changes it wherever the pawn stood. There is nothing here for
/// `Accumulator::count` to add and take away a piece at a time.
#[inline]
fn shelter(board: &Board) -> i32 {
    shelter_with(board, &SHELTER)
}

/// The same fold against weights named by the caller.
///
/// The live weights are the fit's now, and the two halves of every one of
/// them differ, so the sign of this term, the order of the seven counts and
/// the packing all show in an evaluation the engine prints and in the rows
/// the tuner's walk states. The tests still supply weights of their own
/// through here, because what they pin is the fold rather than the fit: a
/// permuted [`SHELTER`] would be a different evaluation and not a wrong
/// one.
#[inline]
fn shelter_with(board: &Board, weights: &[i32; SHELTER_TERMS]) -> i32 {
    let white = board.shelter_counts(Color::White);
    let black = board.shelter_counts(Color::Black);
    let mut packed = 0;
    for ((weight, white), black) in weights.iter().zip(white).zip(black) {
        packed += weight * (white - black);
    }
    packed
}

/// The evaluation's incremental state, hosted by the board and kept in step
/// by being told about every piece placed, removed and relocated.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct Accumulator {
    /// Each side's material, indexed by `Color`'s discriminant the way the
    /// weight tables index by `Piece`'s: an index is a load where a match
    /// on the colour was a branch.
    material: [u32; 2],
    /// Piece square table score, as the packed pair of midgame and endgame
    /// scores the tables hold: see `psqt::pack`. Summing a boardful of pairs
    /// is one add, so carrying both phases costs what carrying one did, and
    /// neither half comes near the sixteen bits it has to stay inside.
    psqt: i32,
    /// What is left on the board, on the scale `PHASE_WEIGHTS` measures, and
    /// so which of the two halves above the position is scored by.
    /// Accumulated rather than counted off the piece boards at every leaf:
    /// four popcounts there measured dearer than one add per piece touched
    /// here.
    phase: i32,
}

impl Accumulator {
    /// A board with nothing on it scores nothing.
    pub(crate) const EMPTY: Self = Self {
        material: [0; 2],
        psqt: 0,
        phase: 0,
    };

    /// Count a piece on to or off of a square. The two directions written
    /// once: they are the same arithmetic with every sign reversed, and
    /// `SET` is settled at compile time, so no branch on it survives into
    /// the search.
    #[inline(always)]
    pub(crate) fn count<const SET: bool>(&mut self, index: u8, piece: Piece, color: Color) {
        // a packed pair, negated whole for black: negating the sum negates
        // both halves, so neither is unpacked until the leaf asks for it
        let psqt = match color {
            Color::White => PIECE_SQUARE_TABLES.get_value(index as usize, piece, Color::White),
            Color::Black => -PIECE_SQUARE_TABLES.get_value(index as usize, piece, Color::Black),
        };
        let phase = PHASE_WEIGHTS[piece as usize];
        let value = MATERIAL[piece as usize];
        if SET {
            self.psqt += psqt;
            self.phase += phase;
            self.material[color as usize] += value;
        } else {
            self.psqt -= psqt;
            self.phase -= phase;
            self.material[color as usize] -= value;
        }
    }

    /// A piece moving between two squares, which is `count` off one square
    /// and on to the other with the halves that cancel left out.
    ///
    /// It never leaves the board, so the material and the phase it counts
    /// for are the same before and after and their two updates undo each
    /// other exactly. What is left is the piece square score, and the pair
    /// is added and subtracted whole either way, so a borrow between the two
    /// halves cancels here as it does there.
    #[inline(always)]
    pub(crate) fn relocate(&mut self, from: u8, to: u8, piece: Piece, color: Color) {
        let moved = PIECE_SQUARE_TABLES.get_value(to as usize, piece, color)
            - PIECE_SQUARE_TABLES.get_value(from as usize, piece, color);
        match color {
            Color::White => self.psqt += moved,
            Color::Black => self.psqt -= moved,
        }
    }

    /// The accumulator the position deserves, computed from the board rather
    /// than accumulated as pieces moved. The hosted one is meant to equal
    /// this at all times, which the board's state check asks on every move
    /// made. A second implementation on purpose, and only worth having while
    /// it stays one: factoring shared code out of this walk and `count`
    /// would leave both sides wrong together and the check passing, which is
    /// worse than not checking at all.
    pub(crate) fn recomputed(board: &Board) -> Self {
        let mut recomputed = Self::EMPTY;
        // walking the occupied squares rather than all sixty four, an empty
        // board is then free rather than sixty four misses
        let mut occupied = board.occupied();
        while occupied != 0 {
            let index = occupied.trailing_zeros() as u8;
            occupied &= occupied - 1;
            if let Some((piece, color)) = board.get_piece_and_color_index(index) {
                let psqt = PIECE_SQUARE_TABLES.get_value(index as usize, piece, color);
                match color {
                    Color::White => recomputed.psqt += psqt,
                    Color::Black => recomputed.psqt -= psqt,
                }
                recomputed.material[color as usize] += MATERIAL[piece as usize];
                recomputed.phase += PHASE_WEIGHTS[piece as usize];
            }
        }
        recomputed
    }

    /// Material seeded from a recount rather than accumulated, which is how
    /// `from_fen` fills a parsed board in: the state check then compares the
    /// seeding against an implementation that did not do the seeding.
    pub(crate) fn seed_material(&mut self, (white, black): (u32, u32)) {
        self.material[Color::White as usize] = white;
        self.material[Color::Black as usize] = black;
    }

    /// What white stands ahead by, for the board's debug print.
    pub(crate) fn material_difference(&self) -> i64 {
        i64::from(self.material[Color::White as usize])
            - i64::from(self.material[Color::Black as usize])
    }

    /// The score from `side`'s point of view.
    ///
    /// The piece square half is read at the phase the position is in rather
    /// than at either end of it, so that a king walks out as the pieces come
    /// off instead of on the move that takes the last one. Material is not
    /// tapered: an endgame piece value is the same thing as a constant added
    /// to that piece's endgame table, and the tables are the tidier place to
    /// say it.
    ///
    /// `leaf` is the leaf terms [`eval`] reads off the board, mobility and the
    /// king's shelter summed, as a packed pair on the same scale. Summing them
    /// before the call is exact, since both are pairs on this scale, and it is
    /// what keeps one divide however many such terms there are. The pair joins
    /// the piece square pair before the
    /// interpolation rather than being tapered beside it, so the two share one
    /// divide. A second divide would answer a centipawn away wherever a
    /// numerator is negative and does not divide evenly, and `tune::reconstruct`
    /// folds a whole row with one.
    #[inline]
    fn score(&self, side: Color, leaf: i32) -> Score {
        // promotions can leave more on the board than the opening had, so the
        // phase is capped. It cannot go the other way: no weight is negative.
        let phase = self.phase.min(TOTAL_PHASE);
        let tapered = self.psqt + leaf;
        let scaled =
            (mg_value(tapered) * phase + eg_value(tapered) * (TOTAL_PHASE - phase)) / TOTAL_PHASE;
        let eval = (self.material[Color::White as usize] as i32
            - self.material[Color::Black as usize] as i32
            + scaled) as Score;
        match side {
            Color::White => eval,
            Color::Black => -eval,
        }
    }
}

#[cfg(test)]
mod evaluate {
    use super::{
        ALL_KINDS, Board, MOBILE_PIECES, MOBILITY, SCORED_KINDS, SHELTER_TERMS, TOTAL_PHASE, eval,
        mobility, mobility_weight, mobility_with, shelter_with,
    };
    use crate::board::fens;
    use crate::misc::Color;
    use crate::psqt::{eg_value, mg_value, pack};
    use pretty_assertions::assert_eq;

    /// Both the accumulator and its recompute read `PHASE_WEIGHTS`, so the
    /// state-in-step check holds them to each other and neither to what the
    /// weights should be. This says what they add up to: a full board is the
    /// midgame end of the taper, kings and pawns alone the endgame end, and
    /// each piece is worth what the interpolation was written expecting.
    #[test]
    fn a_full_board_is_one_end_of_the_taper_and_a_pawn_ending_the_other() {
        assert_eq!(Board::new().eval.phase, TOTAL_PHASE);
        assert_eq!(
            Board::from_fen("4k3/pppppppp/8/8/8/8/PPPPPPPP/4K3 w - - 0 1")
                .unwrap()
                .eval
                .phase,
            0
        );
        for (fen, phase) in [
            ("4k3/8/8/8/8/8/8/3QK3 w - - 0 1", 4),
            ("4k3/8/8/8/8/8/8/3RK3 w - - 0 1", 2),
            ("4k3/8/8/8/8/8/8/3BK3 w - - 0 1", 1),
            ("4k3/8/8/8/8/8/8/3NK3 w - - 0 1", 1),
        ] {
            assert_eq!(Board::from_fen(fen).unwrap().eval.phase, phase, "{}", fen);
        }
    }

    /// After every legal move in the shared positions, the material
    /// accumulators must equal a recount, and the score must be the exact
    /// negative of the opponent's view of it.
    #[test]
    fn material_stays_counted_and_the_eval_stays_antisymmetric() {
        for fen in fens::CORE {
            let mut board = Board::from_fen(fen).unwrap();
            for m in &board.generate_moves() {
                if board.make_move(m) {
                    assert_eq!(
                        (
                            board.eval.material[crate::misc::Color::White as usize],
                            board.eval.material[crate::misc::Color::Black as usize]
                        ),
                        board.material_value(),
                        "{} in {}",
                        m,
                        fen
                    );
                    let score = eval(&board);
                    board.active_color = !board.active_color;
                    assert_eq!(score, -eval(&board), "{} in {}", m, fen);
                    board.active_color = !board.active_color;
                    board.undo_move();
                }
            }
        }
    }

    /// The assertions above hold whichever way up the piece square tables are,
    /// because both colours read them the same way and the symmetry survives.
    /// These say which way is up, a colour at a time.
    #[test]
    fn a_pawn_is_worth_more_the_closer_it_is_to_promoting() {
        for (advanced, home) in [
            (
                "4k3/4P3/8/8/8/8/8/4K3 w - - 0 1",
                "4k3/8/8/8/8/8/4P3/4K3 w - - 0 1",
            ),
            (
                "4k3/8/8/8/8/8/4p3/4K3 b - - 0 1",
                "4k3/4p3/8/8/8/8/8/4K3 b - - 0 1",
            ),
        ] {
            let advanced = Board::from_fen(advanced).unwrap();
            let home = Board::from_fen(home).unwrap();
            assert!(
                eval(&advanced) > eval(&home),
                "the advanced pawn scored {} and the one at home {}",
                eval(&advanced),
                eval(&home)
            );
        }
    }

    /// The point of tapering: the same king on the same square is scored
    /// differently depending on what is left on the board. A bare king wants
    /// the middle; a king with the pieces still on wants the back rank.
    ///
    /// Each pair below differs by the king's square and nothing else, material
    /// included, so the difference between them is the king's table alone. A
    /// phase read the wrong way round would still land inside a pair, so what
    /// is pinned is the direction the score moves in as the board empties
    /// rather than only that it moves.
    #[test]
    fn a_king_is_worth_more_in_the_middle_the_emptier_the_board() {
        // two king squares, e4 and g1, at three phases
        fn centre_over_corner(centre: &str, corner: &str) -> i32 {
            i32::from(eval(&Board::from_fen(centre).unwrap()))
                - i32::from(eval(&Board::from_fen(corner).unwrap()))
        }
        let opening = centre_over_corner(
            "rnbqkbnr/pppppppp/8/8/4K3/8/PPPPPPPP/RNBQ1B1R w kq - 0 1",
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQ1BKR w kq - 0 1",
        );
        // a rook a side, which is a phase of four out of twenty four
        let middlegame = centre_over_corner(
            "r3k3/8/8/8/4K3/8/8/R7 w q - 0 1",
            "r3k3/8/8/8/8/8/8/R5K1 w q - 0 1",
        );
        let ending = centre_over_corner(
            "4k3/8/8/8/4K3/8/8/8 w - - 0 1",
            "4k3/8/8/8/8/8/8/6K1 w - - 0 1",
        );
        assert!(
            opening < middlegame && middlegame < ending,
            "e4 over g1 scored {} in the opening, {} with a rook a side and {} bare",
            opening,
            middlegame,
            ending
        );
        assert!(opening < 0 && ending > 0, "{} then {}", opening, ending);
    }

    /// A position and its reflection, colours swapped, have to score the same
    /// for whoever is to move. This does not catch the tables being upside down,
    /// since that happens to both colours at once, but it does catch one colour
    /// being changed without the other.
    #[test]
    fn a_mirrored_position_scores_the_same() {
        for (white, black) in [
            (
                "4k3/4P3/8/8/8/8/8/4K3 w - - 0 1",
                "4k3/8/8/8/8/8/4p3/4K3 b - - 0 1",
            ),
            (
                "4k3/8/8/8/8/8/8/R3K3 w - - 0 1",
                "r3k3/8/8/8/8/8/8/4K3 b - - 0 1",
            ),
            (
                "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1",
                "rnbqkbnr/pppp1ppp/8/4p3/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
            ),
        ] {
            let white = Board::from_fen(white).unwrap();
            let black = Board::from_fen(black).unwrap();
            assert_eq!(eval(&white), eval(&black), "{} against {}", white, black);
        }
    }

    /// The position the two tests below are read against, and what each side
    /// covers in it, worked out square by square rather than read back off
    /// `mobility_counts`.
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
    const WHITE_COVERS: [i32; MOBILE_PIECES.len()] = [3, 2, 7, 10];
    const BLACK_COVERS: [i32; MOBILE_PIECES.len()] = [0, 7, 9, 9];

    /// Four weights that differ from each other at both ends of the taper, so
    /// that a pair read into the wrong piece's slot lands on a different
    /// number. The four differences are 3, -5, -2 and 1, which differ from
    /// each other too, so a permutation of either array shows.
    const TRIAL: [i32; MOBILE_PIECES.len()] =
        [pack(11, 2), pack(-7, 13), pack(3, -5), pack(29, 41)];

    /// What the fold does with weights that are not zero.
    ///
    /// Six of the shipped weights are zero and the other two are equal, so
    /// almost nothing about the sign, the order of the four pieces or the
    /// packing changes an evaluation the engine prints, and a swapped pair of
    /// halves changes none of them at all. This hands the fold weights of its
    /// own and asserts the packed pair against the arithmetic: white's count
    /// less black's, piece by piece, each half of the pair summed on its own.
    #[test]
    fn the_mobility_fold_reads_white_less_black_piece_by_piece() {
        let board = Board::from_fen(COUNTED).unwrap();
        assert_eq!(
            board.mobility_counts::<{ ALL_KINDS }>(Color::White),
            WHITE_COVERS
        );
        assert_eq!(
            board.mobility_counts::<{ ALL_KINDS }>(Color::Black),
            BLACK_COVERS
        );
        let midgame: i32 = (0..MOBILE_PIECES.len())
            .map(|i| mg_value(TRIAL[i]) * (WHITE_COVERS[i] - BLACK_COVERS[i]))
            .sum();
        let endgame: i32 = (0..MOBILE_PIECES.len())
            .map(|i| eg_value(TRIAL[i]) * (WHITE_COVERS[i] - BLACK_COVERS[i]))
            .sum();
        assert_ne!(
            midgame, endgame,
            "the two halves would not tell a swap apart"
        );
        let packed = mobility_with::<{ ALL_KINDS }>(&board, &TRIAL);
        assert_ne!(midgame, 0, "black less white would answer the same here");
        assert_eq!((mg_value(packed), eg_value(packed)), (midgame, endgame));
    }

    /// What `eval` is allowed to leave out, which is the whole of the contract
    /// between it and the tuner's walk.
    ///
    /// The walk counts all four kinds and `eval` counts [`SCORED_KINDS`], so
    /// the two no longer read one answer. What makes that safe is the size of
    /// the difference and nothing else: a count multiplied by zero adds
    /// nothing, so a kind worth zero can go uncounted without moving a score,
    /// and any other kind cannot. So this asserts the difference is exactly
    /// that, kind by kind, and then that the two fold to the same packed pair
    /// at the shipped weights.
    ///
    /// A later fit that prices one of the three kinds `eval` skips today fails
    /// here, rather than being silently thrown away at every leaf and every
    /// quiescence node.
    ///
    /// The position has to give every kind of both colours something to cover,
    /// or a kind that is skipped and a kind that covers nothing read the same
    /// and the test passes without having looked at anything.
    #[test]
    fn eval_counts_a_kind_exactly_when_its_weight_is_not_zero() {
        let board = Board::from_fen(fens::KIWIPETE).unwrap();
        for color in [Color::White, Color::Black] {
            let all = board.mobility_counts::<{ ALL_KINDS }>(color);
            let scored = board.mobility_counts::<{ SCORED_KINDS }>(color);
            for (index, piece) in MOBILE_PIECES.into_iter().enumerate() {
                assert_ne!(
                    all[index], 0,
                    "{:?} covers nothing here, so this position cannot tell a skipped kind \
                     from a counted one",
                    piece
                );
                let weight = mobility_weight(index);
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
        let whole = mobility_with::<{ ALL_KINDS }>(&board, &MOBILITY);
        assert_ne!(
            whole, 0,
            "mobility is level here, so this position says nothing about the fold"
        );
        assert_eq!(mobility(&board), whole);
    }

    /// Mobility enters the numerator of the one interpolation rather than
    /// being tapered beside it.
    ///
    /// A position whose piece square numerator is negative and does not divide
    /// by twenty four evenly, so the two readings differ: inside the divide
    /// the whole numerator is truncated once, and a second divide would
    /// truncate each part on its own and answer a centipawn away. Nothing else
    /// pins this, and it is what `tune::reconstruct` folds a row with.
    #[test]
    fn mobility_joins_the_numerator_rather_than_being_tapered_beside_it() {
        let board = Board::from_fen("4k3/8/8/8/8/8/4P3/1N2K3 w - - 0 1").unwrap();
        let accumulator = board.eval;
        let phase = accumulator.phase.min(TOTAL_PHASE);
        let mobility = pack(11, -1);
        let together = accumulator.psqt + mobility;
        let inside =
            (mg_value(together) * phase + eg_value(together) * (TOTAL_PHASE - phase)) / TOTAL_PHASE;
        let beside = (mg_value(accumulator.psqt) * phase
            + eg_value(accumulator.psqt) * (TOTAL_PHASE - phase))
            / TOTAL_PHASE
            + (mg_value(mobility) * phase + eg_value(mobility) * (TOTAL_PHASE - phase))
                / TOTAL_PHASE;
        assert_ne!(
            inside, beside,
            "the two divides agree here, so this position says nothing"
        );
        let material = accumulator.material[Color::White as usize] as i32
            - accumulator.material[Color::Black as usize] as i32;
        assert_eq!(
            accumulator.score(Color::White, mobility),
            (material + inside) as crate::misc::Score
        );
    }
    /// The position the test below is read against, and what each side counts
    /// in it, worked out by hand rather than read back off `shelter_counts`.
    ///
    /// White's king on g1 stands behind the f, g and h files. It has f2 and h2
    /// one rank ahead and g3 two, and all three files hold a pawn of its own,
    /// so neither file count fires. Coming the other way it faces g2 one rank
    /// ahead, f3 and h3 two, and f4, g4 and h4 three.
    ///
    /// Black's king on b8 stands behind the a, b and c files with nothing on
    /// either rank in front of it and no white pawn within three ranks, so its
    /// storm is empty. White's pawn on a4 leaves that file half open and the b
    /// and c files hold no pawn at all.
    ///
    /// The seven differences are 2, 1, -2, -1, 1, 2 and 3. None is zero, so
    /// every slot is doing work in the assertion below, which a position with
    /// an empty storm would not manage.
    const SHELTERED: &str = "1k6/8/8/8/P4ppp/5pPp/5PpP/6K1 w - - 0 1";
    const WHITE_SHELTERS: [i32; SHELTER_TERMS] = [2, 1, 0, 0, 1, 2, 3];
    const BLACK_SHELTERS: [i32; SHELTER_TERMS] = [0, 0, 2, 1, 0, 0, 0];

    /// Seven weights that differ from each other at both ends of the taper, so
    /// that a pair read into the wrong count's slot lands on a different
    /// number. The seven differences between the halves are 9, -20, 8, -12,
    /// 20, -29 and -14, which differ from each other too, so a permutation of
    /// either array shows.
    const SHELTER_TRIAL: [i32; SHELTER_TERMS] = [
        pack(11, 2),
        pack(-7, 13),
        pack(3, -5),
        pack(29, 41),
        pack(17, -3),
        pack(-23, 6),
        pack(5, 19),
    ];

    /// What the fold does with weights that are not the shipped ones.
    ///
    /// The shipped weights would do here now that they are not zero. Weights
    /// of this test's own are kept anyway, because the shipped ones are the
    /// fit's and will move again: a pin written against them would have to
    /// be rewritten by every refit, and what it is pinning is the fold. So
    /// this hands the fold seven pairs that differ from each other at both
    /// ends and asserts the packed pair against the arithmetic: white's count
    /// less black's, count by count, each half of the pair summed on its
    /// own.
    #[test]
    fn the_shelter_fold_reads_white_less_black_count_by_count() {
        let board = Board::from_fen(SHELTERED).unwrap();
        assert_eq!(board.shelter_counts(Color::White), WHITE_SHELTERS);
        assert_eq!(board.shelter_counts(Color::Black), BLACK_SHELTERS);
        let midgame: i32 = (0..SHELTER_TERMS)
            .map(|i| mg_value(SHELTER_TRIAL[i]) * (WHITE_SHELTERS[i] - BLACK_SHELTERS[i]))
            .sum();
        let endgame: i32 = (0..SHELTER_TERMS)
            .map(|i| eg_value(SHELTER_TRIAL[i]) * (WHITE_SHELTERS[i] - BLACK_SHELTERS[i]))
            .sum();
        assert_ne!(
            midgame, endgame,
            "the two halves would not tell a swap apart"
        );
        let packed = shelter_with(&board, &SHELTER_TRIAL);
        assert_ne!(midgame, 0, "black less white would answer the same here");
        assert_eq!((mg_value(packed), eg_value(packed)), (midgame, endgame));
    }
}
