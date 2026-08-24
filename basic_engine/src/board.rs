use super::bitboard::BitBoard;
use super::misc::{
    CastlePermissions, Color, Coordinate, File, Piece, PromotePiece, Score, coordinate_to_index,
    coordinate_to_large_index, index_to_coordinate,
};
use super::play::Play;
use crate::magic::Magic;
use crate::psqt::PieceSquareTables;
use crate::zobrist::Zobrist;
use smallvec::SmallVec;
use std::fmt;

pub type MoveList = SmallVec<[Play; 64]>;

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

static PIECE_SQUARE_TABLES: PieceSquareTables = PieceSquareTables::TABLES;
static ZOBRIST: Zobrist = Zobrist::TABLE;

static ATTACK_MASKS: AttackMasks = AttackMasks::new();
// a const rather than a static, so that what is built from it can be built at
// compile time too: a const initialiser may not read a static
pub const BASE_CONVERSIONS: BaseConversions = BaseConversions::new();
// the lint is a guard against a const that loops forever; this one is only
// long, a hundred thousand ray walks filling the two attack tables
#[allow(long_running_const_eval)]
pub(crate) static MAGIC: Magic = Magic::new();
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

/// The sixty four squares laid out inside a ten by ten grid whose border rows
/// and columns are sentinels, so that a step off the side of the board lands on
/// one instead of wrapping onto the far file.
///
/// One row of border is enough because every walk tests a square before
/// stepping again, so an index can never be more than one step out and always
/// stays inside the array.
pub struct BaseConversions {
    base_64_to_100: [u8; 64],
    base_100_to_64: [u8; 100],
}

impl BaseConversions {
    const OFF_BOARD: u8 = 101;

    /// One step in each direction, in this indexing.
    pub const STRAIGHT_STEPS: [isize; 4] = [10, -10, 1, -1]; // rooks and queens
    pub const DIAGONAL_STEPS: [isize; 4] = [9, -9, 11, -11]; // bishops and queens

    /// Built at compile time, so there is nothing to build on startup and
    /// nothing to check on the way to a step. A `const fn` has no `for`, hence
    /// the two `while` walks over what is still a rank and a file.
    const fn new() -> Self {
        let mut base = BaseConversions {
            base_100_to_64: [Self::OFF_BOARD; 100],
            base_64_to_100: [0u8; 64],
        };
        let mut rank = 1;
        while rank <= 8 {
            let mut f = 0;
            while f < File::VARIANTS.len() {
                let file = File::VARIANTS[f];
                let index = coordinate_to_large_index(rank, file);
                let index_64 = coordinate_to_index(rank, file) as usize;
                base.base_100_to_64[index as usize] = index_64 as u8;
                base.base_64_to_100[index_64] = index;
                f += 1;
            }
            rank += 1;
        }
        base
    }

    /// The square one `step` away, or `None` if the step leaves the board.
    #[inline]
    pub const fn step(&self, from: u8, step: isize) -> Option<u8> {
        let index_100 = self.base_64_to_100[from as usize] as isize + step;
        debug_assert!(
            matches!(index_100, 0..100),
            "one step cannot leave the mailbox"
        );
        let square = self.base_100_to_64[index_100 as usize];
        if square != Self::OFF_BOARD {
            Some(square)
        } else {
            None
        }
    }
}

