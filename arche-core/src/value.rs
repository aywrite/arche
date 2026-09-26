// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What a node is worth, and whether that worth describes the position or
//! only the path taken to it.
//!
//! A repetition or a fifty move draw is true of the line that reached a
//! position, not of the position, so a zero read back down another line may
//! be a draw that line cannot reach. The transposition table refuses such a
//! score for a cutoff and keeps only the move, so the fact has to travel with
//! the score, through every negation, from the node that found it to the
//! node that stores it.
//!
//! Mate scores live here too. A mate is scored a fixed distance from
//! `CHECKMATE_SCORE`, further for a longer line, so a faster mate wins the
//! comparison. Everything within a thousand of it is a mate and nothing else
//! can be. The search asks `is_mate` before it prunes against a bound,
//! because a cutoff there would leave a faster mate unsearched, and caps what
//! a pass proved with `below_the_mate_window`, because a pass is not a move
//! and cannot force anything.

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
    /// somewhere below it.
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

    /// The side to move is mated, this many plies into the line. Clean, since
    /// a mate is a property of the position. The shorter the line the further
    /// the score sits below zero, so once a parent negates it the faster mate
    /// is the better one.
    pub(crate) fn mated(line_ply: usize) -> Self {
        Self::clean(-CHECKMATE_SCORE + line_ply as Score)
    }
}

/// What mate distance pruning leaves of a node's window.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum MateDistanceWindow {
    /// The window to search, never wider than the one handed in.
    Open { alpha: Score, beta: Score },
    /// The bounds crossed, and the node answers this score unsearched.
    Closed(Score),
}

/// Mate distance pruning. A node cannot be mated sooner than the ply it
/// stands at, and cannot mate sooner than the ply after it, so the window is
/// bounded by those two whatever the caller asked for. Where the bounds
/// cross, the caller already holds a line at least as good as the fastest
/// mate available here, and nothing below can improve on it, so the node
/// answers the narrowed alpha. The window handed in is never empty: alpha
/// is below beta, as it is at every node the search makes, and a debug
/// build asserts it in `can_narrow`.
///
/// Both bounds are mate scores themselves, so a window with no mate at
/// either end is left exactly as it arrived and cannot cross. That is
/// asked first, one bound a side (`can_narrow`), and it is what every other
/// node pays. Measured under callgrind at bench depth five when the guard
/// changed to the one-sided question: the question takes 8 instructions a
/// node, 0.257% of the total, where asking `is_mate` of both bounds took 12
/// and 0.380%. The two clamps and the cross test after it take 11, on the
/// nodes that pass it, about one in twenty there. The tree is the same
/// either way.
///
/// Where a mate is in the window, this ends every line longer than the mate
/// already found, which is what stops a proven mate being proved again a
/// ply deeper on each iteration.
pub(crate) fn mate_distance_window(
    mut alpha: Score,
    mut beta: Score,
    line_ply: usize,
) -> MateDistanceWindow {
    if can_narrow(alpha, beta) {
        alpha = alpha.max(Value::mated(line_ply).score);
        beta = beta.min(-Value::mated(line_ply + 1).score);
        if alpha >= beta {
            return MateDistanceWindow::Closed(alpha);
        }
    }
    MateDistanceWindow::Open { alpha, beta }
}

