// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The reverse futility guard: which of the cuts the margin makes to decline.
//!
//! A cut the margin would make is scored by a logistic model of whether the
//! reference search, run on the same node, disagrees with it. The inputs are
//! the node's depth, the phase, how far the eval stands past the margin (the
//! slack), and the pair term from the side to move, signed and absolute. A
//! cut whose score reaches the threshold is declined, and the node is then
//! searched as if the margin had not cleared beta.
//!
//! The weights were fitted on about four million cuts the margin made on
//! positions from our own games, and the threshold declines about 5% of
//! them there. They are fitted, not proven: that the declined cuts are the
//! ones worth searching is what a match says.
//!
//! The score is integer throughout, the log of the slack included, so a
//! decision cannot move between platforms and the bench is the same on
//! every machine.

use crate::misc::Score;

// The scale the weights are held at, 2^20.
const F: i64 = 1 << 20;
// The weights, each the fitted coefficient times `F`. The fit read the
// depth as one intercept a depth, the phase over 24, the log of one plus
// the slack over five, and the pair term over a hundred. The terms below
// put every product on one scale (2400 F^2), which is why each carries its
// own factor.
const DEPTH_WEIGHTS: [i64; 4] = [-2_533_338, -1_453_105, -1_076_689, -675_126];
const PHASE_WEIGHT: i64 = -1_899_492;
const LOG_SLACK_WEIGHT: i64 = -3_275_535;
const PAIR_WEIGHT: i64 = -617_421;
const ABS_PAIR_WEIGHT: i64 = -885_941;
// The score at or above which a cut is declined, on the same scale: the
// fit's threshold of about -4.607 (a modelled disagreement rate of about
// 1%), set where 5% of the cuts it was fitted on are declined.
const THRESHOLD: i64 = -12_156_907_328_710_598;

/// The deepest node the weights cover, one intercept a depth from one.
pub(crate) const MAX_DEPTH: usize = DEPTH_WEIGHTS.len();

// The slack the table reads, held to its last entry.
const SLACK_LIMIT: i64 = 4095;
// The size the pair term is held to, for the overflow and nothing else.
const PAIR_LIMIT: i32 = 1 << 15;

/// `round(log1p(s) / 5 x 2^20)` for every slack the guard reads.
static LOG1P: [i64; SLACK_LIMIT as usize + 1] = include!("risk_log1p.rs");

/// The model's score of a cut. `depth` is the node's, one to `MAX_DEPTH`.
/// `phase` is the capped phase the evaluation tapers by (24 is the
/// opening's pieces). `slack` is how far the eval stands above beta past
/// the margin, `eval - beta - margin x depth`, never negative where the
/// margin cut; past 4095 it reads as 4095. `pair` is the pair term from the
/// side to move.
///
/// The pair term is held to a size of 2^15, far past any a position
/// produces, so the two pair products are each under 2^61 and the sum fits
/// an i64 whatever the factor table.
pub(crate) fn score(depth: u8, phase: i32, slack: i64, pair: i32) -> i64 {
    debug_assert!((1..=MAX_DEPTH).contains(&(depth as usize)), "{depth}");
    let slack = slack.clamp(0, SLACK_LIMIT) as usize;
    let phase = i64::from(phase);
    let pair = i64::from(pair.clamp(-PAIR_LIMIT, PAIR_LIMIT));
    DEPTH_WEIGHTS[depth as usize - 1] * 2400 * F
        + PHASE_WEIGHT * phase * 100 * F
        + LOG_SLACK_WEIGHT * LOG1P[slack] * 2400
        + PAIR_WEIGHT * pair * 24 * F
        + ABS_PAIR_WEIGHT * pair.abs() * 24 * F
}

/// Whether the guard declines a cut the margin would make, from the
/// node's depth, the capped phase, the eval and beta the margin read, and
/// the pair term from the side to move.
pub(crate) fn declines(
    depth: u8,
    phase: i32,
    eval: Score,
    beta: Score,
    margin: Score,
    pair: i32,
) -> bool {
    let slack = i64::from(eval) - i64::from(beta) - i64::from(margin) * i64::from(depth);
    score(depth, phase, slack, pair) >= THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::{F, LOG1P, THRESHOLD, declines, score};

    /// The table is the formula its header gives. A float is fine here: the
    /// test only says the generated numbers are the ones claimed.
    #[test]
    fn the_table_is_the_log_it_claims() {
        for (s, &entry) in LOG1P.iter().enumerate() {
            let expected = ((s as f64).ln_1p() / 5.0 * F as f64).round() as i64;
            assert_eq!(entry, expected, "slack {s}");
        }
        assert_eq!(&LOG1P[..3], &[0, 145_363, 230_396]);
        assert_eq!(LOG1P[4095], 1_744_362);
    }

    /// The score against the formula worked by hand, at three nodes. Each
    /// sum is the weights times the inputs, with the log of the slack read
    /// off the table, and its total is written out besides.
    #[test]
    fn the_score_is_the_fitted_sum() {
        // depth one, the opening's phase, a slack of 20 and a pair term of
        // +30. The opening's phase and a positive pair term both lower the
        // score, and this cut is kept
        let accepted = -2_533_338 * 2400 * F
            + -1_899_492 * 24 * 100 * F
            + -3_275_535 * 638_483 * 2400
            + -617_421 * 30 * 24 * F
            + -885_941 * 30 * 24 * F;
        assert_eq!(accepted, -17_309_878_457_372_640);
        assert_eq!(score(1, 24, 20, 30), accepted);
        assert!(accepted < THRESHOLD);
        assert!(!declines(1, 24, 220, 100, 100, 30));

        // depth three, phase four, a slack of 2 and a pair term of -40: deep,
        // late and barely past the margin, so the cut is declined. A negative
        // pair term raises the signed term and lowers the absolute one
        let declined = -1_076_689 * 2400 * F
            + -1_899_492 * 4 * 100 * F
            + -3_275_535 * 230_396 * 2400
            + -617_421 * -40 * 24 * F
            + -885_941 * 40 * 24 * F;
        assert_eq!(declined, -5_587_790_747_913_600);
        assert_eq!(score(3, 4, 2, -40), declined);
        assert!(declined >= THRESHOLD);
        assert!(declines(3, 4, 302, 0, 100, -40));

        // depth four, phase ten, a slack past the table's end and a pair term
        // of -5: the slack reads the last entry, and the cut is kept
        let far = -675_126 * 2400 * F
            + -1_899_492 * 10 * 100 * F
            + -3_275_535 * 1_744_362 * 2400
            + -617_421 * -5 * 24 * F
            + -885_941 * 5 * 24 * F;
        assert_eq!(far, -17_437_484_648_884_800);
        assert_eq!(score(4, 10, 9000, -5), far);
        assert_eq!(score(4, 10, 4095, -5), far);
        assert!(far < THRESHOLD);
        assert!(!declines(4, 10, 9400, 0, 100, -5));
    }
}
