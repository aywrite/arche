use crate::bitboard::BitBoard;
use crate::board::BASE_CONVERSIONS;

// Mask for locations of possible blockers
// for a given slider movement type and board square
struct BlockerMasks {
    straight: [u64; 64], // rooks and queens
    diagonal: [u64; 64], // bishops and queens
}

struct BlockerBoards {
    straight: Vec<Vec<u64>>,
    diagonal: Vec<Vec<u64>>,
    straight_bits: [u8; 64],
    diagonal_bits: [u8; 64],
}

struct MoveBoards {
    straight: Vec<Vec<u64>>,
    diagonal: Vec<Vec<u64>>,
}

pub struct Magic {
    blocker_masks: BlockerMasks,
    straight: [u64; 64],
    straight_moves: Vec<u64>,
    straight_offsets: [u32; 64],
    straight_bits: [u8; 64],
    diagonal: [u64; 64],
    diagonal_moves: Vec<u64>,
    diagonal_offsets: [u32; 64],
    diagonal_bits: [u8; 64],
}

impl Magic {
    pub fn new() -> Self {
        let bm = BlockerMasks::new();
        let bb = BlockerBoards::new(&bm);
        let mb = MoveBoards::new(&bb);
        let mut straight_moves_magic: Vec<u64> = Vec::new();
        let mut diagonal_moves_magic: Vec<u64> = Vec::new();
        let mut straight_offsets = [0u32; 64];
        let mut diagonal_offsets = [0u32; 64];

        for index in 0..64 {
            straight_offsets[index] = straight_moves_magic.len() as u32;
            straight_moves_magic.extend(Magic::build_table(
                &bb.straight[index],
                &mb.straight[index],
                bb.straight_bits[index],
                STRAIGHT_MAGICS[index],
            ));
            diagonal_offsets[index] = diagonal_moves_magic.len() as u32;
            diagonal_moves_magic.extend(Magic::build_table(
                &bb.diagonal[index],
                &mb.diagonal[index],
                bb.diagonal_bits[index],
                DIAGONAL_MAGICS[index],
            ));
        }

        Self {
            blocker_masks: bm,
            straight: STRAIGHT_MAGICS,
            straight_moves: straight_moves_magic,
            straight_offsets,
            straight_bits: bb
                .straight_bits
                .iter()
                .map(|i| 64 - i)
                .collect::<Vec<u8>>()
                .try_into()
                .unwrap(),
            diagonal: DIAGONAL_MAGICS,
            diagonal_moves: diagonal_moves_magic,
            diagonal_offsets,
            diagonal_bits: bb
                .diagonal_bits
                .iter()
                .map(|i| 64 - i)
                .collect::<Vec<u8>>()
                .try_into()
                .unwrap(),
        }
    }

    fn build_table(blockers: &[u64], move_boards: &[u64], bits: u8, magic: u64) -> Vec<u64> {
        let mut result = vec![0u64; 1usize << bits];
        let shift = 64 - bits;
        for (blocker, &move_b) in blockers.iter().zip(move_boards) {
            let magic_index = (blocker.wrapping_mul(magic) >> shift) as usize;
            debug_assert!(result[magic_index] == 0 || result[magic_index] == move_b);
            result[magic_index] = move_b;
        }
        result
    }

    #[inline]
    pub fn get_straight_move(&self, square: u8, mask: u64) -> u64 {
        let blockers = mask & self.blocker_masks.straight[square as usize];
        let index = (blockers.wrapping_mul(self.straight[square as usize]))
            >> self.straight_bits[square as usize];
        self.straight_moves[self.straight_offsets[square as usize] as usize + index as usize]
    }

    #[inline]
    pub fn get_diagonal_move(&self, square: u8, mask: u64) -> u64 {
        let blockers = mask & self.blocker_masks.diagonal[square as usize];
        let index = (blockers.wrapping_mul(self.diagonal[square as usize]))
            >> self.diagonal_bits[square as usize];
        self.diagonal_moves[self.diagonal_offsets[square as usize] as usize + index as usize]
    }
}