/// Print the mailbox as a grid, the sentinels included, for when a walk comes
/// out wrong. Uncalled on purpose, see `BitBoard::debug_print`.
impl fmt::Display for BaseConversions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for rank in 0..10 {
            for file in 0..10 {
                let index = file + (rank * 10);
                write!(f, " {:0>3}", self.base_100_to_64[index as usize])?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

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

/// The whole position with its history, which makes a board a little over
/// forty kilobytes and `Copy`. Copying one is nothing next to a search and a
/// great deal next to a node, so the search makes and unmakes moves on the one
/// board; only `pv_line` and the tests take copies.
#[derive(Debug, PartialEq, Copy, Clone, Eq)]
pub struct Board {
    pawns: u64,
    knights: u64,
    bishops: u64,
    rooks: u64,
    queens: u64,
    kings: u64,

    white: u64,
    black: u64,

    pub active_color: Color,
    castle: CastlePermissions,
    en_passant: Option<Coordinate>,
    // the pieces giving check to the side to move, maintained by make_move
    // from the move itself rather than recomputed by attack probes at every
    // node. Empty when the side to move is not in check; holding the checkers
    // rather than that fact is what lets the search refuse moves that cannot
    // answer a check without playing them
    checkers: u64,

    ply: usize,
    pub line_ply: usize,
    move_number: usize,
    fifty_move_rule: usize,

    white_value: u32,
    black_value: u32,
    // piece square table score, kept up to date incrementally. Wider than the
    // table entries it accumulates so that summing a boardful of them, and
    // adding the material difference on top, cannot overflow.
    psqt: i32,

    history: [Option<PlayState>; MAX_GAME_SIZE],
    pub key: u64,
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
        self.generate::<true>()
    }

    /// The moves worth trying here: every pseudo legal one, less those that
    /// cannot answer a check when there is one. Most of what full width
    /// generation returns in check would only be refused by `make_move`, so
    /// dropping it before the list is sorted spares the sort and the make.
    ///
    /// Correct whichever position it is asked of: out of check it is
    /// `generate_moves`. The caller does not have to establish that it is in
    /// check first, which is what the filter used to ask of it.
    #[inline]
    pub fn evasions(&self) -> MoveList {
        let mut moves = self.generate_moves();
        if self.in_check() {
            self.retain_evasions(&mut moves);
        }
        moves
    }

    pub fn generate_moves(&self) -> MoveList {
        self.generate::<false>()
    }

    /// The one generator behind generate_moves and generate_captures. The
    /// const parameter is settled at compile time, so each wrapper
    /// monomorphises into the equivalent of a hand-written copy: the captures
    /// one masks every piece's targets with the opponent's pieces and drops
    /// the quiet-only sections, with nothing tested per move.
    fn generate<const CAPTURES_ONLY: bool>(&self) -> MoveList {
        let mut moves = MoveList::new();
        let (color_mask, capture_mask) = match self.active_color {
            Color::Black => (self.black, self.white),
            Color::White => (self.white, self.black),
        };
        let all_pieces = self.black | self.white;
        let attack_masks = &ATTACK_MASKS;
        let magic = &MAGIC;
        // the captures list keeps only the squares the opponent stands on,
        // the full list keeps every square our own pieces do not
        let target_filter = if CAPTURES_ONLY {
            capture_mask
        } else {
            !color_mask
        };
        // when every target is a capture there is nothing to ask per move
        let capture_at = |to: u8| {
            if CAPTURES_ONLY {
                self.get_piece_index(to)
            } else {
                self.capture_on(to, capture_mask)
            }
        };
        // knights
        let mut knights = self.knights & color_mask;
        while knights != 0 {
            let from = pop_lsb(&mut knights);
            let mut targets = attack_masks.knights[from as usize] & target_filter;
            while targets != 0 {
                let to = pop_lsb(&mut targets);
                moves.push(Play::new(from, to, capture_at(to), None, false, false));
            }
        }
        // queens and rooks
        let mut queens_and_rooks = (self.queens | self.rooks) & color_mask;
        while queens_and_rooks != 0 {
            let from = pop_lsb(&mut queens_and_rooks);
            let mut targets = magic.get_straight_move(from, all_pieces) & target_filter;
            while targets != 0 {
                let to = pop_lsb(&mut targets);
                moves.push(Play::new(from, to, capture_at(to), None, false, false));
            }
        }
        // queens and bishops
        let mut queens_and_bishops = (self.queens | self.bishops) & color_mask;
        while queens_and_bishops != 0 {
            let from = pop_lsb(&mut queens_and_bishops);
            let mut targets = magic.get_diagonal_move(from, all_pieces) & target_filter;
            while targets != 0 {
                let to = pop_lsb(&mut targets);
                moves.push(Play::new(from, to, capture_at(to), None, false, false));
            }
        }
        // kings
        let mut kings = self.kings & color_mask;
        while kings != 0 {
            let from = pop_lsb(&mut kings);
            let mut targets = attack_masks.kings[from as usize] & target_filter;
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
        //pawns
        let mut pawns = self.pawns & color_mask;
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
            let mut targets = pmoves;
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
                // can't make a forward move if the square is occupied
                if (0..64).contains(&to) && !all_pieces.is_bit_set(to as u8) {
                    let to = to as u8;
                    if can_promote {
                        for p in PromotePiece::VARIANTS {
                            moves.push(Play::new(from, to, None, Some(p), false, false));
                        }
                    } else {
                        moves.push(Play::new(from, to, None, None, false, false));
                        if match self.active_color {
                            Color::White => rank == 2,
                            Color::Black => rank == 7,
                        } {
                            let to = match self.active_color {
                                Color::White => to as isize + 8,
                                Color::Black => to as isize - 8,
                            };
                            // can't make a double forward move if the to square is occupied
                            if !all_pieces.is_bit_set(to as u8) {
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
        moves
    }

    #[inline]
    pub fn eval(&self) -> Score {
        let eval = (self.white_value as i32 - self.black_value as i32 + self.psqt) as Score;
        match self.active_color {
            Color::White => eval,
            Color::Black => -eval,
        }
    }

    /// Check everything maintained a piece at a time against the position it
    /// describes. Perft looks at none of it, so without this a mistake leaves
    /// every count correct and shows up only as the engine evaluating or
    /// transposing wrongly. Debug only: it walks the whole board.
    ///
    /// Each `recompute_*` below is a second implementation on purpose, and only
    /// worth having while it stays one. Factoring shared code out of a
    /// recompute and the piece-at-a-time path it is checked against would leave
    /// both sides wrong together and this passing, which is worse than not
    /// checking at all: do not tidy them into each other.
    fn debug_assert_state_in_step(&self) {
        debug_assert_eq!(self.psqt, self.recompute_psqt(), "psqt out of step");
        debug_assert_eq!(
            (self.white_value, self.black_value),
            self.recompute_material(),
            "material out of step"
        );
        debug_assert_eq!(self.key, self.recompute_key(), "key out of step");
        // the recompute reads the en passant field as it stands, so the check
        // above cannot tell a field set against the rule: assert the rule
        // itself, that a recorded square is one the side to move can take on
        if let Some(en_passant) = self.en_passant {
            debug_assert!(
                self.pawn_can_capture_on(en_passant.as_index(), self.active_color),
                "en passant square no pawn can take"
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

    /// The piece square score of the position as it stands, computed from the
    /// board rather than accumulated as pieces move. `psqt` is meant to equal
    /// this at all times, which `debug_assert_state_in_step` checks on every
    /// move made.
    fn recompute_psqt(&self) -> i32 {
        let mut total: i32 = 0;
        // walking the occupied squares rather than all sixty four, an empty
        // board is then free rather than sixty four misses
        let mut occupied = self.white | self.black;
        while occupied != 0 {
            let index = pop_lsb(&mut occupied);
            if let Some((piece, color)) = self.get_piece_and_color_index(index) {
                total += i32::from(match color {
                    Color::White => {
                        PIECE_SQUARE_TABLES.get_value(index as usize, piece, Color::White)
                    }
                    Color::Black => {
                        -PIECE_SQUARE_TABLES.get_value(index as usize, piece, Color::Black)
                    }
                });
            }
        }
        total
    }

    /// The material of each side, computed the same way and for the same
    /// reason. Deliberately not `material_value`, which the accumulators are
    /// seeded from: see the note there.
    fn recompute_material(&self) -> (u32, u32) {
        let mut white = 0;
        let mut black = 0;
        let mut occupied = self.white | self.black;
        while occupied != 0 {
            let index = pop_lsb(&mut occupied);
            if let Some((piece, color)) = self.get_piece_and_color_index(index) {
                match color {
                    Color::White => white += piece.material_value(),
                    Color::Black => black += piece.material_value(),
                }
            }
        }
        (white, black)
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
        if (pawn_masks[index as usize] & self.pawns & color_mask) > 0 {
            return true;
        }

        // knights
        if (attack_masks.knights[index as usize] & self.knights & color_mask) > 0 {
            return true;
        }

        // bishops & queens
        let bishop_or_queen = (self.bishops | self.queens) & color_mask;
        if (attack_masks.diagonal[index as usize] & bishop_or_queen) > 0 {
            let move_mask = magic.get_diagonal_move(index, all);
            if (move_mask & bishop_or_queen) > 0 {
                return true;
            }
        }

        // rooks & queens
        let rook_or_queen = (self.rooks | self.queens) & color_mask;
        if (attack_masks.straight[index as usize] & rook_or_queen) > 0 {
            let move_mask = magic.get_straight_move(index, all);
            if (move_mask & rook_or_queen) > 0 {
                return true;
            }
        }

        // kings
        if (attack_masks.kings[index as usize] & self.kings & color_mask) > 0 {
            return true;
        };

        false
    }

    /// How many times this position has already appeared, not counting the
    /// position itself.
    fn repetition_count(&self) -> usize {
        // only the fifty move window can hold a repetition, since a pawn move or
        // a capture in between puts the position out of reach for good. A fen
        // can claim a fifty move count longer than the history or the game
        let window = self.fifty_move_rule.min(self.ply).min(MAX_GAME_SIZE - 1);
        (self.ply - window..self.ply)
            .filter_map(|ply| self.history[history_index(ply)])
            .filter(|state| state.position_key == self.key)
            .count()
    }

    /// Whether the fifty move counter has run out.
    ///
    /// Not the same as drawn: a mate delivered on the hundredth half move
    /// ends the game on it, before the side mated has a move to claim the
    /// draw with, so a caller that can tell a mate has to ask that too.
    pub fn fifty_move_expired(&self) -> bool {
        self.fifty_move_rule >= 100
    }

    /// True on the third occurrence, which is when a game is actually drawn.
    ///
    /// Nothing in the search calls it: the search takes a draw on the first
    /// repetition instead, for the reason `has_repeated` gives. This is the
    /// rule that one is measured against, and what the tests contrast it
    /// with, so it is kept rather than inlined into them.
    #[allow(dead_code)]
    pub(crate) fn is_repetition(&self) -> bool {
        self.repetition_count() >= 2
    }

    /// True once this position has come up before. Inside a search that is
    /// already enough to call it a draw: a position reached twice can be
    /// reached a third time by whichever side wants it, so neither can be made
    /// to avoid it, and waiting for the third costs four plies of depth to see
    /// something that is available now.
    pub fn has_repeated(&self) -> bool {
        self.repetition_count() >= 1
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

    pub fn make_move(&mut self, play: &Play) -> bool {
        self.make_move_impl::<true>(play)
    }

    /// The one caller that never reads `checkers` is perft, which plays its
    /// moves through the impl with MAINTAIN_CHECKERS off: the legality probe
    /// runs unconditionally, since the stale checkers cannot be consulted,
    /// and `checkers_given` is skipped. History still saves and restores the
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
        // update castling permissions
        let old_castle = self.castle;
        match play.from {
            A1 => self.castle.white_queen_side = false,
            E1 => {
                self.castle.white_queen_side = false;
                self.castle.white_king_side = false;
            }
            H1 => self.castle.white_king_side = false,
            A8 => self.castle.black_queen_side = false,
            E8 => {
                self.castle.black_queen_side = false;
                self.castle.black_king_side = false;
            }
            H8 => self.castle.black_king_side = false,
            _ => (),
        }
        match play.to {
            // This covers the case where a rook which hasn't moved is captured
            // since it would end the game we don't need to check the same for king
            A1 => self.castle.white_queen_side = false,
            H1 => self.castle.white_king_side = false,
            A8 => self.castle.black_queen_side = false,
            H8 => self.castle.black_king_side = false,
            _ => (),
        }
        // XORing both the old and new castle keys removes the old permissions
        // from the position key and adds the new ones (a no-op when unchanged)
        self.key ^= ZOBRIST.castle_key(old_castle) ^ ZOBRIST.castle_key(self.castle);
        if let Some(en_passant) = self.en_passant {
            // the en passant rights of the previous position have expired
            self.key ^= ZOBRIST.en_passant_key(en_passant.as_index());
        }
        self.en_passant = None;
        self.fifty_move_rule += 1;

        if self.pawns.is_bit_set(play.from) {
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

        // move piece
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
            // move rook if castling
            match play.to {
                C1 => self.move_piece(A1, D1, Piece::Rook, None, self.active_color),
                C8 => self.move_piece(A8, D8, Piece::Rook, None, self.active_color),
                G1 => self.move_piece(H1, F1, Piece::Rook, None, self.active_color),
                G8 => self.move_piece(H8, F8, Piece::Rook, None, self.active_color),
                _ => unreachable!(),
            }
        }

        // update the ply
        self.ply += 1;
        self.line_ply += 1;
        if self.active_color == Color::Black {
            // update the full move counter
            self.move_number += 1;
        }

        // return false if king in check
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

    pub fn undo_move(&mut self) {
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

        // move piece
        let from_piece = self
            .get_piece_index(play.to)
            .expect("The to square must always be occupied when undoing");
        if let Some(promote) = play.promote {
            self.clear_piece_index(play.to, (&promote).into(), opposing_color);
            self.set_piece_index(play.from, Piece::Pawn, opposing_color);
        } else {
            self.clear_piece_index(play.to, from_piece, opposing_color);
            self.set_piece_index(play.from, from_piece, opposing_color);
        }

        if let Some(capture) = play.capture {
            if !play.en_passant {
                self.set_piece_index(play.to, capture, self.active_color);
            }
        }
        if play.castle {
            // move rook if castling
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

    #[inline]
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
        self.clear_piece_index(from, piece, color);
        if let Some(promote) = promote_piece {
            self.set_piece_index(to, (&promote).into(), color);
        } else {
            self.set_piece_index(to, piece, color);
        }
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
        from & self.pawns & pawns != 0
    }

    /// Where this side's king stands. Every board has exactly one king a side,
    /// which is what `from_fen` checks for: without a king this returns 64 and
    /// the attack masks are indexed off the end.
    fn king_index(&self, color: Color) -> u8 {
        let mask = match color {
            Color::White => self.white,
            Color::Black => self.black,
        };
        (self.kings & mask).trailing_zeros() as u8
    }

    /// Whether the side to move stands in check, read from the checkers
    /// `make_move` maintains rather than by probing the king's square for an
    /// attack. The two would answer the same; this one is for the search,
    /// which asks at every node.
    pub fn in_check(&self) -> bool {
        self.checkers != 0
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
            let attackers = (self.bishops | self.queens) & attacker_mask;
            if attackers != 0 {
                checkers |= magic.get_diagonal_move(king, all) & attackers;
            }
        }
        let straight = attack_masks.straight[king as usize];
        if (matches!(landed, Piece::Rook | Piece::Queen) && straight.is_bit_set(to))
            || straight.is_bit_set(from)
        {
            let attackers = (self.rooks | self.queens) & attacker_mask;
            if attackers != 0 {
                checkers |= magic.get_straight_move(king, all) & attackers;
            }
        }
        checkers
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
        let mut checkers = pawn_masks[king as usize] & self.pawns & attacker_mask;
        checkers |= attack_masks.knights[king as usize] & self.knights & attacker_mask;
        let bishop_or_queen = (self.bishops | self.queens) & attacker_mask;
        if attack_masks.diagonal[king as usize] & bishop_or_queen != 0 {
            checkers |= magic.get_diagonal_move(king, all) & bishop_or_queen;
        }
        let rook_or_queen = (self.rooks | self.queens) & attacker_mask;
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
    /// pins and squares the king may not step to — refusing here only spares
    /// that work for moves it would certainly refuse. En passant is kept
    /// unexamined: the captured pawn does not stand on the to square, so the
    /// capture and block masks misread it, and it is rare.
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
        let kings = self.kings;
        moves.retain(|m| kings.is_bit_set(m.from) || m.en_passant || targets.is_bit_set(m.to));
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

    /// Put a piece on a square, with the position key, the piece square score
    /// and the material accumulators following it on.
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

    /// Take a piece off a square, undoing all of the above.
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
        self.key ^= ZOBRIST.get_piece_key(index, piece, color);

        let psqt = i32::from(match color {
            Color::White => PIECE_SQUARE_TABLES.get_value(index as usize, piece, Color::White),
            Color::Black => -PIECE_SQUARE_TABLES.get_value(index as usize, piece, Color::Black),
        });
        if SET {
            self.psqt += psqt;
        } else {
            self.psqt -= psqt;
        }

        let board = match piece {
            Piece::Pawn => &mut self.pawns,
            Piece::Knight => &mut self.knights,
            Piece::Bishop => &mut self.bishops,
            Piece::Rook => &mut self.rooks,
            Piece::Queen => &mut self.queens,
            Piece::King => &mut self.kings,
        };
        if SET {
            board.set_bit(index);
        } else {
            board.clear_bit(index);
        }

        let value = piece.material_value();
        let (side, total) = match color {
            Color::Black => (&mut self.black, &mut self.black_value),
            Color::White => (&mut self.white, &mut self.white_value),
        };
        if SET {
            side.set_bit(index);
            *total += value;
        } else {
            side.clear_bit(index);
            *total -= value;
        }
    }

    /// What is being taken on the to square, without asking when nothing can
    /// be. `get_piece_index` only answers `None` after testing all six piece
    /// bitboards, and most of the moves generated are quiet, so the mask of
    /// squares a capture is even possible on is worth a look first.
    #[inline(always)]
    fn capture_on(&self, to: u8, capture_mask: u64) -> Option<Piece> {
        if capture_mask.is_bit_set(to) {
            self.get_piece_index(to)
        } else {
            None
        }
    }

    #[inline]
    pub fn get_piece_index(&self, index: u8) -> Option<Piece> {
        let mask = 1u64 << index;
        if (self.pawns & mask) > 0 {
            Some(Piece::Pawn)
        } else if (self.knights & mask) > 0 {
            Some(Piece::Knight)
        } else if (self.bishops & mask) > 0 {
            Some(Piece::Bishop)
        } else if (self.rooks & mask) > 0 {
            Some(Piece::Rook)
        } else if (self.queens & mask) > 0 {
            Some(Piece::Queen)
        } else if (self.kings & mask) > 0 {
            Some(Piece::King)
        } else {
            None
        }
    }

    /// Repeats the search `get_piece_index` does rather than calling it. The
    /// recomputes reach a piece through here and `make_move` reaches one
    /// through there, which keeps a mistake in either from hiding itself in the
    /// state check.
    #[inline]
    fn get_piece_and_color_index(&self, index: u8) -> Option<(Piece, Color)> {
        let mask = 1u64 << index;
        let piece = if (self.pawns & mask) > 0 {
            Piece::Pawn
        } else if (self.knights & mask) > 0 {
            Piece::Knight
        } else if (self.bishops & mask) > 0 {
            Piece::Bishop
        } else if (self.rooks & mask) > 0 {
            Piece::Rook
        } else if (self.queens & mask) > 0 {
            Piece::Queen
        } else if (self.kings & mask) > 0 {
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
    /// thing as `recompute_material` and shares no code with it on purpose:
    /// `from_fen` seeds the accumulators from this one, and the state check
    /// compares them against that one. Collapse the two and a freshly parsed
    /// board would be checked against the function that filled it in.
    fn material_value(&self) -> (u32, u32) {
        let mut black_value = 0;
        let mut white_value = 0;

        white_value += (self.pawns & self.white).count_ones() * Piece::Pawn.material_value();
        black_value += (self.pawns & self.black).count_ones() * Piece::Pawn.material_value();

        white_value += (self.knights & self.white).count_ones() * Piece::Knight.material_value();
        black_value += (self.knights & self.black).count_ones() * Piece::Knight.material_value();

        white_value += (self.bishops & self.white).count_ones() * Piece::Bishop.material_value();
        black_value += (self.bishops & self.black).count_ones() * Piece::Bishop.material_value();

        white_value += (self.rooks & self.white).count_ones() * Piece::Rook.material_value();
        black_value += (self.rooks & self.black).count_ones() * Piece::Rook.material_value();

        white_value += (self.queens & self.white).count_ones() * Piece::Queen.material_value();
        black_value += (self.queens & self.black).count_ones() * Piece::Queen.material_value();

        white_value += (self.kings & self.white).count_ones() * Piece::King.material_value();
        black_value += (self.kings & self.black).count_ones() * Piece::King.material_value();

        (white_value, black_value)
    }

    pub fn perft(&mut self, depth: u8) -> u64 {
        // Based on pseudocode at https://www.chessprogramming.org/Perft
        let mut nodes = 0;

        if depth == 0 {
            return 1;
        }

        for m in &self.generate_moves() {
            if self.make_move_impl::<false>(m) {
                nodes += self.perft(depth - 1);
                self.undo_move();
            }
        }
        nodes
    }

    /// The position a fen describes, or what is wrong with the fen.
    ///
    /// Validated only as far as what the search cannot survive: see the
    /// checks below and the known limitations in `docs/ROADMAP.md` for what
    /// an illegal position can still get away with.
    pub fn from_fen(fen: &str) -> Result<Self, String> {
        let mut fen_iter = fen.split(' ');
        let position = fen_iter
            .next()
            .ok_or("Error parsing FEN: could not find position block")?;
        let active_color_token = match fen_iter.next() {
            Some(c) => {
                if c.len() == 1 {
                    c.chars().next().ok_or("Expected a single character token")
                } else {
                    Err("Expected a single character token")
                }
            }
            None => Err("Error parsing FEN: expected active color token found none"),
        }?;
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
            pawns: 0,
            knights: 0,
            bishops: 0,
            rooks: 0,
            queens: 0,
            kings: 0,
            white: 0,
            black: 0,

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
            white_value: 0,
            black_value: 0,
            psqt: 0,

            history: EMPTY_HISTORY,
            key: INITIAL_KEY,
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
            if (board.kings & mask).count_ones() != 1 {
                return Err(format!(
                    "Error parsing FEN: expected exactly one {:?} king",
                    color
                ));
            }
        }
        if board.square_attacked(board.king_index(!board.active_color), board.active_color) {
            return Err("Error parsing FEN: the side which is not to move is in check".to_string());
        }

        // fold the non-piece state into the position key so that keys are
        // comparable between boards parsed from FEN and boards reached by
        // playing moves
        if board.active_color == Color::Black {
            board.key ^= ZOBRIST.side;
        }
        board.key ^= ZOBRIST.castle_key(board.castle);
        // the same rule as make_move, or a position parsed and the same one
        // played would not hash alike, which is worse than what is being fixed
        if let Some(en_passant) = board.en_passant {
            if board.pawn_can_capture_on(en_passant.as_index(), board.active_color) {
                board.key ^= ZOBRIST.en_passant_key(en_passant.as_index());
            } else {
                board.en_passant = None;
            }
        }
        (board.white_value, board.black_value) = board.material_value();
        board.checkers = board.recompute_checkers();
        // a parsed position must satisfy the same invariants a played one
        // does, or the two ways of reaching a position drift apart
        board.debug_assert_state_in_step();
        Ok(board)
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
                };
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
            (i64::from(self.white_value) - i64::from(self.black_value)),
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
mod evaluate {
    use super::{Board, fens};
    use pretty_assertions::assert_eq;

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
                        (board.white_value, board.black_value),
                        board.material_value(),
                        "{} in {}",
                        m,
                        fen
                    );
                    let score = board.eval();
                    board.active_color = !board.active_color;
                    assert_eq!(score, -board.eval(), "{} in {}", m, fen);
                    board.active_color = !board.active_color;
                    board.undo_move();
                }
            }
        }
    }

    /// The assertions above hold whichever way up the piece square tables are,
    /// because both colours read them the same way and the symmetry survives.
    /// These say which way is up.
    #[test]
    fn a_pawn_is_worth_more_the_closer_it_is_to_promoting() {
        let advanced = Board::from_fen("4k3/4P3/8/8/8/8/8/4K3 w - - 0 1").unwrap();
        let home = Board::from_fen("4k3/8/8/8/8/8/4P3/4K3 w - - 0 1").unwrap();
        assert!(
            advanced.eval() > home.eval(),
            "a pawn on e7 scored {} and one on e2 scored {}",
            advanced.eval(),
            home.eval()
        );
    }

    #[test]
    fn a_pawn_is_worth_more_the_closer_it_is_to_promoting_for_black_too() {
        let advanced = Board::from_fen("4k3/8/8/8/8/8/4p3/4K3 b - - 0 1").unwrap();
        let home = Board::from_fen("4k3/4p3/8/8/8/8/8/4K3 b - - 0 1").unwrap();
        assert!(
            advanced.eval() > home.eval(),
            "a pawn on e2 scored {} and one on e7 scored {}",
            advanced.eval(),
            home.eval()
        );
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
            assert_eq!(white.eval(), black.eval(), "{} against {}", white, black);
        }
    }
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
                let mut played = board;
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

    /// The history is a ring, so a game long enough to run past the end of it
    /// wraps instead. This position starts at ply 1023, one short of the wrap,
    /// so the cycle below is recorded either side of it.
    #[test]
    fn a_repetition_is_still_seen_when_the_history_wraps() {
        let mut board = Board::from_fen(
            "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 b - - 3 511",
        )
        .unwrap();
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
        let start = Board::from_fen(
            "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 b - - 3 511",
        )
        .unwrap();
        let mut board = start;
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
mod position_key {
    use super::Board;
    use pretty_assertions::{assert_eq, assert_ne};

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "en passant square no pawn can take")]
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

        // moving the king drops castle rights, which must change the key
        play_move(&mut board, "e1e2");
        let fen = Board::from_fen("rnbqkb1r/pppppppp/5n2/8/4P3/8/PPPPKPPP/RNBQ1BNR b KQkq - 2 2")
            .unwrap();
        assert_ne!(board.key, fen.key);
        let fen =
            Board::from_fen("rnbqkb1r/pppppppp/5n2/8/4P3/8/PPPPKPPP/RNBQ1BNR b kq - 2 2").unwrap();
        assert_eq!(board.key, fen.key);
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
}

#[cfg(test)]
mod fen_parsing {
    use super::{Board, fens};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn random_str_doesnt_crash(s in ".*") {
            _ = Board::from_fen(&s);
        }
    }

    #[test]
    fn the_starting_position_parses() {
        assert!(Board::from_fen(fens::START).is_ok());
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
    const CASES: [(&str, u8, u64, &str); 23] = [
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

    const POSITIONS: [&str; 5] = [
        fens::START,
        fens::KIWIPETE,
        fens::PAWN_ENDGAME,
        fens::PROMOTIONS,
        "r2q1rk1/1b1nbppp/p2ppn2/1p6/3NPP2/1BN1B3/PPPQ2PP/2KR3R w - - 0 13",
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
        let board =
            Board::from_fen("r2q1rk1/1b1nbppp/p2ppn2/1p6/3NPP2/1BN1B3/PPPQ2PP/2KR3R w - - 0 13")
                .unwrap();

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

    #[test]
    fn out_of_check_the_evasions_are_every_move() {
        // asked of a position not in check it filters nothing, which is what
        // lets the caller ask without establishing that first
        let board = Board::new();
        assert_eq!(board.evasions().len(), board.generate_moves().len());
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
                let before = board;
                let play = moves[pick.index(moves.len())];
                if board.make_move(&play) {
                    prop_assert_eq!(
                        board.key,
                        board.recompute_key(),
                        "key out of step after {}",
                        play
                    );
                    prop_assert_eq!(
                        board.psqt,
                        board.recompute_psqt(),
                        "psqt out of step after {}",
                        play
                    );
                    prop_assert_eq!(
                        (board.white_value, board.black_value),
                        board.recompute_material(),
                        "material out of step after {}",
                        play
                    );
                    prop_assert_eq!(
                        board.checkers,
                        board.recompute_checkers(),
                        "checkers out of step after {}",
                        play
                    );
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
