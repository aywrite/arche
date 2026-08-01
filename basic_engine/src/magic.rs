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
        let mut am = BlockerMasks {
            straight: [0; 64], // rooks and queens
            diagonal: [0; 64], // bishops and queens
        };
        for i in 0usize..64 {
            for j in 1..7 {
                let horizontal_index = (i / 8 * 8) + j;
                let vertical_index = (i % 8) + (j * 8);
                am.straight[i].set_bit(horizontal_index as u8);
                am.straight[i].set_bit(vertical_index as u8);
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
                    am.diagonal[i].set_bit(check_index);
                }
            }
            am.diagonal[i].clear_bit(i as u8); // can't be blocked by self
            am.straight[i].clear_bit(i as u8); // can't be blocked by self
        }
        am
    }
}

#[cfg(test)]
pub fn test() {
    let bm = BlockerMasks::new();
    let bb = BlockerBoards::new(&bm);
    let mv = MoveBoards::new(&bb);
    let magic = Magic::new();
    //let mut res = bb.straight[27].clone();
    //res.sort();
    //res.dedup();

    //for board in &bb.straight[27] {
    //    board.debug_print();
    //}
    //println!("length {}", bb.straight[27].len());
    //println!("unique {}", res.len()); // TODO turn this into a test

    println!("bm");
    bm.straight[0].debug_print();
    println!("bb");
    bb.straight[0][3].debug_print();
    println!("mb");
    mv.straight[0][3].debug_print();

    println!("bm");
    bm.diagonal[55].debug_print();
    println!("bb");
    bb.diagonal[55][3].debug_print();
    println!("mb");
    mv.diagonal[55][3].debug_print();

    let mask = 10000982834900933;
    let moves = magic.get_straight_move(27, mask);
    let moves_d = magic.get_diagonal_move(27, mask);
    println!("FINALLY");
    println!("MASK");
    mask.debug_print();
    println!("MOVES D");
    moves_d.debug_print();
    println!("MOVES");
    moves.debug_print();
}

#[rustfmt::skip]
pub const STRAIGHT_MAGICS: [u64; 64] = [
    0x1080002011400080, 0x0040200040029000, 0x1200081080220042, 0x01001000202c8900,
    0x0080040006080080, 0x0100022c00010018, 0x0200081886000104, 0x0880004100102880,
    0x4500802040008004, 0x0412004861810200, 0x0001004015002002, 0x0048800802801000,
    0x3100801400580081, 0x2841000204000900, 0x1011000a00240900, 0x0002000062009401,
    0x0004208001400a80, 0x92acc04000201001, 0x0020048024801000, 0x009e0200104008a1,
    0x0002020020841028, 0x0000808004001200, 0x182a040001028810, 0x0300820004410284,
    0x0061688180004000, 0x4002010200402081, 0x0102200100104100, 0x0000420200100820,
    0x0408040080080080, 0x0002040080800200, 0x8048010c000a9008, 0x0029800080086900,
    0x00028040008000a2, 0x5002804000802004, 0x2048c20082001029, 0x0008809800805000,
    0x2904080080800400, 0x0082000402001008, 0x0001010804003002, 0x000881024e0000a4,
    0x20192088400a8000, 0x0002406010004002, 0x0042002080120040, 0x400020f200420008,
    0x00c800a400808008, 0x2001000a24010008, 0x204a0008c40a0005, 0x000000c402820019,
    0xc080205080010100, 0x2020044000288880, 0x0000802000100080, 0x0828080084900080,
    0x0021018800241100, 0x0812000204008080, 0x1182a20110388400, 0x0000010084004200,
    0x0210610080024391, 0x4013908240002903, 0x000010408201200a, 0x4050990020045001,
    0x0201000224080031, 0x0201000822440005, 0x1000020090082104, 0x0800040221004192,
];

#[rustfmt::skip]
pub const DIAGONAL_MAGICS: [u64; 64] = [
    0x0140100421004892, 0x00041006520020a0, 0x208c0806004d1008, 0x00041c0080080802,
    0x0004042090002815, 0x0002020a22012240, 0x005242102420c800, 0x0106024042282000,
    0x0040864808080480, 0x4201041022851104, 0x0020410e02004000, 0x40000c040c886601,
    0x00003d10c0041440, 0x80002202100c0220, 0x1800058808080440, 0x4a000a1542080460,
    0x091004420a220c08, 0x2104001210042100, 0x0828004408002109, 0x0154000645020000,
    0x0404015822080840, 0x2000400808121001, 0x230c010205510820, 0x00020000c2020188,
    0x00a84e1040101200, 0x1c04841202500c00, 0xa0102a0004040400, 0x4800d80100820040,
    0x1105010000104004, 0x0200820005004208, 0x0028019018440480, 0x20030a0000c0e400,
    0x004c122000082004, 0x402a086008044102, 0x0881209000280020, 0x0610040c00080210,
    0x209002008028100c, 0x65124800c00a0045, 0x0010810208284200, 0x4004408204106900,
    0x0008261004e03004, 0x08c0c410286c0400, 0x0000701098091000, 0x0000091148002c00,
    0x180e810124000201, 0x1040100400200110, 0x012062120040220c, 0x1004010441000210,
    0x0144030431040108, 0x0208808410028010, 0x1000002215300000, 0x480a910020981400,
    0x0001006102440810, 0x2102400448008000, 0x002818505082003c, 0x8410242800404080,
    0x0300206208444000, 0x84c0010403010800, 0x0118000480482202, 0x0400500000460801,
    0x8030004020042400, 0x2212004820080480, 0x0040482004040240, 0x0290203089020060,
];

#[cfg(test)]
mod magic_generation {
    use super::{BlockerBoards, BlockerMasks, DIAGONAL_MAGICS, MoveBoards, STRAIGHT_MAGICS};
    use rand::rngs::SmallRng;
    use rand::{Rng, SeedableRng};

    /// The search that produced the committed magics. Kept here so they can be
    /// regenerated, and compiled with the tests so it cannot rot.
    fn find_magic(rng: &mut SmallRng, blockers: &[u64], move_boards: &[u64], bits: u8) -> u64 {
        let mut table = vec![0u64; 1usize << bits];
        let shift = 64 - bits;
        'outer: loop {
            let candidate: u64 = rng.random::<u64>() & rng.random::<u64>() & rng.random::<u64>();
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
    /// it takes about half a second and is only needed if the blocker masks or
    /// the table layout change.
    ///
    ///     cargo test -p basic_engine regenerate_magics -- --ignored --nocapture
    #[test]
    #[ignore = "prints replacement constants, see the doc comment"]
    fn regenerate_magics() {
        let bm = BlockerMasks::new();
        let bb = BlockerBoards::new(&bm);
        let mb = MoveBoards::new(&bb);
        let mut rng: SmallRng = SeedableRng::seed_from_u64(102938423890384);

        // the searches are interleaved per square, the same order the original
        // code consumed the rng in, so this reproduces the committed values
        let mut straight = Vec::with_capacity(64);
        let mut diagonal = Vec::with_capacity(64);
        for i in 0..64 {
            straight.push(find_magic(
                &mut rng,
                &bb.straight[i],
                &mb.straight[i],
                bb.straight_bits[i],
            ));
            diagonal.push(find_magic(
                &mut rng,
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

#[cfg(test)]
mod magic_test {
    use super::test;
    //use pretty_assertions::assert_eq;

    #[test]
    fn test_perft_starting() {
        test();
    }
}
