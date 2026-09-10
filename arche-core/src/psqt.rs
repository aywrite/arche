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

// The king is the piece the two phases disagree about most, and the page gives
// a table for each: hidden behind its own pawns while there are pieces to
// hide from, and in the middle of the board once there are not. A single table
// cannot say both, which is why the king carried a table of zeroes until the
// score was tapered.

#[rustfmt::skip]
const KING: [i16; 64] = [
    -30,-40,-40,-50,-50,-40,-40,-30,
    -30,-40,-40,-50,-50,-40,-40,-30,
    -30,-40,-40,-50,-50,-40,-40,-30,
    -30,-40,-40,-50,-50,-40,-40,-30,
    -20,-30,-30,-40,-40,-30,-30,-20,
    -10,-20,-20,-20,-20,-20,-20,-10,
     20, 20,  0,  0,  0,  0, 20, 20,
     20, 30, 10,  0,  0, 10, 30, 20
];

#[rustfmt::skip]
const KING_END: [i16; 64] = [
    -50,-40,-30,-20,-20,-30,-40,-50,
    -30,-20,-10,  0,  0,-10,-20,-30,
    -30,-10, 20, 30, 30, 20,-10,-30,
    -30,-10, 30, 40, 40, 30,-10,-30,
    -30,-10, 30, 40, 40, 30,-10,-30,
    -30,-10, 20, 30, 30, 20,-10,-30,
    -30,-30,  0,  0,  0,  0,-30,-30,
    -50,-30,-30,-30,-30,-30,-30,-50
];

/// The pawn is the other one, and here the page has nothing to offer: it
/// prints a single pawn table, and that table is a middlegame one. It pushes
/// the two centre pawns and holds the rest back to shelter a castled king,
/// which is why a pawn still at home on d2 scores less there than one on a2.
/// With no pieces left to shelter from, none of that is true and only the
/// distance to promotion is, so the endgame table is a ramp: the same for
/// every file, steepening as the pawn gets close enough for the ending to be
/// about it.
#[rustfmt::skip]
const PAWNS_END: [i16; 64] = [
      0,  0,  0,  0,  0,  0,  0,  0,
     80, 80, 80, 80, 80, 80, 80, 80,
     50, 50, 50, 50, 50, 50, 50, 50,
     30, 30, 30, 30, 30, 30, 30, 30,
     15, 15, 15, 15, 15, 15, 15, 15,
      5,  5,  5,  5,  5,  5,  5,  5,
      0,  0,  0,  0,  0,  0,  0,  0,
      0,  0,  0,  0,  0,  0,  0,  0
];

// The other four had no endgame table at all. Each handed its one array to
// both ends of the taper, so a knight on the rim was worth the same at move
// fifteen and at move seventy. These four are that array copied entry for
// entry, which is what leaves the evaluation exactly where it was: what the
// two ends should say about a knight, a bishop, a rook and a queen is a
// question for the fit that follows.
//
// Copied and not aliased. `const KNIGHTS_END: [i16; 64] = KNIGHTS;` would
// hold the same numbers today and move both tables when the fit edits one.

