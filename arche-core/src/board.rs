// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use super::bitboard::BitBoard;
use super::misc::{
    CastlePermissions, Color, Coordinate, File, Piece, PromotePiece, coordinate_to_index,
    index_to_coordinate,
};
use super::play::Play;
use crate::eval::{self, Accumulator};
use crate::magic::MAGIC;
use crate::zobrist::Zobrist;
use smallvec::SmallVec;
use std::fmt;
use std::mem::MaybeUninit;

// A list's inline capacity, and the size of the ordering module's key
// buffer, so every list short of a spill sorts on the stack. The value is
// measured and settled: see the roadmap before moving it.
pub(crate) const MOVE_LIST_INLINE: usize = 64;
pub type MoveList = SmallVec<[Play; MOVE_LIST_INLINE]>;

/// The widest a generated list can be. No legal position has been shown to
/// offer more than 218 moves, but `from_fen` bounds neither the number of
/// pieces nor what they are (nine queens is accepted and played from), so
/// the margin is against a parsed position rather than the generator. The
/// buffer is uninitialised and lives in one frame, so the width costs stack
/// and nothing else.
const MAX_GENERATED: usize = 512;

/// See `Board::check_info`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CheckInfo {
    king: u8,
    squares: [u64; 6],
    blockers: u64,
}

/// A move list while it is being generated: a plain array and a length.
///
/// A `SmallVec` push asks whether the list has spilled and whether it is
/// full before it stores anything, and both answers are the same for every
/// one of the million and a half pushes a search makes. Here a push is a
/// store and an increment, and the list is built once at the end.
struct Building {
    moves: [MaybeUninit<Play>; MAX_GENERATED],
    len: usize,
}

impl Building {
    #[inline(always)]
    fn new() -> Self {
        // `Play` has no zero value to memset (`None` for the captured piece
        // is a niche), so an initialiser costs a store per entry, and one
        // here measured thirteen percent slower than the pushing it replaced
        // (49747fa).
        Self {
            moves: [const { MaybeUninit::uninit() }; MAX_GENERATED],
            len: 0,
        }
    }

    #[inline(always)]
    fn push(&mut self, play: Play) {
        self.moves[self.len].write(play);
        self.len += 1;
    }

    #[inline(always)]
    fn finish(&self) -> MoveList {
        let filled = &self.moves[..self.len];
        // SAFETY: `push` writes an entry before it counts it, so the first
        // `len` are all initialised, and `MaybeUninit<Play>` has the layout
        // of `Play`.
        let filled = unsafe { &*(filled as *const [MaybeUninit<Play>] as *const [Play]) };
        MoveList::from_slice(filled)
    }
}

/// Pop the lowest set bit and return its index.
#[inline(always)]
pub(crate) fn pop_lsb(bb: &mut u64) -> u8 {
    let i = bb.trailing_zeros() as u8;
    *bb &= *bb - 1;
    i
}

/// Which probe a made move needs before it stands: whether it could have
/// exposed its own king, and along which kind of line if so.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Exposure {
    None,
    Straight,
    Diagonal,
    Whole,
}

/// One ply of history: what `undo_move` needs that the move itself does not
/// carry.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct PlayState {
    play: Play,

    en_passant: Option<Coordinate>,
    castle: CastlePermissions,
    fifty_move_rule: usize,
    position_key: u64,
    checkers: u64,
}

/// The play a pass records in the history. Nothing plays it back; it only
/// lets a debug build check that the ply being taken back was a pass.
const NULL_PLAY: Play = Play {
    from: 0,
    to: 0,
    capture: None,
    promote: None,
    en_passant: false,
    castle: false,
};

// Plies of history the board records, as a ring. Only the fifty move window
// is ever read back, so this has to cover that plus the depth of a search,
// not the whole game.
const MAX_GAME_SIZE: usize = 1024;

/// Where a ply is recorded. A game past MAX_GAME_SIZE plies, or a position
/// parsed at a move number past it, wraps rather than running off the end.
fn history_index(ply: usize) -> usize {
    ply % MAX_GAME_SIZE
}

/// The value a position key starts from, before any piece or right is folded
/// into it. Arbitrary, it only has to be the same everywhere.
const INITIAL_KEY: u64 = 2_340_980_257_093;
static EMPTY_HISTORY: [Option<PlayState>; MAX_GAME_SIZE] = [None; MAX_GAME_SIZE];

const A1: u8 = 0;
const B1: u8 = 1;
const C1: u8 = 2;
const D1: u8 = 3;
const E1: u8 = 4;
const F1: u8 = 5;
const G1: u8 = 6;
const H1: u8 = 7;

const A8: u8 = 56;
const B8: u8 = 57;
const C8: u8 = 58;
const D8: u8 = 59;
const E8: u8 = 60;
const F8: u8 = 61;
const G8: u8 = 62;
const H8: u8 = 63;

/// The light squares, indexed from a1 at zero: a square is light when its
/// file and rank sum to an odd number. Read by `drawn_by_material` alone.
const LIGHT_SQUARES: u64 = 0x55AA_55AA_55AA_55AA;

pub(crate) static ZOBRIST: Zobrist = Zobrist::TABLE;

/// What each square leaves of the castling rights, as the word
/// `CastlePermissions` holds them in. A right is lost when a king or rook
/// leaves its square or a rook is taken on one, and which right depends on
/// the square alone, so the from and to entries are masked into the rights
/// together.
///
/// The tables differ on e1 and e8 alone: leaving one takes both of that
/// side's rights, landing on one takes neither, because in a position that
/// holds the rights the king stands there and a move taking it would have
/// ended the game. `from_fen` drops a right whose king or rook is elsewhere,
/// so that holds of a parsed position too.
static CASTLE_LEAVING: [u8; 64] = castle_masks(true);
static CASTLE_LANDING: [u8; 64] = castle_masks(false);

const fn castle_masks(leaving: bool) -> [u8; 64] {
    use CastlePermissions as C;
    let mut masks = [C::ALL; 64];
    masks[A1 as usize] = C::ALL & !C::WHITE_QUEEN_SIDE;
    masks[H1 as usize] = C::ALL & !C::WHITE_KING_SIDE;
    masks[A8 as usize] = C::ALL & !C::BLACK_QUEEN_SIDE;
    masks[H8 as usize] = C::ALL & !C::BLACK_KING_SIDE;
    if leaving {
        masks[E1 as usize] = C::ALL & !(C::WHITE_KING_SIDE | C::WHITE_QUEEN_SIDE);
        masks[E8 as usize] = C::ALL & !(C::BLACK_KING_SIDE | C::BLACK_QUEEN_SIDE);
    }
    masks
}

/// Folded into the key a pass records in the history, so the entry matches
/// nothing: `has_repeated` says why a pass must never answer a repetition
/// test. Odd, so it changes every key, and otherwise arbitrary.
const NULL_HISTORY_SALT: u64 = 0x9e37_79b9_7f4a_7c15;

static ATTACK_MASKS: AttackMasks = AttackMasks::new();
// the squares strictly between two aligned squares, empty for a pair that
// shares no line: what blocks a slider on one checking a king on the other
static BETWEEN: [[u64; 64]; 64] = between_masks();

const fn between_masks() -> [[u64; 64]; 64] {
    let mut between = [[0u64; 64]; 64];
    let mut a = 0u8;
    while a < 64 {
        let mut b = 0u8;
        while b < 64 {
            between[a as usize][b as usize] = between_squares(a, b);
            b += 1;
        }
        a += 1;
    }
    between
}

/// One step from `from` towards `to` along the axis they are measured on, and
/// none when they already agree on it.
const fn towards(from: i8, to: i8) -> i8 {
    if to > from {
        1
    } else if to < from {
        -1
    } else {
        0
    }
}

/// Walk from `a` towards `b` collecting what is passed over. Only an aligned
/// pair is ever landed on; any other walk steps off the board first, which
/// gives the empty answer. Built without the magic tables so it can be a
/// `const`; `a_ray_walk_finds_what_the_sliders_do` holds the two together.
const fn between_squares(a: u8, b: u8) -> u64 {
    if a == b {
        return 0;
    }
    let (target_rank, target_file) = ((b / 8) as i8, (b % 8) as i8);
    let (mut rank, mut file) = ((a / 8) as i8, (a % 8) as i8);
    let rank_step = towards(rank, target_rank);
    let file_step = towards(file, target_file);
    let mut mask = 0u64;
    loop {
        rank += rank_step;
        file += file_step;
        if !(rank >= 0 && rank < 8 && file >= 0 && file < 8) {
            return 0;
        }
        if rank == target_rank && file == target_file {
            return mask;
        }
        mask |= 1u64 << ((rank * 8 + file) as u32);
    }
}

// the squares that have to be empty for each castle
const B1_C1_D1: u64 = 1 << B1 | 1 << C1 | 1 << D1;
const F1_G1: u64 = 1 << F1 | 1 << G1;
const B8_C8_D8: u64 = 1 << B8 | 1 << C8 | 1 << D8;
const F8_G8: u64 = 1 << F8 | 1 << G8;

/// One castle: the squares that must be empty, the squares the king may not
/// be attacked on as it goes, and the square it lands on.
type Castle = (u64, [u8; 2], u8);

/// Each colour's two castles, queen's side first, which is the order the
/// generator produces them in and so part of the order the search sees.
const WHITE_CASTLES: [Castle; 2] = [(B1_C1_D1, [C1, D1], C1), (F1_G1, [F1, G1], G1)];
const BLACK_CASTLES: [Castle; 2] = [(B8_C8_D8, [C8, D8], C8), (F8_G8, [F8, G8], G8)];

/// What a piece on each square attacks with nothing in the way.
///
/// The two slider entries are the whole lines through the square, edges and
/// the square itself included: they are only asked whether two squares share
/// a line, to rule a slider out before the blocker-aware probe in `magic`.
struct AttackMasks {
    black_pawns: [u64; 64],
    white_pawns: [u64; 64],
    knights: [u64; 64],
    straight: [u64; 64], // rooks and queens
    diagonal: [u64; 64], // bishops and queens
    kings: [u64; 64],
}

/// The squares one step from `from` in each of `steps`, a step that leaves
/// the board dropped. Steps are a rank and a file, so a step off the side
/// is caught by the file going out of range.
const fn stepped(from: u8, steps: &[(i8, i8)]) -> u64 {
    let rank = (from / 8) as i8;
    let file = (from % 8) as i8;
    let mut mask = 0u64;
    let mut i = 0;
    while i < steps.len() {
        let (rank_step, file_step) = steps[i];
        let (r, f) = (rank + rank_step, file + file_step);
        if r >= 0 && r < 8 && f >= 0 && f < 8 {
            mask |= 1u64 << ((r * 8 + f) as u32);
        }
        i += 1;
    }
    mask
}

/// Every square reached from `from` by repeating each of `steps` until the
/// board runs out, the square itself included.
const fn rayed(from: u8, steps: &[(i8, i8)]) -> u64 {
    let mut mask = 1u64 << from;
    let mut i = 0;
    while i < steps.len() {
        let (rank_step, file_step) = steps[i];
        let mut r = (from / 8) as i8 + rank_step;
        let mut f = (from % 8) as i8 + file_step;
        while r >= 0 && r < 8 && f >= 0 && f < 8 {
            mask |= 1u64 << ((r * 8 + f) as u32);
            r += rank_step;
            f += file_step;
        }
        i += 1;
    }
    mask
}

/// The directions each leaper moves in, as a rank step and a file step. A
/// pawn's are the squares it must stand on to attack the one indexed, the
/// mirror of what it attacks: a white pawn takes upwards, so it stands one
/// rank below.
#[rustfmt::skip]
const KING_STEPS: [(i8, i8); 8] = [
    (1, -1), (1, 0), (1, 1), (0, -1), (0, 1), (-1, -1), (-1, 0), (-1, 1),
];
#[rustfmt::skip]
const KNIGHT_STEPS: [(i8, i8); 8] = [
    (2, -1), (2, 1), (1, -2), (1, 2), (-1, -2), (-1, 2), (-2, -1), (-2, 1),
];
const WHITE_PAWN_STEPS: [(i8, i8); 2] = [(-1, -1), (-1, 1)];
const BLACK_PAWN_STEPS: [(i8, i8); 2] = [(1, -1), (1, 1)];
const STRAIGHT_STEPS: [(i8, i8); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const DIAGONAL_STEPS: [(i8, i8); 4] = [(1, 1), (1, -1), (-1, 1), (-1, -1)];

impl AttackMasks {
    /// Built at compile time, which is why the walks above are in
    /// coordinates rather than through the mailbox.
    const fn new() -> Self {
        let mut masks = AttackMasks {
            black_pawns: [0; 64],
            white_pawns: [0; 64],
            knights: [0; 64],
            straight: [0; 64],
            diagonal: [0; 64],
            kings: [0; 64],
        };
        let mut square = 0u8;
        while square < 64 {
            let i = square as usize;
            masks.kings[i] = stepped(square, &KING_STEPS);
            masks.knights[i] = stepped(square, &KNIGHT_STEPS);
            masks.white_pawns[i] = stepped(square, &WHITE_PAWN_STEPS);
            masks.black_pawns[i] = stepped(square, &BLACK_PAWN_STEPS);
            masks.straight[i] = rayed(square, &STRAIGHT_STEPS);
            masks.diagonal[i] = rayed(square, &DIAGONAL_STEPS);
            square += 1;
        }
        masks
    }
}

/// The squares a knight on `from` attacks, whatever stands on them. Read by
/// the evaluation; the slider entries stay the generator's own.
#[inline]
pub(crate) fn knight_attacks(from: u8) -> u64 {
    ATTACK_MASKS.knights[from as usize]
}

/// The squares a king on `from` attacks, whatever stands on them. The king's
/// own square is not in it.
#[inline]
pub(crate) fn king_attacks(from: u8) -> u64 {
    ATTACK_MASKS.kings[from as usize]
}

/// Every square a side's pawns attack, as one span: a shift rather than a
/// mask a pawn at a time, because the evaluation asks for the whole span at
/// every leaf. The file masks stop the shift carrying a bit around into the
/// next rank.
pub(crate) const fn pawn_attacks(pawns: u64, color: Color) -> u64 {
    const A_FILE: u64 = 0x0101_0101_0101_0101;
    const H_FILE: u64 = 0x8080_8080_8080_8080;
    match color {
        Color::White => ((pawns & !A_FILE) << 7) | ((pawns & !H_FILE) << 9),
        Color::Black => ((pawns & !H_FILE) >> 7) | ((pawns & !A_FILE) >> 9),
    }
}

/// What each piece is worth to `see`, indexed by `Piece`. An ordering
/// oracle, separate from `eval::material` so either can move without
/// dragging the other along. The king's price has to dwarf every exchange
/// the swap can build without overflowing one; the ordering module reads
/// the table's bounds to prove its bands apart.
pub(crate) const SEE_VALUES: [i32; 6] = [100, 300, 300, 500, 900, 10_000];

/// Why `Board::play_by_name` refused a move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unplayable {
    /// No move of that name exists in this position.
    NoSuchMove,
    /// The move exists and would leave the mover's king in check.
    LeavesKingInCheck,
}

/// The reason as a short phrase, for the protocol to put after the move.
impl fmt::Display for Unplayable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Unplayable::NoSuchMove => "no such move here",
            Unplayable::LeavesKingInCheck => "leaves the king in check",
        })
    }
}

/// The whole position with its history, a little over forty kilobytes. The
/// search makes and unmakes moves on the one board and never clones it; the
/// type is not `Copy`, so a copy has to be written as a clone.
///
/// `squares`, `key`, `pawn_key`, `eval` and `checkers` restate the piece
/// boards and are kept in step with them by every move made and unmade.
/// `key` also folds in the side to move, the castle rights and the en
/// passant square. In a debug build `debug_assert_state_in_step` recomputes
/// the first four after every move, and `make_move` checks `checkers`
/// beside it. Outside the crate the position is read through the accessors
/// and moved on through `play_by_name`, which has no counterpart that takes
/// a move back.
#[derive(Debug, PartialEq, Clone, Eq)]
pub struct Board {
    // One board per piece, indexed by `Piece`, rather than a field each: as
    // fields the pick in `move_accumulators` was a six way jump table the
    // branch predictor missed a quarter of the time. Indexing costs no
    // branch, and the accessors below compile to the same loads the fields
    // did.
    pieces: [u64; 6],

    white: u64,
    black: u64,

    // what stands on each square, so asking costs a load rather than a walk
    // down the six boards. Written by `move_accumulators`, the one place a
    // piece is put down or picked up, and read by `get_piece_index`. Not
    // called a mailbox because this crate already calls the sentinel grid in
    // `magic` that.
    squares: [Option<Piece>; 64],

    pub(crate) active_color: Color,
    castle: CastlePermissions,
    en_passant: Option<Coordinate>,
    // the pieces giving check to the side to move, maintained by make_move
    // from the move rather than probed at every node. Holding the checkers
    // rather than the fact of check is what lets the generator drop moves
    // that cannot answer one without playing them
    checkers: u64,

    ply: usize,
    // plies since the root of the search, zeroed by `start_line`: what a
    // mate score's distance is measured in
    pub(crate) line_ply: usize,
    fifty_move_rule: usize,

    // the evaluation's incremental state. The eval module owns what it
    // means; the board only keeps it in step
    pub(crate) eval: Accumulator,

    history: [Option<PlayState>; MAX_GAME_SIZE],
    pub(crate) key: u64,
    /// The zobrist key over both sides' pawns alone: no side to move, castle
    /// rights or en passant square, so two positions with the same pawns
    /// behind different pieces share it. Kept in step by every path that
    /// moves a pawn or takes one off; a promotion takes a pawn out and puts
    /// nothing back.
    pub(crate) pawn_key: u64,
}

/// Nothing calls this: it is here because clippy asks for a `Default`
/// wherever there is a `new` taking no arguments, and the lints are denied.
impl Default for Board {
    fn default() -> Self {
        Board::new()
    }
}

impl Board {
    pub fn new() -> Board {
        Board::from_fen(crate::STARTING_FEN).unwrap()
    }

    pub fn active_color(&self) -> Color {
        self.active_color
    }

    /// The position key: the zobrist hash of the pieces, the side to move,
    /// the castle rights and the en passant square.
    pub fn key(&self) -> u64 {
        self.key
    }

    /// Plies since the root of the current search.
    pub fn line_ply(&self) -> usize {
        self.line_ply
    }

    /// Make this position the root of a search: the line ply starts from
    /// zero here, whatever moves the board was played through to arrive.
    pub(crate) fn start_line(&mut self) {
        self.line_ply = 0;
    }

    /// The move of this name here, or none. The name is the coordinate
    /// notation a `Play` prints as, which is what the protocol sends.
    ///
    /// Read from `generate_moves`, the whole pseudo legal list, so a move
    /// that leaves the king in check is still found. Not from `evasions`,
    /// which would drop a move that ignores a check and so report it as no
    /// move at all.
    pub(crate) fn move_named(&self, name: &str) -> Option<Play> {
        self.generate_moves()
            .iter()
            .find(|m| m.to_string() == name)
            .copied()
    }

    /// Play the move of this name, or say why not. A refused move leaves the
    /// board as it was.
    ///
    /// A name is the only way in from outside the crate, because `make_move`
    /// reads a move's capture, castle and promotion as a description of this
    /// board, and a `Play` carried over from another position would corrupt
    /// it. The move a name finds is always this board's own.
    pub fn play_by_name(&mut self, name: &str) -> Result<(), Unplayable> {
        let play = self.move_named(name).ok_or(Unplayable::NoSuchMove)?;
        // a false from make_move is a move that exposed its own king, and it
        // has already been taken back
        if self.make_move(&play) {
            Ok(())
        } else {
            Err(Unplayable::LeavesKingInCheck)
        }
    }

