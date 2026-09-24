// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The order the search tries moves in: the table's move, the captures
//! `Board::see` prices as winning or even, the killers, the quiet moves by
//! history, and the losing captures last.
//!
//! The list is sorted in two stages. `order` keys the table's move and the
//! captures and leaves the quiet moves in generated order between the two
//! capture bands; `order_quiets` scores and sorts the quiet moves, and the
//! search calls it only when it reaches the first of them. Both sorts are
//! stable and generation order breaks their ties, except among the quiet
//! moves the memories key zero when `SearchConfig::ordering_exploration`
//! draws their order instead. The quiet moves are scored by the memories
//! as they stand when the search reaches them, not when the node was
//! entered, so the node count tests pin the sort, the generation order and
//! when the scoring happens.
//!
//! Two memories carry across nodes: the killers, the quiet moves that cut
//! a node off at each distance from the root, and the history, how often
//! each quiet move has cut off against how often it was tried and did not.
//! The search fills both, and `SearchConfig::move_memory` says whether a
//! node consults them.
//!
//! `the_bands_do_not_overlap` checks the constants. The root's
//! aborted-answer swap is sound only because the table's move sorts first,
//! so nothing may outrank that bonus at the root; the deepening loop says
//! why.

use crate::board::{Board, MOVE_LIST_INLINE, MoveList};
use crate::engine::MAX_PLY;
use crate::misc::{Color, Piece, Score};
use crate::play::Play;
use std::ops::Range;

/// The table's move, ahead of every capture, even one whose swap loses
/// the king's whole price. The root depends on nothing else reaching
/// this; see the module comment.
const TABLE_MOVE_BONUS: i64 = 1_000_000_000_000;
/// Where the winning and even captures start, above the killers, ordered
/// within the band by SEE. An even exchange still opens lines and forces
/// replies, so it ranks with the winners. A losing capture takes no base,
/// and its negative SEE carries it below every quiet move, least losing
/// first.
const WINNING_CAPTURE_BASE: i64 = 20_000_000;
/// The unit a point of SEE is counted in, leaving room under one point
/// for the MVV-LVA tiebreak between captures the swap prices alike.
const SEE_UNIT: i64 = 2_000;
/// The two killers, in the order they are tried. Both sit under the
/// smallest even capture, and above the quiet moves themselves.
const KILLER_BONUS: [i64; 2] = [9_000_000, 8_000_000];
/// The bound `gravitate` holds a history entry to, either way. Nothing
/// halves the table and nothing else ages it. Both bands hold with room:
/// this is three orders under the second killer, so no history score
/// reaches the killers, and a marked down quiet keys at most `HISTORY_MAX`
/// where the least losing capture keys at `SEE_UNIT * 100` less the
/// largest tiebreak, so a quiet stays ahead of every losing capture on
/// the spilled list path. `the_bands_do_not_overlap` checks both.
const HISTORY_MAX: i32 = 8_192;

/// Most valuable victim: what taking each piece is worth.
const VICTIM_SCORES: [i64; 6] = [100, 250, 300, 400, 500, 1000];
/// Least valuable attacker: what taking it with each piece is worth.
const ATTACKER_SCORES: [i64; 6] = [6, 5, 4, 3, 2, 1];

/// How many low bits of a sort key hold the place the move was generated
/// in. Six, since a list wider than the buffer takes the other sort.
const PLACE_BITS: u32 = 6;
/// Those bits on their own.
const PLACE_MASK: i64 = (1 << PLACE_BITS) - 1;
/// The buffer's width has been moved before, and moving it past the place
/// field would reorder moves quietly.
const _: () = assert!(MOVE_LIST_INLINE <= 1 << PLACE_BITS);
/// A second bound, not the same one: a place has to fit the word of bits
/// the sort is handed as well, and widening the place field alone would
/// not widen that.
const _: () = assert!(MOVE_LIST_INLINE <= u64::BITS as usize);

/// What the scratch buffer holds before a sort has written to it: a move
/// no generator produces. Nothing reads it, since the sort fills every
/// slot it goes on to look at.
const NOWHERE: Play = Play {
    from: 0,
    to: 0,
    capture: None,
    promote: None,
    en_passant: false,
    castle: false,
};

/// Cutoffs against tries for each quiet move, by the side that played it
/// and the from and to squares (a butterfly table: the piece is not part
/// of the index, so two pieces that can make the same journey share an
/// entry). Signed, since a move tried more often than it cuts is worth
/// ordering behind one the search knows nothing about.
type History = [[[i32; 64]; 64]; 2];

pub(crate) struct MoveOrdering {
    /// Scratch for the keys, reused by every sort. As a local it was a
    /// five hundred byte memset per list ordered; here only the first
    /// `len` entries are touched. No two uses are alive at once, since a
    /// sort finishes before the search recurses.
    keys: [i64; MOVE_LIST_INLINE],
    /// Where a sorted run is built. Either sort shifts keys and never a
    /// move, so the moves are put in order in one pass at the end, read
    /// from here while the list is written over.
    sorted: [Play; MOVE_LIST_INLINE],
    /// The two most recent quiet cutoffs at each distance from the root.
    /// A killer not legal at the node reading it is simply not in that
    /// node's list.
    killers: [[Option<Play>; 2]; MAX_PLY as usize],
    history: History,
}

impl MoveOrdering {
    pub(crate) fn new() -> Self {
        Self {
            keys: [0; MOVE_LIST_INLINE],
            sorted: [NOWHERE; MOVE_LIST_INLINE],
            killers: [[None; 2]; MAX_PLY as usize],
            history: [[[0; 64]; 64]; 2],
        }
    }

    /// Forget both memories. Each `go` starts with this: the memories
    /// describe the tree being searched now, and the iterations of one
    /// deepening share them.
    pub(crate) fn forget(&mut self) {
        self.killers.fill([None; 2]);
        for side in self.history.iter_mut() {
            for from in side.iter_mut() {
                from.fill(0);
            }
        }
    }

