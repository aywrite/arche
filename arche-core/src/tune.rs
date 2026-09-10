// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What a position's evaluation is made of, weight by weight.
//!
//! The evaluation is material plus a tapered piece square score, and it is
//! linear in the numbers those two are read from. So a position's score is a
//! dot product: a coefficient for each of the weights it touches, against the
//! weights themselves. This module writes the coefficients down, and a fit
//! run outside the engine reads them.
//!
//! The seam is the point. A tuner needs a model of the evaluation, and a
//! second implementation of one in another language diverges quietly: a model
//! wrong by a little still produces plausible weights, and nothing says when.
//! So the engine states the coefficients and never the arithmetic, and what
//! reads them is told how to weigh a position and not how to evaluate one.
//! [`reconstruct`] is the statement that the two are the same thing, and it
//! is asserted on every row a run prints rather than only in a test.
//!
//! The walk is a third pass over the board, beside `Accumulator::count` and
//! `Accumulator::recomputed`. eval.rs defends that duplication on the second
//! and the same terms hold here: two implementations that agree are a check,
//! and a helper shared between them is not. The accumulator the search keeps
//! is neither read nor duplicated.

use crate::bench::Position;
use crate::board::Board;
use crate::engine::{AlphaBeta, SearchConfig};
use crate::eval::{self, TOTAL_PHASE};
use crate::misc::{Color, Piece, Score};
use crate::psqt::{PieceSquareTables, eg_value, mg_value};
use std::fmt;

/// The midgame half of the weight vector: a table of sixty four for each of
/// the six pieces, in `Piece` order.
pub const MIDGAME_SLOTS: usize = 6 * 64;

/// The endgame half, laid out the way the midgame half is, so a square's two
/// weights are [`MIDGAME_SLOTS`] apart.
///
/// It was the pawn's table and the king's and nothing else, because the other
/// four pieces handed one array to both ends of the taper and a knight on a
/// square was one weight rather than two. Giving those four an endgame table
/// took the vector from 518 slots to 774, so a row printed by an older engine
/// and any vector fitted against one no longer parse. That is deliberate:
/// read into this layout they would land on the wrong weights.
pub const ENDGAME_SLOTS: usize = 6 * 64;

/// Where the six material values stand in the vector, after both halves of
/// the tables.
pub const MATERIAL_SLOT: usize = MIDGAME_SLOTS + ENDGAME_SLOTS;

/// The whole weight vector: 384 midgame entries, 384 endgame ones, and the
/// six material values.
pub const SLOTS: usize = MATERIAL_SLOT + 6;

/// The weight a slot names.
///
/// Read out of the live tables rather than out of a copy, which is what
/// leaves a wrong weight impossible here. The only error left is a wrong
/// coefficient, and that is what the pin catches.
///
/// Black reads the tables as written, so a table's own index is the square to
/// ask black about, and a slot is a (piece, table index) pair rather than a
/// (piece, colour, square) triple.
pub fn weight(slot: usize) -> i32 {
    let packed = |piece: Piece, entry: usize| {
        PieceSquareTables::TABLES.get_value(entry, piece, Color::Black)
    };
    if slot < MIDGAME_SLOTS {
        mg_value(packed(Piece::PIECES[slot / 64], slot % 64))
    } else if slot < MATERIAL_SLOT {
        let entry = slot - MIDGAME_SLOTS;
        eg_value(packed(Piece::PIECES[entry / 64], entry % 64))
    } else {
        eval::material(Piece::PIECES[slot - MATERIAL_SLOT]) as i32
    }
}

/// One position's evaluation, decomposed over the weight vector.
///
/// The coefficients are in the side to move's frame, so a row's own
/// arithmetic is the evaluation with no further step. They are sparse and
/// sorted by slot: over the strategic suite's quiet positions a row names
/// thirty four of the seven hundred and seventy four weights at the median,
/// and a column's non-zero count is what says how much of the corpus a
/// weight is fitted on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terms {
    /// What is left on the board, capped the way the evaluation caps it. A
    /// property of the position rather than of the weights, which is what
    /// leaves the decomposition linear.
    pub phase: i32,
    /// Slot and coefficient, ascending by slot, zeroes left out.
    pub coefficients: Vec<(u16, i32)>,
}

