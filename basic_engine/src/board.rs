use super::bitboard::BitBoard;
use super::misc::{
    CastlePermissions, Color, Coordinate, File, Piece, PromotePiece, Score, coordinate_to_index,
    coordinate_to_large_index, index_to_coordinate,
};
use super::play::Play;
use crate::Game;
use crate::magic::Magic;
use crate::pvt::PieceValueTables;
use crate::zorbrist::Zorbrist;
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

/// Play State is used to store the history of moves (plays)
///
/// Although the move/play object already contains most of the information we need, in order to
/// undo a move we need some additional state.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
struct PlayState {
    play: Play,

    en_passant: Option<Coordinate>,
    castle: CastlePermissions,
    fifty_move_rule: usize,
    position_key: u64,
}

// Plies of history the board can record. This has to cover the whole game as
// replayed by the gui plus the depth of the current search, 375 was not enough
// for games which reached move 175 or so.
const MAX_GAME_SIZE: usize = 1024;
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

lazy_static! {
    static ref ATTACK_MASKS: AttackMasks = AttackMasks::new();
    pub static ref BASE_CONVERSIONS: BaseConversions = BaseConversions::new();
    static ref CASTLE_PERMISSION_SQUARES: [u8; 6] = [
        coordinate_to_index(1, File::A),
        coordinate_to_index(1, File::E),
        coordinate_to_index(1, File::H),
        coordinate_to_index(8, File::A),
        coordinate_to_index(8, File::E),
        coordinate_to_index(8, File::H),
    ];
    static ref ZORB: Zorbrist = Zorbrist::new();
    static ref PVT: PieceValueTables = PieceValueTables::new();
    static ref MAGIC: Magic = Magic::new();
    static ref B1_C1_D1: u64 = {
        let mut mask = 0u64;
        mask.set_bit(B1);
        mask.set_bit(C1);
        mask.set_bit(D1);
        mask
    };
    static ref F1_G1: u64 = {
        let mut mask = 0u64;
        mask.set_bit(F1);
        mask.set_bit(G1);
        mask
    };
    static ref B8_C8_D8: u64 = {
        let mut mask = 0u64;
        mask.set_bit(B8);
        mask.set_bit(C8);
        mask.set_bit(D8);
        mask
    };
    static ref F8_G8: u64 = {
        let mut mask = 0u64;
        mask.set_bit(F8);
        mask.set_bit(G8);
        mask
    };
}

pub struct BaseConversions {
    pub base_64_to_100: [u8; 64],
    pub base_100_to_64: [u8; 100],
}

impl BaseConversions {
    const OFF_BOARD: u8 = 101;
    fn new() -> Self {
        let mut base = BaseConversions {
            base_100_to_64: [Self::OFF_BOARD; 100],
            base_64_to_100: [0u8; 64],
        };
        for rank in 1..=8 {
            for file in File::VARIANTS {
                let index = coordinate_to_large_index(rank, file);
                let index_64 = coordinate_to_index(rank, file) as usize;
                base.base_100_to_64[index as usize] = index_64 as u8;
                base.base_64_to_100[index_64] = index;
            }
        }
        base
    }

    #[inline(always)]
    pub fn is_offboard(&self, index_100: usize) -> bool {
        self.base_100_to_64[index_100] == Self::OFF_BOARD
    }
}

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

struct AttackMasks {
    black_pawns: [u64; 64],
    white_pawns: [u64; 64],
    knights: [u64; 64],
    straight: [u64; 64], // rooks and queens
    diagonal: [u64; 64], // bishops and queens
    kings: [u64; 64],
}

