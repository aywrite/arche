// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The order the search tries moves in: the table's move ahead of
//! everything, captures by MVV-LVA, then the quiet moves by what the
//! search has learned about them (the defended piece penalty can carry a
//! queen capture behind even the quiet moves, which is where it means to
//! put one).
//! The sort is stable and generation order breaks its ties, so the tree a
//! search walks depends on both, and the node count tests pin the pair.
//!
//! Two memories carry across nodes. The killers are the quiet moves that
//! cut a node off at each distance from the root, tried early by that
//! ply's other nodes; the history is how often each quiet move has cut off
//! anywhere, which orders the moves no killer names. Both are held here
//! and both are the search's to fill: `SearchConfig::move_memory` says
//! whether a node consults them at all.
//!
//! The bands do not overlap, and `the_bands_do_not_overlap` says so with
//! the constants rather than a comment. Mind that the root's
//! aborted-answer swap is sound only because the table's move sorts first,
//! so nothing may outrank that bonus at the root. The deepening loop says
//! why.

use crate::board::{Board, MOVE_LIST_INLINE, MoveList};
use crate::engine::MAX_PLY;
use crate::misc::{Color, Piece};
use crate::play::Play;

/// The unit an MVV-LVA score is counted in. A quiet move scores zero
/// without the memories, so counting every capture in these leaves room
/// under the captures for the memories to speak in, and changes no order
/// by itself: the old key times a positive constant sorts as the old key
/// did.
///
/// How much room there is takes the defended piece penalty into account.
/// The smallest capture score MVV-LVA still puts above a quiet move is
/// two, a queen taking a defended bishop, so what the memories have is
/// two units and not one. `the_bands_do_not_overlap` works that out from
/// the tables rather than trusting this paragraph.
const QUIET_BAND: i64 = 10_000_000;
/// The table's move, ahead of every capture. The root depends on this
/// being unreachable by anything else; see the module comment.
const TABLE_MOVE_BONUS: i64 = 100_000 * QUIET_BAND;
/// The two killers, in the order they are tried. Both sit under the
/// smallest capture MVV-LVA ranks above a quiet move, and above the quiet
/// moves themselves.
const KILLER_BONUS: [i64; 2] = [9_000_000, 8_000_000];
/// What a history entry may reach before the whole table is halved. It is
/// below the second killer, so no move the history likes ever reaches the
/// killers' band however long a search runs. At the depths the engine is
/// measured at, entries peak thousands of times below this, so the
/// halving is a guard on the band and not aging; a threshold low enough
/// to age inside a search is its own change to measure.
const HISTORY_MAX: u32 = 7_000_000;

/// Most valuable victim: what taking each piece is worth.
const VICTIM_SCORES: [i64; 6] = [100, 250, 300, 400, 500, 1000];
/// Least valuable attacker: what taking it with each piece is worth.
const ATTACKER_SCORES: [i64; 6] = [6, 5, 4, 3, 2, 1];
/// What a queen taking a defended piece gives up, which is enough to carry
/// most of those captures behind the quiet moves.
const DEFENDED_QUEEN_PENALTY: i64 = 300;

/// How often each quiet move has cut a node off, by the side that played
/// it and the squares it moved between. The from and to squares alone,
/// which is what a butterfly table is: the piece is not part of the index,
/// so two pieces that can make the same journey share an entry.
type History = [[[u32; 64]; 64]; 2];

pub(crate) struct MoveOrdering {
    /// Scratch for the keys, one buffer reused by every sort. As a local it
    /// had to be initialised on every call, and the compiler made that a
    /// five hundred byte memset per list ordered; here it is written once
    /// and only ever the first `len` entries are read or written. The sort
    /// finishes with the buffer before the search recurses, so no two uses
    /// are ever alive at once.
    keys: [i64; MOVE_LIST_INLINE],
    /// The two most recent quiet cutoffs at each distance from the root.
    /// A killer that is not legal at the node reading it is simply not in
    /// that node's list, which costs nothing.
    killers: [[Option<Play>; 2]; MAX_PLY as usize],
    history: History,
}

impl MoveOrdering {
    pub(crate) fn new() -> Self {
        Self {
            keys: [0; MOVE_LIST_INLINE],
            killers: [[None; 2]; MAX_PLY as usize],
            history: [[[0; 64]; 64]; 2],
        }
    }

    /// Forget both memories. Each `go` starts with this: what a killer or a
    /// history score says is about the tree being searched now. The
    /// iterations of one deepening share them, which is the point of
    /// keeping them at all.
    pub(crate) fn forget(&mut self) {
        self.killers.fill([None; 2]);
        for side in self.history.iter_mut() {
            for from in side.iter_mut() {
                from.fill(0);
            }
        }
    }

