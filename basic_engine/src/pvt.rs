use crate::misc::Color;
use crate::misc::Piece;

fn mirror(array: &[isize; 64]) -> [isize; 64] {
    let mut mirrored: [isize; 64] = [0; 64];
    for (i, a) in array.rchunks_exact(8).flatten().enumerate() {
        mirrored[i] = *a;
    }
    mirrored
}

pub struct PieceValueTables {
    white_pawns: [isize; 64],
    black_pawns: [isize; 64],

    white_knights: [isize; 64],
    black_knights: [isize; 64],

    white_bishops: [isize; 64],
    black_bishops: [isize; 64],

    white_rooks: [isize; 64],
    black_rooks: [isize; 64],

    white_queens: [isize; 64],
    black_queens: [isize; 64],
}

impl PieceValueTables {
    #[inline]
    pub fn get_value(&self, index: usize, piece: Piece, color: Color) -> isize {
        match (piece, color) {
            (Piece::Pawn, Color::White) => self.white_pawns[index],
            (Piece::Knight, Color::White) => self.white_knights[index],
            (Piece::Bishop, Color::White) => self.white_bishops[index],
            (Piece::Rook, Color::White) => self.white_rooks[index],
            (Piece::Queen, Color::White) => self.white_queens[index],
            (Piece::Pawn, Color::Black) => self.black_pawns[index],
            (Piece::Knight, Color::Black) => self.black_knights[index],
            (Piece::Bishop, Color::Black) => self.black_bishops[index],
            (Piece::Rook, Color::Black) => self.black_rooks[index],
            (Piece::Queen, Color::Black) => self.black_queens[index],
            (Piece::King, _) => 0,
        }
    }

    pub fn new() -> Self {
        // From https://www.chessprogramming.org/Simplified_Evaluation_Function.
        //
        // These are written the way the page prints them, with the eighth rank
        // in the top row, so the first entry is a8 and the last is h1. The
        // board counts the other way, a1 being index zero, which is why black
        // takes the tables as written and white takes them mirrored.
        #[rustfmt::skip]
        let pawns = [
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
        let knights = [
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
        let bishops = [
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
        let rooks = [
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
        let queens = [
            -20,-10,-10, -5, -5,-10,-10,-20,
            -10,  0,  0,  0,  0,  0,  0,-10,
            -10,  0,  5,  5,  5,  5,  0,-10,
             -5,  0,  5,  5,  5,  5,  0, -5,
              0,  0,  5,  5,  5,  5,  0, -5,
            -10,  5,  5,  5,  5,  5,  0,-10,
            -10,  0,  5,  0,  0,  0,  0,-10,
            -20,-10,-10, -5, -5,-10,-10,-20
        ];
        //#[rustfmt::skip]
        //let kings = [
        //   -30,-40,-40,-50,-50,-40,-40,-30,
        //   -30,-40,-40,-50,-50,-40,-40,-30,
        //   -30,-40,-40,-50,-50,-40,-40,-30,
        //   -30,-40,-40,-50,-50,-40,-40,-30,
        //   -20,-30,-30,-40,-40,-30,-30,-20,
        //   -10,-20,-20,-20,-20,-20,-20,-10,
        //    20, 20,  0,  0,  0,  0, 20, 20,
        //    20, 30, 10,  0,  0, 10, 30, 20
        //];
        Self {
            white_pawns: mirror(&pawns),
            black_pawns: pawns,
            white_knights: mirror(&knights),
            black_knights: knights,
            white_bishops: mirror(&bishops),
            black_bishops: bishops,
            white_rooks: mirror(&rooks),
            black_rooks: rooks,
            white_queens: mirror(&queens),
            black_queens: queens,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::File;
    use crate::misc::coordinate_to_index;

    fn value(piece: Piece, color: Color, file: File, rank: u8) -> isize {
        let index = coordinate_to_index(rank, file) as usize;
        PieceValueTables::new().get_value(index, piece, color)
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
