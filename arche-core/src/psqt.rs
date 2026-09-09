// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

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

/// Both halves of a tapered score in one word: the midgame value in the low
/// sixteen bits and the endgame value in the high ones.
///
/// Summing a boardful of these is one add rather than two, which is what lets
/// the accumulator carry two numbers for what it cost to carry one. A negative
/// midgame value borrows from the endgame half, which `eg_value` undoes by
/// adding the borrow back before it shifts. Negation needs no unpacking
/// either: negating the sum negates both halves, which is what lets the
/// accumulator subtract a black piece the way it subtracts a white one.
///
/// The halves are only independent while each stays inside an `i16`. A
/// boardful of these tables reaches about sixteen hundred either way, so there
/// is an order of magnitude in hand.
pub const fn pack(mg: i16, eg: i16) -> i32 {
    ((eg as i32) << 16) + mg as i32
}

/// The midgame half, which is the low bits as they stand.
#[inline]
pub const fn mg_value(score: i32) -> i32 {
    score as u16 as i16 as i32
}

/// The endgame half, taken after adding back what a negative midgame half
/// borrowed from it.
#[inline]
pub const fn eg_value(score: i32) -> i32 {
    ((score as u32).wrapping_add(0x8000) >> 16) as u16 as i16 as i32
}

/// One table's worth of the two phases packed together.
const fn packed(mg: [i16; 64], eg: [i16; 64]) -> [i32; 64] {
    let mut out: [i32; 64] = [0; 64];
    let mut i = 0;
    while i < 64 {
        out[i] = pack(mg[i], eg[i]);
        i += 1;
    }
    out
}

// The tables came from
// https://www.chessprogramming.org/Simplified_Evaluation_Function and a fit
// has since replaced every entry of them.
//
// Fitted 2026-09-09 on 1,814 of our own games at 10+0.1, whose 220,369 unique
// positions the quiet filter cut to 100,726, recorded by `arche terms` at
// 2c69bc5 and labelled by the result of the game each came from, split by
// fen-hash parity. Texel's loss at K = 1.2882, with a ridge of 1e-8 toward the
// shipped weights chosen on the held-out split, took the held-out mean squared
// error from 0.089859 to 0.084020, and rounding the weights to integers gave
// back 0.000010 of that. `eval::MATERIAL` was held at what it already was, so
// the fit moved the tables' shape and not the scale a pawn is a hundred on.
// `scripts/tune.py fit` produced these numbers and rebuilds them from the same
// rows.
//
// They are written the way the page printed them, with the eighth rank in the
// top row, so the first entry is a8 and the last is h1. The board counts the
// other way, a1 being index zero, which is why black takes the tables as
// written and white takes them mirrored.

#[rustfmt::skip]
const PAWNS: [i16; 64] = [
       0,   0,   0,   0,   0,   0,   0,   0,
      57,  49,  56,  54,  63,  48,  50,  61,
      31,  18,  29,  48,  35,  33,  36,  -2,
       9,  25,  15,  22,  25, -39, -19, -47,
       8, -10,  18,  -3,  18, -26, -20,  -6,
       1,  -5,  17,  -5,  15, -21, -12, -44,
      -7, -15,   1, -33,   6, -18,  19, -15,
       0,   0,   0,   0,   0,   0,   0,   0,
];

#[rustfmt::skip]
const KNIGHTS: [i16; 64] = [
     -51, -38, -24, -20, -29, -24, -37, -46,
     -27, -18,  -5,   4,  26,  -3, -54, -39,
     -35,  20,  34,  17,   7,  16,  -1, -31,
     -35,  18,   2,   9,   9,  34,   3, -20,
     -22,  26,  -2,  14,   2,  23,   2, -28,
     -29, -38,  -8,  -6,  28, -15, -17, -56,
     -40, -24,   0, -29,   0, -11, -21, -25,
     -51, -44, -54, -40, -53, -18, -35, -72,
];

