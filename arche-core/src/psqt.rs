// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use crate::misc::Color;
use crate::misc::Piece;

/// Flip a table top to bottom, so that a table written with the eighth rank
/// first reads correctly for a board that indexes a1 as zero.
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
/// sixteen bits and the endgame value in the high ones, so summing a boardful
/// is one add rather than two. A negative midgame value borrows from the
/// endgame half, which `eg_value` undoes by adding the borrow back before it
/// shifts. Negating the sum negates both halves, so the accumulator subtracts
/// a black piece packed.
///
/// The halves are only independent while each stays inside an `i16`, which
/// `a_boardful_stays_inside_the_packed_halves` in `tune` holds a boardful to.
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

// These started as https://www.chessprogramming.org/Simplified_Evaluation_Function
// and were fitted from there; the ridge pulls toward the page's numbers rather
// than toward zero.
//
// They are written the way the page prints them, eighth rank first, so the
// first entry is a8 and the last h1. The board counts a1 as index zero, which
// is why black takes the tables as written and white takes them mirrored.
//
// Fitted 2026-09-10 by `scripts/tune.py` over 1,812 archived strength-run
// games at 10+0.1, 100,726 quiet rows extracted by `arche terms` at d17622f,
// K held at 1.3902, at a ridge of 3e-7. The sealed group scores 0.084551 at
// the weights these replace and 0.082797 at these, a paired difference of
// -0.001754 against a standard error of 0.000674 over 371 games, and
// 0.082778 read with the engine's own arithmetic. Each reading is of the
// rounded weights; the float fit was not saved. Commit 96bad35 is the
// report. Material was held at its shipped values because the delta margin
// in quiescence reads it, and moving it would change the search tree for a
// reason that is not the evaluation's accuracy.
//
// The tests below pin the shapes rather than the entries.

#[rustfmt::skip]
const PAWNS: [i16; 64] = [
      0,   0,   0,   0,   0,   0,   0,   0,
     50,  51,  51,  50,  50,  50,  50,  51,
     14,  11,  19,  29,  29,  21,  13,  11,
      5,   8,  11,  29,  25,   4,   3,  -1,
      5,   5,   8,  12,  17,  -7,  -2,   5,
      5,  -5,  -2,   0,   0,  -1,  -4,  -5,
      2,  -3,  12, -22, -17,   5,  23,   1,
      0,   0,   0,   0,   0,   0,   0,   0,
];

#[rustfmt::skip]
const KNIGHTS: [i16; 64] = [
    -50, -40, -30, -30, -30, -30, -40, -50,
    -40, -20,   0,   0,   1,   0, -20, -39,
    -31,  -1,  11,  16,  15,  10,   0, -30,
    -30,   8,  14,  21,  17,  20,   4, -29,
    -28,   0,  13,  21,  19,  15,   2, -28,
    -32,  -7,   4,  13,  21,   7,   1, -31,
    -40, -21,   0,  -1,   5,   1, -19, -38,
    -50, -34, -31, -31, -31, -30, -38, -50,
];

#[rustfmt::skip]
const BISHOPS: [i16; 64] = [
    -20, -10, -10, -10, -10, -10, -10, -20,
    -10,  -1,   0,   0,   0,   0,   0, -10,
    -10,   0,   5,  11,  11,   5,   0,  -9,
    -11,   8,   7,  12,  13,   5,   4, -11,
     -9,   1,   7,   9,   4,  11,   0, -11,
    -10,  13,  10,   5,  16,  11,   9,  -9,
    -11,  10,   3,  -2,   3,   0,   5,  -9,
    -20, -10,  -6, -10, -10, -19, -10, -21,
];

#[rustfmt::skip]
const ROOKS: [i16; 64] = [
     0,  0,  0,  0,  0,  0,  0,  1,
     7, 11, 12, 12, 11, 10, 10,  5,
    -4,  1,  3,  2,  0,  0,  0, -4,
    -4,  3,  1,  1,  2,  0,  0, -5,
    -8,  1,  0,  1,  0, -2,  0, -5,
    -7, -1,  0,  1, -2,  0,  0, -4,
    -7,  1,  0, -3, -2,  0,  0, -6,
    -9,  1,  5,  2,  3, -1,  0, -7,
];