    /// Whether this move is one `generate_moves` would produce here.
    ///
    /// A table hit is this position's entry, except on a key collision (the
    /// slot in `transposition` gives the odds), when its move belongs to
    /// another position. Ordering passes such a move over because it matches
    /// nothing generated; playing it is worse, since `make_move` reads the
    /// capture, promotion and castling fields as a description of this board
    /// and a foreign move corrupts the position.
    ///
    /// Answering no when the truth is yes is free, the caller generates as
    /// it would have anyway, so castling, en passant and promotion are
    /// refused rather than checked. This is also `make_move`'s precondition
    /// written down: keep it stated over the move and the position alone.
    pub fn is_pseudo_legal(&self, m: &Play) -> bool {
        if m.castle || m.en_passant || m.promote.is_some() {
            return false;
        }

        let color_mask = match self.active_color {
            Color::Black => self.black,
            Color::White => self.white,
        };
        if !color_mask.is_bit_set(m.from) || color_mask.is_bit_set(m.to) {
            return false;
        }
        let Some(piece) = self.get_piece_index(m.from) else {
            return false;
        };
        // make_move clears exactly the piece the move names
        if m.capture != self.get_piece_index(m.to) {
            return false;
        }

        let all_pieces = self.black | self.white;
        let attack_masks = &ATTACK_MASKS;
        let magic = &MAGIC;
        match piece {
            Piece::Knight => attack_masks.knights[m.from as usize].is_bit_set(m.to),
            Piece::King => attack_masks.kings[m.from as usize].is_bit_set(m.to),
            Piece::Rook => magic.get_straight_move(m.from, all_pieces).is_bit_set(m.to),
            Piece::Bishop => magic.get_diagonal_move(m.from, all_pieces).is_bit_set(m.to),
            Piece::Queen => {
                magic.get_straight_move(m.from, all_pieces).is_bit_set(m.to)
                    || magic.get_diagonal_move(m.from, all_pieces).is_bit_set(m.to)
            }
            Piece::Pawn => {
                let (rank, _) = index_to_coordinate(m.from);
                // a pawn one step from the far rank only ever promotes
                if match self.active_color {
                    Color::White => rank == 7,
                    Color::Black => rank == 2,
                } {
                    return false;
                }
                if m.capture.is_some() {
                    return match self.active_color {
                        Color::White => attack_masks.black_pawns[m.from as usize].is_bit_set(m.to),
                        Color::Black => attack_masks.white_pawns[m.from as usize].is_bit_set(m.to),
                    };
                }
                let one = match self.active_color {
                    Color::White => m.from as isize + 8,
                    Color::Black => m.from as isize - 8,
                };
                if !(0..64).contains(&one) || all_pieces.is_bit_set(one as u8) {
                    return false;
                }
                if m.to as isize == one {
                    return true;
                }
                let from_start = match self.active_color {
                    Color::White => rank == 2,
                    Color::Black => rank == 7,
                };
                let two = match self.active_color {
                    Color::White => one + 8,
                    Color::Black => one - 8,
                };
                from_start && m.to as isize == two && !all_pieces.is_bit_set(two as u8)
            }
        }
    }

    /// The subset of generate_moves that changes material, the captures and
    /// the promoting pushes, in the same order.
    pub fn generate_captures(&self) -> MoveList {
        self.generate::<true, false>()
    }

    /// Every pseudo legal move, less those that cannot answer a check when
    /// there is one; out of check it is `generate_moves`. Most of the full
    /// list in check would only be refused by `make_move`, so leaving it
    /// ungenerated spares the push, the sort and the make.
    ///
    /// The list is the one `retain_evasions` returns, move for move, which
    /// `the_masked_generator_keeps_what_the_filter_kept` holds it to.
    #[inline]
    pub fn evasions(&self) -> MoveList {
        if self.in_check() {
            self.generate::<false, true>()
        } else {
            self.generate_moves()
        }
    }

    pub fn generate_moves(&self) -> MoveList {
        self.generate::<false, false>()
    }

    /// The squares a piece other than the king must land on to answer the
    /// check: the sole checker or its line, and nothing under a double check.
    /// En passant is left unmasked, since the captured pawn does not stand on
    /// the to square and the mask would misread it.
    fn evasion_targets(&self) -> u64 {
        debug_assert!(self.checkers != 0, "asked of a position not in check");
        if self.checkers.count_ones() > 1 {
            return 0;
        }
        let checker = self.checkers.trailing_zeros() as usize;
        let king = self.king_index(self.active_color) as usize;
        self.checkers | BETWEEN[king][checker]
    }

    /// The one generator behind the three lists. The const parameters are
    /// settled at compile time, so each wrapper monomorphises with nothing
    /// tested per move.
    fn generate<const CAPTURES_ONLY: bool, const EVASIONS: bool>(&self) -> MoveList {
        let mut moves = Building::new();
        let (color_mask, capture_mask) = self.sides(self.active_color);
        let all_pieces = self.black | self.white;
        let attack_masks = &ATTACK_MASKS;
        let magic = &MAGIC;
        // the king takes this filter as it stands: it answers a check by
        // leaving, not by landing on the checker's line
        let king_filter = if CAPTURES_ONLY {
            capture_mask
        } else {
            !color_mask
        };
        let evasion_filter = if EVASIONS { self.evasion_targets() } else { !0 };
        let target_filter = king_filter & evasion_filter;
        let capture_at = |to: u8| {
            if CAPTURES_ONLY {
                self.get_piece_index(to)
            } else {
                self.capture_on(to, capture_mask)
            }
        };
        let mut knights = self.knights() & color_mask;
        while knights != 0 {
            let from = pop_lsb(&mut knights);
            let mut targets = attack_masks.knights[from as usize] & target_filter;
            while targets != 0 {
                let to = pop_lsb(&mut targets);
                moves.push(Play::new(from, to, capture_at(to), None, false, false));
            }
        }
        let mut queens_and_rooks = (self.queens() | self.rooks()) & color_mask;
        while queens_and_rooks != 0 {
            let from = pop_lsb(&mut queens_and_rooks);
            let mut targets = magic.get_straight_move(from, all_pieces) & target_filter;
            while targets != 0 {
                let to = pop_lsb(&mut targets);
                moves.push(Play::new(from, to, capture_at(to), None, false, false));
            }
        }
        let mut queens_and_bishops = (self.queens() | self.bishops()) & color_mask;
        while queens_and_bishops != 0 {
            let from = pop_lsb(&mut queens_and_bishops);
            let mut targets = magic.get_diagonal_move(from, all_pieces) & target_filter;
            while targets != 0 {
                let to = pop_lsb(&mut targets);
                moves.push(Play::new(from, to, capture_at(to), None, false, false));
            }
        }
        let mut kings = self.kings() & color_mask;
        while kings != 0 {
            let from = pop_lsb(&mut kings);
            let mut targets = attack_masks.kings[from as usize] & king_filter;
            while targets != 0 {
                let to = pop_lsb(&mut targets);
                moves.push(Play::new(from, to, capture_at(to), None, false, false));
            }
            if CAPTURES_ONLY {
                continue;
            }
            // castling: the right is held, the king is not in check, the
            // squares between are empty, and the king does not pass through
            // an attacked square
            let (king_square, opponent, held, castles) = match self.active_color {
                Color::White => (
                    E1,
                    Color::Black,
                    [
                        self.castle.holds(CastlePermissions::WHITE_QUEEN_SIDE),
                        self.castle.holds(CastlePermissions::WHITE_KING_SIDE),
                    ],
                    &WHITE_CASTLES,
                ),
                Color::Black => (
                    E8,
                    Color::White,
                    [
                        self.castle.holds(CastlePermissions::BLACK_QUEEN_SIDE),
                        self.castle.holds(CastlePermissions::BLACK_KING_SIDE),
                    ],
                    &BLACK_CASTLES,
                ),
            };
            if (held[0] || held[1]) && !self.square_attacked(king_square, opponent) {
                for (i, &(empty, passes, king_to)) in castles.iter().enumerate() {
                    if held[i]
                        && (empty & all_pieces) == 0
                        && !passes.iter().any(|s| self.square_attacked(*s, opponent))
                    {
                        moves.push(Play::new(from, king_to, None, None, false, true));
                    }
                }
            }
        }
        let mut pawns = self.pawns() & color_mask;
        while pawns != 0 {
            let from = pop_lsb(&mut pawns);
            let (rank, _) = index_to_coordinate(from);
            let can_promote = match self.active_color {
                Color::White => rank == 7,
                Color::Black => rank == 2,
            };
            let pmoves: u64 = match self.active_color {
                Color::White => attack_masks.black_pawns[from as usize] & capture_mask,
                Color::Black => attack_masks.white_pawns[from as usize] & capture_mask,
            };
            let mut targets = pmoves & evasion_filter;
            while targets != 0 {
                let to = pop_lsb(&mut targets);
                let capture = self.get_piece_index(to);
                if can_promote {
                    for p in PromotePiece::VARIANTS {
                        moves.push(Play::new(from, to, capture, Some(p), false, false));
                    }
                } else {
                    moves.push(Play::new(from, to, capture, None, false, false));
                }
            }
            // the captures list keeps the promoting pushes: quiescence would
            // otherwise stand a pawn on the seventh and score it as a pawn
            if !CAPTURES_ONLY || can_promote {
                let to = match self.active_color {
                    Color::White => from as isize + 8,
                    Color::Black => from as isize - 8,
                };
                // the evasion mask is asked of each push and not of the step
                // they share: a double push can block a check the single
                // push does not reach
                if (0..64).contains(&to) && !all_pieces.is_bit_set(to as u8) {
                    let to = to as u8;
                    let blocks = evasion_filter.is_bit_set(to);
                    if can_promote {
                        if blocks {
                            for p in PromotePiece::VARIANTS {
                                moves.push(Play::new(from, to, None, Some(p), false, false));
                            }
                        }
                    } else {
                        if blocks {
                            moves.push(Play::new(from, to, None, None, false, false));
                        }
                        if match self.active_color {
                            Color::White => rank == 2,
                            Color::Black => rank == 7,
                        } {
                            let to = match self.active_color {
                                Color::White => to as isize + 8,
                                Color::Black => to as isize - 8,
                            };
                            if !all_pieces.is_bit_set(to as u8)
                                && evasion_filter.is_bit_set(to as u8)
                            {
                                moves.push(Play::new(from, to as u8, None, None, false, false));
                            }
                        }
                    }
                }
            }
            if let Some(en_passant) = &self.en_passant {
                let i = en_passant.as_index();
                let can_en_passant = match self.active_color {
                    Color::White => attack_masks.black_pawns[from as usize].is_bit_set(i),
                    Color::Black => attack_masks.white_pawns[from as usize].is_bit_set(i),
                };
                if can_en_passant {
                    moves.push(Play::new(from, i, Some(Piece::Pawn), None, true, false));
                }
            }
        }
        moves.finish()
    }

    /// Check everything maintained a piece at a time against the position it
    /// describes. Perft looks at none of it, so without this a mistake leaves
    /// every count correct and shows up as the engine evaluating or
    /// transposing wrongly.
    ///
    /// Each recompute (the ones below and `Accumulator::recomputed`) is a
    /// second implementation on purpose. Factoring shared code out of a
    /// recompute and the path it checks would leave both wrong together and
    /// this passing: do not tidy them into each other.
    fn debug_assert_state_in_step(&self) {
        debug_assert_eq!(
            self.eval,
            Accumulator::recomputed(self),
            "evaluation out of step"
        );
        debug_assert_eq!(self.key, self.recompute_key(), "key out of step");
        debug_assert_eq!(
            self.pawn_key,
            self.recompute_pawn_key(),
            "pawn key out of step"
        );
        debug_assert_eq!(
            self.squares,
            self.recompute_squares(),
            "squares out of step"
        );
        // the key recompute reads the rights and the en passant square as
        // they stand, so it cannot tell a field set against the rule: assert
        // the rules themselves. Both hold of a played position, and
        // `from_fen` drops what a fen states past them.
        debug_assert_eq!(
            self.castle,
            self.rights_the_pieces_bear_out(),
            "a castle right without the king and rook for it"
        );
        if let Some(en_passant) = self.en_passant {
            debug_assert!(
                self.en_passant_can_be_played(en_passant.as_index()),
                "en passant square the position does not bear out"
            );
        }
    }

    /// The position key computed from the board, built the way `from_fen`
    /// builds it. `key` is meant to equal this at all times.
    fn recompute_key(&self) -> u64 {
        let mut key = INITIAL_KEY;
        let mut occupied = self.white | self.black;
        while occupied != 0 {
            let index = pop_lsb(&mut occupied);
            if let Some((piece, color)) = self.get_piece_and_color_index(index) {
                key ^= ZOBRIST.get_piece_key(index, piece, color);
            }
        }
        if self.active_color == Color::Black {
            key ^= ZOBRIST.side;
        }
        key ^= ZOBRIST.castle_key(self.castle);
        if let Some(en_passant) = self.en_passant {
            key ^= ZOBRIST.en_passant_key(en_passant.as_index());
        }
        key
    }

    /// The pawn key computed from the pawn boards. `pawn_key` is meant to
    /// equal this at all times.
    fn recompute_pawn_key(&self) -> u64 {
        let mut key = 0;
        let mut pawns = self.pawns();
        while pawns != 0 {
            let index = pop_lsb(&mut pawns);
            let color = if self.white.is_bit_set(index) {
                Color::White
            } else {
                Color::Black
            };
            key ^= ZOBRIST.get_piece_key(index, Piece::Pawn, color);
        }
        key
    }

    /// The occupied squares, for the eval module's recompute walk.
    pub(crate) fn occupied(&self) -> u64 {
        self.white | self.black
    }

    /// What stands on each square according to the piece boards. `squares`
    /// is meant to equal this at all times, over all sixty four squares: an
    /// entry left behind on a square since emptied is the drift worth
    /// catching, and a walk of the occupied squares would never look at it.
    fn recompute_squares(&self) -> [Option<Piece>; 64] {
        // popping the pieces off their boards rather than asking each square
        // what stands on it: this runs on every move of every debug test,
        // and the per-square walk took the debug half of ci from under five
        // minutes to fourteen. The empty squares stay None by never being
        // written
        let mut squares = [None; 64];
        for (index, board) in self.pieces.iter().enumerate() {
            let piece = Piece::PIECES[index];
            let mut remaining = *board;
            while remaining != 0 {
                squares[pop_lsb(&mut remaining) as usize] = Some(piece);
            }
        }
        squares
    }

    pub fn square_attacked(&self, index: u8, color: Color) -> bool {
        let all = self.black | self.white;
        let attack_masks = &ATTACK_MASKS;
        let magic = &MAGIC;
        let (color_mask, pawn_masks) = match color {
            Color::Black => (self.black, &attack_masks.black_pawns),
            Color::White => (self.white, &attack_masks.white_pawns),
        };
        if (pawn_masks[index as usize] & self.pawns() & color_mask) > 0 {
            return true;
        }

        if (attack_masks.knights[index as usize] & self.knights() & color_mask) > 0 {
            return true;
        }

        let bishop_or_queen = (self.bishops() | self.queens()) & color_mask;
        if (attack_masks.diagonal[index as usize] & bishop_or_queen) > 0 {
            let move_mask = magic.get_diagonal_move(index, all);
            if (move_mask & bishop_or_queen) > 0 {
                return true;
            }
        }

        let rook_or_queen = (self.rooks() | self.queens()) & color_mask;
        if (attack_masks.straight[index as usize] & rook_or_queen) > 0 {
            let move_mask = magic.get_straight_move(index, all);
            if (move_mask & rook_or_queen) > 0 {
                return true;
            }
        }

        if (attack_masks.kings[index as usize] & self.kings() & color_mask) > 0 {
            return true;
        }

        false
    }

    /// Whether a rook line (`STRAIGHT`) or a bishop line of `color`'s reaches
    /// `index` through the occupancy as it stands: the one slider kind of
    /// `square_attacked`, for the legality probe that knows which kind a
    /// move could have opened.
    #[inline(always)]
    fn slider_reaches<const STRAIGHT: bool>(&self, index: u8, color: Color) -> bool {
        let (theirs, _) = self.sides(color);
        let all = self.black | self.white;
        let magic = &MAGIC;
        if STRAIGHT {
            magic.get_straight_move(index, all) & (self.rooks() | self.queens()) & theirs != 0
        } else {
            magic.get_diagonal_move(index, all) & (self.bishops() | self.queens()) & theirs != 0
        }
    }

    /// Every piece of either colour bearing on `index` through `occupied`.
    /// The swap asks for the two halves separately; this is the whole
    /// statement of an attacker, which `recompute_checkers` reads and the
    /// exhaustive model `see` is checked against. `square_attacked` keeps
    /// its own reading, which stops at the first attacker it finds:
    /// written over this it measured 0.36% more instructions.
    #[inline]
    fn attackers_to(&self, index: u8, occupied: u64) -> u64 {
        (self.steppers_onto(index) | self.sliders_onto(index, occupied)) & occupied
    }

    /// The pawns, knights and kings bearing on `index`: the half of
    /// `attackers_to` that does not depend on the occupancy, so a swap works
    /// it out once rather than at every capture it prices.
    #[inline]
    fn steppers_onto(&self, index: u8) -> u64 {
        let attack_masks = &ATTACK_MASKS;
        let i = index as usize;
        let pawns = ((attack_masks.white_pawns[i] & self.white)
            | (attack_masks.black_pawns[i] & self.black))
            & self.pawns();
        pawns | (attack_masks.knights[i] & self.knights()) | (attack_masks.kings[i] & self.kings())
    }

    /// The bishops, rooks and queens bearing on `index` through `occupied`.
    #[inline]
    fn sliders_onto(&self, index: u8, occupied: u64) -> u64 {
        let attack_masks = &ATTACK_MASKS;
        let magic = &MAGIC;
        let i = index as usize;
        let mut attackers = 0;
        let diagonal = self.bishops() | self.queens();
        if attack_masks.diagonal[i] & diagonal != 0 {
            attackers |= magic.get_diagonal_move(index, occupied) & diagonal;
        }
        let straight = self.rooks() | self.queens();
        if attack_masks.straight[i] & straight != 0 {
            attackers |= magic.get_straight_move(index, occupied) & straight;
        }
        attackers
    }

    /// The least valuable piece of `set`: the bit of one such piece and what
    /// it is. `set` is a subset of one side's pieces.
    fn least_valuable(&self, set: u64) -> Option<(u64, Piece)> {
        // every swap ends by asking this of an empty set
        if set == 0 {
            return None;
        }
        for piece in Piece::PIECES {
            let subset = set & self.pieces[piece as usize];
            if subset != 0 {
                return Some((subset & subset.wrapping_neg(), piece));
            }
        }
        None
    }

    /// Static exchange evaluation: what this capture wins, in centipawns,
    /// once every profitable recapture on its square has been traded
    /// through. The least valuable attacker captures on each side in turn,
    /// sliders behind a capturer joining as the line opens, and a negamax
    /// fold over the recorded gains lets either side stop where continuing
    /// stands worse.
    ///
    /// En passant is played exactly: the pawn taken is lifted from its own
    /// square, so a slider it was blocking joins the swap. A promotion is
    /// counted as the pawn it was on both sides of the exchange, which
    /// undervalues promoting captures; the ordering promotions get is theirs
    /// to fix. Pins are ignored. A move with no victim is worth zero.
    pub(crate) fn see(&self, m: &Play) -> i32 {
        let Some(victim) = m.capture else {
            return 0;
        };
        // more slots than pieces that could ever join one square's swap.
        // Every slot is written before it is read: `d` counts the swap up
        // and the fold reads it down
        let mut gain = [const { MaybeUninit::<i32>::uninit() }; 32];
        gain[0].write(SEE_VALUES[victim as usize]);
        let mut occupied = self.white | self.black;
        occupied &= !(1u64 << m.from);
        if m.en_passant {
            let taken = match self.active_color {
                Color::White => m.to - 8,
                Color::Black => m.to + 8,
            };
            occupied &= !(1u64 << taken);
        }
        let mut on_square = self
            .get_piece_index(m.from)
            .expect("a capture moves a piece of ours");
        let mut side = !self.active_color;
        let mut d = 0;
        // the steppers do not change as the swap empties the square, so they
        // are found once; masking by `occupied` drops the ones that have
        // already captured
        let steppers = self.steppers_onto(m.to);
        loop {
            let side_mask = match side {
                Color::White => self.white,
                Color::Black => self.black,
            };
            let attackers = (steppers | self.sliders_onto(m.to, occupied)) & occupied & side_mask;
            let Some((bit, piece)) = self.least_valuable(attackers) else {
                break;
            };
            d += 1;
            // SAFETY: slot `d - 1` was written before `d` reached this
            // value, slot zero above and every later one here
            let taken = unsafe { gain[d - 1].assume_init() };
            gain[d].write(SEE_VALUES[on_square as usize] - taken);
            // taking a king ends the swap: the side whose king would be
            // taken could not legally have captured here, which the fold
            // reads off the king's price
            if matches!(on_square, Piece::King) {
                break;
            }
            occupied &= !bit;
            on_square = piece;
            side = !side;
        }
        // at each step the side to move keeps the better of stopping and
        // the exchange it recorded
        while d > 0 {
            // SAFETY: the loop above wrote every slot up to the `d` it left
            let (stop, go_on) = unsafe { (gain[d - 1].assume_init(), gain[d].assume_init()) };
            gain[d - 1].write(-(-stop).max(go_on));
            d -= 1;
        }
        // SAFETY: slot zero was written before the swap began
        unsafe { gain[0].assume_init() }
    }

    /// How many times this position has already appeared, not counting the
    /// position itself, stopping once `enough` have been found.
    ///
    /// Every other ply is looked at: a key carries the side to move and
    /// every ply hands the move over, so an entry an odd number of plies
    /// back can never equal this key.
    fn prior_occurrences(&self, enough: usize) -> usize {
        // only the fifty move window can hold a repetition, since a pawn
        // move or a capture puts the position out of reach for good. A fen
        // can claim a count longer than the history or the game
        let window = self.fifty_move_rule.min(self.ply).min(MAX_GAME_SIZE - 1);
        let mut found = 0;
        let mut back = 2;
        while back <= window {
            if let Some(state) = self.history[history_index(self.ply - back)] {
                if state.position_key == self.key {
                    found += 1;
                    if found >= enough {
                        return found;
                    }
                }
            }
            back += 2;
        }
        found
    }

    /// The fifty move counter, as the fen prints it.
    pub fn halfmove_clock(&self) -> usize {
        self.fifty_move_rule
    }

