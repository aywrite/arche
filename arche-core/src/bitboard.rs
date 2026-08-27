use crate::misc::{File, coordinate_to_index};

pub trait BitBoard {
    fn set_bit(&mut self, index: u8);
    fn clear_bit(&mut self, index: u8);
    fn is_bit_set(&self, index: u8) -> bool;

    /// Print the board as a rank and file grid.
    ///
    /// One of three printers nothing calls: this one, `Board::attacked_print`
    /// and the `Display` on `BaseConversions`. They are kept to be reached for
    /// when the thing each prints comes out wrong, which is the only time
    /// anyone wants to look at a raw bitboard, an attack map or the mailbox.
    /// Deleting one because nothing calls it is deleting it for the reason it
    /// exists.
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
        // the eighth rank first, the way the board display and a diagram write
        // it, rather than in index order
        for rank in (1..=8).rev() {
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