    /// A move that cut a node off, `ply` from the root with `depth` left,
    /// and the moves the node made and searched before it. It becomes this
    /// ply's first killer. Its history entry gains the square of the depth
    /// and every quiet in `tried` loses the same: the maluses are the
    /// denominator a count of cutoffs lacks, without which an entry
    /// rewards a move for being ordered early.
    ///
    /// A capture is dropped, cutting or tried, since the swap orders the
    /// captures already. The caller passes the moves it has rather than
    /// sorting the quiets out first.
    pub(crate) fn cutoff<'a>(
        &mut self,
        color: Color,
        m: &Play,
        tried: impl IntoIterator<Item = &'a Play>,
        ply: usize,
        depth: u8,
    ) {
        debug_assert!(ply < MAX_PLY as usize, "no killers past the rail");
        if m.capture.is_some() {
            return;
        }
        let killers = &mut self.killers[ply];
        // the old first killer shifts down unless the move is already it,
        // which keeps one move out of both slots
        if killers[0] != Some(*m) {
            killers[1] = killers[0];
            killers[0] = Some(*m);
        }
        // d², held to the bound: a step wider than the bound would carry
        // an entry past it. Only the rail plus a chain of check extensions
        // reaches such a depth, but both bands rest on the bound holding
        let bonus = (i32::from(depth) * i32::from(depth)).min(HISTORY_MAX);
        for t in tried {
            if t.capture.is_none() {
                gravitate(
                    &mut self.history[color as usize][t.from as usize][t.to as usize],
                    -bonus,
                );
            }
        }
        gravitate(
            &mut self.history[color as usize][m.from as usize][m.to as usize],
            bonus,
        );
    }

    /// Test-only. Signed, so a side taught nothing and a side whose
    /// maluses outweigh its bonuses are told apart.
    #[cfg(test)]
    pub(crate) fn history_total(&self, color: Color) -> i64 {
        self.history[color as usize]
            .iter()
            .flatten()
            .map(|&e| i64::from(e))
            .sum()
    }

    /// Test-only: the side's entries below zero, which only a malus can
    /// produce, so this says the tried moves reached the table at all.
    #[cfg(test)]
    pub(crate) fn history_marked_down(&self, color: Color) -> usize {
        self.history[color as usize]
            .iter()
            .flatten()
            .filter(|&&e| e < 0)
            .count()
    }

    /// The killers standing at a ply. A hot path and not only an
    /// instrument's: the late move decision reads it, since whether the
    /// move is a killer is a feature of the attention score. The cutoff
    /// census and the tests read it too.
    pub(crate) fn killers_at(&self, ply: usize) -> [Option<Play>; 2] {
        self.killers[ply]
    }

    /// The score `order_quiets` would rank one of `color`'s moves by, read
    /// without teaching anything. A feature of the attention score in the
    /// late move decision, and a census column.
    pub(crate) fn history_score(&self, color: Color, m: &Play) -> i32 {
        self.history[color as usize][m.from as usize][m.to as usize]
    }

    /// The first stage: the table's move, if there is one, then the
    /// winning and even captures, sorted, at the front of the list; the
    /// quiet moves behind them in generated order; the losing captures,
    /// sorted, at the end. Returns how many moves the front holds, and the
    /// search calls `order_quiets` when it reaches the first move past
    /// them, so a node the front cuts off never scores a quiet move.
    ///
    /// The table's move keeps its bonus even when the search already
    /// played it without generating and skips it here: one it declined to
    /// play early is still in this list and still has to be tried first.
    ///
    /// `ply` is the node's distance from the root when the quiet memories
    /// are consulted, and none when they are not (quiescence, the root,
    /// and every node under a configuration with `move_memory` off). The
    /// count is the same either way, since a quiet move scores zero here.
    ///
    /// Quiescence reads the count for its losing capture skip, which needs
    /// every capture from that index on to be one the swap priced as
    /// losing. The stack sort's count also has no losing capture before
    /// it, the table's move aside. A list that spilled the buffer is
    /// ordered whole, memories included, and its length comes back, which
    /// the skip reads as nothing to skip; quiescence orders captures and
    /// evasions, and neither list gets that long.
    pub(crate) fn order(
        &mut self,
        board: &Board,
        moves: &mut MoveList,
        table_move: Option<Play>,
        ply: Option<usize>,
    ) -> usize {
        let keys = &mut self.keys;
        let sorted = &mut self.sorted;
        // most lists are short (quiescence lists average under nine
        // moves), and sort_by_cached_key allocates scratch on every call,
        // which at that size costs more than the sorting. The allocating
        // sort is kept only for a list that spilled the buffer, which is
        // ordered whole with the memories read here
        if moves.len() > MOVE_LIST_INLINE {
            let quiet = ply.map(|ply| Quiet {
                killers: self.killers[ply],
                history: &self.history[board.active_color as usize],
            });
            moves.sort_by_cached_key(|m| ordering_key(board, m, table_move, quiet.as_ref()));
            return moves.len();
        }
        // a quiet move keys zero, between the front (negative keys) and
        // the losing captures (positive), and the stable sort keeps the
        // quiet moves in generated order for the second stage. The front
        // is counted as the keys are written
        let mut front = 0;
        let mut scored = 0;
        let mut plain = 0;
        for (i, m) in moves.iter().enumerate() {
            let key = if m.capture.is_some() || table_move == Some(*m) {
                ordering_key(board, m, table_move, None)
            } else {
                0
            };
            // a key of zero is handed to the sort as a bit rather than a
            // key
            if key == 0 {
                plain |= 1 << i;
            } else {
                front += usize::from(key < 0);
                keys[scored] = pack(key, i);
                scored += 1;
            }
        }
        sort_on_the_stack(moves, &mut keys[..scored], plain, front, sorted);
        front
    }

    /// The second stage: `rest` starts at the first move past the front,
    /// and the quiet moves run from there to the first losing capture.
    /// They are scored by the memories as they stand now, killers first
    /// and the rest by history, and sorted in place; the losing captures
    /// behind them are already in order. A move the history has marked
    /// down goes behind the quiets nothing is known about and still ahead
    /// of every losing capture.
    ///
    /// `explore` is the node's draw from `exploration_draw`, or none, which
    /// leaves the quiets nothing is known about in generation order. With
    /// one they are put in the order their tags give, and nothing else in
    /// the band moves.
    ///
    /// Returns where in `rest` those quiets stand, which the ordering
    /// instrument reads: a range and not a copy, since the moves do not move
    /// again before the node answers.
    pub(crate) fn order_quiets(
        &mut self,
        board: &Board,
        rest: &mut [Play],
        ply: usize,
        explore: Option<u64>,
    ) -> Range<usize> {
        debug_assert!(ply < MAX_PLY as usize, "no killers past the rail");
        // `order` hands back the whole length of a list that spilled, so
        // what reaches here fits the buffer, which the sort below indexes
        // the key buffer by
        debug_assert!(
            rest.len() <= MOVE_LIST_INLINE,
            "the run has to fit the buffer"
        );
        let run = rest.iter().take_while(|m| m.capture.is_none()).count();
        let quiets = &mut rest[..run];
        let quiet = Quiet {
            killers: self.killers[ply],
            history: &self.history[board.active_color as usize],
        };
        let keys = &mut self.keys;
        let sorted = &mut self.sorted;
        // a move the memories say nothing about keys zero, handed to the
        // sort as a place and not as a key
        let mut front = 0;
        let mut scored = 0;
        let mut plain = 0;
        for (i, m) in quiets.iter().enumerate() {
            let key = -quiet.bonus(m);
            if key == 0 {
                plain |= 1 << i;
            } else {
                // a marked down move keys positive, and the count is what
                // puts it behind the plain quiets. Reading the front off
                // the sign measured 0.3% faster than handing `scored` over,
                // when a bonus could only be positive
                front += usize::from(key < 0);
                keys[scored] = pack(key, i);
                scored += 1;
            }
        }
        sort_on_the_stack(quiets, &mut keys[..scored], plain, front, sorted);
        // the plain moves now stand together straight behind the front
        let tied = front..front + plain.count_ones() as usize;
        if let Some(draw) = explore {
            shuffle_ties(&mut quiets[tied.clone()], draw, keys, sorted);
        }
        tied
    }
}