    /// A move that cut a node off, `ply` from the root with `depth` left to
    /// search. It becomes this ply's first killer and its history entry
    /// gains the square of the depth, so a cutoff proved over a deeper
    /// subtree counts for more than a shallow one.
    ///
    /// A capture is dropped. MVV-LVA orders the captures already, and a
    /// killer slot holding one would order nothing the sort does not.
    pub(crate) fn cutoff(&mut self, color: Color, m: &Play, ply: usize, depth: u8) {
        debug_assert!(ply < MAX_PLY as usize, "no killers past the rail");
        if m.capture.is_some() {
            return;
        }
        let killers = &mut self.killers[ply];
        // the old first killer shifts down unless the move is already it,
        // which is what keeps one move out of both slots
        if killers[0] != Some(*m) {
            killers[1] = killers[0];
            killers[0] = Some(*m);
        }
        let bonus = u32::from(depth) * u32::from(depth);
        let entry = &mut self.history[color as usize][m.from as usize][m.to as usize];
        *entry += bonus;
        if *entry >= HISTORY_MAX {
            // the whole table halves rather than the hot entry sticking at
            // the top, so if the ceiling is ever reached the entries keep
            // their proportions and none reaches the killers' band. See
            // HISTORY_MAX for why no measured search gets here
            for side in self.history.iter_mut() {
                for from in side.iter_mut() {
                    for to in from.iter_mut() {
                        *to /= 2;
                    }
                }
            }
        }
    }

    /// Test-only reads, for the search level checks that a cutoff is
    /// credited to the side and the ply that earned it.
    #[cfg(test)]
    pub(crate) fn history_total(&self, color: Color) -> u64 {
        self.history[color as usize]
            .iter()
            .flatten()
            .map(|&e| u64::from(e))
            .sum()
    }

    #[cfg(test)]
    pub(crate) fn killers_at(&self, ply: usize) -> [Option<Play>; 2] {
        self.killers[ply]
    }

    /// MVV-LVA order, with the table's move for this position, if there is
    /// one, ahead of everything else.
    ///
    /// The search may already have played that move without generating, in
    /// which case it skips it here. The bonus still earns its keep: a table
    /// move it declined to play early was never searched, so it is still in
    /// this list and still has to be the first one tried.
    ///
    /// `ply` is the node's distance from the root when the quiet memories
    /// are consulted, and none when they are not: quiescence and the root
    /// order without them, and so does every node under a configuration
    /// with `move_memory` off.
    pub(crate) fn order(
        &mut self,
        board: &Board,
        moves: &mut MoveList,
        table_move: Option<Play>,
        ply: Option<usize>,
    ) {
        // the fields are borrowed apart so that the keys can be written
        // while the memories are read
        let Self {
            keys,
            killers,
            history,
        } = self;
        let quiet = ply.map(|ply| {
            debug_assert!(ply < MAX_PLY as usize, "no killers past the rail");
            Quiet {
                killers: killers[ply],
                history,
            }
        });
        // Most lists here are short: quiescence sorts a handful of captures
        // or the evasions the filter kept, and the counts say under nine
        // moves on average. sort_by_cached_key allocates scratch on every
        // call, which at that size costs more than the sorting, so lists
        // take the stack sort instead, keeping the allocating sort only for
        // a list that spilled the buffer. Callgrind swept smaller cutoffs
        // and they are a wash: the mid length lists of full width nodes
        // keep paying the allocating fallback while every call pays the
        // extra branch.
        if moves.len() <= MOVE_LIST_INLINE {
            for (i, m) in moves.iter().enumerate() {
                keys[i] = ordering_key(board, m, table_move, quiet.as_ref());
            }
            sort_on_the_stack(moves, keys);
        } else {
            moves.sort_by_cached_key(|m| ordering_key(board, m, table_move, quiet.as_ref()));
        }
    }
}

/// What the memories say at one node: this ply's killers, and the history
/// table the moves they do not name are ordered by.
struct Quiet<'a> {
    killers: [Option<Play>; 2],
    history: &'a History,
}

impl Quiet<'_> {
    /// What a quiet move is worth here. A move nothing is known about
    /// scores zero, and nothing this returns reaches the smallest capture
    /// the sort puts above a quiet move.
    #[inline]
    fn bonus(&self, color: Color, m: &Play) -> i64 {
        if self.killers[0] == Some(*m) {
            return KILLER_BONUS[0];
        }
        if self.killers[1] == Some(*m) {
            return KILLER_BONUS[1];
        }
        i64::from(self.history[color as usize][m.from as usize][m.to as usize])
    }
}

