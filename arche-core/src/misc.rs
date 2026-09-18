// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use std::fmt;
use std::ops::Not;

/// An evaluation, in centipawns.
///
/// Sixteen bits for the transposition table's sake: a narrower score makes a
/// smaller entry, so a table of a given size holds more positions and more of
/// it fits in cache. The range is ample. Mate is thirty thousand, and an
/// evaluation is bounded by the material on the board, a little over ten
/// thousand even with every pawn promoted to a queen.
pub type Score = i16;

/// One step of splitmix64. Small enough to run at compile time, which the
/// zobrist keys need, and well spread enough for the magic search, which does
/// not need unpredictable. Returns the value alongside the next state rather
/// than taking a `&mut`, so that draws chain.
pub const fn split_mix(state: u64) -> (u64, u64) {
    let state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31), state)
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
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

/// The square as a fen and a uci move write it, file then rank, which is
/// what `from_string` reads back.
impl fmt::Display for Coordinate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.file, self.rank)
    }
}

#[cfg(test)]
mod coordinate {
    use super::Coordinate;

    #[test]
    fn a_square_parses_and_a_dash_is_no_square() {
        assert!(Coordinate::from_string("e3").unwrap().is_some());
        assert!(Coordinate::from_string("a1").unwrap().is_some());
        assert!(Coordinate::from_string("h8").unwrap().is_some());
        assert!(Coordinate::from_string("-").unwrap().is_none());
    }

    #[test]
    fn print_round_trips() {
        for square in ["a1", "e4", "h8"] {
            let parsed = Coordinate::from_string(square).unwrap().unwrap();
            assert_eq!(parsed.to_string(), square);
        }
    }

    #[test]
    fn anything_that_is_no_square_at_all_is_refused() {
        assert!(Coordinate::from_string("").is_err());
        assert!(Coordinate::from_string("e").is_err());
        assert!(Coordinate::from_string("ee").is_err());
        assert!(Coordinate::from_string("e0").is_err());
        assert!(Coordinate::from_string("e9").is_err());
        assert!(Coordinate::from_string("e33").is_err());
        assert!(Coordinate::from_string("é").is_err()); // two bytes, one char
    }
}

/// Who may still castle where, one bit a right. A right is given up for
/// good, by the king or the rook moving or the rook being taken, so a bit
/// only ever goes from set to clear, and `make_move` takes rights away by
/// masking the word.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct CastlePermissions(u8);

impl CastlePermissions {
    pub const WHITE_KING_SIDE: u8 = 1;
    pub const WHITE_QUEEN_SIDE: u8 = 1 << 1;
    pub const BLACK_KING_SIDE: u8 = 1 << 2;
    pub const BLACK_QUEEN_SIDE: u8 = 1 << 3;
    /// Every right, which is also the mask a word of rights fits in.
    pub const ALL: u8 = (1 << 4) - 1;
    pub const NONE: Self = Self(0);

    /// The rights as a word, for a mask to take some away.
    #[inline]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Rights from a word, which has to fit the four.
    #[inline]
    pub const fn from_bits(bits: u8) -> Self {
        debug_assert!(bits & !Self::ALL == 0, "a right that is none of the four");
        Self(bits)
    }

    /// Whether any of `rights` is held.
    #[inline]
    pub const fn holds(self, rights: u8) -> bool {
        self.0 & rights != 0
    }

    /// These rights and `rights` too.
    pub const fn with(self, rights: u8) -> Self {
        Self(self.0 | rights)
    }

    pub fn from_fen(s: &str) -> Result<CastlePermissions, String> {
        let mut perms = Self::NONE;
        if s == "-" {
            return Ok(perms);
        }
        for c in s.chars() {
            perms = perms.with(match c {
                'K' => Self::WHITE_KING_SIDE,
                'Q' => Self::WHITE_QUEEN_SIDE,
                'k' => Self::BLACK_KING_SIDE,
                'q' => Self::BLACK_QUEEN_SIDE,
                _ => {
                    return Err(format!(
                        "Unexpected character {} in castle permissions token",
                        c
                    ));
                }
            });
        }
        Ok(perms)
    }

    /// The rights in the order a fen writes them, or a dash for none.
    pub fn as_fen(&self) -> String {
        let mut s = String::new();
        for (right, letter) in [
            (Self::WHITE_KING_SIDE, 'K'),
            (Self::WHITE_QUEEN_SIDE, 'Q'),
            (Self::BLACK_KING_SIDE, 'k'),
            (Self::BLACK_QUEEN_SIDE, 'q'),
        ] {
            if self.holds(right) {
                s.push(letter);
            }
        }
        if s.is_empty() {
            s.push('-');
        }
        s
    }
}

#[cfg(test)]
mod castle_permissions {
    use super::CastlePermissions;

    #[test]
    fn every_set_of_rights_comes_back_as_it_went_in() {
        for rights in ["KQkq", "-", "Kq"] {
            assert_eq!(
                CastlePermissions::from_fen(rights).unwrap().as_fen(),
                rights
            );
        }
    }

    #[test]
    fn a_letter_that_is_no_right_is_refused() {
        assert!(CastlePermissions::from_fen("ksd").is_err());
    }
}

/// The index of a square, counting a1 as zero. Ranks are one based, the way
/// a fen and the board display write them.
pub const fn coordinate_to_index(rank: u8, file: File) -> u8 {
    debug_assert!(matches!(rank, 1..=8));
    ((rank - 1) * 8) + (file) as u8
}

/// The same square in the mailbox indexing, which offsets by a row and a column
/// of sentinels. See `BaseConversions`.
pub const fn coordinate_to_large_index(rank: u8, file: File) -> u8 {
    debug_assert!(matches!(rank, 1..=8));
    ((rank - 1) * 10) + (file) as u8 + 11
}

pub fn index_to_coordinate(index: u8) -> (u8, File) {
    let rank = ((index) / 8) + 1;
    let file = File::try_from(index % 8).unwrap();
    (rank, file)
}

#[cfg(test)]
mod index_conversion {
    use super::coordinate_to_index;
    use super::index_to_coordinate;

    #[test]
    fn every_index_survives_the_round_trip() {
        for index in 0u8..64 {
            let (rank, file) = index_to_coordinate(index);
            assert_eq!(index, coordinate_to_index(rank, file));
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
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

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Piece {
    Pawn,
    Knight,
    Bishop,
    Rook,
    Queen,
    King,
}

impl Piece {
    /// The pieces in discriminant order, for walking something indexed the
    /// way `pieces` and the tables are. The assertion below holds the order
    /// to the discriminants.
    pub const PIECES: [Piece; 6] = [
        Piece::Pawn,
        Piece::Knight,
        Piece::Bishop,
        Piece::Rook,
        Piece::Queen,
        Piece::King,
    ];
}

// the material, phase, piece square, zobrist and mvv-lva tables all index by
// the discriminants and nothing else pins them, so reordering the enum fails
// here rather than by scoring a queen as a pawn
const _: () = assert!(
    Piece::Pawn as usize == 0
        && Piece::Knight as usize == 1
        && Piece::Bishop as usize == 2
        && Piece::Rook as usize == 3
        && Piece::Queen as usize == 4
        && Piece::King as usize == 5
);

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

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
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

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
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
