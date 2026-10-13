// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! Magic bitboards: a slider's moves in one multiply, shift and lookup.
//!
//! For a square, the blockers that can matter are a fixed handful of bits. A
//! magic is a multiplier that scatters those bits into the top of the word so
//! that a shift leaves each configuration on its own index, and the attack set
//! is read straight out of a table at that index. The multipliers are searched
//! for rather than derived, by `regenerate_magics`.

use crate::misc::{File, coordinate_to_index, coordinate_to_large_index};
use std::fmt;

/// The sixty four squares laid out inside a ten by ten grid whose border rows
/// and columns are sentinels, so that a step off the side of the board lands on
/// one instead of wrapping onto the far file.
///
/// One row of border is enough because every walk tests a square before
/// stepping again.
struct BaseConversions {
    base_64_to_100: [u8; 64],
    base_100_to_64: [u8; 100],
}

impl BaseConversions {
    const OFF_BOARD: u8 = 101;

    /// One step in each direction, in this indexing.
    const STRAIGHT_STEPS: [isize; 4] = [10, -10, 1, -1]; // rooks and queens
    const DIAGONAL_STEPS: [isize; 4] = [9, -9, 11, -11]; // bishops and queens

    const fn new() -> Self {
        let mut base = BaseConversions {
            base_100_to_64: [Self::OFF_BOARD; 100],
            base_64_to_100: [0u8; 64],
        };
        let mut rank = 1;
        while rank <= 8 {
            let mut f = 0;
            while f < File::VARIANTS.len() {
                let file = File::VARIANTS[f];
                let index = coordinate_to_large_index(rank, file);
                let index_64 = coordinate_to_index(rank, file) as usize;
                base.base_100_to_64[index as usize] = index_64 as u8;
                base.base_64_to_100[index_64] = index;
                f += 1;
            }
            rank += 1;
        }
        base
    }

    /// The square one `step` away, or `None` if the step leaves the board.
    #[inline]
    const fn step(&self, from: u8, step: isize) -> Option<u8> {
        let index_100 = self.base_64_to_100[from as usize] as isize + step;
        debug_assert!(
            matches!(index_100, 0..100),
            "one step cannot leave the mailbox"
        );
        let square = self.base_100_to_64[index_100 as usize];
        if square != Self::OFF_BOARD {
            Some(square)
        } else {
            None
        }
    }
}