/// What a move sorts by, smaller first: its MVV-LVA score with the table's
/// move pushed ahead of everything else and what the memories say about a
/// quiet move under it, negated so that the best score is the smallest key.
#[inline]
fn ordering_key(
    board: &Board,
    m: &Play,
    table_move: Option<Play>,
    quiet: Option<&Quiet<'_>>,
) -> i64 {
    let mut score = mvv_lva(board, m) * QUIET_BAND;
    if m.capture.is_none() {
        if let Some(quiet) = quiet {
            score += quiet.bonus(board.active_color, m);
        }
    }
    if table_move == Some(*m) {
        score += TABLE_MOVE_BONUS;
    }
    -score
}

/// Most valuable victim, least valuable attacker: take the biggest piece
/// with the smallest one first.
///
/// The scores index by piece rather than matching on it: the arms did
/// different arithmetic per piece, which compiled to an indirect jump
/// taken once per capture scored, and the pieces arrive in no order a
/// predictor can learn. Inline because the sort computes a key per move
/// generated, and this is most of the key.
#[inline]
fn mvv_lva(board: &Board, m: &Play) -> i64 {
    let Some(victim) = m.capture else {
        return 0;
    };
    let Some(attacker) = board.get_piece_index(m.from) else {
        return 0;
    };
    let score = VICTIM_SCORES[victim as usize] + ATTACKER_SCORES[attacker as usize];
    // a queen taking a defended piece is usually just losing the queen, so
    // push it below the other captures rather than trying it first
    if matches!(attacker, Piece::Queen) && board.square_attacked(m.to, !board.active_color) {
        return score - DEFENDED_QUEEN_PENALTY;
    }
    score
}

/// What sort_by_cached_key does, minus its allocation, for a list that fits
/// the buffer: a stable insertion sort over keys the caller computed once
/// each. Shifting only while strictly greater keeps equal keys in their
/// generated order, exactly as the stable sort does, so the two produce the
/// same order and the tree searched is the same whichever runs; the node
/// count tests hold both to that. The keys arrive in a buffer beside the
/// moves rather than as a function to call: a key closure of any weight was
/// a call per move rather than code in this loop, which is where most of
/// the sorting went.
#[inline]
fn sort_on_the_stack(moves: &mut [Play], keys: &mut [i64; MOVE_LIST_INLINE]) {
    debug_assert!(moves.len() <= MOVE_LIST_INLINE);
    for i in 1..moves.len() {
        let k = keys[i];
        let m = moves[i];
        let mut j = i;
        while j > 0 && keys[j - 1] > k {
            keys[j] = keys[j - 1];
            moves[j] = moves[j - 1];
            j -= 1;
        }
        keys[j] = k;
        moves[j] = m;
    }
}

#[cfg(test)]
mod order {
    use super::MoveOrdering;
    use crate::board::Board;
    use crate::misc::Color;
    use crate::play::Play;

    // white to move: the pawn on e4 and the queen on h5 can both take the
    // queen on d5, and the queen can take the pawn on h7, which its king
    // defends
    const CAPTURES: &str = "rn5k/7p/8/3q3Q/4P3/8/8/6K1 w - - 0 1";

    fn ordered(fen: &str, table_move: Option<Play>) -> Vec<Play> {
        ordered_by(fen, table_move, &mut MoveOrdering::new())
    }

