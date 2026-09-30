// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The evaluation: what a position scores, and every number that opinion is
//! built from. The board hosts an [`Accumulator`] and tells it about each
//! piece placed, removed and moved; the search asks [`eval`] for the score. A
//! term cheap enough to keep incrementally belongs in the accumulator; one
//! computed at the leaf belongs in a module of its own beside this one.
//!
//! This file holds what the terms share, including [`TERMS`], the list the
//! tuner reads the leaf terms through. Each leaf term owns its counts, its
//! weights, and its memo's key and table width where it has one. Mobility and
//! the king attack zone are weighed here, in one shared walk.

mod cache;
pub(crate) mod factors;
mod king_attack;
mod mobility;
mod pawn_structure;
mod shelter;

use cache::Cache;

use crate::board::{Board, king_attacks, knight_attacks, pawn_attacks, pop_lsb};
use crate::magic::MAGIC;
use crate::misc::{Color, Piece, Score};
use crate::psqt::{PieceSquareTables, eg_value, mg_value};

static PIECE_SQUARE_TABLES: PieceSquareTables = PieceSquareTables::TABLES;

/// What each piece leaves on the board, in `Piece` order, on the scale the
/// two halves of a tapered score are interpolated on: a queen four, a rook
/// two and a minor one, so the opening's pieces come to `TOTAL_PHASE`. Pawns
/// count for nothing because an ending is an ending whether or not there are
/// pawns in it.
static PHASE_WEIGHTS: [i32; 6] = [0, 1, 1, 2, 4, 0];
/// What the opening's pieces add up to under `PHASE_WEIGHTS`.
pub(crate) const TOTAL_PHASE: i32 = 24;

/// A table rather than a match: the match compiled to a jump table, and most
/// of the search's indirect mispredicts were this dispatch.
const MATERIAL: [u32; 6] = [100, 310, 320, 500, 900, 10000];

/// Everything a make reads for one piece of one colour on one square, so one
/// index finds all of it. Indexed as `Zobrist` and `PieceSquareTables` index
/// their rows; `psqt` is signed from white's side. `Accumulator::recomputed`
/// and the key recompute read the source tables rather than these rows.
#[derive(Debug, Clone, Copy)]
#[repr(C, align(32))]
pub(crate) struct Row {
    pub(crate) lanes: [[i16; factors::RANK]; 2],
    pub(crate) diagonal: [[i32; factors::LIVE]; 2],
    pub(crate) psqt: i32,
    pub(crate) key: u64,
}

pub(crate) static ROWS: [Row; 768] = rows();

const fn rows() -> [Row; 768] {
    let empty = Row {
        lanes: [[0; factors::RANK]; 2],
        diagonal: [[0; factors::LIVE]; 2],
        psqt: 0,
        key: 0,
    };
    let mut out = [empty; 768];
    let colors = [Color::Black, Color::White];
    let pieces = [
        Piece::Pawn,
        Piece::Knight,
        Piece::Bishop,
        Piece::Rook,
        Piece::Queen,
        Piece::King,
    ];
    let mut table = 0;
    while table < 12 {
        let color = if table < 6 {
            Color::White
        } else {
            Color::Black
        };
        let piece = pieces[table % 6];
        let mut square = 0;
        while square < 64 {
            let row = &mut out[table * 64 + square];
            let mut at = 0;
            while at < 2 {
                let feature = factors::feature(colors[at], square as u8, piece, color);
                row.lanes[at] = factors::FACTORS[feature];
                row.diagonal[at] = factors::DIAGONAL[feature];
                at += 1;
            }
            let value = PieceSquareTables::TABLES.value_at(table, square);
            row.psqt = match color {
                Color::White => value,
                Color::Black => -value,
            };
            row.key = crate::zobrist::Zobrist::TABLE.piece_key_at(table, square);
            square += 1;
        }
        table += 1;
    }
    out
}

/// The row of a piece on a square.
#[inline(always)]
pub(crate) fn row(index: u8, piece: Piece, color: Color) -> &'static Row {
    let table = match color {
        Color::White => piece as usize,
        Color::Black => piece as usize + 6,
    };
    &ROWS[table * 64 + (index & 63) as usize]
}

/// The material weight of one piece, for the board's own seeding walk.
pub(crate) fn material(piece: Piece) -> u32 {
    MATERIAL[piece as usize]
}

