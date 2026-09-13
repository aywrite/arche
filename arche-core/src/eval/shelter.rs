// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What stands between each king and the board, and what that is worth.
//!
//! The term owns its counts, the masks they are read off, its weights, its
//! fold and the memo the search hands it. All seven counts are read off the
//! king's square, so a king move rewrites the side's whole reading and a pawn
//! move changes it wherever the pawn stood. There is nothing here for
//! `Accumulator::count` to add and take away a piece at a time.

use super::pawn_structure::files_of;
use crate::board::{Board, ZOBRIST};
use crate::misc::{Color, Piece};
use crate::psqt::pack;

/// How many ranks in front of the king are masked. Three, which is as far as
/// an enemy pawn is counted: for a king at home that is the rank its own pawns
/// start on and the two beyond it.
const RANKS_AHEAD: usize = 3;

/// How many counts the king's shelter is measured in, and so how many weights
/// it carries at each end of the taper. [`counts_of`] prints them in this
/// order: this side's pawns one rank in front of its king, its pawns two ranks
/// in front, the king's files with no pawn of either colour on them, the
/// king's files holding an enemy pawn and none of ours, and then the enemy
/// pawns one, two and three ranks in front of the king.
pub(crate) const COUNTS: usize = 7;

/// The three files a king on `square` stands behind, as a bit per file.
///
/// A king on the a file or the h file is read against three files rather than
/// two, by stepping the middle one in: the b file and the g file are the
/// centres a corner king keeps. So every king square names three files and
/// the counts off them are on one scale wherever the king stands.
const fn king_files(square: u8) -> u8 {
    let file = square % 8;
    let centre = match file {
        0 => 1,
        7 => 6,
        file => file,
    };
    0b111 << (centre - 1)
}

/// The three squares `ahead` ranks in front of a king on `square`, on the
/// files [`king_files`] names, and empty where that rank is off the board.
///
/// `forward` is the direction the side's pawns push, so a white king is read
/// up the board and a black king down it. A king that has walked far enough
/// up is left with nothing in front of it, which is the answer rather than a
/// case to rule out: a king off its own back ranks has no shelter, and what
/// that is worth is for the weights to say.
const fn shelter_rank(square: u8, forward: i8, ahead: i8) -> u64 {
    let rank = (square / 8) as i8 + forward * ahead;
    if rank < 0 || rank > 7 {
        return 0;
    }
    // a file bit and a square index share their low three bits, so the byte
    // shifted to the rank is the three squares on it
    (king_files(square) as u64) << (rank * 8)
}

/// The squares in front of each king, and which files it stands behind.
///
/// `ahead` is indexed by how many ranks forward, then by `Color`'s
/// discriminant the way the accumulator's material is, then by the king's
/// square. The colour is an index because the ranks in front of a king run
/// opposite ways for the two; the files do not, and one entry serves both.
///
/// The same three masks serve both sides of the term. Read against this side's
/// pawns they are the cover the king has, and against the other side's they
/// are the pawns coming for it.
struct Masks {
    ahead: [[[u64; 64]; 2]; RANKS_AHEAD],
    files: [u8; 64],
}

impl Masks {
    /// Built at compile time, the way the attack masks in `board` are.
    const fn new() -> Self {
        let mut masks = Masks {
            ahead: [[[0; 64]; 2]; RANKS_AHEAD],
            files: [0; 64],
        };
        let mut square = 0u8;
        while square < 64 {
            let i = square as usize;
            masks.files[i] = king_files(square);
            let mut rank = 0;
            while rank < RANKS_AHEAD {
                let ahead = rank as i8 + 1;
                masks.ahead[rank][Color::White as usize][i] = shelter_rank(square, 1, ahead);
                masks.ahead[rank][Color::Black as usize][i] = shelter_rank(square, -1, ahead);
                rank += 1;
            }
            square += 1;
        }
        masks
    }
}

static MASKS: Masks = Masks::new();

