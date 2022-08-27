use super::misc::{
    coordinate_to_index, coordinate_to_large_index, index_to_coordinate, BitBoard,
    CastlePermissions, Color, Coordinate, File, Piece, PromotePiece,
};
use super::play::Play;
use crate::Game;
use std::fmt;

/// Play State is used to store the history of moves (plays)
///
/// Although the move/play object already contains most of the information we need, in order to
/// undo a move we need some additional state.
#[derive(Debug, Copy, Clone, PartialEq)]
struct PlayState {
    play: Play,

    en_passant: Option<Coordinate>,
    castle: CastlePermissions,
    fifty_move_rule: u64,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Board {
    pawns: u64,
    knights: u64,
    bishops: u64,
    rooks: u64,
    queens: u64,
    kings: u64,

    white: u64,
    black: u64,

    active_color: Color,
    castle: CastlePermissions,
    en_passant: Option<Coordinate>,

    half_move_clock: u64,
    move_number: u64,
    fifty_move_rule: u64,

    history: Vec<PlayState>,
}

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
    static ref BASE_CONVERSIONS: BaseConversions = BaseConversions::new();
    static ref CASTLE_PERMISSION_SQUARES: [u8; 6] = [
        coordinate_to_index(1, &File::A) as u8,
        coordinate_to_index(1, &File::E) as u8,
        coordinate_to_index(1, &File::H) as u8,
        coordinate_to_index(8, &File::A) as u8,
        coordinate_to_index(8, &File::E) as u8,
        coordinate_to_index(8, &File::H) as u8,
    ];
}

struct BaseConversions {
    index_64_to_index_100: [u8; 64],
    index_100_to_index_64: [u8; 100],
}

impl BaseConversions {
    const OFF_BOARD: u8 = 101;
    fn new() -> Self {
        let mut base = BaseConversions {
            index_100_to_index_64: [Self::OFF_BOARD; 100],
            index_64_to_index_100: [0u8; 64],
        };
        for rank in 1..=8 {
            for file in File::VARIANTS {
                let index = coordinate_to_large_index(rank, &file);
                let index_64 = coordinate_to_index(rank.into(), &file) as usize;
                base.index_100_to_index_64[index as usize] = index_64 as u8;
                base.index_64_to_index_100[index_64] = index;
            }
        }
        base
    }

    fn is_offboard(&self, index_100: usize) -> bool {
        self.index_100_to_index_64[index_100] == Self::OFF_BOARD
    }
}

impl fmt::Display for BaseConversions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for rank in 0..10 {
            for file in 0..10 {
                let index = file + (rank * 10);
                write!(f, " {:0>3}", self.index_100_to_index_64[index as usize])?;
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
            let (rank, file) = index_to_coordinate(i as u64);
            let mut kings: Vec<isize> = vec![-1, 1, -8, 8, 7, 9, -7, -9];
            let mut black_pawns: Vec<isize> = vec![7, 9]; // TODO these seem the wrong way around?
            let mut white_pawns: Vec<isize> = vec![-7, -9];
            let knights = [15, 17, -15, -17, 6, 10, -6, -10];

            let top_rank = i <= 7;
            let bottom_rank = i > 55;
            let left_edge = (i % 8) == 0;
            let right_edge = (i % 8) == 7;

            if top_rank {
                kings.retain(|j| ![-7, -8, -9].contains(j));
                white_pawns = vec![];
            } else if bottom_rank {
                kings.retain(|j| ![7, 8, 9].contains(j));
                black_pawns = vec![]
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

            for j in kings.iter() {
                let index = i + j;
                am.kings[i as usize].set_bit(index as u64);
            }

            for j in white_pawns.iter() {
                let index = i + j;
                am.white_pawns[i as usize].set_bit(index as u64);
            }
            for j in black_pawns.iter() {
                let index = i + j;
                am.black_pawns[i as usize].set_bit(index as u64);
            }
            for j in knights.iter() {
                let index = i + j;
                if index < 64 && index > 0 {
                    let (new_rank, new_file) = index_to_coordinate(index as u64);
                    let rank_diff = rank as isize - new_rank as isize;
                    let file_diff = file as isize - new_file as isize;

                    if (rank_diff).abs() <= 2 && (file_diff).abs() <= 2 {
                        am.knights[i as usize].set_bit(index as u64);
                    };
                }
            }

            for j in 0..8 {
                let horizontal_index = (i / 8 * 8) + j;
                let vertical_index = (i % 8) + (j * 8);
                am.straight[i as usize].set_bit(horizontal_index as u64);
                am.straight[i as usize].set_bit(vertical_index as u64);
            }

            let directions = [9isize, -9, 11, -11];
            for k in directions {
                let mut j = 0;
                loop {
                    let check_100_index =
                        BASE_CONVERSIONS.index_64_to_index_100[i as usize] as isize + (k * j);
                    if BASE_CONVERSIONS.is_offboard(check_100_index as usize) {
                        break;
                    };
                    let check_index =
                        BASE_CONVERSIONS.index_100_to_index_64[check_100_index as usize] as u64;
                    j += 1;
                    am.diagonal[i as usize].set_bit(check_index);
                }
            }
        }
        am
    }
}