    /// The move number, as the fen prints it: `from_fen` starts the ply at
    /// twice the number it read, one more with black to move, so the number
    /// is the ply halved and needs no keeping.
    fn move_number(&self) -> usize {
        self.ply / 2
    }

    /// Whether the fifty move counter has run out. Not the same as drawn: a
    /// mate delivered on the hundredth half move ends the game before the
    /// side mated has a move to claim the draw with, so a caller that can
    /// tell a mate asks `has_legal_move` as well.
    pub fn fifty_move_expired(&self) -> bool {
        self.fifty_move_rule >= 100
    }

    /// Whether the fifty move counter stands within four plies of expiry:
    /// the horizon behind which the rule50 taint policy refuses every
    /// transposition cutoff, as Stockfish does in its main search, and here
    /// in quiescence besides.
    pub fn fifty_move_near_expiry(&self) -> bool {
        self.fifty_move_rule >= 96
    }

    /// True on the third occurrence, which is when a game is actually drawn.
    /// Nothing in the search calls it; it is the rule `has_repeated` is
    /// measured against, and what the tests contrast it with.
    #[allow(dead_code)]
    pub(crate) fn is_repetition(&self) -> bool {
        self.prior_occurrences(2) >= 2
    }

    /// True once this position has come up before. Inside a search that is
    /// enough to call it a draw: a position reached twice can be reached a
    /// third time by whichever side wants it, and waiting for the third
    /// costs four plies of depth to see what is available now.
    ///
    /// A claim here is a claim that a legal path came back to this position,
    /// which is why the entry a pass writes is salted. A pass is not a move
    /// either side has, and an unsalted entry would let a later real
    /// position count the passed-from position as an occurrence and take a
    /// draw no rule grants. The window is a range of plies rather than a
    /// walk back from here, so the real entries either side of a pass still
    /// compare as they always did. Engines that let a repetition be claimed
    /// through a pass differ here.
    pub fn has_repeated(&self) -> bool {
        self.prior_occurrences(1) >= 1
    }

    /// Whether the side to move has a legal move at all. Asked only where a
    /// draw rule and a mate could coincide, so it plays the moves rather
    /// than keeping anything incremental.
    pub fn has_legal_move(&mut self) -> bool {
        let moves = self.evasions();
        for m in &moves {
            if self.make_move(m) {
                self.undo_move();
                return true;
            }
        }
        false
    }

    pub(crate) fn make_move(&mut self, play: &Play) -> bool {
        self.make_move_impl::<true>(play)
    }

    /// Perft counts with MAINTAIN_CHECKERS off: the legality probe then runs
    /// unconditionally, since the stale checkers cannot be consulted, and
    /// `checkers_given` is skipped. History still saves and restores the
    /// field, so the board's checkers are intact once the walk unwinds.
    fn make_move_impl<const MAINTAIN_CHECKERS: bool>(&mut self, play: &Play) -> bool {
        self.history[history_index(self.ply)] = Some(PlayState {
            play: *play,
            en_passant: self.en_passant,
            castle: self.castle,
            fifty_move_rule: self.fifty_move_rule,
            position_key: self.key,
            checkers: self.checkers,
        });

        let opposing_color = !self.active_color;
        let old_castle = self.castle;
        let bits = old_castle.bits()
            & CASTLE_LEAVING[play.from as usize]
            & CASTLE_LANDING[play.to as usize];
        // the rights change on a handful of moves in a game; on every other
        // one the old and new castle keys would cancel, so one comparison
        // spares folding both
        if bits != old_castle.bits() {
            self.castle = CastlePermissions::from_bits(bits);
            self.key ^= ZOBRIST.castle_key(old_castle) ^ ZOBRIST.castle_key(self.castle);
        }
        if let Some(en_passant) = self.en_passant {
            self.key ^= ZOBRIST.en_passant_key(en_passant.as_index());
        }
        self.en_passant = None;
        self.fifty_move_rule += 1;

        if self.pawns().is_bit_set(play.from) {
            self.fifty_move_rule = 0;
            if (play.from as isize - play.to as isize).abs() == 16 {
                // the square passed over belongs in the key only if a pawn
                // can take on it. Hashing it unconditionally makes one
                // position hash two ways, which costs transposition hits and
                // hides a repetition either side of a double push
                let passed = match self.active_color {
                    Color::White => play.to - 8,
                    Color::Black => play.to + 8,
                };
                if self.pawn_can_capture_on(passed, opposing_color) {
                    self.en_passant = Some(Coordinate::from_index(passed));
                    self.key ^= ZOBRIST.en_passant_key(passed);
                }
            }
            if play.en_passant {
                let clear_index = match self.active_color {
                    Color::White => play.to - 8,
                    Color::Black => play.to + 8,
                };
                self.clear_piece_index(clear_index, Piece::Pawn, opposing_color);
            }
        }

        if let Some(capture) = play.capture {
            if !play.en_passant {
                self.fifty_move_rule = 0;
                self.clear_piece_index(play.to, capture, opposing_color);
            }
        }
        let from_piece = self
            .get_piece_index(play.from)
            .expect("The from square must always be occupied");
        self.move_piece(
            play.from,
            play.to,
            from_piece,
            play.promote,
            self.active_color,
        );

        if play.castle {
            match play.to {
                C1 => self.move_piece(A1, D1, Piece::Rook, None, self.active_color),
                C8 => self.move_piece(A8, D8, Piece::Rook, None, self.active_color),
                G1 => self.move_piece(H1, F1, Piece::Rook, None, self.active_color),
                G8 => self.move_piece(H8, F8, Piece::Rook, None, self.active_color),
                _ => unreachable!(),
            }
        }

        self.ply += 1;
        self.line_ply += 1;

        let king_index = self.king_index(self.active_color);
        // A move can only expose its own king when there was a check to walk
        // back into, the king itself moved, en passant emptied a second
        // square, or a square on a line through the king was vacated. Any
        // other move leaves the king as unattacked as it was. The first
        // three take the full probe. The fourth takes one slider probe: the
        // king stood unattacked, so the only attack the move can open runs
        // through the square it left, a rook line or a bishop line and
        // never both, and the landing square can only block a line. A
        // probe from the king over the occupancy as it now stands reads
        // every line of that kind at once, and any it finds open is the one
        // the move opened. `checkers` still holds the mover's own checkers
        // here; it is replaced below once the move stands.
        let attack_masks = &ATTACK_MASKS;
        let probe = if !MAINTAIN_CHECKERS
            || self.checkers != 0
            || from_piece == Piece::King
            || play.en_passant
        {
            Exposure::Whole
        } else if attack_masks.straight[king_index as usize].is_bit_set(play.from) {
            Exposure::Straight
        } else if attack_masks.diagonal[king_index as usize].is_bit_set(play.from) {
            Exposure::Diagonal
        } else {
            Exposure::None
        };
        self.active_color = opposing_color;
        self.key ^= ZOBRIST.side;
        self.debug_assert_state_in_step();
        let exposed = match probe {
            Exposure::Whole => self.square_attacked(king_index, opposing_color),
            Exposure::Straight => self.slider_reaches::<true>(king_index, opposing_color),
            Exposure::Diagonal => self.slider_reaches::<false>(king_index, opposing_color),
            Exposure::None => false,
        };
        debug_assert_eq!(
            exposed,
            self.square_attacked(king_index, opposing_color),
            "the line probe and the full probe disagree after {}",
            play
        );
        if exposed {
            self.undo_move();
            false
        } else {
            if MAINTAIN_CHECKERS {
                let landed = match play.promote {
                    Some(promote) => (&promote).into(),
                    None => from_piece,
                };
                self.checkers = self.checkers_given(play, landed);
                debug_assert_eq!(
                    self.checkers,
                    self.recompute_checkers(),
                    "checkers out of step after {}",
                    play
                );
            }
            true
        }
    }

    #[inline(always)]
    pub(crate) fn undo_move(&mut self) {
        let previous = history_index(self.ply - 1);
        let history = self.history[previous].unwrap();
        self.history[previous] = None;
        let play = history.play;

        let opposing_color = !self.active_color;
        self.castle = history.castle;
        self.en_passant = history.en_passant;
        self.fifty_move_rule = history.fifty_move_rule;
        self.ply -= 1;
        self.line_ply -= 1;

        if play.en_passant {
            let en_passant_index = match opposing_color {
                Color::White => play.to - 8,
                Color::Black => play.to + 8,
            };
            self.set_piece_index(en_passant_index, Piece::Pawn, self.active_color);
        }

        let from_piece = self
            .get_piece_index(play.to)
            .expect("The to square must always be occupied when undoing");
        if let Some(promote) = play.promote {
            self.clear_piece_index(play.to, (&promote).into(), opposing_color);
            self.set_piece_index(play.from, Piece::Pawn, opposing_color);
        } else {
            self.relocate_piece_index(play.to, play.from, from_piece, opposing_color);
        }

        if let Some(capture) = play.capture {
            if !play.en_passant {
                self.set_piece_index(play.to, capture, self.active_color);
            }
        }
        if play.castle {
            match play.to {
                C1 => self.move_piece(D1, A1, Piece::Rook, None, opposing_color),
                C8 => self.move_piece(D8, A8, Piece::Rook, None, opposing_color),
                G1 => self.move_piece(F1, H1, Piece::Rook, None, opposing_color),
                G8 => self.move_piece(F8, H8, Piece::Rook, None, opposing_color),
                _ => unreachable!(),
            }
        }

        self.active_color = opposing_color;
        // the key comes back from the history rather than being unfolded, so
        // make and undo cannot let it drift
        self.key = history.position_key;
        self.checkers = history.checkers;
    }

    /// Hand the move to the other side without touching a piece. Not a
    /// chess move: the search passes to ask what a position is worth to a
    /// side that does nothing.
    ///
    /// The fifty move counter runs on, so near the horizon passing does not
    /// buy the side to move its way out of a draw. The en passant square
    /// goes, as on any move. The checkers come out empty, because the side
    /// not to move is never in check. The history entry's key is salted to
    /// keep the pass out of the repetition arithmetic; `has_repeated` has
    /// the reasoning.
    pub(crate) fn make_null_move(&mut self) {
        debug_assert!(!self.in_check(), "a side in check cannot pass");
        self.history[history_index(self.ply)] = Some(PlayState {
            play: NULL_PLAY,
            en_passant: self.en_passant,
            castle: self.castle,
            fifty_move_rule: self.fifty_move_rule,
            position_key: self.key ^ NULL_HISTORY_SALT,
            checkers: self.checkers,
        });

        if let Some(en_passant) = self.en_passant {
            self.key ^= ZOBRIST.en_passant_key(en_passant.as_index());
            self.en_passant = None;
        }
        self.active_color = !self.active_color;
        self.key ^= ZOBRIST.side;
        self.fifty_move_rule += 1;
        self.ply += 1;
        self.line_ply += 1;
        self.checkers = 0;

        self.debug_assert_state_in_step();
        debug_assert_eq!(
            self.checkers,
            self.recompute_checkers(),
            "checkers out of step after a pass"
        );
    }

    /// Take the pass back. The mirror of `make_null_move`, and the only thing
    /// that may follow one.
    pub(crate) fn undo_null_move(&mut self) {
        let previous = history_index(self.ply - 1);
        let history = self.history[previous].unwrap();
        self.history[previous] = None;
        debug_assert_eq!(history.play, NULL_PLAY, "the last ply was not a pass");

        self.castle = history.castle;
        self.en_passant = history.en_passant;
        self.fifty_move_rule = history.fifty_move_rule;
        self.ply -= 1;
        self.line_ply -= 1;
        self.active_color = !self.active_color;
        self.key = history.position_key ^ NULL_HISTORY_SALT;
        self.checkers = history.checkers;

        self.debug_assert_state_in_step();
    }

    #[inline(always)]
    fn move_piece(
        &mut self,
        from: u8,
        to: u8,
        piece: Piece,
        promote_piece: Option<PromotePiece>,
        color: Color,
    ) {
        debug_assert!((self.black | self.white).is_bit_set(from));
        debug_assert!(!(self.black | self.white).is_bit_set(to));
        match promote_piece {
            // a promotion is not a relocation: the material and the phase
            // both change
            Some(promote) => {
                self.clear_piece_index(from, piece, color);
                self.set_piece_index(to, (&promote).into(), color);
            }
            None => self.relocate_piece_index(from, to, piece, color),
        }
    }

    /// The castle rights this position has the pieces for: a right whose king
    /// or rook is not on the square the castle moves it from is dropped.
    ///
    /// The generator reads the right rather than the pieces, so a right held
    /// over a king somewhere else is a castle out of that square, and
    /// make_move relocates whatever sits on the rook's corner as though it
    /// were the rook. Only `from_fen` can produce such a right.
    fn rights_the_pieces_bear_out(&self) -> CastlePermissions {
        let holds = |index: u8, piece: Piece, color: Color| {
            self.get_piece_and_color_index(index) == Some((piece, color))
        };
        let white_king = holds(E1, Piece::King, Color::White);
        let black_king = holds(E8, Piece::King, Color::Black);
        let mut borne_out = CastlePermissions::ALL;
        for (right, standing) in [
            (
                CastlePermissions::WHITE_KING_SIDE,
                white_king && holds(H1, Piece::Rook, Color::White),
            ),
            (
                CastlePermissions::WHITE_QUEEN_SIDE,
                white_king && holds(A1, Piece::Rook, Color::White),
            ),
            (
                CastlePermissions::BLACK_KING_SIDE,
                black_king && holds(H8, Piece::Rook, Color::Black),
            ),
            (
                CastlePermissions::BLACK_QUEEN_SIDE,
                black_king && holds(A8, Piece::Rook, Color::Black),
            ),
        ] {
            if !standing {
                borne_out &= !right;
            }
        }
        CastlePermissions::from_bits(self.castle.bits() & borne_out)
    }

    /// Whether an en passant capture on this square is one this position can
    /// actually make: the rank a double push crosses, a pawn of ours placed
    /// to take there, the square itself empty, and the pawn the capture
    /// removes standing behind it.
    ///
    /// The pawn placed to take is make_move's own rule and says whether the
    /// square belongs in the key. The generator emits the capture from the
    /// square alone and checks none of the rest; without them make_move
    /// clears a pawn from a square holding something else, or lands the
    /// capturer on top of a piece nothing took.
    fn en_passant_can_be_played(&self, index: u8) -> bool {
        let (rank, _) = index_to_coordinate(index);
        let crossed = match self.active_color {
            Color::White => 6,
            Color::Black => 3,
        };
        if rank != crossed {
            return false;
        }
        // the rank check above is what keeps this on the board
        let taken = match self.active_color {
            Color::White => index - 8,
            Color::Black => index + 8,
        };
        self.pawn_can_capture_on(index, self.active_color)
            && !(self.white | self.black).is_bit_set(index)
            && self.get_piece_and_color_index(taken) == Some((Piece::Pawn, !self.active_color))
    }

    /// Whether a pawn of this colour is placed to take on this square.
    fn pawn_can_capture_on(&self, index: u8, capturer: Color) -> bool {
        let attack_masks = &ATTACK_MASKS;
        let (from, pawns) = match capturer {
            Color::White => (attack_masks.white_pawns[index as usize], self.white),
            Color::Black => (attack_masks.black_pawns[index as usize], self.black),
        };
        from & self.pawns() & pawns != 0
    }

    /// The six piece boards by name, each a constant index into `pieces`.
    #[inline]
    pub(crate) fn pawns(&self) -> u64 {
        self.pieces[Piece::Pawn as usize]
    }

    #[inline]
    pub(crate) fn knights(&self) -> u64 {
        self.pieces[Piece::Knight as usize]
    }

    #[inline]
    pub(crate) fn bishops(&self) -> u64 {
        self.pieces[Piece::Bishop as usize]
    }

    #[inline]
    pub(crate) fn rooks(&self) -> u64 {
        self.pieces[Piece::Rook as usize]
    }

    #[inline]
    pub(crate) fn queens(&self) -> u64 {
        self.pieces[Piece::Queen as usize]
    }

    #[inline]
    pub(crate) fn kings(&self) -> u64 {
        self.pieces[Piece::King as usize]
    }

    /// Where this side's king stands. Every board has exactly one king a side,
    /// which is what `from_fen` checks for: without a king this returns 64 and
    /// the attack masks are indexed off the end.
    pub(crate) fn king_index(&self, color: Color) -> u8 {
        let (ours, _) = self.sides(color);
        (self.kings() & ours).trailing_zeros() as u8
    }

    /// This side's pieces and the other side's, in that order.
    #[inline]
    pub(crate) fn sides(&self, color: Color) -> (u64, u64) {
        match color {
            Color::White => (self.white, self.black),
            Color::Black => (self.black, self.white),
        }
    }

    /// Whether the side to move stands in check, read from the checkers
    /// `make_move` maintains rather than by probing the king's square.
    pub fn in_check(&self) -> bool {
        self.checkers != 0
    }

    /// Whether the side to move has anything but pawns and its king. A side
    /// that has not is the side zugzwang happens to: every move it has
    /// commits a pawn or the king, so the static eval is no floor there.
    pub fn has_non_pawn_material(&self) -> bool {
        let ours = match self.active_color {
            Color::White => self.white,
            Color::Black => self.black,
        };
        ours & !(self.pawns() | self.kings()) != 0
    }

    /// Whether what stands on the board cannot mate, whoever is to move and
    /// however the pieces are placed. A fact about the material, not the
    /// path.
    ///
    /// Four signatures: two bare kings; a lone minor; two knights against a
    /// bare king, where mate exists but cannot be forced; and bishops all on
    /// one square colour with no knight, whichever side owns them. Two
    /// minors that cancel (a knight each, bishops on opposite colours) are
    /// drawn in practice and left out: the evaluation already reads them
    /// near zero, and pricing the near drawn endings is a longer rule.
    pub fn drawn_by_material(&self) -> bool {
        if self.pawns() | self.rooks() | self.queens() != 0 {
            return false;
        }
        let knights = self.knights();
        let bishops = self.bishops();
        // two bare kings, or a lone minor against a bare king
        if (knights | bishops).count_ones() < 2 {
            return true;
        }
        // every bishop on one colour, whichever side owns which
        if knights == 0 && (bishops & LIGHT_SQUARES == 0 || bishops & !LIGHT_SQUARES == 0) {
            return true;
        }
        // both knights on one side, and nothing else
        bishops == 0
            && knights.count_ones() == 2
            && (knights & self.white == 0 || knights & self.black == 0)
    }

    /// The pieces checking the new side to move, asked of the board after the
    /// move. Answered from the move rather than by a full probe: only the
    /// landed piece can check directly, and a slider check needs the move to
    /// have landed a slider on a line through the king or vacated a square
    /// on one, since the king stood unattacked before it. Castling and en
    /// passant displace a second piece and take the full probe.
    ///
    /// The direct and slider findings accumulate rather than short circuit:
    /// a double check is answered differently to a single one.
    fn checkers_given(&self, play: &Play, landed: Piece) -> u64 {
        let defender = self.active_color;
        let king = self.king_index(defender);
        if play.castle || play.en_passant {
            return self.recompute_checkers();
        }
        let attack_masks = &ATTACK_MASKS;
        let magic = &MAGIC;
        let to = play.to;
        let from = play.from;
        let mut checkers = 0u64;

        match landed {
            Piece::Pawn => {
                let masks = match !defender {
                    Color::White => &attack_masks.white_pawns,
                    Color::Black => &attack_masks.black_pawns,
                };
                if masks[king as usize].is_bit_set(to) {
                    checkers.set_bit(to);
                }
            }
            Piece::Knight if attack_masks.knights[king as usize].is_bit_set(to) => {
                checkers.set_bit(to);
            }
            _ => {}
        }

        let attacker_mask = match defender {
            Color::White => self.black,
            Color::Black => self.white,
        };
        let all = self.black | self.white;
        let diagonal = attack_masks.diagonal[king as usize];
        if (matches!(landed, Piece::Bishop | Piece::Queen) && diagonal.is_bit_set(to))
            || diagonal.is_bit_set(from)
        {
            let attackers = (self.bishops() | self.queens()) & attacker_mask;
            if attackers != 0 {
                checkers |= magic.get_diagonal_move(king, all) & attackers;
            }
        }
        let straight = attack_masks.straight[king as usize];
        if (matches!(landed, Piece::Rook | Piece::Queen) && straight.is_bit_set(to))
            || straight.is_bit_set(from)
        {
            let attackers = (self.rooks() | self.queens()) & attacker_mask;
            if attackers != 0 {
                checkers |= magic.get_straight_move(king, all) & attackers;
            }
        }
        checkers
    }