/// What one of those seven counts is worth, as the packed pairs the taper is
/// read from.
///
/// Fitted 2026-09-12 by `scripts/tune.py` over the whole archived strength
/// run: 28,675 games (27,133 at 10+0.1, 1,500 at 30+0.3 and 42 at 2+0.02),
/// whose 3,711,074 post-book plies gave 3,536,193 positions and 1,623,149
/// quiet rows in 28,618 of them, extracted by `arche terms` at 4e5bd7d, with
/// K held at 1.2071 and the games split 17,192 that trained, 5,654 that chose
/// the ridge of 1e-8 and 5,772 that were sealed. The 768 table entries, the
/// six material values and the eight mobility weights were all held where
/// they stand, so these fourteen are the only thing that moved. Every number
/// here is of the rounded vector that ships. The selection group scores
/// 0.090395 at zero and 0.089746 at these, a paired difference of -0.000649
/// against a standard error of 0.000116 over its 5,654 games, at a design
/// factor of 3.9. That is outside its interval, which the mobility fit's
/// reading was not.
///
/// The sealed group was opened once, after the games had accepted this
/// vector, over 5,772 games and 324,273 positions no fit and no ridge choice
/// had read. It scores 0.090914 at zero and 0.090377 at these, a paired
/// difference of -0.000537 against a standard error of 0.000118 at a design
/// factor of 3.9. That is outside its interval, and 0.68 standard errors
/// from the selection group's reading, so the two groups agree. The piece
/// square fit's two did not.
///
/// Not one of the fourteen rounded to nothing, so every count is priced and
/// none can be left uncounted at the leaf the way `mobility::SCORED_KINDS`
/// leaves a mobility kind. [`Cache`] is what pays for the seven instead.
///
/// Two of the storm's three signs are not what the term was named for. An
/// enemy pawn one rank in front of the king reads 11 and 35, and three ranks
/// out reads 14, so the fit likes the near storm where the term expected it
/// to fear it, and only the middle rank is negative. Our own cover is worth
/// 21 in the midgame and -26 in the ending, which reads as a king that wants
/// to be active rather than covered. The corpus is the first place to look
/// and not the term: 66.4% of its appearances have six or fewer pieces left
/// on the board and 6.0% have thirteen or more of the fourteen, so the
/// midgame half of the taper, which is the half king safety is about, is
/// fitted on the thinnest slice of the games. The count with the oddest
/// weight is also the thinnest supported: an enemy pawn one rank in front of
/// a king carries a coefficient in 4.65% of the rows against 47.83% for the
/// near cover, because a king usually takes such a pawn and the position is
/// then not quiet. The loss says these fourteen score the corpus better than
/// zero did, and that is all it says.
///
/// A side's seven counts come to eighteen at the very most. Five of them are
/// at most three pawns each, and the other two share three files between them
/// rather than reaching three each. Against these weights the largest total
/// any legal set of counts reaches on one side is 168 in the midgame half and
/// -207 in the ending half, so a boardful of both colours leaves the sixteen
/// bits `pack` gives each half a long way off. Six of the fourteen are past
/// single figures, which the paragraph here said they would not be while they
/// were all zero.
static SHELTER: [i32; COUNTS] = [
    pack(10, -11),
    pack(21, -26),
    pack(-11, -26),
    pack(-8, -1),
    pack(11, 35),
    pack(-18, 2),
    pack(14, -6),
];

/// The weight of one of the seven counts, as the packed pair. The tuner's
/// seam asks through [`super::TERMS`], so that a slot names the live weight
/// rather than a copy of it, the way it reads the tables.
pub(crate) fn weight(index: usize) -> i32 {
    SHELTER[index]
}

/// What this term depends on and nothing else: both sides' pawns and both
/// kings' squares.
///
/// [`counts_of`] reads the pawn boards and the two king squares and nothing
/// else, so two positions whose pawns and kings agree agree on every count of
/// it whatever else has moved. This key stands in for that agreement rather
/// than being it: two positions can share it and differ, which takes a
/// collision across the whole sixty four bits. The pawn key already hashes the
/// pawns of both colours and is kept in step move by move, so this is that key
/// with the two kings folded in, from the same zobrist table the position key
/// uses.
///
/// Composed here rather than maintained beside `Board::pawn_key`, because a
/// king move would then have to write it and the cost of this is two loads and
/// two xors at the one place that asks.
#[inline]
pub(crate) fn key(board: &Board) -> u64 {
    board.pawn_key
        ^ ZOBRIST.get_piece_key(board.king_index(Color::White), Piece::King, Color::White)
        ^ ZOBRIST.get_piece_key(board.king_index(Color::Black), Piece::King, Color::Black)
}