#[rustfmt::skip]
const BISHOPS: [i16; 64] = [
     -12, -33, -17, -19,  12,  -6, -26, -21,
      -9,  -5,   4,   1,   1, -10,  13, -34,
     -15,  15,   7,  13,   4,  -3,  13,   0,
     -16,   0,  -6,  15,  44, -10,  -1, -24,
       0, -15,   1,   5, -10,  11,  12, -11,
      -1,  -1,  -1, -11,  19,  -6,   8, -10,
      -2,  -5,  -8, -18,  -9,  -6,   0,  29,
     -27,   3, -15, -11, -18, -36,  -1, -49,
];

#[rustfmt::skip]
const ROOKS: [i16; 64] = [
      10,  13,  10,  14,  16,   6,   4,   3,
      28,  15,  15,  25,  22,  24,   1,   7,
      14,  -2,  25,  15,  20,   7,  11,  10,
      13,  11,  -1,   0,  16,   8,  18,   9,
     -23,   3, -12,   7,   2, -19,  -8,   4,
     -26, -24,   0,   6,  -7, -10, -18,  -5,
     -41,   9, -23, -16, -36,  -8,  -4, -11,
     -32, -12,  -6, -13, -16, -16,  -4, -30,
];

#[rustfmt::skip]
const QUEENS: [i16; 64] = [
     -12, -13,  -3,   3,  -8,   2,  -6, -13,
      -8,  -1,   0,  10,  12,  -4,   9,   0,
       7,   0,   7,  16,  14,  20,  -1,   4,
       4,  28,  -1,   4,  18,  31,  21,  36,
     -17,   6,  -5,   2,  26,  11,  22,  15,
      -5, -14, -15,   9,  -7,   4, -23, -12,
       0, -25, -19, -12,   2, -12,  -1, -16,
     -15, -29, -14,  -8,  10, -32, -14, -19,
];

// The king is the piece the two phases disagree about most, and it takes a
// table for each: hidden behind its own pawns while there are pieces to hide
// from, and in the middle of the board once there are not. A single table
// cannot say both, which is why the king carried a table of zeroes until the
// score was tapered. The fit kept the two apart: g1 is the midgame table's
// best square and its worst are the middle of the board, and the ending's
// table is the other way round.

#[rustfmt::skip]
const KING: [i16; 64] = [
     -32, -43, -37, -49, -48, -34, -37, -29,
     -31, -31, -37, -51, -51, -36, -36, -28,
     -28, -41, -38, -51, -49, -37, -35, -31,
     -29, -45, -40, -52, -49, -35, -22, -27,
     -22, -32, -26, -40, -31, -26, -33, -17,
      -9, -21, -16, -14,  -5, -25, -22,  -9,
      20,   1,  -1, -14, -26, -12,  35,  10,
     -16,  12,  18, -29,  32, -31,  61,  47,
];

#[rustfmt::skip]
const KING_END: [i16; 64] = [
     -52, -40, -18, -20, -17, -16, -26, -46,
     -30,   2,   0, -26,   1,   3,  10, -13,
     -15,  -2,  32,  33,  31,  45,  11, -15,
     -20, -15,   4,  14,  40,  18,   9, -20,
     -28, -14,  16,  14,  15,  22,  -3, -26,
     -32,   1,  -6,  19,   5,   2,  -2, -21,
     -24, -32, -12, -17, -20,   2, -13, -35,
     -73,  -5, -51, -34, -42, -26, -45, -36,
];

/// The pawn is the other one. The midgame table pushes the two centre pawns
/// and holds the rest back to shelter a castled king, which is why a pawn
/// still at home on d2 scores less there than one on a2. With no pieces left
/// to shelter from none of that is true and the distance to promotion is most
/// of what is, so the endgame table pays more for a pawn on every rank it can
/// stand on. It was written as a ramp, one number a rank; the fit gave every
/// square its own and what is left of the ramp is in the ranks' totals.
#[rustfmt::skip]
const PAWNS_END: [i16; 64] = [
       0,   0,   0,   0,   0,   0,   0,   0,
      83,  59, 105,  47,  85,  65,  85,  90,
     115,  51,  50,  35,  37,  53,  54,  71,
      34,  29,  18,  22,  24, -16,  38,  27,
      16,  35,  17,   3, -15,  12,  43,   4,
      -3,  14,  32,  33,   9,  14,  18,  14,
      17,  26,  -2, -32, -18,  18,  14,  11,
       0,   0,   0,   0,   0,   0,   0,   0,
];