impl Board {
    fn new() -> Board {
        Board {
            pawns: 0,
            knights: 0,
            bishops: 0,
            rooks: 0,
            queens: 0,
            kings: 0,
            white: 0,
            black: 0,

            active_color: Color::White,
            castle: CastlePermissions::new(),
            half_move_clock: 0,
            move_number: 0,
            en_passant: None,
            fifty_move_rule: 0,

            history: Vec::new(),
        }
    }

    pub fn generate_moves(&self) -> Vec<Play> {
        let mut moves = Vec::new();
        let (color_mask, capture_mask) = match self.active_color {
            Color::Black => (self.black, self.white),
            Color::White => (self.white, self.black),
        };
        let all_pieces = self.black | self.white;
        // knights
        let knights = (self.knights & color_mask).get_set_bits();
        for from in knights {
            // Only include moves which don't have another piece of our color at the to square
            let kmoves = ATTACK_MASKS.knights[from] & (!color_mask);
            for to in kmoves.get_set_bits() {
                let mut capture = None;
                if capture_mask.is_bit_set(to as u64) {
                    capture = self.get_piece_index(to as u8);
                }
                moves.push(Play::new(from as u8, to as u8, capture, None, false, false));
            }
        }
        // queens and rooks
        let queens_and_rooks = ((self.queens | self.rooks) & color_mask).get_set_bits();
        for from in queens_and_rooks {
            let directions = [10isize, -10, 1, -1];
            for i in directions {
                let mut j = 1;
                loop {
                    let mut capture = None;
                    let check_100_index =
                        BASE_CONVERSIONS.index_64_to_index_100[from] as isize + (i * j);
                    if BASE_CONVERSIONS.is_offboard(check_100_index as usize) {
                        break;
                    };
                    let to = BASE_CONVERSIONS.index_100_to_index_64[check_100_index as usize];
                    if capture_mask.is_bit_set(to.into()) {
                        capture = self.get_piece_index(to);
                        moves.push(Play::new(from as u8, to, capture, None, false, false));
                        break;
                    } else if color_mask.is_bit_set(to.into()) {
                        break;
                    }
                    moves.push(Play::new(from as u8, to, capture, None, false, false));
                    j += 1;
                }
            }
        }
        // queens and bishops
        let queens_and_bishops = ((self.queens | self.bishops) & color_mask).get_set_bits();
        for from in queens_and_bishops {
            let directions = [9isize, -9, 11, -11];
            for i in directions {
                let mut j = 1;
                loop {
                    let mut capture = None;
                    let check_100_index =
                        BASE_CONVERSIONS.index_64_to_index_100[from] as isize + (i * j);
                    if BASE_CONVERSIONS.is_offboard(check_100_index as usize) {
                        break;
                    };
                    let to = BASE_CONVERSIONS.index_100_to_index_64[check_100_index as usize];
                    if capture_mask.is_bit_set(to.into()) {
                        capture = self.get_piece_index(to);
                        moves.push(Play::new(from as u8, to, capture, None, false, false));
                        break;
                    } else if color_mask.is_bit_set(to.into()) {
                        break;
                    }
                    moves.push(Play::new(from as u8, to, capture, None, false, false));
                    j += 1;
                }
            }
        }
        // kings
        let kings = (self.kings & color_mask).get_set_bits();
        for from in kings {
            // Only include moves which don't have another piece of our color at the to square
            let kmove = ATTACK_MASKS.kings[from] & (!color_mask);
            for to in kmove.get_set_bits() {
                let mut capture = None;
                if capture_mask.is_bit_set(to as u64) {
                    capture = self.get_piece_index(to as u8);
                }
                moves.push(Play::new(from as u8, to as u8, capture, None, false, false));
            }
            // 1. castle permission is available
            // 2. king is not in check
            // 3. movement squares are not occupied
            // 4. None of the squares are in check
            if matches!(self.active_color, Color::White) {
                let check = self.square_attacked(4, Color::Black);
                if !check {
                    if self.castle.white_queen_side
                        && !check
                        && [B1, C1, D1]
                            .iter()
                            .all(|i| !all_pieces.is_bit_set((*i).into()))
                        && [C1, D1]
                            .iter()
                            .all(|i| !self.square_attacked(*i, Color::Black))
                    {
                        moves.push(Play::new(from as u8, 2u8, None, None, false, true));
                    }
                    if self.castle.white_king_side
                        && !check
                        && [5, 6].iter().all(|i| !all_pieces.is_bit_set(*i))
                        && [5u8, 6]
                            .iter()
                            .all(|i| !self.square_attacked(*i, Color::Black))
                    {
                        moves.push(Play::new(from as u8, 6u8, None, None, false, true));
                    }
                }
            }
            if matches!(self.active_color, Color::Black) {
                let check = self.square_attacked(4, Color::White);
                if !check {
                    if self.castle.black_queen_side
                        && [57, 58, 59].iter().all(|i| !all_pieces.is_bit_set(*i))
                        && [58u8, 59]
                            .iter()
                            .all(|i| !self.square_attacked(*i, Color::White))
                    {
                        moves.push(Play::new(from as u8, 58u8, None, None, false, true));
                    }
                    if self.castle.black_king_side
                        && [61, 62].iter().all(|i| !all_pieces.is_bit_set(*i))
                        && [61u8, 62]
                            .iter()
                            .all(|i| !self.square_attacked(*i, Color::White))
                    {
                        moves.push(Play::new(from as u8, 62u8, None, None, false, true));
                    }
                }
            }
        }
        //pawns
        let pawns = (self.pawns & color_mask).get_set_bits();
        for from in pawns {
            let (rank, _) = index_to_coordinate(from as u64);
            let can_promote = match self.active_color {
                Color::White => rank == 7,
                Color::Black => rank == 2,
            };
            // move diagonally and capture
            let pmoves = match self.active_color {
                Color::White => ATTACK_MASKS.black_pawns[from] & (capture_mask),
                Color::Black => ATTACK_MASKS.white_pawns[from] & (capture_mask),
            };
            for to in pmoves.get_set_bits() {
                let capture = self.get_piece_index(to as u8);
                if can_promote {
                    for p in PromotePiece::VARIANTS {
                        moves.push(Play::new(
                            from as u8,
                            to as u8,
                            capture,
                            Some(p),
                            false,
                            false,
                        ));
                    }
                } else {
                    moves.push(Play::new(from as u8, to as u8, capture, None, false, false));
                }
            }
            // move forward
            let to = match self.active_color {
                Color::White => from as isize + 8,
                Color::Black => from as isize - 8,
            };
            // can't make a forward move if the square is occupied
            if (0..64).contains(&to) && !all_pieces.is_bit_set(to as u64) {
                if can_promote {
                    for p in PromotePiece::VARIANTS {
                        moves.push(Play::new(from as u8, to as u8, None, Some(p), false, false));
                    }
                } else {
                    moves.push(Play::new(from as u8, to as u8, None, None, false, false));
                    if match self.active_color {
                        Color::White => rank == 2,
                        Color::Black => rank == 7,
                    } {
                        let to = match self.active_color {
                            Color::White => to as isize + 8,
                            Color::Black => to as isize - 8,
                        };
                        // can't make a double forward move if the to square is occupied
                        if !all_pieces.is_bit_set(to as u64) {
                            moves.push(Play::new(from as u8, to as u8, None, None, false, false));
                        }
                    }
                }
            }
            // en passant
            if let Some(en_passant) = &self.en_passant {
                let i = en_passant.to_index();
                let can_en_passant = match self.active_color {
                    Color::White => ATTACK_MASKS.black_pawns[from].is_bit_set(i.into()),
                    Color::Black => ATTACK_MASKS.white_pawns[from].is_bit_set(i.into()),
                };
                if can_en_passant {
                    moves.push(Play::new(
                        from as u8,
                        i,
                        Some(Piece::Pawn),
                        None,
                        true,
                        false,
                    ));
                }
            }
        }
        moves
    }