/// What stands between this side's king and the board, as seven counts in the
/// order [`COUNTS`] names them: this side's pawns one rank in front of the
/// king and two ranks in front, how many of the king's three files hold no
/// pawn of either colour, how many hold an enemy pawn and none of this side's,
/// and then the enemy pawns one, two and three ranks in front of the king.
///
/// All seven are read off the three files the king stands behind, which
/// [`king_files`] steps in at the two corners so that the counts mean the
/// same thing on every square. Pawns and nothing else: a piece in front of
/// the king shelters it too, but a term that pays for one pays a piece to
/// sit still, and the piece square tables already hold an opinion about
/// where a piece belongs. A pawn is the part of the cover the king cannot
/// get back.
///
/// The last three are the storm, and they are the same masks read against
/// the other side's pawns. A pawn of ours on g3 is cover and a pawn of
/// theirs on g3 is not the absence of cover, it is a lever, so the two are
/// counted apart and each rank apart from the next: how far the storm has
/// come is most of what it is worth, and the weights are where that is
/// said. Three ranks is as far as it is followed, which for a king at home
/// reaches the fourth rank.
///
/// The two file counts overlap the two pawn counts, since a file with no
/// pawn of ours on it adds nothing to either of those. They are kept apart
/// because they are different knowledge: a missing g pawn and a g pawn
/// pushed to g4 both leave the near count short, and only the first opens
/// the file to a rook.
///
/// Nothing here is gated on the king standing at home. A king that has
/// walked up the board has no rank in front of it inside the masks and
/// counts nothing, so the term fades rather than falling off a cliff the
/// search could step over.
///
/// Both the evaluation and the tuner's walk read this, so the identity
/// between them cannot see a wrong count here. What pins it is the hand counts
/// in the tests below, the way the mobility counts are pinned.
#[inline]
pub(crate) fn counts_of(board: &Board, color: Color) -> [i32; COUNTS] {
    let masks = &MASKS;
    let square = board.king_index(color) as usize;
    let side = color as usize;
    let (ours, theirs) = board.sides(color);
    let pawns = board.pawns();
    let (our_pawns, their_pawns) = (pawns & ours, pawns & theirs);
    let ahead =
        |pawns: u64, rank: usize| (pawns & masks.ahead[rank][side][square]).count_ones() as i32;
    // the king's files with no pawn of ours on them, split by whether the
    // other side has one there
    let files = masks.files[square];
    let bare = files & !files_of(our_pawns);
    let theirs_on = files_of(their_pawns);
    let open = (bare & !theirs_on).count_ones() as i32;
    let half_open = (bare & theirs_on).count_ones() as i32;
    [
        ahead(our_pawns, 0),
        ahead(our_pawns, 1),
        open,
        half_open,
        ahead(their_pawns, 0),
        ahead(their_pawns, 1),
        ahead(their_pawns, 2),
    ]
}

/// The seven counts, written into `into`, which is what [`super::TERMS`] hands
/// the tuner's walk.
pub(crate) fn counts(board: &Board, color: Color, into: &mut [i32]) {
    into.copy_from_slice(&counts_of(board, color));
}

/// What white's king shelter stands ahead by, as a packed pair on the scale
/// the piece square pair is on.
#[inline]
pub(crate) fn fold(board: &Board) -> i32 {
    fold_with(board, &SHELTER)
}

/// The same fold against weights named by the caller.
///
/// The live weights are the fit's now, and the two halves of every one of
/// them differ, so the sign of this term, the order of the seven counts and
/// the packing all show in an evaluation the engine prints and in the rows
/// the tuner's walk states. The tests still supply weights of their own
/// through here, because what they pin is the fold rather than the fit: a
/// permuted [`SHELTER`] would be a different evaluation and not a wrong one.
#[inline]
fn fold_with(board: &Board, weights: &[i32; COUNTS]) -> i32 {
    let white = counts_of(board, Color::White);
    let black = counts_of(board, Color::Black);
    let mut packed = 0;
    for ((weight, white), black) in weights.iter().zip(white).zip(black) {
        packed += weight * (white - black);
    }
    packed
}