/// The entries are `i32` rather than a machine word because every piece that is
/// set or cleared reads one, which is several times per move made or unmade, and
/// the twelve tables are then 3072 bytes rather than 6144 and stay in L1
/// alongside everything else the search is touching. Each entry is a packed
/// pair rather than one value, so the width buys both phases rather than range:
/// see `pack`.
///
/// One array picked by arithmetic rather than a table per colour and piece
/// picked by a match. The match compiled to a jump table, and reading it was
/// the largest single source of mispredicted indirect branches in the search:
/// the piece being placed is whatever the position holds, so the branch
/// predictor has nothing to go on and missed it about half the time. The kings
/// take a row of their own rather than a case of their own, which is what
/// leaves the pick with nothing to branch on.
pub struct PieceSquareTables {
    tables: [[i32; 64]; 12],
}

impl PieceSquareTables {
    /// The packed pair for a piece on a square. Both phases at once, since a
    /// caller accumulating them wants one read and one add rather than two.
    #[inline]
    pub fn get_value(&self, index: usize, piece: Piece, color: Color) -> i32 {
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

    /// Built at compile time, so there is nothing to construct on startup and
    /// nothing to synchronise on when reading it.
    ///
    /// Written in the order `table_index` reads it: the six pieces as `Piece`
    /// declares them for white, then the same six for black. A piece whose two
    /// phases agree is handed the same table twice, which is every piece but
    /// the king and the pawn: a knight belongs in the middle of the board and
    /// a rook on the seventh whatever else is left on it.
    pub const TABLES: PieceSquareTables = PieceSquareTables {
        tables: [
            packed(mirror(&PAWNS), mirror(&PAWNS_END)),
            packed(mirror(&KNIGHTS), mirror(&KNIGHTS)),
            packed(mirror(&BISHOPS), mirror(&BISHOPS)),
            packed(mirror(&ROOKS), mirror(&ROOKS)),
            packed(mirror(&QUEENS), mirror(&QUEENS)),
            packed(mirror(&KING), mirror(&KING_END)),
            packed(PAWNS, PAWNS_END),
            packed(KNIGHTS, KNIGHTS),
            packed(BISHOPS, BISHOPS),
            packed(ROOKS, ROOKS),
            packed(QUEENS, QUEENS),
            packed(KING, KING_END),
        ],
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::File;
    use crate::misc::coordinate_to_index;

    fn packed_at(piece: Piece, color: Color, file: File, rank: u8) -> i32 {
        let index = coordinate_to_index(rank, file) as usize;
        PieceSquareTables::TABLES.get_value(index, piece, color)
    }

    /// The midgame half of a square, which is what the tables said before
    /// there were two of them and what most of these tests are still about.
    fn value(piece: Piece, color: Color, file: File, rank: u8) -> i32 {
        mg_value(packed_at(piece, color, file, rank))
    }

    fn eg(piece: Piece, color: Color, file: File, rank: u8) -> i32 {
        eg_value(packed_at(piece, color, file, rank))
    }

    /// A whole table the way white reads it, square by square, for the tests
    /// that are about where a piece's best and worst squares are rather than
    /// about what any one of them scores.
    fn white_table(piece: Piece, half: fn(i32) -> i32) -> Vec<(File, u8, i32)> {
        let mut squares = Vec::with_capacity(64);
        for rank in 1..=8 {
            for file in File::VARIANTS {
                squares.push((file, rank, half(packed_at(piece, Color::White, file, rank))));
            }
        }
        squares
    }

    /// Squares, in the order `white_table` walks them.
    type Squares = Vec<(File, u8)>;

    /// The squares a table scores worst and best, as sets, so a test can name
    /// them rather than name the numbers they hold.
    fn extremes(piece: Piece, half: fn(i32) -> i32) -> (Squares, Squares) {
        let squares = white_table(piece, half);
        let least = squares
            .iter()
            .map(|(_, _, v)| *v)
            .min()
            .expect("64 squares");
        let most = squares
            .iter()
            .map(|(_, _, v)| *v)
            .max()
            .expect("64 squares");
        let at = |want: i32| {
            squares
                .iter()
                .filter(|(_, _, v)| *v == want)
                .map(|(file, rank, _)| (*file, *rank))
                .collect()
        };
        (at(least), at(most))
    }

    const CORNERS: [(File, u8); 4] = [(File::A, 1), (File::H, 1), (File::A, 8), (File::H, 8)];

    /// The four squares in the middle, which is what the corners are held
    /// against where a table is about the centre.
    const MIDDLE: [(File, u8); 4] = [(File::D, 4), (File::E, 4), (File::D, 5), (File::E, 5)];

    fn on_the_edge(file: File, rank: u8) -> bool {
        rank == 1 || rank == 8 || file == File::A || file == File::H
    }

    /// a1 to h8, or h1 to a8. The file's own number is its distance from the
    /// a file, and the rank counts from one, which is why the two conditions
    /// are not symmetric.
    fn on_a_long_diagonal(file: File, rank: u8) -> bool {
        let file = file as u8;
        file + 1 == rank || file + rank == 8
    }

    /// A rank's entries added up across the files. A fit gives every square a
    /// number of its own, so a statement about where a piece belongs is one
    /// about a rank rather than about any one square of it.
    fn rank_total(piece: Piece, half: fn(i32) -> i32, rank: u8) -> i32 {
        File::VARIANTS
            .into_iter()
            .map(|file| half(packed_at(piece, Color::White, file, rank)))
            .sum()
    }

    /// The worst of a handful of named squares.
    fn worst_of(piece: Piece, squares: [(File, u8); 4]) -> i32 {
        squares
            .into_iter()
            .map(|(file, rank)| value(piece, Color::White, file, rank))
            .min()
            .expect("four squares")
    }

    /// The tables are written with the eighth rank first and the board indexes
    /// a1 as zero, so it is easy to hand each colour the other one's table. Both
    /// colours are then wrong together, which leaves the two of them still
    /// mirroring each other and the evaluation still symmetric. Nothing but an
    /// assertion about which way up a table is will notice, so these name the
    /// squares rather than compare the colours.
    ///
    /// They name the squares and not the numbers. What a square is worth is
    /// something a fit may move, and a pin on the number would fail every
    /// re-tune while saying nothing about whether the table still meant what
    /// it used to. What each table is for is the durable statement, and it is
    /// the one worth failing on.
    ///
    /// The first fit moved five of them anyway, which is the cost of naming a
    /// single square. Where a table's worst square used to be pinned as the
    /// four corners exactly, or its rank read file by file, the pin is now on
    /// the region or on the rank's total, which is the statement that was
    /// meant. Where the fit contradicted what a table was said to be for, as
    /// the queen's does about the edges, the pin is gone and the comment says
    /// what replaced it.
    ///
    /// White's squares alone: `the_two_colours_are_reflections_of_each_other`
    /// walks every piece on every square, so black's follow from white's and
    /// naming them here would only say the same thing twice.
    #[test]
    fn a_white_pawn_is_worth_more_the_closer_it_gets_to_promoting() {
        let e = |rank| value(Piece::Pawn, Color::White, File::E, rank);
        assert!(e(7) > e(4) && e(4) > e(2), "{}, {}, {}", e(2), e(4), e(7));
    }

    /// A pawn cannot stand on either back rank, so neither table says
    /// anything about those sixteen squares and both leave them at nothing.
    /// Sixteen of the five hundred and twelve table entries have no support
    /// in any corpus, and this is which ones.
    #[test]
    fn a_pawn_scores_nothing_on_a_rank_it_cannot_stand_on() {
        for file in File::VARIANTS {
            for rank in [1, 8] {
                assert_eq!(value(Piece::Pawn, Color::White, file, rank), 0);
                assert_eq!(eg(Piece::Pawn, Color::White, file, rank), 0);
            }
        }
    }

    /// By the rank's total across the files and not file by file. The fit
    /// gave every square a number of its own and it scores a rook better on
    /// the sixth than on the seventh on three of the eight files, so a
    /// file-by-file pin would be a pin on those three.
    #[test]
    fn a_rook_belongs_on_the_seventh_rank() {
        let seventh = rank_total(Piece::Rook, mg_value, 7);
        for rank in [2, 4, 6] {
            let below = rank_total(Piece::Rook, mg_value, rank);
            assert!(
                seventh > below,
                "the seventh totals {} and the {} totals {}",
                seventh,
                rank,
                below
            );
        }
        // and the centre files of the back rank beat its corners, which is
        // the open file the rook is put on before there is a seventh to take
        assert!(
            value(Piece::Rook, Color::White, File::D, 1)
                > value(Piece::Rook, Color::White, File::A, 1)
        );
    }

    /// The corners against the middle as regions. Naming the table's single
    /// worst square was a pin on a number wearing a square's name: the fit
    /// moved the worst of the four corners to h1 without moving what the
    /// table is for.
    #[test]
    fn a_knight_is_worth_less_in_the_corners_than_in_the_middle() {
        let middle = worst_of(Piece::Knight, MIDDLE);
        for (file, rank) in CORNERS {
            let corner = value(Piece::Knight, Color::White, file, rank);
            assert!(
                corner < middle,
                "{:?}{} is {} and the middle's worst is {}",
                file,
                rank,
                corner,
                middle
            );
        }
        // and nowhere on the edge is the best a knight can do
        let (_, most) = extremes(Piece::Knight, mg_value);
        for (file, rank) in &most {
            assert!(
                !on_the_edge(*file, *rank),
                "{:?}{} is the best square",
                file,
                rank
            );
        }
    }

    /// The bishop and the queen are the two the reflection test below cannot
    /// tell apart: reading one table into the other's row passes it and every
    /// other test here. Each has a shape of its own below, and the test after
    /// them is what says the two shapes belong to two tables.
    #[test]
    fn a_bishop_is_worth_most_on_a_long_diagonal_and_least_in_the_corners() {
        let (_, most) = extremes(Piece::Bishop, mg_value);
        for (file, rank) in &most {
            assert!(
                on_a_long_diagonal(*file, *rank),
                "{:?}{} is the best square",
                file,
                rank
            );
        }
        // the fianchetto squares, which are on a long diagonal and next to a
        // corner the table wants a bishop nowhere near
        for (file, corner) in [(File::B, File::A), (File::G, File::H)] {
            assert!(
                value(Piece::Bishop, Color::White, file, 2)
                    > value(Piece::Bishop, Color::White, corner, 1),
                "{:?}2 against {:?}1",
                file,
                corner
            );
        }
    }

    /// The corners against the middle, and nothing about the edges. The fit's
    /// best square for a queen is h5, which is on one, so what the table used
    /// to say about not being pushed out is no longer true of it.
    #[test]
    fn a_queen_is_worth_less_in_the_corners_than_in_the_middle() {
        let middle = worst_of(Piece::Queen, MIDDLE);
        for (file, rank) in CORNERS {
            let corner = value(Piece::Queen, Color::White, file, rank);
            assert!(
                corner < middle,
                "{:?}{} is {} and the middle's worst is {}",
                file,
                rank,
                corner,
                middle
            );
        }
    }

    /// The two are different tables, which nothing above says: each of the
    /// shapes named for them holds of the other's table as well, and the
    /// reflection test walks both colours of one piece rather than two pieces.
    #[test]
    fn a_bishop_and_a_queen_do_not_read_one_table() {
        assert_ne!(
            white_table(Piece::Bishop, mg_value),
            white_table(Piece::Queen, mg_value)
        );
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
            Piece::King,
        ] {
            for rank in 1..=8 {
                for file in File::VARIANTS {
                    // the whole pair at once, so a table tapered for one
                    // colour and not the other fails here
                    assert_eq!(
                        packed_at(piece, Color::White, file, rank),
                        packed_at(piece, Color::Black, file, 9 - rank),
                        "{:?} on {:?}{}",
                        piece,
                        file,
                        rank
                    );
                }
            }
        }
    }

    /// The two king tables are the reason the score is tapered at all, so
    /// these say which way round they are: behind its own pawns in the
    /// middlegame, in the middle of the board in the ending.
    #[test]
    fn a_king_hides_in_the_middlegame_and_comes_out_in_the_ending() {
        let castled = |half: fn(i32) -> i32| half(packed_at(Piece::King, Color::White, File::G, 1));
        let centre = |half: fn(i32) -> i32| half(packed_at(Piece::King, Color::White, File::E, 4));
        // the two halves disagree about the same two squares, which is what
        // the taper exists to say
        assert!(castled(mg_value) > centre(mg_value));
        assert!(centre(eg_value) > castled(eg_value));
        // and the ending's worst square is a corner, which is where a king
        // has least of the board in reach
        let (least, most) = extremes(Piece::King, eg_value);
        assert!(
            least.contains(&(File::A, 1)),
            "the worst squares are {:?}",
            least
        );
        for (file, rank) in &most {
            assert!(
                !on_the_edge(*file, *rank),
                "{:?}{} is the best square",
                file,
                rank
            );
        }
    }

    /// What the two pawn tables are for. The ending pays more for a pawn
    /// wherever it stands and more still the closer it is to promoting, which
    /// is the whole of the difference between them.
    ///
    /// By the rank's total rather than a number a rank: the endgame table was
    /// written as a ramp and the fit gave every square its own value. It rises
    /// from the fourth rank up; the fourth and the third it has two centipawns
    /// a square apart and the wrong way round, which is not an order worth
    /// asserting either way.
    #[test]
    fn the_ending_pays_more_for_a_pawn_the_closer_it_is_to_promoting() {
        for rank in 2..=7 {
            let ending = rank_total(Piece::Pawn, eg_value, rank);
            let middlegame = rank_total(Piece::Pawn, mg_value, rank);
            assert!(
                ending > middlegame,
                "rank {} totals {} in the ending and {} in the middlegame",
                rank,
                ending,
                middlegame
            );
        }
        for rank in 5..=7 {
            let nearer = rank_total(Piece::Pawn, eg_value, rank);
            let further = rank_total(Piece::Pawn, eg_value, rank - 1);
            assert!(nearer > further, "rank {} totals {}", rank, nearer);
        }
        // shelter in the middlegame, where the same rank is not one number:
        // a pawn on d2 is held back to cover a castled king and one on a2 is
        // not
        assert_ne!(
            value(Piece::Pawn, Color::White, File::D, 2),
            value(Piece::Pawn, Color::White, File::A, 2)
        );
    }

    /// A pair packs and unpacks to itself, negative halves included: a black
    /// piece is subtracted from the accumulator packed, never unpacked first.
    #[test]
    fn a_packed_pair_survives_being_packed() {
        for (mg, eg) in [(0, 0), (50, -50), (-50, 80), (-1, -1), (1600, -1600)] {
            let score = pack(mg, eg);
            assert_eq!(
                (mg_value(score), eg_value(score)),
                (i32::from(mg), i32::from(eg))
            );
            assert_eq!(
                (mg_value(-score), eg_value(-score)),
                (-i32::from(mg), -i32::from(eg))
            );
        }
    }

    /// Packed pairs add as pairs, which is what the accumulator relies on.
    #[test]
    fn packed_pairs_sum_a_half_at_a_time() {
        let total = pack(50, -50) + pack(-20, 80) + pack(-1, -1);
        assert_eq!((mg_value(total), eg_value(total)), (29, 29));
    }
}
