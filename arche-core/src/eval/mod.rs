// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The evaluation: what a position scores, and every number that opinion is
//! built from. The board hosts an [`Accumulator`] and tells it about each
//! piece placed, removed and moved from one square to another; the search
//! asks [`eval`] for the score. A term cheap enough to keep incrementally
//! belongs in the accumulator; one computed at the leaf belongs in a module
//! of its own beside this one.
//!
//! This file holds what the terms share: the material values, the phase
//! weights the taper is read at, the accumulator, the sum the search asks for
//! and [`TERMS`], the list the tuner's seam reads the leaf terms through.
//! Each leaf term owns the rest of itself, its counts and the masks they are
//! read off, its weights with the fit that produced them, its fold and its
//! memo where it has one.

mod king_attack;
mod mobility;
mod pawn_structure;
mod shelter;

use crate::board::Board;
use crate::misc::{Color, Piece, Score};
use crate::psqt::{PieceSquareTables, eg_value, mg_value};

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

/// What one piece leaves on the board, on the scale the taper is read at.
/// The tuner's walk asks, because a position's phase decides what its
/// coefficients are and a copy of the table there would be a second opinion
/// about the taper.
pub(crate) fn phase_weight(piece: Piece) -> i32 {
    PHASE_WEIGHTS[piece as usize]
}

/// One leaf term, as everything outside its own file sees it.
///
/// Four fields, and between them they are the whole of what the tuner needs
/// to lay a term out, price it and read its coefficients off a position. The
/// evaluation does not go through here: [`sum`] names the four folds
/// directly, so the hot path costs no indirect call and the descriptor is
/// free to be the offline seam it is for.
pub(crate) struct Term {
    /// What the layout line calls the term, which is the name
    /// `scripts/tune.py` keys its bounds and its holds by.
    pub(crate) name: &'static str,
    /// How many counts the term is measured in, per side and per half of the
    /// taper. It occupies twice this in the weight vector, the midgame half
    /// first.
    pub(crate) width: usize,
    /// The packed weight pair of one of those counts, read out of the live
    /// array rather than a copy of it.
    pub(crate) weight: fn(usize) -> i32,
    /// One side's counts, written into the first `width` entries of the
    /// slice.
    pub(crate) counts: fn(&Board, Color, &mut [i32]),
}

/// The leaf terms, in the order the weight vector holds them.
///
/// A term is appended rather than inserted, so that adding one moves no slot
/// a fit has already been written against. This order is the order
/// `tune::SLOTS` sums, the order `Terms::of` walks and the order the run's
/// layout line prints, so the three follow from one list rather than agreeing
/// with each other.
pub(crate) const TERMS: &[Term] = &[
    Term {
        name: "mobility",
        width: mobility::COUNTS,
        weight: mobility::weight,
        counts: mobility::counts,
    },
    Term {
        name: "shelter",
        width: shelter::COUNTS,
        weight: shelter::weight,
        counts: shelter::counts,
    },
    Term {
        name: "pawn_structure",
        width: pawn_structure::COUNTS,
        weight: pawn_structure::weight,
        counts: pawn_structure::counts,
    },
    Term {
        name: "king_attack",
        width: king_attack::COUNTS,
        weight: king_attack::weight,
        counts: king_attack::counts,
    },
];

/// The widest term, which is how long a buffer the tuner's walk needs to ask
/// any of them for its counts.
pub(crate) const WIDEST: usize = widest();

const fn widest() -> usize {
    let mut widest = 0;
    let mut index = 0;
    while index < TERMS.len() {
        if TERMS[index].width > widest {
            widest = TERMS[index].width;
        }
        index += 1;
    }
    widest
}

/// What the two remembered terms answer, asked of whatever the caller is
/// carrying.
///
/// The sum is written once and reads the position through this, so the cached
/// evaluation and the uncached one are one body rather than two that have to
/// be kept saying the same thing. A method per cached term, so a fourth such
/// term adds one here and an implementation in each of the two below. That is
/// the seam, and it is meant to be written twice.
trait Memo {
    fn shelter(&mut self, board: &Board) -> i32;
    fn pawn_structure(&mut self, board: &Board) -> i32;
}