/// What a node's tie break is drawn from: the seed, the node and the window
/// it was entered with, and nothing about the run or about the order the
/// tree was reached in, so a seed searches the same tree every time and the
/// pinned counts need no clock. `position_key` alone would give a position
/// the same order at every depth and ply it recurs at.
///
/// The window is there for the node a parent searches twice, a scout and
/// then a wider search. Whether the second visit happens depends on what
/// the first returned, so a second visit that kept the first one's order
/// would try first, more often than one time in `k`, the move that did not
/// settle it the first time.
pub(crate) fn exploration_draw(
    seed: u64,
    position_key: u64,
    depth: u8,
    ply: usize,
    alpha: Score,
    beta: Score,
) -> u64 {
    let node = (u64::from(depth) << 32) | ply as u64;
    let window = (u64::from(alpha as u16) << 16) | u64::from(beta as u16);
    mix(seed ^ mix(position_key ^ mix(node ^ mix(window))))
}

/// A move's tag under a node's draw: its from and to squares and its
/// promotion, the four promotions of one pawn step being four moves.
fn tie_tag(draw: u64, m: &Play) -> u64 {
    let promote = m.promote.map_or(0, |piece| piece as u64 + 1);
    mix(draw ^ (u64::from(m.from) | (u64::from(m.to) << 6) | (promote << 12)))
}

/// The finaliser of splitmix64: every input bit reaches every output bit,
/// so tags drawn from inputs a square apart are unrelated.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Put a run of tied moves in the order of their tags. The tag goes above
/// the place in the key, so the stack sort orders the run and hands it back
/// without moving a move twice; the top bits are dropped to keep the key
/// positive, and two tags alike in the rest fall back to generation order.
fn shuffle_ties(
    tied: &mut [Play],
    draw: u64,
    keys: &mut [i64; MOVE_LIST_INLINE],
    sorted: &mut [Play; MOVE_LIST_INLINE],
) {
    if tied.len() < 2 {
        return;
    }
    for (place, m) in tied.iter().enumerate() {
        keys[place] = pack((tie_tag(draw, m) >> (PLACE_BITS + 1)) as i64, place);
    }
    sort_on_the_stack(tied, &mut keys[..tied.len()], 0, 0, sorted);
}

/// What the memories say at one node: this ply's killers, and the side to
/// move's half of the history, settled once for the node since every move
/// in a list is that side's.
struct Quiet<'a> {
    killers: [Option<Play>; 2],
    history: &'a [[i32; 64]; 64],
}

impl Quiet<'_> {
    /// What a quiet move is worth here: zero for a move nothing is known
    /// about, under zero for one tried more often than it has cut, and
    /// never past the capture bands either side.
    ///
    /// The squares are masked to six bits to take the two bounds checks
    /// off the table read, which this stage makes for every quiet move it
    /// sorts. The mask is measured per site (docs/ROADMAP.md) and pays
    /// here and at no other read of this table: the write in `cutoff` and
    /// the census read keep their check, which panics where a mask reads
    /// a different square.
    #[inline(always)]
    fn bonus(&self, m: &Play) -> i64 {
        if self.killers[0] == Some(*m) {
            return KILLER_BONUS[0];
        }
        if self.killers[1] == Some(*m) {
            return KILLER_BONUS[1];
        }
        i64::from(self.history[(m.from & 63) as usize][(m.to & 63) as usize])
    }
}

/// One update of a history entry: `entry += bonus - entry * |bonus| / MAX`.
/// The step shrinks as the entry nears the bound, so it never passes
/// `HISTORY_MAX` either way and rests at the bound times the net share of
/// its updates that were cutoffs (cutoffs less the rest, over the whole):
/// half and half rests near zero. That share makes the entry a rate
/// rather than a count, so a move tried a hundred times and cutting ten
/// settles where one tried ten and cutting one does. The same step ages
/// the table: an old cutoff is worn away by the updates after it, which is
/// why nothing halves the table.
#[inline]
fn gravitate(entry: &mut i32, bonus: i32) {
    debug_assert!(
        bonus.abs() <= HISTORY_MAX,
        "a step wider than the bound would carry an entry past it"
    );
    *entry += bonus - *entry * bonus.abs() / HISTORY_MAX;
}

/// What a move sorts by, smaller first: a capture by the swap, a quiet
/// move by the memories, and the table's move ahead of everything, negated
/// so that the best score is the smallest key.
#[inline(always)]
fn ordering_key(
    board: &Board,
    m: &Play,
    table_move: Option<Play>,
    quiet: Option<&Quiet<'_>>,
) -> i64 {
    // one look at the capture field rather than two
    let mut score = match m.capture {
        Some(victim) => capture_score(board, m, victim),
        None => match quiet {
            Some(quiet) => quiet.bonus(m),
            None => 0,
        },
    };
    if table_move == Some(*m) {
        score += TABLE_MOVE_BONUS;
    }
    -score
}

/// Where a capture sorts: the swap's sign picks the band, the SEE value
/// orders within it, and MVV-LVA breaks ties between captures the swap
/// prices alike. Only moves with a victim get here, so a quiet move never
/// pays for a swap.
#[inline]
fn capture_score(board: &Board, m: &Play, victim: Piece) -> i64 {
    let see = i64::from(board.see(m));
    let score = see * SEE_UNIT + mvv_lva(board, m, victim);
    if see >= 0 {
        WINNING_CAPTURE_BASE + score
    } else {
        score
    }
}

/// Most valuable victim, least valuable attacker: take the biggest piece
/// with the smallest one first. The scores index by piece rather than
/// matching on it: a match compiled to an indirect jump per capture
/// scored, and the pieces arrive in no order a predictor can learn.
#[inline]
fn mvv_lva(board: &Board, m: &Play, victim: Piece) -> i64 {
    let Some(attacker) = board.get_piece_index(m.from) else {
        return 0;
    };
    VICTIM_SCORES[victim as usize] + ATTACKER_SCORES[attacker as usize]
}

/// A key with the place its move was generated in under it. Two moves of
/// equal worth then differ in these bits alone, so comparing packed keys
/// settles the tie the way a stable sort does, and the sort can carry the
/// moves without touching them. The sign survives the packing.
#[inline(always)]
fn pack(key: i64, place: usize) -> i64 {
    debug_assert!(place < 1 << PLACE_BITS, "a place has to fit its field");
    (key << PLACE_BITS) | place as i64
}