impl MoveBoards {
    fn new(bb: &BlockerBoards) -> Self {
        let mut straight_moves = Vec::with_capacity(64);
        for i in 0u8..64 {
            let mut v: Vec<u64> = Vec::new();
            for &mask in &bb.straight[i as usize] {
                v.push(Self::gen_straight_moves(i, mask));
            }
            straight_moves.push(v);
        }

        let mut diagonal_moves = Vec::with_capacity(64);
        for i in 0u8..64 {
            let mut v: Vec<u64> = Vec::new();
            for &mask in &bb.diagonal[i as usize] {
                v.push(Self::gen_diagonal_moves(i, mask));
            }
            diagonal_moves.push(v);
        }

        Self {
            straight: straight_moves,
            diagonal: diagonal_moves,
        }
    }

    fn gen_straight_moves(from: u8, blocker_board: u64) -> u64 {
        let mut moves = 0u64;
        let directions = [10isize, -10, 1, -1];
        for i in directions {
            let mut j = 1;
            loop {
                let check_100_index =
                    BASE_CONVERSIONS.base_64_to_100[from as usize] as isize + (i * j);
                if BASE_CONVERSIONS.is_offboard(check_100_index as usize) {
                    break;
                };
                let to = BASE_CONVERSIONS.base_100_to_64[check_100_index as usize];
                if blocker_board.is_bit_set(to) {
                    moves.set_bit(to);
                    break;
                }
                moves.set_bit(to);
                j += 1;
            }
        }
        moves
    }

    fn gen_diagonal_moves(from: u8, blocker_board: u64) -> u64 {
        let mut moves = 0u64;
        let directions = [9isize, -9, 11, -11];
        for i in directions {
            let mut j = 1;
            loop {
                let check_100_index =
                    BASE_CONVERSIONS.base_64_to_100[from as usize] as isize + (i * j);
                if BASE_CONVERSIONS.is_offboard(check_100_index as usize) {
                    break;
                };
                let to = BASE_CONVERSIONS.base_100_to_64[check_100_index as usize];
                if blocker_board.is_bit_set(to) {
                    moves.set_bit(to);
                    break;
                }
                moves.set_bit(to);
                j += 1;
            }
        }
        moves
    }
}

impl BlockerBoards {
    fn new(bm: &BlockerMasks) -> Self {
        let mut straight_blockers = Vec::with_capacity(64);
        let mut straight_bits = Vec::with_capacity(64);
        for i in 0..64 {
            let mut v: Vec<u64> = Vec::new();
            for bits in 0..(1 << bm.straight[i].count_ones()) {
                v.push(Self::generate_blocker_board(bits as u64, bm.straight[i]));
            }
            straight_blockers.push(v);
            straight_bits.push(bm.straight[i].count_ones() as u8);
        }

        let mut diagonal_blockers = Vec::with_capacity(64);
        let mut diagonal_bits = Vec::with_capacity(64);
        for i in 0..64 {
            let mut v: Vec<u64> = Vec::new();
            for bits in 0..(1 << bm.diagonal[i].count_ones()) {
                v.push(Self::generate_blocker_board(bits as u64, bm.diagonal[i]));
            }
            diagonal_blockers.push(v);
            diagonal_bits.push(bm.diagonal[i].count_ones() as u8);
        }

        Self {
            straight: straight_blockers,
            diagonal: diagonal_blockers,
            straight_bits: straight_bits.try_into().unwrap(),
            diagonal_bits: diagonal_bits.try_into().unwrap(),
        }
    }

    fn generate_blocker_board(index: u64, mask: u64) -> u64 {
        let mut board = mask;
        let mut bit_index = 0u8;
        for i in 0u8..64 {
            if mask.is_bit_set(i) {
                if !index.is_bit_set(bit_index) {
                    board.clear_bit(i);
                }
                bit_index += 1;
            }
        }
        board
    }
}

impl BlockerMasks {
    fn new() -> Self {
        let mut blocker_masks = BlockerMasks {
            straight: [0; 64], // rooks and queens
            diagonal: [0; 64], // bishops and queens
        };
        for i in 0usize..64 {
            for j in 1..7 {
                let horizontal_index = (i / 8 * 8) + j;
                let vertical_index = (i % 8) + (j * 8);
                blocker_masks.straight[i].set_bit(horizontal_index as u8);
                blocker_masks.straight[i].set_bit(vertical_index as u8);
            }

            let directions = [9isize, -9, 11, -11];
            for k in directions {
                let mut j = 0;
                loop {
                    let check_100_index = BASE_CONVERSIONS.base_64_to_100[i] as isize + (k * j);
                    let check_index = BASE_CONVERSIONS.base_100_to_64[check_100_index as usize];
                    j += 1;
                    let check_100_index = BASE_CONVERSIONS.base_64_to_100[i] as isize + (k * j);
                    if BASE_CONVERSIONS.is_offboard(check_100_index as usize) {
                        break; // if the next one is offboard then break now before setting the bit
                        // since a piece on the edge in direction of movement can't block
                    };
                    blocker_masks.diagonal[i].set_bit(check_index);
                }
            }
            blocker_masks.diagonal[i].clear_bit(i as u8); // can't be blocked by self
            blocker_masks.straight[i].clear_bit(i as u8); // can't be blocked by self
        }
        blocker_masks
    }
}