    pub fn square_attacked(&self, index: u8, color: Color) -> bool {
        let all = self.black | self.white;
        let attack_masks = &ATTACK_MASKS;
        let color_mask = match color {
            Color::Black => self.black,
            Color::White => self.white,
        };
        // pawns
        let pawn_masks = match color {
            Color::Black => attack_masks.black_pawns,
            Color::White => attack_masks.white_pawns,
        };
        if (pawn_masks[index as usize] & self.pawns & color_mask) > 0 {
            return true;
        }
        // TODO handle en passant?

        // knights
        if (attack_masks.knights[index as usize] & self.knights & color_mask) > 0 {
            return true;
        }

        // rooks & queens
        if (attack_masks.straight[index as usize] & (self.rooks | self.queens) & color_mask) > 0 {
            // if is necessary but not sufficient to show attack
            let directions = [10isize, -10, 1, -1];
            for i in directions {
                let mut j = 1;
                loop {
                    let check_100_index =
                        BASE_CONVERSIONS.index_64_to_index_100[index as usize] as isize + (i * j);
                    if BASE_CONVERSIONS.is_offboard(check_100_index as usize) {
                        break;
                    };
                    let check_index =
                        BASE_CONVERSIONS.index_100_to_index_64[check_100_index as usize] as u64;
                    if (self.rooks | self.queens).is_bit_set(check_index) {
                        return true;
                    } else if all.is_bit_set(check_index) {
                        break;
                    }
                    j += 1;
                }
            }
        };

        // bishops & queens
        if (attack_masks.diagonal[index as usize] & (self.bishops | self.queens) & color_mask) > 0 {
            let directions = [9isize, -9, 11, -11];
            for i in directions {
                let mut j = 1;
                loop {
                    let check_100_index =
                        BASE_CONVERSIONS.index_64_to_index_100[index as usize] as isize + (i * j);
                    if BASE_CONVERSIONS.is_offboard(check_100_index as usize) {
                        break;
                    };
                    let check_index =
                        BASE_CONVERSIONS.index_100_to_index_64[check_100_index as usize] as u64;
                    if (self.bishops | self.queens).is_bit_set(check_index) {
                        return true;
                    } else if all.is_bit_set(check_index) {
                        break;
                    }
                    j += 1;
                }
            }
        };

        // kings
        if (attack_masks.kings[index as usize] & self.kings & color_mask) > 0 {
            return true;
        };
        false
    }