impl Terms {
    /// The decomposition of the position on the board.
    ///
    /// Walks the occupied squares the way `Accumulator::recomputed` does,
    /// twice: the phase decides every piece square coefficient, so it is
    /// counted before any of them is written.
    pub fn of(board: &Board) -> Self {
        let phase = phase_of(board).min(TOTAL_PHASE);
        // the row is the side to move's, so the sign that the evaluation
        // applies at the end is folded into every coefficient here. The
        // divide truncates toward zero, which is odd, so a sign inside it
        // and a sign outside it give the same integer
        let mover = match board.active_color {
            Color::White => 1,
            Color::Black => -1,
        };
        let mut coefficients = [0_i32; SLOTS];
        let mut occupied = board.occupied();
        while occupied != 0 {
            let index = occupied.trailing_zeros() as u8;
            occupied &= occupied - 1;
            let Some((piece, color)) = board.get_piece_and_color_index(index) else {
                continue;
            };
            let sign = mover
                * match color {
                    Color::White => 1,
                    Color::Black => -1,
                };
            coefficients[MATERIAL_SLOT + piece as usize] += sign;
            // white reads the tables mirrored and black as written, so the
            // slot is named by the table's own index and one weight serves
            // both colours
            let entry = match color {
                Color::White => usize::from(index ^ 56),
                Color::Black => usize::from(index),
            };
            // every square is two weights, one at each end of the taper,
            // and the phase divides the position between them
            let midgame = piece as usize * 64 + entry;
            coefficients[midgame] += sign * phase;
            coefficients[MIDGAME_SLOTS + midgame] += sign * (TOTAL_PHASE - phase);
        }
        Self {
            phase,
            coefficients: coefficients
                .into_iter()
                .enumerate()
                .filter(|(_, coefficient)| *coefficient != 0)
                .map(|(slot, coefficient)| (slot as u16, coefficient))
                .collect(),
        }
    }
}

/// What is left on the board, on the scale the taper is read at.
///
/// Uncapped: the cap belongs where the evaluation puts it, and a promotion
/// can leave more on the board than the opening had.
fn phase_of(board: &Board) -> i32 {
    let mut phase = 0;
    let mut occupied = board.occupied();
    while occupied != 0 {
        let index = occupied.trailing_zeros() as u8;
        occupied &= occupied - 1;
        if let Some((piece, _)) = board.get_piece_and_color_index(index) {
            phase += eval::phase_weight(piece);
        }
    }
    phase
}

/// The evaluation a row states, folded back against the live tables.
///
/// Three details here are the whole of what a reader of these rows has to
/// get right, and each of them is a way to be wrong by a centipawn:
///
/// - the divide truncates toward zero, where python's `//` floors. On a
///   negative numerator that does not divide evenly the two differ by one;
/// - the material is added outside the divide and not scaled into it.
///   `trunc((24 * 1 + -5) / 24)` is 0 where `1 + trunc(-5 / 24)` is 1;
/// - the phase is capped before it is used, which [`Terms::of`] does.
///
/// The cast is the evaluation's own, which wraps. Nothing here widens what
/// the engine narrows: a row that came back a different integer would be a
/// row about some other evaluation.
pub fn reconstruct(terms: &Terms) -> Score {
    let mut material = 0;
    let mut numerator = 0;
    for &(slot, coefficient) in &terms.coefficients {
        let product = coefficient * weight(usize::from(slot));
        if usize::from(slot) >= MATERIAL_SLOT {
            material += product;
        } else {
            numerator += product;
        }
    }
    (material + numerator / TOTAL_PHASE) as Score
}

