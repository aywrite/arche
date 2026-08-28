// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The order the search tries moves in: the table's move ahead of
//! everything, captures by MVV-LVA, and the rest as they were generated —
//! though the defended piece penalty can carry a queen capture behind even
//! the quiet moves.
//! The sort is stable and generation order breaks its ties, so the tree a
//! search walks depends on both, and the node count tests pin the pair.
//! Ordering that remembers across nodes — killers, history — lands here
//! when it arrives; mind that the root's aborted-answer swap is sound only
//! because the table's move sorts first, so nothing may outrank the bonus
//! at the root. The deepening loop says why.

use crate::board::{Board, MOVE_LIST_INLINE, MoveList};
use crate::misc::Piece;
use crate::play::Play;

pub(crate) struct MoveOrdering {
    /// Scratch for the keys, one buffer reused by every sort. As a local it
    /// had to be initialised on every call, and the compiler made that a
    /// five hundred byte memset per list ordered; here it is written once
    /// and only ever the first `len` entries are read or written. The sort
    /// finishes with the buffer before the search recurses, so no two uses
    /// are ever alive at once.
    keys: [i64; MOVE_LIST_INLINE],
}

impl MoveOrdering {
    pub(crate) fn new() -> Self {
        Self {
            keys: [0; MOVE_LIST_INLINE],
        }
    }

    /// MVV-LVA order, with the table's move for this position, if there is
    /// one, ahead of everything else.
    ///
    /// The search may already have played that move without generating, in
    /// which case it skips it here. The bonus still earns its keep: a table
    /// move it declined to play early was never searched, so it is still in
    /// this list and still has to be the first one tried.
    pub(crate) fn order(&mut self, board: &Board, moves: &mut MoveList, table_move: Option<Play>) {
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
                self.keys[i] = ordering_key(board, m, table_move);
            }
            sort_on_the_stack(moves, &mut self.keys);
        } else {
            moves.sort_by_cached_key(|m| ordering_key(board, m, table_move));
        }
    }
}

/// What a move sorts by, smaller first: its MVV-LVA score with the table's
/// move pushed ahead of everything else, negated so that the best score is
/// the smallest key.
#[inline]
fn ordering_key(board: &Board, m: &Play, table_move: Option<Play>) -> i64 {
    let mut score = mvv_lva(board, m);
    if table_move == Some(*m) {
        score += 100_000;
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
    const VICTIM_SCORES: [i64; 6] = [100, 250, 300, 400, 500, 1000];
    const ATTACKER_SCORES: [i64; 6] = [6, 5, 4, 3, 2, 1];
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
        return score - 300;
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
    use crate::play::Play;

    // white to move: the pawn on e4 and the queen on h5 can both take the
    // queen on d5, and the queen can take the pawn on h7, which its king
    // defends
    const CAPTURES: &str = "rn5k/7p/8/3q3Q/4P3/8/8/6K1 w - - 0 1";

    fn ordered(fen: &str, table_move: Option<Play>) -> Vec<Play> {
        let board = Board::from_fen(fen).unwrap();
        let mut moves = board.generate_moves();
        MoveOrdering::new().order(&board, &mut moves, table_move);
        moves.to_vec()
    }

    fn position_of(moves: &[Play], name: &str) -> usize {
        moves
            .iter()
            .position(|m| m.to_string() == name)
            .unwrap_or_else(|| panic!("{name} was not generated"))
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
        let generated: Vec<Play> = board
            .generate_moves()
            .iter()
            .filter(|m| m.capture.is_none())
            .copied()
            .collect();
        let sorted: Vec<Play> = ordered(CAPTURES, None)
            .iter()
            .filter(|m| m.capture.is_none())
            .copied()
            .collect();
        assert_eq!(generated, sorted);
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