/// The memo that remembers nothing, which is what [`eval`] hands the sum.
struct NoMemo;

impl Memo for NoMemo {
    #[inline]
    fn shelter(&mut self, board: &Board) -> i32 {
        shelter::fold(board)
    }

    #[inline]
    fn pawn_structure(&mut self, board: &Board) -> i32 {
        pawn_structure::fold(board)
    }
}

/// What a searcher carries so that the two remembered terms are not computed
/// again at every leaf.
///
/// One value rather than a cache per term: the search holds one field and
/// passes one argument, and a term that learns to remember itself is a field
/// here rather than a third parameter everywhere a score is asked for.
///
/// Two tables inside it rather than one wider entry under the shelter's key.
/// The alternative is one probe for both terms, and it would recompute the
/// pawn structure on every king move, which is the half of the shelter's key
/// that term does not need. Which is cheaper is a measurement and not an
/// opinion, and it was made on the fitted build.
#[derive(Default)]
pub(crate) struct Caches {
    shelter: shelter::Cache,
    pawns: pawn_structure::Cache,
}

impl Memo for Caches {
    #[inline]
    fn shelter(&mut self, board: &Board) -> i32 {
        self.shelter.get(board)
    }

    #[inline]
    fn pawn_structure(&mut self, board: &Board) -> i32 {
        self.pawns.get(board)
    }
}

/// The score of the position from the side to move's point of view, with the
/// two remembered terms taken from `memo`.
///
/// Everything incremental is read off the board's accumulator; a term
/// computed at the leaf is added here, from the board itself. The four leaf
/// terms are summed before the call, which is exact since all four are pairs
/// on one scale, and it is what keeps one divide however many such terms
/// there are.
///
/// The king attack zone is behind [`king_attack::SCORED`], which is true at
/// the fitted weights and would be false if all eight were zero. A count
/// multiplied by nothing scores nothing, and llvm does not take the walk that
/// produces it out on that ground, so at a constant false the summand is not
/// compiled rather than computed and thrown away.
///
/// Material that cannot mate reads zero before any of it. That is the one
/// place in the evaluation that is not a dot product against the weights,
/// which is why the tuner turns such a position away rather than fitting it:
/// see `tune::run`. It sits here rather than at the node because the model
/// gate, the tuner's walk and the instruments all read this function, and a
/// zero returned from the search instead would leave them saying a dead draw
/// is worth a piece.
#[inline]
fn sum(board: &Board, memo: &mut impl Memo) -> Score {
    if board.drawn_by_material() {
        return 0;
    }
    let leaf = mobility::fold(board)
        + memo.shelter(board)
        + memo.pawn_structure(board)
        + if king_attack::SCORED {
            king_attack::fold(board)
        } else {
            0
        };
    board.eval.score(board.active_color, leaf)
}

/// The score with the shelter and the pawn structure computed every time.
///
/// The tuner's walk and the instruments ask this one, and so do the five
/// places in the search that want a score depending on the position alone.
/// Neither door is hot enough there for the difference between them to
/// matter.
#[inline]
pub(crate) fn eval(board: &Board) -> Score {
    sum(board, &mut NoMemo)
}

