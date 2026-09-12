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

/// The widest a generated list can be.
///
/// The most moves a legal position has ever been shown to offer is 218, and
/// generation adds nothing to that: it walks the same pieces to the same
/// squares and only declines to ask whether the mover's king is left in
/// check. This is more than twice that, because `from_fen` bounds neither
/// the number of pieces nor what they are (a position with nine queens is
/// accepted and played from), so the margin is against a position no game
/// reaches rather than against the generator. Nothing here is initialised,
/// so the width costs stack and no instructions, and the stack it costs is
/// one frame's: the buffer is gone before the search recurses.
const MAX_GENERATED: usize = 512;

/// A move list while it is being generated: a plain array and a length.
///
/// Pushing to the `SmallVec` asks whether the list has spilled, and so where
/// its buffer is, and then whether it is full, before it can store anything.
/// Generation pushes about twenty two times a call and a million and a half
/// times a search, and both answers are the same every time. Here a push is
/// a store and an increment, and the list is built once at the end.
struct Building {
    moves: [MaybeUninit<Play>; MAX_GENERATED],
    len: usize,
}

impl Building {
    #[inline(always)]
    fn new() -> Self {
        // nothing is written here, and that is the point: giving every entry
        // a value first costs a store each, and `Play` has no zero value to
        // memset (`None` for the piece a move captures is a niche rather
        // than a zero), so an initialiser here measured slower than the
        // pushing it replaced.
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
fn pop_lsb(bb: &mut u64) -> u8 {
    let i = bb.trailing_zeros() as u8;
    *bb &= *bb - 1;
    i
}

/// One ply of history: everything `undo_move` needs that the move itself does
/// not carry, being the rights and counters the move cleared and the key and
/// checkers it replaced.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct PlayState {
    play: Play,

    en_passant: Option<Coordinate>,
    castle: CastlePermissions,
    fifty_move_rule: usize,
    position_key: u64,
    checkers: u64,
}

/// The play a pass records. A pass is not a move and has none of its own, so
/// this is a placeholder: nothing plays it back, and `undo_null_move` reads it
/// only to check in a debug build that the ply it is taking back was a pass.
const NULL_PLAY: Play = Play {
    from: 0,
    to: 0,
    capture: None,
    promote: None,
    en_passant: false,
    castle: false,
};

// Plies of history the board can record, as a ring. Only the fifty move window
// is ever read back, so this has to cover that plus the depth of the current
// search rather than the whole game.
const MAX_GAME_SIZE: usize = 1024;

/// Where a ply is recorded. The history is a ring, so a game played past
/// MAX_GAME_SIZE plies, or a position parsed at a move number past it, wraps
/// rather than running off the end.
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

static ZOBRIST: Zobrist = Zobrist::TABLE;

/// What each square leaves of the castling rights, as the four bytes
/// `CastlePermissions` is laid out in.
///
/// A right is lost when a king or a rook leaves its square, or when a rook
/// is taken on one, and which right that is depends on the square alone. So
/// the from square's entry and the to square's are masked into the rights
/// together and no move is a case of its own. Every other square keeps all
/// four, which is what nearly every move meets.
///
/// The two tables differ on e1 and e8 alone. Leaving one takes both of that
/// side's rights; landing on one takes neither, because the piece standing
/// there in a position that holds the rights is the king, and a move that
/// took it would have ended the game. That holds of a parsed position as
/// well as of a played one: `from_fen` drops a right whose king or rook is
/// somewhere else.
static CASTLE_LEAVING: [u32; 64] = castle_masks(true);
static CASTLE_LANDING: [u32; 64] = castle_masks(false);

/// The rights as the one word they occupy. Four `bool` fields at alignment
/// one, four bytes with nothing between them, which `misc` asserts, so each
/// byte is one right's own zero or one and masking two words together masks
/// the rights a pair at a time. `CastlePermissions` already compares itself
/// this way.
const fn castle_bits(rights: CastlePermissions) -> u32 {
    // SAFETY: the layout above, which `misc` holds to four bytes.
    unsafe { std::mem::transmute(rights) }
}

/// The other way round. Unsafe because only a word built by anding such
/// words may take it: a byte that is a zero or a one stays one under an
/// and, and any other byte is not a `bool` at all.
///
/// # Safety
///
/// Every byte of `bits` must be a zero or a one.
const unsafe fn castle_rights(bits: u32) -> CastlePermissions {
    // SAFETY: the layout above, and the caller's obligation for the bytes.
    unsafe { std::mem::transmute(bits) }
}

const fn castle_masks(leaving: bool) -> [u32; 64] {
    const fn rights(
        black_king_side: bool,
        black_queen_side: bool,
        white_king_side: bool,
        white_queen_side: bool,
    ) -> u32 {
        castle_bits(CastlePermissions {
            black_king_side,
            black_queen_side,
            white_king_side,
            white_queen_side,
        })
    }
    let mut masks = [rights(true, true, true, true); 64];
    masks[A1 as usize] = rights(true, true, true, false);
    masks[H1 as usize] = rights(true, true, false, true);
    masks[A8 as usize] = rights(true, false, true, true);
    masks[H8 as usize] = rights(false, true, true, true);
    if leaving {
        masks[E1 as usize] = rights(true, true, false, false);
        masks[E8 as usize] = rights(false, false, true, true);
    }
    masks
}

/// What a pass folds into the key it records in the history, so that the
/// entry matches nothing. See `make_null_move` and `has_repeated` for why an
/// entry a pass wrote must never answer a repetition test. Odd, so it changes
/// every key it is applied to, and otherwise arbitrary.
const NULL_HISTORY_SALT: u64 = 0x9e37_79b9_7f4a_7c15;

static ATTACK_MASKS: AttackMasks = AttackMasks::new();
static SHELTER_MASKS: ShelterMasks = ShelterMasks::new();
static PAWN_MASKS: PawnMasks = PawnMasks::new();
// the squares strictly between two aligned squares, and empty for a pair
// that shares no line. What a piece must land on to block a slider on one
// square checking a king on the other.
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

/// Walk from `a` one square at a time in the direction of `b`, collecting what
/// it passes over. A rank, a file or a diagonal is the only way that walk can
/// land on `b`: for any other pair it steps off the board first, which is the
/// empty answer an unaligned pair is meant to give.
///
/// The blocker-aware probes said the same thing when asked with only the
/// endpoints occupied, and this owes nothing to the magic tables, which is what
/// lets it be built at compile time. `a_ray_walk_finds_what_the_sliders_do`
/// holds the two to each other.
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
/// The two slider entries are not that. They are the whole rank and file, and
/// the whole diagonals, edges and the square itself included, because what the
/// search asks of them is only whether two squares share a line: that rules a
/// slider out before the blocker-aware probe in `magic` is worth running.
/// Nothing asks whether a square shares a line with itself.
struct AttackMasks {
    black_pawns: [u64; 64],
    white_pawns: [u64; 64],
    knights: [u64; 64],
    straight: [u64; 64], // rooks and queens
    diagonal: [u64; 64], // bishops and queens
    kings: [u64; 64],
}

/// The squares one step from `from` in each of `steps`, a step that leaves
/// the board dropped. Steps are a rank and a file, so a step off the side is
/// caught by the file going out of range rather than by a per direction mask
/// that would have to be got right eight times over.
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

/// The eight directions each leaper moves in, as a rank step and a file step.
/// A pawn's are the squares it must stand on to attack the one indexed, which
/// is the mirror of what it attacks: a white pawn takes upwards, so it stands
/// one rank below.
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
    /// Built at compile time, which is why the walks above are in coordinates
    /// rather than through the mailbox: a `const` leaves nothing to build on
    /// startup and nothing to check on the way to a mask.
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

/// Every square a side's pawns attack, as one span.
///
/// A shift rather than a mask a pawn at a time, because the one caller wants
/// the whole span and asks for it at every leaf. A white pawn takes up the
/// board, to a square one rank on and a file either side, and a black pawn
/// down it; a pawn on the a file has no capture to its left and one on the h
/// file none to its right, which is what the two masks drop before the shift
/// carries a bit around into the next rank.
const fn pawn_attacks(pawns: u64, color: Color) -> u64 {
    const A_FILE: u64 = 0x0101_0101_0101_0101;
    const H_FILE: u64 = 0x8080_8080_8080_8080;
    match color {
        Color::White => ((pawns & !A_FILE) << 7) | ((pawns & !H_FILE) << 9),
        Color::Black => ((pawns & !H_FILE) >> 7) | ((pawns & !A_FILE) >> 9),
    }
}

/// The three files a king on `square` stands behind, as a bit per file.
///
/// A king on the a file or the h file is read against three files rather than
/// two, by stepping the middle one in: the b file and the g file are the
/// centres a corner king keeps. So every king square names three files and
/// the counts off them are on one scale wherever the king stands.
const fn king_files(square: u8) -> u8 {
    let file = square % 8;
    let centre = match file {
        0 => 1,
        7 => 6,
        file => file,
    };
    0b111 << (centre - 1)
}

/// The three squares `ahead` ranks in front of a king on `square`, on the
/// files [`king_files`] names, and empty where that rank is off the board.
///
/// `forward` is the direction the side's pawns push, so a white king is read
/// up the board and a black king down it. A king that has walked far enough
/// up is left with nothing in front of it, which is the answer rather than a
/// case to rule out: a king off its own back ranks has no shelter, and what
/// that is worth is for the weights to say.
const fn shelter_rank(square: u8, forward: i8, ahead: i8) -> u64 {
    let rank = (square / 8) as i8 + forward * ahead;
    if rank < 0 || rank > 7 {
        return 0;
    }
    // a file bit and a square index share their low three bits, so the byte
    // shifted to the rank is the three squares on it
    (king_files(square) as u64) << (rank * 8)
}

/// How many ranks in front of the king are masked. Three, which is as far as
/// an enemy pawn is counted: for a king at home that is the rank its own pawns
/// start on and the two beyond it.
const RANKS_AHEAD: usize = 3;

/// The squares in front of each king, and which files it stands behind.
///
/// `ahead` is indexed by how many ranks forward, then by `Color`'s
/// discriminant the way the accumulator's material is, then by the king's
/// square. A white king is read up the board and a black king down it, and the
/// files are the same either way.
///
/// The same three masks serve both sides of the term. Read against this side's
/// pawns they are the cover the king has, and against the other side's they
/// are the pawns coming for it.
struct ShelterMasks {
    ahead: [[[u64; 64]; 2]; RANKS_AHEAD],
    files: [u8; 64],
}

impl ShelterMasks {
    /// Built at compile time, the way `AttackMasks` is.
    const fn new() -> Self {
        let mut masks = ShelterMasks {
            ahead: [[[0; 64]; 2]; RANKS_AHEAD],
            files: [0; 64],
        };
        let mut square = 0u8;
        while square < 64 {
            let i = square as usize;
            masks.files[i] = king_files(square);
            let mut rank = 0;
            while rank < RANKS_AHEAD {
                let ahead = rank as i8 + 1;
                masks.ahead[rank][Color::White as usize][i] = shelter_rank(square, 1, ahead);
                masks.ahead[rank][Color::Black as usize][i] = shelter_rank(square, -1, ahead);
                rank += 1;
            }
            square += 1;
        }
        masks
    }
}

/// A pawn's own file and the files beside it, as a bit per file.
///
/// [`king_files`] steps the middle file in at the two edges so that a king
/// always names three; this does not, because the two are asking different
/// questions. A white pawn on a4 is passed while black has no pawn on the a
/// file or the b file, and a black pawn on the c file has nothing to say
/// about it. Stepping in would let that pawn stop it.
const fn pawn_files(square: u8) -> u8 {
    let own = 1u8 << (square % 8);
    // a shift off either end of the byte drops the bit, which is the edge
    // case: the a file has no file to its left and the h file none to its
    // right
    own | (own << 1) | (own >> 1)
}

/// The squares on `files` on every rank strictly ahead of `square`, ahead
/// meaning the direction `forward` pushes.
const fn span_ahead(square: u8, forward: i8, files: u8) -> u64 {
    let mut mask = 0;
    let mut rank = (square / 8) as i8 + forward;
    while rank >= 0 && rank < 8 {
        mask |= (files as u64) << (rank * 8);
        rank += forward;
    }
    mask
}

/// What stands in a pawn's way, as two masks a square names.
///
/// `front_span` is the pawn's file and the two beside it, on every rank ahead
/// of it. A pawn of ours is passed when no pawn of theirs stands anywhere in
/// it, which is the whole of the standard definition bar one clause.
/// `file_ahead` is the same span without the neighbouring files, and it
/// answers that clause: whether a pawn of ours is already in front of this
/// one. It is kept as its own table rather than masked out of the first at
/// the leaf, because a leaf term pays for every instruction it adds and a
/// table costs half a kilobyte.
///
/// Indexed by `Color`'s discriminant and then the square, the way
/// `ShelterMasks` is indexed and for the same reason. A white pawn is read up
/// the board and a black one down it.
struct PawnMasks {
    front_span: [[u64; 64]; 2],
    file_ahead: [[u64; 64]; 2],
}

impl PawnMasks {
    /// Built at compile time, the way `ShelterMasks` is.
    const fn new() -> Self {
        let mut masks = PawnMasks {
            front_span: [[0; 64]; 2],
            file_ahead: [[0; 64]; 2],
        };
        let mut square = 0u8;
        while square < 64 {
            let i = square as usize;
            let own = 1u8 << (square % 8);
            let beside = pawn_files(square);
            masks.front_span[Color::White as usize][i] = span_ahead(square, 1, beside);
            masks.front_span[Color::Black as usize][i] = span_ahead(square, -1, beside);
            masks.file_ahead[Color::White as usize][i] = span_ahead(square, 1, own);
            masks.file_ahead[Color::Black as usize][i] = span_ahead(square, -1, own);
            square += 1;
        }
        masks
    }
}

/// A set of files put back on the board: every square on every file the byte
/// names. What [`files_of`] undoes, and one multiply rather than eight
/// shifts, since a byte times the a file's eight squares lands a copy of the
/// byte on each rank.
const fn spread(files: u8) -> u64 {
    (files as u64) * 0x0101_0101_0101_0101
}

/// Every square strictly in front of one of `pawns`, on that pawn's own file.
///
/// The pawns shifted one rank on and then doubled three times, which carries
/// them the seven ranks a board has. Seeded with the shift rather than with
/// the pawns, so a pawn is never in its own fill, which is what leaves a lone
/// pawn undoubled.
const fn ahead_of(pawns: u64, color: Color) -> u64 {
    match color {
        Color::White => {
            let mut filled = pawns << 8;
            filled |= filled << 8;
            filled |= filled << 16;
            filled |= filled << 32;
            filled
        }
        Color::Black => {
            let mut filled = pawns >> 8;
            filled |= filled >> 8;
            filled |= filled >> 16;
            filled |= filled >> 32;
            filled
        }
    }
}

/// Which files a set of pawns stands on, as a bit per file.
///
/// The board folded in half three times, so the answer is three shifts, three
/// ors and a narrowing rather than eight masked tests. Nothing here says how
/// many pawns a file holds, which is all the two file counts want to know.
const fn files_of(pawns: u64) -> u8 {
    let folded = pawns | (pawns >> 32);
    let folded = folded | (folded >> 16);
    let folded = folded | (folded >> 8);
    folded as u8
}

/// What each piece is worth to `see`, indexed by `Piece`. An ordering
/// oracle, not the evaluation: these say which capture to try first, and
/// `eval::material` says what a position is worth, so either can move
/// without silently dragging the other along. The king's price only has to
/// dwarf every exchange the swap can build, without overflowing one. The
/// ordering module reads the table's bounds to prove its bands apart.
pub(crate) const SEE_VALUES: [i32; 6] = [100, 300, 300, 500, 900, 10_000];

/// The whole position with its history, which makes a board a little over
/// forty kilobytes. Copying one is nothing next to a search and a great deal
/// next to a node, so the search makes and unmakes moves on the one board;
/// only `pv_line_from` and the tests clone, and the type is not `Copy`, so a
/// copy has to be written as a clone.
///
/// Several fields restate the piece boards and are kept in step with them by
/// every move made and unmade: `squares` says what stands where, `key` and
/// `pawn_key` hash the position, `eval` carries the material and piece square
/// totals, and `checkers` holds the pieces giving check. `key` also folds in
/// the side to move, the castle rights and the en passant square, so none of
/// those changes without it. In a debug build `debug_assert_state_in_step`
/// recomputes the first four after every move, and `make_move` checks
/// `checkers` beside it. The fields are written from this file and from tests
/// that set a position up by hand; outside the crate the position is read
/// through the accessors and moved through `try_make` and `try_undo`.
#[derive(Debug, PartialEq, Clone, Eq)]
pub struct Board {
    // One board per piece, indexed by `Piece`, rather than a field each.
    // `move_accumulators` picks the one it is given a piece for, and as fields
    // that pick was a six way match compiling to a jump table which the branch
    // predictor missed about a fifth of the time. Indexing costs no branch.
    // The accessors below are what everything else reads, so only the pick
    // changed.
    pieces: [u64; 6],