/// How many shelter scores the cache holds. A power of two, so the index is
/// a mask rather than a remainder.
///
/// Eight thousand entries at sixteen bytes is a hundred and twenty eight
/// kilobytes, which is past the first level cache and inside the second.
/// The size was measured rather than reasoned about. Callgrind over the
/// bench with the cache simulated, at eleven, twelve, thirteen and fourteen
/// bits, reads 3,968,905,639, 3,958,897,319, 3,950,063,019 and 3,943,552,801
/// instructions against last level misses of 284,996, 285,117, 286,191 and
/// 292,291. Each bit buys fewer instructions than the one before it and the
/// misses are flat until fourteen, where they turn up by six thousand. So
/// this is the last size the memory does not notice, and the whole range is
/// within two thirds of a percent of instructions: the constant is not
/// load bearing and a later working set can move it.
const CACHE_BITS: usize = 13;
pub(super) const CACHE_SLOTS: usize = 1 << CACHE_BITS;

/// One remembered shelter score, under the key that decides it.
///
/// The whole key is kept rather than the bits the index does not use, so a
/// hit is a hit on the position's pawns and kings and not on a tag that
/// happens to agree. A wrong hit here would be a silently wrong evaluation,
/// which is the one error a search does not report, and sixty four bits of
/// key costs four bytes against the alternative.
#[derive(Copy, Clone)]
struct Entry {
    key: u64,
    packed: i32,
}

/// The king shelter, remembered by what it depends on.
///
/// The term reads both sides' pawns and the two king squares and nothing
/// else, so a position that agrees with a remembered one on those scores the
/// same however its pieces stand. Most moves in a search are piece moves,
/// which leave every one of those alone, so the score computed at one leaf
/// answers a great many of the leaves after it.
///
/// Direct mapped and never cleared. An entry is only ever read against the
/// key that wrote it, so a stale one is a miss rather than a wrong answer,
/// and a search that begins with the last search's entries begins with a warm
/// cache. That is also what leaves the node counts alone: the cache changes
/// how a score is arrived at and not what it is, so the counts are the ones
/// the weights alone produce.
///
/// Owned by the searcher rather than by the board, because it is scratch and
/// not position. A board carries what it would take to undo a move and
/// compares equal to another board holding the same position; a cache does
/// neither, and one thread's cache is its own.
pub(super) struct Cache {
    entries: Box<[Entry]>,
}

impl Default for Cache {
    fn default() -> Self {
        // an empty entry is key zero holding a score of zero, so a position
        // whose pawns and kings xor to nothing would read it as its own and
        // take nothing from it. That is a sixty four bit coincidence, which
        // is the same one a wrong hit needs anywhere else in the table
        Cache {
            entries: vec![Entry { key: 0, packed: 0 }; CACHE_SLOTS].into_boxed_slice(),
        }
    }
}

impl Cache {
    /// What white's shelter stands ahead by, remembered or computed.
    #[inline]
    pub(super) fn get(&mut self, board: &Board) -> i32 {
        let key = key(board);
        let slot = (key as usize) & (CACHE_SLOTS - 1);
        let entry = &mut self.entries[slot];
        if entry.key == key {
            return entry.packed;
        }
        let packed = fold(board);
        *entry = Entry { key, packed };
        packed
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Board, CACHE_SLOTS, COUNTS, Cache, Color, MASKS, RANKS_AHEAD, counts_of, files_of, fold,
        key, king_files,
    };
    use crate::board::fens;
    use crate::psqt::{eg_value, mg_value, pack};
    use pretty_assertions::assert_eq;

    /// A remembered score is read back rather than recomputed, which is the
    /// whole point of the cache and is not visible in what it answers.
    ///
    /// Asked twice for one position, the second answer comes from the entry
    /// the first wrote. There is no counter to read, so this says it the way
    /// a caller could: the entry the key lands on holds the score after the
    /// first call, and holds it under that key and no other.
    #[test]
    fn a_score_is_remembered_under_the_key_that_wrote_it() {
        let board = Board::from_fen(fens::MIDDLEGAME).unwrap();
        let mut cache = Cache::default();
        let key = key(&board);
        let slot = (key as usize) & (CACHE_SLOTS - 1);
        assert_eq!(cache.entries[slot].key, 0, "the slot starts empty");
        let first = cache.get(&board);
        assert_eq!(cache.entries[slot].key, key);
        assert_eq!(cache.entries[slot].packed, fold(&board));
        assert_eq!(cache.get(&board), first);
    }

