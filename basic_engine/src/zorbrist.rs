use crate::Color;
use crate::misc::{CastlePermissions, Piece};

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

pub struct Zorbrist {
    pieces: [[u64; 64]; 12],
    pub side: u64,
    castling: [u64; 4],
    en_passant: [u64; 8],
}

impl Zorbrist {
    pub fn new() -> Self {
        let mut rng: SmallRng = <SmallRng as SeedableRng>::seed_from_u64(0x38655440d1b63d78);
        let mut pieces = [[0u64; 64]; 12];
        for b in &mut pieces {
            let mut array = [0u64; 64];
            rng.fill(&mut array);
            *b = array;
        }

        Self {
            pieces,
            side: rng.random(),
            castling: rng.random(),
            en_passant: rng.random(),
        }
    }

    pub fn get_piece_key(&self, index: u8, piece: Piece, color: Color) -> u64 {
        let piece_index = match color {
            Color::White => piece as usize,
            Color::Black => piece as usize + 6,
        };
        self.pieces[piece_index][index as usize]
    }

    pub fn en_passant_key(&self, index: u8) -> u64 {
        self.en_passant[(index % 8) as usize]
    }

    /// The combined key for a set of castle permissions. XORing the keys for
    /// the old and new permissions into the position key updates it in place.
    pub fn castle_key(&self, castle: CastlePermissions) -> u64 {
        let mut key = 0;
        if castle.white_king_side {
            key ^= self.castling[0];
        }
        if castle.white_queen_side {
            key ^= self.castling[1];
        }
        if castle.black_king_side {
            key ^= self.castling[2];
        }
        if castle.black_queen_side {
            key ^= self.castling[3];
        }
        key
    }
}

#[cfg(test)]
mod test_zorbrist {
    use super::Zorbrist;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_all_random_numbers_unique() {
        let z = Zorbrist::new();
        let mut all = z.pieces.iter().flatten().map(|&c| c).collect::<Vec<u64>>();
        all.push(z.side);
        all.extend(z.castling);
        all.extend(z.en_passant);
        let len = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(len, all.len());
    }
}
