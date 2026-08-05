use std::fmt;
use std::ops::Not;

/// An evaluation, in centipawns.
///
/// Sixteen bits is what engines generally store a score in, and the reason is
/// the transposition table: a narrower score makes for a smaller entry, so a
/// table of a given size holds more positions and more of it fits in cache.
///
/// The range is far wider than anything a score needs. Mate is thirty thousand,
/// and an evaluation is bounded by the material on the board, which even with
/// every pawn promoted to a queen comes to a little over ten thousand.
pub type Score = i16;

/// One step of splitmix64, which turns a seed into a stream of well spread
/// numbers. Small enough to run at compile time, which is what the zorbrist
/// keys need, and good enough for the magic search, which only wants candidates
/// that are well spread rather than unpredictable.
///
/// Returns the value alongside the next state rather than taking a `&mut` so
/// that draws chain, which is what the magic search wants when it needs three
/// of them for one candidate.
pub const fn split_mix(state: u64) -> (u64, u64) {
    let state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31), state)
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct Coordinate {
    rank: u8,
    file: File,
}

impl Coordinate {
    pub fn from_string(s: &str) -> Result<Option<Self>, String> {
        if s == "-" {
            return Ok(None);
        }
        let mut chars = s.chars();
        let (Some(file_char), Some(rank_char), None) = (chars.next(), chars.next(), chars.next())
        else {
            return Err(format!("Expected two characters, got {}", s));
        };
        let rank = rank_char
            .to_digit(10)
            .ok_or_else(|| format!("Expected a digit for the rank, got {}", rank_char))?
            as u8;
        if !(1..=8).contains(&rank) {
            return Err(format!("Rank must be between 1 and 8, got {}", rank));
        }
        let c = Coordinate {
            file: File::try_from(file_char)?,
            rank,
        };
        Ok(Some(c))
    }
    pub fn as_index(self) -> u8 {
        coordinate_to_index(self.rank, self.file)
    }
    pub fn from_index(index: u8) -> Self {
        let (rank, file) = index_to_coordinate(index);
        Coordinate { rank, file }
    }
}

#[cfg(test)]
mod test_coordinate {
    use super::Coordinate;

    #[test]
    fn parse_valid() {
        assert!(Coordinate::from_string("e3").unwrap().is_some());
        assert!(Coordinate::from_string("a1").unwrap().is_some());
        assert!(Coordinate::from_string("h8").unwrap().is_some());
        assert!(Coordinate::from_string("-").unwrap().is_none());
    }

    #[test]
    fn parse_invalid_does_not_panic() {
        assert!(Coordinate::from_string("").is_err());
        assert!(Coordinate::from_string("e").is_err());
        assert!(Coordinate::from_string("ee").is_err());
        assert!(Coordinate::from_string("e0").is_err());
        assert!(Coordinate::from_string("e9").is_err());
        assert!(Coordinate::from_string("e33").is_err());
        assert!(Coordinate::from_string("é").is_err()); // two bytes, one char
    }
}

// Each color/side bit is true if that color is still allowed to castle on that side
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct CastlePermissions {
    pub black_king_side: bool,
    pub black_queen_side: bool,
    pub white_king_side: bool,
    pub white_queen_side: bool,
}

impl CastlePermissions {
    pub fn from_fen(s: &str) -> Result<CastlePermissions, String> {
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
                    ));
                }
            }
        }
        Ok(perms)
    }
    pub fn as_fen(&self) -> String {
        let mut s = String::new();
        if self.white_king_side {
            s.push('K');
        };
        if self.white_queen_side {
            s.push('Q');
        };
        if self.black_king_side {
            s.push('k');
        };
        if self.black_queen_side {
            s.push('q');
        };
        if s.is_empty() {
            s.push('-');
        };
        s
    }
}

#[cfg(test)]
mod test_castle_permissions {
    use super::CastlePermissions;

    #[test]
    fn round_trip_all() {
        let initial = "KQkq";
        assert_eq!(
            CastlePermissions::from_fen(initial).unwrap().as_fen(),
            initial,
        );
    }

    #[test]
    fn round_trip_none() {
        let initial = "-";
        assert_eq!(
            CastlePermissions::from_fen(initial).unwrap().as_fen(),
            initial,
        );
    }

    #[test]
    fn round_trip_mixed() {
        let initial = "Kq";
        assert_eq!(
            CastlePermissions::from_fen(initial).unwrap().as_fen(),
            initial,
        );
    }

    #[test]
    fn invalid_chars() {
        let initial = "ksd";
        assert!(CastlePermissions::from_fen(initial).is_err());
    }
}

pub fn coordinate_to_index(rank: u8, file: File) -> u8 {
    ((rank - 1) * 8) + (file) as u8
}

pub fn coordinate_to_large_index(rank: u8, file: File) -> u8 {
    ((rank - 1) * 10) + (file) as u8 + 11
}

