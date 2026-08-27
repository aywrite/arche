//! Magic bitboards: a slider's moves in one multiply, shift and lookup.
//!
//! For a square, the blockers that can matter are a fixed handful of bits. A
//! magic is a multiplier that scatters those bits into the top of the word so
//! that a shift leaves each configuration on its own index, and the attack set
//! is read straight out of a table at that index. The multipliers are searched
//! for rather than derived, which is what the ignored test at the bottom of
//! this file does.

use crate::board::{BASE_CONVERSIONS, BaseConversions};

/// The squares a slider on `from` could be blocked on: its rays, less the last
/// square of each, since a piece there blocks nothing behind it, and less the
/// square the slider stands on.
///
/// Trimming the ends is what keeps the mask small, and the mask is what sizes
/// the table. A rook in a corner is blocked on twelve squares rather than
/// fourteen, and every bit dropped halves what its square needs.
const fn blocker_mask(mailbox: &BaseConversions, from: u8, directions: [isize; 4]) -> u64 {
    let mut mask = 0u64;
    let mut d = 0;
    while d < directions.len() {
        let step = directions[d];
        let mut square = from;
        // stop before the edge: a blocker on the last square of a ray has
        // nothing behind it to block
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
/// the rays outwards. Each ray runs until it meets a blocker, which it stops on
/// because it may capture there.
///
/// This is what the tables are built from, and it is also the answer a lookup
/// has to agree with. A free function of the position alone so that it can be
/// asked directly, rather than only being reachable while a table is filled in:
/// `the_tables_answer_what_a_ray_walk_would` compares the two.
///
/// The mailbox is handed in rather than read from `BASE_CONVERSIONS` so that
/// the const build makes one and walks it a hundred thousand times, instead of
/// materialising a copy of it at every step of every ray.
pub(crate) const fn attacks_from(
    mailbox: &BaseConversions,
    from: u8,
    blockers: u64,
    directions: [isize; 4],
) -> u64 {
    let mut moves = 0u64;
    let mut d = 0;
    while d < directions.len() {
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
    let bits = mask.count_ones();
    let mut board = mask;
    let mut remaining = mask;
    let mut bit = 0;
    while bit < bits {
        let square = remaining.trailing_zeros();
        remaining &= remaining - 1;
        if index & (1u64 << bit) == 0 {
            board &= !(1u64 << square);
        }
        bit += 1;
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

/// How wide each kind's attack table is: the sum over the squares of two to the
/// power of the bits in that square's blocker mask. Stated rather than counted
/// because an array has to be sized before it is filled; `new` asserts that the
/// masks fill it exactly, so a wrong number fails the build rather than the
/// engine.
const STRAIGHT_ATTACKS: usize = 102_400;
const DIAGONAL_ATTACKS: usize = 5_248;

/// One slider kind's lookup tables. Every square's attack sets sit end to end
/// in a single array, with `offsets` saying where each square's block starts,
/// so a probe is one indirection rather than two.
struct SliderTables<const ATTACKS: usize> {
    blocker_masks: [u64; 64],
    magics: [u64; 64],
    /// `64 - bits` for each square, so a probe shifts without subtracting
    /// first. A shift, not a count of bits, which is what the same number was
    /// called when it lived in two places.
    shifts: [u8; 64],
    offsets: [u32; 64],
    attacks: [u64; ATTACKS],
}

impl<const ATTACKS: usize> SliderTables<ATTACKS> {
    const fn new(directions: [isize; 4], magics: [u64; 64]) -> Self {
        let mut blocker_masks = [0u64; 64];
        let mut shifts = [0u8; 64];
        let mut offsets = [0u32; 64];
        let mut attacks = [0u64; ATTACKS];
        let mut filled = 0usize;
        // one mailbox for the whole build, walked by every ray below
        let mailbox = BASE_CONVERSIONS;

        let mut square = 0u8;
        while square < 64 {
            let i = square as usize;
            let mask = blocker_mask(&mailbox, square, directions);
            let bits = mask.count_ones();
            blocker_masks[i] = mask;
            shifts[i] = 64 - bits as u8;
            offsets[i] = filled as u32;

            let configurations = 1u64 << bits;
            let mut configuration = 0u64;
            while configuration < configurations {
                let blockers = blocker_configuration(mask, configuration);
                let index = (blockers.wrapping_mul(magics[i]) >> shifts[i]) as usize;
                let moves = attacks_from(&mailbox, square, blockers, directions);
                // two configurations may share an index only when they admit
                // the same moves, which is what makes the magic a valid one
                assert!(attacks[filled + index] == 0 || attacks[filled + index] == moves);
                attacks[filled + index] = moves;
                configuration += 1;
            }
            filled += configurations as usize;
            square += 1;
        }
        assert!(
            filled == ATTACKS,
            "the table is not the width the masks ask for"
        );

        Self {
            blocker_masks,
            magics,
            shifts,
            offsets,
            attacks,
        }
    }

    #[inline]
    fn attacks(&self, square: u8, occupied: u64) -> u64 {
        let i = square as usize;
        let blockers = occupied & self.blocker_masks[i];
        let index = blockers.wrapping_mul(self.magics[i]) >> self.shifts[i];
        self.attacks[self.offsets[i] as usize + index as usize]
    }
}

pub struct Magic {
    straight: SliderTables<STRAIGHT_ATTACKS>,
    diagonal: SliderTables<DIAGONAL_ATTACKS>,
}

impl Magic {
    pub const fn new() -> Self {
        Self {
            straight: SliderTables::new(BaseConversions::STRAIGHT_STEPS, STRAIGHT_MAGICS),
            diagonal: SliderTables::new(BaseConversions::DIAGONAL_STEPS, DIAGONAL_MAGICS),
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
    use super::{
        DIAGONAL_MAGICS, STRAIGHT_MAGICS, attacks_from, blocker_configurations, blocker_mask,
    };
    use crate::board::{BASE_CONVERSIONS, BaseConversions, MAGIC};
    use crate::misc::split_mix;

    const STRAIGHT: [isize; 4] = BaseConversions::STRAIGHT_STEPS;
    const DIAGONAL: [isize; 4] = BaseConversions::DIAGONAL_STEPS;

    /// Everything a magic for one square has to map: each blocker
    /// configuration, the attacks it admits, and how wide the index is.
    struct Cases {
        blockers: Vec<u64>,
        attacks: Vec<u64>,
        bits: u8,
    }

    fn cases(square: u8, directions: [isize; 4]) -> Cases {
        let mask = blocker_mask(&BASE_CONVERSIONS, square, directions);
        let blockers = blocker_configurations(mask);
        let attacks = blockers
            .iter()
            .map(|&b| attacks_from(&BASE_CONVERSIONS, square, b, directions))
            .collect();
        Cases {
            blockers,
            attacks,
            bits: mask.count_ones() as u8,
        }
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

    /// True if this magic maps every blocker configuration for the square onto
    /// a collision free index. Two configurations may share an index only when
    /// they have the same attack set, which is harmless.
    fn is_valid(magic: u64, cases: &Cases) -> bool {
        let mut table = vec![0u64; 1usize << cases.bits];
        let shift = 64 - cases.bits;
        for (blocker, &attacks) in cases.blockers.iter().zip(&cases.attacks) {
            let index = (blocker.wrapping_mul(magic) >> shift) as usize;
            if table[index] != 0 && table[index] != attacks {
                return false;
            }
            table[index] = attacks;
        }
        true
    }

    /// The search that produced the committed magics. Kept here so they can be
    /// regenerated, and compiled with the tests so it cannot rot.
    fn find_magic(state: &mut u64, cases: &Cases) -> u64 {
        loop {
            let candidate = sparse_candidate(state);
            if is_valid(candidate, cases) {
                return candidate;
            }
        }
    }

    /// The committed constants have to be valid, not identical to whatever the
    /// search last happened to return. Many magics work for a given square.
    #[test]
    fn committed_magics_are_valid() {
        for square in 0..64u8 {
            let i = square as usize;
            assert!(
                is_valid(STRAIGHT_MAGICS[i], &cases(square, STRAIGHT)),
                "straight magic for square {} does not work",
                square
            );
            assert!(
                is_valid(DIAGONAL_MAGICS[i], &cases(square, DIAGONAL)),
                "diagonal magic for square {} does not work",
                square
            );
        }
    }

    /// A valid magic indexes without collisions, which says nothing about the
    /// table being filled in or read back the right way round. This walks the
    /// rays instead and asks the lookup to agree, over every square and every
    /// blocker configuration its mask admits.
    ///
    /// Exhaustive rather than sampled: a mask never has more than twelve bits,
    /// so the whole space is a hundred thousand or so lookups.
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
    /// because the search is slow enough unoptimised to be worth not running by
    /// accident: about fourteen seconds as invoked below, against under a
    /// second with `--release`.
    ///
    ///     cargo test -p arche-core regenerate_magics -- --ignored --nocapture
    #[test]
    #[ignore = "prints replacement constants, see the doc comment"]
    fn regenerate_magics() {
        let mut state: u64 = 102938423890384;

        // the searches are interleaved per square, the same order the original
        // code consumed the generator in, so this reproduces the committed
        // values
        let mut straight = Vec::with_capacity(64);
        let mut diagonal = Vec::with_capacity(64);
        for square in 0..64u8 {
            straight.push(find_magic(&mut state, &cases(square, STRAIGHT)));
            diagonal.push(find_magic(&mut state, &cases(square, DIAGONAL)));
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