/// What sort_by_cached_key does, minus its allocation, for a run that fits
/// the buffer: a stable insertion sort over keys the caller computed once
/// each. `agrees_with_the_stable_sort_it_replaces` holds it to the library
/// sort's order, so the node count tests pin the pair. The keys arrive in
/// a buffer rather than as a closure, which was a call per move rather
/// than code in this loop.
///
/// Only the keys that are not zero are sorted. Five moves in six key zero
/// at either stage, and a stable sort leaves them in generated order
/// between the negative keys and the positive ones, so the caller marks
/// their places in `plain` and the sorted keys go either side of them.
/// Sorting the whole list carried each scored key back past every move
/// generated before it, which was most of the sort's cost: two keys a
/// call in the first stage and four in the second, against fifteen moves.
///
/// `front` is how many of the keys are negative: the band that goes
/// first, the positive ones being the band that goes last.
///
/// Only the keys are shifted. A move is six bytes and a key eight, and
/// shifting both cost four times shifting the key alone; the place packed
/// into each key says which move it belongs to.
#[inline]
fn sort_on_the_stack(
    moves: &mut [Play],
    keys: &mut [i64],
    plain: u64,
    front: usize,
    sorted: &mut [Play; MOVE_LIST_INLINE],
) {
    let len = moves.len();
    debug_assert!(len <= MOVE_LIST_INLINE);
    debug_assert!(
        keys.len() + plain.count_ones() as usize == len,
        "every move is either keyed or marked plain, and not both"
    );
    debug_assert!(front <= keys.len(), "the front is part of the keys");
    // a list with nothing keyed is already in order, and one with a single
    // key is a rotation: the keyed move goes to the head or to the foot
    // and every other move keeps its place in the run. Between them the
    // two are two lists in five at the first stage and one in four at the
    // second, and both cost the whole copy below for nothing
    match keys.len() {
        0 => return,
        1 => {
            let place = (keys[0] & PLACE_MASK) as usize;
            if front == 1 {
                moves[..=place].rotate_right(1);
            } else {
                moves[place..].rotate_left(1);
            }
            return;
        }
        _ => {}
    }
    for i in 1..keys.len() {
        let k = keys[i];
        let mut j = i;
        while j > 0 && keys[j - 1] > k {
            keys[j] = keys[j - 1];
            j -= 1;
        }
        keys[j] = k;
    }
    sorted[..len].copy_from_slice(moves);
    for (slot, key) in moves[..front].iter_mut().zip(&keys[..front]) {
        *slot = sorted[(key & PLACE_MASK) as usize];
    }
    // the places the caller passed over, lowest first, in generated
    // order. They come in runs (under two a list, eight places long on
    // average, since a generator emits a piece's quiet moves together),
    // and a run is copied whole rather than a six byte Play at a time
    let mut out = front;
    let mut rest = plain;
    while rest != 0 {
        let start = rest.trailing_zeros() as usize;
        // adding one at the run's foot carries through it and stops at
        // the first keyed place above it
        let past = rest & rest.wrapping_add(1 << start);
        let run = (rest ^ past).count_ones() as usize;
        moves[out..out + run].copy_from_slice(&sorted[start..start + run]);
        out += run;
        rest = past;
    }
    for (slot, key) in moves[out..].iter_mut().zip(&keys[front..]) {
        *slot = sorted[(key & PLACE_MASK) as usize];
    }
}

#[cfg(test)]
mod order {
    use super::{MoveOrdering, Quiet};
    use crate::board::Board;
    use crate::misc::Color;
    use crate::play::Play;

    // white to move: the pawn on e4 and the queen on h5 can both take the
    // queen on d5, and the queen can take the pawn on h7, which its king
    // defends
    const CAPTURES: &str = "rn5k/7p/8/3q3Q/4P3/8/8/6K1 w - - 0 1";

    // the two hundred move position with a knight added on c3, so that
    // taking the pawn on a2 loses (against the king alone the swap knows
    // a defended piece cannot be taken back). Long enough to spill the
    // buffer, which is the path where one sort orders the whole list
    const CROWDED: &str = "R6R/3Q4/1Q4Q1/4Q3/2Q4Q/Q1n2Q2/pp1Q4/kBNN1KB1 w - - 0 1";

    fn ordered(fen: &str, table_move: Option<Play>) -> Vec<Play> {
        ordered_by(fen, table_move, &mut MoveOrdering::new())
    }

    /// The same, ordered by an ordering the test has already taught
    /// something, and read at the ply that ordering was taught at.
    fn ordered_by(fen: &str, table_move: Option<Play>, ordering: &mut MoveOrdering) -> Vec<Play> {
        let board = Board::from_fen(fen).unwrap();
        let mut moves = board.generate_moves();
        let front = ordering.order(&board, &mut moves, table_move, Some(0));
        ordering.order_quiets(&board, &mut moves[front..], 0, None);
        moves.to_vec()
    }

    fn position_of(moves: &[Play], name: &str) -> usize {
        moves
            .iter()
            .position(|m| m.to_string() == name)
            .unwrap_or_else(|| panic!("{name} was not generated"))
    }

    fn named(moves: &[Play], name: &str) -> Play {
        moves[position_of(moves, name)]
    }

    fn quiets(moves: &[Play]) -> Vec<Play> {
        moves
            .iter()
            .filter(|m| m.capture.is_none())
            .copied()
            .collect()
    }

    // both takings of the queen win her clean, so their SEE agrees and
    // MVV-LVA breaks the tie: the smaller attacker first
    #[test]
    fn the_biggest_victim_and_the_smallest_attacker_go_first() {
        let moves = ordered(CAPTURES, None);
        assert_eq!(position_of(&moves, "e4d5"), 0);
        assert_eq!(position_of(&moves, "h5d5"), 1);
    }

    // the queen takes a pawn its king defends, which the swap prices at a
    // pawn for a queen: behind every quiet move, history or none
    #[test]
    fn a_losing_capture_waits_behind_the_quiet_moves() {
        let board = Board::from_fen(CAPTURES).unwrap();
        let taught = named(&board.generate_moves(), "g1f1");
        // taught at another ply, so it sorts by history and not as a killer
        let mut ordering = MoveOrdering::new();
        ordering.cutoff(Color::White, &taught, &[], 1, 4);

        let moves = ordered_by(CAPTURES, None, &mut ordering);
        let losing = position_of(&moves, "h5h7");
        assert!(losing > position_of(&moves, "g1f1"));
        for quiet in quiets(&moves) {
            assert!(losing > position_of(&moves, &quiet.to_string()));
        }
    }

