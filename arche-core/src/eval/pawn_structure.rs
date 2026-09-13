// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What each side's pawns stand as, and what that is worth.
//!
//! The term owns its counts, the masks they are read off, its weights, its
//! fold and the memo the search hands it. A pawn that moves changes which of
//! the pawns behind and beside it are passed, isolated or doubled, so there is
//! nothing here for `Accumulator::count` to add and take away a pawn at a
//! time. What makes that cheap anyway is that only a pawn move changes it at
//! all, which is what [`Cache`] is built on.

use crate::board::Board;
use crate::misc::Color;
use crate::psqt::pack;

/// How many ranks the passed pawn count is split over: a pawn's relative
/// second through its relative seventh, which is every rank one can stand on.
///
/// A table by rank rather than one weight times the rank. What a passer is
/// worth is convex in how far it has come, and a ramp cannot say so; six
/// numbers can, and the tables already spend sixty four on where a pawn
/// stands.
const PASSED_RANKS: usize = 6;

/// How many counts the pawn structure is measured in, in the order
/// [`PAWN_STRUCTURE`] and [`counts_of`] are indexed by: the passed pawns by
/// relative rank, then the isolated pawns, then the doubled ones.
pub(crate) const COUNTS: usize = PASSED_RANKS + 2;

/// A pawn's own file and the files beside it, as a bit per file.
///
/// The shelter's `king_files` steps the middle file in at the two edges so
/// that a king always names three; this does not, because the two are asking
/// different questions. A white pawn on a4 is passed while black has no pawn
/// on the a file or the b file, and a black pawn on the c file has nothing to
/// say about it. Stepping in would let that pawn stop it.
const fn pawn_files(square: u8) -> u8 {
    let own = 1u8 << (square % 8);
    // a shift off either end of the byte drops the bit, which is the edge
    // case: the a file has no file to its left and the h file none to its
    // right
    own | (own << 1) | (own >> 1)
}

/// The squares on `files` on every rank strictly ahead of `square`, ahead
/// meaning the direction `forward` pushes.
const fn span_ahead(square: u8, forward: i8, files: u8) -> u64 {
    let mut mask = 0;
    let mut rank = (square / 8) as i8 + forward;
    while rank >= 0 && rank < 8 {
        mask |= (files as u64) << (rank * 8);
        rank += forward;
    }
    mask
}

/// What stands in a pawn's way, as two masks a square names.
///
/// `front_span` is the pawn's file and the two beside it, on every rank ahead
/// of it. A pawn of ours is passed when no pawn of theirs stands anywhere in
/// it, which is the whole of the standard definition bar one clause.
/// `file_ahead` is the same span without the neighbouring files, and it
/// answers that clause: whether a pawn of ours is already in front of this
/// one. It is kept as its own table rather than masked out of the first at
/// the leaf, because a leaf term pays for every instruction it adds and a
/// table costs half a kilobyte.
///
/// Indexed by `Color`'s discriminant and then the square, the way the
/// shelter's masks are indexed and for the same reason. A white pawn is read
/// up the board and a black one down it.
struct Masks {
    front_span: [[u64; 64]; 2],
    file_ahead: [[u64; 64]; 2],
}

impl Masks {
    /// Built at compile time, the way the shelter's are.
    const fn new() -> Self {
        let mut masks = Masks {
            front_span: [[0; 64]; 2],
            file_ahead: [[0; 64]; 2],
        };
        let mut square = 0u8;
        while square < 64 {
            let i = square as usize;
            let own = 1u8 << (square % 8);
            let beside = pawn_files(square);
            masks.front_span[Color::White as usize][i] = span_ahead(square, 1, beside);
            masks.front_span[Color::Black as usize][i] = span_ahead(square, -1, beside);
            masks.file_ahead[Color::White as usize][i] = span_ahead(square, 1, own);
            masks.file_ahead[Color::Black as usize][i] = span_ahead(square, -1, own);
            square += 1;
        }
        masks
    }
}

static MASKS: Masks = Masks::new();

/// A set of files put back on the board: every square on every file the byte
/// names. What [`files_of`] undoes, and one multiply rather than eight
/// shifts, since a byte times the a file's eight squares lands a copy of the
/// byte on each rank.
const fn spread(files: u8) -> u64 {
    (files as u64) * 0x0101_0101_0101_0101
}

