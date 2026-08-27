// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use crate::Color;
use crate::misc::{CastlePermissions, Piece, split_mix};

pub struct Zobrist {
    pieces: [[u64; 64]; 12],
    pub side: u64,
    castling: [u64; 4],
    en_passant: [u64; 8],
}

impl Zobrist {
    /// The keys, built at compile time. They only have to be well spread and
    /// the same on both sides of a game, so there is nothing to be gained from
    /// drawing them at startup, and a `const` leaves nothing standing between a
    /// proof about the key and the code it is about. Worth keeping one.
    pub const TABLE: Zobrist = Self::build(0x3865_5440_d1b6_3d78);

    const fn build(seed: u64) -> Self {
        let mut state = seed;

        let mut pieces = [[0u64; 64]; 12];
        let mut piece = 0;
        while piece < 12 {
            let mut square = 0;
            while square < 64 {
                let (value, next) = split_mix(state);
                state = next;
                pieces[piece][square] = value;
                square += 1;
            }
            piece += 1;
        }

        let (side, next) = split_mix(state);
        state = next;

        let mut castling = [0u64; 4];
        let mut i = 0;
        while i < 4 {
            let (value, next) = split_mix(state);
            state = next;
            castling[i] = value;
            i += 1;
        }

        let mut en_passant = [0u64; 8];
        let mut i = 0;
        while i < 8 {
            let (value, next) = split_mix(state);
            state = next;
            en_passant[i] = value;
            i += 1;
        }

        Self {
            pieces,
            side,
            castling,
            en_passant,
        }
    }

    #[inline]
    pub fn get_piece_key(&self, index: u8, piece: Piece, color: Color) -> u64 {
        let piece_index = match color {
            Color::White => piece as usize,
            Color::Black => piece as usize + 6,
        };
        self.pieces[piece_index][index as usize]
    }

    #[inline]
    pub fn en_passant_key(&self, index: u8) -> u64 {
        self.en_passant[(index % 8) as usize]
    }

    /// The combined key for a set of castle permissions. XORing the keys for
    /// the old and new permissions into the position key updates it in place.
    #[inline]
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
mod keys {
    use super::Zobrist;
    use pretty_assertions::assert_eq;

    /// The keys are constants rather than draws now, so this guards the seed and
    /// the generator rather than a run of a prng: two equal keys would make two
    /// different positions share a hash.
    #[test]
    fn every_key_is_distinct() {
        let z = Zobrist::TABLE;
        let mut all = z.pieces.iter().flatten().copied().collect::<Vec<u64>>();
        all.push(z.side);
        all.extend(z.castling);
        all.extend(z.en_passant);
        let len = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(len, all.len());
    }
}
