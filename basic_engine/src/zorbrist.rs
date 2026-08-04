use crate::Color;
use crate::misc::{CastlePermissions, Piece};

/// One step of splitmix64, which is the usual way to turn a seed into a stream
/// of well spread numbers and is simple enough to run at compile time.
///
/// Returns the value alongside the next state rather than taking a `&mut`, so
/// that it needs nothing newer than const arithmetic.
const fn split_mix(state: u64) -> (u64, u64) {
    let state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31), state)
}

pub struct Zorbrist {
    pieces: [[u64; 64]; 12],
    pub side: u64,
    castling: [u64; 4],
    en_passant: [u64; 8],
}

impl Zorbrist {
    /// The keys, built at compile time. They only have to be well spread and
    /// the same on both sides of a game, so there is nothing to be gained from
    /// drawing them at startup.
    pub const TABLE: Zorbrist = Self::build(0x3865_5440_d1b6_3d78);

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
mod test_zorbrist {
    use super::Zorbrist;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_all_random_numbers_unique() {
        let z = Zorbrist::TABLE;
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