/// What one piece leaves on the board, on the scale the taper is read at, for
/// the tuner's walk.
pub(crate) fn phase_weight(piece: Piece) -> i32 {
    PHASE_WEIGHTS[piece as usize]
}

/// One leaf term, as everything outside its own file sees it: what the tuner
/// needs to lay a term out, price it and read its coefficients off a
/// position. The evaluation does not go through here: [`sum`] names the walk
/// and the folds directly, so the hot path costs no indirect call.
pub(crate) struct Term {
    /// What the layout line calls the term, which is the name
    /// `scripts/tune.py` keys its bounds and its holds by.
    pub(crate) name: &'static str,
    /// How many counts the term is measured in, per side and per half of the
    /// taper. It occupies twice this in the weight vector, the midgame half
    /// first.
    pub(crate) width: usize,
    /// The packed weight pair of one count, read out of the live array rather
    /// than a copy of it.
    pub(crate) weight: fn(usize) -> i32,
    /// One side's counts, written into the first `width` entries of the
    /// slice.
    pub(crate) counts: fn(&Board, Color, &mut [i32]),
}

/// The leaf terms, in the order the weight vector holds them.
///
/// A term is appended, never inserted, so adding one moves no slot a fit has
/// been written against. `tune::SLOTS`, `Terms::of` and the run's layout line
/// all follow from this one list.
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

/// White's counts less black's, count by count, against `weights`, as a
/// packed pair on the scale the piece square pair is on. The shelter and the
/// pawn structure are read through it, and the tests read mobility and the
/// king attack zone through it to check the walk.
#[inline]
fn weigh<const N: usize>(weights: &[i32; N], white: [i32; N], black: [i32; N]) -> i32 {
    let mut packed = 0;
    for ((weight, white), black) in weights.iter().zip(white).zip(black) {
        packed += weight * (white - black);
    }
    packed
}

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
/// carrying, so the cached evaluation and the uncached one are one [`sum`]
/// rather than two kept saying the same thing. A term that learns to remember
/// itself adds a method here and an implementation in each of the two below.
trait Memo {
    fn shelter(&mut self, board: &Board) -> i32;
    fn pawn_structure(&mut self, board: &Board) -> i32;
    /// Whether each table holds the position, or none for no tables.
    #[cfg(feature = "trace")]
    fn hits(&self, board: &Board) -> Option<(bool, bool)>;
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

    #[cfg(feature = "trace")]
    fn hits(&self, _: &Board) -> Option<(bool, bool)> {
        None
    }
}

/// What a searcher carries so that the two remembered terms are not computed
/// again at every leaf. One value, so a term that learns to remember itself
/// is a field here rather than a parameter everywhere a score is asked for.
///
/// Two tables rather than one wider entry under the shelter's key: one probe
/// for both would recompute the pawn structure on every king move, the half
/// of the shelter's key that term does not need. Measured on the fitted build.
#[derive(Default)]
pub(crate) struct Caches {
    shelter: ShelterCache,
    pawns: PawnCache,
}

/// The shelter's table, as wide as that term measured it wants.
type ShelterCache = Cache<{ shelter::CACHE_BITS }>;
/// The pawn structure's, on its own key and its own measurement.
type PawnCache = Cache<{ pawn_structure::CACHE_BITS }>;

impl Memo for Caches {
    #[inline]
    fn shelter(&mut self, board: &Board) -> i32 {
        self.shelter
            .get(shelter::key(board), || shelter::fold(board))
    }

    #[inline]
    fn pawn_structure(&mut self, board: &Board) -> i32 {
        self.pawns
            .get(board.pawn_key, || pawn_structure::fold(board))
    }

    #[cfg(feature = "trace")]
    fn hits(&self, board: &Board) -> Option<(bool, bool)> {
        Some((
            self.shelter.stored(shelter::key(board)).is_some(),
            self.pawns.stored(board.pawn_key).is_some(),
        ))
    }
}

