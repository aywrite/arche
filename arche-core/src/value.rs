// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What a node is worth, and whether that worth describes the position or
//! only the path taken to it.
//!
//! A score alone cannot say which it is. A repetition or a fifty move draw is
//! true of the line that reached a position rather than of the position, so a
//! zero read back down another line may be a draw that line cannot reach; the
//! transposition table refuses such a score for a cutoff and keeps only the
//! move. That fact has to travel with the score from the node that discovered
//! it to the node that stores it, through every negation on the way, which is
//! what this is for.
//!
//! Mate scores live here too. A mate is scored a fixed distance from
//! `CHECKMATE_SCORE`, further for a longer line, so a faster mate always
//! wins the comparison; everything within a thousand of it is a mate and
//! nothing else can be, since a static eval is bounded by the material on
//! the board, far below. The search asks `is_mate` before it prunes
//! against a beta, because a cutoff there would leave a faster mate
//! unsearched, and holds what a pass proved under the threshold with
//! `below_the_mate_window`, because a pass is not a move and cannot force
//! anything.

use crate::misc::Score;

const CHECKMATE_SCORE: Score = 30_000;
// Any score this close to CHECKMATE_SCORE is a forced mate. Regular evals are
// bounded by the material on the board, which cannot come near it.
pub(crate) const CHECKMATE_THRESHOLD: Score = CHECKMATE_SCORE - 1000;

/// Whether a score is a forced mate, for either side.
pub(crate) fn is_mate(score: Score) -> bool {
    score.abs() > CHECKMATE_THRESHOLD
}

/// The moves until mate a score encodes, positive when the side the score
/// belongs to is mating, or nothing when the score is no mate at all.
pub(crate) fn checkmate_in(score: Score) -> Option<Score> {
    if !is_mate(score) {
        return None;
    }
    let mut mate = (CHECKMATE_SCORE - score.abs() + 1) / 2;
    if score < 0 {
        mate = -mate;
    }
    Some(mate)
}

/// The score capped just under where mates are read, for a claim that a
/// position is very good without being a claim that it is won.
pub(crate) fn below_the_mate_window(score: Score) -> Score {
    score.min(CHECKMATE_THRESHOLD - 1)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Value {
    pub score: Score,
    /// True if the score flowed from a repetition or a fifty move draw
    /// somewhere below it. See the docs on graph history interaction.
    pub tainted: bool,
}

impl Value {
    /// A score that is true of the position: a static evaluation, a mate, a
    /// stalemate, or anything built only from these.
    pub fn clean(score: Score) -> Self {
        Self {
            score,
            tainted: false,
        }
    }

    /// A score that is true of the path taken here and not of the position:
    /// the draw a rule allows either side to claim.
    pub fn tainted(score: Score) -> Self {
        Self {
            score,
            tainted: true,
        }
    }

    /// The same score with the taint given rather than the one it carries,
    /// for a score whose taint was established elsewhere: a stored entry
    /// read back, or a pass's answer clamped under the mate window.
    pub fn with_taint(score: Score, tainted: bool) -> Self {
        Self { score, tainted }
    }

    /// The side to move is mated, this many plies into the line. Clean by
    /// definition: a mate is a property of the position, not of the path
    /// that reached it. The shorter the line the further the score sits
    /// below zero, so once a parent negates it, the faster mate is the
    /// better one.
    pub(crate) fn mated(line_ply: usize) -> Self {
        Self::clean(-CHECKMATE_SCORE + line_ply as Score)
    }
}

/// From the other side of the board. The score changes sign; where it came
/// from does not.
impl std::ops::Neg for Value {
    type Output = Self;

    fn neg(self) -> Self {
        Self {
            score: -self.score,
            tainted: self.tainted,
        }
    }
}

/// What a node has seen so far. The taint of a node is the taint of every
/// child it looked at, not of the one it chose: a best move found beside a
/// tainted score still stands on a comparison against that score. Each child
/// is absorbed as it is searched, and the answer is stamped with the whole.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct Taint(bool);

impl Taint {
    pub(crate) fn absorb(&mut self, value: Value) {
        self.0 |= value.tainted;
    }

    pub(crate) fn stamp(self, score: Score) -> Value {
        Value {
            score,
            tainted: self.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Taint, Value, below_the_mate_window, checkmate_in, is_mate};
    use crate::misc::Score;
    use pretty_assertions::assert_eq;

    #[test]
    fn negation_turns_the_score_and_leaves_the_taint() {
        assert_eq!(-Value::clean(30), Value::clean(-30));
        assert_eq!(-Value::tainted(0), Value::tainted(0));
        assert_eq!(-Value::tainted(-12), Value::tainted(12));
    }

    #[test]
    fn negating_twice_is_the_value_again() {
        for value in [Value::clean(1), Value::tainted(-7), Value::clean(0)] {
            assert_eq!(-(-value), value);
        }
    }

    #[test]
    fn a_taint_can_be_given_rather_than_carried() {
        // a node reports the taint of everything it looked at, which is not
        // the taint of the score it settled on
        assert_eq!(Value::with_taint(5, true), Value::tainted(5));
        assert_eq!(Value::with_taint(5, false), Value::clean(5));
    }

    #[test]
    fn a_taint_absorbed_is_never_given_back() {
        let mut taint = Taint::default();
        taint.absorb(Value::clean(10));
        assert_eq!(taint.stamp(10), Value::clean(10));
        taint.absorb(Value::tainted(0));
        taint.absorb(Value::clean(40));
        // the clean child that won does not wash out the tainted one that
        // was compared against
        assert_eq!(taint.stamp(40), Value::tainted(40));
    }

    #[test]
    fn a_mate_is_read_back_as_the_moves_it_takes() {
        // mated on the spot, and mating in one: one ply is half a move,
        // rounded towards the mate being delivered
        assert_eq!(checkmate_in(Value::mated(0).score), Some(0));
        assert_eq!(checkmate_in(-Value::mated(1).score), Some(1));
        assert_eq!(checkmate_in(-Value::mated(3).score), Some(2));
        assert_eq!(checkmate_in(Value::mated(4).score), Some(-2));
        assert_eq!(checkmate_in(0), None);
        assert_eq!(checkmate_in(900), None);
    }

    #[test]
    fn every_mate_is_a_mate_and_no_eval_is() {
        for ply in [0, 1, 128, 900] {
            assert!(is_mate(Value::mated(ply).score));
            assert!(is_mate(-Value::mated(ply).score));
        }
        // material bounds an eval far below the threshold
        for eval in [0 as Score, 900, -2500, 9000] {
            assert!(!is_mate(eval));
        }
    }

    #[test]
    fn the_cap_holds_a_score_out_of_the_mate_window() {
        let capped = below_the_mate_window(Value::mated(0).score.abs());
        assert!(!is_mate(capped));
        // and an honest score passes through untouched
        assert_eq!(below_the_mate_window(37), 37);
        assert_eq!(below_the_mate_window(-40), -40);
    }
}