#[rustfmt::skip]
const QUEENS: [i16; 64] = [
    -20, -10, -10,  -5,  -5, -10, -10, -20,
    -11,   0,   0,   0,   0,   0,   0,  -9,
     -9,   0,   5,   5,   5,   5,   0,  -9,
     -6,   1,   5,   5,   6,   6,   1,  -3,
     -3,   1,   5,   5,   6,   5,   2,  -5,
    -10,   3,   4,   7,   5,   6,  -6, -10,
     -9,  -1,   0,   2,  -1,   0,   1, -10,
    -20, -13,  -9,  -1,  -2, -11, -10, -20,
];

// The king is the piece the two phases disagree about most, and the page gives
// a table for each: hidden behind its own pawns while there are pieces to
// hide from, in the middle of the board once there are not.

#[rustfmt::skip]
const KING: [i16; 64] = [
    -30, -40, -40, -50, -50, -40, -40, -30,
    -30, -39, -40, -50, -50, -40, -40, -30,
    -30, -40, -40, -50, -50, -40, -40, -30,
    -30, -40, -40, -50, -50, -39, -39, -30,
    -20, -30, -30, -41, -40, -30, -30, -20,
    -10, -20, -22, -20, -20, -21, -20, -10,
     19,  20,   0,  -1,  -3,  -1,  21,  16,
     18,  26,  10,  -2,   3,   7,  43,  23,
];

#[rustfmt::skip]
const KING_END: [i16; 64] = [
    -50, -40, -30, -20, -20, -30, -39, -50,
    -30, -18, -10,   0,   0,  -9, -19, -29,
    -29,  -9,  20,  29,  30,  21,  -9, -28,
    -29,  -9,  27,  38,  41,  31,  -7, -28,
    -30, -10,  29,  36,  36,  30,  -9, -29,
    -30,  -9,  13,  27,  27,  18,  -8, -29,
    -30, -29,  -1,   0,  -5,   1, -24, -32,
    -51, -30, -30, -31, -31, -29, -28, -48,
];

/// The page prints a single pawn table, and it is a middlegame one: it pushes
/// the two centre pawns and holds the rest back to shelter a castled king.
/// With no pieces left to shelter from, only the distance to promotion is
/// true. This started as a ramp, one number a rank and the same on every
/// file; the fit kept the climb and let the files differ by a few centipawns
/// around it.
#[rustfmt::skip]
const PAWNS_END: [i16; 64] = [
     0,  0,  0,  0,  0,  0,  0,  0,
    80, 82, 83, 79, 80, 78, 80, 81,
    58, 50, 54, 48, 48, 50, 53, 52,
    32, 32, 28, 23, 29, 15, 31, 26,
    18, 20, 13,  7,  9, 12, 16, 15,
    -3,  7, 11, 15,  5, 12,  6,  4,
     3,  2,  1, -1,  0,  4,  9, -3,
     0,  0,  0,  0,  0,  0,  0,  0,
];

// The other four handed one array to both ends of the taper until d17622f.
// The fit moved each about two centipawns rms from its midgame twin, against
// nineteen for the pawn and forty three for the king, and the difference sits
// on the first three ranks rather than near the enemy king or on the seventh,
// where endgame piece placement is supposed to diverge. The games are the
// engine's own, and an engine that could not tell the two ends apart did not
// play the positions that would say so.
//
// Written out and not aliased, so editing one table does not move both.

#[rustfmt::skip]
const KNIGHTS_END: [i16; 64] = [
    -50, -40, -30, -30, -29, -30, -40, -50,
    -39, -20,   1,   0,   1,   0, -21, -40,
    -30,  -1,  12,  15,  15,  10,   0, -30,
    -30,   7,  14,  21,  18,  17,   5, -30,
    -30,  -1,  14,  22,  20,  18,   0, -29,
    -30,   2,   7,  13,  16,  11,   6, -30,
    -40, -20,  -1,  -2,   7,   0, -20, -40,
    -50, -39, -31, -30, -31, -30, -40, -50,
];