    white: u64,
    black: u64,

    // what stands on each square, so that asking costs a load rather than a
    // walk down the six boards above. Written by `move_accumulators` beside
    // them, which is the one place a piece is put down or picked up, and read
    // by `get_piece_index`, which generation asks once per capture, ordering
    // once per move scored, and make and unmake once each. Sixty four bytes on
    // a board that is already forty kilobytes, and the board is only copied
    // outside the search.
    //
    // Not called a mailbox, though that is what it is, because this crate
    // already calls the ten by ten sentinel grid in `magic` the mailbox and
    // one name for two things is worse than a plain one here.
    squares: [Option<Piece>; 64],

    pub(crate) active_color: Color,
    castle: CastlePermissions,
    en_passant: Option<Coordinate>,
    // the pieces giving check to the side to move, maintained by make_move
    // from the move itself rather than recomputed by attack probes at every
    // node. Empty when the side to move is not in check; holding the checkers
    // rather than that fact is what lets the search refuse moves that cannot
    // answer a check without playing them
    checkers: u64,

    ply: usize,
    // plies since the root of the search, counted up by every move and pass
    // made and back down by every one unmade, and set to zero by `start_line`
    // when a search begins. What a mate score's distance is measured in, and
    // the depth the table adjusts such a score by
    pub(crate) line_ply: usize,
    move_number: usize,
    fifty_move_rule: usize,

    // the evaluation's incremental state, told about every piece placed,
    // removed and moved from one square to another. The eval module owns
    // what it means; the board only keeps it in step
    pub(crate) eval: Accumulator,

    history: [Option<PlayState>; MAX_GAME_SIZE],
    pub(crate) key: u64,
    /// The same kind of key over the pawns alone: both sides' pawns and the
    /// squares they stand on, and nothing else. Two positions with the same
    /// pawns and different pieces share it, which is what a table keyed by
    /// pawn structure wants. It carries no side to move, no castle rights
    /// and no en passant square, so it says what the pawns are and not whose
    /// turn it is.
    ///
    /// Kept in step beside the position key, and so by every path that moves
    /// a pawn or takes one off: `relocate_piece_index` for a push and for
    /// the pawn doing the taking in a capture or en passant,
    /// `move_accumulators` for the pawn being taken and for a promotion,
    /// which takes a pawn out of the key and puts nothing back. A pass moves
    /// no piece and leaves it alone.
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

    /// Play a move from outside the crate, which is where a move can be one
    /// this position never generated: a move carried over from another
    /// position, or from this one before something else was played. Such a
    /// move is refused rather than made, and so is a legal-looking one that
    /// leaves the king in check. True when the move was made.
    ///
    /// Checked by generating, the way `make_move_str` checks a name, rather
    /// than by `is_pseudo_legal`, which passes on castling, en passant and
    /// promotion and leaves them to generation.
    pub fn try_make(&mut self, play: &Play) -> bool {
        self.generate_moves().contains(play) && self.make_move(play)
    }

    /// Take back the last move `try_make` made. False when there is no move
    /// behind this position to take back, which is a question of the history
    /// and not of the ply: a position read from a fen starts at the ply its
    /// move number says, with nothing behind it. The history keeps the last
    /// `MAX_GAME_SIZE` plies, so a line longer than that cannot be taken all
    /// the way back. `undo_move` itself does not check, since the search
    /// never asks with an empty history.
    pub fn try_undo(&mut self) -> bool {
        if self.ply == 0 {
            return false;
        }
        let Some(last) = self.history[history_index(self.ply - 1)] else {
            return false;
        };
        // a pass is made and unmade by the search alone, which never leaves
        // one behind for a caller to find
        debug_assert_ne!(last.play, NULL_PLAY, "the last ply was a pass");
        self.undo_move();
        true
    }