    /// What `gives_check_with` reads about the other side's king, fixed for
    /// as long as the position is: for each piece kind, the squares a piece
    /// of ours of that kind would check it from, and our pieces that alone
    /// stand between one of our sliders and it.
    pub(crate) fn check_info(&self) -> CheckInfo {
        let king = self.king_index(!self.active_color);
        let attack_masks = &ATTACK_MASKS;
        let magic = &MAGIC;
        let occupied = self.white | self.black;
        let (ours, _) = self.sides(self.active_color);
        let pawns = match self.active_color {
            Color::White => attack_masks.white_pawns[king as usize],
            Color::Black => attack_masks.black_pawns[king as usize],
        };
        let diagonal = magic.get_diagonal_move(king, occupied);
        let straight = magic.get_straight_move(king, occupied);
        let mut snipers = (attack_masks.diagonal[king as usize] & (self.bishops() | self.queens())
            | attack_masks.straight[king as usize] & (self.rooks() | self.queens()))
            & ours;
        let mut blockers = 0;
        while snipers != 0 {
            let sniper = pop_lsb(&mut snipers);
            let between = BETWEEN[king as usize][sniper as usize] & occupied;
            if between != 0 && between & (between - 1) == 0 {
                blockers |= between & ours;
            }
        }
        CheckInfo {
            king,
            squares: [
                pawns,
                attack_masks.knights[king as usize],
                diagonal,
                straight,
                diagonal | straight,
                0,
            ],
            blockers,
        }
    }

    /// `gives_check`, answered from what `check_info` read of this position.
    #[inline(always)]
    pub(crate) fn gives_check_with(&self, info: &CheckInfo, m: &Play) -> bool {
        let answer = self.gives_check_from(info, m);
        debug_assert_eq!(
            answer,
            self.gives_check(m),
            "the check table disagrees on {m}"
        );
        answer
    }

    #[inline(always)]
    fn gives_check_from(&self, info: &CheckInfo, m: &Play) -> bool {
        if m.castle || m.en_passant || m.promote.is_some() {
            return self.gives_check(m);
        }
        let Some(piece) = self.get_piece_index(m.from) else {
            return self.gives_check(m);
        };
        if info.squares[piece as usize] & (1u64 << m.to) != 0 {
            return true;
        }
        info.blockers & (1u64 << m.from) != 0
            && BETWEEN[info.king as usize][m.to as usize] & (1u64 << m.from) == 0
            && BETWEEN[info.king as usize][m.from as usize] & (1u64 << m.to) == 0
    }

    /// Whether this move checks the opponent, asked of the board before the
    /// move is made, for the pruning and reduction gates that want the
    /// answer without paying for make and unmake.
    ///
    /// The occupancy is edited to what the move leaves, the extra square a
    /// castle or en passant touches included, and the slider probes run
    /// from the king over it against our sliders as the move leaves them,
    /// so a discovered check needs no case of its own. Castling is asked
    /// about the rook's destination, since a king cannot check.
    ///
    /// Exact for any move `generate_moves` produces here, which the oracle
    /// test holds over the legal ones; no test makes a refused move to ask.
    pub fn gives_check(&self, m: &Play) -> bool {
        let king = self.king_index(!self.active_color);
        let attack_masks = &ATTACK_MASKS;
        let magic = &MAGIC;
        let from_bit = 1u64 << m.from;
        let to_bit = 1u64 << m.to;
        let mut occupied = ((self.white | self.black) & !from_bit) | to_bit;

        let ours = match self.active_color {
            Color::White => self.white,
            Color::Black => self.black,
        };
        // our sliders as the move leaves them
        let mut diagonal = (self.bishops() | self.queens()) & ours & !from_bit;
        let mut straight = (self.rooks() | self.queens()) & ours & !from_bit;

        let landed = match m.promote {
            Some(promote) => (&promote).into(),
            None => self
                .get_piece_index(m.from)
                .expect("a move moves a piece of ours"),
        };
        match landed {
            Piece::Pawn => {
                let masks = match self.active_color {
                    Color::White => &attack_masks.white_pawns,
                    Color::Black => &attack_masks.black_pawns,
                };
                if masks[king as usize].is_bit_set(m.to) {
                    return true;
                }
            }
            Piece::Knight => {
                if attack_masks.knights[king as usize].is_bit_set(m.to) {
                    return true;
                }
            }
            Piece::Bishop => diagonal |= to_bit,
            Piece::Rook => straight |= to_bit,
            Piece::Queen => {
                diagonal |= to_bit;
                straight |= to_bit;
            }
            Piece::King => {}
        }

        if m.en_passant {
            let taken = match self.active_color {
                Color::White => m.to - 8,
                Color::Black => m.to + 8,
            };
            occupied &= !(1u64 << taken);
        } else if m.castle {
            let (rook_from, rook_to) = match m.to {
                C1 => (A1, D1),
                G1 => (H1, F1),
                C8 => (A8, D8),
                G8 => (H8, F8),
                _ => unreachable!(),
            };
            occupied = (occupied & !(1u64 << rook_from)) | (1u64 << rook_to);
            straight = (straight & !(1u64 << rook_from)) | (1u64 << rook_to);
        }

        if attack_masks.diagonal[king as usize] & diagonal != 0
            && magic.get_diagonal_move(king, occupied) & diagonal != 0
        {
            return true;
        }
        attack_masks.straight[king as usize] & straight != 0
            && magic.get_straight_move(king, occupied) & straight != 0
    }

    /// The pieces checking the side to move, computed from the board.
    /// `checkers` is meant to equal this at all times.
    fn recompute_checkers(&self) -> u64 {
        let king = self.king_index(self.active_color);
        let (theirs, _) = self.sides(!self.active_color);
        // a king cannot give check, so there is no king term
        self.attackers_to(king, self.black | self.white) & theirs & !self.kings()
    }

    /// Drop the moves that cannot answer the check the side to move stands
    /// in: everything but a king move, a capture of the sole checker or a
    /// block of its line. En passant is kept unexamined, since the captured
    /// pawn does not stand on the to square and the masks would misread it.
    ///
    /// The generator masks its targets instead, so this is off the search's
    /// path. It is kept as the second statement of the rule that
    /// `the_masked_generator_keeps_what_the_filter_kept` holds the generator
    /// to: do not fold it into the generator.
    #[cfg(test)]
    fn retain_evasions(&self, moves: &mut MoveList) {
        debug_assert!(self.checkers != 0, "asked of a position not in check");
        let targets = if self.checkers.count_ones() > 1 {
            0
        } else {
            let checker = self.checkers.trailing_zeros() as usize;
            let king = self.king_index(self.active_color) as usize;
            self.checkers | BETWEEN[king][checker]
        };
        let kings = self.kings();
        let list = moves.as_mut_slice();
        let mut kept = 0;
        for i in 0..list.len() {
            let play = list[i];
            if kings.is_bit_set(play.from) || play.en_passant || targets.is_bit_set(play.to) {
                list[kept] = play;
                kept += 1;
            }
        }
        moves.truncate(kept);
    }

    /// Print every square this colour attacks as a grid, for when
    /// `square_attacked` misbehaves. Uncalled on purpose, see
    /// `BitBoard::debug_print`.
    #[allow(dead_code)]
    fn attacked_print(&self, color: Color) {
        println!("   a|b|c|d|e|f|g|h|");
        println!("  ----------------");
        for rank in (1..=8).rev() {
            print!("{} |", rank);
            for file in File::VARIANTS {
                let index = coordinate_to_index(rank, file);
                if self.square_attacked(index, color) {
                    print!("x|");
                } else {
                    print!(".|");
                }
            }
            println!();
        }
        println!();
    }

    /// Put a piece on a square, with the position key, the pawn key, the
    /// piece square score and the material accumulators following it on.
    #[inline]
    fn set_piece_index(&mut self, index: u8, piece: Piece, color: Color) {
        debug_assert!(!self.black.is_bit_set(index));
        debug_assert!(!self.white.is_bit_set(index));
        self.move_accumulators::<true>(index, piece, color);
    }

    fn set_piece(&mut self, piece: Piece, color: Color, rank: u8, file: File) {
        let index = coordinate_to_index(rank, file);
        self.set_piece_index(index, piece, color);
    }

    /// Move a piece between two squares. A clear and a set say the same
    /// thing in halves that cancel: the piece never leaves the board, so the
    /// material and the phase are untouched and only the two squares differ.
    #[inline(always)]
    fn relocate_piece_index(&mut self, from: u8, to: u8, piece: Piece, color: Color) {
        debug_assert!(from != to);
        debug_assert!(from < 64 && to < 64);
        let moved =
            ZOBRIST.get_piece_key(from, piece, color) ^ ZOBRIST.get_piece_key(to, piece, color);
        self.key ^= moved;
        // the pawn key uses the same randoms over the pawns alone
        if piece == Piece::Pawn {
            self.pawn_key ^= moved;
        }
        self.eval.relocate(from, to, piece, color);

        let both = (1u64 << from) | (1u64 << to);
        self.pieces[piece as usize] ^= both;
        match color {
            Color::Black => self.black ^= both,
            Color::White => self.white ^= both,
        }
        self.squares[(from & 63) as usize] = None;
        self.squares[(to & 63) as usize] = Some(piece);
    }

    /// Take a piece off a square, undoing everything `set_piece_index` did.
    #[inline]
    fn clear_piece_index(&mut self, index: u8, piece: Piece, color: Color) {
        debug_assert!((self.black | self.white).is_bit_set(index));
        self.move_accumulators::<false>(index, piece, color);
    }

    /// Put down or pick up a piece, the two directions written once. `SET`
    /// is settled at compile time, so no branch on it survives into the
    /// search.
    #[inline(always)]
    fn move_accumulators<const SET: bool>(&mut self, index: u8, piece: Piece, color: Color) {
        let piece_key = ZOBRIST.get_piece_key(index, piece, color);
        self.key ^= piece_key;
        if piece == Piece::Pawn {
            self.pawn_key ^= piece_key;
        }
        self.eval.count::<SET>(index, piece, color);

        let board = &mut self.pieces[piece as usize];
        if SET {
            board.set_bit(index);
        } else {
            board.clear_bit(index);
        }
        // the callers assert that a set lands on an empty square and a clear
        // on an occupied one, so this never asks what was standing there
        self.squares[(index & 63) as usize] = if SET { Some(piece) } else { None };

        let side = match color {
            Color::Black => &mut self.black,
            Color::White => &mut self.white,
        };
        if SET {
            side.set_bit(index);
        } else {
            side.clear_bit(index);
        }
    }

    /// What is being taken on the to square. Most generated moves are quiet,
    /// so the mask answers those from a register and the load is reached
    /// only for the rest.
    #[inline(always)]
    fn capture_on(&self, to: u8, capture_mask: u64) -> Option<Piece> {
        if capture_mask.is_bit_set(to) {
            self.get_piece_index(to)
        } else {
            None
        }
    }

    /// What stands on a square, read rather than searched for.
    #[inline]
    pub(crate) fn get_piece_index(&self, index: u8) -> Option<Piece> {
        debug_assert!(index < 64);
        // masked so the read carries no bounds check; the debug assert is
        // what catches a square off the board
        self.squares[(index & 63) as usize]
    }

    /// Walks the six piece boards rather than reading `squares`. The
    /// recomputes reach a piece through here and everything else through
    /// `get_piece_index`, so a mistake in either cannot hide in the state
    /// check.
    #[inline]
    pub(crate) fn get_piece_and_color_index(&self, index: u8) -> Option<(Piece, Color)> {
        let mask = 1u64 << index;
        let piece = if (self.pawns() & mask) > 0 {
            Piece::Pawn
        } else if (self.knights() & mask) > 0 {
            Piece::Knight
        } else if (self.bishops() & mask) > 0 {
            Piece::Bishop
        } else if (self.rooks() & mask) > 0 {
            Piece::Rook
        } else if (self.queens() & mask) > 0 {
            Piece::Queen
        } else if (self.kings() & mask) > 0 {
            Piece::King
        } else {
            return None;
        };
        let color = if (self.black & mask) > 0 {
            Color::Black
        } else if (self.white & mask) > 0 {
            Color::White
        } else {
            return None;
        };
        Some((piece, color))
    }

    fn get_piece(&self, rank: u8, file: File) -> Option<(Piece, Color)> {
        self.get_piece_and_color_index(coordinate_to_index(rank, file))
    }

    /// The material of each side, counted a bitboard at a time. Shares no
    /// code with the eval module's recompute on purpose: `from_fen` seeds
    /// the accumulator from this one and the state check compares it
    /// against that one.
    pub(crate) fn material_value(&self) -> (u32, u32) {
        let mut white_value = 0;
        let mut black_value = 0;
        for (piece, board) in Piece::PIECES.into_iter().zip(self.pieces) {
            let value = eval::material(piece);
            white_value += (board & self.white).count_ones() * value;
            black_value += (board & self.black).count_ones() * value;
        }
        (white_value, black_value)
    }

    /// Count the legal moves to a depth, the standard measure of whether
    /// move generation is right.
    pub fn perft(&mut self, depth: u8) -> u64 {
        self.perft_impl::<false, false>(depth)
    }

    /// The same count, walked over `evasions` rather than the whole pseudo
    /// legal list. The legal moves sit inside what `evasions` returns and
    /// `make_move` refuses the rest, so the count must not move: this holds
    /// the evasion mask to the perft suites over every position they reach.
    /// The checkers are maintained, since the mask is read off them.
    #[cfg(test)]
    pub(crate) fn perft_through_evasions(&mut self, depth: u8) -> u64 {
        self.perft_impl::<true, true>(depth)
    }

    /// The same count, walked the way the engine plays: checkers maintained
    /// and the legality probe skipped where they allow. Plain `perft`
    /// compiles `checkers_given` and the skip out, so this is what holds
    /// them to the suites' counts: a skip that wrongly cleared a move, or a
    /// `checkers_given` that wrongly said no check, would let an illegal
    /// move stand and the count rise.
    #[cfg(test)]
    pub(crate) fn perft_as_played(&mut self, depth: u8) -> u64 {
        self.perft_impl::<true, false>(depth)
    }

    fn perft_impl<const MAINTAIN_CHECKERS: bool, const THROUGH_EVASIONS: bool>(
        &mut self,
        depth: u8,
    ) -> u64 {
        // Based on pseudocode at https://www.chessprogramming.org/Perft
        let mut nodes = 0;

        if depth == 0 {
            return 1;
        }

        let moves = if THROUGH_EVASIONS {
            self.evasions()
        } else {
            self.generate_moves()
        };
        for m in &moves {
            if self.make_move_impl::<MAINTAIN_CHECKERS>(m) {
                nodes += self.perft_impl::<MAINTAIN_CHECKERS, THROUGH_EVASIONS>(depth - 1);
                self.undo_move();
            }
        }
        nodes
    }

    /// The position a fen describes, or what is wrong with the fen.
    ///
    /// Validated only as far as what the search cannot survive; the known
    /// limitations in `docs/ROADMAP.md` say what an illegal position can
    /// still get away with. A castle right or en passant square the pieces
    /// do not bear out is cut back rather than refused.
    pub fn from_fen(fen: &str) -> Result<Self, String> {
        let mut fen_iter = fen.split(' ');
        let position = fen_iter
            .next()
            .ok_or("Error parsing FEN: could not find position block")?;
        let active_color = fen_iter
            .next()
            .ok_or("Error parsing FEN: expected active color token found none")?;
        let active_color_token = match active_color.chars().next() {
            Some(c) if active_color.len() == 1 => c,
            _ => {
                return Err(format!(
                    "Expected a single character token: {}",
                    active_color
                ));
            }
        };
        let castle = fen_iter
            .next()
            .ok_or("Error parsing FEN: Could not find castle permissions")?;
        let en_passant = fen_iter
            .next()
            .ok_or("Error parsing FEN: Could not find en passant square")?;
        let half_move_clock = fen_iter
            .next()
            .ok_or("Error parsing FEN: Could not find half move clock")?;
        let full_move_clock = fen_iter
            .next()
            .ok_or("Error parsing FEN: Could not find full move clock")?;
        let move_number = full_move_clock
            .parse::<usize>()
            .map_err(|e| e.to_string())?;

        let mut board = Board {
            pieces: [0; 6],
            white: 0,
            black: 0,
            squares: [None; 64],

            active_color: Color::from_char(active_color_token)
                .ok_or("Failed to parse active color from token")?,
            castle: CastlePermissions::from_fen(castle)?,

            ply: move_number * 2,
            line_ply: 0,
            en_passant: Coordinate::from_string(en_passant)?,
            checkers: 0,
            fifty_move_rule: half_move_clock
                .parse::<usize>()
                .map_err(|e| e.to_string())?,
            eval: Accumulator::EMPTY,

            history: EMPTY_HISTORY,
            key: INITIAL_KEY,
            pawn_key: 0,
        };
        if board.active_color == Color::Black {
            board.ply += 1;
        }

        let mut rank = 8;
        // a number rather than a File, because a complete rank ends one
        // square past the h file
        let mut file = 0u8;
        for c in position.chars() {
            if rank < 1 {
                return Err("Too many ranks found".to_string());
            }
            if c == '/' {
                rank -= 1;
                file = 0;
                continue;
            }
            let step = match c {
                '1'..='8' => c.to_digit(10).unwrap() as u8,
                _ => 1,
            };
            if file + step > 8 {
                return Err("Too many files found in rank".to_string());
            }
            if !('1'..='8').contains(&c) {
                let piece = Piece::try_from(c)
                    .map_err(|e| format!("unexpected character in fen: {}", e))?;
                let color = if c.is_uppercase() {
                    Color::White
                } else {
                    Color::Black
                };
                board.set_piece(piece, color, rank, File::try_from(file)?);
            }
            file += step;
        }
        // a king a side is what lets king_index return a real square, and
        // the side which just moved being out of check is what stops the
        // search replying by taking the king
        for color in [Color::White, Color::Black] {
            let mask = match color {
                Color::White => board.white,
                Color::Black => board.black,
            };
            if (board.kings() & mask).count_ones() != 1 {
                return Err(format!(
                    "Error parsing FEN: expected exactly one {:?} king",
                    color
                ));
            }
        }
        if board.square_attacked(board.king_index(!board.active_color), board.active_color) {
            return Err("Error parsing FEN: the side which is not to move is in check".to_string());
        }

        // a right or a square the pieces do not bear out is one the generator
        // would play; each is cut back before the key it belongs in is
        // folded, so the two never disagree
        board.castle = board.rights_the_pieces_bear_out();

        if board.active_color == Color::Black {
            board.key ^= ZOBRIST.side;
        }
        board.key ^= ZOBRIST.castle_key(board.castle);
        // the rule here has to be make_move's own, or a position parsed and
        // the same one played would not hash alike
        if let Some(en_passant) = board.en_passant {
            if board.en_passant_can_be_played(en_passant.as_index()) {
                board.key ^= ZOBRIST.en_passant_key(en_passant.as_index());
            } else {
                board.en_passant = None;
            }
        }
        board.eval.seed_material(board.material_value());
        board.checkers = board.recompute_checkers();
        board.debug_assert_state_in_step();
        Ok(board)
    }

    /// The position as a fen, all six fields, which `from_fen` reads back.
    /// The clocks travel with the position; the path does not, so a board
    /// parsed back has no history to find a repetition in. The rights and
    /// the en passant square are printed as the board holds them, after
    /// `from_fen` has cut them back, so a fen parsed and printed again may
    /// differ from the one that arrived, and printing that one twice does
    /// not.
    pub fn to_fen(&self) -> String {
        let mut fen = String::new();
        for rank in (1..=8).rev() {
            let mut empty = 0;
            for file in File::VARIANTS {
                match self.get_piece(rank, file) {
                    Some((piece, color)) => {
                        if empty > 0 {
                            fen.push_str(&empty.to_string());
                            empty = 0;
                        }
                        let letter = char::from(piece);
                        fen.push(match color {
                            Color::White => letter.to_ascii_uppercase(),
                            Color::Black => letter,
                        });
                    }
                    None => empty += 1,
                }
            }
            if empty > 0 {
                fen.push_str(&empty.to_string());
            }
            if rank > 1 {
                fen.push('/');
            }
        }
        let active_color = match self.active_color {
            Color::White => 'w',
            Color::Black => 'b',
        };
        let en_passant = match self.en_passant {
            Some(square) => square.to_string(),
            None => "-".to_string(),
        };
        format!(
            "{} {} {} {} {} {}",
            fen,
            active_color,
            self.castle.as_fen(),
            en_passant,
            self.fifty_move_rule,
            self.move_number(),
        )
    }
}

impl fmt::Display for Board {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "    a b c d e f g h")?;
        writeln!(f, "  -----------------")?;
        for rank in (1..=8).rev() {
            write!(f, "{} |", rank)?;
            for file in File::VARIANTS {
                match self.get_piece(rank, file) {
                    Some((piece, Color::White)) => {
                        write!(f, " {}", char::from(piece).to_ascii_uppercase())?
                    }
                    Some((piece, Color::Black)) => write!(f, " {}", char::from(piece))?,
                    None => write!(f, " .")?,
                }
            }
            writeln!(f)?;
        }
        writeln!(f)?;
        writeln!(
            f,
            "{:?} to play.  | {} {:?} ply: {} move: {} last capture: {} material: {}",
            self.active_color,
            self.castle.as_fen(),
            self.en_passant,
            self.ply,
            self.move_number(),
            self.fifty_move_rule,
            self.eval.material_difference(),
        )?;
        writeln!(f)?;
        Ok(())
    }
}