    // a rook for a rook: an even swap ranks with the winning captures,
    // above a killer, not with the losing ones
    #[test]
    fn an_even_capture_ranks_with_the_winners() {
        const ROOKS: &str = "3rr2k/8/8/8/8/8/8/4R2K w - - 0 1";
        let board = Board::from_fen(ROOKS).unwrap();
        let killer = named(&board.generate_moves(), "h1g1");
        let mut ordering = MoveOrdering::new();
        ordering.cutoff(Color::White, &killer, &[], 0, 4);

        let moves = ordered_by(ROOKS, None, &mut ordering);
        assert!(position_of(&moves, "e1e8") < position_of(&moves, "h1g1"));
    }

    // the two takings of the queen, plus a table move whether it is a quiet
    // move or the losing capture itself. The same count with the memories
    // and without, since neither stage scores a capture by them
    #[test]
    fn the_front_is_the_tables_move_and_the_winning_captures() {
        let board = Board::from_fen(CAPTURES).unwrap();
        let generated = board.generate_moves();
        for (table_move, ply) in [
            (None, Some(0)),
            (None, None),
            (Some("e4e5"), Some(0)),
            (Some("h5h7"), Some(0)),
            (Some("h5h7"), None),
        ] {
            let table_move = table_move.map(|name| named(&generated, name));
            let mut moves = generated.clone();
            let front = MoveOrdering::new().order(&board, &mut moves, table_move, ply);
            assert_eq!(front, 2 + usize::from(table_move.is_some()));
            for (i, m) in moves.iter().enumerate() {
                let winning = m.capture.is_some() && board.see(m) >= 0;
                assert_eq!(i < front, winning || table_move == Some(*m), "{m} at {i}");
            }
        }
    }

    // every capture from the count on is one the swap prices as losing,
    // and no capture before it is, the table's move aside. A list that
    // spilled the buffer is ordered whole and its length comes back
    #[test]
    fn the_count_returned_is_where_the_losing_captures_start() {
        for (fen, table_move) in [
            (CAPTURES, None),
            (CAPTURES, Some("h5h7")),
            (CROWDED, None),
            (CROWDED, Some("c1a2")),
        ] {
            let board = Board::from_fen(fen).unwrap();
            let mut moves = board.generate_moves();
            let table_move = table_move.map(|name| named(&moves, name));
            let spilled = moves.len() > crate::board::MOVE_LIST_INLINE;
            assert_eq!(spilled, fen == CROWDED, "{fen}");
            let front = MoveOrdering::new().order(&board, &mut moves, table_move, None);
            if spilled {
                assert_eq!(front, moves.len(), "{fen}");
                continue;
            }
            let captures = moves
                .iter()
                .enumerate()
                .filter(|(_, m)| m.capture.is_some());
            for (i, m) in captures {
                let losing = board.see(m) < 0 && table_move != Some(*m);
                assert_eq!(i >= front, losing, "{fen}: {m} at {i}");
            }
            // the check above was not vacuous: something sorts ahead of the
            // band, and without a table move taking the loser out of it the
            // band is not empty
            assert!(front > 0, "{fen}");
            let band = moves[front..].iter().any(|m| m.capture.is_some());
            assert!(table_move.is_some() || band, "{fen}");
        }
    }

    // a quiet table move, with and without a killer bonus on top: the
    // root's aborted-answer swap rests on nothing outranking the table
    #[test]
    fn the_tables_move_goes_first_whether_or_not_a_killer_names_it() {
        let board = Board::from_fen(CAPTURES).unwrap();
        let push = named(&board.generate_moves(), "e4e5");
        for killer in [false, true] {
            let mut ordering = MoveOrdering::new();
            if killer {
                ordering.cutoff(Color::White, &push, &[], 0, 4);
            }
            let moves = ordered_by(CAPTURES, Some(push), &mut ordering);
            assert_eq!(moves[0], push, "killer {killer}");
            assert_eq!(position_of(&moves, "e4d5"), 1, "killer {killer}");
        }
    }

    #[test]
    fn quiet_moves_keep_the_order_they_were_generated_in() {
        let board = Board::from_fen(CAPTURES).unwrap();
        let generated = quiets(&board.generate_moves());
        let sorted = quiets(&ordered(CAPTURES, None));
        assert_eq!(generated, sorted);
    }

    #[test]
    fn a_killer_sorts_between_the_captures_and_the_other_quiet_moves() {
        let board = Board::from_fen(CAPTURES).unwrap();
        let killer = named(&board.generate_moves(), "e4e5");
        let mut ordering = MoveOrdering::new();
        ordering.cutoff(Color::White, &killer, &[], 0, 4);

        let moves = ordered_by(CAPTURES, None, &mut ordering);
        assert!(position_of(&moves, "e4e5") > position_of(&moves, "e4d5"));
        assert!(position_of(&moves, "e4e5") > position_of(&moves, "h5d5"));
        assert_eq!(quiets(&moves)[0], killer);
        assert!(position_of(&moves, "h5h7") > position_of(&moves, "e4e5"));
    }

    #[test]
    fn the_second_killer_waits_behind_the_first() {
        let board = Board::from_fen(CAPTURES).unwrap();
        let generated = board.generate_moves();
        let first = named(&generated, "e4e5");
        let second = named(&generated, "g1f1");
        let mut ordering = MoveOrdering::new();
        ordering.cutoff(Color::White, &second, &[], 0, 4);
        ordering.cutoff(Color::White, &first, &[], 0, 4);

        let moves = quiets(&ordered_by(CAPTURES, None, &mut ordering));
        assert_eq!(moves[0], first);
        assert_eq!(moves[1], second);
    }

    #[test]
    fn history_orders_the_quiet_moves_no_killer_names() {
        let board = Board::from_fen(CAPTURES).unwrap();
        let generated = quiets(&board.generate_moves());
        let last = *generated.last().expect("the position has quiet moves");
        assert_ne!(generated[0], last);
        // taught at another ply, so this ply's killers are empty and what
        // is left to order the two by is the history alone
        let mut ordering = MoveOrdering::new();
        ordering.cutoff(Color::White, &generated[0], &[], 1, 1);
        ordering.cutoff(Color::White, &last, &[], 1, 4);

        let moves = quiets(&ordered_by(CAPTURES, None, &mut ordering));
        assert_eq!(moves[0], last);
        assert_eq!(moves[1], generated[0]);
    }

    // the move that cut goes first, the moves tried before it go last, and
    // the quiets never asked keep their generated order in between. All of
    // them stay ahead of the losing capture
    #[test]
    fn the_moves_tried_before_a_cutoff_sort_behind_the_ones_it_never_asked() {
        let board = Board::from_fen(CAPTURES).unwrap();
        let generated = quiets(&board.generate_moves());
        let cut = *generated.last().expect("the position has quiet moves");
        let tried = [generated[0], generated[1]];
        let untouched = &generated[2..generated.len() - 1];
        assert!(!untouched.is_empty(), "there is a move to leave alone");

        // taught at another ply, so nothing here stands in a killer slot
        // and the history alone orders the band
        let mut ordering = MoveOrdering::new();
        ordering.cutoff(Color::White, &cut, &tried, 1, 4);

        let moves = ordered_by(CAPTURES, None, &mut ordering);
        let mut expected = vec![cut];
        expected.extend_from_slice(untouched);
        expected.extend_from_slice(&tried);
        assert_eq!(quiets(&moves), expected);
        assert!(position_of(&moves, "h5h7") > position_of(&moves, &tried[1].to_string()));
    }