/// The score of the position from the side to move's point of view, with the
/// two remembered terms taken from `memo`.
///
/// The leaf terms are packed pairs on the accumulator's scale, summed before
/// the one divide in `Accumulator::score`. Mobility and the king attack zone
/// come off one walk over each side's pieces, [`attack_score`].
///
/// The king attack zone is behind [`king_attack::SCORED`]. llvm does not take
/// the walk out for a zero weight, so at a constant false the walk neither
/// counts nor weighs the ring.
///
/// Material that cannot mate reads zero before any of it, which is why
/// `tune::run` turns such a position away rather than fitting it. The check
/// sits here rather than at the node because the model gate, the tuner's
/// walk and the instruments all read this function.
#[inline]
#[cfg_attr(feature = "trace", track_caller)]
fn sum(board: &Board, memo: &mut impl Memo) -> Score {
    #[cfg(feature = "trace")]
    let hits = memo.hits(board);
    if board.drawn_by_material() {
        #[cfg(feature = "trace")]
        traced(std::panic::Location::caller(), board, 0, hits, false);
        return 0;
    }
    let walk =
        |color| attack_score::<{ mobility::SCORED_KINDS }, { king_attack::SCORED }>(board, color);
    let leaf =
        walk(Color::White) - walk(Color::Black) + memo.shelter(board) + memo.pawn_structure(board);
    let score = board.eval.score(board.active_color, leaf);
    #[cfg(feature = "trace")]
    traced(std::panic::Location::caller(), board, score, hits, true);
    score
}

/// The evaluation just made, handed to the trace mode. The four leaf terms
/// are folded again rather than read off the sum, which adds them as it
/// goes; the tables are exact, so the shelter and the pawn structure fold
/// to what the tables answered, and the recorded score is the sum's own.
#[cfg(feature = "trace")]
fn traced(
    at: &'static std::panic::Location<'static>,
    board: &Board,
    score: Score,
    hits: Option<(bool, bool)>,
    walked: bool,
) {
    let accumulator = &board.eval;
    // counted again by each term's own walk, which the shared walk is held
    // to; the sum no longer keeps the counts
    // muted, so their probes are not recorded as the search's
    let [white_scope, black_scope, white_ring, black_ring] = if walked {
        crate::trace::muted(|| {
            [
                mobility::counts_of::<{ mobility::SCORED_KINDS }>(board, Color::White),
                mobility::counts_of::<{ mobility::SCORED_KINDS }>(board, Color::Black),
                king_attack::counts_of(board, Color::White),
                king_attack::counts_of(board, Color::Black),
            ]
        })
    } else {
        Default::default()
    };
    crate::trace::evaluated(
        at,
        board,
        &crate::trace::Evaluation {
            score,
            drawn: !walked,
            hits,
            phase: accumulator.phase,
            psqt: accumulator.psqt,
            material: accumulator.material[Color::White as usize] as i32
                - accumulator.material[Color::Black as usize] as i32,
            machine: accumulator.machine.score(),
            mobility: if walked {
                weigh(&MOBILITY_WEIGHTS, white_scope, black_scope)
            } else {
                0
            },
            king_attack: if walked && king_attack::SCORED {
                weigh(&KING_ATTACK_WEIGHTS, white_ring, black_ring)
            } else {
                0
            },
            shelter: if walked { shelter::fold(board) } else { 0 },
            pawn_structure: if walked {
                pawn_structure::fold(board)
            } else {
                0
            },
            scope: [white_scope, black_scope],
            ring: [white_ring, black_ring],
        },
    );
}

/// The two terms' weights as packed pairs, for the trace mode's fold.
#[cfg(feature = "trace")]
const MOBILITY_WEIGHTS: [i32; mobility::COUNTS] = [
    mobility::weight(0),
    mobility::weight(1),
    mobility::weight(2),
    mobility::weight(3),
];
#[cfg(feature = "trace")]
const KING_ATTACK_WEIGHTS: [i32; king_attack::COUNTS] = [
    king_attack::weight(0),
    king_attack::weight(1),
    king_attack::weight(2),
    king_attack::weight(3),
];

