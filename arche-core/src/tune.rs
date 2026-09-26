// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What a position's evaluation is made of, weight by weight.
//!
//! The evaluation is material plus tapered terms, linear in the weights, so a
//! position's score is a dot product of one coefficient per weight it touches
//! against the weights. This module writes the coefficients down and
//! `scripts/tune.py` fits them.
//!
//! The one place the evaluation is not linear is material that cannot mate,
//! which `eval` answers with a hard zero. `run` turns those positions away
//! and counts them in the header, and `scripts/tune.py` refuses a header
//! without the count, since an extraction printed before the rule holds rows
//! the rule would have dropped.
//!
//! The engine states the coefficients and never the arithmetic, so the tuner
//! carries no second implementation of the evaluation. [`reconstruct`] states
//! that the two agree, and `run` asserts it on every row it prints.
//!
//! The walk is a third pass over the board beside `Accumulator::count` and
//! `Accumulator::recomputed`, and shares no helper with them on purpose: two
//! implementations that agree are a check, and a shared helper is not. The
//! leaf terms are the exception: the walk asks each term in `eval::TERMS` for
//! its counts, which for shelter and pawn structure is the function `eval`
//! reads, so the identity cannot see a wrong count there. For mobility and
//! the king attack zone `eval` reads a shared walk that
//! `the_shared_walk_counts_what_each_term_counts_alone` holds to these
//! functions, and neither can see a count wrong the same way in both. The
//! hand counts beside each term in `eval/` pin those.
//!
//! Where a term stands in the vector, its weights and its width come off
//! `eval::TERMS`, so a term added there needs no edit here, and the layout
//! line states the result for `scripts/tune.py`.
//!
//! On mobility the walk counts all four kinds, so a fit can price a kind
//! worth nothing today, while `eval` counts only kinds with a weight. The
//! identity still holds where a weight is zero, which
//! `eval_counts_a_kind_exactly_when_its_weight_is_not_zero` pins.

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

/// Laid out as the midgame half, so a square's two weights are
/// [`MIDGAME_SLOTS`] apart. Rows printed before d17622f, and vectors fitted
/// against them, are 518 wide; the layout line is what refuses them.
pub const ENDGAME_SLOTS: usize = 6 * 64;

pub const MATERIAL_SLOT: usize = MIDGAME_SLOTS + ENDGAME_SLOTS;

const MATERIAL_SLOTS: usize = 6;

/// Where the leaf terms start. Each takes twice its width, midgame half
/// first, in `eval::TERMS` order. A term is appended so that no slot a fit
/// was written against moves; growing a term in place moves its own endgame
/// half and every term after it.
pub const TERM_SLOT: usize = MATERIAL_SLOT + MATERIAL_SLOTS;

const fn term_slot(index: usize) -> usize {
    let mut slot = TERM_SLOT;
    let mut before = 0;
    while before < index {
        slot += 2 * eval::TERMS[before].width;
        before += 1;
    }
    slot
}

pub const SLOTS: usize = term_slot(eval::TERMS.len());

/// The weight a slot names, read out of the live tables rather than a copy,
/// so the only error left for the pin to catch is a wrong coefficient.
///
/// Black reads the tables as written, so a table's own index is the square to
/// ask black about, and a slot is a (piece, table index) pair rather than a
/// (piece, colour, square) triple.
pub fn weight(slot: usize) -> i32 {
    let packed = |piece: Piece, entry: usize| {
        PieceSquareTables::TABLES.get_value(entry, piece, Color::Black)
    };
    if slot < MIDGAME_SLOTS {
        return mg_value(packed(Piece::PIECES[slot / 64], slot % 64));
    }
    if slot < MATERIAL_SLOT {
        let entry = slot - MIDGAME_SLOTS;
        return eg_value(packed(Piece::PIECES[entry / 64], entry % 64));
    }
    if slot < TERM_SLOT {
        return eval::material(Piece::PIECES[slot - MATERIAL_SLOT]) as i32;
    }
    let mut index = slot - TERM_SLOT;
    for term in eval::TERMS {
        if index < term.width {
            return mg_value((term.weight)(index));
        }
        if index < 2 * term.width {
            return eg_value((term.weight)(index - term.width));
        }
        index -= 2 * term.width;
    }
    panic!("slot {} is past the {} the vector holds", slot, SLOTS)
}

