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
        // From https://www.chessprogramming.org/Simplified_Evaluation_Function
        #[rustfmt::skip]
        let pawns = [
            0,  0,  0,  0,  0,  0,  0,  0,
            50, 50, 50, 50, 50, 50, 50, 50,
            10, 10, 20, 30, 30, 20, 10, 10,
             5,  5, 10, 25, 25, 10,  5,  5,
             0,  0,  0, 20, 20,  0,  0,  0,
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
            white_pawns: pawns,
            black_pawns: mirror(&pawns),
            white_knights: knights,
            black_knights: mirror(&knights),
            white_bishops: bishops,
            black_bishops: mirror(&bishops),
            white_rooks: rooks,
            black_rooks: mirror(&rooks),
            white_queens: queens,
            black_queens: mirror(&queens),
        }
    }
}
