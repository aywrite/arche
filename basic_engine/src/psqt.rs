use crate::misc::Color;
use crate::misc::Piece;

/// Flip a table top to bottom, so that a table written with the eighth rank
/// first reads correctly for a board that indexes a1 as zero.
///
/// Written as a `while` rather than with iterators so that it can run at compile
/// time: the tables are then constants in the binary rather than something built
/// on startup.
const fn mirror(array: &[i16; 64]) -> [i16; 64] {
    let mut mirrored: [i16; 64] = [0; 64];
    let mut rank = 0;
    while rank < 8 {
        let mut file = 0;
        while file < 8 {
            mirrored[rank * 8 + file] = array[(7 - rank) * 8 + file];
            file += 1;
        }
        rank += 1;
    }
    mirrored
}

// From https://www.chessprogramming.org/Simplified_Evaluation_Function.
//
// These are written the way the page prints them, with the eighth rank in the
// top row, so the first entry is a8 and the last is h1. The board counts the
// other way, a1 being index zero, which is why black takes the tables as
// written and white takes them mirrored.
//
// The one change from the page is the fourth rank of the pawn table, where the
// squares outside the two centre files score 1 rather than 0.

#[rustfmt::skip]
const PAWNS: [i16; 64] = [
    0,  0,  0,  0,  0,  0,  0,  0,
    50, 50, 50, 50, 50, 50, 50, 50,
    10, 10, 20, 30, 30, 20, 10, 10,
    5,  5, 10, 25, 25, 10,  5,  5,
    1,  1,  1, 20, 20,  1,  1,  1,
    5, -5,-10,  0,  0,-10, -5,  5,
    5, 10, 10,-20,-20, 10, 10,  5,
    0,  0,  0,  0,  0,  0,  0,  0
];

#[rustfmt::skip]
const KNIGHTS: [i16; 64] = [
    -50,-40,-30,-30,-30,-30,-40,-50,
    -40,-20,  0,  0,  0,  0,-20,-40,
    -30,  0, 10, 15, 15, 10,  0,-30,
    -30,  5, 15, 20, 20, 15,  5,-30,
    -30,  0, 15, 20, 20, 15,  0,-30,
    -30,  5, 10, 15, 15, 10,  5,-30,
    -40,-20,  0,  5,  5,  0,-20,-40,
    -50,-40,-30,-30,-30,-30,-40,-50,
];

#[rustfmt::skip]
const BISHOPS: [i16; 64] = [
    -20,-10,-10,-10,-10,-10,-10,-20,
    -10,  0,  0,  0,  0,  0,  0,-10,
    -10,  0,  5, 10, 10,  5,  0,-10,
    -10,  5,  5, 10, 10,  5,  5,-10,
    -10,  0, 10, 10, 10, 10,  0,-10,
    -10, 10, 10, 10, 10, 10, 10,-10,
    -10,  5,  0,  0,  0,  0,  5,-10,
    -20,-10,-10,-10,-10,-10,-10,-20,
];

#[rustfmt::skip]
const ROOKS: [i16; 64] = [
    0,  0,  0,  0,  0,  0,  0,  0,
    5, 10, 10, 10, 10, 10, 10,  5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    -5,  0,  0,  0,  0,  0,  0, -5,
    0,  0,  0,  5,  5,  0,  0,  0
];

#[rustfmt::skip]
const QUEENS: [i16; 64] = [
    -20,-10,-10, -5, -5,-10,-10,-20,
    -10,  0,  0,  0,  0,  0,  0,-10,
    -10,  0,  5,  5,  5,  5,  0,-10,
    -5,  0,  5,  5,  5,  5,  0, -5,
    0,  0,  5,  5,  5,  5,  0, -5,
    -10,  5,  5,  5,  5,  5,  0,-10,
    -10,  0,  5,  0,  0,  0,  0,-10,
    -20,-10,-10, -5, -5,-10,-10,-20
];

/// The entries are `i16` rather than a machine word because every piece that is
/// set or cleared reads one, which is several times per move made or unmade, and
/// the twelve tables are then 1536 bytes rather than 6144 and stay in L1
/// alongside everything else the search is touching. The values run from -50 to
/// 50, so the narrower type costs nothing.
///
/// One array picked by arithmetic rather than a table per colour and piece
/// picked by a match. The match compiled to a jump table, and reading it was
/// the largest single source of mispredicted indirect branches in the search:
/// the piece being placed is whatever the position holds, so the branch
/// predictor has nothing to go on and missed it about half the time. The kings
/// carry a table of zeroes rather than a case of their own, which is what
/// leaves the pick with nothing to branch on.
pub struct PieceSquareTables {
    tables: [[i16; 64]; 12],
}

impl PieceSquareTables {
    #[inline]
    pub fn get_value(&self, index: usize, piece: Piece, color: Color) -> i16 {
        self.tables[Self::table_index(piece, color)][index]
    }

    /// The same shape `Zobrist` indexes its piece keys by: white takes the
    /// piece's own row, black the one six further on.
    #[inline]
    const fn table_index(piece: Piece, color: Color) -> usize {
        match color {
            Color::White => piece as usize,
            Color::Black => piece as usize + 6,
        }
    }