#[rustfmt::skip]
const KNIGHTS_END: [i16; 64] = [
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
const BISHOPS_END: [i16; 64] = [
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
const ROOKS_END: [i16; 64] = [
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
const QUEENS_END: [i16; 64] = [
    -20,-10,-10, -5, -5,-10,-10,-20,
    -10,  0,  0,  0,  0,  0,  0,-10,
    -10,  0,  5,  5,  5,  5,  0,-10,
    -5,  0,  5,  5,  5,  5,  0, -5,
    0,  0,  5,  5,  5,  5,  0, -5,
    -10,  5,  5,  5,  5,  5,  0,-10,
    -10,  0,  5,  0,  0,  0,  0,-10,
    -20,-10,-10, -5, -5,-10,-10,-20
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
    /// declares them for white, then the same six for black. Every piece hands
    /// two tables of its own to the pair.
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

    /// One end of the taper: what to call it, and how to read it out of a
    /// packed pair.
    type Half = (&'static str, fn(i32) -> i32);

    /// Both ends, since every piece has a table at each of them and a shape
    /// test that walked one would say nothing about the other. The name is
    /// for the failure message: a shape that moved is worth knowing which
    /// half it moved in.
    const HALVES: [Half; 2] = [("midgame", mg_value), ("endgame", eg_value)];

    fn on_the_edge(file: File, rank: u8) -> bool {
        rank == 1 || rank == 8 || file == File::A || file == File::H
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
    /// Sixteen of the seven hundred and sixty eight table entries have no
    /// support in any corpus, and this is which ones.
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
            let (least, most) = extremes(Piece::Knight, half);
            assert_eq!(least, CORNERS, "the worst {} squares are {:?}", name, least);
            assert_eq!(
                most,
                vec![(File::D, 4), (File::E, 4), (File::D, 5), (File::E, 5)],
                "the best {} squares are {:?}",
                name,
                most
            );
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
            let (least, _) = extremes(Piece::Bishop, half);
            assert_eq!(least, CORNERS, "the worst {} squares are {:?}", name, least);
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
            let (least, most) = extremes(Piece::Queen, half);
            assert_eq!(least, CORNERS, "the worst {} squares are {:?}", name, least);
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

    /// The endgame pawn table is a ramp and the midgame one is not, which is
    /// the whole of the difference between them. The centre pawn held back to
    /// shelter a king is the square that shows it.
    #[test]
    fn a_pawn_is_scored_by_rank_alone_in_the_ending() {
        for rank in 2..=7 {
            let ramp = eg(Piece::Pawn, Color::White, File::A, rank);
            for file in File::VARIANTS {
                assert_eq!(
                    eg(Piece::Pawn, Color::White, file, rank),
                    ramp,
                    "{:?}{}",
                    file,
                    rank
                );
            }
            // and it rises toward promotion rather than being flat
            if rank > 2 {
                assert!(ramp > eg(Piece::Pawn, Color::White, File::A, rank - 1));
            }
        }
        // shelter in the middlegame, where the same rank is not one number:
        // a pawn on d2 is held back to cover a castled king and one on a2 is
        // not, so the midgame table is not a ramp
        assert_ne!(
            value(Piece::Pawn, Color::White, File::D, 2),
            value(Piece::Pawn, Color::White, File::A, 2)
        );
    }

    /// The four pieces given an endgame table hold the numbers they held
    /// before they had one, entry for entry. That is what says the commit
    /// that added the tables changed no evaluation: the two halves of every
    /// packed pair are equal, so the interpolation returns what it returned
    /// whatever the phase, and the bench, both node count pins and both gated
    /// suites are unmoved by it.
    ///
    /// This test dies with the fit. Making these halves differ is the whole
    /// point of the arm, so the commit that lands the fitted numbers deletes
    /// this and moves every count named above.
    #[test]
    fn the_four_new_endgame_tables_still_hold_their_midgame_numbers() {
        for piece in [Piece::Knight, Piece::Bishop, Piece::Rook, Piece::Queen] {
            for rank in 1..=8 {
                for file in File::VARIANTS {
                    let packed = packed_at(piece, Color::White, file, rank);
                    assert_eq!(
                        mg_value(packed),
                        eg_value(packed),
                        "{:?} on {:?}{}",
                        piece,
                        file,
                        rank
                    );
                }
            }
        }
    }

    /// Each of the four is written out rather than aliased to its midgame
    /// twin. `const KNIGHTS_END: [i16; 64] = KNIGHTS;` holds the same numbers,
    /// passes every test above, and moves both tables when the fit edits one.
    /// Nothing at run time can tell the copy from the alias, so this reads the
    /// source and asks whether the numbers are there.
    #[test]
    fn each_new_endgame_table_is_written_out_rather_than_aliased() {
        let source = include_str!("psqt.rs");
        for table in ["KNIGHTS_END", "BISHOPS_END", "ROOKS_END", "QUEENS_END"] {
            let written = format!("const {}: [i16; 64] = [", table);
            assert!(source.contains(&written), "{} is not written out", table);
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