pub fn index_to_coordinate(index: u8) -> (u8, File) {
    let rank = ((index) / 8) + 1;
    let file = File::try_from(index % 8).unwrap();
    (rank, file)
}

#[cfg(test)]
mod test_index_coordinate_conversion {
    use super::coordinate_to_index;
    use super::index_to_coordinate;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn round_trip(i in 0u8..64) {
            let (rank, file) = index_to_coordinate(i);
            assert_eq!(i, coordinate_to_index(rank, file));
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum PromotePiece {
    Knight,
    Bishop,
    Rook,
    Queen,
}

impl PromotePiece {
    pub const VARIANTS: [PromotePiece; 4] = [
        PromotePiece::Knight,
        PromotePiece::Bishop,
        PromotePiece::Rook,
        PromotePiece::Queen,
    ];
}

impl From<&PromotePiece> for char {
    fn from(c: &PromotePiece) -> Self {
        match c {
            PromotePiece::Knight => 'n',
            PromotePiece::Bishop => 'b',
            PromotePiece::Rook => 'r',
            PromotePiece::Queen => 'q',
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum Piece {
    Pawn,
    Knight,
    Bishop,
    Rook,
    Queen,
    King,
}

impl Piece {
    pub fn material_value(self) -> u32 {
        match self {
            Piece::Pawn => 100,
            Piece::Knight => 310,
            Piece::Bishop => 320,
            Piece::Rook => 500,
            Piece::Queen => 900,
            Piece::King => 10000,
        }
    }
}

/// The piece's letter as fen and the board display write it, in lowercase.
/// Case carries the colour, which is not the piece's to know.
impl From<Piece> for char {
    fn from(piece: Piece) -> Self {
        match piece {
            Piece::Pawn => 'p',
            Piece::Knight => 'n',
            Piece::Bishop => 'b',
            Piece::Rook => 'r',
            Piece::Queen => 'q',
            Piece::King => 'k',
        }
    }
}

impl TryFrom<char> for Piece {
    type Error = String;

    fn try_from(c: char) -> Result<Self, Self::Error> {
        match c.to_ascii_lowercase() {
            'p' => Ok(Piece::Pawn),
            'n' => Ok(Piece::Knight),
            'b' => Ok(Piece::Bishop),
            'r' => Ok(Piece::Rook),
            'q' => Ok(Piece::Queen),
            'k' => Ok(Piece::King),
            _ => Err(format!("{} is not a piece", c)),
        }
    }
}

impl From<&PromotePiece> for Piece {
    fn from(c: &PromotePiece) -> Self {
        match c {
            PromotePiece::Knight => Piece::Knight,
            PromotePiece::Bishop => Piece::Bishop,
            PromotePiece::Rook => Piece::Rook,
            PromotePiece::Queen => Piece::Queen,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum Color {
    Black,
    White,
}

impl Color {
    pub fn from_char(c: char) -> Option<Color> {
        match c {
            'b' | 'B' => Some(Color::Black),
            'w' | 'W' => Some(Color::White),
            _ => None,
        }
    }
}

impl Not for Color {
    type Output = Self;

    fn not(self) -> Self::Output {
        match self {
            Color::White => Color::Black,
            Color::Black => Color::White,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum File {
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
    pub const VARIANTS: [File; 8] = [
        File::A,
        File::B,
        File::C,
        File::D,
        File::E,
        File::F,
        File::G,
        File::H,
    ];
}

impl fmt::Display for File {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            File::A => write!(f, "a")?,
            File::B => write!(f, "b")?,
            File::C => write!(f, "c")?,
            File::D => write!(f, "d")?,
            File::E => write!(f, "e")?,
            File::F => write!(f, "f")?,
            File::G => write!(f, "g")?,
            File::H => write!(f, "h")?,
        }
        Ok(())
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

impl From<File> for u64 {
    fn from(file: File) -> Self {
        file as u64
    }
}

impl TryFrom<u8> for File {
    type Error = String;

    fn try_from(i: u8) -> Result<Self, Self::Error> {
        match i {
            0 => Ok(File::A),
            1 => Ok(File::B),
            2 => Ok(File::C),
            3 => Ok(File::D),
            4 => Ok(File::E),
            5 => Ok(File::F),
            6 => Ok(File::G),
            7 => Ok(File::H),
            _ => Err(format!(
                "{} is not a valid File value. File only has 8 variants.",
                i
            )),
        }
    }
}

impl TryFrom<char> for File {
    type Error = String;

    fn try_from(c: char) -> Result<Self, Self::Error> {
        match c {
            'A' | 'a' => Ok(File::A),
            'B' | 'b' => Ok(File::B),
            'C' | 'c' => Ok(File::C),
            'D' | 'd' => Ok(File::D),
            'E' | 'e' => Ok(File::E),
            'F' | 'f' => Ok(File::F),
            'G' | 'g' => Ok(File::G),
            'H' | 'h' => Ok(File::H),
            _ => Err(format!("{} is not a valid File token", c)),
        }
    }
}