#[rustfmt::skip]
pub const STRAIGHT_MAGICS: [u64; 64] = [
    0x0080052033400282, 0x0240009004402000, 0x03000c4010200100, 0x8200104020060008,
    0x820020020004d830, 0x018004000d120080, 0x0100408401000200, 0x2600010860804a04,
    0x8000800082204010, 0x0011004001002080, 0x00010054c0200100, 0x2001001900201000,
    0x2006000600595020, 0x0101001c000a0900, 0x001c004d108a0804, 0x0011000500028042,
    0x2410308004884000, 0x0204808020014002, 0x1220808020005000, 0x031012000a0060c2,
    0x0e88008018240081, 0x2002808024000200, 0x0001a40010020881, 0x0ca0220008840641,
    0x4000882080004001, 0x0088802100400100, 0x0180408200120021, 0x0c280080801a1000,
    0x0554008080050800, 0x0100540080800600, 0x0008080c00910210, 0x000400420008812c,
    0x8200400081800220, 0x5210012000400640, 0x0000803202004020, 0x4003041001000860,
    0x0a00080080804400, 0x2500a04028011004, 0x0500108204000809, 0x48028241020000a4,
    0x08c0018040288000, 0x001002200040c001, 0x2488200010008080, 0x0038100009010060,
    0x2418011100290004, 0x40a0820004008080, 0x8001000200050024, 0x00000c1048820011,
    0x0020800044611100, 0x2080288100400100, 0x800120030110c100, 0x2102401202600a00,
    0x0692050008009100, 0x1004010002004040, 0x0200225938100c00, 0x8000204484011200,
    0x2100410020800a51, 0x0010210840001181, 0x02c0404b20001101, 0x004810000844a101,
    0x1041004402100801, 0x0041002804001e85, 0x0a08c8020104b004, 0x0000002144059102,
];

#[rustfmt::skip]
pub const DIAGONAL_MAGICS: [u64; 64] = [
    0x0050050a08020022, 0x8010211504288002, 0x1804040282100000, 0x0282208204308812,
    0x81011140001024a0, 0x0400900421000008, 0x10c08290886001c6, 0x0421010042200410,
    0x0500100a10030204, 0x1802021002020040, 0x0001480208420050, 0x60a0480604408008,
    0x0004041420448344, 0x0000020202200008, 0x0300820802021080, 0x00022a0244124884,
    0x0005102048220808, 0x0002100810214200, 0x4802000400240102, 0x0004000124008000,
    0x001010060210200b, 0x0006000101018285, 0x8004000184011810, 0x8008802a0c440211,
    0x500c400030100901, 0x0010120e04040404, 0x0084020044006402, 0x4408080004202060,
    0x034f010040304001, 0x00010a0007080100, 0x0444010084090101, 0x8000870000640201,
    0x3010102604080801, 0x8808080204188600, 0x2b04042200841400, 0x00402020a0480080,
    0x0140058020020020, 0xa020008100020901, 0x9088080882811182, 0x00824140408b0400,
    0x2804122004001111, 0x2054011908483084, 0x00822018c8011000, 0x0800032124000803,
    0x0000401040910100, 0x002000b000800040, 0x0002a80212800420, 0x0202020246000100,
    0x4090a18820502180, 0x00208404220a4808, 0x0002020100c80000, 0x0008008041108800,
    0x6000801042020010, 0x8041481001160100, 0x2550e06801014080, 0x082830008a10480e,
    0x504200210110b008, 0x24212114008c0404, 0x0048080204ea0810, 0x082001108094040a,
    0x9020010812020200, 0x6000002004100094, 0x0001045044080080, 0x0d20022218010310,
];

#[cfg(test)]
mod magic_generation {
    use super::{BlockerBoards, BlockerMasks, DIAGONAL_MAGICS, MoveBoards, STRAIGHT_MAGICS};
    use crate::misc::split_mix;

