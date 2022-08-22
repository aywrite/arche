use std::mem;

#[derive(Debug)]
pub struct Coordinate {
    rank: u8,
    file: File,
}

impl Coordinate {
    pub fn from_string(s: &str) -> Result<Option<Self>, String> {
        if s == "-" {
            return Ok(None);
        }
        if s.len() != 2 {
            return Err(format!("Expected two characters, got {}", s.len()));
        }
        let mut chars = s.chars();
        let c = Coordinate {
            file: File::try_from(chars.next().unwrap())?,
            rank: u8::try_from(chars.next().unwrap()).map_err(|e| e.to_string())?,
        };
        Ok(Some(c))
    }
}

// Each color/side bit is true if that color is still allowed to castle on that side
#[derive(Debug)]
pub struct CastlePermissions {
    black_king_side: bool,
    black_queen_side: bool,
    white_king_side: bool,
    white_queen_side: bool,
}

impl CastlePermissions {
    pub fn new() -> Self {
        CastlePermissions {
            black_king_side: true,
            black_queen_side: true,
            white_king_side: true,
            white_queen_side: true,
        }
    }
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
                    ))
                }
            }
        }
        Ok(perms)
    }
    pub fn to_fen(&self) -> String {
        let mut s = String::new();
        if self.white_king_side {
            s.push('K')
        };
        if self.white_queen_side {
            s.push('Q')
        };
        if self.black_king_side {
            s.push('k')
        };
        if self.black_queen_side {
            s.push('q')
        };
        if s.is_empty() {
            s.push('-')
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
            CastlePermissions::from_fen(initial).unwrap().to_fen(),
            initial,
        );
    }

    #[test]
    fn round_trip_none() {
        let initial = "-";
        assert_eq!(
            CastlePermissions::from_fen(initial).unwrap().to_fen(),
            initial,
        );
    }

    #[test]
    fn round_trip_mixed() {
        let initial = "Kq";
        assert_eq!(
            CastlePermissions::from_fen(initial).unwrap().to_fen(),
            initial,
        );
    }

    #[test]
    fn invalid_chars() {
        let initial = "ksd";
        assert!(CastlePermissions::from_fen(initial).is_err());
    }
}

pub trait BitBoard {
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
    fn is_bit_set(&self, index: u64) -> bool;
}

impl BitBoard for u64 {
    fn set_bit(&mut self, index: u64) {
        // TODO how should this guard be implemented
        debug_assert!(index <= 64);
        // TODO precompute the set bit mask in an array
        _ = mem::replace(self, *self | (1u64 << index));
    }
    fn clear_bit(&mut self, index: u64) {
        // TODO how should this guard be implemented
        debug_assert!(index <= 64);
        // TODO precompute the clear bit mask in an array
        _ = mem::replace(self, *self ^ (1u64 << index));
    }
    fn is_bit_set(&self, index: u64) -> bool {
        return (self & (1u64 << index)) > 0;
    }
    fn count(&self) -> usize {
        self.count_ones() as usize
    }
    fn debug_print(&self) {
        println!("    a b c d e f g h");
        println!("  -----------------");
        for rank in 1..9 {
            print!("{} |", rank);
            for file in File::variants() {
                if (self & (1u64 << coordinate_to_index(rank, &file))) > 0 {
                    print!(" x");
                } else {
                    print!(" .");
                }
            }
            println!()
        }
    }
}

pub fn coordinate_to_index(rank: u64, file: &File) -> u64 {
    ((rank - 1) * 8) + (*file) as u64
}

pub fn coordinate_to_large_index(rank: u8, file: &File) -> u8 {
    ((rank - 1) * 10) + (*file) as u8 + 11
}

pub fn index_to_coordinate(index: u64) -> (u64, File) {
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
        fn round_trip(i in 1u64..=64) {
            let (rank, file) = index_to_coordinate(i);
            assert_eq!(i, coordinate_to_index(rank, &file));
        }
    }
}

pub enum Piece {
    Pawn,
    Knight,
    Bishop,
    Rook,
    Queen,
    King,
}

#[derive(Debug)]
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

#[derive(Copy, Clone, Debug)]
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
    pub fn variants() -> [File; 8] {
        // TODO make this static
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

    pub fn add(&self, value: u32) -> File {
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

impl From<File> for u64 {
    fn from(file: File) -> Self {
        return file as u64;
    }
}

impl TryFrom<u64> for File {
    type Error = String;

    fn try_from(i: u64) -> Result<Self, Self::Error> {
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