/// Every square strictly in front of one of `pawns`, on that pawn's own file.
///
/// The pawns shifted one rank on and then doubled three times, which carries
/// them the seven ranks a board has. Seeded with the shift rather than with
/// the pawns, so a pawn is never in its own fill, which is what leaves a lone
/// pawn undoubled.
const fn ahead_of(pawns: u64, color: Color) -> u64 {
    match color {
        Color::White => {
            let mut filled = pawns << 8;
            filled |= filled << 8;
            filled |= filled << 16;
            filled |= filled << 32;
            filled
        }
        Color::Black => {
            let mut filled = pawns >> 8;
            filled |= filled >> 8;
            filled |= filled >> 16;
            filled |= filled >> 32;
            filled
        }
    }
}

/// Which files a set of pawns stands on, as a bit per file.
///
/// The board folded in half three times, so the answer is three shifts, three
/// ors and a narrowing rather than eight masked tests. Nothing here says how
/// many pawns a file holds, which is all the counts that ask want to know.
///
/// The shelter reads it too, for the king's files that hold no pawn of ours.
/// It lives here because [`spread`] is its inverse and the pair is easier to
/// check side by side than apart.
pub(super) const fn files_of(pawns: u64) -> u8 {
    let folded = pawns | (pawns >> 32);
    let folded = folded | (folded >> 16);
    let folded = folded | (folded >> 8);
    folded as u8
}

/// What one of those eight counts is worth, as the packed pairs the taper is
/// read from.
///
/// Fitted 2026-09-12 by `scripts/tune.py` over the whole archived strength
/// run: 164 artifacts across 37 runs, 34,175 games, whose 4,428,569 post-book
/// plies gave 4,196,989 positions and 1,922,548 quiet rows, extracted by
/// `arche terms` at 39da0fe. The corpus is sha256 `f9ff326c` and the rows are
/// sha256 `35fd184d`. K was held at 1.1959 and the games split by pair: 10,696
/// pairs trained, 3,638 chose the ridge and 2,750 were sealed. The 768 table
/// entries, the six material values, the eight mobility weights and the
/// fourteen shelter ones were all held where they stand, so these sixteen are
/// the only thing that moved. Every number here is of the rounded vector that
/// ships.
///
/// The selection group scores 0.090580 at zero and 0.089555 at these, a
/// paired difference of -0.001025 against a standard error of 0.000128 over
/// its 7,263 games, at a design factor of 4.8. That is eight standard errors
/// outside its interval, and larger than the king safety fit read on a corpus
/// four fifths this size. By phase it is -0.001274 where six or fewer pieces
/// are left, -0.000671 from seven to twelve, and +0.000135 at thirteen or
/// more: the term pays in the ending, pays a little in the middlegame and
/// costs a little in the opening, which is the shape a pawn structure term
/// should have.
///
/// The sealed group was opened once, after the weights were frozen, over
/// 2,750 pairs and 5,480 games and 312,012 positions that no fit and no
/// ridge choice had read. It scores 0.084236 at zero and 0.083240 at these,
/// a paired difference of -0.000995 against a standard error of 0.000146 at
/// a design factor of 5.0. That is outside its interval, and 0.15 standard
/// errors from the selection group's reading, so the two agree more closely
/// than the king safety fit's two did.
///
/// It also answers the one thing the selection group said against the term.
/// There the opening bucket got worse by 0.000135; on the sealed games it
/// improves by 0.000249, so that was noise rather than a cost. By phase the
/// sealed reading is -0.001107 at six pieces or fewer, -0.000867 from seven
/// to twelve and -0.000249 at thirteen or more, which is the same shape
/// without the sting in its tail.
///
/// The ridge is 1e-8, chosen on the selection group from a grid the zero was
/// taken out of. The grid as it stands includes no regularisation at all and
/// ranked it first, and the vector it produced put 239 on a passed pawn's
/// seventh rank in the midgame and -29 in the ending. `PAWNS_END` already
/// pays a pawn on the seventh 78 to 83, so that says a passed pawn one square
/// from queening is worth less than a blockaded one in the phase that decides
/// it. The seventh rank count carries the fewest rows of the eight (5.31%)
/// and the smallest summed midgame coefficient of all sixteen slots, a
/// twenty seventh of the isolated count's, so it is the direction the corpus
/// constrains least and the one an unregularised fit put its error in.
/// Overruling the grid cost 0.000132 of selection loss against a standard
/// error of 0.00015, and bought a largest weight of 53 rather than 239.
///
/// The selection group did not catch that, which is the part worth knowing:
/// zero scored better there, on games no fit had read. A held-out group
/// detects a vector that has memorised its training games and does not detect
/// one that has learned a real but unrepresentative regularity, and the quiet
/// filter manufactures exactly that wherever a feature's interesting cases
/// are tactical.
///
/// Three of the sixteen are not what the term was built to say, and they are
/// left as the fit gave them rather than tidied. A passed pawn on the seventh
/// is worth less than one on the sixth at both ends, and that survives every
/// ridge on the grid, so it is the corpus and not the regularisation: a
/// position with a passer on the seventh is rarely quiet unless the pawn is
/// blockaded or about to be lost, so the rows that reach the fit are the ones
/// where it is not winning. Passed pawns on the second through the fourth are
/// a midgame penalty. A doubled pawn is worth 8 in the midgame. The isolated
/// count, which carries a coefficient in 56.98% of the rows and has by far
/// the most evidence behind it, reads -11 and -6, which is what a player
/// would have guessed.
///
/// A side's eight pawns can fill the eight counts twenty four times over
/// between them, since one pawn can be passed and isolated and doubled at
/// once, but no count can exceed eight and the six passed counts share the
/// eight between them. So the most this term adds to one half of the packed
/// pair is 776 in the midgame and 440 in the ending, both sides counted,
/// against the 32,767 a half has to stay inside. `bounds_hold` charges eight
/// of every count rather than eight across them, which is looser again and
/// is the screen rather than the arithmetic: it puts the whole vector's
/// boardful at 8,622, of which this term is at most 3,024.
static PAWN_STRUCTURE: [i32; COUNTS] = [
    pack(-18, 18),
    pack(-25, 15),
    pack(-22, 28),
    pack(6, 30),
    pack(53, 39),
    pack(46, 13),
    pack(-11, -6),
    pack(8, -10),
];