/// The table the quiet test searches with.
///
/// Small on purpose and cleared before every position, the residuals
/// replay's reason exactly: a position's answer must not depend on which
/// positions were asked about before it. A capture search over one position
/// would not fill a larger one.
const FILTER_TABLE_BYTES: usize = 64 * 1024;

/// The engine the quiet test asks its question of.
///
/// The reference and not the default. The default's quiescence has the delta
/// margin and the losing capture skip on, so it passes over captures it
/// prices as hopeless, and those skips are guesses. A corpus whose quietness
/// was decided by a guess would carry the guess into every weight fitted on
/// it, and would move when the guess moved.
fn filter_engine() -> AlphaBeta {
    AlphaBeta::with_config(Board::new(), FILTER_TABLE_BYTES, SearchConfig::reference())
}

/// Whether neither side has anything to win by capturing.
///
/// Two sided, and the pass is the half that makes it so. A one sided test
/// keeps the position where the side to move is about to lose a hanging
/// queen and labels an evaluation that misses it with the result of a game
/// that did not.
///
/// The caller has already refused a position whose side to move is in check,
/// which is what leaves the position after a pass a well formed one.
fn settled(engine: &mut AlphaBeta, board: &Board) -> bool {
    if !nothing_to_capture(engine, board.clone()) {
        return false;
    }
    let mut passed = board.clone();
    passed.make_null_move();
    nothing_to_capture(engine, passed)
}

/// Whether the side to move's capture search comes back at the static
/// evaluation.
fn nothing_to_capture(engine: &mut AlphaBeta, board: Board) -> bool {
    engine.board = board;
    engine.clear_transpositions();
    engine.quiescence_value() == eval::eval(&engine.board)
}

/// One position of the corpus and what its evaluation is made of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    /// What `eval::eval` returns for the position, which is what the row's
    /// own arithmetic has to come to.
    pub eval: Score,
    pub terms: Terms,
    pub fen: String,
}

/// A whole run: the suite it read, what the filter turned away, and a row a
/// position.
#[derive(Clone, Debug)]
pub struct Report {
    /// The file the positions were read from, or none for the bench's own.
    pub suite: Option<String>,
    /// Positions the run was given, which is the denominator the three counts
    /// below are shares of.
    pub positions: usize,
    /// Positions refused because the side to move was in check. A checked
    /// position's static evaluation is not a thing to fit, and quiescence
    /// treats it differently anyway.
    pub in_check: usize,
    /// Positions refused because a capture search moved the evaluation, for
    /// one side or the other.
    pub unsettled: usize,
    pub rows: Vec<Row>,
}

/// Extract the terms of every quiet position in the suite.
///
/// The identity is asserted on every row rather than only in a test. A test
/// over a suite says the walk is right on that suite; the assertion says it
/// was right on every row that was actually fitted, which is the statement a
/// corpus needs. It panics rather than dropping the row, the way the
/// recorders panic on a fen that will not parse.
///
/// `suite` names the file the positions came from, for the header alone.
pub fn run(positions: &[Position], suite: Option<&str>) -> Report {
    let mut engine = filter_engine();
    let mut report = Report {
        suite: suite.map(str::to_string),
        positions: positions.len(),
        in_check: 0,
        unsettled: 0,
        rows: Vec::new(),
    };
    for position in positions {
        let board = Board::from_fen(&position.fen)
            .unwrap_or_else(|e| panic!("terms position {} does not parse: {}", position.id, e));
        if board.in_check() {
            report.in_check += 1;
            continue;
        }
        if !settled(&mut engine, &board) {
            report.unsettled += 1;
            continue;
        }
        let terms = Terms::of(&board);
        let eval = eval::eval(&board);
        let rebuilt = reconstruct(&terms);
        assert_eq!(
            rebuilt, eval,
            "terms position {} reconstructs to {} and evaluates to {}",
            position.id, rebuilt, eval
        );
        report.rows.push(Row {
            id: position.id.clone(),
            eval,
            terms,
            fen: board.to_fen(),
        });
    }
    report
}