impl AttackMasks {
    fn new() -> Self {
        let mut am = AttackMasks {
            black_pawns: [0; 64],
            white_pawns: [0; 64],
            knights: [0; 64],
            straight: [0; 64], // rooks and queens
            diagonal: [0; 64], // bishops and queens
            kings: [0; 64],
        };
        for i in 0isize..64 {
            let (rank, file) = index_to_coordinate(i as u8);
            let mut kings: Vec<isize> = vec![-1, 1, -8, 8, 7, 9, -7, -9];
            let mut black_pawns: Vec<isize> = vec![7, 9];
            let mut white_pawns: Vec<isize> = vec![-7, -9];
            let knights = [15, 17, -15, -17, 6, 10, -6, -10];

            let top_rank = i <= H1.into();
            let bottom_rank = i >= A8.into();
            let left_edge = (i % 8) == 0;
            let right_edge = (i % 8) == 7;

            if top_rank {
                kings.retain(|j| ![-7, -8, -9].contains(j));
                white_pawns = vec![];
            } else if bottom_rank {
                kings.retain(|j| ![7, 8, 9].contains(j));
                black_pawns = vec![];
            }

            if left_edge {
                kings.retain(|j| ![-1, -9, 7].contains(j));
                white_pawns.retain(|j| ![-9].contains(j));
                black_pawns.retain(|j| ![7].contains(j));
            } else if right_edge {
                kings.retain(|j| ![1, -7, 9].contains(j));
                black_pawns.retain(|j| ![9].contains(j));
                white_pawns.retain(|j| ![-7].contains(j));
            }

            for j in &kings {
                let index = i + j;
                am.kings[i as usize].set_bit(index as u8);
            }

            for j in &white_pawns {
                let index = i + j;
                am.white_pawns[i as usize].set_bit(index as u8);
            }
            for j in &black_pawns {
                let index = i + j;
                am.black_pawns[i as usize].set_bit(index as u8);
            }

            for j in &knights {
                let index = i + j;
                if (0..64).contains(&index) {
                    let (new_rank, new_file) = index_to_coordinate(index as u8);
                    let rank_diff = rank as isize - new_rank as isize;
                    let file_diff = file as isize - new_file as isize;

                    if (rank_diff).abs() <= 2 && (file_diff).abs() <= 2 {
                        am.knights[i as usize].set_bit(index as u8);
                    };
                }
            }

            for j in 0..8 {
                let horizontal_index = (i / 8 * 8) + j;
                let vertical_index = (i % 8) + (j * 8);
                am.straight[i as usize].set_bit(horizontal_index as u8);
                am.straight[i as usize].set_bit(vertical_index as u8);
            }

            let directions = [9isize, -9, 11, -11];
            for k in directions {
                let mut j = 0;
                loop {
                    let check_100_index =
                        BASE_CONVERSIONS.base_64_to_100[i as usize] as isize + (k * j);
                    if BASE_CONVERSIONS.is_offboard(check_100_index as usize) {
                        break;
                    };
                    let check_index = BASE_CONVERSIONS.base_100_to_64[check_100_index as usize];
                    j += 1;
                    am.diagonal[i as usize].set_bit(check_index);
                }
            }
        }
        am
    }
}

#[derive(Debug, PartialEq, Copy, Clone, Eq, Hash)]
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

    pub ply: usize,
    pub line_ply: usize,
    move_number: usize,
    pub fifty_move_rule: usize,

    pub white_value: u32,
    pub black_value: u32,
    // piece square table score, kept up to date incrementally
    psqt: isize,

    //history: Vec<PlayState>,
    history: [Option<PlayState>; MAX_GAME_SIZE],
    pub key: u64,
}

impl Default for Board {
    fn default() -> Self {
        lazy_static::initialize(&MAGIC); // TODO move this to engine/parse fen?
        Board::from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1").unwrap()
    }
}

impl Board {
    pub fn new() -> Board {
        lazy_static::initialize(&MAGIC); // TODO move this to engine/parse fen?
        Board::from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1").unwrap()
    }