/// The weight of one of the eight counts, as the packed pair. The tuner's
/// seam asks through [`super::TERMS`], so that a slot names the live weight
/// rather than a copy of it, the way it reads the tables.
pub(crate) fn weight(index: usize) -> i32 {
    PAWN_STRUCTURE[index]
}

/// What this side's pawns stand as, in the eight counts [`COUNTS`] names: its
/// passed pawns by relative rank, the second through the seventh, then its
/// isolated pawns and its doubled ones.
///
/// A pawn of ours is passed when no pawn of theirs stands on its file or
/// either file beside it on any rank ahead of it, and no pawn of ours
/// stands ahead of it on its own file. The second clause is what leaves
/// the rear of a doubled pair out: the front pawn is the runner, and the
/// one behind it is going nowhere the front one has not gone first. What
/// stands on the square in front of the pawn is not read, so a passer a
/// knight has blockaded is counted as a passer. That is on purpose and it
/// is the first thing this term leaves out: the stop square reads the
/// pieces, and a term that reads the pieces cannot sit behind a key over
/// the pawns.
///
/// A pawn is isolated when no pawn of ours stands on either file beside
/// it, and doubled when a pawn of ours stands behind it on its own file.
/// Both are counted per pawn rather than per file, so an isolated pair on
/// one file pays the isolated weight twice and a tripled file is doubled
/// two. Per pawn is what one coefficient can state; per file would want a
/// second table to say how many.
///
/// Relative rank is the rank a pawn has come, so a white pawn's is its
/// rank and a black pawn's is nine less. The relative second is a real
/// bucket and not a rounding of the others: a pawn still at home is
/// passed the moment the enemy pawns on its three files are gone.
///
/// Pawns and nothing else is read here, which is the property the pawn
/// hash rests on. Neither king, no piece and not the side to move: two
/// positions whose pawns agree agree on all eight counts, and
/// `Board::pawn_key` already stands for that agreement.
///
/// Both the evaluation and the tuner's walk read this, so the identity
/// between them cannot see a wrong count here, at the fitted weights or at
/// zero. What pins it is the hand counts in the tests below, the way the
/// shelter counts and the mobility counts are pinned.
#[inline]
pub(crate) fn counts_of(board: &Board, color: Color) -> [i32; COUNTS] {
    let masks = &MASKS;
    let side = color as usize;
    let (ours, theirs) = board.sides(color);
    let pawns = board.pawns();
    let (our_pawns, their_pawns) = (pawns & ours, pawns & theirs);
    let mut counts = [0; COUNTS];
    let mut remaining = our_pawns;
    while remaining != 0 {
        let square = remaining.trailing_zeros() as usize;
        remaining &= remaining - 1;
        let relative = match color {
            Color::White => square / 8,
            Color::Black => 7 - square / 8,
        };
        // from_fen accepts a pawn on either back rank, knowingly, and the
        // search has to survive one. Such a pawn has come no ranks or all
        // eight and so names none of the six counted; it is left out of
        // the passed count rather than folded into the nearest bucket,
        // and it still counts toward the two below, which read its file
        // and not its rank
        if !(1..PASSED_RANKS + 1).contains(&relative) {
            continue;
        }
        if their_pawns & masks.front_span[side][square] == 0
            && our_pawns & masks.file_ahead[side][square] == 0
        {
            counts[relative - 1] += 1;
        }
    }
    let files = files_of(our_pawns);
    let beside = (files << 1) | (files >> 1);
    counts[PASSED_RANKS] = (our_pawns & spread(files & !beside)).count_ones() as i32;
    counts[PASSED_RANKS + 1] = (our_pawns & ahead_of(our_pawns, color)).count_ones() as i32;
    counts
}

