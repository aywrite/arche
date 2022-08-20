use std::mem;

fn main() {
    let board =
        Board::from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1".to_string())
            .unwrap();
    board.debug_print();
}

struct Board {
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

#[derive(Debug)]
struct Coordinate {
    rank: u8,
    file: File,
}

impl Coordinate {
    fn from_string(s: &str) -> Result<Option<Self>, String> {
        if s == "-" {
            return Ok(None);
        }
        if s.len() != 2 {
            return Err(format!("Expected two characters, got {}", s.len()));
        }
        let mut chars = s.chars();
        let c = Coordinate {
            file: File::try_from(chars.next().unwrap())?,
            rank: u8::try_from(chars.next().unwrap()).or_else(|e| Err(e.to_string()))?,
        };
        Ok(Some(c))
    }
}

// Each color/side bit is true if that color is still allowed to castle on that side
struct CastlePermissions {
    black_king_side: bool,
    black_queen_side: bool,
    white_king_side: bool,
    white_queen_side: bool,
}

impl CastlePermissions {
    fn new() -> Self {
        CastlePermissions {
            black_king_side: true,
            black_queen_side: true,
            white_king_side: true,
            white_queen_side: true,
        }
    }
    fn from_string(s: &str) -> Result<CastlePermissions, String> {
        let mut perms = CastlePermissions {
            black_king_side: false,
            black_queen_side: false,
            white_king_side: false,
            white_queen_side: false,
        };
        if s == "-" {
            return Ok(perms);
        };
        for c in s.chars() {
            match c {
                'k' => perms.black_king_side = true,
                'q' => perms.black_queen_side = true,
                'K' => perms.white_king_side = true,
                'Q' => perms.white_queen_side = true,
                _ => {
                    return Err(format!(
                        "Unexpected character {} in castle permissions token",
                        c
                    ))
                }
            }
        }
        Ok(perms)
    }
    fn to_string(&self) -> String {
        let mut s = String::new();
        if self.white_king_side {
            s.push_str("K")
        };
        if self.white_queen_side {
            s.push_str("Q")
        };
        if self.black_king_side {
            s.push_str("k")
        };
        if self.black_queen_side {
            s.push_str("q")
        };
        if s.len() == 0 {
            s.push_str("-")
        };
        s
    }
}

trait BitBoard {
    fn set_bit(&mut self, index: u64);
    fn clear_bit(&mut self, index: u64);
    fn count(&self) -> usize;
    fn set_bit_from_coordinate(&mut self, rank: u64, file: &File) {
        self.set_bit(coordinate_to_index(rank, file));
    }
    fn clear_bit_from_coordinate(&mut self, rank: u64, file: &File) {
        self.clear_bit(coordinate_to_index(rank, file));
    }
    fn debug_print(&self);
}

impl BitBoard for u64 {
    fn set_bit(&mut self, index: u64) {
        // TODO precompute the set bit mask in an array
        _ = mem::replace(self, *self | (1u64 << index));
    }
    fn clear_bit(&mut self, index: u64) {
        // TODO precompute the clear bit mask in an array
        _ = mem::replace(self, *self ^ (1u64 << index));
    }
    fn count(&self) -> usize {
        self.count_ones() as usize
    }
    fn debug_print(&self) {
        print!("    a b c d e f g h\n");
        print!("  -----------------\n");
        for rank in 1..9 {
            print!("{} |", rank);
            for file in File::variants() {
                if (self & (1u64 << coordinate_to_index(rank, &file))) > 0 {
                    print!(" x");
                } else {
                    print!(" .");
                }
            }
            print!("\n")
        }
    }
}

impl Board {
    fn new() -> Board {
        let mut board = Board {
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
        };
        board
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

    fn from_fen(fen: String) -> Result<Board, String> {
        let mut fen_iter = fen.split(" ");
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
            castle: CastlePermissions::from_string(castle)?,

            half_move_clock: half_move_clock
                .parse::<u64>()
                .or_else(|e| Err(e.to_string()))?,
            move_number: full_move_clock
                .parse::<u64>()
                .or_else(|e| Err(e.to_string()))?,
            en_passant: Coordinate::from_string(en_passant)?,
        };

        // parse out the pieces on the board
        let mut rank = 1;
        let mut file = File::A;
        for c in position.chars() {
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
                _ => panic!("unexpected character in fen"),
            };
            if let Some(p) = piece {
                let color = if c.is_uppercase() {
                    Color::Black
                } else {
                    Color::White
                };
                board.set_piece(p, color, rank, &file);
            }

            file = match c {
                '1'..='8' => file.add(c.to_digit(10).unwrap()),
                'r' | 'b' | 'n' | 'k' | 'q' | 'p' => file.add(1),
                'R' | 'B' | 'N' | 'K' | 'Q' | 'P' => file.add(1),
                '/' => {
                    rank += 1;
                    File::A
                }
                _ => panic!("unexpected character in fen"),
            };
        }
        Ok(board)
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