/// Positions the test modules share, named for what they bring within reach,
/// so a fen appears once.
#[cfg(test)]
pub(crate) mod fens {
    /// The starting position.
    pub const START: &str = crate::STARTING_FEN;
    /// Kiwipete, the standard tactical middlegame: checks, pins, castling and
    /// an en passant square all within a move or two.
    pub const KIWIPETE: &str =
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
    /// A rook and pawn endgame, position 3 of the standard perft suite.
    pub const PAWN_ENDGAME: &str = "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1";
    /// Promotions for both sides on the next move, position 5 of the standard
    /// perft suite.
    pub const PROMOTIONS: &str = "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8";
    /// A black pawn one push from promoting, with an en passant square set.
    pub const EN_PASSANT_PROMOTION: &str =
        "rnbqkbnr/pp1ppppp/8/2p5/3Pp3/8/PPPP1PpP/RNBQKB1R b KQkq e5 0 2";
    /// A symmetric middlegame where both sides have castled, white to move.
    /// Position 6 of the standard perft suite.
    pub const MIDDLEGAME: &str =
        "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10";
    /// The same position with black to move, which the repetition tests
    /// shuffle rooks in: a8b8 a1b1 b8a8 b1a1 comes straight back to it.
    pub const SHUFFLE: &str =
        "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 b - - 3 19";
    /// A node the late move reduction applies to, and the two kinds of move
    /// at it: the rook can take the pawn on e4 and has quiet moves besides,
    /// one of which checks along the rank.
    pub const A_CAPTURE_AND_QUIETS: &str = "7k/8/8/8/R3p3/8/8/7K w - - 0 1";
    /// A sharp middlegame, white's pieces aimed at the castled black king:
    /// sharp enough that a wrongly reused score moves the verdict, which is
    /// what the search tests want of it.
    pub const SHARP_MIDDLEGAME: &str =
        "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12";
    /// The four positions the accumulator and reversibility suites iterate:
    /// between them promotions, castling, en passant and a bare endgame are
    /// all in reach.
    pub const CORE: [&str; 4] = [START, EN_PASSANT_PROMOTION, MIDDLEGAME, PAWN_ENDGAME];
}

/// The move of this name in this position, so a test can name a line the way
/// the rest of the world writes it.
#[cfg(test)]
pub(crate) fn play_named(board: &Board, name: &str) -> Play {
    board
        .move_named(name)
        .unwrap_or_else(|| panic!("{} is not a move here", name))
}

#[cfg(test)]
mod make_move {
    use super::fens;
    use super::{A1, A8, B1, B8, MAX_GAME_SIZE};
    use super::{Board, Play};
    use pretty_assertions::{assert_eq, assert_ne};

    /// Every legal move must change the position, and unmaking it must give
    /// back a board equal in every field.
    #[test]
    fn every_move_unmakes_back_to_the_position_it_left() {
        for fen in fens::CORE {
            let board = Board::from_fen(fen).unwrap();
            for m in &board.generate_moves() {
                let mut played = board.clone();
                if played.make_move(m) {
                    assert_ne!(board, played, "{} in {}", m, fen);
                    played.undo_move();
                    assert_eq!(board, played, "{} in {}", m, fen);
                }
            }
        }
    }

    /// The captures list is the material changing subset of the full list:
    /// the captures and the promoting pushes, in the same order.
    #[test]
    fn the_captures_list_is_the_material_changing_subset() {
        for fen in fens::CORE {
            let board = Board::from_fen(fen).unwrap();
            let filtered: super::MoveList = board
                .generate_moves()
                .iter()
                .filter(|c| c.capture.is_some() || c.promote.is_some())
                .copied()
                .collect();
            assert_eq!(board.generate_captures(), filtered, "in {}", fen);
        }
    }

    #[test]
    fn a_quiet_promotion_is_in_the_captures_list() {
        // a pawn on the seventh with an empty square ahead: the push captures
        // nothing but changes material like a capture does, and it is the
        // only material changing move here
        let board = Board::from_fen("4k3/P7/8/8/8/8/8/4K3 w - - 0 1").unwrap();
        let captures = board.generate_captures();
        assert_eq!(captures.len(), 4, "one push for each promotion piece");
        for m in &captures {
            assert_eq!(format!("{}", m)[..4].to_string(), "a7a8");
            assert!(m.promote.is_some());
            assert!(m.capture.is_none());
        }
    }

    #[test]
    fn a_long_game_does_not_run_off_the_history() {
        // games which reached about move 175 used to panic in is_repetition
        let mut board = Board::new();
        let cycle = ["g1f3", "b8c6", "f3g1", "c6b8"];
        for i in 0..400 {
            let play = super::play_named(&board, cycle[i % 4]);
            assert!(board.make_move(&play), "failed at ply {}", i);
            board.is_repetition();
        }
    }

    /// The shuffle position at ply 1023, one short of the end of the history
    /// ring, which the two tests below play across.
    const NEAR_THE_WRAP: &str =
        "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 b - - 3 511";

    #[test]
    fn a_repetition_is_still_seen_when_the_history_wraps() {
        let mut board = Board::from_fen(NEAR_THE_WRAP).unwrap();
        assert_eq!(
            board.ply,
            MAX_GAME_SIZE - 1,
            "the cycle must cross the wrap"
        );

        let cycle = [(A8, B8), (A1, B1), (B8, A8), (B1, A1)];
        for (from, to) in cycle {
            assert!(board.make_move(&Play::new(from, to, None, None, false, false)));
        }
        assert!(board.has_repeated());
        for (from, to) in cycle {
            assert!(board.make_move(&Play::new(from, to, None, None, false, false)));
        }
        assert!(board.is_repetition());
    }

    #[test]
    fn moves_can_be_unmade_across_the_wrap() {
        let start = Board::from_fen(NEAR_THE_WRAP).unwrap();
        let mut board = start.clone();
        let cycle = [(A8, B8), (A1, B1), (B8, A8), (B1, A1)];
        for (from, to) in cycle {
            assert!(board.make_move(&Play::new(from, to, None, None, false, false)));
        }
        for _ in cycle {
            board.undo_move();
        }
        assert_eq!(board, start);
    }

    #[test]
    fn a_fifty_move_count_beyond_the_history_is_not_a_repetition() {
        // the fifty move counter from the fen is larger than the history we hold
        let board = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 99 1").unwrap();
        assert_eq!(board.is_repetition(), false);
    }

    #[test]
    fn has_repeated_fires_a_cycle_before_is_repetition() {
        let mut board = Board::from_fen(fens::SHUFFLE).unwrap();
        let cycle = [(A8, B8), (A1, B1), (B8, A8), (B1, A1)];
        assert_eq!(board.has_repeated(), false);

        for (from, to) in cycle {
            // and no false positives anywhere on the way round
            assert_eq!(board.is_repetition(), false);
            board.make_move(&Play::new(from, to, None, None, false, false));
        }
        // first repeat: a draw is available, so the search stops here
        assert_eq!(board.has_repeated(), true);
        assert_eq!(board.is_repetition(), false);

        for (from, to) in cycle {
            assert_eq!(board.is_repetition(), false);
            board.make_move(&Play::new(from, to, None, None, false, false));
        }
        // second repeat: the game is actually drawn
        assert_eq!(board.has_repeated(), true);
        assert_eq!(board.is_repetition(), true);
    }
}

#[cfg(test)]
mod null_move {
    use super::fens;
    use super::play_named;
    use super::{Accumulator, Board};
    use pretty_assertions::{assert_eq, assert_ne};

    #[test]
    fn a_pass_unmakes_back_to_the_position_it_left() {
        for fen in fens::CORE {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(board.in_check(), false, "{}", fen);
            let mut passed = board.clone();
            passed.make_null_move();
            assert_ne!(board, passed, "{}", fen);
            passed.undo_null_move();
            assert_eq!(board, passed, "{}", fen);
        }
    }

    /// The debug build asserts this inside the pass itself; this says it in
    /// a release build too.
    #[test]
    fn a_pass_leaves_the_derived_state_in_step() {
        for fen in fens::CORE {
            let mut board = Board::from_fen(fen).unwrap();
            board.make_null_move();
            assert_eq!(board.key, board.recompute_key(), "{}", fen);
            assert_eq!(board.eval, Accumulator::recomputed(&board), "{}", fen);
            assert_eq!(board.squares, board.recompute_squares(), "{}", fen);
            assert_eq!(board.checkers, board.recompute_checkers(), "{}", fen);
            assert_eq!(board.checkers, 0, "{}", fen);
        }
    }

    #[test]
    fn a_pass_runs_the_fifty_move_counter_on() {
        let mut board = Board::from_fen("4k3/8/8/8/8/8/8/R3K3 w - - 37 40").unwrap();
        assert_eq!(board.fifty_move_rule, 37);
        board.make_null_move();
        assert_eq!(board.fifty_move_rule, 38);
        board.undo_null_move();
        assert_eq!(board.fifty_move_rule, 37);
    }

    #[test]
    fn a_pass_clears_the_en_passant_square() {
        // white has just pushed d2-d4 past a black pawn on e4, so the square
        // is one black can take on and is in the key
        let board = Board::from_fen("rnbqkbnr/pppp1ppp/8/8/3Pp3/8/PPP1PPPP/RNBQKBNR b KQkq d3 0 3")
            .unwrap();
        assert!(board.en_passant.is_some());
        let mut passed = board.clone();
        passed.make_null_move();
        assert_eq!(passed.en_passant, None);
        passed.undo_null_move();
        assert_eq!(passed.en_passant, board.en_passant);
        assert_eq!(passed.key, board.key);
    }

    /// Coming back to the position a pass was made from is not a draw. One
    /// rook travels home in three moves and the other in two, which is what
    /// lets an odd number of plies undo a pass; the key assertion says an
    /// unsalted entry would have matched.
    #[test]
    fn a_pass_is_not_a_prior_occurrence_of_the_position_it_passed_from() {
        let mut board = Board::from_fen("r6k/8/8/8/8/8/8/R6K w - - 0 1").unwrap();
        let key = board.key;
        board.make_null_move();
        for name in ["a8a7", "a1a2", "a7a6", "a2a1", "a6a8"] {
            let play = play_named(&board, name);
            assert!(board.make_move(&play), "{} is not legal here", name);
        }
        assert_eq!(board.key, key, "the line did not come back to the position");
        assert_eq!(board.has_repeated(), false);
    }
}

#[cfg(test)]
mod castling_rights {
    use super::CastlePermissions;
    use super::{A1, A8, CASTLE_LANDING, CASTLE_LEAVING, E1, E8, H1, H8};
    use pretty_assertions::assert_eq;

    /// The rule the two tables stand for, written the way make_move wrote it
    /// before them, so the tables are held to a second statement of it.
    fn by_hand(rights: CastlePermissions, from: u8, to: u8) -> CastlePermissions {
        use CastlePermissions as C;
        let taken = match from {
            A1 => C::WHITE_QUEEN_SIDE,
            E1 => C::WHITE_QUEEN_SIDE | C::WHITE_KING_SIDE,
            H1 => C::WHITE_KING_SIDE,
            A8 => C::BLACK_QUEEN_SIDE,
            E8 => C::BLACK_QUEEN_SIDE | C::BLACK_KING_SIDE,
            H8 => C::BLACK_KING_SIDE,
            _ => 0,
        } | match to {
            A1 => C::WHITE_QUEEN_SIDE,
            H1 => C::WHITE_KING_SIDE,
            A8 => C::BLACK_QUEEN_SIDE,
            H8 => C::BLACK_KING_SIDE,
            _ => 0,
        };
        C::from_bits(rights.bits() & !taken)
    }

    #[test]
    fn the_tables_take_what_the_matches_took() {
        for held in 0..16u8 {
            let rights = CastlePermissions::from_bits(held);
            for from in 0..64u8 {
                for to in 0..64u8 {
                    let masked = CastlePermissions::from_bits(
                        rights.bits() & CASTLE_LEAVING[from as usize] & CASTLE_LANDING[to as usize],
                    );
                    assert_eq!(
                        masked.as_fen(),
                        by_hand(rights, from, to).as_fen(),
                        "{} from {} to {}",
                        rights.as_fen(),
                        from,
                        to
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod position_key {
    use super::Board;
    use pretty_assertions::{assert_eq, assert_ne};

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "en passant square the position does not bear out")]
    fn an_en_passant_square_no_pawn_can_take_fails_the_state_check() {
        use super::{Coordinate, ZOBRIST};
        let mut board = Board::new();
        // e6 is out of reach of every white pawn at the start. The bogus
        // square is hashed into the key as well, so the key still matches
        // its recompute and only the rule itself can object
        board.en_passant = Coordinate::from_string("e6").unwrap();
        board.key ^= ZOBRIST.en_passant_key(board.en_passant.unwrap().as_index());
        board.debug_assert_state_in_step();
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "a castle right without the king and rook for it")]
    fn a_castle_right_without_its_rook_fails_the_state_check() {
        use super::ZOBRIST;
        let mut board = Board::from_fen("4k3/8/8/8/8/8/8/4K3 w - - 0 1").unwrap();
        // folded into the key as well, for the reason above
        let without = board.castle;
        board.castle = without.with(super::CastlePermissions::WHITE_KING_SIDE);
        board.key ^= ZOBRIST.castle_key(without) ^ ZOBRIST.castle_key(board.castle);
        board.debug_assert_state_in_step();
    }

    fn play_move(board: &mut Board, name: &str) {
        let play = super::play_named(board, name);
        assert!(board.make_move(&play), "failed to play {}", name);
    }

    #[test]
    fn castle_rights_change_key() {
        let all = Board::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1").unwrap();
        let none = Board::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w - - 0 1").unwrap();
        let white_only = Board::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w KQ - 0 1").unwrap();
        assert_ne!(all.key, none.key);
        assert_ne!(all.key, white_only.key);
        assert_ne!(none.key, white_only.key);
    }

    #[test]
    fn en_passant_changes_key_when_a_pawn_can_take_there() {
        // a black pawn on d4 takes on e3, so the square is real and belongs in
        // the key
        let without =
            Board::from_fen("rnbqkbnr/ppp1pppp/8/8/3pP3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1").unwrap();
        let with = Board::from_fen("rnbqkbnr/ppp1pppp/8/8/3pP3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1")
            .unwrap();
        assert_ne!(without.key, with.key);
        assert!(with.en_passant.is_some());
    }

    #[test]
    fn en_passant_no_one_can_take_is_not_in_the_key() {
        // every black pawn is still on the seventh, so nothing can take on e3
        let without =
            Board::from_fen("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1").unwrap();
        let with =
            Board::from_fen("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1").unwrap();
        assert_eq!(without.key, with.key);
        assert_eq!(with.en_passant, None);
    }

    #[test]
    fn a_double_push_no_one_can_answer_hashes_like_the_position_without_it() {
        // the same position played and parsed has to hash alike
        let mut played = Board::from_fen("4k3/7p/8/8/8/8/P7/4K3 w - - 0 1").unwrap();
        let a2a4 = super::play_named(&played, "a2a4");
        assert!(played.make_move(&a2a4));
        let parsed = Board::from_fen("4k3/7p/8/8/P7/8/8/4K3 b - - 0 1").unwrap();
        assert_eq!(played.key, parsed.key);
        assert_eq!(played.en_passant, None);
    }

    #[test]
    fn a_double_push_which_can_be_answered_still_records_the_square() {
        let mut played = Board::from_fen("4k3/8/8/8/1p6/8/P7/4K3 w - - 0 1").unwrap();
        let a2a4 = super::play_named(&played, "a2a4");
        assert!(played.make_move(&a2a4));
        let parsed = Board::from_fen("4k3/8/8/8/Pp6/8/8/4K3 b - a3 0 1").unwrap();
        assert_eq!(played.key, parsed.key);
        assert!(played.en_passant.is_some());
    }

    #[test]
    fn active_color_changes_key() {
        let white = Board::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w - - 0 1").unwrap();
        let black = Board::from_fen("r3k2r/8/8/8/8/8/8/R3K2R b - - 0 1").unwrap();
        assert_ne!(white.key, black.key);
    }

    #[test]
    fn key_matches_fen_after_moves() {
        let mut board = Board::new();

        play_move(&mut board, "e2e4");
        let fen =
            Board::from_fen("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1").unwrap();
        assert_eq!(board.key, fen.key);

        // the en passant right expires on the reply (this used to leave a
        // stale en passant key)
        play_move(&mut board, "g8f6");
        let fen = Board::from_fen("rnbqkb1r/pppppppp/5n2/8/4P3/8/PPPP1PPP/RNBQKBNR w KQkq - 1 2")
            .unwrap();
        assert_eq!(board.key, fen.key);

        // moving the king drops white's castle rights
        play_move(&mut board, "e1e2");
        let fen =
            Board::from_fen("rnbqkb1r/pppppppp/5n2/8/4P3/8/PPPPKPPP/RNBQ1BNR b kq - 2 2").unwrap();
        assert_eq!(board.key, fen.key);
        // the same pieces with black's rights gone too is a different
        // position. A fen claiming the rights white gave up cannot make the
        // other half of the point, since from_fen drops them again
        let none =
            Board::from_fen("rnbqkb1r/pppppppp/5n2/8/4P3/8/PPPPKPPP/RNBQ1BNR b - - 2 2").unwrap();
        assert_ne!(board.key, none.key);
    }

    #[test]
    fn key_is_path_independent() {
        let mut a = Board::new();
        for m in ["e2e4", "d7d5", "g1f3", "b8c6"] {
            play_move(&mut a, m);
        }
        let mut b = Board::new();
        for m in ["g1f3", "d7d5", "e2e4", "b8c6"] {
            play_move(&mut b, m);
        }
        // both lines end with a knight move, so any en passant right on the
        // way has expired
        assert_eq!(a.key, b.key);
    }
}

#[cfg(test)]
mod pawn_key {
    use super::{Board, fens, play_named};
    use pretty_assertions::{assert_eq, assert_ne};

    fn play_move(board: &mut Board, name: &str) {
        let play = play_named(board, name);
        assert!(board.make_move(&play), "failed to play {}", name);
    }

    /// One position and one move for each way a pawn can appear, disappear
    /// or travel.
    #[test]
    fn every_pawn_event_moves_the_key_and_unmakes_back_to_it() {
        let cases = [
            ("4k3/8/8/8/8/8/4P3/4K3 w - - 0 1", "e2e3"),
            ("4k3/8/8/8/8/8/4P3/4K3 w - - 0 1", "e2e4"),
            // a pawn taking a piece, and a piece taking a pawn
            ("4k3/8/8/8/8/3n4/4P3/4K3 w - - 0 1", "e2d3"),
            ("4k3/8/8/8/3N4/8/4p3/4K3 w - - 0 1", "d4e2"),
            // en passant, where the pawn taken stands behind the square
            // landed on
            ("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6"),
            // a promotion takes a pawn out of the key and puts nothing back
            ("4k3/P7/8/8/8/8/8/4K3 w - - 0 1", "a7a8q"),
            ("4k3/P7/8/8/8/8/8/4K3 w - - 0 1", "a7a8n"),
            ("1n2k3/P7/8/8/8/8/8/4K3 w - - 0 1", "a7b8q"),
        ];
        for (fen, name) in cases {
            let board = Board::from_fen(fen).unwrap();
            let mut played = board.clone();
            play_move(&mut played, name);
            assert_ne!(board.pawn_key, played.pawn_key, "{} in {}", name, fen);
            assert_eq!(
                played.pawn_key,
                played.recompute_pawn_key(),
                "{} in {}",
                name,
                fen
            );
            played.undo_move();
            assert_eq!(board.pawn_key, played.pawn_key, "{} in {}", name, fen);
        }
    }

    #[test]
    fn a_move_that_touches_no_pawn_leaves_the_key_alone() {
        let board = Board::from_fen(fens::MIDDLEGAME).unwrap();
        let mut played = board.clone();
        play_move(&mut played, "c3d5");
        assert_ne!(board.key, played.key);
        assert_eq!(board.pawn_key, played.pawn_key);
    }

    /// The debug build asserts this inside make_move; this says it in a
    /// release build too.
    #[test]
    fn the_key_survives_every_move_of_every_position() {
        for fen in fens::CORE {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(board.pawn_key, board.recompute_pawn_key(), "{}", fen);
            for m in &board.generate_moves() {
                let mut played = board.clone();
                if played.make_move(m) {
                    assert_eq!(
                        played.pawn_key,
                        played.recompute_pawn_key(),
                        "{} in {}",
                        m,
                        fen
                    );
                    played.undo_move();
                    assert_eq!(board.pawn_key, played.pawn_key, "{} in {}", m, fen);
                }
            }
        }
    }

    #[test]
    fn a_pass_leaves_the_key_alone() {
        for fen in fens::CORE {
            let board = Board::from_fen(fen).unwrap();
            let mut passed = board.clone();
            passed.make_null_move();
            assert_eq!(passed.pawn_key, board.pawn_key, "{}", fen);
            assert_eq!(passed.pawn_key, passed.recompute_pawn_key(), "{}", fen);
            passed.undo_null_move();
            assert_eq!(passed.pawn_key, board.pawn_key, "{}", fen);
        }
    }

    #[test]
    fn the_same_pawns_behind_different_pieces_share_a_key() {
        let bare = Board::from_fen("4k3/pppppppp/8/8/8/8/PPPPPPPP/4K3 w - - 0 1").unwrap();
        let full = Board::from_fen(fens::START).unwrap();
        assert_eq!(bare.pawn_key, full.pawn_key);
        assert_ne!(bare.key, full.key);
    }

    #[test]
    fn nothing_but_the_pawns_is_in_the_key() {
        let white = Board::from_fen("r3k2r/pppppppp/8/8/8/8/PPPPPPPP/R3K2R w KQkq - 0 1").unwrap();
        let black = Board::from_fen("r3k2r/pppppppp/8/8/8/8/PPPPPPPP/R3K2R b KQkq - 0 1").unwrap();
        let no_rights = Board::from_fen("r3k2r/pppppppp/8/8/8/8/PPPPPPPP/R3K2R w - - 0 1").unwrap();
        assert_ne!(white.key, black.key);
        assert_ne!(white.key, no_rights.key);
        assert_eq!(white.pawn_key, black.pawn_key);
        assert_eq!(white.pawn_key, no_rights.pawn_key);

        // a black pawn on d4 takes on e3, so the square is one from_fen keeps
        let without =
            Board::from_fen("rnbqkbnr/ppp1pppp/8/8/3pP3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1").unwrap();
        let with = Board::from_fen("rnbqkbnr/ppp1pppp/8/8/3pP3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1")
            .unwrap();
        assert_ne!(without.key, with.key);
        assert_eq!(without.pawn_key, with.pawn_key);
    }

    #[test]
    fn a_played_position_and_a_parsed_one_agree() {
        let mut board = Board::new();
        for m in ["e2e4", "d7d5", "e4d5", "d8d5", "b1c3", "d5a5"] {
            play_move(&mut board, m);
        }
        let parsed = Board::from_fen(&board.to_fen()).unwrap();
        assert_eq!(board.pawn_key, parsed.pawn_key);
        assert_ne!(board.pawn_key, Board::new().pawn_key);
    }

    #[test]
    fn a_board_with_no_pawns_has_no_key() {
        let board = Board::from_fen("4k3/8/8/8/8/8/8/4K3 w - - 0 1").unwrap();
        assert_eq!(board.pawn_key, 0);
    }
}

#[cfg(test)]
mod perft {
    use super::{Board, fens};
    use pretty_assertions::assert_eq;

    /// The six standard positions, with their counts from depth one up to the
    /// depth the suite can afford. Positions and results taken from
    /// https://www.chessprogramming.org/Perft_Results
    const CASES: [(&str, &str, &[u64]); 6] = [
        ("the starting position", fens::START, &[20, 400, 8902]),
        (
            "position 2, kiwipete",
            fens::KIWIPETE,
            &[48, 2039, 97_862, 4_085_603],
        ),
        (
            "position 3",
            fens::PAWN_ENDGAME,
            &[14, 191, 2812, 43_238, 674_624, 11_030_083],
        ),
        (
            "position 4",
            "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
            &[6, 264, 9467, 422_333, 15_833_292],
        ),
        (
            "position 5",
            fens::PROMOTIONS,
            &[44, 1486, 62_379, 2_103_487],
        ),
        (
            "position 6",
            fens::MIDDLEGAME,
            &[46, 2079, 89_890, 3_894_594],
        ),
    ];

    #[test]
    fn the_standard_positions_count_exactly() {
        for (description, fen, counts) in CASES {
            let mut board = Board::from_fen(fen).unwrap();
            for (i, &expected) in counts.iter().enumerate() {
                let depth = i as u8 + 1;
                assert_eq!(
                    board.perft(depth),
                    expected,
                    "{} at depth {}",
                    description,
                    depth
                );
            }
        }
    }

    /// See `perft_through_evasions`.
    #[test]
    fn the_standard_positions_count_the_same_through_evasions() {
        for (description, fen, counts) in CASES {
            let mut board = Board::from_fen(fen).unwrap();
            for (i, &expected) in counts.iter().enumerate() {
                let depth = i as u8 + 1;
                assert_eq!(
                    board.perft_through_evasions(depth),
                    expected,
                    "{} at depth {}, through evasions",
                    description,
                    depth
                );
            }
        }
    }

    /// See `perft_as_played`.
    #[test]
    fn the_standard_positions_count_the_same_as_played() {
        for (description, fen, counts) in CASES {
            let mut board = Board::from_fen(fen).unwrap();
            for (i, &expected) in counts.iter().enumerate() {
                let depth = i as u8 + 1;
                assert_eq!(
                    board.perft_as_played(depth),
                    expected,
                    "{} at depth {}, as played",
                    description,
                    depth
                );
            }
        }
    }
}

#[cfg(test)]
mod in_step {
    use super::{Accumulator, Board, Color};
    use proptest::prelude::*;

    /// Everything a board has to satisfy, whether it was played to or
    /// parsed: the recomputes `debug_assert_state_in_step` checks, and the
    /// structural facts make_move never breaks but a parsed string can.
    pub(super) fn prop_assert_in_step(board: &Board) -> Result<(), TestCaseError> {
        let pieces = board.pieces;
        for (i, a) in pieces.iter().enumerate() {
            for b in &pieces[i + 1..] {
                prop_assert_eq!(a & b, 0u64, "a square holds two piece types");
            }
        }
        prop_assert_eq!(board.white & board.black, 0u64, "a square is both colours");
        prop_assert_eq!(
            board.white | board.black,
            pieces.iter().fold(0u64, |all, bb| all | bb),
            "the colour boards and the piece boards disagree"
        );
        for color in [Color::White, Color::Black] {
            let mask = match color {
                Color::White => board.white,
                Color::Black => board.black,
            };
            prop_assert_eq!(
                (board.kings() & mask).count_ones(),
                1,
                "not exactly one king of one colour"
            );
        }
        prop_assert_eq!(
            board.key,
            board.recompute_key(),
            "the key is not this position"
        );
        prop_assert_eq!(
            board.pawn_key,
            board.recompute_pawn_key(),
            "the pawn key is not this structure"
        );
        prop_assert_eq!(
            board.eval,
            Accumulator::recomputed(board),
            "the evaluation drifted"
        );
        prop_assert_eq!(
            board.checkers,
            board.recompute_checkers(),
            "the checkers drifted"
        );
        prop_assert_eq!(
            board.squares,
            board.recompute_squares(),
            "the squares drifted"
        );
        prop_assert!(
            !board.square_attacked(board.king_index(!board.active_color), board.active_color),
            "the side not to move is in check"
        );
        Ok(())
    }
}
#[cfg(test)]
mod fen_parsing {
    use super::{Board, fens};
    use proptest::prelude::*;

    /// A well formed placement field: eight ranks of eight squares with one
    /// king a side. Everything else is random, pawns on the back rank
    /// included, because from_fen accepts those and the search has to
    /// survive one.
    fn placement() -> impl Strategy<Value = String> {
        (
            prop::collection::vec(
                prop_oneof![
                    24 => Just(None),
                    2 => Just(Some('p')),
                    2 => Just(Some('P')),
                    1 => Just(Some('n')),
                    1 => Just(Some('N')),
                    1 => Just(Some('b')),
                    1 => Just(Some('B')),
                    1 => Just(Some('r')),
                    1 => Just(Some('R')),
                    1 => Just(Some('q')),
                    1 => Just(Some('Q')),
                ],
                64,
            ),
            any::<prop::sample::Index>(),
            any::<prop::sample::Index>(),
        )
            .prop_map(|(mut squares, white, black)| {
                // the kings go in last, so nothing above can add a second
                let white = white.index(64);
                let mut black = black.index(64);
                if black == white {
                    black = (black + 1) % 64;
                }
                squares[white] = Some('K');
                squares[black] = Some('k');
                render(&squares)
            })
    }

    /// Sixty four squares as the eight rank strings of a fen.
    fn render(squares: &[Option<char>]) -> String {
        let mut ranks = Vec::with_capacity(8);
        for rank in squares.chunks(8) {
            let mut text = String::new();
            let mut empty = 0;
            for square in rank {
                match square {
                    Some(piece) => {
                        if empty > 0 {
                            text.push_str(&empty.to_string());
                            empty = 0;
                        }
                        text.push(*piece);
                    }
                    None => empty += 1,
                }
            }
            if empty > 0 {
                text.push_str(&empty.to_string());
            }
            ranks.push(text);
        }
        ranks.join("/")
    }

    /// A fen the parser will usually accept, which is what reaches the
    /// coherence checks below; the gives_check oracle borrows it for
    /// positions nobody thought to write down.
    pub(super) fn well_formed_fen() -> impl Strategy<Value = String> {
        (
            placement(),
            prop_oneof![Just("w"), Just("b")],
            prop_oneof![Just("-"), Just("KQkq"), Just("Kq"), Just("Q"), Just("kq")],
            prop_oneof![Just("-"), Just("e3"), Just("e6"), Just("a3"), Just("h6")],
            prop_oneof![Just("0"), Just("50"), Just("99")],
            prop_oneof![Just("1"), Just("40")],
        )
            .prop_map(|(placement, side, castle, en_passant, half, full)| {
                format!(
                    "{} {} {} {} {} {}",
                    placement, side, castle, en_passant, half, full
                )
            })
    }

    /// One rank built of plausible parts that almost never sum to eight.
    fn rank() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                Just("p"),
                Just("k"),
                Just("K"),
                Just("Q"),
                Just("1"),
                Just("4"),
                Just("8"),
                Just("0"),
                Just("9"),
                Just("x"),
            ],
            1..9usize,
        )
        .prop_map(|parts| parts.concat())
    }

