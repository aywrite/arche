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

use crate::misc::Score;

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
    /// for a node reporting what it settled on: the taint of a node is the
    /// taint of every child it looked at, not of the one it chose.
    pub fn with_taint(score: Score, tainted: bool) -> Self {
        Self { score, tainted }
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

#[cfg(test)]
mod tests {
    use super::Value;
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
}