    /// The key is the pawn key with the two kings folded in, which is what
    /// the cache is keyed on. What the term reads is the pawns and the two
    /// king squares, so a piece that is neither has to leave it alone and
    /// either king moving has to move it. A key that missed a king would hand
    /// one position's shelter to another.
    #[test]
    fn the_shelter_key_follows_the_pawns_and_the_two_kings() {
        let bare = Board::from_fen("4k3/pppppppp/8/8/8/8/PPPPPPPP/4K3 w - - 0 1").unwrap();

        // the same pawns and the same two kings behind a boardful of other
        // pieces, which the shelter does not read
        let pieced =
            Board::from_fen("rnbqk1nr/pppppppp/8/8/8/8/PPPPPPPP/RNBQK1NR w - - 0 1").unwrap();
        assert_eq!(key(&bare), key(&pieced));

        // either king one square along
        let ours = Board::from_fen("4k3/pppppppp/8/8/8/8/PPPPPPPP/5K2 w - - 0 1").unwrap();
        let theirs = Board::from_fen("5k2/pppppppp/8/8/8/8/PPPPPPPP/4K3 w - - 0 1").unwrap();
        assert_ne!(key(&bare), key(&ours));
        assert_ne!(key(&bare), key(&theirs));
        assert_ne!(key(&ours), key(&theirs));

        // and one pawn pushed, the pawn key being the rest of it
        let pushed = Board::from_fen("4k3/pppppppp/8/8/8/7P/PPPPPPP1/4K3 w - - 0 1").unwrap();
        assert_ne!(key(&bare), key(&pushed));
    }

