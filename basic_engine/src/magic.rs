use crate::board::BASE_CONVERSIONS;
use crate::misc::BitBoard;
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

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
    straight_moves: Vec<Vec<u64>>,
    straight_bits: [u8; 64],
    diagonal: [u64; 64],
    diagonal_moves: Vec<Vec<u64>>,
    diagonal_bits: [u8; 64],
}

impl Magic {
    pub fn new() -> Self {
        let bm = BlockerMasks::new();
        let bb = BlockerBoards::new(&bm);
        let mb = MoveBoards::new(&bb);
        let mut straight_magic_idxs = Vec::new();
        let mut straight_moves_magic = Vec::new();

        let mut diagonal_magic_idxs = Vec::new();
        let mut diagonal_moves_magic = Vec::new();
        let mut rng: SmallRng = <SmallRng as SeedableRng>::seed_from_u64(102938423890384);

        for index in 0..64 {
            let blockers = &bb.straight[index];
            let move_boards = &mb.straight[index];
            let bits = bb.straight_bits[index];
            let (s_magic, s_result) = Magic::find_magic(&mut rng, blockers, move_boards, bits);

            straight_magic_idxs.push(s_magic);
            straight_moves_magic.push(s_result);

            let blockers = &bb.diagonal[index];
            let move_boards = &mb.diagonal[index];
            let bits = bb.diagonal_bits[index];
            let (d_magic, d_result) = Magic::find_magic(&mut rng, blockers, move_boards, bits);

            diagonal_magic_idxs.push(d_magic);
            diagonal_moves_magic.push(d_result);
        }

        Self {
            blocker_masks: bm,
            straight: straight_magic_idxs.try_into().unwrap(),
            straight_moves: straight_moves_magic,
            straight_bits: bb
                .straight_bits
                .iter()
                .map(|i| 64 - i)
                .collect::<Vec<u8>>()
                .try_into()
                .unwrap(),
            diagonal: diagonal_magic_idxs.try_into().unwrap(),
            diagonal_moves: diagonal_moves_magic,
            diagonal_bits: bb
                .diagonal_bits
                .iter()
                .map(|i| 64 - i)
                .collect::<Vec<u8>>()
                .try_into()
                .unwrap(),
        }
    }

    fn find_magic(
        rng: &mut SmallRng,
        blockers: &[u64],
        move_boards: &Vec<u64>,
        bits: u8,
    ) -> (u64, Vec<u64>) {
        let mut result = vec![0; 2usize.pow(bits as u32)];
        let shift = 64 - bits;
        'outer: loop {
            let magic_candidate: u64 = rng.gen::<u64>() & rng.gen::<u64>() & rng.gen::<u64>();
            for item in &mut result {
                *item = 0;
            }

            for (blocker, &move_b) in blockers.iter().zip(move_boards) {
                let magic_index = blocker.wrapping_mul(magic_candidate) >> shift;
                if result[magic_index as usize] == 0 {
                    result[magic_index as usize] = move_b;
                } else if result[magic_index as usize] != move_b {
                    continue 'outer;
                }
            }
            return (magic_candidate, result);
        }
    }

    pub fn get_straight_move(&self, square: u8, mask: u64) -> u64 {
        let blockers = mask & self.blocker_masks.straight[square as usize];
        let index = (blockers.wrapping_mul(self.straight[square as usize]))
            >> self.straight_bits[square as usize];
        self.straight_moves[square as usize][index as usize]
    }

    pub fn get_diagonal_move(&self, square: u8, mask: u64) -> u64 {
        let blockers = mask & self.blocker_masks.diagonal[square as usize];
        let index = (blockers.wrapping_mul(self.diagonal[square as usize]))
            >> self.diagonal_bits[square as usize];
        self.diagonal_moves[square as usize][index as usize]
    }
}

impl MoveBoards {
    fn new(bb: &BlockerBoards) -> Self {
        let mut straight_moves = Vec::with_capacity(64);
        for i in 0u8..64 {
            let mut v: Vec<u64> = Vec::new();
            for mask in &bb.straight[i as usize] {
                v.push(Self::gen_straight_moves(i, mask));
            }
            straight_moves.push(v);
        }

        let mut diagonal_moves = Vec::with_capacity(64);
        for i in 0u8..64 {
            let mut v: Vec<u64> = Vec::new();
            for mask in &bb.diagonal[i as usize] {
                v.push(Self::gen_diagonal_moves(i, mask));
            }
            diagonal_moves.push(v);
        }

        Self {
            straight: straight_moves,
            diagonal: diagonal_moves,
        }
    }

    fn gen_straight_moves(from: u8, blocker_board: &u64) -> u64 {
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

    fn gen_diagonal_moves(from: u8, blocker_board: &u64) -> u64 {
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
mod magic_test {
    use super::test;
    //use pretty_assertions::assert_eq;

    #[test]
    fn test_perft_starting() {
        test();
    }
}

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