    fn debug_print(&self) {
        print!("    a b c d e f g h\n");
        print!("  -----------------\n");
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
                    Some(Color::Black) => print!(" {}", c.to_uppercase()),
                    _ => print!(" {}", c),
                };
            }
            print!("\n")
        }
        print!("\n");
        println!(
            "{:?} {} {:?} {} {}",
            self.active_color,
            self.castle.to_string(),
            self.en_passant,
            self.half_move_clock,
            self.move_number,
        );
    }
}

fn coordinate_to_index(rank: u64, file: &File) -> u64 {
    ((rank - 1) * 8) + (*file) as u64
}

enum Piece {
    Pawn,
    Knight,
    Bishop,
    Rook,
    Queen,
    King,
}

#[derive(Debug)]
enum Color {
    Black,
    White,
}

impl Color {
    fn from_char(c: char) -> Option<Color> {
        match c {
            'b' | 'B' => Some(Color::Black),
            'w' | 'W' => Some(Color::White),
            _ => None,
        }
    }
}

#[derive(Copy, Clone, Debug)]
enum File {
    A = 0,
    B = 1,
    C = 2,
    D = 3,
    E = 4,
    F = 5,
    G = 6,
    H = 7,
}

impl File {
    fn variants() -> [File; 8] {
        [
            File::A,
            File::B,
            File::C,
            File::D,
            File::E,
            File::F,
            File::G,
            File::H,
        ]
    }

    fn add(&self, value: u32) -> File {
        let new_value = ((*self as usize) + value as usize) % 8;
        File::variants()[new_value]
    }
}

impl TryFrom<&str> for File {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        if s.len() != 1 {
            return Err(format!("Expected a single character token, got: {}", s));
        };
        let c = s.chars().next().unwrap();
        File::try_from(c)
    }
}

impl TryFrom<char> for File {
    type Error = String;

    fn try_from(c: char) -> Result<Self, Self::Error> {
        match c {
            'A' => Ok(File::A),
            'B' => Ok(File::B),
            'C' => Ok(File::C),
            'D' => Ok(File::D),
            'E' => Ok(File::E),
            'F' => Ok(File::F),
            'G' => Ok(File::G),
            'H' => Ok(File::H),
            _ => Err(format!("{} is not a valid File token", c)),
        }
    }
}

//fn main() {
//    let mut game = Game::new();
//    let player_1 = RandomEngine::new();
//    let player_2 = RandomEngine::new();
//    while !game.is_finished {
//        game.make_move(player_1.suggest_move(&game))
//            .expect("expect only legal moves");
//        game.make_move(player_2.suggest_move(&game))
//            .expect("expect only legal moves");
//    }
//}
//
//struct Game {
//    is_finished: bool,
//    fifty_move_rule: usize,
//    repeated: usize,
//    pieces: [usize; 120],
//}
//
//enum Color {
//    Black,
//    White,
//}
//
//
//#[derive(Copy, Clone, Debug)]
//struct Move {}
//
//impl Game {
//    fn new() -> Game {
//        Game { is_finished: false }
//    }
//
//    pub fn list_possible_moves(&self) -> Vec<Move> {
//        Vec::new()
//    }
//
//    fn make_move(&mut self, play: Move) -> Result<(), &'static str> {
//        todo!()
//    }
//}
//
//trait Engine {
//    fn suggest_move(&self, game: &Game) -> Move;
//}
//
//struct RandomEngine {}
//
//impl RandomEngine {
//    fn new() -> RandomEngine {
//        RandomEngine {}
//    }
//}
//
//impl Engine for RandomEngine {
//    fn suggest_move(&self, game: &Game) -> Move {
//        *game
//            .list_possible_moves()
//            .choose(&mut rand::thread_rng())
//            .unwrap()
//    }
//}