    // after 1. e4 e5 2. Nf3 Nc6: twenty odd quiet moves and no capture, so
    // the tie group is most of the list
    const OPEN: &str = "r1bqkbnr/pppp1ppp/2n5/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R w KQkq - 2 3";

    fn explored(fen: &str, ordering: &mut MoveOrdering, draw: u64) -> Vec<Play> {
        let board = Board::from_fen(fen).unwrap();
        let mut moves = board.generate_moves();
        let front = ordering.order(&board, &mut moves, None, Some(0));
        ordering.order_quiets(&board, &mut moves[front..], 0, Some(draw));
        quiets(&moves)
    }

    // a killer, a move the history likes and one it has marked down keep
    // their places, and the moves between them are the same moves in
    // another order
    #[test]
    fn exploring_draws_the_order_of_the_ties_and_of_nothing_else() {
        let board = Board::from_fen(OPEN).unwrap();
        let generated = quiets(&board.generate_moves());
        let (killer, liked, marked) = (generated[3], generated[7], generated[11]);
        let mut ordering = MoveOrdering::new();
        // the liked move and the marked one taught at another ply, so they
        // sort by history alone; the killer at this one
        ordering.cutoff(Color::White, &liked, &[marked], 1, 4);
        ordering.cutoff(Color::White, &killer, &[], 0, 1);
        let tied: Vec<Play> = generated
            .iter()
            .filter(|m| ![killer, liked, marked].contains(m))
            .copied()
            .collect();

        let mut moved = false;
        for draw in 0..16 {
            let moves = explored(OPEN, &mut ordering, draw);
            assert_eq!(moves[0], killer, "draw {draw}");
            assert_eq!(moves[1], liked, "draw {draw}");
            assert_eq!(*moves.last().unwrap(), marked, "draw {draw}");
            let group = &moves[2..moves.len() - 1];
            let mut sorted_group = group.to_vec();
            let mut sorted_tied = tied.clone();
            sorted_group.sort_by_key(|m| (m.from, m.to));
            sorted_tied.sort_by_key(|m| (m.from, m.to));
            assert_eq!(sorted_group, sorted_tied, "draw {draw}");
            moved |= group != tied.as_slice();
        }
        assert!(moved, "no draw moved a tie out of generation order");
    }

    // what `order_quiets` hands back is where the quiets keyed zero stand:
    // with nothing taught, with one key at the head (a killer) or at the
    // foot (a marked down move), which the sort does as a rotation, and
    // with keys either side
    #[test]
    fn the_range_returned_is_the_moves_keyed_zero() {
        let board = Board::from_fen(OPEN).unwrap();
        let generated = quiets(&board.generate_moves());
        let (killer, liked, marked) = (generated[3], generated[7], generated[11]);
        let taught: [&dyn Fn(&mut MoveOrdering); 4] = [
            &|_| {},
            &|o| o.cutoff(Color::White, &killer, &[], 0, 1),
            &|o| o.cutoff(Color::White, &generated[0], &[marked], 1, 4),
            &|o| {
                o.cutoff(Color::White, &liked, &[marked], 1, 4);
                o.cutoff(Color::White, &killer, &[], 0, 1);
            },
        ];
        for (case, teach) in taught.iter().enumerate() {
            let mut ordering = MoveOrdering::new();
            teach(&mut ordering);
            let quiet = Quiet {
                killers: ordering.killers[0],
                history: &ordering.history[Color::White as usize],
            };
            let zero: Vec<Play> = generated
                .iter()
                .filter(|m| quiet.bonus(m) == 0)
                .copied()
                .collect();
            for explore in [None, Some(5)] {
                let mut moves = board.generate_moves();
                let front = ordering.order(&board, &mut moves, None, Some(0));
                let tied = ordering.order_quiets(&board, &mut moves[front..], 0, explore);
                let mut inside = moves[front..][tied].to_vec();
                let mut expected = zero.clone();
                inside.sort_by_key(|m| (m.from, m.to));
                expected.sort_by_key(|m| (m.from, m.to));
                assert_eq!(inside, expected, "case {case}, draw {explore:?}");
            }
        }
    }

    #[test]
    fn one_draw_orders_the_ties_the_same_way_every_time() {
        let first = explored(OPEN, &mut MoveOrdering::new(), 7);
        assert_eq!(first, explored(OPEN, &mut MoveOrdering::new(), 7));
        assert_ne!(first, explored(OPEN, &mut MoveOrdering::new(), 8));
    }

    // the draw is the seed, the node and its window, so two seeds differ at
    // a node, and so do two depths, two plies and two windows of one
    // position
    #[test]
    fn a_draw_is_the_seed_the_position_the_depth_the_ply_and_the_window() {
        let draw = super::exploration_draw(1, 0x0123_4567_89ab_cdef, 6, 3, -30, -29);
        assert_eq!(
            draw,
            super::exploration_draw(1, 0x0123_4567_89ab_cdef, 6, 3, -30, -29)
        );
        for other in [
            super::exploration_draw(2, 0x0123_4567_89ab_cdef, 6, 3, -30, -29),
            super::exploration_draw(1, 0x0123_4567_89ab_cdee, 6, 3, -30, -29),
            super::exploration_draw(1, 0x0123_4567_89ab_cdef, 7, 3, -30, -29),
            super::exploration_draw(1, 0x0123_4567_89ab_cdef, 6, 4, -30, -29),
            super::exploration_draw(1, 0x0123_4567_89ab_cdef, 6, 3, -30, 40),
            super::exploration_draw(1, 0x0123_4567_89ab_cdef, 6, 3, -31, -30),
        ] {
            assert_ne!(draw, other);
        }
    }

    // what the instrument's uniformity check reads, asked here of the tags
    // alone: over many draws each member of the group is first about as
    // often as any other
    #[test]
    fn each_tie_is_drawn_first_about_equally_often() {
        const DRAWS: u64 = 20_000;
        let generated = quiets(&Board::from_fen(OPEN).unwrap().generate_moves());
        let k = generated.len();
        let mut first = vec![0u64; k];
        let mut ordering = MoveOrdering::new();
        for seed in 0..DRAWS {
            let draw = super::exploration_draw(seed, 0x5eed, 8, 2, -30, -29);
            let moves = explored(OPEN, &mut ordering, draw);
            first[generated.iter().position(|m| *m == moves[0]).unwrap()] += 1;
        }
        let expected = DRAWS / k as u64;
        for (place, count) in first.iter().enumerate() {
            // five standard deviations of a binomial at this size
            assert!(
                count.abs_diff(expected) < expected / 5,
                "place {place} first {count} times against {expected}"
            );
        }
    }