/// The material values are the only weights added outside the taper's
/// divide.
fn is_material(slot: usize) -> bool {
    (MATERIAL_SLOT..TERM_SLOT).contains(&slot)
}

/// One position's evaluation, decomposed over the weight vector.
///
/// The coefficients are in the side to move's frame, so a row's arithmetic
/// is the evaluation with no further step. A column's non-zero count is how
/// much of the corpus a weight is fitted on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terms {
    /// Capped as the evaluation caps it. A property of the position rather
    /// than of the weights, which keeps the decomposition linear.
    pub phase: i32,
    /// Slot and coefficient, ascending by slot, zeroes left out.
    pub coefficients: Vec<(u16, i32)>,
}

impl Terms {
    pub fn of(board: &Board) -> Self {
        let phase = phase_of(board).min(TOTAL_PHASE);
        // the sign the evaluation applies at the end is folded into every
        // coefficient. The divide truncates toward zero, which is odd, so a
        // sign inside it and a sign outside it give the same integer
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
            // slot is named by the table's own index
            let entry = match color {
                Color::White => usize::from(index ^ 56),
                Color::Black => usize::from(index),
            };
            let midgame = piece as usize * 64 + entry;
            coefficients[midgame] += sign * phase;
            coefficients[MIDGAME_SLOTS + midgame] += sign * (TOTAL_PHASE - phase);
        }
        // the leaf terms count per position rather than per square, tapered
        // the way a square is
        let mut counts = [0; eval::WIDEST];
        for (color, sign) in [(Color::White, mover), (Color::Black, -mover)] {
            let mut slot = TERM_SLOT;
            for term in eval::TERMS {
                let counts = &mut counts[..term.width];
                (term.counts)(board, color, counts);
                for (index, count) in counts.iter().enumerate() {
                    coefficients[slot + index] += sign * count * phase;
                    coefficients[slot + term.width + index] += sign * count * (TOTAL_PHASE - phase);
                }
                slot += 2 * term.width;
            }
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
/// Three details are what a reader of these rows has to get right, and each
/// is a way to be wrong by a centipawn:
///
/// - the divide truncates toward zero, where python's `//` floors. On a
///   negative numerator that does not divide evenly the two differ by one;
/// - the material is added outside the divide and not scaled into it.
///   `trunc((24 * 1 + -5) / 24)` is 0 where `1 + trunc(-5 / 24)` is 1;
/// - the phase is capped before it is used, which [`Terms::of`] does.
///
/// The cast is the evaluation's own, which wraps, so the row comes back the
/// integer the engine gives and not a wider one.
pub fn reconstruct(terms: &Terms) -> Score {
    let mut material = 0;
    let mut numerator = 0;
    for &(slot, coefficient) in &terms.coefficients {
        let product = coefficient * weight(usize::from(slot));
        if is_material(usize::from(slot)) {
            material += product;
        } else {
            numerator += product;
        }
    }
    (material + numerator / TOTAL_PHASE) as Score
}

/// Small, and cleared before every position, so no answer depends on the
/// positions asked about before it.
const FILTER_TABLE_BYTES: usize = 64 * 1024;

/// The reference, not the default: the default's quiescence skips captures
/// on guesses, and a corpus whose quietness was decided by a guess would
/// carry it into every weight fitted on it.
fn filter_engine() -> AlphaBeta {
    AlphaBeta::with_config(Board::new(), FILTER_TABLE_BYTES, SearchConfig::reference())
}

/// Whether neither side has anything to win by capturing. A one sided test
/// would keep a position where the side to move is about to lose a hanging
/// queen.
///
/// The caller has already refused a position in check, so the position
/// after a pass is well formed.
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
    pub eval: Score,
    pub terms: Terms,
    pub fen: String,
}

#[derive(Clone, Debug)]
pub struct Report {
    /// The file the positions were read from, or none for the bench's own.
    pub suite: Option<String>,
    pub positions: usize,
    /// A checked position's static evaluation is not a thing to fit.
    pub in_check: usize,
    /// A capture search moved the evaluation for one side or the other.
    pub unsettled: usize,
    /// The material cannot mate. Turned away rather than emitted with a
    /// flag, because a row kept for the record is a row a later fit reads by
    /// accident.
    pub drawn: usize,
    pub rows: Vec<Row>,
}

/// Extract the terms of every quiet position in the suite, panicking on a
/// row whose identity fails rather than dropping it. `suite` is for the
/// header alone.
pub fn run(positions: &[Position], suite: Option<&str>) -> Report {
    let mut engine = filter_engine();
    let mut report = Report {
        suite: suite.map(str::to_string),
        positions: positions.len(),
        in_check: 0,
        unsettled: 0,
        drawn: 0,
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
        // `eval` answers a hard zero while the coefficients state the pieces
        if board.drawn_by_material() {
            report.drawn += 1;
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

/// A header, the layout line, the weight vector, then a row a position.
///
/// The layout line is `layout midgame 384 endgame 384 material 6` and then a
/// term and its width for each of `eval::TERMS`, a width being the counts a
/// term is measured in per side and per half of the taper, so a term takes
/// twice that in slots, its midgame half first. `scripts/tune.py` derives
/// its slots from this line rather than holding a copy of the layout, and
/// refuses a run naming a term it has no bounds for.
///
/// The weights are printed so that nothing reading these rows transcribes
/// psqt.rs, where a stale copy would fit silently against the wrong table.
///
/// A row is `id eval phase n slot:coefficient... fen`, whitespace separated,
/// and both ends of it can hold spaces: a fen is six fields, and an id is
/// whatever the epd put in the quotes ("ruy lopez", "7th Rank.001", or the
/// fen itself for a line with no id). So a row is read from its right hand
/// end: the fen is the last six fields, the coefficients are the run of
/// `slot:coefficient` in front of it, and what is left before the three
/// numbers is the id. `n` lets the two ends be held against each other. The
/// id is printed as the epd gave it rather than quoted, since the epd has no
/// escape rule and the id is the key a corpus is joined on.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "terms")?;
        if let Some(suite) = &self.suite {
            write!(f, " epd {}", suite)?;
        }
        writeln!(
            f,
            " positions {} in_check {} unsettled {} drawn {} kept {}",
            self.positions,
            self.in_check,
            self.unsettled,
            self.drawn,
            self.rows.len(),
        )?;
        write!(
            f,
            "layout midgame {} endgame {} material {}",
            MIDGAME_SLOTS, ENDGAME_SLOTS, MATERIAL_SLOTS
        )?;
        for term in eval::TERMS {
            write!(f, " {} {}", term.name, term.width)?;
        }
        writeln!(f)?;
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

    /// Where a named term's block starts and how wide it is.
    fn term(name: &str) -> (usize, usize) {
        let mut slot = TERM_SLOT;
        for term in eval::TERMS {
            if term.name == name {
                return (slot, term.width);
            }
            slot += 2 * term.width;
        }
        panic!("no term is called {}", name)
    }

    fn every_shape() -> Vec<String> {
        let mut fens: Vec<String> = fens::CORE.iter().map(|f| f.to_string()).collect();
        fens.extend(bench::positions().into_iter().map(|p| p.fen));
        fens.extend(strategy::positions().into_iter().map(|p| p.fen));
        fens
    }

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

    #[test]
    fn a_row_reconstructs_from_the_side_to_move() {
        for fen in every_shape() {
            let mut board = Board::from_fen(&fen).unwrap();
            let ours = reconstruct(&Terms::of(&board));
            board.active_color = !board.active_color;
            assert_eq!(reconstruct(&Terms::of(&board)), -ours, "{}", fen);
        }
    }

    /// Every piece writes both ends of the taper, asserted on the
    /// coefficients rather than the evaluation they add up to: wherever a
    /// square's two weights are equal, a dot product cannot tell an endgame
    /// coefficient written to its midgame slot from the right one. The two
    /// shares of the taper differ here, so a coefficient on the wrong slot,
    /// or the two swapped, is a different number.
    #[test]
    fn every_piece_writes_both_ends_of_the_taper() {
        // one white piece of each kind and no black piece of any of them to
        // cancel a coefficient out. The kings stand off the mirror of each
        // other for the same reason
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

    /// Every mobile piece writes both ends of the taper too, and the
    /// coefficient is asserted against a count worked out by hand: the
    /// identity reads these eight slots through one sum, so two errors that
    /// cancel pass it, and a weight the next refit puts back at zero hides a
    /// coefficient on its slot entirely.
    #[test]
    fn every_mobile_piece_writes_both_ends_of_the_taper() {
        // the piece square test's position, for the same reason: one white
        // piece of each mobile kind and no black piece to cancel it out
        let fen = "7k/8/8/8/8/8/4P3/RNBQK3 w - - 0 1";
        let board = Board::from_fen(fen).unwrap();
        let terms = Terms::of(&board);
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
        for (index, count, why) in [
            // the knight on b1 has a3, c3 and d2
            (0, 3, "knight"),
            // the bishop on c1 has b2 and a3 one way, and d2 out to h6 the
            // other
            (1, 7, "bishop"),
            // the rook on a1 has the a file, and the knight beside it is
            // neither scope nor something to see through
            (2, 7, "rook"),
            // the queen on d1 has the d file, and c2, b3 and a4 the other
            // way. The pawn on e2 blocks the diagonal beside that one
            (3, 10, "queen"),
        ] {
            let (start, width) = term("mobility");
            assert_eq!(
                coefficient(start + index),
                count * terms.phase,
                "{} midgame",
                why
            );
            assert_eq!(
                coefficient(start + width + index),
                count * (TOTAL_PHASE - terms.phase),
                "{} endgame",
                why
            );
        }
    }

    /// A position and its reflection with the colours swapped state the same
    /// row, coefficient for coefficient, so the mobility counts are signed
    /// and slotted the same way for both sides. The side to move is
    /// reflected too, which is what makes the rows identical rather than
    /// opposite.
    #[test]
    fn a_mirrored_position_states_the_same_row() {
        let white = Board::from_fen("4k3/pp6/2n5/8/3B4/8/6PP/4K3 w - - 0 1").unwrap();
        let black = Board::from_fen("4k3/6pp/8/3b4/8/2N5/PP6/4K3 b - - 0 1").unwrap();
        let terms = Terms::of(&white);
        assert!(
            terms
                .coefficients
                .iter()
                .any(|(slot, _)| usize::from(*slot) >= term("mobility").0),
            "no mobility coefficient here, so this test says nothing about one"
        );
        assert_eq!(terms, Terms::of(&black));
        assert_eq!(eval::eval(&white), eval::eval(&black));
    }

    /// Every piece bearing on the enemy king's ring writes both ends of the
    /// taper too, asserted against a count worked out by hand. The identity
    /// catches a coefficient on the wrong slot only where the two weights
    /// differ, and cannot see a count wrong the same way in the tuner's walk
    /// and in the shared walk `eval` reads.
    #[test]
    fn every_king_attack_count_writes_both_ends_of_the_taper() {
        // one white piece of each kind that carries a weight, and no black
        // piece of any of them to cancel a coefficient out. The black king on
        // g8 has the ring f7, g7, h7, f8 and h8
        let fen = "6k1/R7/4N2Q/8/8/3B4/8/6K1 w - - 0 1";
        let board = Board::from_fen(fen).unwrap();
        let terms = Terms::of(&board);
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
        for (index, count, why) in [
            // the knight on e6 has f8 and g7 of its eight
            (0, 2, "knight"),
            // the bishop on d3 has h7, up e4, f5 and g6
            (1, 1, "bishop"),
            // the rook on a7 has f7, g7 and h7 along the rank
            (2, 3, "rook"),
            // the queen on h6 has h7 and h8 up the file and g7 and f8 up the
            // diagonal
            (3, 4, "queen"),
        ] {
            let (start, width) = term("king_attack");
            assert_eq!(
                coefficient(start + index),
                count * terms.phase,
                "{} midgame",
                why
            );
            assert_eq!(
                coefficient(start + width + index),
                count * (TOTAL_PHASE - terms.phase),
                "{} endgame",
                why
            );
        }
    }

    /// A position and its reflection with the colours swapped state the same
    /// row here too, so the king attack counts are signed and slotted the same
    /// way for both sides and each side reads the other king's ring.
    #[test]
    fn a_mirrored_position_states_the_same_king_attack_row() {
        let white = Board::from_fen("4k3/8/2N5/8/8/5b2/8/4K3 w - - 0 1").unwrap();
        let black = Board::from_fen("4k3/8/5B2/8/8/2n5/8/4K3 b - - 0 1").unwrap();
        let terms = Terms::of(&white);
        assert!(
            terms
                .coefficients
                .iter()
                .any(|(slot, _)| usize::from(*slot) >= term("king_attack").0),
            "no king attack coefficient here, so this test says nothing about one"
        );
        assert_eq!(terms, Terms::of(&black));
        assert_eq!(eval::eval(&white), eval::eval(&black));
    }

    /// A position whose piece square numerator is negative and does not
    /// divide by twenty four evenly, which the two tests below need to tell
    /// two readings of the arithmetic apart.
    const UNEVEN: &str = "4k3/8/8/8/8/8/4P3/1N2K3 w - - 0 1";

    #[test]
    fn the_psqt_divide_truncates_toward_zero() {
        let fen = UNEVEN;
        let board = Board::from_fen(fen).unwrap();
        let terms = Terms::of(&board);
        let numerator: i32 = terms
            .coefficients
            .iter()
            .filter(|(slot, _)| !is_material(usize::from(*slot)))
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
                    if is_material(usize::from(*slot)) {
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

    /// An uncapped phase would give the endgame half a negative share of the
    /// taper.
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

    /// Named squares rather than a walk, since a walk would only restate
    /// `weight`. A slot's entry is a square as black sees it.
    #[test]
    fn a_slot_names_the_table_entry_it_stands_for() {
        let entry = |file: File, rank: u8| usize::from(coordinate_to_index(rank, file));
        // a black pawn on a2 is a square from promoting
        assert_eq!(weight(Piece::Pawn as usize * 64 + entry(File::A, 2)), 50);
        assert_eq!(weight(MIDGAME_SLOTS + entry(File::A, 2)), 80);
        // the king hides in the middlegame and comes out in the ending
        assert_eq!(weight(Piece::King as usize * 64 + entry(File::E, 5)), -40);
        assert_eq!(
            weight(MIDGAME_SLOTS + Piece::King as usize * 64 + entry(File::E, 5)),
            36
        );
        // the other four pieces' endgame tables stand a half of the vector
        // on from their midgame twins. a1 is the one corner the fit left
        // alone in all four, which is why one number serves both halves here
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

    /// The most of any one shelter count a side can show: three pawns on each
    /// of the five ranks counted, and three files, which the open and the
    /// half open counts share between them rather than reach each.
    const MAX_SHELTER: i32 = 3;

    /// The most of any one pawn count a side can show. Charging every one of
    /// the eight counts at eight is far past any position (a side has eight
    /// pawns between all of them), and loose on the safe side on purpose:
    /// what this catches is a vector that has gone somewhere else entirely.
    const MAX_PAWNS: i32 = 8;

    /// The largest one-sided sum of either half of the tables, plus what the
    /// shelter and the pawn structure add to the same halves, against the
    /// sixteen bits each half of a packed pair has to stay inside.
    ///
    /// A boardful and not a legal position: what has to hold is the
    /// arithmetic the accumulator does, and it does not know what is legal.
    /// This is a screen over the terms whose maximum counts are stated here,
    /// the tables and those two. `tune.py::bounds_hold` charges the tables and
    /// every term the layout names, and refuses a fit that has grown past a
    /// boardful.
    #[test]
    fn a_boardful_stays_inside_the_packed_halves() {
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
        let charged = |name: &str, half: fn(i32) -> i32| {
            let named = eval::TERMS
                .iter()
                .find(|term| term.name == name)
                .expect(name);
            (0..named.width)
                .map(|index| half((named.weight)(index)).abs())
                .sum::<i32>()
        };
        let shelter = |half: fn(i32) -> i32| charged("shelter", half);
        let pawns = |half: fn(i32) -> i32| charged("pawn_structure", half);
        // both sides at once, which is what the accumulator carries
        let worst = 2 * midgame.max(endgame)
            + 2 * MAX_SHELTER * shelter(mg_value).max(shelter(eg_value))
            + 2 * MAX_PAWNS * pawns(mg_value).max(pawns(eg_value));
        assert!(
            worst < i32::from(i16::MAX),
            "a boardful comes to {} against {}",
            worst,
            i16::MAX
        );
    }

    /// The evaluation is cast to a `Score` with `as`, which wraps, and
    /// anything over the mate threshold is read as a forced mate. A board of
    /// nothing but queens stays under both.
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

    /// In the third fen white is to move and has nothing to take, so a one
    /// sided test would keep it.
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

    #[test]
    fn the_filter_searches_the_reference_and_not_the_default() {
        assert_eq!(filter_engine().config(), SearchConfig::reference());
        assert_ne!(SearchConfig::reference(), SearchConfig::default());
    }

    fn one_position(fen: &str) -> Vec<Position> {
        vec![Position {
            id: fen.to_string(),
            fen: fen.to_string(),
            operations: std::collections::HashMap::new(),
        }]
    }

    /// A position drawn by material is counted and not kept, and the same
    /// position with a pawn on it is kept. A rule that turned away every
    /// pawnless position, or every position, would pass the first half alone.
    #[test]
    fn a_position_drawn_by_material_is_turned_away_and_counted() {
        let drawn = run(&one_position("8/8/8/8/8/4k3/8/4K1N1 w - - 0 1"), None);
        assert_eq!((drawn.drawn, drawn.rows.len()), (1, 0));
        let kept = run(&one_position("8/8/8/4p3/8/4k3/8/4K1N1 w - - 0 1"), None);
        assert_eq!((kept.drawn, kept.rows.len()), (0, 1));
    }

    /// Why the position above has to be turned away: the evaluation answers
    /// zero and the coefficients still state the knight, so the identity the
    /// run asserts on every kept row does not hold there.
    #[test]
    fn the_identity_is_what_a_drawn_position_would_break() {
        let board = Board::from_fen("8/8/8/8/8/4k3/8/4K1N1 w - - 0 1").unwrap();
        assert_eq!(eval::eval(&board), 0);
        assert_ne!(reconstruct(&Terms::of(&board)), 0);
    }

    #[test]
    fn a_run_states_what_it_kept_and_what_it_turned_away() {
        let positions = bench::positions();
        let report = run(&positions, None);
        assert_eq!(report.positions, positions.len());
        assert_eq!(
            report.in_check + report.unsettled + report.drawn + report.rows.len(),
            positions.len()
        );
        assert!(!report.rows.is_empty(), "the filter kept nothing");
        let text = report.to_string();
        assert!(
            text.starts_with(&format!(
                "terms positions {} in_check {} unsettled {} drawn {} kept {}\n",
                report.positions,
                report.in_check,
                report.unsettled,
                report.drawn,
                report.rows.len()
            )),
            "{}",
            text.lines().next().unwrap_or_default()
        );
        // the header, the layout, the weights and a row a position
        assert_eq!(text.lines().count(), report.rows.len() + 3);
    }

    /// What reads the rows has no copy of the layout, so this line is the
    /// whole of what it is told.
    #[test]
    fn a_run_prints_the_layout_the_slots_are_laid_out_from() {
        let text = run(&bench::positions()[..1], None).to_string();
        let line = text.lines().nth(1).expect("a layout line");
        let mut words = line.split(' ');
        assert_eq!(words.next(), Some("layout"));
        let mut named: Vec<(String, usize)> = Vec::new();
        while let Some(name) = words.next() {
            let width = words.next().expect("a width").parse().expect("a width");
            named.push((name.to_string(), width));
        }
        let mut expected: Vec<(String, usize)> = vec![
            ("midgame".to_string(), MIDGAME_SLOTS),
            ("endgame".to_string(), ENDGAME_SLOTS),
            ("material".to_string(), MATERIAL_SLOTS),
        ];
        expected.extend(
            eval::TERMS
                .iter()
                .map(|term| (term.name.to_string(), term.width)),
        );
        assert_eq!(named, expected);
        // the three blocks are counted whole and a term twice over
        let counted: usize = named[..3].iter().map(|(_, width)| width).sum::<usize>()
            + 2 * named[3..].iter().map(|(_, width)| width).sum::<usize>();
        assert_eq!(counted, SLOTS);
    }

    #[test]
    fn a_run_prints_the_weights_the_coefficients_are_read_against() {
        let report = run(&bench::positions()[..1], None);
        let text = report.to_string();
        let line = text.lines().nth(2).expect("a weights line");
        let mut words = line.split(' ');
        assert_eq!(words.next(), Some("weights"));
        assert_eq!(words.next(), Some(SLOTS.to_string().as_str()));
        let printed: Vec<i32> = words.map(|w| w.parse().expect(w)).collect();
        assert_eq!(printed, (0..SLOTS).map(weight).collect::<Vec<i32>>());
    }

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

    /// The id is taken from the bench's own suite rather than invented, so
    /// the test cannot go on claiming something the suite has stopped doing.
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
            drawn: 0,
            rows: vec![Row {
                id: spaced.id.clone(),
                eval: eval::eval(&board),
                terms: Terms::of(&board),
                fen: fen.to_string(),
            }],
        };
        let text = report.to_string();
        let row = text.lines().nth(3).expect("a row");
        let words: Vec<&str> = row.split(' ').collect();
        // the fen is the last six fields
        let (head, last_six) = words.split_at(words.len() - 6);
        assert_eq!(last_six.join(" "), fen);
        // the coefficients are the run before it, which cannot walk back into
        // the id because the three numbers in between hold no colon
        let count_at = head
            .iter()
            .rposition(|word| !word.contains(':'))
            .expect("a count");
        for word in &head[count_at + 1..] {
            let (slot, coefficient) = word.split_once(':').expect(word);
            assert!(slot.parse::<usize>().expect(word) < SLOTS);
            coefficient.parse::<i32>().expect(word);
        }
        let count: usize = head[count_at].parse().expect("a count");
        assert_eq!(count, report.rows[0].terms.coefficients.len());
        assert_eq!(head[count_at - 2], eval::eval(&board).to_string());
        // no piece worth a phase weight, so the endgame end
        assert_eq!(head[count_at - 1], "0");
        assert_eq!(head[..count_at - 2].join(" "), spaced.id);
    }

    /// Which slots are added outside the taper's divide, pinned as arithmetic
    /// on slot numbers: a term slot sorted into the material block still
    /// reconstructs every row while its weight is zero, so the identity
    /// cannot be relied on to see it.
    #[test]
    fn the_material_values_are_the_only_weights_outside_the_divide() {
        for slot in 0..MATERIAL_SLOT {
            assert!(
                !is_material(slot),
                "the table slot {} is not material",
                slot
            );
        }
        for slot in MATERIAL_SLOT..TERM_SLOT {
            assert!(is_material(slot), "the material slot {} is", slot);
        }
        for slot in TERM_SLOT..SLOTS {
            assert!(
                !is_material(slot),
                "the leaf term slot {} is inside the divide",
                slot
            );
        }
        // and the blocks after the material are each term's width twice over,
        // one after the other
        let mut slot = TERM_SLOT;
        for named in eval::TERMS {
            assert_eq!(term(named.name), (slot, named.width), "{}", named.name);
            slot += 2 * named.width;
        }
        assert_eq!(slot, SLOTS);
    }

    /// Every shelter count writes both ends of the taper too, asserted
    /// against a hand count for the mobility test's reason: the identity
    /// reads the fourteen slots through one sum.
    ///
    /// White's king on g1 has f2 and h2 one rank ahead and g3 two, and its
    /// three files all hold a pawn of its own, while g2, then f3 and h3, then
    /// f4, g4 and h4 come the other way. Black's king on b8 has nothing in
    /// front of it and no white pawn within three ranks, the a file holds a
    /// white pawn and no black one, and the b and c files hold neither. The
    /// queen and the two rooks hold the phase off the middle of the taper,
    /// so that a coefficient written to the wrong end of it shows.
    #[test]
    fn every_shelter_count_writes_both_ends_of_the_taper() {
        let fen = "1k6/8/8/8/P4ppp/5pPp/5PpP/R2Q2KR w - - 0 1";
        let board = Board::from_fen(fen).unwrap();
        let terms = Terms::of(&board);
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
        for (index, count, why) in [
            // f2 and h2, and black has nothing on the rank in front of b8
            (0, 2, "the pawns one rank ahead"),
            // g3, against nothing on a6, b6 or c6
            (1, 1, "the pawns two ranks ahead"),
            // the b and c files hold no pawn at all, and none of white's
            // three is bare
            (2, -2, "the open files"),
            // the a file holds a white pawn and no black one
            (3, -1, "the half open files"),
            // g2, and no white pawn within three ranks of b8
            (4, 1, "the storm one rank ahead"),
            // f3 and h3
            (5, 2, "the storm two ranks ahead"),
            // f4, g4 and h4
            (6, 3, "the storm three ranks ahead"),
        ] {
            let (start, width) = term("shelter");
            assert_eq!(
                coefficient(start + index),
                count * terms.phase,
                "{} midgame",
                why
            );
            assert_eq!(
                coefficient(start + width + index),
                count * (TOTAL_PHASE - terms.phase),
                "{} endgame",
                why
            );
        }
    }

    /// Each of the eight pawn counts writes its own coefficient at both ends
    /// of the taper, asserted one at a time against a hand count: the
    /// identity folds the whole row against the whole vector, so a passed
    /// count on the wrong rank reconstructs whenever two wrong slots cancel,
    /// and two buckets of a rank table are the likeliest pair to.
    ///
    /// White has a7, a3, b3, b2, d5 and d4, and black has f7, g5, e3 and h3,
    /// which the eval module works through square by square beside its own
    /// test of the fold. The rook and the two queens hold the phase off the
    /// middle of the taper, so a coefficient written to the wrong end of it
    /// shows.
    #[test]
    fn every_pawn_count_writes_both_ends_of_the_taper() {
        let fen = "3k4/P4p2/8/3P2p1/3P4/PP2p2p/1P6/QQ4KR w - - 0 1";
        let board = Board::from_fen(fen).unwrap();
        let terms = Terms::of(&board);
        assert_eq!(terms.phase, 10);
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
        for (index, count, why) in [
            // black's f7, against nothing of white's on its second
            (0, -1, "the passers on the second"),
            // white's b3, against nothing of black's
            (1, 1, "the passers on the third"),
            // black's g5
            (2, -1, "the passers on the fourth"),
            // white's d5
            (3, 1, "the passers on the fifth"),
            // black's e3 and h3
            (4, -2, "the passers on the sixth"),
            // white's a7
            (5, 1, "the passers on the seventh"),
            // white's d5 and d4, and no black pawn stands alone
            (6, 2, "the isolated pawns"),
            // white's a7, b3 and d5 each have one behind them
            (7, 3, "the doubled pawns"),
        ] {
            let (start, width) = term("pawn_structure");
            assert_eq!(
                coefficient(start + index),
                count * terms.phase,
                "{} midgame",
                why
            );
            assert_eq!(
                coefficient(start + width + index),
                count * (TOTAL_PHASE - terms.phase),
                "{} endgame",
                why
            );
        }
    }

    /// A position and its reflection with the colours swapped state the same
    /// pawn row, so the eight counts are signed and slotted the same way for
    /// both sides.
    #[test]
    fn a_mirrored_position_states_the_same_pawn_row() {
        let white = Board::from_fen("4k3/P4p2/8/3P2p1/3P4/PP2p2p/1P6/4K3 w - - 0 1").unwrap();
        let black = Board::from_fen("4k3/1p6/pp2P2P/3p4/3p2P1/8/p4P2/4K3 b - - 0 1").unwrap();
        let terms = Terms::of(&white);
        assert!(
            terms
                .coefficients
                .iter()
                .any(|(slot, _)| usize::from(*slot) >= term("pawn_structure").0),
            "no pawn structure coefficient here, so this test says nothing about one"
        );
        assert_eq!(terms, Terms::of(&black));
        assert_eq!(eval::eval(&white), eval::eval(&black));
    }

    /// A position and its reflection with the colours swapped state the same
    /// shelter row, so the seven counts are signed and slotted the same way
    /// for both sides.
    #[test]
    fn a_mirrored_position_states_the_same_shelter_row() {
        let white = Board::from_fen("4k3/pp6/8/8/8/8/3PPP2/4K3 w - - 0 1").unwrap();
        let black = Board::from_fen("4k3/3ppp2/8/8/8/8/PP6/4K3 b - - 0 1").unwrap();
        let terms = Terms::of(&white);
        assert!(
            terms
                .coefficients
                .iter()
                .any(|(slot, _)| usize::from(*slot) >= term("shelter").0),
            "no shelter coefficient here, so this test says nothing about one"
        );
        assert_eq!(terms, Terms::of(&black));
        assert_eq!(eval::eval(&white), eval::eval(&black));
    }
}