    /// Whether this move is one `generate_moves` would produce here.
    ///
    /// A probe checks the key a slot was stored under, so a hit is this
    /// position and its move is one generated for it. Almost: a slot keeps
    /// thirty two bits of the key and its index says another twenty or so,
    /// and a long search sees enough positions for two of them to agree on
    /// all of that. Rare, one probe in thousands of millions, and this is
    /// what keeps rare from being ruinous.
    ///
    /// Ordering never had to care, because a move from another position
    /// matches nothing in the generated list and is passed over. Playing one
    /// does: `make_move` reads the capture, promotion and castling fields as a
    /// description of this board, so a move belonging to a different one
    /// corrupts the position rather than merely wasting a node.
    ///
    /// Answering no when the truth is yes is free: the caller falls back to
    /// generating, which is what it would have done anyway. So the fiddly cases
    /// are simply refused rather than checked, which keeps this cheap enough to
    /// be worth asking on the way past.
    ///
    /// This is also `make_move`'s precondition written down, which it otherwise
    /// does not have: it says what a move has to be for `make_move` to read its
    /// fields as a description of this board. Keep it stated over the move and
    /// the position alone, with nothing else assumed.
    pub fn is_pseudo_legal(&self, m: &Play) -> bool {
        // castling, en passant and promotion each carry conditions of their own
        // that this would have to restate. They are rare, so let them generate.
        if m.castle || m.en_passant || m.promote.is_some() {
            return false;
        }

        let color_mask = match self.active_color {
            Color::Black => self.black,
            Color::White => self.white,
        };
        // the piece has to be ours, and cannot land on top of another of ours
        if !color_mask.is_bit_set(m.from) || color_mask.is_bit_set(m.to) {
            return false;
        }
        let Some(piece) = self.get_piece_index(m.from) else {
            return false;
        };
        // make_move clears exactly the piece the move names, so the move has to
        // name what is actually standing there
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
                // a pawn one step from the far rank only ever promotes, and
                // promotions were refused above
                if match self.active_color {
                    Color::White => rank == 7,
                    Color::Black => rank == 2,
                } {
                    return false;
                }
                if m.capture.is_some() {
                    // that the piece taken is really there, and is really
                    // the one named, was settled above: all that is left is
                    // whether a pawn on the from square attacks the to one
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
                // the double push, only from the rank it is allowed from and
                // only when the square beyond is empty too
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
    /// the promoting pushes, in the same order, made without generating the
    /// quiet moves only to filter them out.
    pub fn generate_captures(&self) -> MoveList {
        self.generate::<true, false>()
    }

    /// The moves worth trying here: every pseudo legal one, less those that
    /// cannot answer a check when there is one. Most of what full width
    /// generation returns in check would only be refused by `make_move`, so
    /// leaving it ungenerated spares the push, the sort and the make.
    ///
    /// Correct whichever position it is asked of: out of check it is
    /// `generate_moves`. The caller does not have to establish that it is in
    /// check first, which is what the filter this replaced used to ask of it.
    ///
    /// The list is the one the filter returned, move for move and in the same
    /// order, which `the_masked_generator_keeps_what_the_filter_kept` holds it
    /// to against the filter itself.
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

    /// The squares a move must land on to answer the check, for a mover that
    /// is not the king. Capturing the sole checker or blocking its line are
    /// the only two, and a double check leaves neither: nothing answers it
    /// but a king move.
    ///
    /// En passant is not in here. The captured pawn does not stand on the to
    /// square, so this mask misreads it, and the generator leaves those moves
    /// unmasked the way the filter left them unexamined.
    fn evasion_targets(&self) -> u64 {
        debug_assert!(self.checkers != 0, "asked of a position not in check");
        if self.checkers.count_ones() > 1 {
            return 0;
        }
        let checker = self.checkers.trailing_zeros() as usize;
        let king = self.king_index(self.active_color) as usize;
        self.checkers | BETWEEN[king][checker]
    }

    /// The one generator behind generate_moves and generate_captures. The
    /// const parameter is settled at compile time, so each wrapper
    /// monomorphises into the equivalent of a hand-written copy: the captures
    /// one masks every piece's targets with the opponent's pieces and drops
    /// the quiet-only sections, with nothing tested per move.
    fn generate<const CAPTURES_ONLY: bool, const EVASIONS: bool>(&self) -> MoveList {
        let mut moves = Building::new();
        let (color_mask, capture_mask) = match self.active_color {
            Color::Black => (self.black, self.white),
            Color::White => (self.white, self.black),
        };
        let all_pieces = self.black | self.white;
        let attack_masks = &ATTACK_MASKS;
        let magic = &MAGIC;
        // the captures list keeps only the squares the opponent stands on,
        // the full list keeps every square our own pieces do not. The king
        // takes this one as it stands, which is why it is named
        let king_filter = if CAPTURES_ONLY {
            capture_mask
        } else {
            !color_mask
        };
        // in check, everything but the king has to capture the checker or
        // block it, so the squares that do neither come off the filter and
        // those moves are never built. A double check leaves this zero,
        // which is the same statement
        let evasion_filter = if EVASIONS { self.evasion_targets() } else { !0 };
        let target_filter = king_filter & evasion_filter;
        // when every target is a capture there is nothing to ask per move
        let capture_at = |to: u8| {
            if CAPTURES_ONLY {
                self.get_piece_index(to)
            } else {
                self.capture_on(to, capture_mask)
            }
        };
        // knights
        let mut knights = self.knights() & color_mask;
        while knights != 0 {
            let from = pop_lsb(&mut knights);
            let mut targets = attack_masks.knights[from as usize] & target_filter;
            while targets != 0 {
                let to = pop_lsb(&mut targets);
                moves.push(Play::new(from, to, capture_at(to), None, false, false));
            }
        }
        // queens and rooks
        let mut queens_and_rooks = (self.queens() | self.rooks()) & color_mask;
        while queens_and_rooks != 0 {
            let from = pop_lsb(&mut queens_and_rooks);
            let mut targets = magic.get_straight_move(from, all_pieces) & target_filter;
            while targets != 0 {
                let to = pop_lsb(&mut targets);
                moves.push(Play::new(from, to, capture_at(to), None, false, false));
            }
        }
        // queens and bishops
        let mut queens_and_bishops = (self.queens() | self.bishops()) & color_mask;
        while queens_and_bishops != 0 {
            let from = pop_lsb(&mut queens_and_bishops);
            let mut targets = magic.get_diagonal_move(from, all_pieces) & target_filter;
            while targets != 0 {
                let to = pop_lsb(&mut targets);
                moves.push(Play::new(from, to, capture_at(to), None, false, false));
            }
        }
        // kings, which take the filter without the evasion mask: a king
        // answers a check by leaving, not by landing on the checker's line,
        // and `make_move` settles which squares it may step to
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
            // castling: the right is still held, the king is not in check,
            // the squares between are empty, and the king does not pass
            // through a square the opponent attacks. Both colours read the
            // one rule below off their own row of the table
            let (king_square, opponent, held, castles) = match self.active_color {
                Color::White => (
                    E1,
                    Color::Black,
                    [self.castle.white_queen_side, self.castle.white_king_side],
                    &WHITE_CASTLES,
                ),
                Color::Black => (
                    E8,
                    Color::White,
                    [self.castle.black_queen_side, self.castle.black_king_side],
                    &BLACK_CASTLES,
                ),
            };
            // one probe of the king's square for both castles rather than
            // one each, and none at all when neither right is left
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
        // pawns
        let mut pawns = self.pawns() & color_mask;
        while pawns != 0 {
            let from = pop_lsb(&mut pawns);
            let (rank, _) = index_to_coordinate(from);
            let can_promote = match self.active_color {
                Color::White => rank == 7,
                Color::Black => rank == 2,
            };
            // move diagonally and capture
            let pmoves: u64 = match self.active_color {
                Color::White => attack_masks.black_pawns[from as usize] & capture_mask,
                Color::Black => attack_masks.white_pawns[from as usize] & capture_mask,
            };
            // a pawn's captures take the evasion mask like every other
            // piece's: taking something that is not the checker leaves the
            // king in check
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
            // move forward. A promotion changes the material on the board the
            // way a capture does, so the captures list keeps the promoting
            // pushes and drops only the quiet ones: quiescence would otherwise
            // stand a pawn on the seventh and score it as a pawn
            if !CAPTURES_ONLY || can_promote {
                let to = match self.active_color {
                    Color::White => from as isize + 8,
                    Color::Black => from as isize - 8,
                };
                // the square being empty is what lets the pawn through, and
                // the double push needs the single one's square empty as
                // well, so the evasion mask is asked of each push and not of
                // the step they share: a double push can block a check the
                // single push does not reach
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
            // en passant
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
    /// every count correct and shows up only as the engine evaluating or
    /// transposing wrongly. Debug only: it walks the whole board.
    ///
    /// Each recompute (the ones below and `Accumulator::recomputed`) is a
    /// second implementation on purpose, and only worth having while it
    /// stays one. Factoring shared code out of a recompute and the
    /// piece-at-a-time path it is checked against would leave both sides
    /// wrong together and this passing, which is worse than not checking at
    /// all: do not tidy them into each other.
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
        // the recompute reads the castle rights and the en passant square as
        // they stand, so the checks above cannot tell a field set against the
        // rule: assert the rules themselves. A right belongs to a king and a
        // rook standing where the castle moves them from, and a square to a
        // pawn the side to move can take there. Both hold of a played
        // position, since make_move gives up the rights of every square a
        // king or rook leaves and records a square only for a double push
        // that was answerable, and `from_fen` drops what a fen states past
        // them.
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

    /// The position key computed from the board rather than maintained as moves
    /// are made, built the way `from_fen` builds it. `key` is meant to equal
    /// this at all times, which `debug_assert_state_in_step` checks on every
    /// move made.
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

    /// The pawn key computed from the pawn boards rather than maintained as
    /// moves are made. `pawn_key` is meant to equal this at all times, which
    /// `debug_assert_state_in_step` checks on every move made.
    ///
    /// A second implementation on purpose, like the recomputes above: it
    /// walks the pawns where the maintained key follows each one placed,
    /// removed and relocated.
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

    /// What stands on each square according to the piece boards, which is the
    /// walk `get_piece_index` used to do before `squares` answered instead.
    /// `squares` is meant to equal this at all times, which
    /// `debug_assert_state_in_step` checks on every move made.
    ///
    /// All sixty four squares rather than the occupied ones alone, unlike the
    /// recomputes above: an entry left behind on a square that has since been
    /// emptied is exactly the drift worth catching, and walking the occupied
    /// squares would never look at it.
    fn recompute_squares(&self) -> [Option<Piece>; 64] {
        // by popping the pieces off their boards rather than asking all
        // sixty four squares what stands on them: this runs on every move
        // of every debug test, and the per-square walk tripled the debug
        // suite's time in ci. The answer is the same array either way, and
        // the empty squares stay None by never being written
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
        // pawns
        if (pawn_masks[index as usize] & self.pawns() & color_mask) > 0 {
            return true;
        }

        // knights
        if (attack_masks.knights[index as usize] & self.knights() & color_mask) > 0 {
            return true;
        }

        // bishops & queens
        let bishop_or_queen = (self.bishops() | self.queens()) & color_mask;
        if (attack_masks.diagonal[index as usize] & bishop_or_queen) > 0 {
            let move_mask = magic.get_diagonal_move(index, all);
            if (move_mask & bishop_or_queen) > 0 {
                return true;
            }
        }

        // rooks & queens
        let rook_or_queen = (self.rooks() | self.queens()) & color_mask;
        if (attack_masks.straight[index as usize] & rook_or_queen) > 0 {
            let move_mask = magic.get_straight_move(index, all);
            if (move_mask & rook_or_queen) > 0 {
                return true;
            }
        }

        // kings
        if (attack_masks.kings[index as usize] & self.kings() & color_mask) > 0 {
            return true;
        }

        false
    }

    /// Every piece of either colour bearing on `index` through `occupied`,
    /// the two halves put back together.
    ///
    /// The swap asks for the halves rather than this, since the steppers do
    /// not change as it empties the square. What is left here is the whole
    /// statement of what an attacker is, which the exhaustive model `see` is
    /// checked against reads.
    #[cfg(test)]
    fn attackers_to(&self, index: u8, occupied: u64) -> u64 {
        (self.steppers_onto(index) | self.sliders_onto(index, occupied)) & occupied
    }

    /// The pawns, knights and kings bearing on `index`. Nothing stands
    /// between a stepper and the square it attacks, so this is the half of
    /// `attackers_to` that does not depend on the occupancy, and a swap
    /// works it out once rather than at every capture it prices.
    #[inline]
    fn steppers_onto(&self, index: u8) -> u64 {
        let attack_masks = &ATTACK_MASKS;
        let i = index as usize;
        let pawns = ((attack_masks.white_pawns[i] & self.white)
            | (attack_masks.black_pawns[i] & self.black))
            & self.pawns();
        pawns | (attack_masks.knights[i] & self.knights()) | (attack_masks.kings[i] & self.kings())
    }

    /// The bishops, rooks and queens bearing on `index` through `occupied`,
    /// which is the half that does depend on what stands between.
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

    /// How many squares this side's knights, bishops, rooks and queens cover,
    /// a count per piece kind in that order, which is the order
    /// `eval::MOBILE_PIECES` names them in. `KINDS` says which of the four to
    /// count; the rest are not looked at and answer zero.
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
    /// Both `eval` and the tuner's walk read this, but no longer for the same
    /// kinds. The walk asks for all four, because it is offline and its
    /// coefficients are what prices a kind; `eval` asks for the kinds whose
    /// weight is not zero, because a count multiplied by zero is not worth the
    /// leaf it is taken at. What keeps the two honest is that the difference
    /// between them is exactly the zero weights, which
    /// `eval_counts_a_kind_exactly_when_its_weight_is_not_zero` pins. Within
    /// one set of kinds the counts are still one answer rather than two, so a
    /// second implementation of them would still be two chances to be wrong
    /// rather than a check on one, and the hand counts below are what pins
    /// them. That is the exception to the rule `tune.rs` states in its header,
    /// which names it.
    ///
    /// `KINDS` is a compile time set, so a kind left out of it costs nothing:
    /// its loop is not compiled rather than skipped.
    ///
    /// Inlined by force. Left to itself llvm keeps this out of line even under
    /// link time optimisation, and `eval` asks for it twice at every leaf and
    /// every quiescence node. That call was three fifths of what the term cost
    /// over the bench: 4.30 billion instructions without the attribute against
    /// 3.76 billion with it.
    #[inline(always)]
    pub(crate) fn mobility_counts<const KINDS: u8>(
        &self,
        color: Color,
    ) -> [i32; eval::MOBILE_PIECES.len()] {
        let occupied = self.occupied();
        let (ours, theirs) = match color {
            Color::White => (self.white, self.black),
            Color::Black => (self.black, self.white),
        };
        let scope = !(ours | pawn_attacks(self.pawns() & theirs, !color));
        let attack_masks = &ATTACK_MASKS;
        let magic = &MAGIC;
        let mut counts = [0; 4];
        if eval::counted(KINDS, 0) {
            let mut knights = self.knights() & ours;
            while knights != 0 {
                let from = pop_lsb(&mut knights);
                counts[0] += (attack_masks.knights[from as usize] & scope).count_ones() as i32;
            }
        }
        if eval::counted(KINDS, 1) {
            let mut bishops = self.bishops() & ours;
            while bishops != 0 {
                let from = pop_lsb(&mut bishops);
                counts[1] += (magic.get_diagonal_move(from, occupied) & scope).count_ones() as i32;
            }
        }
        if eval::counted(KINDS, 2) {
            let mut rooks = self.rooks() & ours;
            while rooks != 0 {
                let from = pop_lsb(&mut rooks);
                counts[2] += (magic.get_straight_move(from, occupied) & scope).count_ones() as i32;
            }
        }
        if eval::counted(KINDS, 3) {
            let mut queens = self.queens() & ours;
            while queens != 0 {
                let from = pop_lsb(&mut queens);
                let attacks = magic.get_straight_move(from, occupied)
                    | magic.get_diagonal_move(from, occupied);
                counts[3] += (attacks & scope).count_ones() as i32;
            }
        }
        counts
    }

    /// The least valuable piece of `set`: the bit of one such piece and what
    /// it is. `set` is a subset of one side's pieces.
    fn least_valuable(&self, set: u64) -> Option<(u64, Piece)> {
        // every swap ends by asking this of an empty set, once per capture
        // priced, and without the test that walks all six boards to say so
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
    /// once every profitable recapture on its square has been traded through.
    /// The swap records the least valuable attacker capturing on both sides
    /// until one side has none left, with sliders behind the piece that just
    /// captured joining in as the line opens; the negamax fold over the
    /// recorded stack then lets either side stop where continuing stands
    /// worse than what it already has.
    ///
    /// En passant is played exactly: the pawn taken is lifted from its own
    /// square, not the target, so a slider it was blocking joins the swap.
    /// A promotion is counted as the pawn it was, on both sides of the
    /// exchange: the first capture still values the piece it takes, but the
    /// queen that appears is worth a pawn to whoever takes it back. That
    /// undervalues promoting captures, and the ordering promotions get is
    /// theirs to fix. Pins are ignored: every attacker is assumed free to
    /// capture, however its king stands. A move with no victim is worth zero
    /// here; the callers only ask about captures.
    pub(crate) fn see(&self, m: &Play) -> i32 {
        let Some(victim) = m.capture else {
            return 0;
        };
        // more slots than pieces that could ever join one square's swap.
        // Every slot is written before it is read: `d` counts the swap up
        // and the fold reads it down, so zero filling the array was a
        // memset per capture ordered that nothing consumed
        let mut gain = [const { MaybeUninit::<i32>::uninit() }; 32];
        gain[0].write(SEE_VALUES[victim as usize]);
        let mut occupied = self.white | self.black;
        occupied &= !(1u64 << m.from);
        if m.en_passant {
            // the pawn taken en passant does not stand on the target square
            let taken = match self.active_color {
                Color::White => m.to - 8,
                Color::Black => m.to + 8,
            };
            occupied &= !(1u64 << taken);
        }
        // the piece standing on the target square, which the next capture
        // takes
        let mut on_square = self
            .get_piece_index(m.from)
            .expect("a capture moves a piece of ours");
        let mut side = !self.active_color;
        let mut d = 0;
        // the steppers bearing on the square are the same however the swap
        // empties it, so they are found once here rather than at every
        // capture. Masking by `occupied` is what drops the ones that have
        // already captured, which is what the whole lookup did before
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
            // taken could not legally have captured onto this square, which
            // the fold below reads off the king's price
            if matches!(on_square, Piece::King) {
                break;
            }
            occupied &= !bit;
            on_square = piece;
            side = !side;
        }
        // negamax over the stack: at each step the side to move keeps the
        // better of stopping and the exchange it recorded
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
    /// position itself, and stopping once `enough` of them have been found.
    /// Both callers name what they need: the search asks whether there was
    /// one at all, and the draw rule whether there were two.
    ///
    /// Every other ply is looked at and the ones between are not. A key
    /// carries the side to move, and every ply hands the move over, so an
    /// entry an odd number of plies back belongs to the other side and can
    /// never equal this key. Stepping in twos halves the walk and changes
    /// no answer.
    fn prior_occurrences(&self, enough: usize) -> usize {
        // only the fifty move window can hold a repetition, since a pawn move or
        // a capture in between puts the position out of reach for good. A fen
        // can claim a fifty move count longer than the history or the game
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

    /// The fifty move counter, as the fen prints it. Read by the residual
    /// sampler, which puts it in a column of its own so that rows filter on
    /// it without a fen being parsed to find it.
    pub fn halfmove_clock(&self) -> usize {
        self.fifty_move_rule
    }

    /// Whether the fifty move counter has run out.
    ///
    /// Not the same as drawn: a mate delivered on the hundredth half move
    /// ends the game on it, before the side mated has a move to claim the
    /// draw with, so a caller that can tell a mate has to ask that too.
    pub fn fifty_move_expired(&self) -> bool {
        self.fifty_move_rule >= 100
    }

    /// Whether the fifty move counter stands within four plies of expiry:
    /// the horizon behind which the rule50 taint policy refuses every
    /// transposition cutoff, as Stockfish does in its main search, and
    /// here in quiescence besides.
    pub fn fifty_move_near_expiry(&self) -> bool {
        self.fifty_move_rule >= 96
    }

    /// True on the third occurrence, which is when a game is actually drawn.
    ///
    /// Nothing in the search calls it: the search takes a draw on the first
    /// repetition instead, for the reason `has_repeated` gives. This is the
    /// rule that one is measured against, and what the tests contrast it
    /// with, so it is kept rather than inlined into them.
    #[allow(dead_code)]
    pub(crate) fn is_repetition(&self) -> bool {
        self.prior_occurrences(2) >= 2
    }

    /// True once this position has come up before. Inside a search that is
    /// already enough to call it a draw: a position reached twice can be
    /// reached a third time by whichever side wants it, so neither can be made
    /// to avoid it, and waiting for the third costs four plies of depth to see
    /// something that is available now.
    ///
    /// A claim here is a claim that a legal path came back to this position,
    /// which is why the entry a pass writes is salted and matches nothing. A
    /// pass is not a move either side has, so a line through one is not a
    /// path a rule knows about, and an unsalted entry would let a later real
    /// position count the passed-from position as an occurrence and take a
    /// draw no rule grants. Every real entry either side of a pass still
    /// compares as it always did: the window is a range of plies rather than
    /// a walk back from here, so removing one entry from it removes nothing
    /// else. Engines that let a repetition be claimed through a pass differ
    /// here, and this is the divergence.
    pub fn has_repeated(&self) -> bool {
        self.prior_occurrences(1) >= 1
    }

    /// Whether the side to move has a legal move at all. Asked only where a
    /// draw rule and a mate could coincide, which is rare, so it plays the
    /// moves rather than keeping anything incremental.
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

    /// The one caller that never reads `checkers` is perft, which counts
    /// with MAINTAIN_CHECKERS off: the legality probe runs unconditionally,
    /// since the stale checkers cannot be consulted, and `checkers_given` is
    /// skipped. History still saves and restores the field, so the board's
    /// checkers are intact once the walk unwinds. `perft_as_played` counts
    /// the same positions with it on, so what a game relies on is held to
    /// the same numbers.
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
        // Update castling permissions. The from square's mask covers a king
        // or a rook leaving, the to square's a rook being taken where it
        // stands; a rook taken on its square is the only piece worth asking
        // the to square about, since taking a king would have ended the game.
        // Both squares are read from one table, so the handful of squares
        // that take a right away cost the same as the sixty that do not.
        let old_castle = self.castle;
        let old_bits = castle_bits(old_castle);
        let bits = old_bits & CASTLE_LEAVING[play.from as usize] & CASTLE_LANDING[play.to as usize];
        // XORing both the old and new castle keys removes the old permissions
        // from the position key and adds the new ones. Asked first rather
        // than folded unconditionally: the rights only change when a king or
        // a rook leaves its square or a rook is taken on one, which is a
        // handful of moves in a game, and on every other one the two keys are
        // the same key and cancel. One comparison decides that, where folding
        // both walked the four rights twice to arrive at nothing.
        if bits != old_bits {
            // SAFETY: every byte of `bits` is a byte of the old rights
            // anded with a byte of each mask, and all three are a `bool`'s
            // own zero or one.
            self.castle = unsafe { castle_rights(bits) };
            self.key ^= ZOBRIST.castle_key(old_castle) ^ ZOBRIST.castle_key(self.castle);
        }
        if let Some(en_passant) = self.en_passant {
            // the en passant rights of the previous position have expired
            self.key ^= ZOBRIST.en_passant_key(en_passant.as_index());
        }
        self.en_passant = None;
        self.fifty_move_rule += 1;

        if self.pawns().is_bit_set(play.from) {
            // pawn moves reset the fifty move rule
            self.fifty_move_rule = 0;
            if (play.from as isize - play.to as isize).abs() == 16 {
                // the square the pawn passed over only belongs in the key if
                // something can be taken on it. Hashing it unconditionally
                // makes one position hash two ways, which costs transposition
                // hits and hides a repetition either side of a double push
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
        if self.active_color == Color::Black {
            self.move_number += 1;
        }

        let king_index = self.king_index(self.active_color);
        // A move only exposes its own king when there was a check to walk
        // back into, the king itself moved, a square on a line through the
        // king was vacated, or en passant emptied a second square. Anything
        // else keeps the king exactly as attacked as it was, which was not
        // at all, so the probe has nothing to find. `checkers` still holds
        // the mover's own checkers here: it is only replaced below, once the
        // move has been allowed to stand.
        let attack_masks = &ATTACK_MASKS;
        let could_expose_king = !MAINTAIN_CHECKERS
            || self.checkers != 0
            || from_piece == Piece::King
            || play.en_passant
            || attack_masks.straight[king_index as usize].is_bit_set(play.from)
            || attack_masks.diagonal[king_index as usize].is_bit_set(play.from);
        self.active_color = opposing_color;
        self.key ^= ZOBRIST.side;
        self.debug_assert_state_in_step();
        debug_assert!(
            could_expose_king || !self.square_attacked(king_index, opposing_color),
            "a move the filter cleared left the king attacked: {}",
            play
        );
        if could_expose_king && self.square_attacked(king_index, opposing_color) {
            self.undo_move();
            false
        } else {
            if MAINTAIN_CHECKERS {
                // the piece that landed on the to square, once promotion has
                // had its say
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
        // castle rights, en passant and the fifty move counter cannot be
        // recomputed from the move alone, they come back from the history
        self.castle = history.castle;
        self.en_passant = history.en_passant;
        self.fifty_move_rule = history.fifty_move_rule;
        self.ply -= 1;
        self.line_ply -= 1;
        if opposing_color == Color::Black {
            self.move_number -= 1;
        }

        if play.en_passant {
            // the captured pawn stood behind the to square, not on it
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
        // restore the position key exactly as it was before the move was made,
        // this guarantees make/undo can never let the key drift out of sync
        self.key = history.position_key;
        self.checkers = history.checkers;
    }

    /// Hand the move to the other side without touching a piece.
    ///
    /// Not a chess move, and nothing outside the search has any business
    /// playing one: the search passes to ask what a position is worth to a
    /// side that does nothing, which is a question about the tree rather than
    /// about the game. No piece moves, so the piece boards, the squares and
    /// the evaluation accumulators are all left exactly as they stand.
    ///
    /// A pass is a reversible ply. Nothing was captured and no pawn moved, so
    /// the fifty move counter runs on, which near the horizon is the point:
    /// passing does not buy the side to move its way out of a draw.
    ///
    /// The en passant square goes, as it does on any move: the right belonged
    /// to the side that has just given the move away. The checkers come out
    /// empty, because the side not to move is never in check, so the side
    /// inheriting the move is not in check either.
    ///
    /// The history entry's key is salted, which is what keeps a pass out of
    /// the repetition arithmetic; `has_repeated` has the reasoning.
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
        // the salt comes back off: what was recorded was this position's key
        // with it on
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
            // a promotion is not a relocation: the piece that stands on the
            // board afterwards is not the one that left, so what it is worth
            // and what it counts for the phase both change
            Some(promote) => {
                self.clear_piece_index(from, piece, color);
                self.set_piece_index(to, (&promote).into(), color);
            }
            None => self.relocate_piece_index(from, to, piece, color),
        }
    }

    /// The castle rights this position has the pieces for: a right whose king
    /// or rook is not standing on the square the castle moves it from is
    /// dropped.
    ///
    /// The generator asks the right, the empty squares and the attacked
    /// squares, and takes the king from wherever it stands, so a right held
    /// over a king somewhere else is a castle out of that square; make_move
    /// then relocates whatever sits on the rook's corner as though it were the
    /// rook. Only `from_fen` can produce such a right, since a played move
    /// gives up the rights of every square a king or rook leaves.
    fn rights_the_pieces_bear_out(&self) -> CastlePermissions {
        let holds = |index: u8, piece: Piece, color: Color| {
            self.get_piece_and_color_index(index) == Some((piece, color))
        };
        let white_king = holds(E1, Piece::King, Color::White);
        let black_king = holds(E8, Piece::King, Color::Black);
        CastlePermissions {
            white_king_side: self.castle.white_king_side
                && white_king
                && holds(H1, Piece::Rook, Color::White),
            white_queen_side: self.castle.white_queen_side
                && white_king
                && holds(A1, Piece::Rook, Color::White),
            black_king_side: self.castle.black_king_side
                && black_king
                && holds(H8, Piece::Rook, Color::Black),
            black_queen_side: self.castle.black_queen_side
                && black_king
                && holds(A8, Piece::Rook, Color::Black),
        }
    }

    /// Whether an en passant capture on this square is one this position can
    /// actually make: the rank a double push crosses, a pawn of ours placed to
    /// take there, the square itself empty, and the pawn the capture removes
    /// standing behind it.
    ///
    /// The pawn placed to take is make_move's own rule and says whether the
    /// square belongs in the key. The rest is what the square claims and the
    /// generator does not check, since the capture is emitted from the square
    /// alone. Without them make_move clears a pawn from a square holding
    /// something else, or lands the capturer on top of a piece nothing took.
    fn en_passant_can_be_played(&self, index: u8) -> bool {
        let (rank, _) = index_to_coordinate(index);
        // the rank the push crossed, which is the far side's third
        let crossed = match self.active_color {
            Color::White => 6,
            Color::Black => 3,
        };
        if rank != crossed {
            return false;
        }
        // where the pawn that pushed now stands, which is the square make_move
        // clears. The rank above is what puts it on the board
        let taken = match self.active_color {
            Color::White => index - 8,
            Color::Black => index + 8,
        };
        self.pawn_can_capture_on(index, self.active_color)
            && !(self.white | self.black).is_bit_set(index)
            && self.get_piece_and_color_index(taken) == Some((Piece::Pawn, !self.active_color))
    }

    /// Whether a pawn of this colour is placed to take on this square. A mask
    /// holds the squares a pawn of that colour must stand on to attack the one
    /// indexed, which is what is being asked here.
    fn pawn_can_capture_on(&self, index: u8, capturer: Color) -> bool {
        let attack_masks = &ATTACK_MASKS;
        let (from, pawns) = match capturer {
            Color::White => (attack_masks.white_pawns[index as usize], self.white),
            Color::Black => (attack_masks.black_pawns[index as usize], self.black),
        };
        from & self.pawns() & pawns != 0
    }

    /// The six piece boards by name. Each is a constant index into `pieces`,
    /// so these read as the fields they replaced and compile to the same load.
    #[inline]
    fn pawns(&self) -> u64 {
        self.pieces[Piece::Pawn as usize]
    }

    #[inline]
    fn knights(&self) -> u64 {
        self.pieces[Piece::Knight as usize]
    }

    #[inline]
    fn bishops(&self) -> u64 {
        self.pieces[Piece::Bishop as usize]
    }

    #[inline]
    fn rooks(&self) -> u64 {
        self.pieces[Piece::Rook as usize]
    }

    #[inline]
    fn queens(&self) -> u64 {
        self.pieces[Piece::Queen as usize]
    }

    #[inline]
    fn kings(&self) -> u64 {
        self.pieces[Piece::King as usize]
    }

    /// Where this side's king stands. Every board has exactly one king a side,
    /// which is what `from_fen` checks for: without a king this returns 64 and
    /// the attack masks are indexed off the end.
    fn king_index(&self, color: Color) -> u8 {
        let mask = match color {
            Color::White => self.white,
            Color::Black => self.black,
        };
        (self.kings() & mask).trailing_zeros() as u8
    }

    /// What the king shelter depends on and nothing else: both sides' pawns
    /// and both kings' squares.
    ///
    /// `shelter_counts` reads the pawn boards and the two king squares and
    /// nothing else, so two positions whose pawns and kings agree agree on
    /// every count of it whatever else has moved. This key stands in for that
    /// agreement rather than being it: two positions can share it and differ,
    /// which takes a collision across the whole sixty four bits. The pawn key
    /// already hashes the pawns of both colours and is kept in step move by
    /// move, so this is that key with the two kings folded in, from the same
    /// zobrist table the position key uses.
    ///
    /// Composed here rather than maintained beside `pawn_key`, because a
    /// king move would then have to write it and the cost of this is two
    /// loads and two xors at the one place that asks.
    #[inline]
    pub(crate) fn shelter_key(&self) -> u64 {
        self.pawn_key
            ^ ZOBRIST.get_piece_key(self.king_index(Color::White), Piece::King, Color::White)
            ^ ZOBRIST.get_piece_key(self.king_index(Color::Black), Piece::King, Color::Black)
    }

    /// What stands between this side's king and the board, as seven counts in
    /// the order `eval::SHELTER_TERMS` names them: this side's pawns one rank
    /// in front of the king and two ranks in front, how many of the king's
    /// three files hold no pawn of either colour, how many hold an enemy pawn
    /// and none of this side's, and then the enemy pawns one, two and three
    /// ranks in front of the king.
    ///
    /// All seven are read off the three files the king stands behind, which
    /// `king_files` steps in at the two corners so that the counts mean the
    /// same thing on every square. Pawns and nothing else: a piece in front of
    /// the king shelters it too, but a term that pays for one pays a piece to
    /// sit still, and the piece square tables already hold an opinion about
    /// where a piece belongs. A pawn is the part of the cover the king cannot
    /// get back.
    ///
    /// The last three are the storm, and they are the same masks read against
    /// the other side's pawns. A pawn of ours on g3 is cover and a pawn of
    /// theirs on g3 is not the absence of cover, it is a lever, so the two are
    /// counted apart and each rank apart from the next: how far the storm has
    /// come is most of what it is worth, and the weights are where that is
    /// said. Three ranks is as far as it is followed, which for a king at home
    /// reaches the fourth rank.
    ///
    /// The two file counts overlap the two pawn counts, since a file with no
    /// pawn of ours on it adds nothing to either of those. They are kept apart
    /// because they are different knowledge: a missing g pawn and a g pawn
    /// pushed to g4 both leave the near count short, and only the first opens
    /// the file to a rook.
    ///
    /// Nothing here is gated on the king standing at home. A king that has
    /// walked up the board has no rank in front of it inside the masks and
    /// counts nothing, so the term fades rather than falling off a cliff the
    /// search could step over.
    ///
    /// Both `eval` and the tuner's walk read this, so the identity between
    /// them cannot see a wrong count here. What pins it is the hand counts
    /// beside this in the tests, the way the mobility counts are pinned.
    #[inline]
    pub(crate) fn shelter_counts(&self, color: Color) -> [i32; eval::SHELTER_TERMS] {
        let masks = &SHELTER_MASKS;
        let square = self.king_index(color) as usize;
        let side = color as usize;
        let (ours, theirs) = match color {
            Color::White => (self.white, self.black),
            Color::Black => (self.black, self.white),
        };
        let pawns = self.pawns();
        let (our_pawns, their_pawns) = (pawns & ours, pawns & theirs);
        let ahead =
            |pawns: u64, rank: usize| (pawns & masks.ahead[rank][side][square]).count_ones() as i32;
        // the king's files with no pawn of ours on them, split by whether the
        // other side has one there
        let files = masks.files[square];
        let bare = files & !files_of(our_pawns);
        let theirs_on = files_of(their_pawns);
        let open = (bare & !theirs_on).count_ones() as i32;
        let half_open = (bare & theirs_on).count_ones() as i32;
        [
            ahead(our_pawns, 0),
            ahead(our_pawns, 1),
            open,
            half_open,
            ahead(their_pawns, 0),
            ahead(their_pawns, 1),
            ahead(their_pawns, 2),
        ]
    }

    /// What this side's pawns stand as, in the eight counts
    /// `eval::PAWN_TERMS` names: its passed pawns by relative rank, the
    /// second through the seventh, then its isolated pawns and its doubled
    /// ones.
    ///
    /// A pawn of ours is passed when no pawn of theirs stands on its file or
    /// either file beside it on any rank ahead of it, and no pawn of ours
    /// stands ahead of it on its own file. The second clause is what leaves
    /// the rear of a doubled pair out: the front pawn is the runner, and the
    /// one behind it is going nowhere the front one has not gone first. What
    /// stands on the square in front of the pawn is not read, so a passer a
    /// knight has blockaded is counted as a passer. That is on purpose and it
    /// is the first thing this term leaves out: the stop square reads the
    /// pieces, and a term that reads the pieces cannot sit behind a key over
    /// the pawns.
    ///
    /// A pawn is isolated when no pawn of ours stands on either file beside
    /// it, and doubled when a pawn of ours stands behind it on its own file.
    /// Both are counted per pawn rather than per file, so an isolated pair on
    /// one file pays the isolated weight twice and a tripled file is doubled
    /// two. Per pawn is what one coefficient can state; per file would want a
    /// second table to say how many.
    ///
    /// Relative rank is the rank a pawn has come, so a white pawn's is its
    /// rank and a black pawn's is nine less. The relative second is a real
    /// bucket and not a rounding of the others: a pawn still at home is
    /// passed the moment the enemy pawns on its three files are gone.
    ///
    /// Pawns and nothing else is read here, which is the property the pawn
    /// hash rests on. Neither king, no piece and not the side to move: two
    /// positions whose pawns agree agree on all eight counts, and
    /// `Board::pawn_key` already stands for that agreement.
    ///
    /// Both `eval` and the tuner's walk read this, so the identity between
    /// them cannot see a wrong count here, at the fitted weights or at zero.
    /// What pins it is the hand counts beside this in the tests, the way the
    /// shelter counts and the mobility counts are pinned.
    #[inline]
    pub(crate) fn pawn_structure_counts(&self, color: Color) -> [i32; eval::PAWN_TERMS] {
        let masks = &PAWN_MASKS;
        let side = color as usize;
        let (ours, theirs) = match color {
            Color::White => (self.white, self.black),
            Color::Black => (self.black, self.white),
        };
        let pawns = self.pawns();
        let (our_pawns, their_pawns) = (pawns & ours, pawns & theirs);
        let mut counts = [0; eval::PAWN_TERMS];
        let mut remaining = our_pawns;
        while remaining != 0 {
            let square = remaining.trailing_zeros() as usize;
            remaining &= remaining - 1;
            let relative = match color {
                Color::White => square / 8,
                Color::Black => 7 - square / 8,
            };
            // from_fen accepts a pawn on either back rank, knowingly, and the
            // search has to survive one. Such a pawn has come no ranks or all
            // eight and so names none of the six counted; it is left out of
            // the passed count rather than folded into the nearest bucket,
            // and it still counts toward the two below, which read its file
            // and not its rank
            if !(1..eval::PASSED_RANKS + 1).contains(&relative) {
                continue;
            }
            if their_pawns & masks.front_span[side][square] == 0
                && our_pawns & masks.file_ahead[side][square] == 0
            {
                counts[relative - 1] += 1;
            }
        }
        let files = files_of(our_pawns);
        let beside = (files << 1) | (files >> 1);
        counts[eval::PASSED_RANKS] = (our_pawns & spread(files & !beside)).count_ones() as i32;
        counts[eval::PASSED_RANKS + 1] =
            (our_pawns & ahead_of(our_pawns, color)).count_ones() as i32;
        counts
    }

    /// Whether the side to move stands in check, read from the checkers
    /// `make_move` maintains rather than by probing the king's square for an
    /// attack. The two would answer the same; this one is for the search,
    /// which asks at every node.
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

    /// The pieces checking the new side to move after the move just made,
    /// asked of the board after the move. Answered from the move rather than
    /// by probing the king square from scratch: only the piece that landed
    /// can check directly, which a pawn or knight settles with a mask, and a
    /// slider check needs the move to have touched a line through the king,
    /// by landing a slider on one or vacating a square that sat on one. Any
    /// slider a probe then finds is a check this move opened, because the
    /// king stood unattacked before it. Castling and en passant displace a
    /// second piece each and are rare, so they take the full probe instead
    /// of restating its cases.
    ///
    /// The direct and slider findings accumulate rather than short circuit:
    /// a move can uncover a slider while checking on its own, and a double
    /// check is answered differently to a single one.
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

    /// Whether this move checks the opponent, asked of the board before the
    /// move is made. `checkers_given` answers the same question of the board
    /// after it; this one is for a caller that wants the answer without
    /// paying for make and unmake, which is what the late move reduction's
    /// exemption asks.
    ///
    /// The occupancy is edited to what the move leaves: the from square
    /// emptied, the to square filled, and the extra square a castle or an en
    /// passant capture touches besides. A direct check is the landed piece
    /// attacking the king from its destination, a promotion attacking as the
    /// piece it becomes. The slider probes run from the king over the edited
    /// occupancy against our sliders as the move leaves them, so a discovered
    /// check needs no case of its own: the probe sees through whatever the
    /// move vacated. En passant empties the mover's square and the taken
    /// pawn's at once, the double vacation a discovered check can need, and
    /// castling is asked about the rook's destination, since a king cannot
    /// check.
    ///
    /// Exact for any move `generate_moves` produces here. The oracle test
    /// holds that over the legal ones; for a move `make_move` would refuse
    /// the construction is the same and the answer is what the made board
    /// would say, though no test makes a refused move to ask.
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
        // our sliders as the move leaves them: the mover gone from its
        // square, and standing on its destination when what landed there
        // slides. The captured piece, if any, was never in these
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
                // the mask holds the squares a pawn of our colour must stand
                // on to attack the king's square
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

    /// The pieces checking the side to move, computed from the board rather
    /// than maintained as moves are made, the way `square_attacked` asks its
    /// question but keeping the attackers instead of stopping at the first.
    /// `checkers` is meant to equal this at all times.
    fn recompute_checkers(&self) -> u64 {
        let king = self.king_index(self.active_color);
        let all = self.black | self.white;
        let attack_masks = &ATTACK_MASKS;
        let magic = &MAGIC;
        let (attacker_mask, pawn_masks) = match !self.active_color {
            Color::Black => (self.black, &attack_masks.black_pawns),
            Color::White => (self.white, &attack_masks.white_pawns),
        };
        let mut checkers = pawn_masks[king as usize] & self.pawns() & attacker_mask;
        checkers |= attack_masks.knights[king as usize] & self.knights() & attacker_mask;
        let bishop_or_queen = (self.bishops() | self.queens()) & attacker_mask;
        if attack_masks.diagonal[king as usize] & bishop_or_queen != 0 {
            checkers |= magic.get_diagonal_move(king, all) & bishop_or_queen;
        }
        let rook_or_queen = (self.rooks() | self.queens()) & attacker_mask;
        if attack_masks.straight[king as usize] & rook_or_queen != 0 {
            checkers |= magic.get_straight_move(king, all) & rook_or_queen;
        }
        // a king cannot give check, so unlike square_attacked there is no
        // king term
        checkers
    }

    /// Drop the moves that cannot answer the check the side to move stands
    /// in. The legal answers to a check are moving the king, capturing the
    /// sole checker, or blocking the sole checker's line, so a move doing
    /// none of them can be refused without being played; the checker, its
    /// line and the king are found once for the whole list rather than once
    /// per move. The moves kept still go through `make_move`, which settles
    /// pins and squares the king may not step to. Refusing here only spares
    /// that work for moves it would certainly refuse. En passant is kept
    /// unexamined: the captured pawn does not stand on the to square, so the
    /// capture and block masks misread it, and it is rare.
    ///
    /// The generator masks its targets instead, so this is no longer on the
    /// search's path. It is kept as the second implementation the masked
    /// generator is held to, two ways of saying which moves answer a check,
    /// pinned against each other by
    /// `the_masked_generator_keeps_what_the_filter_kept` over a corpus of
    /// positions in check. Do not fold it into the generator: then there
    /// would be one statement of the rule and nothing to check it against.
    #[cfg(test)]
    fn retain_evasions(&self, moves: &mut MoveList) {
        debug_assert!(self.checkers != 0, "asked of a position not in check");
        let targets = if self.checkers.count_ones() > 1 {
            // only the king can answer a double check
            0
        } else {
            let checker = self.checkers.trailing_zeros() as usize;
            let king = self.king_index(self.active_color) as usize;
            self.checkers | BETWEEN[king][checker]
        };
        let kings = self.kings();
        // Compacted in place rather than through `retain`, which reaches the
        // list through its index operator once per move and asks each time
        // whether the list has spilled to the heap. One slice taken here
        // answers that once for the whole list.
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

    /// Move a piece between two squares, which is neither a placement nor a
    /// removal.
    ///
    /// Clearing one square and setting the other says the same thing, but it
    /// says it in halves that cancel: the piece never leaves the board, so
    /// what it is worth and what it counts for the phase are the same on both
    /// squares, and each board it stands on ends with the bits it started
    /// with. Only the two squares differ, and only they are touched here.
    #[inline(always)]
    fn relocate_piece_index(&mut self, from: u8, to: u8, piece: Piece, color: Color) {
        debug_assert!(from != to);
        debug_assert!(from < 64 && to < 64);
        let moved =
            ZOBRIST.get_piece_key(from, piece, color) ^ ZOBRIST.get_piece_key(to, piece, color);
        self.key ^= moved;
        // and the pawn key with it when it is a pawn that moved, on the same
        // randoms and so on the value already in hand
        if piece == Piece::Pawn {
            self.pawn_key ^= moved;
        }
        self.eval.relocate(from, to, piece, color);

        // one board keeps its population, so the two bits flip together
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

    /// The two directions written once. They are the same walk with every
    /// sign reversed, and `SET` is settled at compile time, so each caller
    /// above monomorphises into what was spelled out twice before: no branch
    /// on it survives into the search.
    #[inline(always)]
    fn move_accumulators<const SET: bool>(&mut self, index: u8, piece: Piece, color: Color) {
        let piece_key = ZOBRIST.get_piece_key(index, piece, color);
        self.key ^= piece_key;
        // the pawn key is the same randoms over the pawns alone, so it takes
        // the value already loaded rather than a second lookup
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
        // and the same news told to `squares`. The callers above assert that a
        // set lands on an empty square and a clear on an occupied one, so this
        // never has to ask what was standing there
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

    /// What is being taken on the to square, without asking when nothing can
    /// be. Most of the moves generated are quiet, so the mask of squares a
    /// capture is even possible on is still worth a look first: it answers
    /// from a register for the moves that are, and the load below is only
    /// reached for the ones that might not be.
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
        // masked so the read carries no bounds check, which is the bargain the
        // bitboard accessors already strike: the assert above is what catches
        // a square off the board, and it is a debug assert, so the search pays
        // nothing for it.
        self.squares[(index & 63) as usize]
    }

    /// Walks the six piece boards rather than reading `squares`. The
    /// recomputes reach a piece through here and everything else reaches one
    /// through `get_piece_index`, which keeps a mistake in either from hiding
    /// itself in the state check.
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

    /// The material of each side, counted a bitboard at a time. Says the same
    /// thing as the eval module's recompute and shares no code with it on
    /// purpose: `from_fen` seeds the accumulator from this one, and the state
    /// check compares it against that one. Collapse the two and a freshly
    /// parsed board would be checked against the function that filled it in.
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
    /// legal list.
    ///
    /// It has to come to the same number. Every legal move answers a check
    /// when there is one, so the legal moves sit inside what `evasions`
    /// returns, which sits inside what `generate_moves` returns; `make_move`
    /// refuses the rest either way. So this is the masked generator held to
    /// the counts the perft suites already pin, over every position they
    /// reach rather than the handful a test can name. Perft itself walks
    /// `generate_moves`, so without this the evasion mask has no exhaustive
    /// check at all.
    ///
    /// The checkers are maintained here, as `perft_as_played` maintains
    /// them, because that is what the mask is read off. The plain `perft`
    /// leaves them stale, which no caller in the search does.
    #[cfg(test)]
    pub(crate) fn perft_through_evasions(&mut self, depth: u8) -> u64 {
        self.perft_impl::<true, true>(depth)
    }

    /// The same count, walked the way the engine plays: checkers maintained
    /// as each move is made, and the legality probe skipped wherever those
    /// checkers say it can be.
    ///
    /// A correct board counts the same either way, which is the whole of
    /// what this is for. The counts the perft suites pin are the accepted
    /// ones, hand verified and exhaustive, but the walk that produced them
    /// is not the walk a game takes: `checkers_given` and the skip it feeds
    /// are compiled out of it. Asking for the same positions again this way
    /// puts those two under the same counts. A skip that wrongly cleared a
    /// move would let an illegal one stand and the count would rise; a
    /// `checkers_given` that wrongly said no check would clear the skip the
    /// same way. In a debug build the assertion beside `checkers_given`
    /// catches the harmless direction too, over every move of every position
    /// rather than the few thousand a proptest reaches.
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
    /// Validated only as far as what the search cannot survive: see the
    /// checks below and the known limitations in `docs/ROADMAP.md` for what
    /// an illegal position can still get away with. A field that describes
    /// pieces the placement does not have is cut back rather than refused,
    /// since the position itself is playable and only the field is not.
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
            // an empty board, filled in by the `set_piece` calls below along
            // with everything else the accumulators carry
            squares: [None; 64],

            active_color: Color::from_char(active_color_token)
                .ok_or("Failed to parse active color from token")?,
            castle: CastlePermissions::from_fen(castle)?,

            ply: move_number * 2,
            line_ply: 0,
            move_number,
            en_passant: Coordinate::from_string(en_passant)?,
            // filled in below, once the pieces are on the board
            checkers: 0,
            fifty_move_rule: half_move_clock
                .parse::<usize>()
                .map_err(|e| e.to_string())?,
            eval: Accumulator::EMPTY,

            history: EMPTY_HISTORY,
            key: INITIAL_KEY,
            // an empty board has no pawns on it, and the `set_piece` calls
            // below fold in each one that arrives
            pawn_key: 0,
        };
        if board.active_color == Color::Black {
            board.ply += 1;
        }

        // parse out the pieces on the board
        let mut rank = 8;
        // counted as a number rather than held as a File, because a complete
        // rank ends one square past the h file, which is not a File a square
        // can have
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
        // Everything below assumes a position which could actually arise, and
        // crashes rather than playing badly when it could not. A king a side is
        // what lets king_index return a real square, and the side which just
        // moved being out of check is what stops the search replying by taking
        // the king and emptying that square again.
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

        // A right and an en passant square each describe pieces the rest of
        // the fen need not agree with, and a field kept here is one the
        // generator will play: see the two rules for what that costs. Each is
        // cut back beside the key it belongs in, so the two never disagree.
        board.castle = board.rights_the_pieces_bear_out();

        // fold the non-piece state into the position key so that keys are
        // comparable between boards parsed from FEN and boards reached by
        // playing moves
        if board.active_color == Color::Black {
            board.key ^= ZOBRIST.side;
        }
        board.key ^= ZOBRIST.castle_key(board.castle);
        // make_move's own rule is the first of these, or a position parsed and
        // the same one played would not hash alike, which is worse than what
        // is being fixed
        if let Some(en_passant) = board.en_passant {
            if board.en_passant_can_be_played(en_passant.as_index()) {
                board.key ^= ZOBRIST.en_passant_key(en_passant.as_index());
            } else {
                board.en_passant = None;
            }
        }
        board.eval.seed_material(board.material_value());
        board.checkers = board.recompute_checkers();
        // a parsed position must satisfy the same invariants a played one
        // does, or the two ways of reaching a position drift apart
        board.debug_assert_state_in_step();
        Ok(board)
    }

    /// The position as a fen, all six fields, which `from_fen` reads back.
    ///
    /// The clocks are printed as well as the pieces, so the fifty move
    /// counter travels with the position and a board printed here is scored
    /// for a draw the way this one is. What does not travel is the path: a
    /// fen names the position and nothing that was played to reach it, so a
    /// board parsed back from one has no history to find a repetition in.
    ///
    /// The castle rights and the en passant square are printed as the board
    /// holds them, which is after `from_fen` has cut back what the pieces do
    /// not bear out. So a fen parsed and printed again may differ from the
    /// one that arrived, and printing that one twice does not.
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
            self.move_number,
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
            self.move_number,
            self.fifty_move_rule,
            self.eval.material_difference(),
        )?;
        writeln!(f)?;
        Ok(())
    }
}

/// Positions the test modules share, named for what they bring within reach.
/// Kept here so a fen appears once, and so a suite that wants, say, a position
/// with promotions available does not grow another copy with a different move
/// counter.
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
    /// A sharp middlegame: white's knight on g5 and bishop on d3 are aimed at
    /// the castled black king while the white king is still on e1. Sharp
    /// enough that a wrongly reused score would move the verdict, which is
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
    *board
        .generate_moves()
        .iter()
        .find(|m| format!("{}", m) == name)
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

    /// The shuffle position with its move number wound on, so the board it
    /// parses to stands at ply 1023. That is one short of the end of the
    /// history ring, which the two tests below play across.
    const NEAR_THE_WRAP: &str =
        "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 b - - 3 511";

    /// The history is a ring, so a game long enough to run past the end of it
    /// wraps instead. The cycle below is recorded either side of the wrap.
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

    /// Unmaking reads back the entry making wrote, so it has to agree about
    /// where the wrap put it.
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

    /// The search does not wait for the third occurrence. Once a position has
    /// come back once, either side can take the draw, so there is nothing to be
    /// gained by spending four more plies of depth confirming it.
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

    /// A pass changes the position and unmaking it gives back a board equal
    /// in every field, which is what the search relies on either side of one.
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

    /// No piece moves, so everything kept incrementally has to come out of a
    /// pass equal to a recompute. The debug build asserts this inside the
    /// pass itself; this says it in a release build too, which is where the
    /// bench and the tactical suite run.
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

    /// A pass captures nothing and moves no pawn, so the counter runs on,
    /// and comes back where it was.
    #[test]
    fn a_pass_runs_the_fifty_move_counter_on() {
        let mut board = Board::from_fen("4k3/8/8/8/8/8/8/R3K3 w - - 37 40").unwrap();
        assert_eq!(board.fifty_move_rule, 37);
        board.make_null_move();
        assert_eq!(board.fifty_move_rule, 38);
        board.undo_null_move();
        assert_eq!(board.fifty_move_rule, 37);
    }

    /// The en passant square goes with the move, as it does on any move: it
    /// was the right of the side that has just passed the move on, and a
    /// square nobody can take on fails the board's own check.
    #[test]
    fn a_pass_clears_the_en_passant_square() {
        // white has just pushed d2-d4 past a black pawn on e4, so the square
        // it passed over is one black can take on and is in the key
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

    /// A line through a pass is not a path a repetition rule knows about, so
    /// coming back to the position the pass was made from is not a draw. Both
    /// rooks travel home again here, one of them in three moves and the other
    /// in two, which is what lets an odd number of plies undo a pass; the key
    /// assertion is what says an unsalted entry would have matched.
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
    use super::{A1, A8, CASTLE_LANDING, CASTLE_LEAVING, E1, E8, H1, H8};
    use super::{CastlePermissions, castle_bits, castle_rights};
    use pretty_assertions::assert_eq;

    /// The rule the two tables stand for, written the way make_move wrote
    /// it before them: a match on the square the move leaves and a match on
    /// the square it lands on. Kept here rather than deleted, so that what
    /// the tables have to agree with is a second statement of the rule and
    /// not the tables themselves.
    fn by_hand(mut rights: CastlePermissions, from: u8, to: u8) -> CastlePermissions {
        match from {
            A1 => rights.white_queen_side = false,
            E1 => {
                rights.white_queen_side = false;
                rights.white_king_side = false;
            }
            H1 => rights.white_king_side = false,
            A8 => rights.black_queen_side = false,
            E8 => {
                rights.black_queen_side = false;
                rights.black_king_side = false;
            }
            H8 => rights.black_king_side = false,
            _ => (),
        }
        match to {
            A1 => rights.white_queen_side = false,
            H1 => rights.white_king_side = false,
            A8 => rights.black_queen_side = false,
            H8 => rights.black_king_side = false,
            _ => (),
        }
        rights
    }

    #[test]
    fn the_tables_take_what_the_matches_took() {
        for held in 0..16u8 {
            let rights = CastlePermissions {
                black_king_side: held & 1 != 0,
                black_queen_side: held & 2 != 0,
                white_king_side: held & 4 != 0,
                white_queen_side: held & 8 != 0,
            };
            for from in 0..64u8 {
                for to in 0..64u8 {
                    // SAFETY: the bytes are those of three sets of
                    // rights anded together, so each is still a `bool`'s.
                    let masked = unsafe {
                        castle_rights(
                            castle_bits(rights)
                                & CASTLE_LEAVING[from as usize]
                                & CASTLE_LANDING[to as usize],
                        )
                    };
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
        // e6 is out of reach of every white pawn at the start. Hash the bogus
        // square into the key as well as the field, so the key still matches
        // its recompute and only the rule itself can object: this is exactly
        // the corruption the recompute comparison is blind to.
        board.en_passant = Coordinate::from_string("e6").unwrap();
        board.key ^= ZOBRIST.en_passant_key(board.en_passant.unwrap().as_index());
        board.debug_assert_state_in_step();
    }

    /// The castle rights are held to their pieces the same way, and this is
    /// the corruption that check exists for: the rights a fen states are the
    /// squares the generator castles from.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "a castle right without the king and rook for it")]
    fn a_castle_right_without_its_rook_fails_the_state_check() {
        use super::ZOBRIST;
        let mut board = Board::from_fen("4k3/8/8/8/8/8/8/4K3 w - - 0 1").unwrap();
        // the right is folded into the key as well as the field, for the
        // reason above
        let without = board.castle;
        board.castle.white_king_side = true;
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
        // every black pawn is still on the seventh, so nothing can take on e3.
        // Interfaces leave the square out of the key in that case, and a key
        // which disagrees makes one position hash two ways
        let without =
            Board::from_fen("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1").unwrap();
        let with =
            Board::from_fen("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1").unwrap();
        assert_eq!(without.key, with.key);
        assert_eq!(with.en_passant, None);
    }

    #[test]
    fn a_double_push_no_one_can_answer_hashes_like_the_position_without_it() {
        // the same position played and parsed has to hash alike, which is the
        // half of this that has to match make_move or the fix is worse than the
        // problem
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
        // the key of a position reached by playing moves must equal the key of
        // the same position parsed directly from FEN
        let mut board = Board::new();

        play_move(&mut board, "e2e4");
        let fen =
            Board::from_fen("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1").unwrap();
        assert_eq!(board.key, fen.key);

        // after the reply the en passant rights expire and the key must no
        // longer include them (this used to leave a stale en passant key)
        play_move(&mut board, "g8f6");
        let fen = Board::from_fen("rnbqkb1r/pppppppp/5n2/8/4P3/8/PPPP1PPP/RNBQKBNR w KQkq - 1 2")
            .unwrap();
        assert_eq!(board.key, fen.key);

        // moving the king drops white's castle rights, and the fen for the
        // same position states the two black still holds
        play_move(&mut board, "e1e2");
        let fen =
            Board::from_fen("rnbqkb1r/pppppppp/5n2/8/4P3/8/PPPPKPPP/RNBQ1BNR b kq - 2 2").unwrap();
        assert_eq!(board.key, fen.key);
        // the rights are in the key, so the same pieces with black's gone as
        // well is a different position. A fen cannot make the other half of
        // this point any more: one claiming the rights white just gave up has
        // them dropped again by from_fen, since the king is no longer on e1
        let none =
            Board::from_fen("rnbqkb1r/pppppppp/5n2/8/4P3/8/PPPPKPPP/RNBQ1BNR b - - 2 2").unwrap();
        assert_ne!(board.key, none.key);
    }

    #[test]
    fn key_is_path_independent() {
        // reaching the same position via different move orders (with different
        // numbers of double pawn pushes on the way) must produce the same key
        let mut a = Board::new();
        for m in ["e2e4", "d7d5", "g1f3", "b8c6"] {
            play_move(&mut a, m);
        }
        let mut b = Board::new();
        for m in ["g1f3", "d7d5", "e2e4", "b8c6"] {
            play_move(&mut b, m);
        }
        // Note: both lines end with a knight move so any en passant rights
        // created along the way have expired in both final positions
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

    /// One position and one move for each way a pawn can appear, disappear or
    /// travel. Each has to move the key, agree with the recompute once made,
    /// and come back on the unmake.
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

    /// A move that touches no pawn leaves the key exactly as it was, which is
    /// what makes the key a name for the structure rather than for the
    /// position.
    #[test]
    fn a_move_that_touches_no_pawn_leaves_the_key_alone() {
        let board = Board::from_fen(fens::MIDDLEGAME).unwrap();
        let mut played = board.clone();
        play_move(&mut played, "c3d5");
        assert_ne!(board.key, played.key);
        assert_eq!(board.pawn_key, played.pawn_key);
    }

    /// Every move of a position, made and unmade, against a recompute both
    /// times. The debug build asserts this inside make_move; this says it in
    /// a release build too.
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

    /// A pass moves no piece, so the pawns are where they were and so is the
    /// key.
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

    /// The pieces are not in the key: two positions with the same pawns
    /// behind different pieces are one structure and share it.
    #[test]
    fn the_same_pawns_behind_different_pieces_share_a_key() {
        let bare = Board::from_fen("4k3/pppppppp/8/8/8/8/PPPPPPPP/4K3 w - - 0 1").unwrap();
        let full = Board::from_fen(fens::START).unwrap();
        assert_eq!(bare.pawn_key, full.pawn_key);
        assert_ne!(bare.key, full.key);
    }

    /// Nor is anything else the position key carries: the side to move, the
    /// castle rights and the en passant square all change the key and leave
    /// this one where it was.
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

    /// A position played to and the same position parsed have the same key,
    /// which is what says `from_fen` builds it rather than inheriting one.
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

    /// An empty board of pawns is an empty key, so a structure that trades
    /// every pawn off comes back to where it started.
    #[test]
    fn a_board_with_no_pawns_has_no_key() {
        let board = Board::from_fen("4k3/8/8/8/8/8/8/4K3 w - - 0 1").unwrap();
        assert_eq!(board.pawn_key, 0);
    }

    /// `shelter_key` is this key with the two kings folded in, which is what
    /// the shelter cache is keyed on. What the shelter reads is the pawns and
    /// the two king squares, so a piece that is neither has to leave it alone
    /// and either king moving has to move it. A key that missed a king would
    /// hand one position's shelter to another.
    #[test]
    fn the_shelter_key_follows_the_pawns_and_the_two_kings() {
        let bare = Board::from_fen("4k3/pppppppp/8/8/8/8/PPPPPPPP/4K3 w - - 0 1").unwrap();

        // the same pawns and the same two kings behind a boardful of other
        // pieces, which the shelter does not read
        let pieced =
            Board::from_fen("rnbqk1nr/pppppppp/8/8/8/8/PPPPPPPP/RNBQK1NR w - - 0 1").unwrap();
        assert_eq!(bare.shelter_key(), pieced.shelter_key());

        // either king one square along
        let ours = Board::from_fen("4k3/pppppppp/8/8/8/8/PPPPPPPP/5K2 w - - 0 1").unwrap();
        let theirs = Board::from_fen("5k2/pppppppp/8/8/8/8/PPPPPPPP/4K3 w - - 0 1").unwrap();
        assert_ne!(bare.shelter_key(), ours.shelter_key());
        assert_ne!(bare.shelter_key(), theirs.shelter_key());
        assert_ne!(ours.shelter_key(), theirs.shelter_key());

        // and one pawn pushed, the pawn key being the rest of it
        let pushed = Board::from_fen("4k3/pppppppp/8/8/8/7P/PPPPPPP1/4K3 w - - 0 1").unwrap();
        assert_ne!(bare.shelter_key(), pushed.shelter_key());
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

    /// The same counts again, walked over `evasions` rather than the whole
    /// pseudo legal list. The masked generator drops only moves `make_move`
    /// would refuse, so the count cannot move. See `perft_through_evasions`.
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

    /// The same counts, walked the way the engine plays: the numbers above
    /// are the accepted ones, and this is what holds the game's own path to
    /// them. See `perft_as_played`.
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

    /// Everything a board has to satisfy, whether it was played to or parsed.
    ///
    /// The recompute comparisons are what `debug_assert_state_in_step`
    /// checks on every made move: state kept incrementally has to equal a
    /// recompute from the pieces, or the search reads a score, a key or a
    /// check that the position does not have. The rest cannot fail on a board
    /// that was played to, because make_move never produces them, and can
    /// fail on a board built from a string somebody sent us.
    pub(super) fn prop_assert_in_step(board: &Board) -> Result<(), TestCaseError> {
        // one board a piece, so two of them sharing a bit is a square with
        // two pieces on it and nothing downstream would notice
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

    /// A piece placement field that is always well formed: eight ranks of
    /// eight squares with one king a side. Everything else about it is
    /// random, pawns on the back rank included, because from_fen accepts
    /// those knowingly and the search has to survive one.
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
                // exactly one king a side, put in last so nothing above can
                // have taken the square or added a second
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

    /// A fen the parser will usually accept. This is the one that reaches the
    /// coherence check below; a malformed fen is refused before there is a
    /// board to check, so a generator that only produced those would be
    /// asking nothing at all. The gives_check oracle borrows it for the same
    /// reason it exists here: positions nobody thought to write down.
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

    /// One rank of a placement field built out of parts that are plausible on
    /// their own, which as a whole will almost never sum to eight.
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

    /// A fen assembled from parts that are each plausible and together are
    /// usually nonsense. This one is about the refusal path: every field can
    /// be wrong in a different way, and none of them may crash the parser.
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

    /// A fen that was valid until one edit landed on it. Nearer to what a
    /// buggy interface sends than anything assembled from parts, and it is
    /// what reaches the paths only a nearly-right fen gets to.
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

    /// The coherence tests below say something only about the fens that are
    /// accepted, so a generator that stopped producing any would leave them
    /// passing and asking nothing. This is what says so out loud.
    ///
    /// About a third get through, and the two thirds that do not are all one
    /// rule: pieces scattered at random leave the side not to move in check
    /// most of the time, and from_fen refuses those because the search would
    /// answer by taking the king. So the floor is a tenth, well under what
    /// the generator manages. What is being caught is a generator that has
    /// collapsed to nearly nothing, not one that drifted by a few per cent.
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

    /// The printer's job: a position printed and parsed again is the same
    /// position. No fen here has an en passant square, and the rights each
    /// states have their pieces, so nothing the parser may cut back is in
    /// one and the text comes back word for word too.
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

    /// The clocks are printed as well as the pieces, so a position eighty
    /// half moves into a shuffle comes back eighty half moves in rather than
    /// fresh. That is what the residual sampler needs of the printer: a
    /// position it prints is scored for the fifty move rule the way the
    /// search that printed it scored it.
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

        /// The parser cuts back a castle right and an en passant square the
        /// pieces do not bear out, so the first print of a parsed fen need
        /// not match the text that arrived. Printing it again does, and that
        /// is what a caller who prints a position and parses it back depends
        /// on.
        #[test]
        fn printing_a_parsed_position_is_settled_after_one_pass(fen in well_formed_fen()) {
            if let Ok(board) = Board::from_fen(&fen) {
                let printed = board.to_fen();
                let parsed = Board::from_fen(&printed).expect("what we print, we parse");
                prop_assert_eq!(parsed.to_fen(), printed);
                prop_assert_eq!(parsed.key, board.key);
            }
        }

        /// A refusal is always allowed. What is not allowed is accepting a
        /// board that is not in step: the search trusts everything from_fen
        /// hands it, and a key that does not match the pieces poisons the
        /// table for the rest of the game.
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

        /// The board a fen is accepted as being in step says nothing about
        /// the moves it licenses. A castle right or an en passant square the
        /// rest of the position does not agree with parses into a coherent
        /// board and corrupts it one move later, when make_move reads the
        /// right as a description of where the pieces are. So every legal
        /// move is played and the board asked again.
        #[test]
        fn every_legal_move_of_a_well_formed_fen_leaves_the_board_in_step(fen in well_formed_fen()) {
            if let Ok(mut board) = Board::from_fen(&fen) {
                let before = board.clone();
                for m in &board.generate_moves() {
                    if board.make_move(m) {
                        super::in_step::prop_assert_in_step(&board)?;
                        board.undo_move();
                    }
                    // by hand rather than prop_assert_eq, which would print
                    // two boards and the thousand plies of history each
                    // carries
                    prop_assert!(board == before, "{} did not unmake", m);
                }
            }
        }
    }

    /// The rows `rights_the_pieces_bear_out` is there for, and the position
    /// that found it first. A right names pieces as well as a side, and what
    /// the generator did with one whose pieces were elsewhere is written
    /// over that function.
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

    /// An en passant square is a claim about the pawn that has just passed
    /// over it, and the parser only asked whether anything could capture
    /// there. A square with no pawn behind it produced a capture whose
    /// make_move cleared a pawn from a square holding something else.
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

    /// The other half of the rule above: a square the position does bear out
    /// is kept, which is what stops the check dropping every one of them.
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

    /// Each of these parsed happily and then took the engine down on the first
    /// search, which is the worst place to find out: mid game, from a position
    /// the interface sent us.
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

    /// The side to move being in check is the ordinary case and has to stay
    /// accepted, which is what stops the check above rejecting real positions.
    #[test]
    fn a_position_where_the_side_to_move_is_in_check_is_accepted() {
        assert!(Board::from_fen("4k3/8/8/8/8/8/8/4R1K1 b - - 0 1").is_ok());
    }
}

#[cfg(test)]
mod perft_edge_cases {
    use super::{Board, fens};
    use pretty_assertions::assert_eq;

    /// Positions that the six standard perft positions do not reach: the two
    /// en passant pins, en passant giving check, castling into or through an
    /// attack, promoting out of check, and stalemate. Each entry was checked
    /// against python-chess rather than transcribed, since a published table is
    /// only worth as much as the copy of it.
    const CASES: [(&str, u8, u64, &str); 24] = [
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
            "8/8/8/2k5/2pP4/8/B7/4K3 b - d3 0 3",
            1,
            8,
            "en passant would expose the king",
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
            // taking en passant empties two squares, and the one the
            // captured pawn stood on is the one that mattered: it blocked
            // the bishop's diagonal to the king. The capturing pawn comes
            // from a square on no line through the king at all, so nothing
            // about where it started says the capture is worth checking
            "1b5k/8/8/3Pp3/8/8/7K/8 w - e6 0 2",
            1,
            6,
            "en passant uncovers a bishop on a diagonal the capturer never stood on",
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

    /// The same cases through `evasions`. These are the shapes most likely
    /// to catch the evasion mask out: the promotions that answer a check,
    /// the en passant captures the mask deliberately does not examine, and
    /// the pins that leave a move looking like an answer when it is not.
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

    /// The same cases, walked the way the engine plays. These are the shapes
    /// most likely to catch checkers maintenance out: the pins, the en
    /// passant discoveries, the castles through an attacked square and the
    /// promotions that answer a check.
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

    /// "d4" to the index the board uses, so the cases below read as squares
    /// rather than as arithmetic.
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

    /// A middlegame with the kings castled on opposite wings, where no move
    /// can castle, take en passant or promote. The test that refuses a foreign
    /// move names its squares.
    const OPPOSITE_WINGS: &str =
        "r2q1rk1/1b1nbppp/p2ppn2/1p6/3NPP2/1BN1B3/PPPQ2PP/2KR3R w - - 0 13";

    const POSITIONS: [&str; 5] = [
        fens::START,
        fens::KIWIPETE,
        fens::PAWN_ENDGAME,
        fens::PROMOTIONS,
        OPPOSITE_WINGS,
    ];

    /// The point of the check is to accept what the generator produces: a move
    /// refused here is one the search declines to play early and has to find
    /// again the slow way. Castling, en passant and promotions are refused on
    /// purpose, and this pins that they are the only ones.
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

    /// A move handed back for another position can say anything at all, and
    /// make_move acts on what it says. These are the shapes that would corrupt
    /// the board if they were played.
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

        // and the moves those are variations of, so none of it passes vacuously
        assert!(board.is_pseudo_legal(&quiet("d4", "f5")));
        assert!(board.is_pseudo_legal(&takes("d4", "e6", Piece::Pawn)));
    }

    /// A pawn push is the one move whose legality turns on squares the move
    /// itself never names.
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
        // the knight on f6 and the rook on e1 both check, and no move
        // answers both, so the king has to move. Black has a rook and a pawn
        // with moves of their own for the filter to drop
        let board = Board::from_fen("r3k3/7p/5N2/8/8/8/8/4R1K1 b - - 0 1").unwrap();
        assert_eq!(answers(&board), Vec::<String>::new());
        assert!(
            board.evasions().len() < board.generate_moves().len(),
            "the filter dropped nothing"
        );
    }

    #[test]
    fn a_slider_check_may_be_taken_or_blocked() {
        // the rook on e1 checks up the file. The rook on a1 can take it and
        // the knight can step in front of it at e2 or e6; the knight's other
        // six moves and the rook's whole file answer nothing
        let board = Board::from_fen("4k3/8/8/8/3n4/8/8/r3R1K1 b - - 0 1").unwrap();
        assert_eq!(answers(&board), vec!["a1e1", "d4e2", "d4e6"]);
    }
}

#[cfg(test)]
mod random_games {
    use super::{Board, fens};
    use proptest::prelude::*;

    /// Walks start from positions with different machinery in reach: the
    /// opening with castling ahead of it, a tactical middlegame, a bare
    /// endgame, and a position full of promotions.
    const STARTS: [&str; 4] = [
        fens::START,
        fens::KIWIPETE,
        fens::PAWN_ENDGAME,
        fens::PROMOTIONS,
    ];

    proptest! {
        /// Play a random line of moves, checking on every ply that the
        /// incrementally maintained state agrees with a recompute, then unmake
        /// the whole line and check that every position comes back exactly.
        ///
        /// The fixed-position reversible tests do this one ply deep from
        /// positions somebody thought to write down; this walks lines nobody
        /// did, and a failure arrives already shrunk to a short one. The same
        /// walk sweeps is_pseudo_legal, whose fixed tests also only see
        /// positions somebody chose.
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
                    // checkmate or stalemate: the line is over
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
                // every move the evasion filter drops has to be one
                // make_move refuses: a legal evasion dropped would read as a
                // mate to the search that trusts the filter
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
                    // a move refused for leaving the king attacked has to
                    // leave the board exactly as it found it
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

    /// The table used to be built by probing the magic tables with only the two
    /// endpoints occupied and intersecting what each end saw. The ray walk that
    /// replaced it has to answer identically for every one of the four thousand
    /// pairs, aligned and not, or a check would be answered with the wrong
    /// squares.
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

    /// Winning a piece does not end the story. The rook takes a queen a
    /// rook defends, is taken back, and the swap prices queen for rook,
    /// not the queen outright.
    #[test]
    fn a_won_piece_is_still_recaptured() {
        assert_eq!(see_of("3r3k/3q4/8/8/8/8/8/3R3K w - - 0 1", "d1d7"), 400);
    }

    /// Doubled rooks against a defended pawn: the front rook takes, and the
    /// one behind it joins the swap the moment the line opens. Without the
    /// x-ray the same capture would read as losing the rook.
    #[test]
    fn a_rook_behind_the_capturing_rook_joins_the_exchange() {
        assert_eq!(see_of("3rk3/8/8/3p4/8/8/3R4/3R2K1 w - - 0 1", "d2d5"), 100);
    }

    /// The pawn is defended twice. The bishop could take the recapturing
    /// pawn, but pressing on only feeds the second defender, so the swap
    /// prices the capture as knight for pawn and stops there.
    #[test]
    fn the_swap_stops_rather_than_feed_the_second_defender() {
        assert_eq!(
            see_of("4k3/8/3p1p2/4p3/8/5N2/1B6/4K3 w - - 0 1", "f3e5"),
            -200
        );
    }

    /// En passant lifts the taken pawn from its own square, not the target:
    /// plain and defended cases first, then a rook backing the capture
    /// through the square the taken pawn left.
    #[test]
    fn en_passant_opens_the_taken_pawns_square() {
        assert_eq!(see_of("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6"), 100);
        assert_eq!(see_of("4k3/2p5/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6"), 0);
        assert_eq!(see_of("4k3/2p5/8/3pP3/8/8/8/3RK3 w - d6 0 1", "e5d6"), 100);
    }

    /// A capture that promotes values the piece it takes. The queen that
    /// appears is counted as the pawn it was, the documented approximation,
    /// so the knight takes it back at a pawn's price and the exchange comes
    /// to rook for pawn.
    #[test]
    fn a_promoting_capture_values_the_piece_taken() {
        assert_eq!(see_of("r3k3/1Pn5/8/8/8/8/8/4K3 w - - 0 1", "b7a8q"), 400);
    }

    /// A king may recapture only where nothing answers it: with a second
    /// rook behind the first the king cannot legally take, so the pawn is
    /// simply won; without it the same capture loses the rook.
    #[test]
    fn a_king_capture_ends_the_sequence() {
        assert_eq!(see_of("8/8/2k5/3p4/8/8/3R4/3R2K1 w - - 0 1", "d2d5"), 100);
        assert_eq!(see_of("8/8/2k5/3p4/8/8/3R4/6K1 w - - 0 1", "d2d5"), -400);
    }

    /// The model `see` answers within, played out in full: either side may
    /// capture with any attacker or stop, and a taken king ends the line at
    /// the king's price. Free choice of attacker, where `see` commits to the
    /// least valuable, so agreement says the commitment loses nothing.
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

    /// Every capture two plies deep from the core positions, plus the two
    /// perft positions thick with captures and promotions, priced by `see`
    /// and by the exhaustive negamax. The count asserts the walk really
    /// visited the captures it was pointed at.
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
mod try_make {
    use super::fens;
    use super::play_named;
    use super::{Board, Color};
    use pretty_assertions::assert_eq;

    #[test]
    fn a_move_from_another_position_is_refused_and_changes_nothing() {
        let foreign = play_named(&Board::from_fen(fens::KIWIPETE).unwrap(), "e2a6");
        let mut board = Board::new();
        let before = board.clone();
        assert!(!board.try_make(&foreign));
        assert_eq!(board, before);
    }

    #[test]
    fn a_move_the_position_has_moved_on_from_is_refused() {
        let mut board = Board::new();
        let opening = play_named(&board, "e2e4");
        assert!(board.try_make(&opening));
        // the same value again: the pawn is no longer on e2
        assert!(!board.try_make(&opening));
        assert_eq!(board.active_color(), Color::Black);
    }

    #[test]
    fn a_castle_is_made() {
        // is_pseudo_legal would refuse this; try_make checks by generating
        let mut board = Board::from_fen(fens::KIWIPETE).unwrap();
        let castle = play_named(&board, "e1g1");
        assert!(castle.castle);
        assert!(board.try_make(&castle));
        assert_eq!(board.active_color(), Color::Black);
    }

    #[test]
    fn a_move_that_leaves_the_king_in_check_is_refused() {
        // white's queen is pinned to the king by the rook on e8
        let mut board = Board::from_fen("4r1k1/8/8/8/8/8/4Q3/4K3 w - - 0 1").unwrap();
        let before = board.clone();
        let pinned = play_named(&board, "e2a6");
        assert!(!board.try_make(&pinned));
        assert_eq!(board, before);
    }

    #[test]
    fn undo_gives_back_the_position_and_refuses_an_empty_history() {
        let mut board = Board::new();
        assert!(!board.try_undo());
        let start = board.clone();
        assert!(board.try_make(&play_named(&board, "g1f3")));
        assert!(board.try_undo());
        assert_eq!(board, start);
        assert!(!board.try_undo());
    }
}

#[cfg(test)]
mod gives_check {
    use super::{Board, Play, fens, play_named};

    /// What each claim is held to: make the move, read the check the board
    /// maintains for the side now to move, and take the move back. `None`
    /// for a move `make_move` refuses, which the walks skip and the named
    /// cases never offer.
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
    /// is made and held to the made board's answer. The counts say what the
    /// walk really asked, the way the see walk counts its captures.
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

    /// The oracle over played positions: every legal move two plies deep
    /// from the core positions, plus the two perft positions thick with
    /// castling, promotions and en passant.
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

    /// The same oracle over positions nobody played to: fens from the
    /// parser's own generator, pieces scattered at random, one ply deep.
    /// The runner is the deterministic one, so the corpus is the same
    /// corpus every run.
    ///
    /// The castle and en passant fields are dropped before parsing. Against
    /// a random placement either is the known limitation the roadmap
    /// records, a right or a square the position cannot have granted, and
    /// playing the move it licenses corrupts the board it is claimed of.
    /// Castling and en passant are covered by the played walk above, whose
    /// rights are real, and by the named cases below.
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
    /// rule broke rather than which random position found it. Each is also
    /// held to the made board, so a wrong expectation here cannot stand.
    #[test]
    fn a_direct_check_is_seen_from_the_destination() {
        // a quiet rook move to the king's file
        assert!(claims("3k4/8/8/8/8/8/8/R4K2 w - - 0 1", "a1d1"));
        assert!(!claims("3k4/8/8/8/8/8/8/R4K2 w - - 0 1", "a1b1"));
        // a capture on the checking line: the rook lands on the file by
        // taking the pawn that blocked it
        assert!(claims("3k4/8/8/3p4/8/8/8/3R1K2 w - - 0 1", "d1d5"));
        // a pawn's two step ends beside the king's diagonal
        assert!(claims("8/8/8/2k5/8/8/1P6/4K3 w - - 0 1", "b2b4"));
        assert!(!claims("8/8/8/2k5/8/8/1P6/4K3 w - - 0 1", "b2b3"));
    }

    #[test]
    fn a_discovered_check_is_seen_through_the_vacated_square() {
        // the knight leaves the rook's file: anywhere it goes discovers the
        // check, and from f6 it checks on its own besides, the double check
        assert!(claims("4k3/8/8/8/4N3/8/8/4RK2 w - - 0 1", "e4f6"));
        assert!(claims("4k3/8/8/8/4N3/8/8/4RK2 w - - 0 1", "e4c3"));
        // the same knight with no rook behind it checks from f6 alone
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
        // the rook lands on f1 with the black king on the f file
        assert!(claims("5k2/8/8/8/8/8/8/4K2R w K - 0 1", "e1g1"));
        assert!(!claims("k7/8/8/8/8/8/8/4K2R w K - 0 1", "e1g1"));
        // and on d1 from the other side
        assert!(claims("3k4/8/8/8/8/8/8/R3K3 w Q - 0 1", "e1c1"));
        assert!(!claims("2k5/8/8/8/8/8/8/R3K3 w Q - 0 1", "e1c1"));
    }

    #[test]
    fn en_passant_vacates_both_squares_at_once() {
        // the rook's line to the king runs through the capturing pawn's
        // square and the taken pawn's: the capture empties both, and no
        // single vacation opens it
        assert!(claims("8/8/8/1k2pP1R/8/8/8/4K3 w - e6 0 1", "f5e6"));
        // the plain push empties only the mover's square and the taken
        // pawn still blocks
        assert!(!claims("8/8/8/1k2pP1R/8/8/8/4K3 w - e6 0 1", "f5f6"));
        // and the taken pawn's square alone: the bishop's diagonal runs
        // through the pawn being taken and not through the taker
        assert!(claims("1k6/8/8/3Pp3/8/8/7B/4K3 w - e6 0 1", "d5e6"));
        // en passant checking directly, the pawn landing beside the king
        assert!(claims("8/2k5/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6"));
    }
}

#[cfg(test)]
mod mobility {
    use super::{ATTACK_MASKS, Board, Color, pawn_attacks};
    use crate::eval::ALL_KINDS;
    use pretty_assertions::assert_eq;

    /// The counts by hand, square by square, because nothing else pins them.
    /// The tuner's identity folds a row against the live weights, which are
    /// zero, so it is blind to a wrong count here and stays blind to one after
    /// the fit: `eval` and the tuner's walk read the same helper, so the two
    /// sides of the identity move together whatever the helper answers. These
    /// cases are the only check this term has.
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
            // the enemy pawn stops the file at d6 rather than being seen
            // through, so the file gives d5 and d6 beside the rank. This is
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
                board.mobility_counts::<{ ALL_KINDS }>(Color::White),
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
            white.mobility_counts::<{ ALL_KINDS }>(Color::White),
            black.mobility_counts::<{ ALL_KINDS }>(Color::Black)
        );
        assert_eq!(
            white.mobility_counts::<{ ALL_KINDS }>(Color::Black),
            [0, 0, 0, 0]
        );
    }

    /// The span is shifted rather than gathered a pawn at a time, and the
    /// shifts have to answer what the masks generation reads already say. A
    /// pawn attacks upward for white and downward for black, so the span of a
    /// white pawn on a square is the mask of the black pawns that could attack
    /// it, which is where the two files come off.
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
mod shelter {
    use super::{Board, Color, RANKS_AHEAD, SHELTER_MASKS, files_of, king_files};
    use pretty_assertions::assert_eq;

    /// The counts by hand, square by square, because nothing else pins them.
    /// The tuner's identity folds a row against the live weights, which are
    /// zero, so it is blind to a wrong count here and stays blind to one after
    /// the fit: `eval` and the tuner's walk read the same helper, so the two
    /// sides of the identity move together whatever the helper answers. These
    /// cases are the only check this term has.
    ///
    /// Each case names what the count is made of, in the order the helper
    /// returns them: our pawns one rank ahead and two, the king's files that
    /// hold no pawn at all, the ones that hold an enemy pawn and none of ours,
    /// and then the enemy pawns one, two and three ranks ahead. The other king
    /// stands out of the way.
    #[test]
    fn a_king_shelters_behind_what_a_hand_count_says_it_does() {
        for (fen, counts, why) in [
            // three pawns where a castled king wants them
            (
                "k7/8/8/8/8/8/5PPP/6K1 w - - 0 1",
                [3, 0, 0, 0, 0, 0, 0],
                "a king on g1 behind f2, g2 and h2",
            ),
            // the g pawn one square further on is the far rank rather than
            // the near one
            (
                "k7/8/8/8/8/6P1/5P1P/6K1 w - - 0 1",
                [2, 1, 0, 0, 0, 0, 0],
                "a king on g1 with the g pawn on g3",
            ),
            // the g file holds no pawn of either colour
            (
                "k7/8/8/8/8/8/5P1P/6K1 w - - 0 1",
                [2, 0, 1, 0, 0, 0, 0],
                "a king on g1 with no g pawn",
            ),
            // the same file with a black pawn on it is half open rather than
            // open. The pawn is on g7, which is past the three ranks the
            // storm is followed over, so it is a file and not a storm
            (
                "k7/6p1/8/8/8/8/5P1P/6K1 w - - 0 1",
                [2, 0, 0, 1, 0, 0, 0],
                "a king on g1 with a black pawn on g7",
            ),
            // h2 near, g3 far, and the f file holding a black pawn on f5,
            // which is a rank further out than the storm reaches
            (
                "k7/8/8/5p2/8/6P1/7P/6K1 w - - 0 1",
                [1, 1, 0, 1, 0, 0, 0],
                "a king on g1 with the f file gone",
            ),
            // an enemy pawn standing on a rank in front of the king is not
            // cover. It is the storm, counted by the rank it has reached
            (
                "k7/8/8/8/8/8/5PpP/6K1 w - - 0 1",
                [2, 0, 0, 1, 1, 0, 0],
                "a king on g1 with a black pawn on g2",
            ),
            (
                "k7/8/8/8/8/6p1/5P1P/6K1 w - - 0 1",
                [2, 0, 0, 1, 0, 1, 0],
                "a king on g1 with a black pawn on g3",
            ),
            (
                "k7/8/8/8/6p1/8/5P1P/6K1 w - - 0 1",
                [2, 0, 0, 1, 0, 0, 1],
                "a king on g1 with a black pawn on g4",
            ),
            // two ranks of storm at once, on two files
            (
                "k7/8/8/8/7p/6p1/5P1P/6K1 w - - 0 1",
                [2, 0, 0, 1, 0, 1, 1],
                "a king on g1 against pawns on g3 and h4",
            ),
            // a king in the corner is read against three files, so the f file
            // it does not stand beside is still counted. Two files would
            // leave this at nothing
            (
                "k7/8/8/8/8/8/6PP/7K w - - 0 1",
                [2, 0, 1, 0, 0, 0, 0],
                "a king on h1 behind g2 and h2",
            ),
            (
                "k7/8/8/8/8/8/PPP5/K7 w - - 0 1",
                [3, 0, 0, 0, 0, 0, 0],
                "a king on a1 behind a2, b2 and c2",
            ),
            // a king off its own ranks has no rank in front of it inside the
            // masks, so the pawns it left behind are not shelter
            (
                "k7/8/8/4K3/8/8/3PPP2/8 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 0],
                "a king on e5 with its pawns at home",
            ),
            // a pawn on the file stops it counting as open wherever on the
            // file it stands, so a passed pawn up the board is not a hole
            // behind the king it left
            (
                "k7/6P1/8/8/8/8/8/6K1 w - - 0 1",
                [0, 0, 2, 0, 0, 0, 0],
                "a king on g1 whose only pawn is on g7",
            ),
            // the ranks in front of a king on the eighth are off the board
            // rather than round the other side of it
            (
                "4K3/8/8/8/8/8/8/k7 w - - 0 1",
                [0, 0, 3, 0, 0, 0, 0],
                "a king on e8 with no pawns anywhere",
            ),
        ] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(board.shelter_counts(Color::White), counts, "{}", why);
        }
    }

    /// The same reading for black, whose king is measured down the board
    /// rather than up it.
    #[test]
    fn a_black_king_is_measured_down_the_board() {
        // the rank in front of a king on the first is off the board, and the
        // three files all hold a white pawn and no black one
        let board = Board::from_fen("7K/8/8/8/8/8/3PPP2/4k3 b - - 0 1").unwrap();
        assert_eq!(board.shelter_counts(Color::Black), [0, 0, 0, 3, 0, 0, 0]);
        // and the storm the other way up: the white pawn on g6 stands two
        // ranks in front of a black king on g8, and is not its cover
        let stormed = Board::from_fen("6k1/5ppp/6P1/8/8/8/8/6K1 b - - 0 1").unwrap();
        assert_eq!(stormed.shelter_counts(Color::Black), [3, 0, 0, 0, 0, 1, 0]);
    }

    /// Black's count of a position is white's count of its reflection, so the
    /// two colours are read the same way round.
    #[test]
    fn the_two_colours_count_the_same_squares() {
        let white = Board::from_fen("k7/8/8/5p2/8/6P1/7P/6K1 w - - 0 1").unwrap();
        let black = Board::from_fen("6k1/7p/6p1/8/5P2/8/8/K7 b - - 0 1").unwrap();
        assert_eq!(
            white.shelter_counts(Color::White),
            black.shelter_counts(Color::Black)
        );
        // and the storm half of it, which the pair above leaves at zero
        let stormed = Board::from_fen("k7/8/8/8/7p/6p1/5P1P/6K1 w - - 0 1").unwrap();
        let mirrored = Board::from_fen("6k1/5p1p/6P1/7P/8/8/8/K7 b - - 0 1").unwrap();
        assert_eq!(
            stormed.shelter_counts(Color::White),
            mirrored.shelter_counts(Color::Black)
        );
    }

    /// Every square names three files, the two corners included, and the
    /// three are the king's own file and its neighbours wherever there is
    /// room for them.
    #[test]
    fn every_king_square_names_three_files() {
        for square in 0..64u8 {
            let files = king_files(square);
            assert_eq!(files.count_ones(), 3, "the files of {}", square);
            assert_eq!(
                files & (1 << (square % 8)),
                1 << (square % 8),
                "the king's own file is not among the files of {}",
                square
            );
            assert_eq!(
                files.trailing_zeros() + 2,
                7 - files.leading_zeros(),
                "the files of {} are not three in a row",
                square
            );
        }
    }

    /// Each mask holds the rank its index names, on those same three files,
    /// and nothing where the board has run out.
    #[test]
    fn the_masks_hold_the_ranks_in_front_of_the_king() {
        for square in 0..64u8 {
            let rank = i32::from(square / 8);
            for (side, forward) in [(Color::White, 1), (Color::Black, -1)] {
                let i = side as usize;
                for step in 0..RANKS_AHEAD {
                    let mask = SHELTER_MASKS.ahead[step][i][square as usize];
                    let target = rank + forward * (step as i32 + 1);
                    if !(0..8).contains(&target) {
                        assert_eq!(mask, 0, "{:?} on {} at {} ahead", side, square, step + 1);
                        continue;
                    }
                    assert_eq!(mask.count_ones(), 3, "{:?} on {}", side, square);
                    assert_eq!(
                        files_of(mask),
                        king_files(square),
                        "{:?} on {} covers other files",
                        side,
                        square
                    );
                    let rank_mask = 0xffu64 << (target * 8);
                    assert_eq!(
                        mask & rank_mask,
                        mask,
                        "{:?} on {} is off its rank",
                        side,
                        square
                    );
                }
            }
        }
    }

    /// The fold down to a file a bit answers what a walk of the squares does.
    #[test]
    fn the_file_fold_answers_a_walk_of_the_squares() {
        for square in 0..64u8 {
            assert_eq!(files_of(1u64 << square), 1 << (square % 8), "{}", square);
        }
        let board = Board::from_fen("k7/8/8/5p2/8/6P1/7P/6K1 w - - 0 1").unwrap();
        // white's pawns stand on g3 and h2, and black's on f5
        assert_eq!(files_of(board.pawns() & board.white), 0b1100_0000);
        assert_eq!(files_of(board.pawns() & board.black), 0b0010_0000);
    }
}

#[cfg(test)]
mod evasion_targets {
    use super::fens;
    use super::{Board, MoveList};
    use pretty_assertions::assert_eq;

    /// Positions whose evasions exercise a case the target mask has to get
    /// right on its own, each with a piece able to reach the square the
    /// mask must refuse. A case with nothing to refuse passes whatever the
    /// mask says, which is how an earlier double check here let a mask
    /// missing its double check rule through the whole suite.
    const IN_CHECK: &[(&str, &str)] = &[
        // a bishop checks along a5 to e1, and the only block a pawn can
        // reach is b4, two squares ahead. Masking the step the two pushes
        // share rather than each push would lose b2b4 here
        ("double push blocks", "4k3/8/8/b7/8/8/1P6/4K3 w - - 0 1"),
        // a knight checks, so nothing can be interposed and only the king's
        // moves and captures of the knight answer
        ("knight check", "4k3/8/8/8/8/5n2/8/4K3 w - - 0 1"),
        // two checkers at once, which leaves the target mask empty. The
        // queen can take the rook, which answers one check and not the
        // other, so a mask that read only the first checker would keep a
        // move the filter drops
        ("double check", "4k3/8/8/8/8/5n2/4r3/R2QK2R w KQ - 0 1"),
        // a pawn gives the check and stands beside the king
        ("pawn check", "4k3/8/8/8/8/8/3p4/4K3 w - - 0 1"),
        // a rook checks along the eighth rank and the pawn promotes onto
        // the one square between, so a promoting push has to be kept
        ("promotion blocks", "r3K3/2P5/8/8/8/8/8/7k w - - 0 1"),
        // the same rook taken by a promoting capture
        (
            "promotion takes the checker",
            "r3K3/1P6/8/8/8/8/8/7k w - - 0 1",
        ),
        // the checking pawn is the one taken en passant, which the mask
        // deliberately does not examine: the pawn taken does not stand on
        // the square captured to, so the mask would read it wrongly
        (
            "en passant takes the checker",
            "4k3/8/8/3pP3/4K3/8/8/8 w - d6 0 1",
        ),
        // and an en passant that answers nothing, kept unexamined all the
        // same and refused later by make_move
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

    /// Every position in check that a walk of `depth` plies from `fen`
    /// reaches, handed to `visit`. Illegal moves are dropped by `make_move`
    /// the way the search drops them.
    fn walk(board: &mut Board, depth: u8, visit: &mut impl FnMut(&Board)) {
        if board.in_check() {
            visit(board);
        }
        if depth == 0 {
            return;
        }
        for m in board.generate_moves() {
            // a refused move has already put the board back, which is the
            // contract the search relies on
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

        // and over every position in check a short walk of the suite
        // reaches, which is where the shapes nobody thought to name are
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

        // The corpus is only worth its shapes. A count of positions says
        // nothing about whether the ones the mask can get wrong are in
        // there, and the walk reaches no double check, no en passant in
        // check and no promotion that answers one, which is why the named
        // cases above carry those three and this says so.
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
mod pawn_structure {
    use super::{Board, Color, PAWN_MASKS, ahead_of, files_of, pawn_files, spread};
    use pretty_assertions::assert_eq;

    /// The counts by hand, position by position, because nothing else pins
    /// them. The tuner's identity folds a row against the live weights, which
    /// are zero, so it is blind to a wrong count here, and it stays blind to
    /// one after a fit: `eval` and the tuner's walk read this same helper, so
    /// the two sides of the identity move together whatever it answers. These
    /// cases are the only check this term has.
    ///
    /// Each case names the eight counts in the order the helper returns them:
    /// the passed pawns on the relative second through the relative seventh,
    /// then the isolated pawns, then the doubled ones. The other king stands
    /// out of the way.
    #[test]
    fn pawns_count_as_a_hand_count_says_they_do() {
        for (fen, counts, why) in [
            // a pawn with nothing in front of it anywhere is passed, and a
            // pawn with no pawn beside it is isolated. The lone pawn is both
            (
                "4k3/8/8/8/4P3/8/8/4K3 w - - 0 1",
                [0, 0, 1, 0, 0, 0, 1, 0],
                "a white pawn on e4 and no black pawn",
            ),
            // an enemy pawn on an adjacent file ahead of it stops it
            (
                "4k3/8/8/3p4/4P3/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 1, 0],
                "the same against a black pawn on d5",
            ),
            // level is not ahead. A black pawn beside the white one has
            // already been passed
            (
                "4k3/8/8/8/3pP3/8/8/4K3 w - - 0 1",
                [0, 0, 1, 0, 0, 0, 1, 0],
                "the same against a black pawn on d4",
            ),
            // the whole file ahead is read and not the next rank or two
            (
                "4k3/5p2/8/8/4P3/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 1, 0],
                "the same against a black pawn on f7",
            ),
            // a pawn of ours in front of it stops it too, which is what
            // leaves the rear of a doubled pair out of the passed count
            (
                "4k3/8/8/4P3/4P3/8/8/4K3 w - - 0 1",
                [0, 0, 0, 1, 0, 0, 2, 1],
                "white pawns on e4 and e5",
            ),
            // a tripled file is two doubled pawns and not one or three
            (
                "4k3/8/8/8/4P3/4P3/4P3/4K3 w - - 0 1",
                [0, 0, 1, 0, 0, 0, 3, 2],
                "white pawns on e2, e3 and e4",
            ),
            // the relative second is a real bucket. A pawn still at home is
            // passed once the enemy pawns on its three files are gone
            (
                "4k3/8/8/8/8/8/4P3/4K3 w - - 0 1",
                [1, 0, 0, 0, 0, 0, 1, 0],
                "a white pawn on e2 with no black pawn",
            ),
            (
                "4k3/4P3/8/8/8/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 1, 1, 0],
                "a white pawn on e7",
            ),
            // the stop square is not read, so a blockaded passer is a passer.
            // On purpose: the stop square is the first thing this term leaves
            // out, and a later arm has to be able to find the place it goes
            (
                "4k3/4n3/4P3/8/8/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 1, 0, 1, 0],
                "a white pawn on e6 behind a black knight on e7",
            ),
            // a pawn two files away is no company
            (
                "4k3/8/8/8/8/8/P1P5/4K3 w - - 0 1",
                [2, 0, 0, 0, 0, 0, 2, 0],
                "white pawns on a2 and c2",
            ),
            (
                "4k3/8/8/8/8/8/PP6/4K3 w - - 0 1",
                [2, 0, 0, 0, 0, 0, 0, 0],
                "white pawns on a2 and b2",
            ),
            // the two edge files at once. The file mask is a shift and not a
            // rotate, so the a file has no neighbour off the left of the byte
            // and the h file none off the right; a rotate would make each of
            // these the other's neighbour and leave both counted as company
            (
                "4k3/8/8/8/8/8/P6P/4K3 w - - 0 1",
                [2, 0, 0, 0, 0, 0, 2, 0],
                "white pawns on a2 and h2",
            ),
            // the edge cases the file mask decides. A pawn on the a file is
            // read against the a and b files and not against the c file, so
            // stepping the mask in the way `king_files` steps it would let
            // the pawn on c5 stop this one
            (
                "4k3/8/8/1p6/P7/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 1, 0],
                "a white pawn on a4 against a black pawn on b5",
            ),
            (
                "4k3/8/8/2p5/P7/8/8/4K3 w - - 0 1",
                [0, 0, 1, 0, 0, 0, 1, 0],
                "a white pawn on a4 against a black pawn on c5",
            ),
            (
                "4k3/8/8/6p1/7P/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 1, 0],
                "a white pawn on h4 against a black pawn on g5",
            ),
            // from_fen accepts a pawn on either back rank and the search has
            // to survive one. It has come no ranks or all eight, so it names
            // none of the six passed buckets, and it still counts toward the
            // two that read its file
            (
                "4k3/8/8/8/8/8/8/3K1P2 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 1, 0],
                "a white pawn on f1",
            ),
            (
                "4k1P1/8/8/8/8/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 1, 0],
                "a white pawn on g8",
            ),
        ] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(board.pawn_structure_counts(Color::White), counts, "{}", why);
        }
    }

    /// The same reading for black, whose pawns are measured down the board
    /// rather than up it. Each case is the reflection of one above, so a
    /// relative rank read the wrong way up shows as a count in the wrong
    /// bucket rather than as no count at all.
    #[test]
    fn a_black_pawn_is_measured_down_the_board() {
        for (fen, counts, why) in [
            (
                "4k3/8/8/4p3/8/8/8/4K3 w - - 0 1",
                [0, 0, 1, 0, 0, 0, 1, 0],
                "a black pawn on e5 and no white pawn",
            ),
            (
                "4k3/8/8/4p3/3P4/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 1, 0],
                "the same against a white pawn on d4",
            ),
            (
                "4k3/8/8/4p3/4p3/8/8/4K3 w - - 0 1",
                [0, 0, 0, 1, 0, 0, 2, 1],
                "black pawns on e4 and e5",
            ),
            (
                "4k3/8/8/8/8/8/4p3/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 1, 1, 0],
                "a black pawn on e2",
            ),
            (
                "4k3/4p3/8/8/8/8/8/4K3 w - - 0 1",
                [1, 0, 0, 0, 0, 0, 1, 0],
                "a black pawn on e7",
            ),
        ] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(board.pawn_structure_counts(Color::Black), counts, "{}", why);
        }
    }

    /// Black's count of a position is white's count of its reflection, so the
    /// two colours are read the same way round.
    #[test]
    fn the_two_colours_count_the_same_way() {
        let white = Board::from_fen("4k3/P4p2/8/3P2p1/3P4/PP2p2p/1P6/4K3 w - - 0 1").unwrap();
        let black = Board::from_fen("4k3/1p6/pp2P2P/3p4/3p2P1/8/p4P2/4K3 w - - 0 1").unwrap();
        assert_eq!(
            white.pawn_structure_counts(Color::White),
            black.pawn_structure_counts(Color::Black)
        );
        assert_eq!(
            white.pawn_structure_counts(Color::Black),
            black.pawn_structure_counts(Color::White)
        );
    }

    /// Nothing but the pawns decides the counts, which is the property the
    /// pawn hash rests on. The same pawns behind different pieces, and with
    /// the two kings somewhere else, count the same.
    #[test]
    fn nothing_but_the_pawns_is_counted() {
        let bare = Board::from_fen("4k3/pp3ppp/8/8/8/8/PPP2PP1/4K3 w - - 0 1").unwrap();
        let full =
            Board::from_fen("r1bq1rk1/pp3ppp/2n5/8/8/2N5/PPP2PP1/R1BQK2R w KQ - 0 1").unwrap();
        for color in [Color::White, Color::Black] {
            assert_eq!(
                bare.pawn_structure_counts(color),
                full.pawn_structure_counts(color),
                "{:?}",
                color
            );
        }
    }

    /// A pawn's own file and the files beside it, and no more than that. The
    /// edges are the case: two files there and not three, and not three with
    /// the middle one stepped in the way `king_files` steps it.
    #[test]
    fn a_pawn_names_its_own_file_and_the_ones_beside_it() {
        for square in 0..64u8 {
            let file = u32::from(square % 8);
            let files = pawn_files(square);
            let expected = if file == 0 || file == 7 { 2 } else { 3 };
            assert_eq!(files.count_ones(), expected, "the files of {}", square);
            assert_eq!(
                files & (1 << file),
                1 << file,
                "not its own file: {}",
                square
            );
            assert_eq!(
                files.trailing_zeros() + expected - 1,
                7 - files.leading_zeros(),
                "the files of {} are not in a row",
                square
            );
        }
    }

    /// Each front span holds every rank ahead of the pawn and no rank level
    /// with it or behind it, on the files the pawn names and no others.
    #[test]
    fn the_front_span_is_the_files_beside_the_pawn_on_the_ranks_ahead() {
        for square in 0..64u8 {
            let rank = i32::from(square / 8);
            for (side, forward) in [(Color::White, 1), (Color::Black, -1)] {
                let span = PAWN_MASKS.front_span[side as usize][square as usize];
                let ranks = if forward == 1 { 7 - rank } else { rank };
                assert_eq!(
                    span.count_ones(),
                    pawn_files(square).count_ones() * ranks as u32,
                    "{:?} on {}",
                    side,
                    square
                );
                if span != 0 {
                    assert_eq!(
                        files_of(span),
                        pawn_files(square),
                        "{:?} on {} covers other files",
                        side,
                        square
                    );
                }
                for step in 0..8i32 {
                    let on_rank = span & (0xffu64 << (step * 8));
                    let ahead = (step - rank) * forward > 0;
                    assert_eq!(
                        on_rank != 0,
                        ahead,
                        "{:?} on {} holds rank {}",
                        side,
                        square,
                        step
                    );
                }
            }
        }
    }

    /// The file ahead is the front span with the neighbouring files taken
    /// off, which is the relationship the two tables are built to have. Kept
    /// as its own table rather than masked out at the leaf, so this is what
    /// says the two agree.
    #[test]
    fn the_file_ahead_is_the_front_span_on_the_pawns_own_file() {
        for square in 0..64u8 {
            let own = spread(1u8 << (square % 8));
            for side in [Color::White, Color::Black] {
                let i = side as usize;
                assert_eq!(
                    PAWN_MASKS.file_ahead[i][square as usize],
                    PAWN_MASKS.front_span[i][square as usize] & own,
                    "{:?} on {}",
                    side,
                    square
                );
            }
        }
    }

    /// The forward fill holds every square in front of a pawn and never the
    /// pawn itself, which is what leaves a lone pawn undoubled.
    #[test]
    fn the_fill_starts_one_rank_in_front_of_the_pawn() {
        for square in 0..64u8 {
            let pawn = 1u64 << square;
            for side in [Color::White, Color::Black] {
                let filled = ahead_of(pawn, side);
                assert_eq!(
                    filled & pawn,
                    0,
                    "{:?} on {} is in its own fill",
                    side,
                    square
                );
                assert_eq!(
                    filled, PAWN_MASKS.file_ahead[side as usize][square as usize],
                    "{:?} on {}",
                    side, square
                );
            }
        }
    }
}