    // the same band on the spilled list path, where one sort orders the
    // quiets and the losing captures together and a marked down quiet keys
    // positive like a losing capture. What keeps the two apart is the
    // distance between `HISTORY_MAX` and the least losing capture's key
    #[test]
    fn a_marked_down_quiet_stays_ahead_of_the_losing_captures_on_a_spilled_list() {
        let board = Board::from_fen(CROWDED).unwrap();
        let generated = board.generate_moves();
        assert!(
            generated.len() > crate::board::MOVE_LIST_INLINE,
            "the list has to spill, or the second stage sorts the quiets instead"
        );
        let band = quiets(&generated);
        let (cut, marked) = (band[0], band[1]);

        // taught at another ply, so this ply's killers are empty and the
        // history alone orders the quiet moves
        let mut ordering = MoveOrdering::new();
        ordering.cutoff(Color::White, &cut, &[marked], 1, 4);
        assert!(ordering.history_score(Color::White, &marked) < 0);

        let moves = ordered_by(CROWDED, None, &mut ordering);
        let at = |m: &Play| {
            moves
                .iter()
                .position(|x| x == m)
                .unwrap_or_else(|| panic!("{m} was not generated"))
        };
        let losing = moves
            .iter()
            .position(|m| m.capture.is_some() && board.see(m) < 0)
            .expect("the position has a losing capture");
        assert!(at(&marked) < losing, "a marked down quiet fell behind");
        for m in band.iter().filter(|m| **m != cut && **m != marked) {
            assert!(at(&cut) < at(m) && at(m) < at(&marked), "{m}");
        }
    }
}

#[cfg(test)]
mod memory {
    use super::SEE_UNIT;
    use super::{ATTACKER_SCORES, HISTORY_MAX, KILLER_BONUS, MoveOrdering, PLACE_BITS};
    use super::{TABLE_MOVE_BONUS, VICTIM_SCORES, WINNING_CAPTURE_BASE, gravitate};
    use crate::board::SEE_VALUES;
    use crate::engine::MAX_PLY;
    use crate::misc::{Color, Piece};
    use crate::play::Play;

    fn quiet(from: u8, to: u8) -> Play {
        Play::new(from, to, None, None, false, false)
    }

    /// Every tiebreak `mvv_lva` can return, worked out from the tables it
    /// reads so that a table edited later is what this test reads.
    fn tiebreaks() -> Vec<i64> {
        let mut scores = Vec::new();
        for victim in VICTIM_SCORES {
            for attacker in ATTACKER_SCORES {
                scores.push(victim + attacker);
            }
        }
        scores
    }

    #[test]
    fn the_bands_do_not_overlap() {
        let ties = tiebreaks();
        let biggest = *ties.iter().max().expect("the tables are not empty");
        let smallest = *ties.iter().min().expect("the tables are not empty");
        // the tiebreak stays inside one point of SEE and never turns a
        // losing capture positive
        assert!(0 < smallest && biggest < SEE_UNIT);
        assert!(-SEE_UNIT + biggest < 0);

        // the swap never wins more than the first victim, a queen at most,
        // and never loses more than the king's price
        let best_swap = i64::from(SEE_VALUES[Piece::Queen as usize]);
        let worst_swap = -i64::from(SEE_VALUES[Piece::King as usize]);
        let best_capture = WINNING_CAPTURE_BASE + best_swap * SEE_UNIT + biggest;
        let worst_capture = worst_swap * SEE_UNIT + smallest;

        // the table's move ahead of the best capture even when it is itself
        // the worst, which the root's aborted answer swap rests on
        assert!(TABLE_MOVE_BONUS + worst_capture > best_capture);
        // the winning and even captures above the killers, the killers in
        // order above the history
        assert!(WINNING_CAPTURE_BASE + smallest > KILLER_BONUS[0]);
        assert!(KILLER_BONUS[0] > KILLER_BONUS[1]);
        assert!(KILLER_BONUS[1] > i64::from(HISTORY_MAX));

        // the other side of the history's band: a losing capture loses a
        // pawn at least, so the best it can key is that swap with the
        // largest tiebreak on top, and the worst a quiet can key is the
        // whole bound. The quiet has to stay ahead on the spilled list path
        let least_losing = -i64::from(SEE_VALUES[Piece::Pawn as usize]) * SEE_UNIT + biggest;
        assert!(-i64::from(HISTORY_MAX) > least_losing);

        // the widest key there could be has to survive the place shift; the
        // place half of `pack` has a compile time assert of its own
        let widest = TABLE_MOVE_BONUS + best_capture;
        assert!(
            widest.checked_mul(1 << PLACE_BITS).is_some(),
            "the widest key does not survive being packed"
        );
    }

    #[test]
    fn the_killer_slots_hold_the_two_most_recent_cutoffs() {
        let mut ordering = MoveOrdering::new();
        let first = quiet(8, 16);
        let second = quiet(9, 17);
        // a move already in the first slot is not put in both (the second
        // step), and a move promoted back out of the second does not stay
        // in it (the fourth)
        for (m, killers) in [
            (first, [Some(first), None]),
            (first, [Some(first), None]),
            (second, [Some(second), Some(first)]),
            (first, [Some(first), Some(second)]),
        ] {
            ordering.cutoff(Color::White, &m, &[], 3, 4);
            assert_eq!(ordering.killers[3], killers, "after {m}");
        }
        // and the ply is what indexes them
        assert_eq!(ordering.killers[4], [None, None]);
    }

    #[test]
    fn a_capture_cutoff_touches_neither_memory() {
        let mut ordering = MoveOrdering::new();
        let take = Play::new(8, 16, Some(Piece::Pawn), None, false, false);
        let tried = quiet(9, 17);
        ordering.cutoff(Color::White, &take, &[tried], 0, 4);
        assert_eq!(ordering.killers[0], [None, None]);
        assert_eq!(ordering.history[Color::White as usize][8][16], 0);
        // the moves tried before it are not marked down either
        assert_eq!(ordering.history[Color::White as usize][9][17], 0);
    }

    #[test]
    fn a_cutoff_gains_the_square_of_the_depth_and_the_moves_tried_lose_it() {
        let mut ordering = MoveOrdering::new();
        let m = quiet(8, 16);
        let tried = quiet(9, 17);
        let take = Play::new(10, 18, Some(Piece::Pawn), None, false, false);
        ordering.cutoff(Color::White, &m, &[tried, take], 0, 5);
        let history = &ordering.history[Color::White as usize];
        // gravity gives back a share of the entry, which is nothing at
        // zero, so a first update is the step itself
        assert_eq!(history[8][16], 25);
        assert_eq!(history[9][17], -25);
        // a capture among the moves tried is passed over
        assert_eq!(history[10][18], 0);
        // the side that played it is part of the index
        assert_eq!(ordering.history[Color::Black as usize][8][16], 0);
    }