/// One side's mobility and king attack zone as one packed pair, from one walk
/// over its knights, bishops, rooks and queens. Each piece's two counts are
/// weighed as they are read, so the walk carries one sum rather than a
/// counter per kind and term, sixteen with both sides inlined into [`sum`],
/// which spilled.
///
/// `KINDS` says which kinds mobility counts, as [`mobility::counted`] reads
/// it, and `RING` whether the ring is counted at all. A count not asked for
/// is not compiled and adds nothing.
///
/// A second statement of the two terms, on purpose. Each term keeps its own
/// `counts_of`, which the tuner reads, and
/// `the_shared_walk_counts_what_each_term_counts_alone` holds this walk to
/// their counts weighed.
///
/// Inlined by force, for the reason `mobility::counts_of` gives.
#[inline(always)]
fn attack_score<const KINDS: u8, const RING: bool>(board: &Board, color: Color) -> i32 {
    let occupied = board.occupied();
    let (ours, theirs) = board.sides(color);
    let scope = !(ours | pawn_attacks(board.pawns() & theirs, !color));
    let ring = if RING {
        king_attacks(board.king_index(!color))
    } else {
        0
    };
    let magic = &MAGIC;
    let mut packed = 0;
    let mut read = |index: usize, attacks: u64| {
        if mobility::counted(KINDS, index) {
            packed += mobility::weight(index) * (attacks & scope).count_ones() as i32;
        }
        if RING {
            packed += king_attack::weight(index) * (attacks & ring).count_ones() as i32;
        }
    };
    let walked = |index: usize| mobility::counted(KINDS, index) || RING;
    if walked(0) {
        let mut knights = board.knights() & ours;
        while knights != 0 {
            let from = pop_lsb(&mut knights);
            read(0, knight_attacks(from));
            #[cfg(feature = "trace")]
            walked_one::<KINDS, RING>(color, 0, from, knight_attacks(from), scope, ring);
        }
    }
    if walked(1) {
        let mut bishops = board.bishops() & ours;
        while bishops != 0 {
            let from = pop_lsb(&mut bishops);
            let attacks = magic.get_diagonal_move(from, occupied);
            read(1, attacks);
            #[cfg(feature = "trace")]
            walked_one::<KINDS, RING>(color, 1, from, attacks, scope, ring);
        }
    }
    if walked(2) {
        let mut rooks = board.rooks() & ours;
        while rooks != 0 {
            let from = pop_lsb(&mut rooks);
            let attacks = magic.get_straight_move(from, occupied);
            read(2, attacks);
            #[cfg(feature = "trace")]
            walked_one::<KINDS, RING>(color, 2, from, attacks, scope, ring);
        }
    }
    if walked(3) {
        let mut queens = board.queens() & ours;
        while queens != 0 {
            let from = pop_lsb(&mut queens);
            let attacks =
                magic.get_straight_move(from, occupied) | magic.get_diagonal_move(from, occupied);
            read(3, attacks);
            #[cfg(feature = "trace")]
            walked_one::<KINDS, RING>(color, 3, from, attacks, scope, ring);
        }
    }
    packed
}

/// One piece of the walk, handed to the trace mode with the squares each
/// term counted of its attack set.
#[cfg(feature = "trace")]
fn walked_one<const KINDS: u8, const RING: bool>(
    color: Color,
    index: usize,
    from: u8,
    attacks: u64,
    scope: u64,
    ring: u64,
) {
    let counted = |on: bool, mask: u64| {
        if on {
            (attacks & mask).count_ones() as u8
        } else {
            u8::MAX
        }
    };
    crate::trace::walked(
        color,
        index,
        from,
        attacks,
        counted(mobility::counted(KINDS, index), scope),
        counted(RING, ring),
    );
}

/// The score with the shelter and the pawn structure computed every time, for
/// the tuner's walk, the instruments and the places in the search that want a
/// score of the position alone. None is hot enough for the difference between
/// the two doors to matter.
#[inline]
#[cfg_attr(feature = "trace", track_caller)]
pub(crate) fn eval(board: &Board) -> Score {
    sum(board, &mut NoMemo)
}

/// The same score with those two terms taken from the searcher's caches.
///
/// Equal to [`eval`] for every position, which
/// `the_cache_answers_what_the_full_evaluation_does` holds the two to, so the
/// node counts do not move when the search calls this instead.
#[inline]
#[cfg_attr(feature = "trace", track_caller)]
pub(crate) fn eval_cached(board: &Board, caches: &mut Caches) -> Score {
    sum(board, caches)
}

/// The evaluation's incremental state, hosted by the board. The board's make
/// tells it about every piece placed, removed and relocated, and its unmake
/// copies back the state saved before the move.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct Accumulator {
    /// Each side's material, indexed by `Color`'s discriminant: an index is a
    /// load where a match on the colour was a branch.
    material: [u32; 2],
    /// Piece square score, as the packed pair of midgame and endgame halves
    /// the tables hold (`psqt::pack`), so carrying both phases costs one add.
    psqt: i32,
    /// What is left on the board, on the scale `PHASE_WEIGHTS` measures.
    /// Accumulated rather than counted off the piece boards at every leaf:
    /// four popcounts there measured dearer than one add per piece touched
    /// here.
    phase: i32,
    /// The pair term's sums, which take no room when it is off.
    machine: factors::Machine,
}