    /// The same, ordered by an ordering the test has already taught
    /// something, and read at the ply that ordering was taught at.
    fn ordered_by(fen: &str, table_move: Option<Play>, ordering: &mut MoveOrdering) -> Vec<Play> {
        let board = Board::from_fen(fen).unwrap();
        let mut moves = board.generate_moves();
        ordering.order(&board, &mut moves, table_move, Some(0));
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

    #[test]
    fn the_biggest_victim_and_the_smallest_attacker_go_first() {
        let moves = ordered(CAPTURES, None);
        assert_eq!(position_of(&moves, "e4d5"), 0);
        assert_eq!(position_of(&moves, "h5d5"), 1);
    }

    #[test]
    fn a_queen_taking_a_defended_piece_waits_behind_the_other_captures() {
        let moves = ordered(CAPTURES, None);
        assert!(position_of(&moves, "h5h7") > position_of(&moves, "h5d5"));
    }

    #[test]
    fn the_tables_move_goes_first_even_when_quiet() {
        let board = Board::from_fen(CAPTURES).unwrap();
        let generated = board.generate_moves();
        let push = generated[position_of(&generated, "e4e5")];
        let moves = ordered(CAPTURES, Some(push));
        assert_eq!(moves[0], push);
        assert_eq!(position_of(&moves, "e4d5"), 1);
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
        ordering.cutoff(Color::White, &killer, 0, 4);

        let moves = ordered_by(CAPTURES, None, &mut ordering);
        // behind both captures the sort puts above a quiet move
        assert!(position_of(&moves, "e4e5") > position_of(&moves, "e4d5"));
        assert!(position_of(&moves, "e4e5") > position_of(&moves, "h5d5"));
        // ahead of every other quiet move
        assert_eq!(quiets(&moves)[0], killer);
        // and ahead of the defended queen capture, which the penalty puts
        // behind the quiet moves whether there is a killer among them or
        // not
        assert!(position_of(&moves, "h5h7") > position_of(&moves, "e4e5"));
    }

    #[test]
    fn the_second_killer_waits_behind_the_first() {
        let board = Board::from_fen(CAPTURES).unwrap();
        let generated = board.generate_moves();
        let first = named(&generated, "e4e5");
        let second = named(&generated, "g1f1");
        let mut ordering = MoveOrdering::new();
        ordering.cutoff(Color::White, &second, 0, 4);
        ordering.cutoff(Color::White, &first, 0, 4);

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
        ordering.cutoff(Color::White, &generated[0], 1, 1);
        ordering.cutoff(Color::White, &last, 1, 4);

        let moves = quiets(&ordered_by(CAPTURES, None, &mut ordering));
        assert_eq!(moves[0], last);
        assert_eq!(moves[1], generated[0]);
    }

    #[test]
    fn a_killer_that_is_also_the_tables_move_sorts_by_the_table_bonus() {
        let board = Board::from_fen(CAPTURES).unwrap();
        let killer = named(&board.generate_moves(), "e4e5");
        let mut ordering = MoveOrdering::new();
        ordering.cutoff(Color::White, &killer, 0, 4);

        let moves = ordered_by(CAPTURES, Some(killer), &mut ordering);
        assert_eq!(moves[0], killer);
        assert_eq!(position_of(&moves, "e4d5"), 1);
    }
}

#[cfg(test)]
mod memory {
    use super::{ATTACKER_SCORES, DEFENDED_QUEEN_PENALTY, HISTORY_MAX, KILLER_BONUS, MoveOrdering};
    use super::{QUIET_BAND, TABLE_MOVE_BONUS, VICTIM_SCORES};
    use crate::misc::{Color, Piece};
    use crate::play::Play;

    fn quiet(from: u8, to: u8) -> Play {
        Play::new(from, to, None, None, false, false)
    }

    /// Every score `mvv_lva` can return for a capture, worked out from the
    /// tables it reads rather than written down, so that a table edited
    /// later is what this test is read against. The defended piece penalty
    /// is part of it: it is what makes the smallest capture that still
    /// outranks a quiet move worth two rather than a hundred and one.
    fn capture_scores() -> Vec<i64> {
        let mut scores = Vec::new();
        for victim in VICTIM_SCORES {
            for (piece, attacker) in ATTACKER_SCORES.iter().enumerate() {
                scores.push(victim + attacker);
                // the penalty is the queen's alone
                if piece == Piece::Queen as usize {
                    scores.push(victim + attacker - DEFENDED_QUEEN_PENALTY);
                }
            }
        }
        scores
    }

    #[test]
    fn the_bands_do_not_overlap() {
        let scores = capture_scores();
        let best = *scores.iter().max().expect("the tables are not empty");
        // the smallest capture the sort still puts above a quiet move. The
        // ones below zero sort behind the quiet moves, which is where the
        // penalty means to put them, and the memories are under no
        // obligation to stay behind those
        let smallest_above_a_quiet = *scores
            .iter()
            .filter(|score| **score > 0)
            .min()
            .expect("some capture outranks a quiet move");
        assert_eq!(smallest_above_a_quiet, 2);

        // the table's move ahead of the best capture there could be, which
        // is what the root's swap rests on
        assert!(TABLE_MOVE_BONUS > best * QUIET_BAND);
        // and the memories under every capture that outranks a quiet move
        assert!(smallest_above_a_quiet * QUIET_BAND > KILLER_BONUS[0]);
        assert!(KILLER_BONUS[0] > KILLER_BONUS[1]);
        assert!(KILLER_BONUS[1] > i64::from(HISTORY_MAX));
    }