/// The report as the command prints it: a header naming the suite and what
/// the filter did with it, the weight vector, then a row a position.
///
/// A row is `id eval phase n slot:coefficient... fen`, whitespace separated,
/// and both ends of it can hold spaces. A fen is six fields, and an id is
/// whatever an epd put in the quotes, which in the bench's own suite is
/// "ruy lopez" and in the strategic suite "7th Rank.001" (a line that names
/// no id is called by its own fen, so an id can be six fields itself). So a
/// row is read from its right hand end: the fen is the last six fields, the
/// coefficients are the run of `slot:coefficient` in front of it, and what
/// is left before the three numbers is the id. `n` is printed so the two
/// ends can be held against each other rather than one of them trusted.
///
/// The id is printed as the epd gave it rather than quoted. Quoting would
/// need an escape rule the epd itself does not have, and the id is the key
/// a corpus is joined on, so what is printed has to be the name the file
/// wrote.
///
/// The weights are printed as well as the coefficients, so that what reads
/// these rows never transcribes psqt.rs. A transcription is the same failure
/// as a reimplemented evaluation and quieter: a table copied out and left
/// behind fits weights against a position it scores differently from the
/// engine, and nothing says so. With the vector on the line a reader can
/// rebuild every row's stated evaluation and find out.
///
/// The header states what the run turned away beside what it kept, for the
/// reason the recorders state their events beside their records: a corpus is
/// a share of a suite, and the share cannot be read without knowing what it
/// is a share of.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "terms")?;
        // the bench's own suite is the default and reads as absent, the way
        // the other instruments' headers leave it out
        if let Some(suite) = &self.suite {
            write!(f, " epd {}", suite)?;
        }
        writeln!(
            f,
            " positions {} in_check {} unsettled {} kept {}",
            self.positions,
            self.in_check,
            self.unsettled,
            self.rows.len(),
        )?;
        write!(f, "weights {}", SLOTS)?;
        for slot in 0..SLOTS {
            write!(f, " {}", weight(slot))?;
        }
        writeln!(f)?;
        for row in &self.rows {
            write!(
                f,
                "{} {} {} {}",
                row.id,
                row.eval,
                row.terms.phase,
                row.terms.coefficients.len()
            )?;
            for (slot, coefficient) in &row.terms.coefficients {
                write!(f, " {}:{}", slot, coefficient)?;
            }
            writeln!(f, " {}", row.fen)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::fens;
    use crate::misc::File;
    use crate::misc::coordinate_to_index;
    use crate::value::CHECKMATE_THRESHOLD;
    use crate::{bench, strategy};
    use pretty_assertions::assert_eq;

    /// Every position three suites of different shapes hold: the shared
    /// fens, the bench's eighteen and the strategic suite's fifteen hundred.
    fn every_shape() -> Vec<String> {
        let mut fens: Vec<String> = fens::CORE.iter().map(|f| f.to_string()).collect();
        fens.extend(bench::positions().into_iter().map(|p| p.fen));
        fens.extend(strategy::positions().into_iter().map(|p| p.fen));
        fens
    }

    /// The pin the whole arm rests on. A row's own arithmetic against the
    /// live tables is the evaluation, exactly, on every position of three
    /// suites of different shapes.
    #[test]
    fn a_positions_terms_reconstruct_its_evaluation() {
        let fens = every_shape();
        assert!(fens.len() > 1_500, "{} positions", fens.len());
        for fen in fens {
            let board = Board::from_fen(&fen).unwrap_or_else(|e| panic!("{}: {}", fen, e));
            assert_eq!(
                reconstruct(&Terms::of(&board)),
                eval::eval(&board),
                "{}",
                fen
            );
        }
    }

    /// The coefficients carry the sign rather than the reader carrying it, so
    /// the same position read from the other side is its exact negative
    /// through the row.
    #[test]
    fn a_row_reconstructs_from_the_side_to_move() {
        for fen in every_shape() {
            let mut board = Board::from_fen(&fen).unwrap();
            let ours = reconstruct(&Terms::of(&board));
            board.active_color = !board.active_color;
            assert_eq!(reconstruct(&Terms::of(&board)), -ours, "{}", fen);
        }
    }

    /// Every piece writes both ends of the taper, and the coefficients say
    /// so rather than the evaluation they add up to.
    ///
    /// While the four new tables are copies of their twins the weight at a
    /// square is the same at both ends, so a dot product cannot tell a split
    /// from no split: an endgame coefficient written to its midgame slot
    /// reproduces every row of the corpus and every reconstruction test above.
    /// The two shares of the taper differ here, so a coefficient on the wrong
    /// slot, or the two swapped, is a different number and not a different
    /// route to the same one.
    #[test]
    fn every_piece_writes_both_ends_of_the_taper() {
        // a knight, a bishop, a rook and a queen for white, so the four
        // tables that were added carry a coefficient of their own, and no
        // black piece of any of those kinds to cancel one out. The kings
        // stand off the mirror of each other for the same reason
        let fen = "7k/8/8/8/8/8/4P3/RNBQK3 w - - 0 1";
        let board = Board::from_fen(fen).unwrap();
        let terms = Terms::of(&board);
        // a knight and a bishop at one apiece, a rook at two and a queen at
        // four
        assert_eq!(terms.phase, 8);
        assert_ne!(
            terms.phase,
            TOTAL_PHASE - terms.phase,
            "the two ends hold the same share here, so this test cannot tell them apart"
        );
        let coefficient = |slot: usize| {
            terms
                .coefficients
                .iter()
                .find(|(named, _)| usize::from(*named) == slot)
                .map_or(0, |(_, coefficient)| *coefficient)
        };
        for (piece, file, rank) in [
            (Piece::Pawn, File::E, 2),
            (Piece::Knight, File::B, 1),
            (Piece::Bishop, File::C, 1),
            (Piece::Rook, File::A, 1),
            (Piece::Queen, File::D, 1),
            (Piece::King, File::E, 1),
        ] {
            // white reads the tables mirrored, so a white piece's slot is
            // named by the square black would be on
            let entry = usize::from(coordinate_to_index(rank, file) ^ 56);
            let slot = piece as usize * 64 + entry;
            assert_eq!(coefficient(slot), terms.phase, "{:?} midgame", piece);
            assert_eq!(
                coefficient(MIDGAME_SLOTS + slot),
                TOTAL_PHASE - terms.phase,
                "{:?} endgame",
                piece
            );
        }
    }

    /// A position whose piece square numerator is negative and does not
    /// divide by twenty four evenly, which is what the two tests below need
    /// to tell two readings of the arithmetic apart. A knight a side would
    /// leave the phase at nothing and the numerator a multiple of the taper.
    const UNEVEN: &str = "4k3/8/8/8/8/8/4P3/1N2K3 w - - 0 1";

    /// The divide is Rust's, which truncates toward zero, where a floor would
    /// take a negative numerator the other way, so the two differ by a
    /// centipawn here and the row is asserted to the integer the engine gives
    /// it.
    #[test]
    fn the_psqt_divide_truncates_toward_zero() {
        let fen = UNEVEN;
        let board = Board::from_fen(fen).unwrap();
        let terms = Terms::of(&board);
        let numerator: i32 = terms
            .coefficients
            .iter()
            .filter(|(slot, _)| usize::from(*slot) < MATERIAL_SLOT)
            .map(|(slot, coefficient)| coefficient * weight(usize::from(*slot)))
            .sum();
        assert!(numerator < 0, "the numerator is {}", numerator);
        assert!(
            numerator % TOTAL_PHASE != 0,
            "the numerator {} divides evenly, so this test cannot tell the two apart",
            numerator
        );
        let floored = numerator.div_euclid(TOTAL_PHASE);
        assert_eq!(
            numerator / TOTAL_PHASE,
            floored + 1,
            "the two readings agree here"
        );
        assert_eq!(reconstruct(&terms), eval::eval(&board), "{}", fen);
    }

    /// Material is added after the divide rather than scaled into it. Folding
    /// it in by multiplying the material coefficients by twenty four gives a
    /// different integer whenever the piece square numerator is negative and
    /// does not divide evenly, which is what the position below arranges.
    #[test]
    fn material_is_added_outside_the_divide() {
        let fen = UNEVEN;
        let board = Board::from_fen(fen).unwrap();
        let terms = Terms::of(&board);
        let (material, numerator) =
            terms
                .coefficients
                .iter()
                .fold((0, 0), |(material, numerator), (slot, coefficient)| {
                    let product = coefficient * weight(usize::from(*slot));
                    if usize::from(*slot) >= MATERIAL_SLOT {
                        (material + product, numerator)
                    } else {
                        (material, numerator + product)
                    }
                });
        let folded = (material * TOTAL_PHASE + numerator) / TOTAL_PHASE;
        assert_ne!(
            folded,
            material + numerator / TOTAL_PHASE,
            "folding the material in agrees here, so this test says nothing"
        );
        assert_eq!(
            i32::from(eval::eval(&board)),
            material + numerator / TOTAL_PHASE
        );
    }

    /// The phase is capped before the coefficients are written. Two extra
    /// queens a side leave more on the board than the opening had, and a row
    /// built from the uncapped count would give the endgame half a negative
    /// share of the taper.
    #[test]
    fn the_phase_is_capped_before_the_coefficients_are_written() {
        let fen = "rnbqkbnr/pppppppp/8/3QQ3/3qq3/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
        let board = Board::from_fen(fen).unwrap();
        assert!(
            phase_of(&board) > TOTAL_PHASE,
            "{} has a phase of {}",
            fen,
            phase_of(&board)
        );
        let terms = Terms::of(&board);
        assert_eq!(terms.phase, TOTAL_PHASE);
        assert_eq!(reconstruct(&terms), eval::eval(&board), "{}", fen);
    }

    /// A slot names the same weight the tables hold, so a fit that moves slot
    /// n moves the entry a reader of psqt.rs would go looking for. Named
    /// squares rather than a walk, since a walk would only restate `weight`.
    ///
    /// A slot's entry is a square as black sees it, because black is the
    /// colour that reads the tables as they are written.
    #[test]
    fn a_slot_names_the_table_entry_it_stands_for() {
        let entry = |file: File, rank: u8| usize::from(coordinate_to_index(rank, file));
        // a black pawn on a2 is a square from promoting, which is fifty
        assert_eq!(weight(Piece::Pawn as usize * 64 + entry(File::A, 2)), 50);
        // and the same square in the endgame table is eighty
        assert_eq!(weight(MIDGAME_SLOTS + entry(File::A, 2)), 80);
        // the king hides in the middlegame and comes out in the ending
        assert_eq!(weight(Piece::King as usize * 64 + entry(File::E, 5)), -40);
        assert_eq!(
            weight(MIDGAME_SLOTS + Piece::King as usize * 64 + entry(File::E, 5)),
            36
        );
        // the four tables that were added stand where the same arithmetic
        // puts them, a half of the vector on from their midgame twins. a1 is
        // the one corner the fit left alone in all four, which is why the
        // same number serves both halves here and nowhere else in this test.
        // The corners are not generally untouched: the rook's other three
        // moved, and moved by different amounts in the two halves
        for (piece, corner) in [
            (Piece::Knight, -50),
            (Piece::Bishop, -20),
            (Piece::Rook, 0),
            (Piece::Queen, -20),
        ] {
            let slot = piece as usize * 64 + entry(File::A, 1);
            assert_eq!(weight(slot), corner, "{:?}", piece);
            assert_eq!(weight(MIDGAME_SLOTS + slot), corner, "{:?}", piece);
        }
        for (piece, value) in [
            (Piece::Pawn, 100),
            (Piece::Knight, 310),
            (Piece::Bishop, 320),
            (Piece::Rook, 500),
            (Piece::Queen, 900),
            (Piece::King, 10_000),
        ] {
            assert_eq!(weight(MATERIAL_SLOT + piece as usize), value, "{:?}", piece);
        }
    }

    /// The largest one-sided sum of either half of the tables, against the
    /// sixteen bits each half of a packed pair has to stay inside.
    ///
    /// A boardful and not a legal position: what has to hold is the arithmetic
    /// the accumulator does, and it does not know what is legal.
    #[test]
    fn a_boardful_of_tables_stays_inside_the_packed_halves() {
        let mut midgame = 0;
        let mut endgame = 0;
        for entry in 0..64 {
            let largest = |half: fn(i32) -> i32| {
                Piece::PIECES
                    .iter()
                    .map(|piece| {
                        half(PieceSquareTables::TABLES.get_value(entry, *piece, Color::Black)).abs()
                    })
                    .max()
                    .unwrap_or(0)
            };
            midgame += largest(mg_value);
            endgame += largest(eg_value);
        }
        // both sides at once, which is what the accumulator carries
        let worst = 2 * midgame.max(endgame);
        assert!(
            worst < i32::from(i16::MAX),
            "a boardful comes to {} against {}",
            worst,
            i16::MAX
        );
    }

    /// The evaluation is cast to a `Score` with `as`, which wraps, and
    /// anything over the mate threshold is read as a forced mate. A board of
    /// nothing but queens stays well under both.
    #[test]
    fn an_evaluation_stays_under_the_mate_threshold() {
        let fen = "qqqqkqqq/qqqqqqqq/8/8/8/8/8/4K3 w - - 0 1";
        let board = Board::from_fen(fen).unwrap();
        let eval = eval::eval(&board);
        assert_eq!(reconstruct(&Terms::of(&board)), eval);
        assert!(
            eval.abs() < CHECKMATE_THRESHOLD,
            "{} evaluates to {}",
            fen,
            eval
        );
    }

    /// A quiet position keeps its row and one with a capture to make does
    /// not, whichever side has the capture. The third fen is the one the
    /// pass is for: white is to move and has nothing to take, and a one
    /// sided test would keep it and label an evaluation that misses the
    /// knight black is about to win.
    #[test]
    fn a_position_with_a_capture_to_make_is_not_quiet() {
        let quiet = "4k3/8/8/8/8/8/4P3/4K3 w - - 0 1";
        // a black knight the white pawn can take
        let ours = "4k3/8/8/8/8/3n4/4P3/6K1 w - - 0 1";
        // a white knight the black rook can take, with white to move
        let theirs = "r3k3/8/8/N7/8/8/8/4K3 w - - 0 1";
        let mut engine = filter_engine();
        for (fen, expected) in [(quiet, true), (ours, false), (theirs, false)] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(settled(&mut engine, &board), expected, "{}", fen);
        }
    }

    /// The filter's engine is the reference and not the default, which is the
    /// premise the corpus rests on: the default's quiescence skips captures
    /// its margins price as hopeless, and those skips are guesses.
    #[test]
    fn the_filter_searches_the_reference_and_not_the_default() {
        assert_eq!(filter_engine().config(), SearchConfig::reference());
        assert_ne!(SearchConfig::reference(), SearchConfig::default());
    }

    /// A run over a small suite: the header counts what it turned away beside
    /// what it kept, and every row it printed is one the identity held on.
    #[test]
    fn a_run_states_what_it_kept_and_what_it_turned_away() {
        let positions = bench::positions();
        let report = run(&positions, None);
        assert_eq!(report.positions, positions.len());
        assert_eq!(
            report.in_check + report.unsettled + report.rows.len(),
            positions.len()
        );
        assert!(!report.rows.is_empty(), "the filter kept nothing");
        let text = report.to_string();
        assert!(
            text.starts_with(&format!(
                "terms positions {} in_check {} unsettled {} kept {}\n",
                report.positions,
                report.in_check,
                report.unsettled,
                report.rows.len()
            )),
            "{}",
            text.lines().next().unwrap_or_default()
        );
        // the header, the weights and a row a position
        assert_eq!(text.lines().count(), report.rows.len() + 2);
    }

    /// The weight vector is printed beside the coefficients, so that nothing
    /// reading these rows has to transcribe psqt.rs. What is printed is what
    /// `weight` reads out of the live tables, slot by slot.
    #[test]
    fn a_run_prints_the_weights_the_coefficients_are_read_against() {
        let report = run(&bench::positions()[..1], None);
        let text = report.to_string();
        let line = text.lines().nth(1).expect("a weights line");
        let mut words = line.split(' ');
        assert_eq!(words.next(), Some("weights"));
        assert_eq!(words.next(), Some(SLOTS.to_string().as_str()));
        let printed: Vec<i32> = words.map(|w| w.parse().expect(w)).collect();
        assert_eq!(printed, (0..SLOTS).map(weight).collect::<Vec<i32>>());
    }

    /// A run over a suite of its own says so in the header, the way the other
    /// instruments' headers name a file that is not the bench's.
    #[test]
    fn the_header_names_a_suite_that_is_not_the_benchs() {
        let report = run(&bench::positions()[..1], Some("corpus.epd"));
        assert!(
            report
                .to_string()
                .starts_with("terms epd corpus.epd positions 1 "),
            "{}",
            report
        );
    }

    /// The fields of a row are found from the fen back, which is what lets
    /// an id hold spaces. The name is taken from the bench's own suite, the
    /// default this command runs, rather than invented here, so the test
    /// cannot go on claiming something the suite has stopped doing.
    #[test]
    fn a_row_reads_from_the_right_and_its_id_can_hold_spaces() {
        let spaced = bench::positions()
            .into_iter()
            .find(|position| position.id.contains(' '))
            .expect("the bench's suite names a position with a space in it");
        let fen = "4k3/8/8/8/8/8/4P3/4K3 w - - 0 1";
        let board = Board::from_fen(fen).unwrap();
        let report = Report {
            suite: None,
            positions: 1,
            in_check: 0,
            unsettled: 0,
            rows: vec![Row {
                id: spaced.id.clone(),
                eval: eval::eval(&board),
                terms: Terms::of(&board),
                fen: fen.to_string(),
            }],
        };
        let text = report.to_string();
        let row = text.lines().nth(2).expect("a row");
        let words: Vec<&str> = row.split(' ').collect();
        // the fen is the last six fields
        let (head, last_six) = words.split_at(words.len() - 6);
        assert_eq!(last_six.join(" "), fen);
        // and the coefficients are the run before it, which cannot walk back
        // into the id because the three numbers in between hold no colon
        let count_at = head
            .iter()
            .rposition(|word| !word.contains(':'))
            .expect("a count");
        for word in &head[count_at + 1..] {
            let (slot, coefficient) = word.split_once(':').expect(word);
            assert!(slot.parse::<usize>().expect(word) < SLOTS);
            coefficient.parse::<i32>().expect(word);
        }
        // a pawn and two kings: three material slots and, since each of the
        // three reads two tables, up to six piece square slots
        let count: usize = head[count_at].parse().expect("a count");
        assert_eq!(count, report.rows[0].terms.coefficients.len());
        assert_eq!(head[count_at - 2], eval::eval(&board).to_string());
        // three pieces and no piece worth a phase weight, so the endgame end
        assert_eq!(head[count_at - 1], "0");
        assert_eq!(head[..count_at - 2].join(" "), spaced.id);
    }
}