/// Whether mate distance pruning can move either bound: only a score of
/// being mated at alpha, or of mating at beta, can be narrowed. With alpha
/// below beta this answers as `is_mate(alpha) || is_mate(beta)` does. A
/// mate at alpha's top end puts one at beta's too, and one at beta's bottom
/// end puts one at alpha's, so the two one-sided tests are the only ones
/// that can decide.
fn can_narrow(alpha: Score, beta: Score) -> bool {
    debug_assert!(alpha < beta, "an empty window ({alpha}, {beta})");
    alpha < -CHECKMATE_THRESHOLD || beta > CHECKMATE_THRESHOLD
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
/// tainted score still stands on a comparison against that score.
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
    use super::{
        CHECKMATE_SCORE, CHECKMATE_THRESHOLD, MateDistanceWindow, Taint, Value,
        below_the_mate_window, can_narrow, checkmate_in, is_mate, mate_distance_window,
    };
    use crate::engine::MAX_PLY;
    use crate::misc::Score;
    use pretty_assertions::assert_eq;

    /// The plies a window is read at: the root, the first few, one in the
    /// middle, and the last the rule runs at and the rail past it.
    const PLIES: [usize; 7] = [0, 1, 2, 3, 64, MAX_PLY as usize - 1, MAX_PLY as usize];

    /// The bounds a window is built from: the root window's ends, scores
    /// nowhere near a mate, a few either side of the threshold, and each
    /// ply's fastest mate either way with a point either side of it.
    fn bounds() -> Vec<Score> {
        let mut bounds = vec![Score::MIN + 1, Score::MAX - 1, -900, -1, 0, 1, 900];
        for edge in [-CHECKMATE_THRESHOLD, CHECKMATE_THRESHOLD] {
            bounds.extend(edge - 2..=edge + 2);
        }
        for ply in PLIES {
            for mate in [Value::mated(ply).score, -Value::mated(ply + 1).score] {
                bounds.extend([mate - 1, mate, mate + 1, -mate]);
            }
        }
        bounds.sort_unstable();
        bounds.dedup();
        bounds
    }

    /// Every window the bounds make, as (alpha, beta) with alpha below beta.
    fn windows() -> Vec<(Score, Score)> {
        let bounds = bounds();
        let mut windows = Vec::new();
        for &alpha in &bounds {
            for &beta in bounds.iter().filter(|&&beta| beta > alpha) {
                windows.push((alpha, beta));
            }
        }
        windows
    }

    #[test]
    fn the_one_sided_question_answers_as_asking_both_bounds_would() {
        // every value within four of zero, of both thresholds, of both
        // mate scores and of the ends of the range, and every window they
        // make. Score::MIN is left out: no bound reaches it, and it is the
        // one value `abs` cannot take
        let mut values = Vec::new();
        for centre in [
            0,
            CHECKMATE_THRESHOLD,
            -CHECKMATE_THRESHOLD,
            CHECKMATE_SCORE,
            -CHECKMATE_SCORE,
        ] {
            values.extend(centre - 4..=centre + 4);
        }
        values.extend(Score::MIN + 1..=Score::MIN + 5);
        values.extend(Score::MAX - 5..=Score::MAX);
        for &alpha in &values {
            for &beta in values.iter().filter(|&&beta| beta > alpha) {
                assert_eq!(
                    can_narrow(alpha, beta),
                    is_mate(alpha) || is_mate(beta),
                    "({alpha}, {beta})"
                );
            }
        }
    }

    #[test]
    fn mate_distance_pruning_never_widens_a_window() {
        for ply in PLIES {
            for (alpha, beta) in windows() {
                match mate_distance_window(alpha, beta, ply) {
                    MateDistanceWindow::Open { alpha: a, beta: b } => {
                        assert!(
                            alpha <= a && a < b && b <= beta,
                            "({alpha}, {beta}) at ply {ply} opened as ({a}, {b})"
                        );
                    }
                    MateDistanceWindow::Closed(score) => assert!(
                        score >= alpha,
                        "({alpha}, {beta}) at ply {ply} closed on {score}, under alpha"
                    ),
                }
            }
        }
    }

    #[test]
    fn mate_distance_pruning_leaves_a_narrowed_window_where_it_is() {
        for ply in PLIES {
            for (alpha, beta) in windows() {
                let narrowed = mate_distance_window(alpha, beta, ply);
                if let MateDistanceWindow::Open { alpha, beta } = narrowed {
                    assert_eq!(
                        mate_distance_window(alpha, beta, ply),
                        narrowed,
                        "({alpha}, {beta}) at ply {ply}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_bounds_cross_where_the_fastest_mate_is_already_held() {
        // at alpha: the fastest mate this node can deliver is a ply away, so
        // a caller already holding it leaves nothing to find, and a caller
        // holding one a ply slower leaves that one ply to find
        for ply in PLIES {
            let mating = -Value::mated(ply + 1).score;
            assert_eq!(
                mate_distance_window(mating, Score::MAX - 1, ply),
                MateDistanceWindow::Closed(mating),
                "ply {ply}"
            );
            assert_eq!(
                mate_distance_window(mating - 1, Score::MAX - 1, ply),
                MateDistanceWindow::Open {
                    alpha: mating - 1,
                    beta: mating
                },
                "ply {ply}"
            );
        }
        // at beta: this node cannot be mated before its own ply, so a caller
        // that refutes it at that score or below has already refuted it
        for ply in PLIES {
            let mated = Value::mated(ply).score;
            assert_eq!(
                mate_distance_window(Score::MIN + 1, mated, ply),
                MateDistanceWindow::Closed(mated),
                "ply {ply}"
            );
            assert_eq!(
                mate_distance_window(Score::MIN + 1, mated + 1, ply),
                MateDistanceWindow::Open {
                    alpha: mated,
                    beta: mated + 1
                },
                "ply {ply}"
            );
        }
        // a crossing has a mate score at both ends, since the bound that
        // crosses is one and the other lies beyond it. So the other bound
        // cannot be an ordinary score where the window crosses; where it is
        // one, the mate at this end is still clamped to the ply's fastest
        // mate, which is where a crossing would start
        for ply in PLIES {
            let mated = Value::mated(ply).score;
            let mating = -Value::mated(ply + 1).score;
            for ordinary in [-900, 0, 900] {
                assert_eq!(
                    mate_distance_window(ordinary, Score::MAX - 1, ply),
                    MateDistanceWindow::Open {
                        alpha: ordinary,
                        beta: mating
                    },
                    "ply {ply}"
                );
                assert_eq!(
                    mate_distance_window(Score::MIN + 1, ordinary, ply),
                    MateDistanceWindow::Open {
                        alpha: mated,
                        beta: ordinary
                    },
                    "ply {ply}"
                );
            }
        }
    }

    #[test]
    fn a_window_with_no_mate_at_either_end_is_left_as_it_came() {
        let mut read = 0;
        for ply in PLIES {
            for (alpha, beta) in windows() {
                if is_mate(alpha) || is_mate(beta) {
                    continue;
                }
                assert_eq!(
                    mate_distance_window(alpha, beta, ply),
                    MateDistanceWindow::Open { alpha, beta },
                    "ply {ply}"
                );
                read += 1;
            }
        }
        assert!(read > 0, "no window without a mate was read");
    }

    #[test]
    fn nothing_wraps_at_the_root_or_at_the_rail() {
        // the extremes of both bounds at both ends of the line: each clamp
        // lands on the mate score the ply names and never past it
        for (ply, mated, mating) in [(0, -30_000, 29_999), (MAX_PLY as usize, -29_872, 29_871)] {
            assert_eq!(Value::mated(ply).score, mated);
            assert_eq!(-Value::mated(ply + 1).score, mating);
            assert_eq!(
                mate_distance_window(Score::MIN + 1, Score::MIN + 2, ply),
                MateDistanceWindow::Closed(mated),
                "ply {ply}"
            );
            assert_eq!(
                mate_distance_window(Score::MAX - 2, Score::MAX - 1, ply),
                MateDistanceWindow::Closed(Score::MAX - 2),
                "ply {ply}"
            );
            assert_eq!(
                mate_distance_window(Score::MIN + 1, 0, ply),
                MateDistanceWindow::Open {
                    alpha: mated,
                    beta: 0
                },
                "ply {ply}"
            );
            assert_eq!(
                mate_distance_window(0, Score::MAX - 1, ply),
                MateDistanceWindow::Open {
                    alpha: 0,
                    beta: mating
                },
                "ply {ply}"
            );
        }
    }

    #[test]
    fn the_root_window_narrows_to_the_bounds_its_ply_allows() {
        for ply in 0..=MAX_PLY as usize {
            assert_eq!(
                mate_distance_window(Score::MIN + 1, Score::MAX - 1, ply),
                MateDistanceWindow::Open {
                    alpha: Value::mated(ply).score,
                    beta: -Value::mated(ply + 1).score
                },
                "ply {ply}"
            );
        }
    }

    #[test]
    fn every_window_answers_the_clamped_bounds_or_their_crossing() {
        // the clamps with no guard in front of them, read against every
        // window: an open window is the two clamped bounds, and a crossing
        // answers the clamped alpha, which is the score `alpha_beta`
        // returns clean
        let mut crossed = 0;
        for ply in PLIES {
            let mated = Value::mated(ply).score;
            let mating = -Value::mated(ply + 1).score;
            for (alpha, beta) in windows() {
                let narrowed = (alpha.max(mated), beta.min(mating));
                let expected = if narrowed.0 >= narrowed.1 {
                    crossed += 1;
                    MateDistanceWindow::Closed(narrowed.0)
                } else {
                    MateDistanceWindow::Open {
                        alpha: narrowed.0,
                        beta: narrowed.1,
                    }
                };
                assert_eq!(
                    mate_distance_window(alpha, beta, ply),
                    expected,
                    "({alpha}, {beta}) at ply {ply}"
                );
            }
        }
        assert!(crossed > 0, "no window crossed");
    }

    #[test]
    fn negation_turns_the_score_and_leaves_the_taint() {
        assert_eq!(-Value::clean(30), Value::clean(-30));
        assert_eq!(-Value::tainted(0), Value::tainted(0));
        assert_eq!(-Value::tainted(-12), Value::tainted(12));
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