    /// A fen whose every field can be wrong in a different way, for the
    /// refusal path.
    fn malformed_fen() -> impl Strategy<Value = String> {
        (
            prop::collection::vec(rank(), 1..10usize),
            prop_oneof![Just("w"), Just("b"), Just("W"), Just("-")],
            prop_oneof![Just("-"), Just("KQkq"), Just("QQ"), Just("KQkqA")],
            prop_oneof![Just("-"), Just("e3"), Just("e9"), Just("i3"), Just("ee")],
            prop_oneof![
                Just("0"),
                Just("-1"),
                Just("99999999999999999999"),
                Just("x")
            ],
            prop_oneof![Just("1"), Just("-3"), Just("y")],
        )
            .prop_map(|(ranks, side, castle, en_passant, half, full)| {
                format!(
                    "{} {} {} {} {} {}",
                    ranks.join("/"),
                    side,
                    castle,
                    en_passant,
                    half,
                    full
                )
            })
    }

    /// A fen that was valid until one edit landed on it, which is nearer to
    /// what a buggy interface sends than anything assembled from parts.
    fn mutated_fen() -> impl Strategy<Value = String> {
        (
            prop::sample::select(&fens::CORE[..]),
            any::<prop::sample::Index>(),
            0u8..4,
        )
            .prop_map(|(fen, index, how)| {
                // every fen here is ascii, so any index is a char boundary
                let mut fen = fen.to_string();
                let at = index.index(fen.len());
                match how {
                    0 => {
                        fen.remove(at);
                        fen
                    }
                    1 => fen[..at].to_string(),
                    2 => {
                        fen.insert(at, 'Z');
                        fen
                    }
                    _ => format!("{} {}", fen, &fen[..at]),
                }
            })
    }

    /// The coherence tests say something only about the fens that are
    /// accepted, so a generator that stopped producing any would leave them
    /// passing vacuously. About a third get through (the rest leave the side
    /// not to move in check), so a floor of a tenth catches a generator that
    /// has collapsed, not one that drifted by a few per cent.
    #[test]
    fn the_generator_reaches_the_parser() {
        use proptest::strategy::ValueTree;
        use proptest::test_runner::TestRunner;
        let mut runner = TestRunner::deterministic();
        let strategy = well_formed_fen();
        let accepted = (0..200)
            .filter(|_| {
                let fen = strategy.new_tree(&mut runner).unwrap().current();
                Board::from_fen(&fen).is_ok()
            })
            .count();
        assert!(
            accepted > 20,
            "only {} of 200 generated fens parsed, so the coherence tests are close to vacuous",
            accepted
        );
    }

    /// No fen here has anything the parser may cut back, so the text comes
    /// back word for word too.
    #[test]
    fn a_printed_position_parses_back_to_itself() {
        for fen in [
            fens::START,
            fens::KIWIPETE,
            fens::PAWN_ENDGAME,
            fens::PROMOTIONS,
            fens::MIDDLEGAME,
            fens::SHUFFLE,
        ] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(board.to_fen(), fen);
            assert_eq!(Board::from_fen(&board.to_fen()).unwrap(), board, "{}", fen);
        }
    }

    /// The residual sampler needs a position it prints to be scored for the
    /// fifty move rule the way the search that printed it scored it.
    #[test]
    fn the_clocks_travel_with_the_position() {
        let fen = "8/8/4k3/8/8/4K3/8/6R1 w - - 83 62";
        let board = Board::from_fen(fen).unwrap();
        assert_eq!(board.to_fen(), fen);
    }

    proptest! {
        #[test]
        fn random_str_doesnt_crash(s in ".*") {
            _ = Board::from_fen(&s);
        }

        /// The first print of a parsed fen need not match the text that
        /// arrived, since the parser cuts back what the pieces do not bear
        /// out. Printing it again does.
        #[test]
        fn printing_a_parsed_position_is_settled_after_one_pass(fen in well_formed_fen()) {
            if let Ok(board) = Board::from_fen(&fen) {
                let printed = board.to_fen();
                let parsed = Board::from_fen(&printed).expect("what we print, we parse");
                prop_assert_eq!(parsed.to_fen(), printed);
                prop_assert_eq!(parsed.key, board.key);
            }
        }

        /// A refusal is always allowed; accepting a board that is not in
        /// step is not, since the search trusts everything from_fen hands
        /// it.
        #[test]
        fn a_well_formed_fen_is_refused_or_coherent(fen in well_formed_fen()) {
            if let Ok(board) = Board::from_fen(&fen) {
                super::in_step::prop_assert_in_step(&board)?;
            }
        }

        #[test]
        fn a_malformed_fen_is_refused_or_coherent(fen in malformed_fen()) {
            if let Ok(board) = Board::from_fen(&fen) {
                super::in_step::prop_assert_in_step(&board)?;
            }
        }

        #[test]
        fn a_mutated_fen_is_refused_or_coherent(fen in mutated_fen()) {
            if let Ok(board) = Board::from_fen(&fen) {
                super::in_step::prop_assert_in_step(&board)?;
            }
        }

        /// A castle right or an en passant square the position does not
        /// agree with parses into a coherent board and corrupts it one move
        /// later, so every legal move is played and the board asked again.
        #[test]
        fn every_legal_move_of_a_well_formed_fen_leaves_the_board_in_step(fen in well_formed_fen()) {
            if let Ok(mut board) = Board::from_fen(&fen) {
                let before = board.clone();
                for m in &board.generate_moves() {
                    if board.make_move(m) {
                        super::in_step::prop_assert_in_step(&board)?;
                        board.undo_move();
                    }
                    // not prop_assert_eq, which would print two boards and
                    // the thousand plies of history each carries
                    prop_assert!(board == before, "{} did not unmake", m);
                }
            }
        }
    }

    /// The rows `rights_the_pieces_bear_out` is there for, the first being
    /// the position that found it.
    #[test]
    fn a_castle_right_without_the_king_and_rook_for_it_is_dropped() {
        for (fen, left, why) in [
            (
                "3bn2B/Q7/P1R4K/7b/B5k1/PP1q2B1/Rb1P3R/1Prpq3 b KQkq - 51 1",
                "-",
                "neither king stands on its square",
            ),
            (
                "1r2k2r/8/8/8/8/8/8/R3K1R1 w KQkq - 0 1",
                "Qk",
                "a rook stands beside its corner rather than on it",
            ),
            (
                "R2pkb1R/8/8/8/8/8/8/4K3 w kq - 0 1",
                "-",
                "the rooks on the corners are the other colour's",
            ),
            (
                "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1",
                "KQkq",
                "every right has the pieces for it",
            ),
        ] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(board.castle.as_fen(), left, "{}: {}", why, fen);
        }
    }

    /// The parser once asked only whether anything could capture there, and
    /// a square with no pawn behind it produced a capture whose make_move
    /// cleared a pawn from a square holding something else.
    #[test]
    fn an_en_passant_square_the_position_does_not_bear_out_is_dropped() {
        for (fen, why) in [
            (
                "4k3/8/8/3Pr3/8/8/8/4K3 w - e6 0 1",
                "a rook stands where the taken pawn should",
            ),
            (
                "4k3/8/8/3P4/8/8/8/4K3 w - e6 0 1",
                "nothing stands where the taken pawn should",
            ),
            (
                "4k3/8/8/3PP3/8/8/8/4K3 w - e6 0 1",
                "the pawn behind the square is our own",
            ),
            (
                "4k3/8/4b3/3Pp3/8/8/8/4K3 w - e6 0 1",
                "the square itself is occupied",
            ),
            (
                "4k3/8/4P3/3Pp3/8/8/8/4K3 w - e6 0 1",
                "the square holds a piece of ours",
            ),
            (
                "4k3/8/8/8/3p4/8/8/4K3 b - e3 0 1",
                "black to move and nothing to take behind the square",
            ),
            (
                "4k3/8/8/8/8/8/Pp6/4K3 b - a1 0 1",
                "no double push crosses the first rank",
            ),
            ("4k3/pP6/8/8/8/8/8/4K3 w - a8 0 1", "nor the eighth"),
        ] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(board.en_passant, None, "{}: {}", why, fen);
            assert!(
                !board.generate_moves().iter().any(|m| m.en_passant),
                "{}: {}",
                why,
                fen
            );
        }
    }

    #[test]
    fn an_en_passant_square_with_the_pawn_behind_it_is_kept() {
        let board = Board::from_fen("rnbqkbnr/ppp1pppp/8/8/3pP3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1")
            .unwrap();
        assert_eq!(board.to_fen().split(' ').nth(3), Some("e3"));
        assert!(board.generate_moves().iter().any(|m| m.en_passant));
    }
    #[test]
    fn the_wikipedia_examples_parse() -> Result<(), String> {
        Board::from_fen("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1")?;
        Board::from_fen("rnbqkbnr/pp1ppppp/8/2p5/4P3/8/PPPP1PPP/RNBQKBNR w KQkq c6 0 2")?;
        Board::from_fen("rnbqkbnr/pp1ppppp/8/2p5/4P3/5N2/PPPP1PPP/RNBQKB1R b KQkq - 1 2")?;
        Ok(())
    }

    #[test]
    fn too_many_ranks_are_rejected() {
        assert!(
            Board::from_fen("rnbqkbnr/pppppppp/8/8/8/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1")
                .is_err()
        );
    }
    #[test]
    fn a_doubled_slash_is_rejected() {
        assert!(
            Board::from_fen("rnbqkbnr/pppppppp/8/8//4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1")
                .is_err()
        );
    }
    /// A ninth file used to wrap back onto the a file and corrupt the square
    /// it landed on, rather than fail to parse.
    #[test]
    fn a_ninth_file_is_rejected() {
        assert!(
            Board::from_fen("rnbqkbnr/ppppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1")
                .is_err()
        );
        assert!(
            Board::from_fen("rnbqkbnr/pppppppp/45/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1")
                .is_err(),
            "digits which sum past the h file are the same mistake"
        );
    }
    #[test]
    fn an_unknown_piece_letter_is_rejected() {
        assert!(
            Board::from_fen("rnbqkbnar/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1")
                .is_err()
        );
    }

    /// Each of these parsed and then took the engine down on the first
    /// search.
    #[test]
    fn a_position_which_could_not_arise_is_rejected_rather_than_searched() {
        for (fen, why) in [
            ("8/8/8/8/8/8/8/8 w - - 0 1", "no kings at all"),
            ("4k3/8/8/8/8/8/8/8 w - - 0 1", "no white king"),
            ("8/8/8/8/8/8/8/4K3 w - - 0 1", "no black king"),
            ("4k2k/8/8/8/8/8/8/4K3 w - - 0 1", "two black kings"),
            (
                "4k3/8/8/8/8/8/8/4R1K1 w - - 0 1",
                "black is in check with white to move, so white takes the king",
            ),
        ] {
            assert!(Board::from_fen(fen).is_err(), "{}: {}", why, fen);
        }
    }

    #[test]
    fn a_position_where_the_side_to_move_is_in_check_is_accepted() {
        assert!(Board::from_fen("4k3/8/8/8/8/8/8/4R1K1 b - - 0 1").is_ok());
    }
}

#[cfg(test)]
mod perft_edge_cases {
    use super::{Board, fens};
    use pretty_assertions::assert_eq;

