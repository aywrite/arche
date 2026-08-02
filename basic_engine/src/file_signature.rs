//! Scoring the shape of a pawn structure from the files it occupies.
//!
//! Each side's pawns reduce to one bit per file, the two bytes together index a
//! table built at compile time, and the evaluation is then a single load. The
//! occupancy says nothing about how many pawns stand on a file, so doubled
//! pawns are invisible to this term.

/// Centipawns charged for a pawn on a file with neither neighbouring file occupied.
pub(crate) const ISOLATED_FILE: i16 = 12;
/// Centipawns charged for every pawn island after the first.
pub(crate) const EXTRA_ISLAND: i16 = 6;

const fn penalty(files: u8) -> i16 {
    if files == 0 {
        return 0;
    }
    let islands = (files & !(files << 1)).count_ones() as i16;
    let isolated = (files & !(files << 1 | files >> 1)).count_ones() as i16;
    (islands - 1) * EXTRA_ISLAND + isolated * ISOLATED_FILE
}

const fn build() -> [i16; 65536] {
    let mut scores = [0i16; 65536];
    let mut white = 0usize;
    while white < 256 {
        let mut black = 0usize;
        while black < 256 {
            scores[white | black << 8] = penalty(black as u8) - penalty(white as u8);
            black += 1;
        }
        white += 1;
    }
    scores
}

static SCORES: [i16; 65536] = build();

/// Positive is good for white.
#[inline]
pub fn score(white_files: u8, black_files: u8) -> i16 {
    SCORES[white_files as usize | (black_files as usize) << 8]
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// The files a side occupies are the whole of the input, and file topology
    /// does not know which colour it belongs to, so swapping the two bytes has
    /// to swap the sign.
    #[test]
    fn the_table_is_antisymmetric() {
        for white in 0..=255usize {
            for black in 0..=255usize {
                assert_eq!(
                    score(white as u8, black as u8),
                    -score(black as u8, white as u8),
                    "white {:08b} against black {:08b}",
                    white,
                    black
                );
            }
        }
    }

    #[test]
    fn a_full_board_of_files_is_even() {
        assert_eq!(score(0xFF, 0xFF), 0);
        assert_eq!(score(0, 0), 0);
    }

    /// A pawn on the a file has only one neighbouring file, and the shift that
    /// looks for the other one must not find the h file.
    #[test]
    fn an_edge_pawn_is_isolated_when_its_only_neighbour_is_empty() {
        assert_eq!(score(0b0000_0001, 0), -ISOLATED_FILE);
        assert_eq!(score(0b1000_0000, 0), -ISOLATED_FILE);
        assert_eq!(score(0b0000_0011, 0), 0);
        assert_eq!(score(0b1100_0000, 0), 0);
    }

    #[test]
    fn splitting_a_structure_costs_an_island() {
        assert_eq!(score(0b1111_1111, 0), 0);
        assert_eq!(score(0b1110_1111, 0), -EXTRA_ISLAND);
        assert_eq!(score(0b1110_1011, 0), -2 * EXTRA_ISLAND - ISOLATED_FILE);
    }

    #[test]
    fn a_side_is_only_charged_for_its_own_files() {
        assert_eq!(score(0b0000_0101, 0b0000_0101), 0);
        assert_eq!(score(0, 0b0000_0101), 2 * ISOLATED_FILE + EXTRA_ISLAND);
    }
}
