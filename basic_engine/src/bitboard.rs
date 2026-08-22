use crate::misc::{File, coordinate_to_index};

pub trait BitBoard {
    fn set_bit(&mut self, index: u8);
    fn clear_bit(&mut self, index: u8);
    fn is_bit_set(&self, index: u8) -> bool;

    /// Print the board as a rank and file grid. Nothing calls it: it is a
    /// debugging aid kept to be reached for when bitboard code misbehaves,
    /// and nothing else prints a raw bitboard.
    #[allow(dead_code)]
    fn debug_print(&self);
}

impl BitBoard for u64 {
    #[inline(always)]
    fn set_bit(&mut self, index: u8) {
        debug_assert!(index < 64);
        *self |= 1u64 << index;
    }
    #[inline(always)]
    fn clear_bit(&mut self, index: u8) {
        debug_assert!(index < 64);
        *self &= !(1u64 << index);
    }
    #[inline(always)]
    fn is_bit_set(&self, index: u8) -> bool {
        debug_assert!(index < 64);
        (self & (1u64 << index)) > 0
    }

    fn debug_print(&self) {
        println!("    a b c d e f g h");
        println!("  -----------------");
        for rank in 1..9 {
            print!("{} |", rank);
            for file in File::VARIANTS {
                if (self & (1u64 << coordinate_to_index(rank, file))) > 0 {
                    print!(" x");
                } else {
                    print!(" .");
                }
            }
            println!();
        }
    }
}