    /// Built at compile time, so there is nothing to construct on startup,
    /// nothing to synchronise on when reading it, and nothing standing between
    /// a proof and the code it is about. Worth keeping a `const`.
    ///
    /// Written in the order `table_index` reads it: the six pieces as `Piece`
    /// declares them for white, then the same six for black.
    pub const TABLES: PieceSquareTables = PieceSquareTables {
        tables: [
            mirror(&PAWNS),
            mirror(&KNIGHTS),
            mirror(&BISHOPS),
            mirror(&ROOKS),
            mirror(&QUEENS),
            [0; 64],
            PAWNS,
            KNIGHTS,
            BISHOPS,
            ROOKS,
            QUEENS,
            [0; 64],
        ],
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::File;
    use crate::misc::coordinate_to_index;

    fn value(piece: Piece, color: Color, file: File, rank: u8) -> i16 {
        let index = coordinate_to_index(rank, file) as usize;
        PieceSquareTables::TABLES.get_value(index, piece, color)
    }

    /// The tables are written with the eighth rank first and the board indexes
    /// a1 as zero, so it is easy to hand each colour the other one's table. Both
    /// colours are then wrong together, which leaves the two of them still
    /// mirroring each other and the evaluation still symmetric. Nothing but an
    /// assertion about which way up a table is will notice, so these name the
    /// squares rather than compare the colours.
    #[test]
    fn a_white_pawn_is_worth_more_the_closer_it_gets_to_promoting() {
        assert_eq!(value(Piece::Pawn, Color::White, File::E, 2), -20);
        assert_eq!(value(Piece::Pawn, Color::White, File::E, 4), 20);
        assert_eq!(value(Piece::Pawn, Color::White, File::E, 7), 50);
    }

    #[test]
    fn a_black_pawn_is_worth_more_the_closer_it_gets_to_promoting() {
        assert_eq!(value(Piece::Pawn, Color::Black, File::E, 7), -20);
        assert_eq!(value(Piece::Pawn, Color::Black, File::E, 5), 20);
        assert_eq!(value(Piece::Pawn, Color::Black, File::E, 2), 50);
    }

    #[test]
    fn a_rook_belongs_on_the_seventh_rank() {
        // the seventh rank row is 5 at the edges and 10 across the middle
        assert_eq!(value(Piece::Rook, Color::White, File::D, 7), 10);
        assert_eq!(value(Piece::Rook, Color::White, File::A, 7), 5);
        assert_eq!(value(Piece::Rook, Color::White, File::A, 2), -5);
        // and the square it lands on castling short is worth a little
        assert_eq!(value(Piece::Rook, Color::White, File::D, 1), 5);

        assert_eq!(value(Piece::Rook, Color::Black, File::D, 2), 10);
        assert_eq!(value(Piece::Rook, Color::Black, File::A, 7), -5);
        assert_eq!(value(Piece::Rook, Color::Black, File::D, 8), 5);
    }

    #[test]
    fn a_knight_is_worth_least_in_the_corners() {
        for (file, rank) in [(File::A, 1), (File::H, 1), (File::A, 8), (File::H, 8)] {
            assert_eq!(value(Piece::Knight, Color::White, file, rank), -50);
        }
        assert_eq!(value(Piece::Knight, Color::White, File::E, 4), 20);
    }

    /// The bishop and the queen are the two the reflection test below cannot
    /// tell apart: both tables are symmetric about the centre, so reading one
    /// into the other's row passes it and every other test here. The squares
    /// named are ones the two disagree on, which is what pins each table to
    /// its own row rather than to a shape they share.
    #[test]
    fn a_bishop_is_worth_most_on_the_long_diagonals() {
        assert_eq!(value(Piece::Bishop, Color::White, File::B, 3), 10);
        assert_eq!(value(Piece::Bishop, Color::White, File::C, 4), 10);
        assert_eq!(value(Piece::Bishop, Color::White, File::A, 1), -20);
        assert_eq!(value(Piece::Bishop, Color::Black, File::B, 6), 10);
    }

    #[test]
    fn a_queen_is_kept_off_the_edges_but_not_pushed_out() {
        // the same squares a bishop scores 10 on, which is what tells the two
        // tables apart
        assert_eq!(value(Piece::Queen, Color::White, File::B, 3), 5);
        assert_eq!(value(Piece::Queen, Color::White, File::C, 4), 5);
        assert_eq!(value(Piece::Queen, Color::White, File::A, 1), -20);
        assert_eq!(value(Piece::Queen, Color::Black, File::B, 6), 5);
    }

    /// Weaker than the tests above, since it holds whether or not the tables are
    /// the right way up, but it is what keeps one colour from being changed
    /// without the other.
    #[test]
    fn the_two_colours_are_reflections_of_each_other() {
        for piece in [
            Piece::Pawn,
            Piece::Knight,
            Piece::Bishop,
            Piece::Rook,
            Piece::Queen,
        ] {
            for rank in 1..=8 {
                for file in File::VARIANTS {
                    assert_eq!(
                        value(piece, Color::White, file, rank),
                        value(piece, Color::Black, file, 9 - rank),
                        "{:?} on {:?}{}",
                        piece,
                        file,
                        rank
                    );
                }
            }
        }
    }

    #[test]
    fn a_king_is_not_scored_by_square() {
        assert_eq!(value(Piece::King, Color::White, File::E, 1), 0);
        assert_eq!(value(Piece::King, Color::Black, File::E, 8), 0);
    }
}