#[rustfmt::skip]
const BISHOPS_END: [i16; 64] = [
    -20, -11, -11, -11, -10, -10, -11, -20,
    -11,  -1,  -1,   0,   1,  -1,   1, -11,
    -10,   0,   4,  10,  10,   5,   1, -10,
    -10,   5,   3,  12,  12,   4,   4, -10,
     -9,  -1,   8,   7,   6,  10,   1, -10,
     -9,  11,   7,   7,  13,  10,  10, -10,
     -8,   6,  -2,  -2,   1,   0,   4, -10,
    -20,  -9, -14,  -9, -10, -14, -10, -21,
];

#[rustfmt::skip]
const ROOKS_END: [i16; 64] = [
     0,  2,  0,  1,  1,  1,  0,  0,
     9, 12, 12, 12, 11, 10, 11,  5,
    -2,  2,  3,  1,  1,  0,  0, -3,
    -3,  1,  0,  1,  0,  1,  1, -4,
    -8,  1, -1,  2,  0, -2,  0, -5,
    -5, -1,  1,  2,  0, -1, -4, -5,
    -8,  1, -3, -1, -2,  0, -2, -7,
    -6, -1, -2,  6,  3,  0,  0, -4,
];

#[rustfmt::skip]
const QUEENS_END: [i16; 64] = [
    -20, -10, -10,  -5,  -5, -10, -10, -20,
    -10,   0,   0,   0,   0,   0,   0, -10,
    -10,   0,   5,   5,   5,   5,   0,  -9,
     -5,   0,   5,   5,   6,   5,   0,  -5,
      0,   1,   5,   5,   6,   6,   1,  -5,
    -10,   5,   4,   5,   4,   6,  -1, -10,
    -10,  -1,   5,   0,   0,   0,   0, -10,
    -20, -10, -10,  -5,  -5, -10, -10, -20,
];

/// The entries are `i32` rather than a machine word so the tables stay small
/// in L1; each is a packed pair (see `pack`).
///
/// One array picked by arithmetic rather than a table per colour and piece
/// picked by a match. The match compiled to a jump table and was the largest
/// single source of mispredicted indirect branches in the search, missed
/// about half the time.
pub struct PieceSquareTables {
    tables: [[i32; 64]; 12],
}

impl PieceSquareTables {
    /// The packed pair for a piece on a square, both phases at once.
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