    pub fn make_move(&mut self, play: &Play) -> bool {
        self.history.push(PlayState {
            play: *play,
            en_passant: self.en_passant,
            castle: self.castle,
            fifty_move_rule: self.fifty_move_rule,
        });
        let opposing_color = match self.active_color {
            Color::White => Color::Black,
            Color::Black => Color::White,
        };
        // update castleing permissions
        match play.from {
            0 => self.castle.white_queen_side = false,
            4 => {
                self.castle.white_queen_side = false;
                self.castle.white_king_side = false
            }
            7 => self.castle.white_king_side = false,
            56 => self.castle.black_queen_side = false,
            60 => {
                self.castle.black_queen_side = false;
                self.castle.black_king_side = false
            }
            63 => self.castle.black_king_side = false,
            _ => (),
        }
        self.en_passant = None;

        if self.pawns.is_bit_set(play.from.into()) {
            // pawn moves reset the fifty move rule
            self.fifty_move_rule = 0;
            if (play.from as isize - play.to as isize).abs() == 16 {
                // if a pawn moved two squares forward then we must update the en_passant square
                self.en_passant = match self.active_color {
                    Color::White => Some(Coordinate::from_index(play.to - 8)),
                    Color::Black => Some(Coordinate::from_index(play.to + 8)),
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
            // move rook if casteling
            match play.to {
                C1 => self.move_piece(A1, D1, Piece::Rook, None, self.active_color),
                C8 => self.move_piece(A8, D8, Piece::Rook, None, self.active_color),
                G1 => self.move_piece(H1, F1, Piece::Rook, None, self.active_color),
                G8 => self.move_piece(H8, F8, Piece::Rook, None, self.active_color),
                _ => unreachable!(),
            }
        }

        // update the ply
        self.half_move_clock += 1;
        if matches!(self.active_color, Color::Black) {
            // update the full move counter
            self.move_number += 1;
        }

        // return false if king in check
        let king_index = match self.active_color {
            Color::White => (self.kings & self.white).get_set_bits()[0],
            Color::Black => (self.kings & self.black).get_set_bits()[0],
        };
        self.active_color = opposing_color;
        return if self.square_attacked(king_index as u8, opposing_color) {
            self.undo_move().unwrap();
            false
        } else {
            true
        };
    }

    pub fn undo_move(&mut self) -> Result<(), &str> {
        let history = self
            .history
            .pop()
            .ok_or("Failed to undo move: no more history")?;
        let play = history.play;

        let opposing_color = match self.active_color {
            Color::White => Color::Black,
            Color::Black => Color::White,
        };
        // update castleing permissions
        self.castle = history.castle;
        self.en_passant = history.en_passant;
        self.fifty_move_rule = history.fifty_move_rule;
        self.half_move_clock -= 1;
        if matches!(opposing_color, Color::Black) {
            // update the full move counter
            self.move_number -= 1;
        }

        if self.pawns.is_bit_set(play.from.into()) {
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
            // move rook if casteling
            match play.to {
                C1 => self.move_piece(D1, A1, Piece::Rook, None, opposing_color),
                C8 => self.move_piece(D8, A8, Piece::Rook, None, opposing_color),
                G1 => self.move_piece(F1, H1, Piece::Rook, None, opposing_color),
                G8 => self.move_piece(F8, H8, Piece::Rook, None, opposing_color),
                _ => unreachable!(),
            }
        }

        self.active_color = opposing_color;
        Ok(())
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
        debug_assert!(!(self.black & self.white).is_bit_set(to.into()));
        self.clear_piece_index(from, piece, color);
        if let Some(promote) = promote_piece {
            self.set_piece_index(to, (&promote).into(), color);
        } else {
            self.set_piece_index(to, piece, color);
        }
    }

    fn attacked_print(&self) {
        println!("    a b c d e f g h");
        println!("  -----------------");
        for rank in 1..=8 {
            print!("{} |", rank);
            for file in File::VARIANTS {
                let index = coordinate_to_index(rank, &file);
                if self.square_attacked(index as u8, Color::White) {
                    print!(" x");
                } else {
                    print!(" .");
                }
            }
            println!()
        }
        println!();
    }

    fn set_piece_index(&mut self, index: u8, piece: Piece, color: Color) {
        match piece {
            Piece::Pawn => self.pawns.set_bit(index.into()),
            Piece::Knight => self.knights.set_bit(index.into()),
            Piece::Bishop => self.bishops.set_bit(index.into()),
            Piece::Rook => self.rooks.set_bit(index.into()),
            Piece::Queen => self.queens.set_bit(index.into()),
            Piece::King => self.kings.set_bit(index.into()),
        };
        match color {
            Color::Black => self.black.set_bit(index.into()),
            Color::White => self.white.set_bit(index.into()),
        };
    }

    fn set_piece(&mut self, piece: Piece, color: Color, rank: u64, file: &File) {
        let index = coordinate_to_index(rank, file) as u8;
        self.set_piece_index(index, piece, color);
    }

    fn clear_piece_index(&mut self, index: u8, piece: Piece, color: Color) {
        match piece {
            Piece::Pawn => self.pawns.clear_bit(index.into()),
            Piece::Knight => self.knights.clear_bit(index.into()),
            Piece::Bishop => self.bishops.clear_bit(index.into()),
            Piece::Rook => self.rooks.clear_bit(index.into()),
            Piece::Queen => self.queens.clear_bit(index.into()),
            Piece::King => self.kings.clear_bit(index.into()),
        };
        match color {
            Color::Black => self.black.clear_bit(index.into()),
            Color::White => self.white.clear_bit(index.into()),
        };
    }

    fn get_piece_index(&self, index: u8) -> Option<Piece> {
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

    fn get_piece(&self, rank: u64, file: File) -> (Option<Piece>, Option<Color>) {
        let index = coordinate_to_index(rank, &file);
        let mask = 1u64 << index;
        let color = if (self.black & mask) > 0 {
            Some(Color::Black)
        } else if (self.white & mask) > 0 {
            Some(Color::White)
        } else {
            None
        };
        let piece = self.get_piece_index(index as u8);
        (piece, color)
    }
}

impl Game for Board {
    fn from_fen(fen: String) -> Result<Self, String> {
        let mut fen_iter = fen.split(' ');
        let position = fen_iter
            .next()
            .ok_or("Error parsing FEN: could not find position block")?;
        let active_color_token = match fen_iter.next() {
            Some(c) => {
                if c.len() != 1 {
                    Err("Expected a single character token")
                } else {
                    c.chars().next().ok_or("Expected a single character token")
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

            half_move_clock: half_move_clock.parse::<u64>().map_err(|e| e.to_string())?,
            move_number: full_move_clock.parse::<u64>().map_err(|e| e.to_string())?,
            en_passant: Coordinate::from_string(en_passant)?,
            fifty_move_rule: 0,

            history: Vec::new(),
        };

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
                board.set_piece(p, color, rank, &file);
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
        Ok(board)
    }
}

impl fmt::Display for Board {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "    a b c d e f g h")?;
        writeln!(f, "  -----------------")?;
        for rank in 1..=8 {
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
            "{:?} {} {:?} {} {}",
            self.active_color,
            self.castle.to_fen(),
            self.en_passant,
            self.half_move_clock,
            self.move_number,
        )?;
        writeln!(f)?;
        Ok(())
    }
}

#[cfg(test)]
mod make_move {
    use super::Board;
    use super::Game;
    use pretty_assertions::{assert_eq, assert_ne};

    fn do_undo(board: Board) {
        let moves = board.generate_moves();
        for m in moves.iter() {
            let old = board.clone();
            let mut new = board.clone();
            if new.make_move(m) {
                assert_ne!(old, new);
                new.undo_move().unwrap();
                assert_eq!(old, new);
            }
        }
    }

    #[test]
    fn test_reversible_starting() {
        let board =
            Board::from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1".to_string())
                .unwrap();
        do_undo(board);
    }

    #[test]
    fn test_reversible_1() {
        let board = Board::from_fen(
            "rnbqkbnr/pp1ppppp/8/2p5/3Pp3/8/PPPP1PpP/RNBQKB1R b KQkq e5 0 2".to_string(),
        )
        .unwrap();
        do_undo(board);
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
            _ = Board::from_fen(s);
        }

        #[test]
        fn random_fen_doesnt_crash(s in ("([NBRPKQnbrpkq1-9]{9}/){7}[NBRPKQnbrpkq1-9]{4,} [bw]{1} [kqKQ-]{1,4} [a-hA-H][1-9] [1-9]{1,} [1-9]{1,}").prop_filter("", |v| {println!("{}", v); true})) {
            _ = Board::from_fen(s);
        }
    }

    #[test]
    fn test_starting() {
        assert!(Board::from_fen(
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1".to_string()
        )
        .is_ok());
    }

    #[test]
    fn test_from_wikipedia() -> Result<(), String> {
        Board::from_fen("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1".to_string())?;
        Board::from_fen(
            "rnbqkbnr/pp1ppppp/8/2p5/4P3/8/PPPP1PPP/RNBQKBNR w KQkq c6 0 2".to_string(),
        )?;
        Board::from_fen(
            "rnbqkbnr/pp1ppppp/8/2p5/4P3/5N2/PPPP1PPP/RNBQKB1R b KQkq - 1 2".to_string(),
        )?;
        Ok(())
    }

    #[test]
    fn test_invalid_extra_ranks() {
        assert!(Board::from_fen(
            "rnbqkbnr/pppppppp/8/8/8/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1".to_string()
        )
        .is_err());
    }
    #[test]
    fn test_invalid_extra_slash() {
        assert!(Board::from_fen(
            "rnbqkbnr/pppppppp/8/8//4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1".to_string()
        )
        .is_err());
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
        assert!(Board::from_fen(
            "rnbqkbnar/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1".to_string()
        )
        .is_err());
    }
}