/// The eight counts, written into `into`, which is what [`super::TERMS`] hands
/// the tuner's walk.
pub(crate) fn counts(board: &Board, color: Color, into: &mut [i32]) {
    into.copy_from_slice(&counts_of(board, color));
}

/// What white's pawn structure stands ahead by, as a packed pair on the scale
/// the piece square pair is on.
#[inline]
pub(crate) fn fold(board: &Board) -> i32 {
    fold_with(board, &PAWN_STRUCTURE)
}

/// The same fold against weights named by the caller.
///
/// The live weights are the fit's now, and no two of the eight pairs agree,
/// so the sign of this term, the order of the eight counts and the packing
/// all show in an evaluation the engine prints. The tests still supply weights
/// of their own, because what they pin is the fold rather than the fit: a
/// permuted [`PAWN_STRUCTURE`] would be a different evaluation and not a wrong
/// one.
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

/// How many pawn structure scores the cache holds. A power of two, so the
/// index is a mask rather than a remainder.
///
/// Four thousand entries at sixteen bytes is sixty four kilobytes, half what
/// the shelter's table takes. The size was measured rather than reasoned
/// about, the way the shelter's bits were, and the measurement was retaken
/// on the fitted tree when the weights arrived, since a different tree is a
/// different working set. Callgrind over the bench with the cache simulated,
/// at eleven, twelve, thirteen and fourteen bits, reads 4,098,418,313,
/// 4,092,752,759, 4,089,023,904 and 4,086,576,985 instructions against last
/// level misses of 287,010, 287,113, 290,125 and 294,220. Each bit buys
/// fewer instructions than the one before it, and the misses are flat from
/// eleven to twelve and then turn up by three thousand at thirteen and four
/// thousand more at fourteen. So twelve is the last size the memory does not
/// notice, and the whole range is inside three tenths of a percent of
/// instructions: the constant is not load bearing and a later working set
/// can move it. The sweep at zero weights, before the fit, put the turn in
/// the same place.
///
/// Half the shelter's table is what this term's key predicted before either
/// sweep was run. The same pawns under two different pairs of king squares
/// are two entries there and one entry here, so the working set behind this
/// key is the smaller of the two.
///
/// The bench counts the same 4,066,438 nodes at all four sizes, which is
/// what says the cache changes how a score is arrived at and not what it is.
const CACHE_BITS: usize = 12;
pub(super) const CACHE_SLOTS: usize = 1 << CACHE_BITS;

/// One remembered pawn structure score, under the key that decides it. The
/// whole key, for the reason the shelter's entry keeps the whole of its own.
#[derive(Copy, Clone)]
struct Entry {
    key: u64,
    packed: i32,
}

/// The pawn structure, remembered by what it depends on.
///
/// The term reads the two pawn boards and nothing else, so its key is
/// `Board::pawn_key` with nothing folded in. That is the difference between
/// this cache and the shelter's, and it is the whole reason this term could
/// be cached in the commit that introduced it: the shelter's key carries both
/// kings and so misses on every king move, and this one does not. What misses
/// here is a pawn move and the capture of a pawn, which is every way the two
/// pawn boards change and nothing else.
///
/// Direct mapped and never cleared, on the terms the shelter's cache sets
/// out. An empty entry is key zero holding zero, and here that is exact
/// rather than a coincidence to be argued about: a board with no pawns has a
/// pawn key of zero, which `a_board_with_no_pawns_has_no_key` pins, and the
/// structure of no pawns is eight zero counts. The one position that reads an
/// empty entry as its own reads the right answer from it.
pub(super) struct Cache {
    entries: Box<[Entry]>,
}

