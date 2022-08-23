use super::misc::{
    coordinate_to_index, coordinate_to_large_index, index_to_coordinate, BitBoard,
    CastlePermissions, Color, Coordinate, File, Piece,
};
use crate::Game;
use std::fmt;

#[derive(Debug)]
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
}

lazy_static! {
    static ref ATTACK_MASKS: AttackMasks = AttackMasks::new();
    static ref BASE_CONVERSIONS: BaseConversions = BaseConversions::new();
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
        }
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

    fn set_piece(&mut self, piece: Piece, color: Color, rank: u64, file: &File) {
        match piece {
            Piece::Pawn => self.pawns.set_bit_from_coordinate(rank, file),
            Piece::Knight => self.knights.set_bit_from_coordinate(rank, file),
            Piece::Bishop => self.bishops.set_bit_from_coordinate(rank, file),
            Piece::Rook => self.rooks.set_bit_from_coordinate(rank, file),
            Piece::Queen => self.queens.set_bit_from_coordinate(rank, file),
            Piece::King => self.kings.set_bit_from_coordinate(rank, file),
        };
        match color {
            Color::Black => self.black.set_bit_from_coordinate(rank, file),
            Color::White => self.white.set_bit_from_coordinate(rank, file),
        };
    }

    fn get_piece(&self, rank: u64, file: File) -> (Option<Piece>, Option<Color>) {
        let mask = 1u64 << coordinate_to_index(rank, &file);
        let color = if (self.black & mask) > 0 {
            Some(Color::Black)
        } else if (self.white & mask) > 0 {
            Some(Color::White)
        } else {
            None
        };
        let piece = if (self.pawns & mask) > 0 {
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
        };
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
mod test_fen {
    use super::Board;
    use super::Game;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

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
