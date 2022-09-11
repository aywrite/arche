use crate::misc::Piece;
use crate::Color;

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

pub struct Zorbrist {
    pieces: [[u64; 64]; 12],
    pub side: u64,
    //TODO castling:
    en_passant: [u64; 8],
}

impl Zorbrist {
    pub fn new() -> Self {
        let mut rng: SmallRng = <SmallRng as SeedableRng>::seed_from_u64(0xDEADBEEF);
        let mut pieces = [[0u64; 64]; 12];
        for b in &mut pieces {
            let mut array = [0u64; 64];
            rng.fill(&mut array);
            *b = array;
        }

        Self {
            pieces,
            side: rng.gen(),
            en_passant: rng.gen(),
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
}
