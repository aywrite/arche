// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The order the search tries moves in: the table's move ahead of
//! everything, the captures `Board::see` prices as winning or even, the
//! killers, the quiet moves by what the search has learned about them, and
//! the losing captures last of all.
//! The list is sorted in two stages. `order` keys the table's move and the
//! captures and leaves the quiet moves in generated order between the two
//! capture bands; `order_quiets` scores and sorts the quiet moves, and the
//! search calls it only when it reaches the first of them, so a node its
//! captures cut off never scores one. Both sorts are stable and generation
//! order breaks their ties. The quiet moves are scored by the memories as
//! they stand when the search reaches them, not when the node was entered,
//! so the tree a search walks depends on the sort, the generation order and
//! when the scoring happens, and the node count tests pin all three.
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
use crate::misc::Color;
use crate::play::Play;

/// The table's move, ahead of every capture, even one whose swap loses
/// the king's whole price. The root depends on this being unreachable by
/// anything else; see the module comment.
const TABLE_MOVE_BONUS: i64 = 1_000_000_000_000;
/// Where the winning and even captures start: above the killers, ordered
/// within the band by what `Board::see` says each wins. An even exchange
/// still opens lines and forces replies, so it ranks with the winners
/// rather than the losers; cheap to revisit if that reads wrong one day.
/// A losing capture takes no base at all, and its negative SEE carries it
/// below every quiet move, least losing first: a capture the swap already
/// prices as losing is a worse bet than a quiet with history behind it.
const WINNING_CAPTURE_BASE: i64 = 20_000_000;
/// The unit a point of SEE is counted in, leaving room under one point
/// for the MVV-LVA tiebreak between captures the swap prices alike.
const SEE_UNIT: i64 = 2_000;
/// The two killers, in the order they are tried. Both sit under the
/// smallest even capture, and above the quiet moves themselves.
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
    /// A capture is dropped. The swap orders the captures already, and a
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

    /// The first stage of the band order the module comment describes:
    /// the table's move for this position, if there is one, then the
    /// winning and even captures, sorted, at the front of the list. Behind
    /// them the quiet moves stand in the order they were generated in and
    /// the losing captures, sorted, close the list. How many moves the
    /// front holds is returned, and the search calls `order_quiets` when
    /// it reaches the first move past them, so a node the front cuts off
    /// never scores a quiet move at all.
    ///
    /// The search may already have played the table's move without
    /// generating, in which case it skips it here. The bonus still earns
    /// its keep: a table move it declined to play early was never
    /// searched, so it is still in this list and still has to be the first
    /// one tried.
    ///
    /// `ply` is the node's distance from the root when the quiet memories
    /// are consulted, and none when they are not: quiescence and the root
    /// order without them, and so does every node under a configuration
    /// with `move_memory` off. The count comes back the same without them:
    /// a quiet move scores zero either way, and the search asks for the
    /// second stage only when it has a ply to score at.
    ///
    /// Quiescence reads the count for its losing capture skip, and what the
    /// skip needs of it is that every capture from that index on is one the
    /// swap priced as losing. The stack sort's count says more, that no
    /// capture before it is losing either, the table's move aside, which
    /// sorts ahead of everything however the swap prices it. A list that
    /// spilled the buffer is ordered whole, memories included, and its
    /// length comes back, which the skip reads as nothing to skip. That is
    /// sound, and moot: quiescence orders captures and evasions, and
    /// neither list gets that long.
    pub(crate) fn order(
        &mut self,
        board: &Board,
        moves: &mut MoveList,
        table_move: Option<Play>,
        ply: Option<usize>,
    ) -> usize {
        let keys = &mut self.keys;
        // Most lists here are short: quiescence sorts a handful of captures
        // or the evasions the filter kept, and the counts say under nine
        // moves on average. sort_by_cached_key allocates scratch on every
        // call, which at that size costs more than the sorting, so lists
        // take the stack sort instead, keeping the allocating sort only for
        // a list that spilled the buffer, which takes the whole order at
        // once with the memories read here.
        if moves.len() > MOVE_LIST_INLINE {
            let quiet = ply.map(|ply| Quiet {
                killers: self.killers[ply],
                history: &self.history,
            });
            moves.sort_by_cached_key(|m| ordering_key(board, m, table_move, quiet.as_ref()));
            return moves.len();
        }
        // a quiet move keys zero here, which sits between the front, whose
        // keys are negative, and the losing captures, whose keys are
        // positive; the sort is stable, so the quiet moves keep their
        // generated order for the second stage to sort within. The front is
        // counted as the keys are written, so the losing band's edge costs
        // no search of the sorted keys afterwards
        let mut front = 0;
        for (i, m) in moves.iter().enumerate() {
            let key = if m.capture.is_some() || table_move == Some(*m) {
                ordering_key(board, m, table_move, None)
            } else {
                0
            };
            front += usize::from(key < 0);
            keys[i] = key;
        }
        sort_on_the_stack(moves, keys);
        front
    }

    /// The second stage: `rest` starts at the first move past the front,
    /// and the quiet moves run from there to the first losing capture. They
    /// are scored by the memories as they stand now, killers first and the
    /// rest by history, and sorted in place; the losing captures behind
    /// them are already in their order.
    pub(crate) fn order_quiets(&mut self, board: &Board, rest: &mut [Play], ply: usize) {
        debug_assert!(ply < MAX_PLY as usize, "no killers past the rail");
        let run = rest.iter().take_while(|m| m.capture.is_none()).count();
        let quiets = &mut rest[..run];
        let quiet = Quiet {
            killers: self.killers[ply],
            history: &self.history,
        };
        let keys = &mut self.keys;
        for (i, m) in quiets.iter().enumerate() {
            keys[i] = -quiet.bonus(board.active_color, m);
        }
        sort_on_the_stack(quiets, keys);
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
    #[inline(always)]
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

/// What a move sorts by, smaller first: a capture by what the swap says of
/// it, a quiet move by what the memories say, and the table's move pushed
/// ahead of everything else, negated so that the best score is the
/// smallest key.
#[inline]
fn ordering_key(
    board: &Board,
    m: &Play,
    table_move: Option<Play>,
    quiet: Option<&Quiet<'_>>,
) -> i64 {
    let mut score = if m.capture.is_some() {
        capture_score(board, m)
    } else if let Some(quiet) = quiet {
        quiet.bonus(board.active_color, m)
    } else {
        0
    };
    if table_move == Some(*m) {
        score += TABLE_MOVE_BONUS;
    }
    -score
}

/// Where a capture sorts. The swap's verdict picks the band: winning and
/// even captures above the killers, losing ones below every quiet move.
/// Within a band the SEE value orders, and MVV-LVA breaks the ties
/// between captures the swap prices alike. Only moves with a victim get
/// here, so the quiet moves never pay for a swap.
#[inline]
fn capture_score(board: &Board, m: &Play) -> i64 {
    let see = i64::from(board.see(m));
    let score = see * SEE_UNIT + mvv_lva(board, m);
    if see >= 0 {
        WINNING_CAPTURE_BASE + score
    } else {
        score
    }
}

/// Most valuable victim, least valuable attacker: take the biggest piece
/// with the smallest one first.
///
/// The scores index by piece rather than matching on it: the arms did
/// different arithmetic per piece, which compiled to an indirect jump
/// taken once per capture scored, and the pieces arrive in no order a
/// predictor can learn.
#[inline]
fn mvv_lva(board: &Board, m: &Play) -> i64 {
    let Some(victim) = m.capture else {
        return 0;
    };
    let Some(attacker) = board.get_piece_index(m.from) else {
        return 0;
    };
    VICTIM_SCORES[victim as usize] + ATTACKER_SCORES[attacker as usize]
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
        let front = ordering.order(&board, &mut moves, table_move, Some(0));
        ordering.order_quiets(&board, &mut moves[front..], 0);
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
        ordering.cutoff(Color::White, &taught, 1, 4);

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
        ordering.cutoff(Color::White, &killer, 0, 4);

        let moves = ordered_by(ROOKS, None, &mut ordering);
        assert!(position_of(&moves, "e1e8") < position_of(&moves, "h1g1"));
    }

    // the front is the table's move and the captures the swap prices as
    // winning or even: the two takings of the queen, plus a table move
    // whether it is a quiet move or the losing capture itself. The same
    // count with the memories and without, since neither stage scores a
    // capture by them
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

    // what quiescence reads off the count is where the losing captures
    // start: every capture from there on is one the swap prices as losing,
    // and no capture before it is, the table's move aside. The quiet moves
    // in between are not the skip's business. A list that spilled the
    // buffer is ordered whole and its length comes back, so the skip has
    // nothing to read there
    #[test]
    fn the_count_returned_is_where_the_losing_captures_start() {
        // the two hundred move position with a knight added on c3, so that
        // the pawn on a2 has a defender besides the king: taking it with
        // the knight or the bishop then loses, where against the king alone
        // the swap knows a defended piece cannot be taken back
        const CROWDED: &str = "R6R/3Q4/1Q4Q1/4Q3/2Q4Q/Q1n2Q2/pp1Q4/kBNN1KB1 w - - 0 1";
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
            // and the check above was not vacuous: something sorts ahead of
            // the band, and without a table move taking the loser out of it
            // the band is not empty
            assert!(front > 0, "{fen}");
            let band = moves[front..].iter().any(|m| m.capture.is_some());
            assert!(table_move.is_some() || band, "{fen}");
        }
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
        // and ahead of the losing capture, which the swap puts behind the
        // quiet moves whether there is a killer among them or not
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
    use super::{ATTACKER_SCORES, HISTORY_MAX, KILLER_BONUS, MoveOrdering, SEE_UNIT};
    use super::{TABLE_MOVE_BONUS, VICTIM_SCORES, WINNING_CAPTURE_BASE};
    use crate::board::SEE_VALUES;
    use crate::misc::{Color, Piece};
    use crate::play::Play;

    fn quiet(from: u8, to: u8) -> Play {
        Play::new(from, to, None, None, false, false)
    }

    /// Every tiebreak `mvv_lva` can return for a capture, worked out from
    /// the tables it reads rather than written down, so that a table edited
    /// later is what this test is read against.
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

        // the table's move ahead of the best capture there could be, even
        // when it is itself the worst, which is what the root's aborted
        // answer swap rests on
        assert!(TABLE_MOVE_BONUS + worst_capture > best_capture);
        // the winning and even captures above the killers, the killers in
        // order above the history, and every losing capture below zero,
        // which is the least a quiet move scores
        assert!(WINNING_CAPTURE_BASE + smallest > KILLER_BONUS[0]);
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
