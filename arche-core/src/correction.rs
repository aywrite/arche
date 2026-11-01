// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! A correction history: the running error between the static evaluation
//! and the answers the search found under it, kept by the side to move and
//! a key, and added back to the evaluation the two shortcut gates read.
//!
//! The key is the size of the pair term's embedding for the side to move,
//! `‖s‖` (see `eval::factors`), in 64 buckets. Positions the pair term sees
//! as alike in size share a slot, so the table learns where the term, in
//! its own coordinates, misjudges. The pawn and material keys were screened
//! beside it at fixed nodes and read below it.
//!
//! The update and the read are the conventional ones. An entry is held at
//! `GRAIN` times the centipawns, moved a weight of `min(depth + 1, 16)` in
//! 256 towards the error each time a node answers, with the target held to
//! `LIMIT`. The read is held to `CLAMP`, which is smaller than the stored
//! range so the margin it sits beside keeps its headroom.

use crate::misc::{Color, Score};
use crate::value::{CHECKMATE_THRESHOLD, Value, is_mate};

/// The slots a side has: the magnitude's buckets, the last one taking
/// everything above it.
pub(crate) const SLOTS: usize = 64;

/// The fixed point an entry is kept in, so the small weights still move it.
const GRAIN: i32 = 256;

/// The most an entry stores, in centipawns.
const LIMIT: i32 = 64;

/// The most the read adds to or takes from an evaluation, in centipawns.
/// Chosen with the reverse futility margin on shadow rows before any game
/// was played. It equals `LIMIT`, so it does not bind: the selection found
/// no smaller clamp that did better.
pub(crate) const CLAMP: i32 = 64;

/// What the node's answer says about the evaluation: a fail high is a
/// floor, a fail low a ceiling, and only an error whose sign the bound can
/// vouch for is learned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bound {
    Lower,
    Exact,
    Upper,
}

/// The two sides' slots.
#[derive(Clone, Debug)]
pub(crate) struct Table {
    entries: [[i32; SLOTS]; 2],
}

impl Default for Table {
    fn default() -> Self {
        Self {
            entries: [[0; SLOTS]; 2],
        }
    }
}

impl Table {
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// The correction for `side` at `key`, in centipawns, held to `CLAMP`.
    #[inline(always)]
    pub(crate) fn read(&self, side: Color, key: usize) -> i32 {
        (self.entries[side as usize][key] / GRAIN).clamp(-CLAMP, CLAMP)
    }

    /// Move the entry towards what the search found. `raw` is the
    /// uncorrected evaluation the node stood on and `answer` what it
    /// returned under `bound`. A tainted answer or a mate on either side
    /// teaches nothing: neither is an error the evaluation could have made.
    pub(crate) fn update(
        &mut self,
        side: Color,
        key: usize,
        raw: Score,
        answer: Value,
        bound: Bound,
        depth: u8,
    ) {
        if answer.tainted || is_mate(answer.score) || is_mate(raw) {
            return;
        }
        let error = i32::from(answer.score) - i32::from(raw);
        match bound {
            Bound::Lower if error <= 0 => return,
            Bound::Upper if error >= 0 => return,
            _ => {}
        }
        let weight = (i32::from(depth) + 1).min(16);
        let target = (error * GRAIN).clamp(-LIMIT * GRAIN, LIMIT * GRAIN);
        let entry = &mut self.entries[side as usize][key];
        *entry = (*entry * (256 - weight) + target * weight) / 256;
    }
}

/// `raw` moved by `correction`, kept off the mate scores.
#[inline(always)]
pub(crate) fn corrected(raw: Score, correction: i32) -> Score {
    let edge = i32::from(CHECKMATE_THRESHOLD) - 1;
    (i32::from(raw) + correction).clamp(-edge, edge) as Score
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A floor under the evaluation teaches only that it was too low, and a
    /// ceiling only that it was too high.
    #[test]
    fn a_bound_teaches_only_the_side_it_vouches_for() {
        let mut table = Table::default();
        table.update(Color::White, 3, 0, Value::clean(-50), Bound::Lower, 8);
        table.update(Color::White, 3, 0, Value::clean(50), Bound::Upper, 8);
        assert_eq!(table.read(Color::White, 3), 0);
        table.update(Color::White, 3, 0, Value::clean(50), Bound::Lower, 8);
        assert!(table.read(Color::White, 3) > 0);
    }

    /// Repeated errors pull the entry towards the error and no further than
    /// the clamp, and the other side's slot is untouched.
    #[test]
    fn an_entry_converges_inside_the_clamp() {
        let mut table = Table::default();
        for _ in 0..2000 {
            table.update(Color::Black, 0, 10, Value::clean(-990), Bound::Exact, 15);
        }
        // the weighted step truncates toward zero, so an entry settles a
        // centipawn short of the limit it is pulled to
        assert!((-LIMIT.min(CLAMP)..=-LIMIT.min(CLAMP) + 1).contains(&table.read(Color::Black, 0)));
        assert_eq!(table.read(Color::White, 0), 0);
        for _ in 0..2000 {
            table.update(Color::Black, 0, 10, Value::clean(30), Bound::Exact, 15);
        }
        assert!((19..=20).contains(&table.read(Color::Black, 0)));
    }

    /// Mates and draws by rule are not evaluation errors.
    #[test]
    fn mates_and_tainted_answers_teach_nothing() {
        let mut table = Table::default();
        let mate = CHECKMATE_THRESHOLD + 5;
        table.update(Color::White, 1, 0, Value::clean(mate), Bound::Exact, 8);
        table.update(
            Color::White,
            1,
            0,
            Value {
                score: 300,
                tainted: true,
            },
            Bound::Exact,
            8,
        );
        assert_eq!(table.read(Color::White, 1), 0);
    }

    #[test]
    fn a_correction_never_reaches_a_mate_score() {
        assert_eq!(
            corrected(CHECKMATE_THRESHOLD - 2, 64),
            CHECKMATE_THRESHOLD - 1
        );
        assert_eq!(
            corrected(-CHECKMATE_THRESHOLD + 2, -64),
            -CHECKMATE_THRESHOLD + 1
        );
        assert_eq!(corrected(10, -4), 6);
    }
}