impl Accumulator {
    /// A board with nothing on it scores nothing.
    pub(crate) const EMPTY: Self = Self {
        material: [0; 2],
        psqt: 0,
        phase: 0,
        machine: factors::Machine::EMPTY,
    };

    /// Count a piece on to or off of a square, from its row. The row's pair
    /// is negated whole for black, and negating the sum negates both halves.
    #[inline(always)]
    pub(crate) fn count<const SET: bool>(&mut self, row: &Row, piece: Piece, color: Color) {
        let phase = PHASE_WEIGHTS[piece as usize];
        let value = MATERIAL[piece as usize];
        if SET {
            self.psqt += row.psqt;
            self.phase += phase;
            self.material[color as usize] += value;
        } else {
            self.psqt -= row.psqt;
            self.phase -= phase;
            self.material[color as usize] -= value;
        }
        self.machine.count::<SET>(row);
    }

    /// A piece moving between two squares, from the two squares' rows:
    /// `count` off one and on to the other, with the material and phase
    /// updates, which cancel, left out. The pair is added and subtracted whole
    /// either way, so a borrow between the two halves cancels here as it does
    /// there.
    #[inline(always)]
    pub(crate) fn relocate(&mut self, left: &Row, arrived: &Row) {
        self.psqt += arrived.psqt - left.psqt;
        self.machine.relocate(left, arrived);
    }

    /// The accumulator the position deserves, computed from the board, for
    /// `Board::debug_assert_state_in_step`. A second implementation on
    /// purpose: code shared with `count` would be wrong on both sides at once
    /// and the check would still pass.
    pub(crate) fn recomputed(board: &Board) -> Self {
        let mut recomputed = Self::EMPTY;
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
        recomputed.machine = factors::Machine::of(pieces_of(board));
        recomputed
    }

    /// Material seeded from a recount, which is how `from_fen` fills a parsed
    /// board in; the state check then compares the seeding against an
    /// implementation that did not do it.
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
    /// The piece square half is read at the phase the position is in, so a
    /// king walks out as the pieces come off rather than on the move that
    /// takes the last one. Material is not tapered: an endgame piece value is
    /// a constant added to that piece's endgame table, and the tables are the
    /// place to say it.
    ///
    /// `leaf` is the leaf terms summed, as a packed pair on the same scale.
    /// It joins the piece square pair before the interpolation, so the two
    /// share one divide. A second divide would answer a centipawn away
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
        // the pair term is not tapered, so it joins material outside the
        // divide
        let eval = (self.material[Color::White as usize] as i32
            - self.material[Color::Black as usize] as i32
            + scaled
            + self.machine.score()) as Score;
        match side {
            Color::White => eval,
            Color::Black => -eval,
        }
    }
}

/// Every piece on the board as the square, the piece and its colour.
fn pieces_of(board: &Board) -> impl Iterator<Item = (u8, Piece, Color)> + '_ {
    let mut occupied = board.occupied();
    std::iter::from_fn(move || {
        while occupied != 0 {
            let index = occupied.trailing_zeros() as u8;
            occupied &= occupied - 1;
            if let Some((piece, color)) = board.get_piece_and_color_index(index) {
                return Some((index, piece, color));
            }
        }
        None
    })
}

#[cfg(test)]
mod evaluate {
    use super::{
        Board, Caches, Memo, PawnCache, ShelterCache, TERMS, TOTAL_PHASE, eval, eval_cached,
        factors, king_attack, mobility, pawn_structure, pieces_of, shelter,
    };
    use crate::board::fens;
    use crate::misc::{Color, File, coordinate_to_index};
    use crate::psqt::{eg_value, mg_value, pack};
    use pretty_assertions::assert_eq;
    use std::collections::{HashMap, HashSet};

    /// Both the accumulator and its recompute read `PHASE_WEIGHTS`, so the
    /// state check holds them to each other and neither to what the weights
    /// should be. This says what they add up to.
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
    /// entry points, since the search reads the cached one and the tuner, the
    /// model gate and the instruments read the other. The figures in the
    /// comments are what the evaluation returned before the rule; the search
    /// played every one of them as a win.
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