impl Default for Cache {
    fn default() -> Self {
        Cache {
            entries: vec![Entry { key: 0, packed: 0 }; CACHE_SLOTS].into_boxed_slice(),
        }
    }
}

impl Cache {
    /// What white's pawn structure stands ahead by, remembered or computed.
    #[inline]
    pub(super) fn get(&mut self, board: &Board) -> i32 {
        let key = board.pawn_key;
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
        Board, CACHE_SLOTS, COUNTS, Cache, Color, MASKS, ahead_of, counts_of, files_of, fold,
        pawn_files, spread,
    };
    use crate::board::fens;
    use crate::psqt::{eg_value, mg_value, pack};
    use pretty_assertions::assert_eq;

    /// A remembered score is read back rather than recomputed, under the pawn
    /// key rather than that key with the two kings folded in.
    ///
    /// What this reads is the key as much as the score: an entry written
    /// under the position's pawn key is the entry the next position with
    /// those pawns is handed. The score is now the fit's rather than zero, so
    /// the entry holding it says something too.
    #[test]
    fn a_pawn_structure_is_remembered_under_the_pawn_key() {
        let board = Board::from_fen(fens::MIDDLEGAME).unwrap();
        let mut cache = Cache::default();
        let key = board.pawn_key;
        assert_ne!(key, 0, "an empty entry would answer this one correctly");
        let slot = (key as usize) & (CACHE_SLOTS - 1);
        assert_eq!(cache.entries[slot].key, 0, "the slot starts empty");
        let first = cache.get(&board);
        assert_eq!(cache.entries[slot].key, key);
        assert_eq!(cache.entries[slot].packed, fold(&board));
        assert_eq!(cache.get(&board), first);
    }