    #[test]
    fn a_cutoff_takes_the_first_slot_and_shifts_the_old_one_down() {
        let mut ordering = MoveOrdering::new();
        let first = quiet(8, 16);
        let second = quiet(9, 17);
        ordering.cutoff(Color::White, &first, 3, 4);
        assert_eq!(ordering.killers[3], [Some(first), None]);
        ordering.cutoff(Color::White, &second, 3, 4);
        assert_eq!(ordering.killers[3], [Some(second), Some(first)]);
        // and the ply is what indexes them
        assert_eq!(ordering.killers[4], [None, None]);
    }

    #[test]
    fn one_move_does_not_hold_both_killer_slots() {
        let mut ordering = MoveOrdering::new();
        let m = quiet(8, 16);
        let other = quiet(9, 17);
        // cutting off twice in a row leaves the second slot empty
        ordering.cutoff(Color::White, &m, 0, 4);
        ordering.cutoff(Color::White, &m, 0, 4);
        assert_eq!(ordering.killers[0], [Some(m), None]);
        // and a move promoted out of the second slot does not stay in it,
        // which is what the shift would get wrong
        ordering.cutoff(Color::White, &other, 0, 4);
        assert_eq!(ordering.killers[0], [Some(other), Some(m)]);
        ordering.cutoff(Color::White, &m, 0, 4);
        assert_eq!(ordering.killers[0], [Some(m), Some(other)]);
    }

    #[test]
    fn a_capture_cutoff_touches_neither_memory() {
        let mut ordering = MoveOrdering::new();
        let take = Play::new(8, 16, Some(Piece::Pawn), None, false, false);
        ordering.cutoff(Color::White, &take, 0, 4);
        assert_eq!(ordering.killers[0], [None, None]);
        assert_eq!(ordering.history[Color::White as usize][8][16], 0);
    }

    #[test]
    fn a_history_entry_gains_the_square_of_the_depth() {
        let mut ordering = MoveOrdering::new();
        let m = quiet(8, 16);
        ordering.cutoff(Color::White, &m, 0, 5);
        assert_eq!(ordering.history[Color::White as usize][8][16], 25);
        // the side that played it is part of the index
        assert_eq!(ordering.history[Color::Black as usize][8][16], 0);
    }

    #[test]
    fn an_entry_at_the_ceiling_halves_the_table_rather_than_wrapping() {
        const DEPTH: u8 = 8;
        let bonus = u32::from(DEPTH) * u32::from(DEPTH);
        let mut ordering = MoveOrdering::new();
        let hot = quiet(8, 16);
        let cool = quiet(9, 17);
        ordering.cutoff(Color::White, &cool, 0, DEPTH);
        for _ in 0..(HISTORY_MAX / bonus + 2) {
            ordering.cutoff(Color::White, &hot, 0, DEPTH);
        }
        let history = &ordering.history[Color::White as usize];
        assert!(history[8][16] > 0 && history[8][16] < HISTORY_MAX);
        // the whole table aged, not the entry that reached the ceiling
        assert!(history[9][17] > 0 && history[9][17] < bonus);
    }

    #[test]
    fn forgetting_empties_both_memories() {
        let mut ordering = MoveOrdering::new();
        let m = quiet(8, 16);
        ordering.cutoff(Color::White, &m, 2, 4);
        ordering.forget();
        assert_eq!(ordering.killers[2], [None, None]);
        assert_eq!(ordering.history[Color::White as usize][8][16], 0);
    }
}

#[cfg(test)]
mod stack_sort {
    use super::{MOVE_LIST_INLINE, sort_on_the_stack};
    use crate::play::Play;
    use proptest::prelude::*;

    proptest! {
        // the claim the sort's comment makes: the stack sort and the stable
        // library sort produce the same order for any keys, ties included.
        // the key domain is kept small so that ties actually occur: sixty
        // four draws from the whole of i64 would never produce one
        #[test]
        fn agrees_with_the_stable_sort_it_replaces(input in prop::collection::vec(-8i64..8, 0..=MOVE_LIST_INLINE)) {
            let moves: Vec<Play> = (0..input.len())
                .map(|i| Play::new(i as u8, 0, None, None, false, false))
                .collect();

            let mut expected: Vec<(i64, Play)> =
                input.iter().copied().zip(moves.iter().copied()).collect();
            expected.sort_by_key(|(key, _)| *key);
            let expected: Vec<Play> = expected.into_iter().map(|(_, m)| m).collect();

            let mut keys = [0i64; MOVE_LIST_INLINE];
            keys[..input.len()].copy_from_slice(&input);
            let mut sorted = moves;
            sort_on_the_stack(&mut sorted, &mut keys);

            prop_assert_eq!(sorted, expected);
        }
    }
}