    /// Shapes the six standard perft positions do not reach. Each count was
    /// checked against python-chess rather than transcribed. The last four
    /// put the shapes the evasion mask has a rule for at the root: a double
    /// check with pieces able to take a checker, an en passant capture
    /// answering a check, and a promotion that does.
    const CASES: [(&str, u8, u64, &str); 28] = [
        (
            "3k4/3p4/8/K1P4r/8/8/8/8 b - - 0 1",
            6,
            1_134_888,
            "en passant capture is pinned along the rank",
        ),
        (
            "8/8/4k3/8/2p5/8/B2P2K1/8 w - - 0 1",
            6,
            1_015_133,
            "en passant capture is pinned along the diagonal",
        ),
        (
            "8/8/1k6/2b5/2pP4/8/5K2/8 b - d3 0 1",
            6,
            1_440_467,
            "en passant capture gives check",
        ),
        (
            "5k2/8/8/8/8/8/8/4K2R w K - 0 1",
            6,
            661_072,
            "castling short gives check",
        ),
        (
            "3k4/8/8/8/8/8/8/R3K3 w Q - 0 1",
            6,
            803_711,
            "castling long gives check",
        ),
        (
            "r3k2r/1b4bq/8/8/8/8/7B/R3K2R w KQkq - 0 1",
            4,
            1_274_206,
            "castling rights are given up correctly",
        ),
        (
            "r3k2r/8/3Q4/8/8/5q2/8/R3K2R b KQkq - 0 1",
            4,
            1_720_476,
            "castling is prevented by attacked squares",
        ),
        (
            "2K2r2/4P3/8/8/8/8/8/3k4 w - - 0 1",
            6,
            3_821_001,
            "promoting gets out of check",
        ),
        (
            "8/8/1P2K3/8/2n5/1q6/8/5k2 b - - 0 1",
            5,
            1_004_658,
            "discovered check",
        ),
        (
            "4k3/1P6/8/8/8/8/K7/8 w - - 0 1",
            6,
            217_342,
            "promoting gives check",
        ),
        (
            "8/P1k5/K7/8/8/8/8/8 w - - 0 1",
            6,
            92_683,
            "underpromoting gives check",
        ),
        (
            "K1k5/8/P7/8/8/8/8/8 w - - 0 1",
            6,
            2_217,
            "stalemating ourselves",
        ),
        (
            "8/k1P5/8/1K6/8/8/8/8 w - - 0 1",
            7,
            567_584,
            "stalemate and checkmate",
        ),
        (
            "8/8/2k5/5q2/5n2/8/5K2/8 b - - 0 1",
            4,
            23_527,
            "stalemate and checkmate again",
        ),
        (
            fens::START,
            5,
            4_865_609,
            "the start, deeper than the other suite goes",
        ),
        (
            "n1n5/PPPk4/8/8/8/8/4Kppp/5N1N w - - 0 1",
            5,
            3_605_103,
            "promotions of every piece for both sides",
        ),
        (
            "8/8/8/3k4/8/3K4/8/8 w - - 0 1",
            1,
            5,
            "the kings may not stand next to each other",
        ),
        (
            "r6r/1b2k1bq/8/8/7B/8/8/R3K2R b KQ - 3 2",
            1,
            8,
            "moving into check is not legal",
        ),
        (
            // the bishop behind the vacated square misses the king, so the
            // capture stands
            "8/8/8/2k5/2pP4/8/B7/4K3 b - d3 0 3",
            1,
            8,
            "en passant takes the checker with a bishop behind the capturer",
        ),
        (
            "r1bqkbnr/pppppppp/n7/8/8/P7/1PPPPPPP/RNBQKBNR w KQkq - 2 2",
            1,
            19,
            "a quiet position, move count only",
        ),
        (
            "r3k2r/p1pp1pb1/bn2Qnp1/2qPN3/1p2P3/2N5/PPPBBPPP/R3K2R b KQkq - 3 2",
            1,
            5,
            "only check evasions are legal",
        ),
        (
            "2kr3r/p1ppqpb1/bn2Qnp1/3PN3/1p2P3/2N5/PPPBBPPP/R3K2R b KQ - 3 2",
            1,
            44,
            "not in check despite the queen",
        ),
        (
            "rnb2k1r/pp1Pbppp/2p5/q7/2B5/8/PPPQNnPP/RNB1K2R w KQ - 3 9",
            1,
            39,
            "castling with a knight on f2",
        ),
        (
            // the captured pawn's square blocked the bishop's diagonal, and
            // the capturer starts on no line through the king at all
            "1b5k/8/8/3Pp3/8/8/7K/8 w - e6 0 2",
            1,
            6,
            "en passant uncovers a bishop on a diagonal the capturer never stood on",
        ),
        (
            // the knight and the rook check together; the queen could take
            // the rook and the knight the knight, each answering one check
            // and not the other, so only the king may move
            "r3k3/8/8/8/8/5n2/3Nr3/R2QK2R w Qq - 0 1",
            5,
            1_381_393,
            "double check with a piece able to take each checker",
        ),
        (
            // the bishop and the knight check together; the queen could
            // take the knight and the pawn the bishop
            "r2qk2r/8/p4N2/1B6/8/8/8/4K3 b kq - 0 1",
            5,
            1_014_262,
            "double check against black",
        ),
        (
            // the pawn giving check is the one taken en passant, from
            // either side of it
            "4k3/8/1n6/2pPpP2/5K2/8/1R6/8 w - e6 0 1",
            5,
            447_179,
            "en passant takes the checker",
        ),
        (
            // the rook checks along the rank; promoting on a1 takes it and
            // promoting on b1 blocks it. the white version is "promoting
            // gets out of check" above
            "7K/6P1/8/8/8/8/1p6/R3k3 b - - 0 1",
            5,
            306_360,
            "promotion by black takes or blocks the checker",
        ),
    ];

    #[test]
    fn every_edge_case_counts_exactly() {
        for (fen, depth, expected, description) in CASES {
            let mut board = Board::from_fen(fen).unwrap();
            assert_eq!(
                board.perft(depth),
                expected,
                "{} ({} at depth {})",
                description,
                fen,
                depth
            );
        }
    }

    /// The shapes most likely to catch the evasion mask out: promotions that
    /// answer a check, the en passant captures the mask does not examine,
    /// and pins that leave a move looking like an answer.
    #[test]
    fn every_edge_case_counts_the_same_through_evasions() {
        for (fen, depth, expected, description) in CASES {
            let mut board = Board::from_fen(fen).unwrap();
            assert_eq!(
                board.perft_through_evasions(depth),
                expected,
                "{} ({} at depth {}), through evasions",
                description,
                fen,
                depth
            );
        }
    }

    /// The shapes most likely to catch checkers maintenance out.
    #[test]
    fn every_edge_case_counts_the_same_as_played() {
        for (fen, depth, expected, description) in CASES {
            let mut board = Board::from_fen(fen).unwrap();
            assert_eq!(
                board.perft_as_played(depth),
                expected,
                "{} ({} at depth {}), as played",
                description,
                fen,
                depth
            );
        }
    }
}

#[cfg(test)]
mod pseudo_legal {
    use super::fens;
    use super::{Board, Play};
    use crate::misc::Piece;

    /// "d4" to the index the board uses.
    fn sq(name: &str) -> u8 {
        let mut c = name.chars();
        let file = c.next().unwrap() as u8 - b'a';
        let rank = c.next().unwrap() as u8 - b'1';
        rank * 8 + file
    }

    fn quiet(from: &str, to: &str) -> Play {
        Play::new(sq(from), sq(to), None, None, false, false)
    }

    fn takes(from: &str, to: &str, piece: Piece) -> Play {
        Play::new(sq(from), sq(to), Some(piece), None, false, false)
    }

    /// A middlegame where no move can castle, take en passant or promote.
    const OPPOSITE_WINGS: &str =
        "r2q1rk1/1b1nbppp/p2ppn2/1p6/3NPP2/1BN1B3/PPPQ2PP/2KR3R w - - 0 13";

    const POSITIONS: [&str; 5] = [
        fens::START,
        fens::KIWIPETE,
        fens::PAWN_ENDGAME,
        fens::PROMOTIONS,
        OPPOSITE_WINGS,
    ];

    /// A move refused here is one the search has to find again the slow
    /// way; this pins that the three refused kinds are the only ones.
    #[test]
    fn accepts_every_generated_move_but_the_refused_kinds() {
        for fen in POSITIONS {
            let board = Board::from_fen(fen).unwrap();
            for m in &board.generate_moves() {
                let refused_kind = m.castle || m.en_passant || m.promote.is_some();
                assert_eq!(board.is_pseudo_legal(m), !refused_kind, "{} in {}", m, fen);
            }
        }
    }

    /// The shapes of foreign move that would corrupt the board if played.
    #[test]
    fn refuses_a_move_that_does_not_belong_to_this_position() {
        let board = Board::from_fen(OPPOSITE_WINGS).unwrap();

        // d3 is empty, so there is nothing there to move
        assert!(!board.is_pseudo_legal(&quiet("d3", "d5")));
        // a6 is a black pawn and it is white to move
        assert!(!board.is_pseudo_legal(&quiet("a6", "a5")));
        // c1 is our king and d1 is our own rook
        assert!(!board.is_pseudo_legal(&quiet("c1", "d1")));
        // a knight on d4 does not reach d5
        assert!(!board.is_pseudo_legal(&quiet("d4", "d5")));
        // the rook on d1 cannot pass through the queen on d2
        assert!(!board.is_pseudo_legal(&quiet("d1", "d5")));
        // claiming a capture on an empty square
        assert!(!board.is_pseudo_legal(&takes("d4", "f5", Piece::Queen)));
        // and naming the wrong piece on an occupied one: e6 holds a pawn
        assert!(!board.is_pseudo_legal(&takes("d4", "e6", Piece::Queen)));
        // a capture that forgets to say it is one
        assert!(!board.is_pseudo_legal(&quiet("d4", "e6")));

        // and the moves those are variations of
        assert!(board.is_pseudo_legal(&quiet("d4", "f5")));
        assert!(board.is_pseudo_legal(&takes("d4", "e6", Piece::Pawn)));
    }

    /// A pawn push turns on squares the move itself never names.
    #[test]
    fn refuses_a_push_the_position_does_not_allow() {
        let board =
            Board::from_fen("rnbqkbnr/pppppppp/8/8/8/5N2/PPPPPPPP/RNBQKB1R w KQkq - 0 1").unwrap();
        // the knight on f3 blocks both the single and the double push
        assert!(!board.is_pseudo_legal(&quiet("f2", "f3")));
        assert!(!board.is_pseudo_legal(&quiet("f2", "f4")));
        // its neighbour is clear
        assert!(board.is_pseudo_legal(&quiet("e2", "e4")));

        // a pawn that has already moved cannot double push again
        let moved =
            Board::from_fen("rnbqkbnr/pppppppp/8/8/8/4P3/PPPP1PPP/RNBQKBNR w KQkq - 0 2").unwrap();
        assert!(!moved.is_pseudo_legal(&quiet("e3", "e5")));
        assert!(moved.is_pseudo_legal(&quiet("e3", "e4")));
    }
}

#[cfg(test)]
mod evasions {
    use super::Board;
    use pretty_assertions::assert_eq;

    /// Everything the filter kept that was not the king moving, by name.
    fn answers(board: &Board) -> Vec<String> {
        let king = board.king_index(board.active_color);
        let mut named: Vec<String> = board
            .evasions()
            .iter()
            .filter(|m| m.from != king)
            .map(|m| m.to_string())
            .collect();
        named.sort();
        named
    }

    #[test]
    fn a_double_check_leaves_only_king_moves() {
        // the knight on f6 and the rook on e1 both check; black has a rook
        // and a pawn with moves of their own for the filter to drop
        let board = Board::from_fen("r3k3/7p/5N2/8/8/8/8/4R1K1 b - - 0 1").unwrap();
        assert_eq!(answers(&board), Vec::<String>::new());
        assert!(
            board.evasions().len() < board.generate_moves().len(),
            "the filter dropped nothing"
        );
    }

    #[test]
    fn a_slider_check_may_be_taken_or_blocked() {
        // the rook on e1 checks up the file: the rook on a1 can take it and
        // the knight can block at e2 or e6
        let board = Board::from_fen("4k3/8/8/8/3n4/8/8/r3R1K1 b - - 0 1").unwrap();
        assert_eq!(answers(&board), vec!["a1e1", "d4e2", "d4e6"]);
    }
}

#[cfg(test)]
mod random_games {
    use super::{Board, fens};
    use proptest::prelude::*;

    /// Starts with different machinery in reach: castling, a tactical
    /// middlegame, a bare endgame and promotions.
    const STARTS: [&str; 4] = [
        fens::START,
        fens::KIWIPETE,
        fens::PAWN_ENDGAME,
        fens::PROMOTIONS,
    ];

    proptest! {
        /// Play a random line, checking every ply against a recompute, then
        /// unmake the whole line. The fixed tests do this one ply deep from
        /// positions somebody chose; this walks lines nobody did, and a
        /// failure arrives already shrunk. The same walk sweeps
        /// is_pseudo_legal and the evasion filter.
        #[test]
        fn a_random_line_stays_in_step_and_unmakes_exactly(
            start in prop::sample::select(&STARTS[..]),
            picks in prop::collection::vec(any::<prop::sample::Index>(), 0..120),
        ) {
            let mut board = Board::from_fen(start).unwrap();
            let mut line = Vec::new();
            for pick in picks {
                let moves = board.generate_moves();
                if moves.is_empty() {
                    break;
                }
                for m in &moves {
                    let refused_kind = m.castle || m.en_passant || m.promote.is_some();
                    prop_assert_eq!(
                        board.is_pseudo_legal(m),
                        !refused_kind,
                        "is_pseudo_legal disagrees about {}",
                        m
                    );
                }
                // a legal evasion dropped would read as a mate to the search
                if board.in_check() {
                    let kept = board.evasions();
                    for m in &moves {
                        if !kept.contains(m) {
                            prop_assert!(
                                !board.make_move(m),
                                "the filter dropped a legal evasion {}",
                                m
                            );
                        }
                    }
                }
                let before = board.clone();
                let play = moves[pick.index(moves.len())];
                if board.make_move(&play) {
                    super::in_step::prop_assert_in_step(&board).map_err(|e| {
                        TestCaseError::fail(format!("out of step after {}: {:?}", play, e))
                    })?;
                    line.push(before);
                } else {
                    prop_assert_eq!(&board, &before, "a refused {} left a trace", play);
                }
            }
            for before in line.iter().rev() {
                board.undo_move();
                prop_assert_eq!(&board, before, "unmaking did not restore the position");
            }
        }
    }
}

#[cfg(test)]
mod between {
    use super::{ATTACK_MASKS, BETWEEN, MAGIC};
    use crate::bitboard::BitBoard;
    use pretty_assertions::assert_eq;

    /// The table used to be built by probing the magic tables with only the
    /// two endpoints occupied; the ray walk has to answer identically for
    /// every pair.
    #[test]
    fn a_ray_walk_finds_what_the_sliders_do() {
        let magic = &MAGIC;
        for a in 0..64u8 {
            for b in 0..64u8 {
                let mut probed = 0u64;
                if a != b {
                    let ends = (1u64 << a) | (1u64 << b);
                    if ATTACK_MASKS.straight[a as usize].is_bit_set(b) {
                        probed =
                            magic.get_straight_move(a, ends) & magic.get_straight_move(b, ends);
                    } else if ATTACK_MASKS.diagonal[a as usize].is_bit_set(b) {
                        probed =
                            magic.get_diagonal_move(a, ends) & magic.get_diagonal_move(b, ends);
                    }
                }
                assert_eq!(
                    BETWEEN[a as usize][b as usize], probed,
                    "between {} and {}",
                    a, b
                );
            }
        }
    }
}

#[cfg(test)]
mod see {
    use super::{Board, Color, Piece, Play, SEE_VALUES, play_named};
    use pretty_assertions::assert_eq;

    fn see_of(fen: &str, name: &str) -> i32 {
        let board = Board::from_fen(fen).unwrap();
        board.see(&play_named(&board, name))
    }

    #[test]
    fn a_hanging_piece_is_won_outright() {
        assert_eq!(see_of("4k3/8/8/4p3/8/5N2/8/4K3 w - - 0 1", "f3e5"), 100);
    }

    #[test]
    fn a_defended_pawn_costs_the_rook_that_takes_it() {
        assert_eq!(see_of("4k3/8/4p3/3p4/8/8/8/3RK3 w - - 0 1", "d1d5"), -400);
    }

    /// The rook takes a defended queen and is taken back: queen for rook,
    /// not the queen outright.
    #[test]
    fn a_won_piece_is_still_recaptured() {
        assert_eq!(see_of("3r3k/3q4/8/8/8/8/8/3R3K w - - 0 1", "d1d7"), 400);
    }

    /// Doubled rooks against a defended pawn: without the x-ray the capture
    /// would read as losing the rook.
    #[test]
    fn a_rook_behind_the_capturing_rook_joins_the_exchange() {
        assert_eq!(see_of("3rk3/8/8/3p4/8/8/3R4/3R2K1 w - - 0 1", "d2d5"), 100);
    }

    /// The pawn is defended twice: pressing on with the bishop only feeds
    /// the second defender, so the swap stops at knight for pawn.
    #[test]
    fn the_swap_stops_rather_than_feed_the_second_defender() {
        assert_eq!(
            see_of("4k3/8/3p1p2/4p3/8/5N2/1B6/4K3 w - - 0 1", "f3e5"),
            -200
        );
    }

    /// Plain and defended cases, then a rook backing the capture through
    /// the square the taken pawn left.
    #[test]
    fn en_passant_opens_the_taken_pawns_square() {
        assert_eq!(see_of("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6"), 100);
        assert_eq!(see_of("4k3/2p5/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6"), 0);
        assert_eq!(see_of("4k3/2p5/8/3pP3/8/8/8/3RK3 w - d6 0 1", "e5d6"), 100);
    }

    /// The queen that appears is counted as the pawn it was, so the knight
    /// takes it back at a pawn's price: rook for pawn.
    #[test]
    fn a_promoting_capture_values_the_piece_taken() {
        assert_eq!(see_of("r3k3/1Pn5/8/8/8/8/8/4K3 w - - 0 1", "b7a8q"), 400);
    }

    /// With a second rook behind the first the king cannot legally take, so
    /// the pawn is simply won; without it the same capture loses the rook.
    #[test]
    fn a_king_capture_ends_the_sequence() {
        assert_eq!(see_of("8/8/2k5/3p4/8/8/3R4/3R2K1 w - - 0 1", "d2d5"), 100);
        assert_eq!(see_of("8/8/2k5/3p4/8/8/3R4/6K1 w - - 0 1", "d2d5"), -400);
    }

    /// The model `see` answers within, played out in full with free choice
    /// of attacker, where `see` commits to the least valuable: agreement
    /// says the commitment loses nothing.
    fn exhaustive(board: &Board, target: u8, occupied: u64, on_square: Piece, side: Color) -> i32 {
        let side_mask = match side {
            Color::White => board.white,
            Color::Black => board.black,
        };
        let mut set = board.attackers_to(target, occupied) & side_mask;
        let mut best = 0;
        while set != 0 {
            let bit = set & set.wrapping_neg();
            set &= set - 1;
            let value = if matches!(on_square, Piece::King) {
                SEE_VALUES[Piece::King as usize]
            } else {
                let piece = board
                    .get_piece_index(bit.trailing_zeros() as u8)
                    .expect("an attacker stands on its square");
                SEE_VALUES[on_square as usize]
                    - exhaustive(board, target, occupied & !bit, piece, !side)
            };
            best = best.max(value);
        }
        best
    }

    /// `see`'s setup verbatim, handing the first exchange to `exhaustive`.
    fn exhaustive_see(board: &Board, m: &Play) -> i32 {
        let victim = m.capture.expect("only captures are priced");
        let mut occupied = (board.white | board.black) & !(1u64 << m.from);
        if m.en_passant {
            let taken = match board.active_color {
                Color::White => m.to - 8,
                Color::Black => m.to + 8,
            };
            occupied &= !(1u64 << taken);
        }
        let mover = board
            .get_piece_index(m.from)
            .expect("a capture moves a piece of ours");
        SEE_VALUES[victim as usize] - exhaustive(board, m.to, occupied, mover, !board.active_color)
    }

    fn walk(board: &mut Board, depth: usize, priced: &mut usize) {
        let moves = board.generate_moves();
        for m in &moves {
            if m.capture.is_some() {
                assert_eq!(
                    board.see(m),
                    exhaustive_see(board, m),
                    "{} in {}",
                    m,
                    board.to_fen()
                );
                *priced += 1;
            }
        }
        if depth == 0 {
            return;
        }
        for m in &moves {
            if board.make_move(m) {
                walk(board, depth - 1, priced);
                board.undo_move();
            }
        }
    }

    /// Every capture two plies deep from the core positions and the two
    /// perft positions thick with captures, priced both ways.
    #[test]
    fn the_swap_agrees_with_an_exhaustive_negamax() {
        let mut priced = 0;
        for fen in super::fens::CORE
            .iter()
            .chain([super::fens::KIWIPETE, super::fens::PROMOTIONS].iter())
        {
            let mut board = Board::from_fen(fen).unwrap();
            walk(&mut board, 2, &mut priced);
        }
        assert!(priced > 2000, "only {} captures priced", priced);
    }
}

#[cfg(test)]
mod play_by_name {
    use super::fens;
    use super::play_named;
    use super::{Board, Color, Unplayable};
    use pretty_assertions::assert_eq;

    /// Why the move of this name is refused on this board, having checked
    /// that the refusal left every field of the board as it was.
    fn refused_on(board: &mut Board, name: &str) -> Unplayable {
        let before = board.clone();
        let why = board.play_by_name(name).expect_err("the move was played");
        assert_eq!(*board, before, "{} moved the board", name);
        why
    }

    fn refused(fen: &str, name: &str) -> Unplayable {
        refused_on(&mut Board::from_fen(fen).unwrap(), name)
    }