/// The same score with those two terms taken from the searcher's caches
/// wherever they are there.
///
/// Equal to [`eval`] for every position, which is what
/// `the_cache_answers_what_the_full_evaluation_does` holds both of them to.
/// The search calls this and the node counts do not move, because a
/// remembered score is the score that would have been computed.
#[inline]
pub(crate) fn eval_cached(board: &Board, caches: &mut Caches) -> Score {
    sum(board, caches)
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
    /// `leaf` is the leaf terms [`eval`] reads off the board, mobility, the
    /// king's shelter, the pawn structure and the king attack zone summed, as
    /// a packed pair on the same scale. The pair joins the piece square pair
    /// before the interpolation rather than being tapered beside it, so the
    /// two share one divide. A second divide would answer a centipawn away
    /// wherever a numerator is negative and does not divide evenly, and
    /// `tune::reconstruct` folds a whole row with one.
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
        Board, Caches, TERMS, TOTAL_PHASE, eval, eval_cached, king_attack, mobility,
        pawn_structure, shelter,
    };
    use crate::board::fens;
    use crate::misc::{Color, File, coordinate_to_index};
    use crate::psqt::{eg_value, mg_value, pack};
    use pretty_assertions::assert_eq;
    use std::collections::{HashMap, HashSet};

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

    /// Material that cannot mate scores zero from either side, through both
    /// entry points.
    ///
    /// Both, because the search reads the cached one and the tuner, the model
    /// gate and the instruments read the other, and a rule in one of them
    /// would have the two disagree about the same node. The caches are passed
    /// and neither table is read: a drawn position has no pawn on it, so the
    /// pawn structure would have contributed nothing had it been reached.
    ///
    /// The figures in the comments are what the evaluation returned before the
    /// rule. The search played every one of them as a win.
    #[test]
    fn material_that_cannot_mate_scores_zero() {
        for fen in [
            "8/8/8/8/8/4k3/8/4K1N1 w - - 0 1", // a knight, read as +320 and a table
            "8/8/8/8/8/4k3/8/4K1N1 b - - 0 1",
            "8/8/8/8/8/4k3/8/4KB2 w - - 0 1", // a bishop, +320 and a table
            "8/8/8/8/8/4k3/8/4KB2 b - - 0 1",
            "8/8/8/8/8/4k3/8/4K1NN w - - 0 1", // two knights, +640 and a table
            "8/8/8/8/8/4k3/8/4K1NN b - - 0 1",
            "4k3/8/8/8/4K3/8/8/8 w - - 0 1", // already zero, by cancellation
            "4k3/8/8/8/4K3/8/8/8 b - - 0 1",
            "4k3/8/8/8/8/B7/8/2B1K3 w - - 0 1",
            "4k3/8/8/8/8/B7/8/2B1K3 b - - 0 1",
            "3bk3/8/8/8/8/8/8/2B1K3 w - - 0 1",
            "3bk3/8/8/8/8/8/8/2B1K3 b - - 0 1",
        ] {
            let board = Board::from_fen(fen).unwrap();
            let mut caches = Caches::default();
            assert_eq!(eval(&board), 0, "{}", fen);
            assert_eq!(eval_cached(&board, &mut caches), 0, "{}", fen);
        }
    }

    /// The same signatures with a pawn on the board are outside the rule and
    /// are scored as they were.
    ///
    /// Without this, the test above would pass on a rule that answered zero
    /// for every pawnless position, or for every position at all.
    #[test]
    fn a_pawn_takes_a_position_out_of_the_rule() {
        for fen in [
            "8/8/8/4p3/8/4k3/8/4K1N1 w - - 0 1",
            "8/8/8/4p3/8/4k3/8/4KB2 w - - 0 1",
            "8/8/8/4p3/8/4k3/8/4K1NN w - - 0 1",
        ] {
            let board = Board::from_fen(fen).unwrap();
            let mut caches = Caches::default();
            assert_ne!(eval(&board), 0, "{}", fen);
            assert_eq!(eval_cached(&board, &mut caches), eval(&board), "{}", fen);
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
        // a pawn a side, because two bare kings are drawn by material and
        // both fens would read zero. A pawn does not count towards the phase,
        // so this is still the ending, and the two pawns stand on the same
        // squares in both fens and away from either king's files, which
        // leaves the difference the king's own square
        let ending = centre_over_corner(
            "4k3/p7/8/8/4K3/8/P7/8 w - - 0 1",
            "4k3/p7/8/8/8/8/P7/6K1 w - - 0 1",
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

    /// The searcher's caches and the keys a walk saw through them. One thing
    /// to carry rather than three, and the keys are what say whether the run
    /// evicted anything.
    #[derive(Default)]
    struct Walk {
        caches: Caches,
        shelter_keys: HashSet<u64>,
        pawn_keys: HashSet<u64>,
        counted: HashMap<u64, [[i32; pawn_structure::COUNTS]; 2]>,
        compared: usize,
    }

    impl Walk {
        /// What the pawn key claims, checked against what the counts say.
        ///
        /// The cache hands a remembered score to every position whose pawn
        /// key it matches, so the key has to decide the counts. The identity
        /// below sees a wrong key now that the weights are the fit's, and it
        /// sees it as a wrong score; this says the same thing one step
        /// earlier and in the terms the term is defined in. Two positions
        /// under one key that disagree on the eight counts are one position
        /// handed the other's score, whatever the weights make that worth.
        fn note(&mut self, board: &Board) {
            let counts = [
                pawn_structure::counts_of(board, Color::White),
                pawn_structure::counts_of(board, Color::Black),
            ];
            if let Some(seen) = self.counted.insert(board.pawn_key, counts) {
                self.compared += 1;
                assert_eq!(
                    seen,
                    counts,
                    "two positions share a pawn key and not its counts, at {}",
                    board.to_fen()
                );
            }
        }
    }

    /// How many of `keys` there are and how many slots of a table of `slots`
    /// they land in. The second being the smaller is what says two keys
    /// shared a slot, so an entry was written over rather than only written.
    fn filled(keys: &HashSet<u64>, slots: usize) -> (usize, usize) {
        let landed: HashSet<usize> = keys
            .iter()
            .map(|key| (*key as usize) & (slots - 1))
            .collect();
        (keys.len(), landed.len())
    }

    /// Every position reachable inside `budget` moves of `board`, scored both
    /// ways through the caches.
    fn walk(board: &Board, depth: usize, walked: &mut Walk, budget: &mut usize) {
        if depth == 0 || *budget == 0 {
            return;
        }
        for m in &board.generate_moves() {
            if *budget == 0 {
                break;
            }
            let mut played = board.clone();
            if !played.make_move(m) {
                continue;
            }
            *budget -= 1;
            walked.shelter_keys.insert(shelter::key(&played));
            walked.pawn_keys.insert(played.pawn_key);
            walked.note(&played);
            assert_eq!(
                eval_cached(&played, &mut walked.caches),
                eval(&played),
                "a cache and the evaluation part company at {}",
                played.to_fen()
            );
            walk(&played, depth - 1, walked, budget);
        }
    }

    /// The cache answers what the full evaluation does, position for
    /// position.
    ///
    /// A shelter score is remembered under the pawns and the two king
    /// squares, and every other piece is outside that key, so a key missing
    /// something the term reads would hand one position's shelter to
    /// another. Nothing in a search would say so: the score is simply not
    /// the position's, and a wrong evaluation is the one error it does not
    /// report.
    ///
    /// A walk that never evicted would be testing a cache that only ever
    /// grows, so the run has to overwrite entries as well as write and read
    /// them. Counting the positions does not say it happened: the walk
    /// revisits keys, and far fewer keys than positions reach the table. What
    /// says it is the keys against the slots they land in, so the test
    /// collects the keys and asserts that two of them shared a slot.
    ///
    /// `Walk::note` carries the same claim in the counts rather than in the
    /// score, which is what it was written to do while the sixteen weights
    /// were zero and the score said nothing. It is kept now that they are
    /// fitted, because a count is a sharper thing to compare than a sum of
    /// sixteen products: two wrong counts whose weights happen to cancel
    /// would pass the score and fail the note.
    #[test]
    fn the_cache_answers_what_the_full_evaluation_does() {
        let mut walked = Walk::default();
        let mut budget = 4 * shelter::CACHE_SLOTS.max(pawn_structure::CACHE_SLOTS);
        for fen in fens::CORE {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(
                eval_cached(&board, &mut walked.caches),
                eval(&board),
                "{}",
                fen
            );
            walked.shelter_keys.insert(shelter::key(&board));
            walked.pawn_keys.insert(board.pawn_key);
            walked.note(&board);
            walk(&board, 3, &mut walked, &mut budget);
        }
        assert!(
            walked.compared > 0,
            "every pawn key here was seen once, so no two positions were held against each other"
        );
        for (name, keys, slots) in [
            ("shelter", &walked.shelter_keys, shelter::CACHE_SLOTS),
            ("pawn", &walked.pawn_keys, pawn_structure::CACHE_SLOTS),
        ] {
            let (keys, landed) = filled(keys, slots);
            assert!(
                keys > landed,
                "{} keys over {} slots of the {} cache, so no slot was written twice and nothing was evicted",
                keys,
                landed,
                name
            );
        }
    }

    /// A king move leaves the pawn key alone and moves the shelter key, so
    /// the entry this term wrote answers the position after it and the
    /// shelter's does not.
    ///
    /// The difference between the two caches, stated rather than implied. It
    /// is why this term could be cached in the commit that introduced it
    /// where the shelter's cache had to wait for a fit to show it was needed.
    #[test]
    fn a_king_move_keeps_the_pawn_entry_and_loses_the_shelter_one() {
        let board = Board::from_fen("4k3/pppppppp/8/8/8/8/PPPPPPPP/4K3 w - - 0 1").unwrap();
        let from = coordinate_to_index(1, File::E);
        let king = board
            .generate_moves()
            .into_iter()
            .find(|m| m.from == from)
            .expect("the king on e1 has a move here");
        let mut moved = board.clone();
        assert!(moved.make_move(&king));
        assert_eq!(moved.pawn_key, board.pawn_key);
        assert_ne!(shelter::key(&moved), shelter::key(&board));
    }

    /// Every term the table names carries a width, a weight and a count
    /// helper that agree with each other, and the helper fills every count
    /// the width claims.
    ///
    /// The descriptor is what the tuner lays its slots out from, so a width
    /// that disagrees with the helper would move every slot after it with
    /// nothing here to say so. The buffer starts at `i32::MIN` and every
    /// entry the width names has to have been written over, which is the
    /// half of that a slice cannot catch: a helper handed `width` entries
    /// and writing fewer leaves the rest of them where they stood, where one
    /// writing more is out of bounds and panics.
    #[test]
    fn every_term_writes_the_counts_its_width_claims() {
        let board = Board::from_fen(fens::KIWIPETE).unwrap();
        let mut named: HashSet<&str> = HashSet::new();
        for term in TERMS {
            assert!(term.width > 0, "{} is measured in nothing", term.name);
            assert!(
                term.name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_')
            );
            assert!(named.insert(term.name), "two terms called {}", term.name);
            assert!(term.width <= super::WIDEST, "{} is wider", term.name);
            let mut counts = [i32::MIN; super::WIDEST];
            (term.counts)(&board, Color::White, &mut counts[..term.width]);
            assert!(
                counts[..term.width].iter().all(|c| *c != i32::MIN),
                "{} left a count its width claims unwritten",
                term.name
            );
            let priced = (0..term.width).any(|index| {
                mg_value((term.weight)(index)) != 0 || eg_value((term.weight)(index)) != 0
            });
            assert!(priced, "{} is worth nothing at either end", term.name);
        }
    }

    /// The table names the four leaf terms the sum adds, which is what makes
    /// the tuner's row and the evaluation the same arithmetic.
    ///
    /// The sum is hand written rather than a walk over the table, so this is
    /// the one place the two lists are held against each other. A priced term
    /// added to the table and left out of the sum would print coefficients the
    /// evaluation never reads, and the tuner's identity would fail on the
    /// first position that touched it; this says which of the two is wrong.
    #[test]
    fn the_table_names_the_terms_the_sum_adds() {
        let names: Vec<&str> = TERMS.iter().map(|term| term.name).collect();
        assert_eq!(
            names,
            ["mobility", "shelter", "pawn_structure", "king_attack"]
        );
        // two queens and a rook against none, a king in each corner of the
        // board and pawns of both colours on six files, so that no one of the
        // four folds to nothing and the test says something about each. The
        // queen on a4 bears on d7 and e8 of the black king's ring, which is
        // what leaves the king attack counts unlevel
        let board = Board::from_fen("3k4/P4p2/8/3P2p1/Q2P4/PP2p2p/1P6/1Q4KR w - - 0 1").unwrap();
        for (name, term) in [
            ("mobility", mobility::fold(&board)),
            ("shelter", shelter::fold(&board)),
            ("pawn structure", pawn_structure::fold(&board)),
            ("king attack", king_attack::fold(&board)),
        ] {
            assert_ne!(term, 0, "{} is level here, so it says nothing", name);
        }
        let leaf = mobility::fold(&board)
            + shelter::fold(&board)
            + pawn_structure::fold(&board)
            + king_attack::fold(&board);
        assert_eq!(eval(&board), board.eval.score(board.active_color, leaf));
    }
}