    /// Three draws anded together, which leaves each bit set with probability
    /// one in eight. A magic needs few enough bits set that the multiply and
    /// shift lands every blocker configuration on its own index, so sparse
    /// candidates are worth far more attempts than uniform ones.
    fn sparse_candidate(state: &mut u64) -> u64 {
        let (a, next) = split_mix(*state);
        let (b, next) = split_mix(next);
        let (c, next) = split_mix(next);
        *state = next;
        a & b & c
    }

    /// The search that produced the committed magics. Kept here so they can be
    /// regenerated, and compiled with the tests so it cannot rot.
    fn find_magic(state: &mut u64, blockers: &[u64], move_boards: &[u64], bits: u8) -> u64 {
        let mut table = vec![0u64; 1usize << bits];
        let shift = 64 - bits;
        'outer: loop {
            let candidate = sparse_candidate(state);
            table.fill(0);
            for (blocker, &move_b) in blockers.iter().zip(move_boards) {
                let index = (blocker.wrapping_mul(candidate) >> shift) as usize;
                if table[index] == 0 {
                    table[index] = move_b;
                } else if table[index] != move_b {
                    continue 'outer;
                }
            }
            return candidate;
        }
    }

    /// True if this magic maps every blocker configuration for the square onto a
    /// collision free index. Two configurations may share an index only when they
    /// have the same attack set, which is harmless.
    fn is_valid(magic: u64, blockers: &[u64], move_boards: &[u64], bits: u8) -> bool {
        let mut table = vec![0u64; 1usize << bits];
        let shift = 64 - bits;
        for (blocker, &move_b) in blockers.iter().zip(move_boards) {
            let index = (blocker.wrapping_mul(magic) >> shift) as usize;
            if table[index] != 0 && table[index] != move_b {
                return false;
            }
            table[index] = move_b;
        }
        true
    }

    /// The committed constants have to be valid, not identical to whatever the
    /// search last happened to return. Many magics work for a given square.
    #[test]
    fn committed_magics_are_valid() {
        let bm = BlockerMasks::new();
        let bb = BlockerBoards::new(&bm);
        let mb = MoveBoards::new(&bb);
        for square in 0..64 {
            assert!(
                is_valid(
                    STRAIGHT_MAGICS[square],
                    &bb.straight[square],
                    &mb.straight[square],
                    bb.straight_bits[square]
                ),
                "straight magic for square {} does not work",
                square
            );
            assert!(
                is_valid(
                    DIAGONAL_MAGICS[square],
                    &bb.diagonal[square],
                    &mb.diagonal[square],
                    bb.diagonal_bits[square]
                ),
                "diagonal magic for square {} does not work",
                square
            );
        }
    }

    /// Prints a fresh set of constants to paste into this file. Ignored because
    /// it is only needed if the blocker masks or the table layout change, and
    /// because the search is slow enough unoptimised to be worth not running by
    /// accident: about fourteen seconds as invoked below, against under a
    /// second with `--release`.
    ///
    ///     cargo test -p basic_engine regenerate_magics -- --ignored --nocapture
    #[test]
    #[ignore = "prints replacement constants, see the doc comment"]
    fn regenerate_magics() {
        let bm = BlockerMasks::new();
        let bb = BlockerBoards::new(&bm);
        let mb = MoveBoards::new(&bb);
        let mut state: u64 = 102938423890384;

        // the searches are interleaved per square, the same order the original
        // code consumed the generator in, so this reproduces the committed
        // values
        let mut straight = Vec::with_capacity(64);
        let mut diagonal = Vec::with_capacity(64);
        for i in 0..64 {
            straight.push(find_magic(
                &mut state,
                &bb.straight[i],
                &mb.straight[i],
                bb.straight_bits[i],
            ));
            diagonal.push(find_magic(
                &mut state,
                &bb.diagonal[i],
                &mb.diagonal[i],
                bb.diagonal_bits[i],
            ));
        }

        for (name, magics) in [("STRAIGHT_MAGICS", straight), ("DIAGONAL_MAGICS", diagonal)] {
            println!("#[rustfmt::skip]");
            println!("pub const {}: [u64; 64] = [", name);
            for chunk in magics.chunks(4) {
                let line: Vec<String> = chunk.iter().map(|m| format!("0x{:016x}", m)).collect();
                println!("    {},", line.join(", "));
            }
            println!("];");
        }
    }
}