    /// The board after the moves of a line, each played by name.
    fn after(line: &[&str]) -> Board {
        let mut board = Board::new();
        for name in line {
            assert_eq!(board.play_by_name(name), Ok(()), "{}", name);
        }
        board
    }

    /// The board after the move of this name, held to the one `make_move`
    /// gives for the same move.
    fn played(fen: &str, name: &str) -> Board {
        let mut by_name = Board::from_fen(fen).unwrap();
        let mut made = by_name.clone();
        let play = play_named(&made, name);
        assert!(made.make_move(&play));
        assert_eq!(by_name.play_by_name(name), Ok(()));
        assert_eq!(by_name, made);
        by_name
    }

    #[test]
    fn a_name_no_move_has_is_refused() {
        assert_eq!(refused(fens::START, "e2e5"), Unplayable::NoSuchMove);
        assert_eq!(refused(fens::START, "wibble"), Unplayable::NoSuchMove);
    }

    #[test]
    fn a_move_of_the_side_not_to_move_is_refused() {
        assert_eq!(refused(fens::START, "e7e5"), Unplayable::NoSuchMove);
    }

    #[test]
    fn a_name_plays_this_boards_move_and_not_another_positions() {
        // e2a6 is a bishop taking a bishop in kiwipete and a quiet bishop
        // move here, so the two moves of that name differ in their capture
        let quiet = "4k3/8/8/8/8/8/4B3/4K3 w - - 0 1";
        let elsewhere = play_named(&Board::from_fen(fens::KIWIPETE).unwrap(), "e2a6");
        let here = play_named(&Board::from_fen(quiet).unwrap(), "e2a6");
        assert!(elsewhere.capture.is_some());
        assert_eq!(here.capture, None);
        played(quiet, "e2a6");
    }

    #[test]
    fn a_move_already_played_is_refused_and_changes_nothing() {
        let mut board = after(&["e2e4"]);
        assert_eq!(refused_on(&mut board, "e2e4"), Unplayable::NoSuchMove);
    }

    #[test]
    fn a_pinned_pawn_is_refused_after_a_line_and_changes_nothing() {
        // c2c3 blocked the bishop's check, and c3c4 steps off its line
        let mut board = after(&["e2e4", "e7e5", "d2d4", "f8b4", "c2c3", "g8f6"]);
        assert_eq!(
            refused_on(&mut board, "c3c4"),
            Unplayable::LeavesKingInCheck
        );
    }

    #[test]
    fn a_pinned_piece_leaving_its_line_is_refused() {
        // white's queen is pinned to the king by the rook on e8
        assert_eq!(
            refused("4r1k1/8/8/8/8/8/4Q3/4K3 w - - 0 1", "e2a6"),
            Unplayable::LeavesKingInCheck
        );
    }

    #[test]
    fn a_move_that_ignores_a_check_is_refused_as_leaving_the_king_in_check() {
        // the bishop on b4 checks the king, and a2a3 is still a move here
        // because the lookup reads the whole pseudo legal list
        let checked = "rnbqk1nr/pppp1ppp/8/4p3/1b1PP3/8/PPP2PPP/RNBQKBNR w KQkq - 1 3";
        assert_eq!(refused(checked, "a2a3"), Unplayable::LeavesKingInCheck);
    }

    #[test]
    fn a_move_is_played_as_make_move_plays_it() {
        let board = played(fens::START, "e2e4");
        assert_eq!(board.active_color(), Color::Black);
    }

    #[test]
    fn the_moves_is_pseudo_legal_refuses_are_played_by_name() {
        // castling, en passant and promotion, each checked to be the kind
        // of move it is named for before it is played
        let castle = Board::from_fen(fens::KIWIPETE).unwrap();
        assert!(play_named(&castle, "e1g1").castle);
        played(fens::KIWIPETE, "e1g1");

        let en_passant = "rnbqkbnr/ppp1pppp/8/8/3pP3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1";
        assert!(play_named(&Board::from_fen(en_passant).unwrap(), "d4e3").en_passant);
        played(en_passant, "d4e3");

        let promotion = Board::from_fen(fens::PROMOTIONS).unwrap();
        assert!(play_named(&promotion, "d7c8q").promote.is_some());
        played(fens::PROMOTIONS, "d7c8q");
    }
}

#[cfg(test)]
mod gives_check {
    use super::{Board, Play, fens, play_named};

    /// What each claim is held to: the check the board maintains once the
    /// move is made. `None` for a move `make_move` refuses.
    fn made(board: &mut Board, m: &Play) -> Option<bool> {
        if board.make_move(m) {
            let checked = board.in_check();
            board.undo_move();
            Some(checked)
        } else {
            None
        }
    }

    /// Every legal move of every position reached, claimed before the move
    /// is made and held to the made board's answer.
    fn walk(board: &mut Board, depth: usize, asked: &mut usize, checks: &mut usize) {
        let moves = board.generate_moves();
        for m in &moves {
            let claimed = board.gives_check(m);
            if let Some(truth) = made(board, m) {
                assert_eq!(claimed, truth, "{} in {}", m, board.to_fen());
                *asked += 1;
                *checks += usize::from(truth);
            }
        }
        if depth == 0 {
            return;
        }
        for m in &moves {
            if board.make_move(m) {
                walk(board, depth - 1, asked, checks);
                board.undo_move();
            }
        }
    }

    /// The oracle over played positions, two plies deep.
    #[test]
    fn the_claim_agrees_with_making_the_move() {
        let mut asked = 0;
        let mut checks = 0;
        for fen in fens::CORE
            .iter()
            .chain([fens::KIWIPETE, fens::PROMOTIONS].iter())
        {
            let mut board = Board::from_fen(fen).unwrap();
            walk(&mut board, 2, &mut asked, &mut checks);
        }
        assert!(asked > 50_000, "only {} moves asked", asked);
        assert!(checks > 500, "only {} checks met", checks);
    }

    /// The same oracle over positions nobody played to, from the parser's
    /// own generator on the deterministic runner, one ply deep.
    ///
    /// The castle and en passant fields are dropped before parsing: against
    /// a random placement either can license a move that corrupts the
    /// board. Castling and en passant are covered by the played walk above
    /// and the named cases below.
    #[test]
    fn the_claim_agrees_on_a_generated_corpus() {
        use proptest::strategy::{Strategy, ValueTree};
        use proptest::test_runner::TestRunner;
        let mut runner = TestRunner::deterministic();
        let strategy = super::fen_parsing::well_formed_fen();
        let mut positions = 0;
        let mut asked = 0;
        let mut checks = 0;
        for _ in 0..400 {
            let fen = strategy.new_tree(&mut runner).unwrap().current();
            let mut fields: Vec<&str> = fen.split(' ').collect();
            fields[2] = "-";
            fields[3] = "-";
            let fen = fields.join(" ");
            if let Ok(mut board) = Board::from_fen(&fen) {
                positions += 1;
                walk(&mut board, 1, &mut asked, &mut checks);
            }
        }
        assert!(positions > 50, "only {} fens parsed", positions);
        assert!(asked > 50_000, "only {} moves asked", asked);
        assert!(checks > 500, "only {} checks met", checks);
    }

    fn claims(fen: &str, name: &str) -> bool {
        let board = Board::from_fen(fen).unwrap();
        let play = play_named(&board, name);
        let claimed = board.gives_check(&play);
        let mut board = board;
        assert_eq!(
            Some(claimed),
            made(&mut board, &play),
            "the claim for {} in {} disagrees with making it",
            name,
            fen
        );
        claimed
    }

    /// The cases with machinery of their own, named so a failure says which
    /// rule broke. Each is also held to the made board, so a wrong
    /// expectation here cannot stand.
    #[test]
    fn a_direct_check_is_seen_from_the_destination() {
        assert!(claims("3k4/8/8/8/8/8/8/R4K2 w - - 0 1", "a1d1"));
        assert!(!claims("3k4/8/8/8/8/8/8/R4K2 w - - 0 1", "a1b1"));
        // the rook lands on the file by taking the pawn that blocked it
        assert!(claims("3k4/8/8/3p4/8/8/8/3R1K2 w - - 0 1", "d1d5"));
        assert!(claims("8/8/8/2k5/8/8/1P6/4K3 w - - 0 1", "b2b4"));
        assert!(!claims("8/8/8/2k5/8/8/1P6/4K3 w - - 0 1", "b2b3"));
    }

    #[test]
    fn a_discovered_check_is_seen_through_the_vacated_square() {
        // the knight leaves the rook's file, and from f6 checks on its own
        // besides
        assert!(claims("4k3/8/8/8/4N3/8/8/4RK2 w - - 0 1", "e4f6"));
        assert!(claims("4k3/8/8/8/4N3/8/8/4RK2 w - - 0 1", "e4c3"));
        assert!(claims("4k3/8/8/8/4N3/8/8/5K2 w - - 0 1", "e4f6"));
        assert!(!claims("4k3/8/8/8/4N3/8/8/5K2 w - - 0 1", "e4c3"));
    }

    #[test]
    fn a_promotion_checks_as_the_piece_it_becomes() {
        let fen = "4k3/P7/8/8/8/8/8/4K3 w - - 0 1";
        assert!(claims(fen, "a7a8q"));
        assert!(claims(fen, "a7a8r"));
        assert!(!claims(fen, "a7a8b"));
        assert!(!claims(fen, "a7a8n"));
    }

    #[test]
    fn a_castle_checks_with_the_rook() {
        assert!(claims("5k2/8/8/8/8/8/8/4K2R w K - 0 1", "e1g1"));
        assert!(!claims("k7/8/8/8/8/8/8/4K2R w K - 0 1", "e1g1"));
        assert!(claims("3k4/8/8/8/8/8/8/R3K3 w Q - 0 1", "e1c1"));
        assert!(!claims("2k5/8/8/8/8/8/8/R3K3 w Q - 0 1", "e1c1"));
    }

    #[test]
    fn en_passant_vacates_both_squares_at_once() {
        // the rook's line runs through both the capturer's square and the
        // taken pawn's; the plain push empties only the first
        assert!(claims("8/8/8/1k2pP1R/8/8/8/4K3 w - e6 0 1", "f5e6"));
        assert!(!claims("8/8/8/1k2pP1R/8/8/8/4K3 w - e6 0 1", "f5f6"));
        // the bishop's diagonal runs through the taken pawn alone
        assert!(claims("1k6/8/8/3Pp3/8/8/7B/4K3 w - e6 0 1", "d5e6"));
        // and a direct check, the pawn landing beside the king
        assert!(claims("8/2k5/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6"));
    }
}

#[cfg(test)]
mod pawn_spans {
    use super::{ATTACK_MASKS, Color, pawn_attacks};
    use pretty_assertions::assert_eq;

    /// The shifts have to answer what the masks generation reads say. The
    /// span of a white pawn on a square is the mask of the black pawns that
    /// could attack it.
    #[test]
    fn the_pawn_span_is_what_the_attack_masks_hold() {
        for square in 0..64u8 {
            let pawn = 1u64 << square;
            assert_eq!(
                pawn_attacks(pawn, Color::White),
                ATTACK_MASKS.black_pawns[square as usize],
                "a white pawn on {}",
                square
            );
            assert_eq!(
                pawn_attacks(pawn, Color::Black),
                ATTACK_MASKS.white_pawns[square as usize],
                "a black pawn on {}",
                square
            );
        }
    }
}

#[cfg(test)]
mod evasion_targets {
    use super::fens;
    use super::{Board, MoveList};
    use pretty_assertions::assert_eq;

    /// Positions with a piece able to reach a square the target mask must
    /// refuse. A case with nothing to refuse passes whatever the mask says,
    /// which is how an earlier double check here let a mask missing its
    /// double check rule through.
    const IN_CHECK: &[(&str, &str)] = &[
        // the only block a pawn can reach is b4, two squares ahead: masking
        // the step the two pushes share would lose b2b4
        ("double push blocks", "4k3/8/8/b7/8/8/1P6/4K3 w - - 0 1"),
        ("knight check", "4k3/8/8/8/8/5n2/8/4K3 w - - 0 1"),
        // the queen can take the rook, which answers one check and not the
        // other, so a mask that read only the first checker would keep it
        ("double check", "4k3/8/8/8/8/5n2/4r3/R2QK2R w KQ - 0 1"),
        ("pawn check", "4k3/8/8/8/8/8/3p4/4K3 w - - 0 1"),
        ("promotion blocks", "r3K3/2P5/8/8/8/8/8/7k w - - 0 1"),
        (
            "promotion takes the checker",
            "r3K3/1P6/8/8/8/8/8/7k w - - 0 1",
        ),
        // the checking pawn is the one taken en passant, which the mask
        // does not examine
        (
            "en passant takes the checker",
            "4k3/8/8/3pP3/4K3/8/8/8 w - d6 0 1",
        ),
        // an en passant that answers nothing, kept unexamined all the same
        // and refused by make_move
        (
            "en passant answers nothing",
            "4k3/8/8/3pP3/8/6b1/8/3RK3 w - d6 0 1",
        ),
    ];

    /// The masked generator and the filter it replaced, on one position.
    fn both_ways(board: &Board) -> (MoveList, MoveList) {
        let masked = board.evasions();
        let mut filtered = board.generate_moves();
        if board.in_check() {
            board.retain_evasions(&mut filtered);
        }
        (masked, filtered)
    }

    /// Every position in check that a walk of `depth` plies reaches, handed
    /// to `visit`.
    fn walk(board: &mut Board, depth: u8, visit: &mut impl FnMut(&Board)) {
        if board.in_check() {
            visit(board);
        }
        if depth == 0 {
            return;
        }
        for m in board.generate_moves() {
            if board.make_move(&m) {
                walk(board, depth - 1, visit);
                board.undo_move();
            }
        }
    }

    #[test]
    fn the_masked_generator_keeps_what_the_filter_kept() {
        for (name, fen) in IN_CHECK {
            let board = Board::from_fen(fen).unwrap();
            assert!(board.in_check(), "{name} is not a position in check");
            let (masked, filtered) = both_ways(&board);
            assert_eq!(masked, filtered, "{name}: {fen}");
        }

        // and over every position in check a short walk reaches
        let mut seen = 0;
        for fen in fens::CORE
            .iter()
            .chain([fens::KIWIPETE, fens::PROMOTIONS].iter())
        {
            let mut board = Board::from_fen(fen).unwrap();
            let mut checked = Vec::new();
            walk(&mut board, 3, &mut |b| checked.push(b.to_fen()));
            for position in &checked {
                let board = Board::from_fen(position).unwrap();
                let (masked, filtered) = both_ways(&board);
                assert_eq!(masked, filtered, "{position}");
            }
            seen += checked.len();
        }
        assert!(seen > 200, "the walk reached only {seen} positions");

        // the walk reaches no double check, no en passant in check and no
        // promotion that answers one, so the named cases have to carry
        // those three
        let mut doubles = 0;
        let mut passing = 0;
        let mut promotions = 0;
        for (_, fen) in IN_CHECK {
            let board = Board::from_fen(fen).unwrap();
            doubles += usize::from(board.checkers.count_ones() > 1);
            passing += usize::from(board.en_passant.is_some());
            promotions += board
                .evasions()
                .iter()
                .filter(|m| m.promote.is_some())
                .count();
        }
        assert!(doubles > 0, "no double check among the named cases");
        assert!(passing > 0, "no en passant among the named cases");
        assert!(promotions > 0, "no promotion answers a check");
    }

    #[test]
    fn out_of_check_the_evasions_are_the_whole_list() {
        for fen in fens::CORE {
            let board = Board::from_fen(fen).unwrap();
            assert!(!board.in_check(), "{fen} is in check");
            assert_eq!(board.evasions(), board.generate_moves(), "{fen}");
        }
    }
}

#[cfg(test)]
mod drawn_by_material {
    use super::{A1, B1, Board, LIGHT_SQUARES};
    use pretty_assertions::assert_eq;

    /// The same position with a named side to move: the rule reads the
    /// material alone, so both sides are asked of every case.
    fn to_move(fen: &str, side: char) -> String {
        let mut fields = fen.split(' ');
        let board = fields.next().unwrap();
        let _ = fields.next();
        let rest: Vec<&str> = fields.collect();
        format!("{} {} {}", board, side, rest.join(" "))
    }

    /// The colour mirror: ranks reversed and piece cases swapped. A vertical
    /// flip puts every bishop on the other square colour, which a rule that
    /// named light or dark rather than agreement on one would fail. The
    /// fields after the side to move are carried over, so no fen here holds
    /// a castling right or an en passant square.
    fn mirrored(fen: &str) -> String {
        let mut fields = fen.split(' ');
        let board = fields.next().unwrap();
        let side = fields.next().unwrap();
        let rest: Vec<&str> = fields.collect();
        assert_eq!(rest[0], "-", "the mirror does not move castling rights");
        let swap = |c: char| {
            if c.is_ascii_uppercase() {
                c.to_ascii_lowercase()
            } else {
                c.to_ascii_uppercase()
            }
        };
        let ranks: Vec<String> = board
            .split('/')
            .rev()
            .map(|rank| rank.chars().map(swap).collect())
            .collect();
        format!("{} {} {}", ranks.join("/"), side, rest.join(" "))
    }

    /// Every signature the rule names and every near neighbour it leaves
    /// out. Nothing else pins these: the evaluation and search tests would
    /// pass on a rule that answered one signature too many.
    #[test]
    fn material_that_cannot_mate_is_drawn_and_material_that_can_is_not() {
        for (fen, drawn, why) in [
            ("4k3/8/8/8/8/8/8/4K3 w - - 0 1", true, "two bare kings"),
            ("4k3/8/8/8/8/8/8/4K1N1 w - - 0 1", true, "a lone knight"),
            ("4k3/8/8/8/8/8/8/4KB2 w - - 0 1", true, "a lone bishop"),
            (
                "4k3/8/8/8/8/8/8/4K1NN w - - 0 1",
                true,
                "two knights, which cannot force mate",
            ),
            (
                "4k3/8/8/8/8/B7/8/2B1K3 w - - 0 1",
                true,
                "a bishop pair on one colour",
            ),
            (
                "3bk3/8/8/8/8/8/8/2B1K3 w - - 0 1",
                true,
                "a bishop each, both on one colour",
            ),
            (
                "3bk3/8/8/8/8/B7/8/2B1K3 w - - 0 1",
                true,
                "three bishops, all on one colour",
            ),
            // a pawn keeps the rule off: KBvKP is usually drawn and KNNvKP
            // is sometimes won
            (
                "4k3/8/8/4p3/8/8/8/4K1N1 w - - 0 1",
                false,
                "a knight and a pawn",
            ),
            (
                "4k3/8/8/4p3/8/8/8/4KB2 w - - 0 1",
                false,
                "a bishop and a pawn",
            ),
            (
                "4k3/8/8/4p3/8/8/8/4K1NN w - - 0 1",
                false,
                "two knights and a pawn",
            ),
            (
                "4k3/8/8/8/8/8/8/2B1KB2 w - - 0 1",
                false,
                "a bishop pair on opposite colours, which mates",
            ),
            (
                "2b1k3/8/8/8/8/8/8/2B1K3 w - - 0 1",
                false,
                "a bishop each on opposite colours",
            ),
            // drawn in practice and outside the rule
            ("4k1n1/8/8/8/8/8/8/4K1N1 w - - 0 1", false, "a knight each"),
            (
                "4k1n1/8/8/8/8/8/8/4KB2 w - - 0 1",
                false,
                "a bishop against a knight",
            ),
            (
                "4k3/8/8/8/8/8/8/4KBN1 w - - 0 1",
                false,
                "a bishop and a knight",
            ),
            // what a promotion can make, which the rule does not reach
            (
                "4k3/8/8/8/8/8/8/3NK1NN w - - 0 1",
                false,
                "three knights, which mate",
            ),
            (
                "4k1n1/8/8/8/8/8/8/4K1NN w - - 0 1",
                false,
                "two knights against one",
            ),
            ("4k3/8/8/8/8/8/8/4K2R w - - 0 1", false, "a rook"),
            ("4k3/8/8/8/8/8/8/4K2Q w - - 0 1", false, "a queen"),
        ] {
            for fen in [fen.to_string(), mirrored(fen)] {
                for side in ['w', 'b'] {
                    let fen = to_move(&fen, side);
                    let board = Board::from_fen(&fen).unwrap();
                    assert_eq!(board.drawn_by_material(), drawn, "{} is {}", fen, why);
                }
            }
        }
    }

    #[test]
    fn the_start_position_is_not_drawn() {
        assert!(!Board::default().drawn_by_material());
    }

    /// a1 is dark and b1 is light, which is what a reader checks the
    /// constant by; the walk says the other sixty two agree.
    #[test]
    fn the_light_squares_are_the_squares_whose_file_and_rank_disagree() {
        assert_eq!(LIGHT_SQUARES & (1 << A1), 0, "a1 is dark");
        assert_ne!(LIGHT_SQUARES & (1 << B1), 0, "b1 is light");
        for square in 0..64u64 {
            let light = (square % 8 + square / 8) % 2 == 1;
            assert_eq!(
                LIGHT_SQUARES & (1 << square) != 0,
                light,
                "square {}",
                square
            );
        }
    }
}
