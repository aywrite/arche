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
/// All zero. The term is computed and the tuner's seam states its
/// coefficients, so what a mobility weight would be worth can be fitted, and
/// until it is the evaluation returns exactly what it returned without the
/// term. The fit is a commit of its own.
///
/// A count is at most twenty seven for a queen and a boardful comes to a few
/// hundred, so a weight in single figures leaves the same order of magnitude
/// in hand that `pack` asks for.
static MOBILITY: [i32; MOBILE_PIECES.len()] = [pack(0, 0); MOBILE_PIECES.len()];

/// The mobility weight of one of the four pieces that carries one, as the
/// packed pair. The tuner's seam asks, so that a slot names the live weight
/// rather than a copy of it, the way it reads the tables.
pub(crate) fn mobility_weight(index: usize) -> i32 {
    MOBILITY[index]
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
    board.eval.score(board.active_color, mobility(board))
}

/// What white's mobility stands ahead by, as a packed pair on the scale the
/// piece square pair is on.
///
/// Read off the board rather than accumulated. A piece that moves changes what
/// every slider looking through its square sees, so there is nothing here for
/// `Accumulator::count` to add and take away.
#[inline]
fn mobility(board: &Board) -> i32 {
    mobility_with(board, &MOBILITY)
}

/// The same fold against weights named by the caller.
///
/// The live weights are zero, so `mobility` returns zero whatever it does with
/// the counts: a reversed sign and a permuted [`MOBILITY`] both score every
/// position exactly as the right answer does. The tests supply weights of
/// their own through here, which is what says the sign, the order and the
/// packing are right before there is a fitted number to notice them by.
#[inline]
fn mobility_with(board: &Board, weights: &[i32; MOBILE_PIECES.len()]) -> i32 {
    let white = board.mobility_counts(Color::White);
    let black = board.mobility_counts(Color::Black);
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
    /// `mobility` is the leaf term [`eval`] reads off the board, as a packed
    /// pair on the same scale. It joins the piece square pair before the
    /// interpolation rather than being tapered beside it, so the two share one
    /// divide. A second divide would answer a centipawn away wherever a
    /// numerator is negative and does not divide evenly, and `tune::reconstruct`
    /// folds a whole row with one.
    #[inline]
    fn score(&self, side: Color, mobility: i32) -> Score {
        // promotions can leave more on the board than the opening had, so the
        // phase is capped. It cannot go the other way: no weight is negative.
        let phase = self.phase.min(TOTAL_PHASE);
        let tapered = self.psqt + mobility;
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
    use super::{Board, MOBILE_PIECES, TOTAL_PHASE, eval, mobility_with};
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
    /// The shipped weights are zero, so nothing about the sign, the order of
    /// the four pieces or the packing changes a single evaluation the engine
    /// prints. This hands the fold weights of its own and asserts the packed
    /// pair against the arithmetic: white's count less black's, piece by
    /// piece, each half of the pair summed on its own.
    #[test]
    fn the_mobility_fold_reads_white_less_black_piece_by_piece() {
        let board = Board::from_fen(COUNTED).unwrap();
        assert_eq!(board.mobility_counts(Color::White), WHITE_COVERS);
        assert_eq!(board.mobility_counts(Color::Black), BLACK_COVERS);
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
        let packed = mobility_with(&board, &TRIAL);
        assert_ne!(midgame, 0, "black less white would answer the same here");
        assert_eq!((mg_value(packed), eg_value(packed)), (midgame, endgame));
    }

    /// Mobility enters the numerator of the one interpolation rather than
    /// being tapered beside it.
    ///
    /// A position whose piece square numerator is negative and does not divide
    /// by twenty four evenly, so the two readings differ: inside the divide
    /// the whole numerator is truncated once, and a second divide would
    /// truncate each part on its own and answer a centipawn away. Nothing else
    /// pins this while the weights are zero, and it is what `tune::reconstruct`
    /// folds a row with.
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
}
