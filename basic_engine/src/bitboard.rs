use crate::misc::{coordinate_to_index, File};
use smallvec::SmallVec;
use std::mem;

pub trait BitBoard {
    fn set_bit(&mut self, index: u8);
    fn clear_bit(&mut self, index: u8);
    fn count(&self) -> u8;
    fn debug_print(&self);
    fn is_bit_set(&self, index: u8) -> bool;
    fn get_set_bits(&self) -> SmallVec<[u8; 32]>;
    fn pop_bit(&mut self) -> Option<u8>;

    // TODO Remove these?
    #[inline(always)]
    fn set_bit_from_coordinate(&mut self, rank: u8, file: File) {
        self.set_bit(coordinate_to_index(rank, file));
    }
    #[inline(always)]
    fn clear_bit_from_coordinate(&mut self, rank: u8, file: File) {
        self.clear_bit(coordinate_to_index(rank, file));
    }
}

impl BitBoard for u64 {
    #[inline(always)]
    fn set_bit(&mut self, index: u8) {
        // TODO how should this guard be implemented
        debug_assert!(index <= 64);
        // TODO precompute the set bit mask in an array
        _ = mem::replace(self, *self | (1u64 << index));
    }
    #[inline(always)]
    fn clear_bit(&mut self, index: u8) {
        // TODO how should this guard be implemented
        debug_assert!(index <= 64);
        // TODO precompute the clear bit mask in an array
        _ = mem::replace(self, *self & !(1u64 << index));
    }
    #[inline(always)]
    fn is_bit_set(&self, index: u8) -> bool {
        (self & (1u64 << index)) > 0
    }
    #[inline(always)]
    fn count(&self) -> u8 {
        self.count_ones() as u8
    }

    #[inline(always)]
    fn get_set_bits(&self) -> SmallVec<[u8; 32]> {
        let mut v = SmallVec::<[u8; 32]>::new();
        let mut value = *self;
        while value != 0 {
            let index = value.trailing_zeros();
            value ^= 1 << index;
            v.push(index as u8);
        }
        v
    }

    fn pop_bit(&mut self) -> Option<u8> {
        if *self == 0 {
            return None;
        }
        let index = self.trailing_zeros();
        *self ^= 1 << index;
        Some(index as u8)
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