    /// The counts by hand, position by position, because nothing else pins
    /// them. The tuner's identity folds a row against the live weights, and
    /// `eval` and the tuner's walk read this same helper, so the two sides of
    /// the identity move together whatever it answers. These cases are the
    /// only check this term has.
    ///
    /// Each case names the eight counts in the order the helper returns them:
    /// the passed pawns on the relative second through the relative seventh,
    /// then the isolated pawns, then the doubled ones. The other king stands
    /// out of the way.
    #[test]
    fn pawns_count_as_a_hand_count_says_they_do() {
        for (fen, counts, why) in [
            // a pawn with nothing in front of it anywhere is passed, and a
            // pawn with no pawn beside it is isolated. The lone pawn is both
            (
                "4k3/8/8/8/4P3/8/8/4K3 w - - 0 1",
                [0, 0, 1, 0, 0, 0, 1, 0],
                "a white pawn on e4 and no black pawn",
            ),
            // an enemy pawn on an adjacent file ahead of it stops it
            (
                "4k3/8/8/3p4/4P3/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 1, 0],
                "the same against a black pawn on d5",
            ),
            // level is not ahead. A black pawn beside the white one has
            // already been passed
            (
                "4k3/8/8/8/3pP3/8/8/4K3 w - - 0 1",
                [0, 0, 1, 0, 0, 0, 1, 0],
                "the same against a black pawn on d4",
            ),
            // the whole file ahead is read and not the next rank or two
            (
                "4k3/5p2/8/8/4P3/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 1, 0],
                "the same against a black pawn on f7",
            ),
            // a pawn of ours in front of it stops it too, which is what
            // leaves the rear of a doubled pair out of the passed count
            (
                "4k3/8/8/4P3/4P3/8/8/4K3 w - - 0 1",
                [0, 0, 0, 1, 0, 0, 2, 1],
                "white pawns on e4 and e5",
            ),
            // a tripled file is two doubled pawns and not one or three
            (
                "4k3/8/8/8/4P3/4P3/4P3/4K3 w - - 0 1",
                [0, 0, 1, 0, 0, 0, 3, 2],
                "white pawns on e2, e3 and e4",
            ),
            // the relative second is a real bucket. A pawn still at home is
            // passed once the enemy pawns on its three files are gone
            (
                "4k3/8/8/8/8/8/4P3/4K3 w - - 0 1",
                [1, 0, 0, 0, 0, 0, 1, 0],
                "a white pawn on e2 with no black pawn",
            ),
            (
                "4k3/4P3/8/8/8/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 1, 1, 0],
                "a white pawn on e7",
            ),
            // the stop square is not read, so a blockaded passer is a passer.
            // On purpose: the stop square is the first thing this term leaves
            // out, and a later arm has to be able to find the place it goes
            (
                "4k3/4n3/4P3/8/8/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 1, 0, 1, 0],
                "a white pawn on e6 behind a black knight on e7",
            ),
            // a pawn two files away is no company
            (
                "4k3/8/8/8/8/8/P1P5/4K3 w - - 0 1",
                [2, 0, 0, 0, 0, 0, 2, 0],
                "white pawns on a2 and c2",
            ),
            (
                "4k3/8/8/8/8/8/PP6/4K3 w - - 0 1",
                [2, 0, 0, 0, 0, 0, 0, 0],
                "white pawns on a2 and b2",
            ),
            // the two edge files at once. The file mask is a shift and not a
            // rotate, so the a file has no neighbour off the left of the byte
            // and the h file none off the right; a rotate would make each of
            // these the other's neighbour and leave both counted as company
            (
                "4k3/8/8/8/8/8/P6P/4K3 w - - 0 1",
                [2, 0, 0, 0, 0, 0, 2, 0],
                "white pawns on a2 and h2",
            ),
            // the edge cases the file mask decides. A pawn on the a file is
            // read against the a and b files and not against the c file, so
            // stepping the mask in the way the shelter's `king_files` steps
            // it would let the pawn on c5 stop this one
            (
                "4k3/8/8/1p6/P7/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 1, 0],
                "a white pawn on a4 against a black pawn on b5",
            ),
            (
                "4k3/8/8/2p5/P7/8/8/4K3 w - - 0 1",
                [0, 0, 1, 0, 0, 0, 1, 0],
                "a white pawn on a4 against a black pawn on c5",
            ),
            (
                "4k3/8/8/6p1/7P/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 1, 0],
                "a white pawn on h4 against a black pawn on g5",
            ),
            // from_fen accepts a pawn on either back rank and the search has
            // to survive one. It has come no ranks or all eight, so it names
            // none of the six passed buckets, and it still counts toward the
            // two that read its file
            (
                "4k3/8/8/8/8/8/8/3K1P2 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 1, 0],
                "a white pawn on f1",
            ),
            (
                "4k1P1/8/8/8/8/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 1, 0],
                "a white pawn on g8",
            ),
        ] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(counts_of(&board, Color::White), counts, "{}", why);
        }
    }

    /// The same reading for black, whose pawns are measured down the board
    /// rather than up it. Each case is the reflection of one above, so a
    /// relative rank read the wrong way up shows as a count in the wrong
    /// bucket rather than as no count at all.
    #[test]
    fn a_black_pawn_is_measured_down_the_board() {
        for (fen, counts, why) in [
            (
                "4k3/8/8/4p3/8/8/8/4K3 w - - 0 1",
                [0, 0, 1, 0, 0, 0, 1, 0],
                "a black pawn on e5 and no white pawn",
            ),
            (
                "4k3/8/8/4p3/3P4/8/8/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 0, 1, 0],
                "the same against a white pawn on d4",
            ),
            (
                "4k3/8/8/4p3/4p3/8/8/4K3 w - - 0 1",
                [0, 0, 0, 1, 0, 0, 2, 1],
                "black pawns on e4 and e5",
            ),
            (
                "4k3/8/8/8/8/8/4p3/4K3 w - - 0 1",
                [0, 0, 0, 0, 0, 1, 1, 0],
                "a black pawn on e2",
            ),
            (
                "4k3/4p3/8/8/8/8/8/4K3 w - - 0 1",
                [1, 0, 0, 0, 0, 0, 1, 0],
                "a black pawn on e7",
            ),
        ] {
            let board = Board::from_fen(fen).unwrap();
            assert_eq!(counts_of(&board, Color::Black), counts, "{}", why);
        }
    }

    /// Black's count of a position is white's count of its reflection, so the
    /// two colours are read the same way round.
    #[test]
    fn the_two_colours_count_the_same_way() {
        let white = Board::from_fen("4k3/P4p2/8/3P2p1/3P4/PP2p2p/1P6/4K3 w - - 0 1").unwrap();
        let black = Board::from_fen("4k3/1p6/pp2P2P/3p4/3p2P1/8/p4P2/4K3 w - - 0 1").unwrap();
        assert_eq!(
            counts_of(&white, Color::White),
            counts_of(&black, Color::Black)
        );
        assert_eq!(
            counts_of(&white, Color::Black),
            counts_of(&black, Color::White)
        );
    }

    /// Nothing but the pawns decides the counts, which is what [`Cache`] is
    /// keyed on. The same pawns behind different pieces, and with the two
    /// kings somewhere else, count the same.
    #[test]
    fn nothing_but_the_pawns_is_counted() {
        let bare = Board::from_fen("4k3/pp3ppp/8/8/8/8/PPP2PP1/4K3 w - - 0 1").unwrap();
        let full =
            Board::from_fen("r1bq1rk1/pp3ppp/2n5/8/8/2N5/PPP2PP1/R1BQK2R w KQ - 0 1").unwrap();
        for color in [Color::White, Color::Black] {
            assert_eq!(
                counts_of(&bare, color),
                counts_of(&full, color),
                "{:?}",
                color
            );
        }
    }

    /// A pawn's own file and the files beside it, and no more than that. The
    /// edges are the case: two files there and not three, and not three with
    /// the middle one stepped in the way the shelter's `king_files` steps it.
    #[test]
    fn a_pawn_names_its_own_file_and_the_ones_beside_it() {
        for square in 0..64u8 {
            let file = u32::from(square % 8);
            let files = pawn_files(square);
            let expected = if file == 0 || file == 7 { 2 } else { 3 };
            assert_eq!(files.count_ones(), expected, "the files of {}", square);
            assert_eq!(
                files & (1 << file),
                1 << file,
                "not its own file: {}",
                square
            );
            assert_eq!(
                files.trailing_zeros() + expected - 1,
                7 - files.leading_zeros(),
                "the files of {} are not in a row",
                square
            );
        }
    }

    /// Each front span holds every rank ahead of the pawn and no rank level
    /// with it or behind it, on the files the pawn names and no others.
    #[test]
    fn the_front_span_is_the_files_beside_the_pawn_on_the_ranks_ahead() {
        for square in 0..64u8 {
            let rank = i32::from(square / 8);
            for (side, forward) in [(Color::White, 1), (Color::Black, -1)] {
                let span = MASKS.front_span[side as usize][square as usize];
                let ranks = if forward == 1 { 7 - rank } else { rank };
                assert_eq!(
                    span.count_ones(),
                    pawn_files(square).count_ones() * ranks as u32,
                    "{:?} on {}",
                    side,
                    square
                );
                if span != 0 {
                    assert_eq!(
                        files_of(span),
                        pawn_files(square),
                        "{:?} on {} covers other files",
                        side,
                        square
                    );
                }
                for step in 0..8i32 {
                    let on_rank = span & (0xffu64 << (step * 8));
                    let ahead = (step - rank) * forward > 0;
                    assert_eq!(
                        on_rank != 0,
                        ahead,
                        "{:?} on {} holds rank {}",
                        side,
                        square,
                        step
                    );
                }
            }
        }
    }

    /// The file ahead is the front span with the neighbouring files taken
    /// off, which is the relationship the two tables are built to have. Kept
    /// as its own table and not worked out at the leaf, so this is what says
    /// the two agree.
    #[test]
    fn the_file_ahead_is_the_front_span_on_the_pawns_own_file() {
        for square in 0..64u8 {
            let own = spread(1u8 << (square % 8));
            for side in [Color::White, Color::Black] {
                let i = side as usize;
                assert_eq!(
                    MASKS.file_ahead[i][square as usize],
                    MASKS.front_span[i][square as usize] & own,
                    "{:?} on {}",
                    side,
                    square
                );
            }
        }
    }

    /// The forward fill holds every square in front of a pawn and never the
    /// pawn itself, which is what leaves a lone pawn undoubled.
    #[test]
    fn the_fill_starts_one_rank_in_front_of_the_pawn() {
        for square in 0..64u8 {
            let pawn = 1u64 << square;
            for side in [Color::White, Color::Black] {
                let filled = ahead_of(pawn, side);
                assert_eq!(
                    filled & pawn,
                    0,
                    "{:?} on {} is in its own fill",
                    side,
                    square
                );
                assert_eq!(
                    filled, MASKS.file_ahead[side as usize][square as usize],
                    "{:?} on {}",
                    side, square
                );
            }
        }
    }

    /// The fold down to a file a bit answers a walk of the squares. What the
    /// shelter borrows from here is pinned at the same time.
    #[test]
    fn the_file_fold_answers_a_walk_of_the_squares() {
        for square in 0..64u8 {
            assert_eq!(files_of(1u64 << square), 1 << (square % 8), "{}", square);
        }
        let board = Board::from_fen("k7/8/8/5p2/8/6P1/7P/6K1 w - - 0 1").unwrap();
        let (white, black) = board.sides(Color::White);
        // white's pawns stand on g3 and h2, and black's on f5
        assert_eq!(files_of(board.pawns() & white), 0b1100_0000);
        assert_eq!(files_of(board.pawns() & black), 0b0010_0000);
    }

    /// The position the fold test below is read against, and what each side
    /// counts in it, worked out by hand rather than read back off
    /// [`counts_of`].
    ///
    /// The two sides are kept to files that do not meet, white on a, b and d
    /// and black on e, f, g and h, so no pawn of one colour stands in any
    /// pawn of the other's way and every passer here is passed for a reason
    /// a reader can check in one line.
    ///
    /// White has a7, a3, b3, b2, d5 and d4. a7 is passed on the seventh and
    /// a3 is not, because a7 is ahead of it on the file; b3 is passed on the
    /// third and b2 is not; d5 is passed on the fifth and d4 is not. The d
    /// file has no white pawn beside it, so d5 and d4 are the two isolated
    /// pawns. a7, b3 and d5 each have a pawn of their own behind them on
    /// their file, which is the three doubled.
    ///
    /// Black has f7, g5, e3 and h3, one to a file and all four passed: f7 on
    /// its relative second, g5 on its relative fourth, and e3 and h3 on its
    /// relative sixth. The four files run together, so nothing is isolated,
    /// and no file holds two, so nothing is doubled.
    ///
    /// The eight differences are -1, 1, -1, 1, -2, 1, 2 and 3. None is zero,
    /// so every slot does work in the assertion below. They are not all
    /// distinct, so this alone would not tell the first count from the third;
    /// what tells those apart is
    /// `every_pawn_count_writes_both_ends_of_the_taper` in tune.rs, which
    /// reads the coefficients bucket by bucket.
    const STRUCTURED: &str = "3k4/P4p2/8/3P2p1/3P4/PP2p2p/1P6/6K1 w - - 0 1";
    const WHITE_STRUCTURE: [i32; COUNTS] = [0, 1, 0, 1, 0, 1, 2, 3];
    const BLACK_STRUCTURE: [i32; COUNTS] = [1, 0, 1, 0, 2, 0, 0, 0];

    /// Eight weights that differ from each other at both ends of the taper,
    /// so that a pair read into the wrong count's slot lands on a different
    /// number. The eight differences between the halves are 38, 20, -24, -30,
    /// 36, 25, -26 and -28, which differ from each other too, so a
    /// permutation of either array shows.
    const TRIAL: [i32; COUNTS] = [
        pack(3, 41),
        pack(-7, 13),
        pack(29, 5),
        pack(11, -19),
        pack(17, 53),
        pack(-23, 2),
        pack(-5, -31),
        pack(37, 9),
    ];

    /// What the fold does with weights that are not the shipped ones.
    ///
    /// The shipped weights would do here now that they are not zero. Weights
    /// of this test's own are kept anyway, because the shipped ones are the
    /// fit's and will move again: a pin written against them would have to be
    /// rewritten by every refit, and what it is pinning is the fold. So this
    /// hands the fold eight pairs that differ from each other at both ends
    /// and asserts the packed pair against the arithmetic: white's count less
    /// black's, count by count, each half of the pair summed on its own.
    #[test]
    fn the_pawn_structure_fold_reads_white_less_black_count_by_count() {
        let board = Board::from_fen(STRUCTURED).unwrap();
        assert_eq!(counts_of(&board, Color::White), WHITE_STRUCTURE);
        assert_eq!(counts_of(&board, Color::Black), BLACK_STRUCTURE);
        let midgame: i32 = (0..COUNTS)
            .map(|i| mg_value(TRIAL[i]) * (WHITE_STRUCTURE[i] - BLACK_STRUCTURE[i]))
            .sum();
        let endgame: i32 = (0..COUNTS)
            .map(|i| eg_value(TRIAL[i]) * (WHITE_STRUCTURE[i] - BLACK_STRUCTURE[i]))
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
