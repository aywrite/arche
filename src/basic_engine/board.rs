use super::misc::{
    coordinate_to_index, BitBoard, CastlePermissions, Color, Coordinate, File, Piece,
};
use crate::Game;

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

    fn debug_print(&self) {
        println!("    a b c d e f g h");
        println!("  -----------------");
        for rank in 1..=8 {
            print!("{} |", rank);
            for file in File::variants() {
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
                    Some(Color::White) => print!(" {}", c.to_uppercase()),
                    _ => print!(" {}", c),
                };
            }
            println!()
        }
        println!();
        println!(
            "{:?} {} {:?} {} {}",
            self.active_color,
            self.castle.to_fen(),
            self.en_passant,
            self.half_move_clock,
            self.move_number,
        );
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