    /// The same signatures with a pawn on the board are outside the rule, so
    /// the test above cannot pass on a rule that answers zero for every
    /// pawnless position, or for every position at all.
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
    /// differently depending on what is left on the board. Each pair below
    /// differs by the king's square and nothing else, material included, and
    /// the pair term is taken out because it is not tapered, so the
    /// difference is what the phase weighs, and what is pinned is the
    /// direction the score moves in as the board empties, which a phase read
    /// the wrong way round would reverse.
    #[test]
    fn a_king_is_worth_more_in_the_middle_the_emptier_the_board() {
        // two king squares, e4 and g1, at three phases. The fitted pair term
        // prefers e4 behind a full board by about eighty centipawns, which
        // hid the opening's sign; it is white relative, as these scores are
        // with white to move
        fn tapered(fen: &str) -> i32 {
            let board = Board::from_fen(fen).unwrap();
            i32::from(eval(&board)) - factors::Machine::of(pieces_of(&board)).score()
        }
        fn centre_over_corner(centre: &str, corner: &str) -> i32 {
            tapered(centre) - tapered(corner)
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
        // and the pawns stand on the same squares in both fens, away from
        // either king's files
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
    /// being tapered beside it. The position's piece square numerator is
    /// negative and does not divide by twenty four evenly, so a second divide
    /// would truncate each part on its own and answer a centipawn away.
    /// Nothing else pins this, and it is what `tune::reconstruct` folds a row
    /// with.
    #[test]
    fn mobility_joins_the_numerator_rather_than_being_tapered_beside_it() {
        let board = Board::from_fen("4k3/8/8/8/8/8/5P2/1N2K3 w - - 0 1").unwrap();
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
        // the pair term is outside the divide, beside material
        assert_eq!(
            accumulator.score(Color::White, mobility),
            (material + inside + accumulator.machine.score()) as crate::misc::Score
        );
    }

    /// The searcher's caches and the keys a walk saw through them; the keys
    /// are what say whether the run evicted anything.
    #[derive(Default)]
    struct Walk {
        caches: Caches,
        shelter_keys: HashSet<u64>,
        pawn_keys: HashSet<u64>,
        counted: HashMap<u64, [[i32; pawn_structure::COUNTS]; 2]>,
        compared: usize,
    }

    impl Walk {
        /// What the pawn key claims, checked in the counts rather than the
        /// score: two positions under one key that disagree on the eight
        /// counts are one position handed the other's score, whatever the
        /// weights make that worth.
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
    /// they land in. The second being the smaller says two keys shared a slot,
    /// so an entry was written over.
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
    /// A key missing something the term reads would hand one position's
    /// shelter to another, and nothing in a search would say so. The run has
    /// to evict as well as write and read, and counting the positions does not
    /// say it happened, since far fewer keys than positions reach the table:
    /// what says it is two keys sharing a slot, which the test asserts.
    ///
    /// `Walk::note` carries the same claim in the counts rather than the
    /// score, because two wrong counts whose weights cancel would pass the
    /// score and fail the note.
    #[test]
    fn the_cache_answers_what_the_full_evaluation_does() {
        let mut walked = Walk::default();
        let mut budget = 4 * ShelterCache::SLOTS.max(PawnCache::SLOTS);
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
            ("shelter", &walked.shelter_keys, ShelterCache::SLOTS),
            ("pawn", &walked.pawn_keys, PawnCache::SLOTS),
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

    /// A king move leaves the pawn key alone and moves the shelter key, which
    /// is the difference between the two caches and why the pawn structure
    /// could be cached in the commit that introduced it.
    ///
    /// Each table is then read under its own term's key. Both hold the
    /// position before the move. After it the pawn table still answers, and
    /// the shelter table has to fold again.
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

        // an empty entry is key zero holding zero, so a pawn key of zero
        // would read as a hit wherever it landed
        assert_ne!(board.pawn_key, 0);
        let mut caches = Caches::default();
        assert_eq!(
            Memo::shelter(&mut caches, &board),
            shelter::fold(&board),
            "the first probe folds"
        );
        assert_eq!(
            Memo::pawn_structure(&mut caches, &board),
            pawn_structure::fold(&board)
        );
        assert_eq!(
            caches.shelter.stored(shelter::key(&board)),
            Some(shelter::fold(&board))
        );
        assert_eq!(
            caches.pawns.stored(board.pawn_key),
            Some(pawn_structure::fold(&board))
        );

        assert_eq!(
            caches.pawns.stored(moved.pawn_key),
            Some(pawn_structure::fold(&moved)),
            "the pawn table answers the position after the king move"
        );
        assert_eq!(
            caches.shelter.stored(shelter::key(&moved)),
            None,
            "and the shelter table does not"
        );
        assert_eq!(Memo::shelter(&mut caches, &moved), shelter::fold(&moved));
        assert_eq!(
            caches.shelter.stored(shelter::key(&moved)),
            Some(shelter::fold(&moved))
        );
    }

    /// Every term the table names carries a width, a weight and a count
    /// helper that agree, and the helper fills every count the width claims,
    /// since a width that disagrees with the helper would move every slot
    /// after it. The buffer starts at `i32::MIN` and every entry the width
    /// names has to have been written over: a helper writing fewer entries
    /// than `width` leaves the rest where they stood, which a slice cannot
    /// catch, where one writing more panics.
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

    /// The table names the four leaf terms the sum adds. The sum is hand
    /// written rather than a walk over the table, so this is the one place
    /// the two lists are held against each other; a priced term in the table
    /// and not the sum would fail the tuner's identity, and this says which
    /// of the two is wrong.
    #[test]
    fn the_table_names_the_terms_the_sum_adds() {
        let names: Vec<&str> = TERMS.iter().map(|term| term.name).collect();
        assert_eq!(
            names,
            ["mobility", "shelter", "pawn_structure", "king_attack"]
        );
        // two queens and a rook against none, a king in each corner and pawns
        // of both colours on six files, so that no one of the four folds to
        // nothing. The queen on a4 bears on d7 and e8 of the black king's
        // ring
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

    /// Holds [`super::attack_score`] to [`mobility::counts_of`] and
    /// [`king_attack::counts_of`] weighed, on one position, for both colours,
    /// with and without each skip. The term tests call this on their hand
    /// count positions.
    pub(super) fn the_shared_walk_agrees(board: &Board, why: &str) {
        fn held<const KINDS: u8, const RING: bool>(board: &Board, color: Color, why: &str) {
            let scope = mobility::counts_of::<{ mobility::ALL_KINDS }>(board, color);
            let ring = king_attack::counts_of(board, color);
            let expected: i32 = (0..mobility::COUNTS)
                .map(|index| {
                    let scoped = if mobility::counted(KINDS, index) {
                        mobility::weight(index) * scope[index]
                    } else {
                        0
                    };
                    let bearing = if RING {
                        king_attack::weight(index) * ring[index]
                    } else {
                        0
                    };
                    scoped + bearing
                })
                .sum();
            assert_eq!(
                super::attack_score::<KINDS, RING>(board, color),
                expected,
                "{} for {:?}, counting kinds {:04b} and the ring {}",
                why,
                color,
                KINDS,
                RING
            );
        }
        for color in [Color::White, Color::Black] {
            held::<{ mobility::ALL_KINDS }, true>(board, color, why);
            held::<{ mobility::SCORED_KINDS }, { king_attack::SCORED }>(board, color, why);
            held::<0b0101, false>(board, color, why);
            held::<0b1010, true>(board, color, why);
            held::<0, false>(board, color, why);
        }
    }

    /// The walk the sum reads mobility and the king attack zone off answers
    /// what the two terms' own counts answer, on every position of the bench,
    /// tactical and strategic suites and the shared fens. The tuner's
    /// identity holds the two together only at the live constants and only
    /// as white less black; this holds each side alone, against each term's
    /// own counts, and under the skips.
    #[test]
    fn the_shared_walk_counts_what_each_term_counts_alone() {
        let mut fens: Vec<String> = fens::CORE.iter().map(|f| f.to_string()).collect();
        fens.extend(crate::bench::positions().into_iter().map(|p| p.fen));
        fens.extend(crate::tactics::positions().into_iter().map(|p| p.fen));
        fens.extend(crate::strategy::positions().into_iter().map(|p| p.fen));
        assert!(fens.len() > 1_500, "{} positions", fens.len());
        for fen in fens {
            let board = Board::from_fen(&fen).unwrap_or_else(|e| panic!("{}: {}", fen, e));
            the_shared_walk_agrees(&board, &fen);
        }
    }
}