    /// The counts by hand, square by square, because nothing else pins them.
    /// The tuner's identity folds a row against the live weights, and `eval`
    /// and the tuner's walk read the same helper, so the two sides of the
    /// identity move together whatever the helper answers. These cases are the
    /// only check this term has.
    ///
    /// Each case names what the count is made of, in the order the helper
    /// returns them: our pawns one rank ahead and two, the king's files that
    /// hold no pawn at all, the ones that hold an enemy pawn and none of ours,
    /// and then the enemy pawns one, two and three ranks ahead. The other king
    /// stands out of the way.
    #[test]
    fn a_king_shelters_behind_what_a_hand_count_says_it_does() {
        for (fen, counts, why) in [
            // three pawns where a castled king wants them
            (
                "k7/8/8/8/8/8/5PPP/6K1 w - - 0 1",
                [3, 0, 0, 0, 0, 0, 0],
                "a king on g1 behind f2, g2 and h2",
            ),
            // the g pawn one square further on is the far rank rather than
            // the near one
            (
                "k7/8/8/8/8/6P1/5P1P/6K1 w - - 0 1",
                [2, 1, 0, 0, 0, 0, 0],
                "a king on g1 with the g pawn on g3",
            ),
            // the g file holds no pawn of either colour
            (
                "k7/8/8/8/8/8/5P1P/6K1 w - - 0 1",
                [2, 0, 1, 0, 0, 0, 0],
                "a king on g1 with no g pawn",
            ),
            // the same file with a black pawn on it is half open rather than
            // open. The pawn is on g7, which is past the three ranks the
            // storm is followed over, so it is a file and not a storm
            (
                "k7/6p1/8/8/8/8/5P1P/6K1 w - - 0 1",
                [2, 0, 0, 1, 0, 0, 0],
                "a king on g1 with a black pawn on g7",
            ),
            // h2 near, g3 far, and the f file holding a black pawn on f5,
            // which is a rank further out than the storm reaches
            (
                "k7/8/8/5p2/8/6P1/7P/6K1 w - - 0 1",
                [1, 1, 0, 1, 0, 0, 0],
                "a king on g1 with the f file gone",
            ),
            // an enemy pawn standing on a rank in front of the king is not
            // cover. It is the storm, counted by the rank it has reached
            (
                "k7/8/8/8/8/8/5PpP/6K1 w - - 0 1",
                [2, 0, 0, 1, 1, 0, 0],
                "a king on g1 with a black pawn on g2",
            ),
            (
                "k7/8/8/8/8/6p1/5P1P/6K1 w - - 0 1",
                [2, 0, 0, 1, 0, 1, 0],
                "a king on g1 with a black pawn on g3",
            ),
            (
                "k7/8/8/8/6p1/8/5P1P/6K1 w - - 0 1",
                [2, 0, 0, 1, 0, 0, 1],
                "a king on g1 with a black pawn on g4",
            ),
            // two ranks of storm at once, on two files
            (
                "k7/8/8/8/7p/6p1/5P1P/6K1 w - - 0 1",
                [2, 0, 0, 1, 0, 1, 1],
                "a king on g1 against pawns on g3 and h4",
            ),
            // a king in the corner is read against three files, so the f file
            // it does not stand beside is still counted. Two files would
            // leave this at nothing
            (
                "k7/8/8/8/8/8/6PP/7K w - - 0 1",
                [2, 0, 1, 0, 0, 0, 0],
                "a king on h1 behind g2 and h2",
            ),
            (
                "k7/8/8/8/8/8/PPP5/K7 w - - 0 1",
                [3, 0, 0, 0, 0, 0, 0],
                "a king on a1 behind a2, b2 and c2",
            ),
            // a king off its own ranks has nothing on the masked ranks, so
            // the pawns it left behind are not shelter
            (
                "k7/8/8/4K3/8/8/3PPP2/8 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 0],
                "a king on e5 with its pawns at home",
            ),
            // a pawn on the file stops it counting as open wherever on the
            // file it stands, so a passed pawn up the board is not a hole
            // behind the king it left
            (
                "k7/6P1/8/8/8/8/8/6K1 w - - 0 1",
                [0, 0, 2, 0, 0, 0, 0],
                "a king on g1 whose only pawn is on g7",
            ),
            // the ranks in front of a king on the eighth are off the board
            // rather than round the other side of it
            (
                "4K3/8/8/8/8/8/8/k7 w - - 0 1",
                [0, 0, 3, 0, 0, 0, 0],
                "a king on e8 with no pawns anywhere",
            ),
        ] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(counts_of(&board, Color::White), counts, "{}", why);
        }
    }

    /// The same reading for black, whose king is measured down the board
    /// rather than up it.
    #[test]
    fn a_black_king_is_measured_down_the_board() {
        // the rank in front of a king on the first is off the board, and the
        // three files all hold a white pawn and no black one
        let board = Board::from_fen("7K/8/8/8/8/8/3PPP2/4k3 b - - 0 1").unwrap();
        assert_eq!(counts_of(&board, Color::Black), [0, 0, 0, 3, 0, 0, 0]);
        // and the storm the other way up: the white pawn on g6 stands two
        // ranks in front of a black king on g8, and is not its cover
        let stormed = Board::from_fen("6k1/5ppp/6P1/8/8/8/8/6K1 b - - 0 1").unwrap();
        assert_eq!(counts_of(&stormed, Color::Black), [3, 0, 0, 0, 0, 1, 0]);
    }

    /// Black's count of a position is white's count of its reflection, so the
    /// two colours are read the same way round.
    #[test]
    fn the_two_colours_count_the_same_squares() {
        let white = Board::from_fen("k7/8/8/5p2/8/6P1/7P/6K1 w - - 0 1").unwrap();
        let black = Board::from_fen("6k1/7p/6p1/8/5P2/8/8/K7 b - - 0 1").unwrap();
        assert_eq!(
            counts_of(&white, Color::White),
            counts_of(&black, Color::Black)
        );
        // and the storm half of it, which the pair above leaves at zero
        let stormed = Board::from_fen("k7/8/8/8/7p/6p1/5P1P/6K1 w - - 0 1").unwrap();
        let mirrored = Board::from_fen("6k1/5p1p/6P1/7P/8/8/8/K7 b - - 0 1").unwrap();
        assert_eq!(
            counts_of(&stormed, Color::White),
            counts_of(&mirrored, Color::Black)
        );
    }

    /// Every square names three files, the two corners included, and the
    /// three are the king's own file and its neighbours wherever there is
    /// room for them.
    #[test]
    fn every_king_square_names_three_files() {
        for square in 0..64u8 {
            let files = king_files(square);
            assert_eq!(files.count_ones(), 3, "the files of {}", square);
            assert_eq!(
                files & (1 << (square % 8)),
                1 << (square % 8),
                "the king's own file is not among the files of {}",
                square
            );
            assert_eq!(
                files.trailing_zeros() + 2,
                7 - files.leading_zeros(),
                "the files of {} are not three in a row",
                square
            );
        }
    }

    /// Each mask holds the rank its index names, on those same three files,
    /// and nothing where the board has run out.
    #[test]
    fn the_masks_hold_the_ranks_in_front_of_the_king() {
        for square in 0..64u8 {
            let rank = i32::from(square / 8);
            for (side, forward) in [(Color::White, 1), (Color::Black, -1)] {
                let i = side as usize;
                for step in 0..RANKS_AHEAD {
                    let mask = MASKS.ahead[step][i][square as usize];
                    let target = rank + forward * (step as i32 + 1);
                    if !(0..8).contains(&target) {
                        assert_eq!(mask, 0, "{:?} on {} at {} ahead", side, square, step + 1);
                        continue;
                    }
                    assert_eq!(mask.count_ones(), 3, "{:?} on {}", side, square);
                    assert_eq!(
                        files_of(mask),
                        king_files(square),
                        "{:?} on {} covers other files",
                        side,
                        square
                    );
                    let rank_mask = 0xffu64 << (target * 8);
                    assert_eq!(
                        mask & rank_mask,
                        mask,
                        "{:?} on {} is off its rank",
                        side,
                        square
                    );
                }
            }
        }
    }

    /// The position the fold test below is read against, and what each side
    /// counts in it, worked out by hand rather than read back off
    /// [`counts_of`].
    ///
    /// White's king on g1 stands behind the f, g and h files. It has f2 and h2
    /// one rank ahead and g3 two, and all three files hold a pawn of its own,
    /// so neither file count fires. Coming the other way it faces g2 one rank
    /// ahead, f3 and h3 two, and f4, g4 and h4 three.
    ///
    /// Black's king on b8 stands behind the a, b and c files with nothing on
    /// either rank in front of it and no white pawn within three ranks, so its
    /// storm is empty. White's pawn on a4 leaves that file half open and the b
    /// and c files hold no pawn at all.
    ///
    /// The seven differences are 2, 1, -2, -1, 1, 2 and 3. None is zero, so
    /// every slot is doing work in the assertion below, which a position with
    /// an empty storm would not manage.
    const SHELTERED: &str = "1k6/8/8/8/P4ppp/5pPp/5PpP/6K1 w - - 0 1";
    const WHITE_SHELTERS: [i32; COUNTS] = [2, 1, 0, 0, 1, 2, 3];
    const BLACK_SHELTERS: [i32; COUNTS] = [0, 0, 2, 1, 0, 0, 0];

    /// Seven weights that differ from each other at both ends of the taper, so
    /// that a pair read into the wrong count's slot lands on a different
    /// number. The seven differences between the halves are 9, -20, 8, -12,
    /// 20, -29 and -14, which differ from each other too, so a permutation of
    /// either array shows.
    const TRIAL: [i32; COUNTS] = [
        pack(11, 2),
        pack(-7, 13),
        pack(3, -5),
        pack(29, 41),
        pack(17, -3),
        pack(-23, 6),
        pack(5, 19),
    ];

    /// What the fold does with weights that are not the shipped ones.
    ///
    /// The shipped weights would do here now that they are not zero. Weights
    /// of this test's own are kept anyway, because the shipped ones are the
    /// fit's and will move again: a pin written against them would have to
    /// be rewritten by every refit, and what it is pinning is the fold. So
    /// this hands the fold seven pairs that differ from each other at both
    /// ends and asserts the packed pair against the arithmetic: white's count
    /// less black's, count by count, each half of the pair summed on its
    /// own.
    #[test]
    fn the_shelter_fold_reads_white_less_black_count_by_count() {
        let board = Board::from_fen(SHELTERED).unwrap();
        assert_eq!(counts_of(&board, Color::White), WHITE_SHELTERS);
        assert_eq!(counts_of(&board, Color::Black), BLACK_SHELTERS);
        let midgame: i32 = (0..COUNTS)
            .map(|i| mg_value(TRIAL[i]) * (WHITE_SHELTERS[i] - BLACK_SHELTERS[i]))
            .sum();
        let endgame: i32 = (0..COUNTS)
            .map(|i| eg_value(TRIAL[i]) * (WHITE_SHELTERS[i] - BLACK_SHELTERS[i]))
            .sum();
        assert_ne!(
            midgame, endgame,
            "the two halves would not tell a swap apart"
        );
        let packed = super::fold_with(&board, &TRIAL);
        assert_ne!(midgame, 0, "black less white would answer the same here");
        assert_eq!((mg_value(packed), eg_value(packed)), (midgame, endgame));
    }
}