    // the closed form the update is: a step of 64 on an entry standing at
    // half the bound gives half of itself back, whichever way it points
    #[test]
    fn gravity_gives_back_the_entry_s_share_of_the_step() {
        for (step, expected) in [(64, 4128), (-64, 4000)] {
            let mut entry = 4096;
            gravitate(&mut entry, step);
            assert_eq!(entry, expected, "a step of {step}");
        }
    }

    // an entry settles against the bound rather than wrapping or being
    // halved back, and it does so from either direction
    #[test]
    fn ten_thousand_steps_leave_an_entry_inside_the_bound() {
        const DEPTH: u8 = 8;
        let mut ordering = MoveOrdering::new();
        let hot = quiet(8, 16);
        let cold = quiet(9, 17);
        for _ in 0..10_000 {
            ordering.cutoff(Color::White, &hot, &[cold], 0, DEPTH);
        }
        let history = &ordering.history[Color::White as usize];
        assert!(history[8][16] > HISTORY_MAX / 2 && history[8][16] <= HISTORY_MAX);
        assert!(history[9][17] < -HISTORY_MAX / 2 && history[9][17] >= -HISTORY_MAX);
    }

    // a step wider than the bound would carry an entry through it, so d²
    // is held to the bound for any depth `cutoff` is handed
    #[test]
    fn a_depth_whose_square_passes_the_bound_lands_on_it() {
        let mut ordering = MoveOrdering::new();
        let m = quiet(8, 16);
        assert!(i32::from(MAX_PLY) * i32::from(MAX_PLY) > HISTORY_MAX);
        for _ in 0..2 {
            ordering.cutoff(Color::White, &m, &[], 0, MAX_PLY);
            assert_eq!(ordering.history[Color::White as usize][8][16], HISTORY_MAX);
        }
    }

    #[test]
    fn forgetting_empties_both_memories() {
        let mut ordering = MoveOrdering::new();
        let m = quiet(8, 16);
        let tried = quiet(9, 17);
        ordering.cutoff(Color::White, &m, &[tried], 2, 4);
        assert!(ordering.history[Color::White as usize][9][17] < 0);
        ordering.forget();
        assert_eq!(ordering.killers[2], [None, None]);
        assert_eq!(ordering.history[Color::White as usize][8][16], 0);
        // a marked down entry goes with the rest of them
        assert_eq!(ordering.history[Color::White as usize][9][17], 0);
    }
}

#[cfg(test)]
mod stack_sort {
    use super::{
        KILLER_BONUS, MOVE_LIST_INLINE, NOWHERE, SEE_UNIT, TABLE_MOVE_BONUS, WINNING_CAPTURE_BASE,
        pack, sort_on_the_stack,
    };
    use crate::play::Play;
    use proptest::prelude::*;

    /// The stack sort put through the split its two callers make, against
    /// the stable library sort over the same keys. Returns both orders.
    fn both_orders(input: &[i64]) -> (Vec<Play>, Vec<Play>) {
        let moves: Vec<Play> = (0..input.len())
            .map(|i| Play::new(i as u8, 0, None, None, false, false))
            .collect();

        let mut expected: Vec<(i64, Play)> =
            input.iter().copied().zip(moves.iter().copied()).collect();
        expected.sort_by_key(|(key, _)| *key);
        let expected: Vec<Play> = expected.into_iter().map(|(_, m)| m).collect();

        // the split the callers make: a key of zero becomes a bit
        let mut keys = [0i64; MOVE_LIST_INLINE];
        let mut front = 0;
        let mut scored = 0;
        let mut plain = 0;
        for (i, key) in input.iter().enumerate() {
            if *key == 0 {
                plain |= 1 << i;
            } else {
                front += usize::from(*key < 0);
                keys[scored] = pack(*key, i);
                scored += 1;
            }
        }
        let mut scratch = [NOWHERE; MOVE_LIST_INLINE];
        let mut sorted = moves;
        sort_on_the_stack(&mut sorted, &mut keys[..scored], plain, front, &mut scratch);
        (sorted, expected)
    }

    /// A key at the density and the width the search makes them: five in
    /// six zero, which puts long runs through the run copy; small keys for
    /// the ties, which draws from the whole of i64 would never produce;
    /// and the bands' own magnitudes, so a constant that outgrew `pack`'s
    /// shift would show here.
    fn a_key() -> impl Strategy<Value = i64> {
        prop_oneof![
            10 => Just(0i64),
            1 => -8i64..8,
            1 => prop::sample::select(vec![
                TABLE_MOVE_BONUS,
                -TABLE_MOVE_BONUS,
                WINNING_CAPTURE_BASE + SEE_UNIT,
                -(WINNING_CAPTURE_BASE + SEE_UNIT),
                KILLER_BONUS[0],
                -KILLER_BONUS[1],
                SEE_UNIT,
                -SEE_UNIT,
            ]),
        ]
    }

    proptest! {
        // the claim the sort's comment makes: the stack sort and the stable
        // library sort produce the same order for any keys, ties included
        #[test]
        fn agrees_with_the_stable_sort_it_replaces(
            input in prop::collection::vec(a_key(), 0..=MOVE_LIST_INLINE),
        ) {
            let (sorted, expected) = both_orders(&input);
            prop_assert_eq!(sorted, expected);
        }
    }

    /// The edges of the run copy, named rather than left to the generator:
    /// nothing to sort, nothing to pass over, and a run reaching the last
    /// place a word of bits holds.
    #[test]
    fn the_ends_of_the_run_copy_agree_too() {
        let full = MOVE_LIST_INLINE;
        let cases: Vec<Vec<i64>> = vec![
            vec![],
            vec![0],
            vec![-1],
            vec![1],
            vec![1, 0, 0],
            vec![0; full],
            (0..full)
                .map(|i| i as i64 - 32)
                .filter(|k| *k != 0)
                .collect(),
            // one scored key at the foot, the rest a run to the last place
            std::iter::once(-1)
                .chain(std::iter::repeat_n(0, full - 1))
                .collect(),
            // one at the head, so the run ends one short of the last place
            std::iter::repeat_n(0, full - 1)
                .chain(std::iter::once(1))
                .collect(),
            // a run either side of a scored key in the middle
            std::iter::repeat_n(0, 31)
                .chain(std::iter::once(-1))
                .chain(std::iter::repeat_n(0, 32))
                .collect(),
        ];
        for input in cases {
            let (sorted, expected) = both_orders(&input);
            assert_eq!(sorted, expected, "keys {input:?}");
        }
    }
}