/// Print the mailbox as a grid, the sentinels included, for when a walk comes
/// out wrong. Uncalled on purpose, see `BitBoard::debug_print`.
impl fmt::Display for BaseConversions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for rank in 0..10 {
            for file in 0..10 {
                let index = file + (rank * 10);
                write!(f, " {:0>3}", self.base_100_to_64[index as usize])?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

// a const rather than a static, so that what is built from it can be built at
// compile time too: a const initialiser may not read a static
const BASE_CONVERSIONS: BaseConversions = BaseConversions::new();
// the lint guards against a const that loops forever; this one is only long.
// Past twice the lint's length rustc prints a warning no `allow` silences, so
// the loops that build the tables make no call they can do without (`d < 4`
// rather than `directions.len()`): the interpreter runs each on every pass.
#[allow(long_running_const_eval)]
pub(crate) static MAGIC: Magic = Magic::new();

/// The squares a slider on `from` could be blocked on: its rays, less the last
/// square of each, since a piece there blocks nothing behind it, and less the
/// square the slider stands on.
///
/// Every bit dropped halves the slots the square fills in the table.
const fn blocker_mask(mailbox: &BaseConversions, from: u8, directions: [isize; 4]) -> u64 {
    let mut mask = 0u64;
    let mut d = 0;
    while d < 4 {
        let step = directions[d];
        let mut square = from;
        while let Some(next) = mailbox.step(square, step) {
            mask |= 1u64 << square;
            square = next;
        }
        d += 1;
    }
    mask &= !(1u64 << from);
    mask
}

/// Where a slider on `from` can move with `blockers` occupied, found by walking
/// the rays outwards, each ray including the blocker it stops on. What the
/// tables are built from, and what the test holds a lookup to.
///
/// The mailbox is a parameter rather than a read of `BASE_CONVERSIONS` so that
/// the const build walks one copy instead of materialising the const at every
/// step of every ray.
const fn attacks_from(
    mailbox: &BaseConversions,
    from: u8,
    blockers: u64,
    directions: [isize; 4],
) -> u64 {
    let mut moves = 0u64;
    let mut d = 0;
    while d < 4 {
        let step = directions[d];
        let mut square = from;
        while let Some(next) = mailbox.step(square, step) {
            moves |= 1u64 << next;
            if blockers & (1u64 << next) != 0 {
                break;
            }
            square = next;
        }
        d += 1;
    }
    moves
}

/// One subset of the mask's set bits: bit `n` of `index` says whether the
/// mask's `n`th set bit is occupied.
const fn blocker_configuration(mask: u64, index: u64) -> u64 {
    let mut board = 0u64;
    let mut remaining = mask;
    let mut index = index;
    while remaining != 0 {
        let rest = remaining & (remaining - 1);
        if index & 1 != 0 {
            board |= remaining ^ rest;
        }
        remaining = rest;
        index >>= 1;
    }
    board
}

/// Every subset of the mask's set bits, in index order.
#[cfg(test)]
fn blocker_configurations(mask: u64) -> Vec<u64> {
    (0..1u64 << mask.count_ones())
        .map(|index| blocker_configuration(mask, index))
        .collect()
}

/// How many bits every square's index takes, for each kind: the widest
/// mask's. One shift for all sixty four squares is a constant, so a probe
/// needs no per square shift.
const STRAIGHT_BITS: u32 = 12;
const DIAGONAL_BITS: u32 = 9;

/// How far each kind's table reaches: one past the highest slot any square
/// writes. Stated rather than counted because an array is sized before it is
/// filled; `new` asserts the reach, so a wrong number fails the build.
const STRAIGHT_ATTACKS: usize = 114_947;
const DIAGONAL_ATTACKS: usize = 5_423;

/// The arrays those tables live in, each the next power of two up. A probe
/// masks its index to the array's length, which is a bound llvm can prove, so
/// the probe carries no bounds check. The mask never changes an index, since
/// every index a probe computes is one `new` wrote, below the table's reach.
const STRAIGHT_LEN: usize = 131_072;
const DIAGONAL_LEN: usize = 8_192;

/// One slider kind's lookup tables, every square's attack sets in one array.
///
/// At a fixed width a square with a narrower mask than the widest leaves most
/// of its `1 << BITS` slots empty, so the squares' blocks overlap: each starts
/// at `offsets[square]`, chosen with the magics, so that its slots land on
/// slots no other square uses, or on one holding the same attack set.
struct SliderTables<const BITS: u32, const ATTACKS: usize, const LEN: usize> {
    blocker_masks: [u64; 64],
    magics: [u64; 64],
    offsets: [u32; 64],
    attacks: [u64; LEN],
}

impl<const BITS: u32, const ATTACKS: usize, const LEN: usize> SliderTables<BITS, ATTACKS, LEN> {
    const fn new(directions: [isize; 4], magics: [u64; 64], offsets: [u32; 64]) -> Self {
        assert!(
            LEN.is_power_of_two() && LEN >= ATTACKS,
            "the array must be a power of two at least the table's reach"
        );
        let mut blocker_masks = [0u64; 64];
        let mut attacks = [0u64; LEN];
        let mut reach = 0usize;
        let mailbox = BASE_CONVERSIONS;

        let mut square = 0u8;
        while square < 64 {
            let i = square as usize;
            let mask = blocker_mask(&mailbox, square, directions);
            assert!(mask.count_ones() <= BITS, "a mask is wider than the index");
            blocker_masks[i] = mask;

            let configurations = 1u64 << mask.count_ones();
            let mut configuration = 0u64;
            while configuration < configurations {
                let blockers = blocker_configuration(mask, configuration);
                let index = (blockers.wrapping_mul(magics[i]) >> (64 - BITS)) as usize;
                let slot = offsets[i] as usize + index;
                let moves = attacks_from(&mailbox, square, blockers, directions);
                // two configurations may share a slot only when they admit the
                // same moves, whether they come from one square or two (every
                // attack set has a bit, so an empty slot reads zero)
                assert!(slot < ATTACKS, "a slot is past the table's reach");
                assert!(attacks[slot] == 0 || attacks[slot] == moves);
                attacks[slot] = moves;
                if slot >= reach {
                    reach = slot + 1;
                }
                configuration += 1;
            }
            square += 1;
        }
        assert!(
            reach == ATTACKS,
            "the table does not reach as far as stated"
        );

        Self {
            blocker_masks,
            magics,
            offsets,
            attacks,
        }
    }

    #[inline]
    fn attacks(&self, square: u8, occupied: u64) -> u64 {
        let i = square as usize;
        let blockers = occupied & self.blocker_masks[i];
        let index = blockers.wrapping_mul(self.magics[i]) >> (64 - BITS);
        self.attacks[(self.offsets[i] as usize + index as usize) & (LEN - 1)]
    }
}

pub struct Magic {
    straight: SliderTables<STRAIGHT_BITS, STRAIGHT_ATTACKS, STRAIGHT_LEN>,
    diagonal: SliderTables<DIAGONAL_BITS, DIAGONAL_ATTACKS, DIAGONAL_LEN>,
}

impl Magic {
    pub const fn new() -> Self {
        Self {
            straight: SliderTables::new(
                BaseConversions::STRAIGHT_STEPS,
                STRAIGHT_MAGICS,
                STRAIGHT_OFFSETS,
            ),
            diagonal: SliderTables::new(
                BaseConversions::DIAGONAL_STEPS,
                DIAGONAL_MAGICS,
                DIAGONAL_OFFSETS,
            ),
        }
    }

    #[inline]
    pub fn get_straight_move(&self, square: u8, mask: u64) -> u64 {
        self.straight.attacks(square, mask)
    }

    #[inline]
    pub fn get_diagonal_move(&self, square: u8, mask: u64) -> u64 {
        self.diagonal.attacks(square, mask)
    }
}

#[rustfmt::skip]
pub const STRAIGHT_MAGICS: [u64; 64] = [
    0x0080052033400282, 0x0020001000080020, 0x8040080050004004, 0x0040040048004002,
    0x0020010020020404, 0x00400100020000c0, 0x0040010000400080, 0x0200010081402204,
    0x1400400020001048, 0x0280100400080090, 0x3000080402010008, 0x0100200400200201,
    0x0100200100200200, 0x0400202000800100, 0xc000202000400080, 0x0000400020800040,
    0x4040002000100020, 0x1090000802040009, 0x2004000802010088, 0x0004002002002144,
    0x0212001001001080, 0x2029002020008001, 0x0000801020004010, 0x1020802000400020,
    0x0200200040001040, 0x2000080010040010, 0x0800880040044010, 0x84000400200200a1,
    0x4000010020020020, 0x0040008020200100, 0x2000400040008001, 0x00c4400040002080,
    0x1c40001000200021, 0x0000080400100012, 0x4044000800404010, 0x8002000400202002,
    0x8000200100200200, 0x4000200080200100, 0x0200008000404001, 0x0080882000200040,
    0xc110204104000803, 0x0010080102000400, 0x0002c00409004010, 0x0042000204002020,
    0x2000a20001002020, 0x0000410000802020, 0x0020401080004001, 0x004000a002009001,
    0x0040012000100020, 0x8000080210040010, 0x6404010200080008, 0x0000040020020020,
    0x0000020020010020, 0x0040200080010020, 0x0210200040008020, 0x1080802000400020,
    0x0201401100220082, 0x0800081081002041, 0x0010050840802012, 0x0000401064200802,
    0x0400041020080102, 0x0000100081040802, 0x8000010400480482, 0x0421030460840042,
];

#[rustfmt::skip]
pub const STRAIGHT_OFFSETS: [u32; 64] = [
         0,  16384,  18432,  20480,  22560,  24616,  26920,   4096,
     28968,  76881,  77921,  78946,  79970,  80994,  82018,  31016,
     33320,  83042,  84066,  85090,  86274,  87298,  88322,  35368,
     37417,  89346,  90370,  91618,  92706,  93730,  94754,  39465,
     41769,  95778,  96802,  98082,  99106, 100130, 101154,  43817,
     45930, 102178, 103203, 104451, 105571, 106627, 107715,  48000,
     50432, 108739, 109827, 110851, 111875, 112899, 113923,  52496,
      8192,  54545,  58321,  62305,  66145,  69729,  73041,  12288,
];

#[rustfmt::skip]
pub const DIAGONAL_MAGICS: [u64; 64] = [
    0x0000820081000408, 0x0000208080100402, 0x0000401020214000, 0x4800804810102904,
    0x4020440200600020, 0x088041008050c800, 0x0020104202004012, 0x0004402080480040,
    0x3864804100401040, 0x000a008100100410, 0x0002804040082000, 0x4412808048101000,
    0x010040440200a000, 0x2006004100804020, 0x0804001100820040, 0x024400201042002a,
    0x2004080100208010, 0x0000200080801040, 0x0002000400100409, 0x0200200200801080,
    0x0e00240080840140, 0x0000120080808800, 0x2008086040208020, 0x0000040008208010,
    0x020202000100401a, 0x0000802402002020, 0x0012004582008008, 0x0040040000410220,
    0x8200840000802008, 0x2000208000404028, 0x1120200400208022, 0xc24040042808200e,
    0x01008200b0010020, 0x5000804108004010, 0x0420808084004200, 0x1000c04800008200,
    0x00042500102400c0, 0x8204008008208020, 0x0001000820201010, 0x0000082020800808,
    0x01c0208200800040, 0x100840404040c021, 0x2000804100800040, 0xa000008840200040,
    0x0800020041000010, 0x4804080020080041, 0x0200404080202004, 0x4000202040104002,
    0x0c40802101004040, 0x00c0104100402800, 0x40c0001010806000, 0x2054830408403007,
    0x0048020100202210, 0x2002820040102003, 0x8000420020401000, 0x2000208020400400,
    0x8a00801082010040, 0x0000001011010020, 0x0200044808208060, 0x448020a008084030,
    0x4000138001002020, 0x3058010040810010, 0x2100040100200810, 0x0001010010808004,
];

#[rustfmt::skip]
pub const DIAGONAL_OFFSETS: [u32; 64] = [
      3599,   3855,   3887,   3919,   3951,   3998,   4016,   3663,
      4078,   4110,   4142,   4174,   4206,   4253,   4271,   4317,
      4379,   4411,   2048,   2176,   2304,   2434,   4443,   4475,
      4507,   4539,   2570,      0,    512,   2698,   4571,   4603,
      4635,   4667,   2826,   1024,   1536,   2954,   4699,   4731,
      4763,   4795,   3082,   3210,   3343,   3471,   4827,   4859,
      4891,   4893,   4955,   4987,   5048,   5080,   5112,   5144,
      3665,   5176,   5234,   5266,   5327,   5359,   5391,   3791,
];

#[cfg(test)]
mod tests {
    use super::{
        BASE_CONVERSIONS, BaseConversions, DIAGONAL_BITS, MAGIC, STRAIGHT_BITS, attacks_from,
        blocker_configurations, blocker_mask,
    };
    use crate::misc::split_mix;

    const STRAIGHT: [isize; 4] = BaseConversions::STRAIGHT_STEPS;
    const DIAGONAL: [isize; 4] = BaseConversions::DIAGONAL_STEPS;

    /// Everything a magic for one square has to map: each blocker
    /// configuration and the attacks it admits.
    struct Cases {
        blockers: Vec<u64>,
        attacks: Vec<u64>,
    }

    fn cases(square: u8, directions: [isize; 4]) -> Cases {
        let mask = blocker_mask(&BASE_CONVERSIONS, square, directions);
        let blockers = blocker_configurations(mask);
        let attacks = blockers
            .iter()
            .map(|&b| attacks_from(&BASE_CONVERSIONS, square, b, directions))
            .collect();
        Cases { blockers, attacks }
    }

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

    /// The slots a magic fills at `bits`, in index order with the attacks
    /// each holds, or nothing if two configurations that admit different
    /// moves land on one slot. `scratch` is `1 << bits` zeros and is left so.
    fn slots(
        magic: u64,
        cases: &Cases,
        bits: u32,
        scratch: &mut [u64],
    ) -> Option<Vec<(usize, u64)>> {
        let mut used = Vec::new();
        let mut valid = true;
        for (blocker, &attacks) in cases.blockers.iter().zip(&cases.attacks) {
            let index = (blocker.wrapping_mul(magic) >> (64 - bits)) as usize;
            if scratch[index] == 0 {
                scratch[index] = attacks;
                used.push(index);
            } else if scratch[index] != attacks {
                valid = false;
                break;
            }
        }
        let mut filled: Vec<(usize, u64)> = used.iter().map(|&i| (i, scratch[i])).collect();
        used.iter().for_each(|&i| scratch[i] = 0);
        filled.sort_unstable();
        valid.then_some(filled)
    }

    /// How many candidates each square draws, and how many of the valid ones
    /// with the narrowest spread of slots it tries to place.
    const DRAWS: usize = 4_000_000;
    const PLACED: usize = 30;

    /// The search that produced the committed magics and offsets. Kept here
    /// so they can be regenerated, and compiled with the tests so it cannot
    /// rot.
    ///
    /// The squares go widest mask first. Each draws its candidates and keeps
    /// the valid ones whose slots spread least. It places each of those at its
    /// lowest offset that collides with nothing already placed (a slot holding
    /// the same attacks is no collision), and takes the one that ends the table
    /// soonest, the lower offset on a tie. Greedy, so the table is not as short
    /// as a longer search could make it, only short enough.
    fn place(state: &mut u64, directions: [isize; 4], bits: u32) -> ([u64; 64], [u32; 64], usize) {
        let all: Vec<Cases> = (0..64).map(|square| cases(square, directions)).collect();
        let mut order: Vec<usize> = (0..64).collect();
        order.sort_by_key(|&square| std::cmp::Reverse(all[square].blockers.len()));

        let mut scratch = vec![0u64; 1 << bits];
        let mut table = vec![0u64; 64 << bits];
        let (mut magics, mut offsets, mut reach) = ([0u64; 64], [0u32; 64], 0);
        for square in order {
            // each valid magic with how far its slots spread, the slots
            // themselves found again for the few that are kept
            let mut spreads = Vec::new();
            for _ in 0..DRAWS {
                let magic = sparse_candidate(state);
                if let Some(filled) = slots(magic, &all[square], bits, &mut scratch) {
                    spreads.push((filled[filled.len() - 1].0 - filled[0].0, magic));
                }
            }
            spreads.sort_by_key(|c| c.0);
            spreads.truncate(PLACED);
            let candidates: Vec<_> = spreads
                .into_iter()
                .map(|(spread, magic)| {
                    let filled = slots(magic, &all[square], bits, &mut scratch).unwrap();
                    (spread, magic, filled)
                })
                .collect();
            assert!(
                !candidates.is_empty(),
                "square {} drew no valid magic",
                square
            );

            let mut best: Option<(usize, usize, usize)> = None; // reach, offset, which
            for (which, (_, _, filled)) in candidates.iter().enumerate() {
                let offset = (0..)
                    .find(|&o| {
                        filled
                            .iter()
                            .all(|&(i, a)| table[o + i] == 0 || table[o + i] == a)
                    })
                    .unwrap();
                let ends = reach.max(offset + filled[filled.len() - 1].0 + 1);
                if best.is_none_or(|b| (ends, offset) < (b.0, b.1)) {
                    best = Some((ends, offset, which));
                }
            }
            let (ends, offset, which) = best.unwrap();
            for &(i, a) in &candidates[which].2 {
                table[offset + i] = a;
            }
            magics[square] = candidates[which].1;
            offsets[square] = offset as u32;
            reach = ends;
        }
        (magics, offsets, reach)
    }

    /// The assert in `SliderTables::new` already fails the build on a magic
    /// that collides, so what is left to check is that the table was filled
    /// in and read back the same way round: every square, every blocker
    /// configuration its mask admits, lookup against ray walk.
    #[test]
    fn the_tables_answer_what_a_ray_walk_would() {
        let magic = &MAGIC;
        for square in 0..64u8 {
            for blockers in
                blocker_configurations(blocker_mask(&BASE_CONVERSIONS, square, STRAIGHT))
            {
                assert_eq!(
                    magic.get_straight_move(square, blockers),
                    attacks_from(&BASE_CONVERSIONS, square, blockers, STRAIGHT),
                    "straight, square {} with blockers {:#018x}",
                    square,
                    blockers
                );
            }
            for blockers in
                blocker_configurations(blocker_mask(&BASE_CONVERSIONS, square, DIAGONAL))
            {
                assert_eq!(
                    magic.get_diagonal_move(square, blockers),
                    attacks_from(&BASE_CONVERSIONS, square, blockers, DIAGONAL),
                    "diagonal, square {} with blockers {:#018x}",
                    square,
                    blockers
                );
            }
        }
    }

    /// Prints a fresh set of constants to paste into this file. Ignored because
    /// it is only needed if the blocker masks or the table layout change, and
    /// it takes about a quarter of an hour.
    ///
    ///     cargo test --release -p arche-core regenerate_magics -- --ignored --nocapture
    #[test]
    #[ignore = "prints replacement constants, see the doc comment"]
    fn regenerate_magics() {
        let mut state: u64 = 102938423890384;
        let (straight, straight_offsets, straight_reach) =
            place(&mut state, STRAIGHT, STRAIGHT_BITS);
        let (diagonal, diagonal_offsets, diagonal_reach) =
            place(&mut state, DIAGONAL, DIAGONAL_BITS);

        println!("const STRAIGHT_ATTACKS: usize = {};", straight_reach);
        println!("const DIAGONAL_ATTACKS: usize = {};", diagonal_reach);
        for (name, magics, offsets) in [
            ("STRAIGHT", straight, straight_offsets),
            ("DIAGONAL", diagonal, diagonal_offsets),
        ] {
            println!("#[rustfmt::skip]");
            println!("pub const {}_MAGICS: [u64; 64] = [", name);
            for chunk in magics.chunks(4) {
                let line: Vec<String> = chunk.iter().map(|m| format!("0x{:016x}", m)).collect();
                println!("    {},", line.join(", "));
            }
            println!("];");
            println!("#[rustfmt::skip]");
            println!("pub const {}_OFFSETS: [u32; 64] = [", name);
            for chunk in offsets.chunks(8) {
                let line: Vec<String> = chunk.iter().map(|o| format!("{:>6}", o)).collect();
                println!("    {},", line.join(", "));
            }
            println!("];");
        }
    }
}