    /// Built at compile time, in the order `table_index` reads it: the six
    /// pieces as `Piece` declares them for white, then the same six for black.
    pub const TABLES: PieceSquareTables = PieceSquareTables {
        tables: [
            packed(mirror(&PAWNS), mirror(&PAWNS_END)),
            packed(mirror(&KNIGHTS), mirror(&KNIGHTS_END)),
            packed(mirror(&BISHOPS), mirror(&BISHOPS_END)),
            packed(mirror(&ROOKS), mirror(&ROOKS_END)),
            packed(mirror(&QUEENS), mirror(&QUEENS_END)),
            packed(mirror(&KING), mirror(&KING_END)),
            packed(PAWNS, PAWNS_END),
            packed(KNIGHTS, KNIGHTS_END),
            packed(BISHOPS, BISHOPS_END),
            packed(ROOKS, ROOKS_END),
            packed(QUEENS, QUEENS_END),
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

    /// The four squares a table scores lowest, in the order `white_table`
    /// walks them. Four squares rather than the whole tie at the minimum,
    /// which `extremes` gives: a fit breaks a hand written table's tie by a
    /// centipawn, and which four squares a table likes least is the durable
    /// statement.
    fn worst_four(piece: Piece, half: fn(i32) -> i32) -> Squares {
        let mut squares = white_table(piece, half);
        squares.sort_by_key(|(file, rank, value)| (*value, *rank, *file as usize));
        let mut worst: Squares = squares
            .iter()
            .take(4)
            .map(|(file, rank, _)| (*file, *rank))
            .collect();
        worst.sort_by_key(|(file, rank)| (*rank, *file as usize));
        worst
    }

    /// The sixteen squares off the two outer rings, which is what "in the
    /// middle" means for a piece that wants the board in reach.
    fn in_the_middle(file: File, rank: u8) -> bool {
        (3..=6).contains(&rank) && matches!(file, File::C | File::D | File::E | File::F)
    }

    /// One end of the taper: what to call it, and how to read it out of a
    /// packed pair.
    type Half = (&'static str, fn(i32) -> i32);

    /// Both ends, since every piece has a table at each of them. The name is
    /// for the failure message.
    const HALVES: [Half; 2] = [("midgame", mg_value), ("endgame", eg_value)];

    fn on_the_edge(file: File, rank: u8) -> bool {
        rank == 1 || rank == 8 || file == File::A || file == File::H
    }

    /// The tables are written with the eighth rank first and the board indexes
    /// a1 as zero, so it is easy to hand each colour the other one's table.
    /// Both colours are then wrong together and the evaluation still
    /// symmetric, so only an assertion about which way up a table is will
    /// notice. These name the squares and not the numbers: what a square is
    /// worth is something a fit may move, and a pin on the number would fail
    /// every re-tune while saying nothing about whether the table still meant
    /// what it did. White's squares alone, since
    /// `the_two_colours_are_reflections_of_each_other` makes black's follow.
    #[test]
    fn a_white_pawn_is_worth_more_the_closer_it_gets_to_promoting() {
        let e = |rank| value(Piece::Pawn, Color::White, File::E, rank);
        assert!(e(7) > e(4) && e(4) > e(2), "{}, {}, {}", e(2), e(4), e(7));
    }

    /// A pawn cannot stand on either back rank, so those sixteen entries have
    /// no support in any corpus and both tables leave them at nothing.
    #[test]
    fn a_pawn_scores_nothing_on_a_rank_it_cannot_stand_on() {
        for file in File::VARIANTS {
            for rank in [1, 8] {
                assert_eq!(value(Piece::Pawn, Color::White, file, rank), 0);
                assert_eq!(eg(Piece::Pawn, Color::White, file, rank), 0);
            }
        }
    }

    #[test]
    fn a_rook_belongs_on_the_seventh_rank() {
        for (name, half) in HALVES {
            let at = |file, rank| half(packed_at(Piece::Rook, Color::White, file, rank));
            for file in File::VARIANTS {
                let seventh = at(file, 7);
                for rank in [2, 4, 6] {
                    let below = at(file, rank);
                    assert!(
                        seventh > below,
                        "{} {:?}7 is {} and {:?}{} is {}",
                        name,
                        file,
                        seventh,
                        file,
                        rank,
                        below
                    );
                }
            }
            // and the centre files of the back rank beat its corners, which is
            // the open file the rook is put on before there is a seventh to take
            assert!(at(File::D, 1) > at(File::A, 1), "{}", name);
        }
    }

    #[test]
    fn a_knight_is_worth_least_in_the_corners_and_most_in_the_middle() {
        for (name, half) in HALVES {
            let worst = worst_four(Piece::Knight, half);
            assert_eq!(worst, CORNERS, "the worst {} squares are {:?}", name, worst);
            let (_, most) = extremes(Piece::Knight, half);
            for (file, rank) in &most {
                assert!(
                    in_the_middle(*file, *rank),
                    "{:?}{} is the best {} square",
                    file,
                    rank,
                    name
                );
            }
        }
    }

    /// The bishop and the queen are the two the reflection test below cannot
    /// tell apart: both tables are symmetric about the centre, so reading one
    /// into the other's row passes it and every other test here. Each has a
    /// shape of its own below, and the test after them is what says the two
    /// shapes belong to two tables.
    #[test]
    fn a_bishop_is_worth_least_in_the_corners_and_most_on_the_long_diagonals() {
        for (name, half) in HALVES {
            let at = |file, rank| half(packed_at(Piece::Bishop, Color::White, file, rank));
            let worst = worst_four(Piece::Bishop, half);
            // the midgame half holds this by a centipawn: its fourth lowest
            // entry is -20 and its fifth is -19, so of the shape pins here
            // this is the one a re-tune is likeliest to break without the
            // shape having moved
            assert_eq!(worst, CORNERS, "the worst {} squares are {:?}", name, worst);
            // the fianchetto squares, which are on a long diagonal and next to a
            // corner that is the table's worst
            for (file, corner) in [(File::B, File::A), (File::G, File::H)] {
                assert!(
                    at(file, 2) > at(corner, 1),
                    "{} {:?}2 against {:?}1",
                    name,
                    file,
                    corner
                );
            }
        }
    }

    #[test]
    fn a_queen_is_kept_off_the_edges_but_not_pushed_out() {
        for (name, half) in HALVES {
            let worst = worst_four(Piece::Queen, half);
            assert_eq!(worst, CORNERS, "the worst {} squares are {:?}", name, worst);
            let (_, most) = extremes(Piece::Queen, half);
            // not pushed out: nowhere on the edge is the best a queen can do,
            // and the corners are the only squares the table really refuses
            for (file, rank) in &most {
                assert!(
                    !on_the_edge(*file, *rank),
                    "{:?}{} is the best {} square",
                    file,
                    rank,
                    name
                );
            }
        }
    }

    /// The two are different tables, which nothing above says: each of the
    /// shapes named for them holds of the other's table as well, and the
    /// reflection test walks both colours of one piece rather than two pieces.
    #[test]
    fn a_bishop_and_a_queen_do_not_read_one_table() {
        for (name, half) in HALVES {
            assert_ne!(
                white_table(Piece::Bishop, half),
                white_table(Piece::Queen, half),
                "{}",
                name
            );
        }
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

    /// The endgame pawn table climbs toward promotion and the midgame one
    /// does not. The fit let the files differ by a few centipawns around the
    /// climb, so what is asserted is the rank's total across the files.
    #[test]
    fn a_pawn_is_worth_more_the_nearer_it_promotes_in_the_ending() {
        let rank_total = |half: fn(i32) -> i32, rank| {
            File::VARIANTS
                .iter()
                .map(|&file| half(packed_at(Piece::Pawn, Color::White, file, rank)))
                .sum::<i32>()
        };
        for rank in 3..=7 {
            assert!(
                rank_total(eg_value, rank) > rank_total(eg_value, rank - 1),
                "rank {} is {} and rank {} is {}",
                rank,
                rank_total(eg_value, rank),
                rank - 1,
                rank_total(eg_value, rank - 1)
            );
        }
        // and the ending pays for the advance more than the middlegame does
        for rank in 2..=7 {
            assert!(
                rank_total(eg_value, rank) > rank_total(mg_value, rank),
                "rank {}",
                rank
            );
        }
        // the middlegame is no climb at all: a pawn that has left the second
        // rank has left the shelter of a castled king
        assert!(rank_total(mg_value, 3) < rank_total(mg_value, 2));
        // and the same rank is not one number there: a pawn on d2 is held
        // back to cover a castled king and one on a2 is not
        assert_ne!(
            value(Piece::Pawn, Color::White, File::D, 2),
            value(Piece::Pawn, Color::White, File::A, 2)
        );
    }

    /// The four pieces given an endgame table have one that says something,
    /// however little: a run that left one of the four an exact copy of its
    /// midgame twin would mean the vector never reached the file rather than
    /// that the corpus had nothing to say.
    #[test]
    fn the_four_new_endgame_tables_no_longer_hold_their_midgame_numbers() {
        for piece in [Piece::Knight, Piece::Bishop, Piece::Rook, Piece::Queen] {
            let moved = (1..=8)
                .flat_map(|rank| File::VARIANTS.iter().map(move |&file| (file, rank)))
                .filter(|&(file, rank)| {
                    let packed = packed_at(piece, Color::White, file, rank);
                    mg_value(packed) != eg_value(packed)
                })
                .count();
            assert!(moved > 0, "{:?} still reads one table at both ends", piece);
        }
    }

    /// Each of the four is written out rather than aliased to its midgame
    /// twin. Nothing at run time can tell the copy from the alias, so this
    /// reads the source.
    #[test]
    fn each_new_endgame_table_is_written_out_rather_than_aliased() {
        let source = include_str!("psqt.rs");
        for table in ["KNIGHTS_END", "BISHOPS_END", "ROOKS_END", "QUEENS_END"] {
            let written = format!("const {}: [i16; 64] = [", table);
            assert!(source.contains(&written), "{} is not written out", table);
        }
    }

    /// Every piece reads its own two tables, in the order `table_index` asks
    /// for them.
    ///
    /// The shape tests mostly cannot see one piece handed another's table: a
    /// bishop's and a queen's pass each other's, and so do three of the four
    /// endgame tables. The bench pins would move, but a commit that re-tunes
    /// rewrites them anyway. Which array a row names is a question about the
    /// source, so this reads the source, the way the aliasing test does, and
    /// pins names rather than numbers; which numbers are in each array is the
    /// test below. Of the 28 ways to exchange two of the eight tables this
    /// fires on all 28, and on the four that exchange a piece's own two
    /// halves it is the only test in this file that does.
    #[test]
    fn every_piece_reads_its_own_two_tables() {
        const PIECES: [&str; 6] = ["PAWNS", "KNIGHTS", "BISHOPS", "ROOKS", "QUEENS", "KING"];
        // .gitattributes lets a windows checkout carry carriage returns, so
        // the line endings are taken out before anything looks for a newline
        let source = include_str!("psqt.rs").replace("\r\n", "\n");
        let written: Vec<&str> = source
            .split_once("tables: [\n")
            .expect("the table array")
            .1
            .split_once("\n        ],")
            .expect("the end of the table array")
            .0
            .lines()
            .map(str::trim)
            .collect();
        let mut wanted: Vec<String> = PIECES
            .iter()
            .map(|piece| format!("packed(mirror(&{piece}), mirror(&{piece}_END)),"))
            .collect();
        wanted.extend(
            PIECES
                .iter()
                .map(|piece| format!("packed({piece}, {piece}_END),")),
        );
        assert_eq!(written, wanted);
    }

    /// Each of the four keeps its own numbers, which the test above cannot
    /// say: exchanging two arrays' contents leaves every name where it was.
    ///
    /// The fit moved each endgame table about two centipawns from its midgame
    /// twin, so a piece's two halves are nearer each other than either is to
    /// any of the other ten tables. The closest pair is the bishop and the
    /// queen: the bishop's halves are 81 apart summed over the sixty four
    /// squares, and the nearer of the two to any other piece's table is 172
    /// from it. A checksum would catch more and is not wanted, for the reason
    /// the shape tests give, and because a re-tune that carries an exchange
    /// would recompute it and bless it. This fires on 24 of the 28 exchanges;
    /// the four it cannot see are a piece's own two halves.
    #[test]
    fn each_of_the_four_stays_nearest_its_own_midgame_twin() {
        let entries = |piece, half: fn(i32) -> i32| -> Vec<i32> {
            white_table(piece, half)
                .into_iter()
                .map(|(_, _, value)| value)
                .collect()
        };
        let apart =
            |a: &[i32], b: &[i32]| -> i32 { a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum() };
        for piece in [Piece::Knight, Piece::Bishop, Piece::Rook, Piece::Queen] {
            let twin = apart(&entries(piece, mg_value), &entries(piece, eg_value));
            for (name, half) in HALVES {
                let mine = entries(piece, half);
                for other in Piece::PIECES.into_iter().filter(|&other| other != piece) {
                    for (other_name, other_half) in HALVES {
                        let away = apart(&mine, &entries(other, other_half));
                        assert!(
                            away > twin,
                            "{:?}'s {} table is {} from the {} {:?} and {} from its own twin",
                            piece,
                            name,
                            away,
                            other_name,
                            other,
                            twin
                        );
                    }
                }
            }
        }
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