    pub fn generate_captures(&self) -> MoveList {
        let mut moves = MoveList::new();
        let (color_mask, capture_mask) = match self.active_color {
            Color::Black => (self.black, self.white),
            Color::White => (self.white, self.black),
        };
        let all_pieces = self.black | self.white;
        let attack_masks: &AttackMasks = &ATTACK_MASKS;
        let magic: &Magic = &MAGIC;
        // knights
        let mut knights = self.knights & color_mask;
        while knights != 0 {
            let from = pop_lsb(&mut knights);
            // Only include moves which don't have another piece of our color at the to square
            let kmoves = attack_masks.knights[from as usize] & (capture_mask);
            let mut __m = kmoves;
            while __m != 0 {
                let to = pop_lsb(&mut __m);
                let capture = self.get_piece_index(to);
                moves.push(Play::new(from, to, capture, None, false, false));
            }
        }
        // queens and rooks
        let mut queens_and_rooks = (self.queens | self.rooks) & color_mask;
        while queens_and_rooks != 0 {
            let from = pop_lsb(&mut queens_and_rooks);
            let move_mask = magic.get_straight_move(from, all_pieces) & capture_mask;
            let mut __m = move_mask;
            while __m != 0 {
                let to = pop_lsb(&mut __m);
                let capture = self.get_piece_index(to);
                moves.push(Play::new(from, to, capture, None, false, false));
            }
        }
        // queens and bishops
        let mut queens_and_bishops = (self.queens | self.bishops) & color_mask;
        while queens_and_bishops != 0 {
            let from = pop_lsb(&mut queens_and_bishops);
            let move_mask = magic.get_diagonal_move(from, all_pieces) & capture_mask;
            let mut __m = move_mask;
            while __m != 0 {
                let to = pop_lsb(&mut __m);
                let capture = self.get_piece_index(to);
                moves.push(Play::new(from, to, capture, None, false, false));
            }
        }
        // kings
        let mut kings = self.kings & color_mask;
        while kings != 0 {
            let from = pop_lsb(&mut kings);
            // Only include moves which don't have another piece of our color at the to square
            let kmove = attack_masks.kings[from as usize] & capture_mask;
            let mut __m = kmove;
            while __m != 0 {
                let to = pop_lsb(&mut __m);
                let capture = self.get_piece_index(to);
                moves.push(Play::new(from, to, capture, None, false, false));
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
            let mut __m = pmoves;
            while __m != 0 {
                let to = pop_lsb(&mut __m);
                let capture = self.get_piece_index(to);
                if can_promote {
                    for p in PromotePiece::VARIANTS {
                        moves.push(Play::new(from, to, capture, Some(p), false, false));
                    }
                } else {
                    moves.push(Play::new(from, to, capture, None, false, false));
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

    pub fn generate_moves(&self) -> MoveList {
        let mut moves = MoveList::new();
        let (color_mask, capture_mask) = match self.active_color {
            Color::Black => (self.black, self.white),
            Color::White => (self.white, self.black),
        };
        let all_pieces = self.black | self.white;
        let attack_masks: &AttackMasks = &ATTACK_MASKS;
        let magic: &Magic = &MAGIC;
        // knights
        let mut knights = self.knights & color_mask;
        while knights != 0 {
            let from = pop_lsb(&mut knights);
            // Only include moves which don't have another piece of our color at the to square
            let kmoves = attack_masks.knights[from as usize] & (!color_mask);
            let mut __m = kmoves;
            while __m != 0 {
                let to = pop_lsb(&mut __m);
                let capture = self.get_piece_index(to);
                moves.push(Play::new(from, to, capture, None, false, false));
            }
        }
        // queens and rooks
        let mut queens_and_rooks = (self.queens | self.rooks) & color_mask;
        while queens_and_rooks != 0 {
            let from = pop_lsb(&mut queens_and_rooks);
            let move_mask = magic.get_straight_move(from, all_pieces) & !color_mask;
            let mut __m = move_mask;
            while __m != 0 {
                let to = pop_lsb(&mut __m);
                let capture = self.get_piece_index(to);
                moves.push(Play::new(from, to, capture, None, false, false));
            }
        }
        // queens and bishops
        let mut queens_and_bishops = (self.queens | self.bishops) & color_mask;
        while queens_and_bishops != 0 {
            let from = pop_lsb(&mut queens_and_bishops);
            let move_mask = magic.get_diagonal_move(from, all_pieces) & !color_mask;
            let mut __m = move_mask;
            while __m != 0 {
                let to = pop_lsb(&mut __m);
                let capture = self.get_piece_index(to);
                moves.push(Play::new(from, to, capture, None, false, false));
            }
        }
        // kings
        let mut kings = self.kings & color_mask;
        while kings != 0 {
            let from = pop_lsb(&mut kings);
            // Only include moves which don't have another piece of our color at the to square
            let kmove = attack_masks.kings[from as usize] & (!color_mask);
            let mut __m = kmove;
            while __m != 0 {
                let to = pop_lsb(&mut __m);
                let capture = self.get_piece_index(to);
                moves.push(Play::new(from, to, capture, None, false, false));
            }
            // 1. castle permission is available
            // 2. king is not in check
            // 3. movement squares are not occupied
            // 4. None of the squares are in check
            if matches!(self.active_color, Color::White)
                && (self.castle.white_king_side || self.castle.white_queen_side)
            {
                let check = self.square_attacked(E1, Color::Black);
                if !check {
                    if self.castle.white_queen_side
                        && (*B1_C1_D1 & all_pieces) == 0
                        && [C1, D1]
                            .iter()
                            .all(|i| !self.square_attacked(*i, Color::Black))
                    {
                        moves.push(Play::new(from, C1, None, None, false, true));
                    }
                    if self.castle.white_king_side
                        && (*F1_G1 & all_pieces) == 0
                        && [F1, G1]
                            .iter()
                            .all(|i| !self.square_attacked(*i, Color::Black))
                    {
                        moves.push(Play::new(from, G1, None, None, false, true));
                    }
                }
            } else if matches!(self.active_color, Color::Black)
                && (self.castle.black_king_side || self.castle.black_queen_side)
            {
                let check = self.square_attacked(E8, Color::White);
                if !check {
                    if self.castle.black_queen_side
                        && (*B8_C8_D8 & all_pieces) == 0
                        && [C8, D8]
                            .iter()
                            .all(|i| !self.square_attacked(*i, Color::White))
                    {
                        moves.push(Play::new(from, C8, None, None, false, true));
                    }
                    if self.castle.black_king_side
                        && (*F8_G8 & all_pieces) == 0
                        && [F8, G8]
                            .iter()
                            .all(|i| !self.square_attacked(*i, Color::White))
                    {
                        moves.push(Play::new(from, G8, None, None, false, true));
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
            let mut __m = pmoves;
            while __m != 0 {
                let to = pop_lsb(&mut __m);
                let capture = self.get_piece_index(to);
                if can_promote {
                    for p in PromotePiece::VARIANTS {
                        moves.push(Play::new(from, to, capture, Some(p), false, false));
                    }
                } else {
                    moves.push(Play::new(from, to, capture, None, false, false));
                }
            }
            // move forward
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
        // TODO should this return white value & black value as separate numbers instead?
        let eval = self.white_value as i32 - self.black_value as i32;

        let eval = (eval + self.psqt as i32) as Score;

        match self.active_color {
            Color::White => eval,
            Color::Black => -eval,
        }
    }

    pub fn square_attacked(&self, index: u8, color: Color) -> bool {
        let all = self.black | self.white;
        let attack_masks: &AttackMasks = &ATTACK_MASKS;
        let magic: &Magic = &MAGIC;
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
        // a position parsed from fen can have a fifty move count higher than the number of plies
        // we hold history for, and a long enough game runs past the end of the history
        let start = self.ply.saturating_sub(self.fifty_move_rule);
        let end = self.ply.min(MAX_GAME_SIZE - 1);
        if start > end {
            return 0;
        }
        self.history[start..=end]
            .iter()
            .filter_map(|h| h.map(|hh| hh.position_key))
            .filter(|k| *k == self.key)
            .count()
    }

    /// True on the third occurrence, which is when a game is actually drawn.
    pub fn is_repetition(&self) -> bool {
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

    pub fn make_move(&mut self, play: &Play) -> bool {
        self.history[self.ply] = Some(PlayState {
            play: *play,
            en_passant: self.en_passant,
            castle: self.castle,
            fifty_move_rule: self.fifty_move_rule,
            position_key: self.key,
        });

        let opposing_color = !self.active_color;
        let zorb: &Zorbrist = &ZORB;
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
        self.key ^= zorb.castle_key(old_castle) ^ zorb.castle_key(self.castle);
        if let Some(en_passant) = self.en_passant {
            // the en passant rights of the previous position have expired
            self.key ^= zorb.en_passant_key(en_passant.as_index());
        }
        self.en_passant = None;
        self.fifty_move_rule += 1;

        if self.pawns.is_bit_set(play.from) {
            // pawn moves reset the fifty move rule
            self.fifty_move_rule = 0;
            if (play.from as isize - play.to as isize).abs() == 16 {
                // if a pawn moved two squares forward then we must update the en_passant square
                self.en_passant = match self.active_color {
                    Color::White => Some(Coordinate::from_index(play.to - 8)),
                    Color::Black => Some(Coordinate::from_index(play.to + 8)),
                };
                self.key ^= zorb.en_passant_key(play.to);
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
        if matches!(self.active_color, Color::Black) {
            // update the full move counter
            self.move_number += 1;
        }

        // return false if king in check
        let king_index = match self.active_color {
            Color::White => (self.kings & self.white).trailing_zeros() as u8,
            Color::Black => (self.kings & self.black).trailing_zeros() as u8,
        };
        self.active_color = opposing_color;
        self.key ^= zorb.side;
        if self.square_attacked(king_index, opposing_color) {
            self.undo_move().unwrap();
            false
        } else {
            true
        }
    }

    pub fn undo_move(&mut self) -> Result<(), &str> {
        let history = self.history[self.ply - 1].unwrap();
        self.history[self.ply - 1] = None;
        let play = history.play;

        let opposing_color = !self.active_color;
        // update castling permissions
        self.castle = history.castle;
        self.en_passant = history.en_passant;
        self.fifty_move_rule = history.fifty_move_rule;
        self.ply -= 1;
        self.line_ply -= 1;
        if matches!(opposing_color, Color::Black) {
            self.move_number -= 1;
        }

        if self.pawns.is_bit_set(play.to) {
            // pawn moves reset the fifty move rule
            if play.en_passant {
                let en_passant_index = match opposing_color {
                    Color::White => play.to - 8,
                    Color::Black => play.to + 8,
                };
                self.set_piece_index(en_passant_index, Piece::Pawn, self.active_color);
            }
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
        Ok(())
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

    pub fn is_king_attacked(&self) -> bool {
        let (index, opposing_color) = match self.active_color {
            Color::White => (
                (self.kings & self.white).trailing_zeros() as u8,
                Color::Black,
            ),
            Color::Black => (
                (self.kings & self.black).trailing_zeros() as u8,
                Color::White,
            ),
        };
        self.square_attacked(index, opposing_color)
    }

    pub fn attacked_print(&self, color: Color) {
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

    #[inline]
    fn set_piece_index(&mut self, index: u8, piece: Piece, color: Color) {
        debug_assert!(!self.black.is_bit_set(index));
        debug_assert!(!self.white.is_bit_set(index));
        let zorb: &Zorbrist = &ZORB;
        self.key ^= zorb.get_piece_key(index, piece, color);
        self.psqt += match color {
            Color::White => PVT.get_value(index as usize, piece, Color::White),
            Color::Black => -PVT.get_value(index as usize, piece, Color::Black),
        };
        match piece {
            Piece::Pawn => self.pawns.set_bit(index),
            Piece::Knight => self.knights.set_bit(index),
            Piece::Bishop => self.bishops.set_bit(index),
            Piece::Rook => self.rooks.set_bit(index),
            Piece::Queen => self.queens.set_bit(index),
            Piece::King => self.kings.set_bit(index),
        };
        match color {
            Color::Black => {
                self.black.set_bit(index);
                self.black_value += piece.material_value();
            }
            Color::White => {
                self.white.set_bit(index);
                self.white_value += piece.material_value();
            }
        };
    }

    fn set_piece(&mut self, piece: Piece, color: Color, rank: u8, file: File) {
        let index = coordinate_to_index(rank, file);
        self.set_piece_index(index, piece, color);
    }

    #[inline]
    fn clear_piece_index(&mut self, index: u8, piece: Piece, color: Color) {
        debug_assert!((self.black | self.white).is_bit_set(index));
        let zorb: &Zorbrist = &ZORB;
        self.key ^= zorb.get_piece_key(index, piece, color);
        self.psqt -= match color {
            Color::White => PVT.get_value(index as usize, piece, Color::White),
            Color::Black => -PVT.get_value(index as usize, piece, Color::Black),
        };
        match piece {
            Piece::Pawn => self.pawns.clear_bit(index),
            Piece::Knight => self.knights.clear_bit(index),
            Piece::Bishop => self.bishops.clear_bit(index),
            Piece::Rook => self.rooks.clear_bit(index),
            Piece::Queen => self.queens.clear_bit(index),
            Piece::King => self.kings.clear_bit(index),
        };
        match color {
            Color::Black => {
                self.black.clear_bit(index);
                self.black_value -= piece.material_value();
            }
            Color::White => {
                self.white.clear_bit(index);
                self.white_value -= piece.material_value();
            }
        };
    }

    #[inline]
    pub fn get_piece_index(&self, index: u8) -> Option<Piece> {
        // TODO this should also return color
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

    #[inline]
    pub fn get_piece_and_color_index(&self, index: u8) -> Option<(Piece, Color)> {
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

    fn get_piece(&self, rank: u8, file: File) -> (Option<Piece>, Option<Color>) {
        let index = coordinate_to_index(rank, file);
        let mask = 1u64 << index;
        let color = if (self.black & mask) > 0 {
            Some(Color::Black)
        } else if (self.white & mask) > 0 {
            Some(Color::White)
        } else {
            None
        };
        let piece = self.get_piece_index(index);
        (piece, color)
    }

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
        // Based on psedocode at https://www.chessprogramming.org/Perft
        let mut nodes = 0;

        if depth == 0 {
            return 1;
        }

        for m in &self.generate_moves() {
            if self.make_move(m) {
                let branch = self.perft(depth - 1);
                nodes += branch;
                //println!("{}", m);
                self.undo_move().unwrap();
                // TODO remove this debug
                //if depth == 2 {
                //    println!("m {} => {}", m, branch); // perft divide
                //};
            }
        }
        nodes
    }
}

impl Game for Board {
    fn from_fen(fen: &str) -> Result<Self, String> {
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

            ply: (full_move_clock
                .parse::<usize>()
                .map_err(|e| e.to_string())?)
                * 2,
            line_ply: 0,
            move_number: full_move_clock
                .parse::<usize>()
                .map_err(|e| e.to_string())?,
            en_passant: Coordinate::from_string(en_passant)?,
            fifty_move_rule: half_move_clock
                .parse::<usize>()
                .map_err(|e| e.to_string())?,
            white_value: 0,
            black_value: 0,
            psqt: 0,

            history: EMPTY_HISTORY,
            key: 2340980257093, // TODO start with random number?
        };
        if matches!(board.active_color, Color::Black) {
            board.ply += 1;
        }

        // parse out the pieces on the board
        let mut rank = 8;
        let mut file = File::A;
        for c in position.chars() {
            if rank < 1 {
                return Err("Too many ranks found".to_string());
            }
            // TODO change piece to PieceType and implement a Piece with from char and to char
            // methods
            let piece = match c {
                'p' | 'P' => Some(Piece::Pawn),
                'n' | 'N' => Some(Piece::Knight),
                'b' | 'B' => Some(Piece::Bishop),
                'r' | 'R' => Some(Piece::Rook),
                'q' | 'Q' => Some(Piece::Queen),
                'k' | 'K' => Some(Piece::King),
                '/' => None,
                '1'..='8' => None,
                _ => return Err("unexpected character in fen".to_string()),
            };
            if let Some(p) = piece {
                let color = if c.is_uppercase() {
                    Color::White
                } else {
                    Color::Black
                };
                board.set_piece(p, color, rank, file);
            }

            file = match c {
                '1'..='8' => file.add(c.to_digit(10).unwrap()),
                'r' | 'b' | 'n' | 'k' | 'q' | 'p' => file.add(1),
                'R' | 'B' | 'N' | 'K' | 'Q' | 'P' => file.add(1),
                '/' => {
                    rank -= 1;
                    File::A
                }
                _ => return Err("unexpected character in fen".to_string()),
            };
        }
        // fold the non-piece state into the position key so that keys are
        // comparable between boards parsed from FEN and boards reached by
        // playing moves
        if matches!(board.active_color, Color::Black) {
            board.key ^= ZORB.side;
        }
        board.key ^= ZORB.castle_key(board.castle);
        if let Some(en_passant) = board.en_passant {
            board.key ^= ZORB.en_passant_key(en_passant.as_index());
        }
        (board.white_value, board.black_value) = board.material_value();
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
                let (piece, color) = self.get_piece(rank, file);
                let c = match piece {
                    Some(Piece::Pawn) => 'p',
                    Some(Piece::Knight) => 'n',
                    Some(Piece::Bishop) => 'b',
                    Some(Piece::Rook) => 'r',
                    Some(Piece::Queen) => 'q',
                    Some(Piece::King) => 'k',
                    None => '.',
                };
                match color {
                    Some(Color::White) => write!(f, " {}", c.to_uppercase())?,
                    _ => write!(f, " {}", c)?,
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

#[cfg(test)]
mod evaluate {
    use super::Board;

    use super::Game;
    use pretty_assertions::assert_eq;

    macro_rules! test_fen {
        ($func:ident, $f:expr) => {
            #[test]
            fn $func() {
                let mut board = Board::from_fen($f).unwrap();
                for m in &board.generate_moves() {
                    if board.make_move(m) {
                        assert_eq!(
                            (board.white_value, board.black_value),
                            board.material_value()
                        );
                        let score = board.eval();
                        board.active_color = !board.active_color;
                        let opp_score = board.eval();
                        assert_eq!(score, -opp_score);
                        board.active_color = !board.active_color;
                        board.undo_move().unwrap();
                    }
                }
            }
        };
    }

    test_fen!(
        initial_position,
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"
    );
    test_fen!(
        promotion,
        "rnbqkbnr/pp1ppppp/8/2p5/3Pp3/8/PPPP1PpP/RNBQKB1R b KQkq e5 0 2"
    );
    test_fen!(
        castling,
        "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10"
    );
    test_fen!(position_3, "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1");

    /// The assertions above hold whichever way up the piece square tables are,
    /// because both colours read them the same way and the symmetry survives.
    /// These say which way is up.
    #[test]
    fn test_a_pawn_is_worth_more_the_closer_it_is_to_promoting() {
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
    fn test_a_pawn_is_worth_more_the_closer_it_is_to_promoting_for_black_too() {
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
    fn test_a_mirrored_position_scores_the_same() {
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
    use super::Board;
    use super::Game;
    use super::Play;
    use super::{A1, A8, B1, B8};
    use pretty_assertions::{assert_eq, assert_ne};

    macro_rules! test_fen_reversible {
        ($func:ident, $f:expr) => {
            #[test]
            fn $func() {
                let board = Board::from_fen($f).unwrap();
                for m in &board.generate_moves() {
                    let old = board.clone();
                    let mut new = board.clone();
                    if new.make_move(m) {
                        assert_ne!(old, new);
                        new.undo_move().unwrap();
                        assert_eq!(old, new);
                    }
                }
            }
        };
    }

    test_fen_reversible!(
        initial_position_reversible,
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"
    );
    test_fen_reversible!(
        promotion_reversible,
        "rnbqkbnr/pp1ppppp/8/2p5/3Pp3/8/PPPP1PpP/RNBQKB1R b KQkq e5 0 2"
    );
    test_fen_reversible!(
        castling_reversible,
        "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10"
    );
    test_fen_reversible!(
        position_3_reversible,
        "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1"
    );

    macro_rules! test_fen_captures {
        ($func:ident, $f:expr) => {
            #[test]
            fn $func() {
                let board = Board::from_fen($f).unwrap();
                let filtered_captures: super::MoveList = board
                    .generate_moves()
                    .iter()
                    .filter(|c| c.capture.is_some())
                    .map(|c| c.clone())
                    .collect();
                let captures = board.generate_captures();
                assert_eq!(captures, filtered_captures);
            }
        };
    }

    test_fen_captures!(
        initial_position,
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"
    );
    test_fen_captures!(
        promotion,
        "rnbqkbnr/pp1ppppp/8/2p5/3Pp3/8/PPPP1PpP/RNBQKB1R b KQkq e5 0 2"
    );
    test_fen_captures!(
        castling,
        "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10"
    );
    test_fen_captures!(position_3, "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1");

    #[test]
    fn test_long_game_does_not_run_off_the_history() {
        // games which reached about move 175 used to panic in is_repetition
        let mut board = Board::new();
        let cycle = ["g1f3", "b8c6", "f3g1", "c6b8"];
        for i in 0..400 {
            let name = cycle[i % 4];
            let m = *board
                .generate_moves()
                .iter()
                .find(|m| format!("{}", m) == name)
                .unwrap_or_else(|| panic!("move {} not found at ply {}", name, i));
            assert!(board.make_move(&m));
            board.is_repetition();
        }
    }

    #[test]
    fn test_is_repetition_with_fifty_move_count_beyond_history() {
        // the fifty move counter from the fen is larger than the history we hold
        let board = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 99 1").unwrap();
        assert_eq!(board.is_repetition(), false);
    }

    /// The search does not wait for the third occurrence. Once a position has
    /// come back once, either side can take the draw, so there is nothing to be
    /// gained by spending four more plies of depth confirming it.
    #[test]
    fn test_has_repeated_fires_a_cycle_before_is_repetition() {
        let mut board = Board::from_fen(
            "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 b - - 3 19",
        )
        .unwrap();
        let cycle = [(A8, B8), (A1, B1), (B8, A8), (B1, A1)];
        assert_eq!(board.has_repeated(), false);
        assert_eq!(board.is_repetition(), false);

        for (from, to) in cycle {
            board.make_move(&Play::new(from, to, None, None, false, false));
        }
        // first repeat: a draw is available, so the search stops here
        assert_eq!(board.has_repeated(), true);
        assert_eq!(board.is_repetition(), false);

        for (from, to) in cycle {
            board.make_move(&Play::new(from, to, None, None, false, false));
        }
        // second repeat: the game is actually drawn
        assert_eq!(board.has_repeated(), true);
        assert_eq!(board.is_repetition(), true);
    }

    #[test]
    fn test_is_repetition() {
        let mut board = Board::from_fen(
            "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 b - - 3 19",
        )
        .unwrap();
        // Position 1
        assert_eq!(board.is_repetition(), false);
        board.make_move(&Play::new(A8, B8, None, None, false, false));
        board.make_move(&Play::new(A1, B1, None, None, false, false));
        assert_eq!(board.is_repetition(), false);
        board.make_move(&Play::new(B8, A8, None, None, false, false));
        assert_eq!(board.is_repetition(), false);
        board.make_move(&Play::new(B1, A1, None, None, false, false));
        assert_eq!(board.is_repetition(), false);
        // Position 1 - (first repeat)
        board.make_move(&Play::new(A8, B8, None, None, false, false));
        assert_eq!(board.is_repetition(), false);
        board.make_move(&Play::new(A1, B1, None, None, false, false));
        assert_eq!(board.is_repetition(), false);
        board.make_move(&Play::new(B8, A8, None, None, false, false));
        assert_eq!(board.is_repetition(), false);
        board.make_move(&Play::new(B1, A1, None, None, false, false));
        // Position 1 - (second repeat)
        assert_eq!(board.is_repetition(), true);
    }
}

#[cfg(test)]
mod position_key {
    use super::Board;
    use super::Game;
    use pretty_assertions::{assert_eq, assert_ne};

    fn play_move(board: &mut Board, mv: &str) {
        let m = *board
            .generate_moves()
            .iter()
            .find(|m| format!("{}", m) == mv)
            .unwrap_or_else(|| panic!("move {} not found", mv));
        assert!(board.make_move(&m));
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
    fn en_passant_changes_key() {
        let without =
            Board::from_fen("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1").unwrap();
        let with =
            Board::from_fen("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1").unwrap();
        assert_ne!(without.key, with.key);
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
    use super::Board;
    use super::Game;
    use pretty_assertions::assert_eq;
    // TODO convert these tests to use macros
    // Positions and perft results taken from https://www.chessprogramming.org/Perft_Results

    #[test]
    fn test_perft_starting() {
        let mut board =
            Board::from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1").unwrap();
        assert_eq!(board.perft(1), 20);
        assert_eq!(board.perft(2), 400);
        assert_eq!(board.perft(3), 8902);
    }

    #[test]
    fn test_perft_position_2() {
        let mut board =
            Board::from_fen("r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1")
                .unwrap();
        assert_eq!(board.perft(1), 48);
        assert_eq!(board.perft(2), 2039);
        assert_eq!(board.perft(3), 97862);
        assert_eq!(board.perft(4), 4085603);
    }

    #[test]
    fn test_perft_position_3() {
        let mut board = Board::from_fen("8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1").unwrap();
        assert_eq!(board.perft(1), 14);
        assert_eq!(board.perft(2), 191);
        assert_eq!(board.perft(3), 2812);
        assert_eq!(board.perft(4), 43238);
        assert_eq!(board.perft(5), 674624);
        assert_eq!(board.perft(6), 11030083);
    }

    #[test]
    fn test_perft_position_4() {
        let mut board =
            Board::from_fen("r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1")
                .unwrap();
        assert_eq!(board.perft(1), 6);
        assert_eq!(board.perft(2), 264);
        assert_eq!(board.perft(3), 9467);
        assert_eq!(board.perft(4), 422333);
        assert_eq!(board.perft(5), 15833292);
    }

    #[test]
    fn test_perft_position_5() {
        let mut board =
            Board::from_fen("rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8").unwrap();
        assert_eq!(board.perft(1), 44);
        assert_eq!(board.perft(2), 1486);
        assert_eq!(board.perft(3), 62379);
        assert_eq!(board.perft(4), 2103487);
    }

    #[test]
    fn test_perft_position_6() {
        let mut board = Board::from_fen(
            "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
        )
        .unwrap();
        assert_eq!(board.perft(1), 46);
        assert_eq!(board.perft(2), 2079);
        assert_eq!(board.perft(3), 89890);
        assert_eq!(board.perft(4), 3894594);
    }
}

#[cfg(test)]
mod test_fen {
    use super::Board;
    use super::Game;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn random_str_doesnt_crash(s in ".*") {
            _ = Board::from_fen(&s);
        }

        //#[test]
        //fn random_fen_doesnt_crash(s in ("([NBRPKQnbrpkq1-9]{9}/){7}[NBRPKQnbrpkq1-9]{4,} [bw]{1} [kqKQ-]{1,4} [a-hA-H][1-9] [1-9]{1,} [1-9]{1,}").prop_filter("", |v| {println!("{}", v); true})) {
        //    _ = Board::from_fen(s);
        //}
    }

    #[test]
    fn test_starting() {
        assert!(
            Board::from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1").is_ok()
        );
    }

    #[test]
    fn test_from_wikipedia() -> Result<(), String> {
        Board::from_fen("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1")?;
        Board::from_fen("rnbqkbnr/pp1ppppp/8/2p5/4P3/8/PPPP1PPP/RNBQKBNR w KQkq c6 0 2")?;
        Board::from_fen("rnbqkbnr/pp1ppppp/8/2p5/4P3/5N2/PPPP1PPP/RNBQKB1R b KQkq - 1 2")?;
        Ok(())
    }

    #[test]
    fn test_invalid_extra_ranks() {
        assert!(
            Board::from_fen("rnbqkbnr/pppppppp/8/8/8/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1")
                .is_err()
        );
    }
    #[test]
    fn test_invalid_extra_slash() {
        assert!(
            Board::from_fen("rnbqkbnr/pppppppp/8/8//4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1")
                .is_err()
        );
    }
    // TODO uncomment this test and fix
    //#[test]
    //fn test_invalid_extra_file() {
    //    assert!(Board::from_fen(
    //        "rnbqkbnr/ppppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1".to_string()
    //    )
    //    .is_err());
    //}
    #[test]
    fn test_invalid_bad_piece() {
        assert!(
            Board::from_fen("rnbqkbnar/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1")
                .is_err()
        );
    }
}

#[cfg(test)]
mod perft_edge_cases {
    use super::Board;
    use super::Game;
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
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
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
    fn test_perft_edge_cases() {
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
