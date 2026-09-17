// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What a full width node does with a quiet move its ordering put late:
//! search it whole, scout it a ply or two shallower first, or not search
//! it at all.
//!
//! `decide` answers with a `Verdict`: how many plies shallower to scout,
//! zero meaning the full search, or `Skip`. The scout itself is
//! `windowed`'s, in `engine.rs`.
//!
//! Two rungs stand behind the verdict. The first is the late move
//! reduction: a flat one ply scout for a quiet move searched after the
//! fourth at a node deep enough to keep a full width ply under it, with
//! the exemptions `reduces` lists. The second is the attention model, a
//! logistic regression over the reduction ledger's feature columns
//! quantized to fixed point, whose score is read against two thresholds:
//! under the first the scout runs two plies shallower, under the second
//! the move is dropped from the node.
//!
//! `features` derives the ledger's columns once, and the gate and the
//! ledger both read them, so the score that decided a move and the row
//! that says why cannot disagree.

use crate::board::Board;
use crate::census;
use crate::engine::{RootBounds, SearchConfig};
use crate::misc::Score;
use crate::ordering::MoveOrdering;
use crate::play::Play;
use crate::value::is_mate;

// How many plies shallower a late quiet move is scouted before it is
// searched at full depth. One and flat, so what the arm prices is the
// mechanism; a table by depth and move count is a match of its own.
pub(crate) const LATE_MOVE_REDUCTION: u8 = 1;
// Two more than the reduction, so the scout keeps a full width ply under
// it. Below this the scout is quiescence or a ply above it, and what it
// saves is noise.
pub(crate) const LATE_MOVE_MIN_DEPTH: u8 = LATE_MOVE_REDUCTION + 2;
// How many moves a node searches at full depth before a quiet move after
// them is scouted shallower. An opening value, not a tuned one.
pub(crate) const LATE_MOVE_THRESHOLD: usize = 4;
// How many plies shallower the deep reduction scouts a late quiet the
// attention model calls dead. Flat, for the one ply reduction's reason.
pub(crate) const DEEP_REDUCTION: u8 = 2;
// Two more than the reduction, on the late move floor's reasoning: the
// scout keeps a full width ply, so `depth - 1 - DEEP_REDUCTION` never
// falls under one.
pub(crate) const DEEP_REDUCTION_MIN_DEPTH: u8 = DEEP_REDUCTION + 2;

// The attention model the deep reduction and the pruning are gated by: a
// logistic regression over the reduction ledger's feature columns,
// quantized to fixed point at a scale of 1024, so the gate is an integer
// dot product and a compare. Fitted by `scripts/fit_attention.py` on
// 2026-09-06 over the ledger `arche reductions 8 every 1 cap 2000000`
// printed on the bench suite at commit 5217271: 193,143 rows labelled by
// the replay, split by fen-hash parity, holdout AUC 0.927. The same
// command on this tree records a different ledger (the deep reduction and
// the pruning did not exist at 5217271), so rerunning it makes a new fit
// rather than this one. The labels are R=1 labels gating an R=2 decision:
// the label (dead at full depth) is R-independent, the weaker scout's
// noise is what is approximated, and the SPRT priced the difference.
const ATTENTION_DEPTH: i64 = 198;
const ATTENTION_INDEX: i64 = -43;
const ATTENTION_BAND8_15: i64 = -475;
const ATTENTION_BAND16P: i64 = -25;
const ATTENTION_HIST_MILLI: i64 = 1;
const ATTENTION_KILLER: i64 = 1186;
const ATTENTION_TT_MOVE: i64 = 6;
const ATTENTION_TT_SCORE_ONLY: i64 = -75;
const ATTENTION_EVAL_BETA: i64 = 4;
const ATTENTION_ALPHA_GAP: i64 = -7;
const ATTENTION_GENERATED: i64 = -50;
const ATTENTION_SEARCHED: i64 = -43;
const ATTENTION_INTERCEPT: i64 = -2503;
// The model's 90% coverage operating point: at or under it the holdout's
// dead region held 89.6% of the sampled reductions at an attention rate of
// 0.077%. The training script read the region as strictly under the
// threshold; at or under admits the fourteen training rows sitting on it
// and moves neither holdout figure at that precision.
const DEEP_REDUCTION_THRESHOLD: i64 = -4637;
// The score at or under which a late quiet is not searched at all: the
// deadest quartile of a census of our own games (17,057,552 scouts from
// 1,428 positions out of 1,814 games at 10+0.1, recorded on master at
// c13a6ed), but of a model refitted to the census rather than of the
// weights above. Under these weights, over the rows this gate can reach
// (depth four and up, the move not giving check), it skips 39% of them at
// 0.031% attention, and the quartile would be -9513. The +18 over 2,000
// games is the gate at 39%, so moving it to the quartile is an arm of its
// own. The corpus rather than the bench because a skip spends the model's
// word where the games go.
const LATE_MOVE_PRUNING_THRESHOLD: i64 = -7954;

/// What the attention model reads about a late quiet at the gate, in the
/// reduction ledger's units. The index bands and the searched count are
/// derived from the index in the score, so a caller cannot build a row the
/// training table could not hold.
struct AttentionFeatures {
    /// The node's depth, the check extension included, as the ledger
    /// records it.
    depth: u8,
    /// The move's place among the searched moves, the ledger's index.
    index: usize,
    /// The move's history score in thousandths of the node's largest
    /// quiet history, or zero when no quiet has any.
    hist_milli: i64,
    /// Whether the move stands in one of the node's killer slots.
    killer: bool,
    /// What the node's table probe had given it.
    tt: census::Table,
    /// The node's static evaluation less its beta.
    eval_beta: i64,
    /// The node's alpha less the same evaluation.
    alpha_gap: i64,
    /// The moves the node generated.
    generated: usize,
}

/// The model's score for one late quiet: the quantized dot product plus
/// the intercept. Higher means more likely to deserve attention (a scout
/// that fails high, or a fail low the replay would call harmful). The
/// bands and the searched count are computed from the index as the
/// training table computed them, the collinear searched column included.
fn attention_score(f: &AttentionFeatures) -> i64 {
    let index = f.index as i64;
    let band8_15 = i64::from((8..=15).contains(&f.index));
    let band16p = i64::from(f.index >= 16);
    let (tt_move, tt_score_only) = match f.tt {
        census::Table::Miss => (0, 0),
        census::Table::Move => (1, 0),
        census::Table::ScoreOnly => (0, 1),
    };
    ATTENTION_DEPTH * i64::from(f.depth)
        + ATTENTION_INDEX * index
        + ATTENTION_BAND8_15 * band8_15
        + ATTENTION_BAND16P * band16p
        + ATTENTION_HIST_MILLI * f.hist_milli
        + ATTENTION_KILLER * i64::from(f.killer)
        + ATTENTION_TT_MOVE * tt_move
        + ATTENTION_TT_SCORE_ONLY * tt_score_only
        + ATTENTION_EVAL_BETA * f.eval_beta
        + ATTENTION_ALPHA_GAP * f.alpha_gap
        + ATTENTION_GENERATED * f.generated as i64
        + ATTENTION_SEARCHED * (index + 1)
        + ATTENTION_INTERCEPT
}

/// What the decision reads of the search. Three references rather than
/// the engine, so a test can stand one up from a board and an ordering
/// with no search behind it.
pub(crate) struct Search<'a> {
    pub(crate) board: &'a Board,
    pub(crate) ordering: &'a MoveOrdering,
    pub(crate) config: &'a SearchConfig,
}

/// The node one move is being decided at.
///
/// Built by the move loop for each move it asks about rather than once
/// for the node, because alpha rises as the node searches and the list is
/// sorted under the loop. What has to outlive an iteration is the two dear
/// features, which is why they are borrowed rather than held.
pub(crate) struct Node<'a> {
    /// The node's depth, the check extension included.
    pub(crate) depth: u8,
    /// The bounds the node stands in as this move is reached.
    pub(crate) alpha: Score,
    pub(crate) beta: Score,
    /// Which of those two the root opened with and no search has claimed.
    pub(crate) root_bounds: RootBounds,
    /// Whether the side to move is in check here.
    pub(crate) in_check: bool,
    /// The node's distance from the root, or none past the rail, which is
    /// where the killer slots are read.
    pub(crate) ply: Option<usize>,
    /// What the node's table probe gave it.
    pub(crate) tt: census::Table,
    /// The moves the node generated.
    pub(crate) moves: &'a [Play],
    /// The node's static evaluation, computed by the first move the gate
    /// scores and read back for the rest. The recorders take their own,
    /// so a node the gate scores nothing at never computes one.
    pub(crate) eval: &'a mut Option<i64>,
    /// The node's history denominator, computed by the first move the gate
    /// scores or the ledger stages, and read back for the rest. Held
    /// rather than walked again so that the row a staging records says
    /// what the gate scored: a child search between two of the node's
    /// moves can teach the history, and the two readings would part.
    pub(crate) history_max: &'a mut Option<i32>,
}

/// What the decision settled for one late quiet: how many plies
/// shallower the node scouts it before searching it, or that the node
/// does not search it at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Scouted this many plies shallower, and searched at the node's
    /// depth only when the scout comes back above alpha. Zero is no
    /// scout: the reduction did not apply.
    Scout(u8),
    /// The model prices the move in its deadest band: the move is not
    /// searched at all.
    Skip,
}

/// What the node knew about a late quiet at the decision, in the units
/// the reduction ledger prints: the history signed and its denominator
/// clamped at zero. The gate scores these and the ledger records them.
/// `AttentionFeatures` is the row the model sums: the bounds and the
/// depth beside a fraction read off these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Features {
    /// The move's place among the searched moves, the census's count: the
    /// table's move, when it was searched, is 0.
    pub(crate) index: usize,
    /// The moves the node generated, as the census records it.
    pub(crate) generated: usize,
    /// The history table's score for the move at the decision. Signed, as
    /// the census's column is. Every reduced move is quiet, so there is no
    /// class to price it by instead.
    pub(crate) history: i32,
    /// The largest history score among the node's generated quiets,
    /// clamped at zero, the denominator `history` is read against.
    pub(crate) history_max: i32,
    /// Whether the move stood in one of the node's killer slots.
    pub(crate) killer: bool,
    /// What the node's table probe had given it, the census's three-state.
    pub(crate) tt: census::Table,
}

impl Features {
    /// The history feature the weights were fitted on, zero to a
    /// thousand: a marked down move reads as one the history knows nothing
    /// about rather than pushing the score where the fit never saw. A
    /// largest at or under zero is a list nothing is known about, or every
    /// move marked down: the same nothing either way.
    fn hist_milli(&self) -> i64 {
        if self.history_max > 0 {
            i64::from(self.history.max(0)) * 1000 / i64::from(self.history_max)
        } else {
            0
        }
    }
}

/// The verdict for one move at a full width node. `searched` is how many
/// moves the node has searched already, the table's move among them. The
/// reduction's exemptions are asked first and cost nothing; a move they
/// turn down is searched whole and never priced, so a node that reduces
/// nothing pays for no feature.
#[inline]
pub(crate) fn decide(search: &Search, node: &mut Node, m: &Play, searched: usize) -> Verdict {
    if !reduces(search, node, m, searched) {
        return Verdict::Scout(0);
    }
    gate(search, node, m, searched)
}

/// Whether a move at a full width node is scouted a ply shallower before
/// it is searched at the node's depth: the late move reduction.
///
/// The exemptions are one rule: the reduction guesses that a move the
/// ordering put late is worth less than alpha, and is refused wherever
/// that guess has nothing to stand on. The first moves are searched
/// whole. A capture or a promotion was priced on material, not on its
/// place in the order; that takes in the losing captures, which sort
/// behind the quiets, and reducing them is a follow-up. A side in check
/// has evasions, not late moves. A window at either edge of the mate
/// scores is the margin family's exemption: a scout a ply short of the
/// mate it is asked about can only say no.
///
/// A beta that is still the root's own bound, which `root_bounds` says,
/// stands the reduction down as well, and that one is a decision. A node
/// whose beta is the root's is a node whose answer the root reports
/// rather than bounds, and the policy is to search it whole: a late move
/// trusted a ply short there costs the answer and not a bound. That
/// records what the engine does and does not claim it is right; an arm
/// that wants to reduce there lifts the flag and plays a match. Alpha's
/// bit is not read, because a node whose alpha is the root's has every
/// move failing low already, which is the reduction's guess rather than
/// something it needs proved.
///
/// A quiet move that gives check is reduced like any other: exempting
/// checks was measured and lost (docs/ROADMAP.md).
#[inline]
fn reduces(search: &Search, node: &Node, m: &Play, searched: usize) -> bool {
    admits(
        search.config,
        node.depth,
        searched,
        node.in_check,
        node.alpha,
        node.beta,
        node.root_bounds,
    ) && m.capture.is_none()
        && m.promote.is_none()
}

/// The half of `reduces` that reads the node and the count rather than
/// the move, on the node's own facts so that a move loop can ask it
/// before it builds a `Node`: where this is false every move is
/// searched whole, and there is nothing for `decide` to read.
#[inline]
pub(crate) fn admits(
    config: &SearchConfig,
    depth: u8,
    searched: usize,
    in_check: bool,
    alpha: Score,
    beta: Score,
    root_bounds: RootBounds,
) -> bool {
    config.late_move_reductions
        && depth >= LATE_MOVE_MIN_DEPTH
        && searched >= LATE_MOVE_THRESHOLD
        && !in_check
        && !is_mate(alpha)
        && !is_mate(beta)
        && !root_bounds.beta
}

/// Whether a move `reduces` already accepted is scouted two plies
/// shallower rather than one, or not searched at all: one score asked
/// two questions, the skip first. The node must be deep enough for the
/// deeper scout to keep its full width ply, a floor the skip inherits,
/// and the move must not give check: the exemption arm measured checks
/// as the scout's blind spot, so neither the deeper scout nor the skip
/// is offered one. The check test runs last because the slider probes
/// cost more than everything before them.
fn gate(search: &Search, node: &mut Node, m: &Play, searched: usize) -> Verdict {
    if (!search.config.deep_reductions && !search.config.late_move_pruning)
        || node.depth < DEEP_REDUCTION_MIN_DEPTH
    {
        return Verdict::Scout(LATE_MOVE_REDUCTION);
    }
    let eval = evaluation(search, node);
    let f = features(search, node, m, searched);
    let score = attention_score(&AttentionFeatures {
        depth: node.depth,
        index: f.index,
        hist_milli: f.hist_milli(),
        killer: f.killer,
        tt: f.tt,
        eval_beta: eval - i64::from(node.beta),
        alpha_gap: i64::from(node.alpha) - eval,
        generated: f.generated,
    });
    if search.config.late_move_pruning && score <= LATE_MOVE_PRUNING_THRESHOLD {
        return if search.board.gives_check(m) {
            Verdict::Scout(LATE_MOVE_REDUCTION)
        } else {
            Verdict::Skip
        };
    }
    if search.config.deep_reductions
        && score <= DEEP_REDUCTION_THRESHOLD
        && !search.board.gives_check(m)
    {
        return Verdict::Scout(DEEP_REDUCTION);
    }
    Verdict::Scout(LATE_MOVE_REDUCTION)
}

/// What the node knew about a move, derived once: the gate scores these
/// and the ledger records them.
pub(crate) fn features(search: &Search, node: &mut Node, m: &Play, searched: usize) -> Features {
    let history_max = denominator(search, node);
    let killers = node
        .ply
        .map_or([None, None], |ply| search.ordering.killers_at(ply));
    Features {
        index: searched,
        generated: node.moves.len(),
        history: search.ordering.history_score(search.board.active_color, m),
        // clamped where the move's own score is not, as the census's
        // denominator is: a fraction of a marked down largest would be a
        // number on no scale
        history_max: history_max.max(0),
        killer: killers.contains(&Some(*m)),
        tt: node.tt,
    }
}

/// The node's static evaluation, computed once and held on the node.
fn evaluation(search: &Search, node: &mut Node) -> i64 {
    *node
        .eval
        .get_or_insert_with(|| i64::from(crate::eval::eval(search.board)))
}

/// The largest history score among the node's generated quiets, the
/// denominator a move's own score is read against. Signed and not
/// clamped: `Features` clamps the printed column, and a list of nothing
/// but marked down moves is what `hist_milli`'s guard meets.
fn denominator(search: &Search, node: &mut Node) -> i32 {
    let moves = node.moves;
    *node.history_max.get_or_insert_with(|| {
        let quiet_history = |m: &Play| {
            if m.capture.is_none() && m.promote.is_none() {
                Some(search.ordering.history_score(search.board.active_color, m))
            } else {
                None
            }
        };
        moves.iter().filter_map(quiet_history).max().unwrap_or(0)
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ATTENTION_KILLER, AttentionFeatures, DEEP_REDUCTION, DEEP_REDUCTION_MIN_DEPTH,
        DEEP_REDUCTION_THRESHOLD, Features, LATE_MOVE_MIN_DEPTH, LATE_MOVE_PRUNING_THRESHOLD,
        LATE_MOVE_REDUCTION, LATE_MOVE_THRESHOLD, Node, Search, Verdict, attention_score, decide,
        features,
    };
    use crate::board::{Board, MoveList, fens, play_named};
    use crate::census::Table;
    use crate::engine::{MAX_PLY, RootBounds, SearchConfig};
    use crate::misc::Score;
    use crate::ordering::MoveOrdering;
    use crate::play::Play;
    use pretty_assertions::assert_eq;

    /// The reference with the late move reductions switched on and
    /// nothing else touched: whatever moves between this and the
    /// reference is the reduction.
    fn reducing() -> SearchConfig {
        SearchConfig {
            late_move_reductions: true,
            ..SearchConfig::reference()
        }
    }

    /// The reduction with the deep reduction on top: whatever moves
    /// between this and `reducing` is the model gated two ply scout.
    fn deep_reducing() -> SearchConfig {
        SearchConfig {
            late_move_reductions: true,
            deep_reductions: true,
            ..SearchConfig::reference()
        }
    }

    /// The deep reduction with the pruning on top: whatever moves between
    /// this and `deep_reducing` is the skip.
    fn pruning() -> SearchConfig {
        SearchConfig {
            late_move_reductions: true,
            deep_reductions: true,
            late_move_pruning: true,
            ..SearchConfig::reference()
        }
    }

    /// Everything a decision reads, owned by the test, with no engine and
    /// no search behind it.
    struct Stand {
        board: Board,
        ordering: MoveOrdering,
        config: SearchConfig,
        moves: MoveList,
        /// The node's facts besides its depth and bounds. A test that
        /// wants a killer slot, a table move or a root bound sets them.
        in_check: bool,
        root_bounds: RootBounds,
        ply: Option<usize>,
        tt: Table,
        eval: Option<i64>,
        history_max: Option<i32>,
    }

    impl Stand {
        fn new(fen: &str, config: SearchConfig) -> Self {
            let board = Board::from_fen(fen).unwrap();
            let moves = board.generate_moves();
            Self {
                board,
                ordering: MoveOrdering::new(),
                config,
                moves,
                in_check: false,
                root_bounds: RootBounds::NEITHER,
                ply: None,
                tt: Table::Miss,
                eval: None,
                history_max: None,
            }
        }

        /// The decision about one move. Each call is a node of its own:
        /// the held features are cleared first, so a test that teaches
        /// the memories between two questions gets an answer that read
        /// them.
        fn verdict(
            &mut self,
            m: &Play,
            searched: usize,
            depth: u8,
            alpha: Score,
            beta: Score,
        ) -> Verdict {
            self.eval = None;
            self.history_max = None;
            let search = Search {
                board: &self.board,
                ordering: &self.ordering,
                config: &self.config,
            };
            let mut node = Node {
                depth,
                alpha,
                beta,
                root_bounds: self.root_bounds,
                in_check: self.in_check,
                ply: self.ply,
                tt: self.tt,
                moves: &self.moves,
                eval: &mut self.eval,
                history_max: &mut self.history_max,
            };
            decide(&search, &mut node, m, searched)
        }

        /// What the node would tell the ledger about one move, read
        /// without clearing what a verdict left behind. The depth and the
        /// bounds stand at nothing, since the features read neither.
        fn features(&mut self, m: &Play, searched: usize) -> Features {
            let search = Search {
                board: &self.board,
                ordering: &self.ordering,
                config: &self.config,
            };
            let mut node = Node {
                depth: 0,
                alpha: 0,
                beta: 1,
                root_bounds: RootBounds::NEITHER,
                in_check: self.in_check,
                ply: self.ply,
                tt: self.tt,
                moves: &self.moves,
                eval: &mut self.eval,
                history_max: &mut self.history_max,
            };
            features(&search, &mut node, m, searched)
        }

        /// The position's static evaluation, which the bounds in the
        /// threshold tests are solved against.
        fn eval(&self) -> i64 {
            i64::from(crate::eval::eval(&self.board))
        }
    }

    /// Bounds that land a score exactly on a threshold. Alpha moves the
    /// score by seven a point and beta by four, so seven betas cover
    /// every residue of seven and one of them leaves alpha a whole number
    /// to close the rest.
    fn solved(score_at: impl Fn(Score, Score) -> i64, threshold: i64) -> (Score, Score) {
        for beta in 100..107 {
            let over = score_at(0, beta) - threshold;
            if over % 7 == 0 {
                return ((over / 7) as Score, beta);
            }
        }
        panic!("seven betas cover every residue of seven");
    }

    #[test]
    fn a_late_quiet_is_reduced_and_the_first_moves_are_not() {
        // the threshold is the count of moves searched before this one,
        // so the move after the fourth is the first reduced
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, reducing());
        let quiet = play_named(&s.board, "a4a5");
        for searched in 0..LATE_MOVE_THRESHOLD {
            assert_eq!(
                s.verdict(&quiet, searched, LATE_MOVE_MIN_DEPTH, -100, 100),
                Verdict::Scout(0),
                "reduced with {} moves searched",
                searched
            );
        }
        assert_eq!(
            s.verdict(&quiet, LATE_MOVE_THRESHOLD, LATE_MOVE_MIN_DEPTH, -100, 100),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
        assert_eq!(
            s.verdict(&quiet, LATE_MOVE_THRESHOLD + 10, MAX_PLY, -100, 100),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
        // and not under the floor, where the scout would be quiescence
        assert_eq!(
            s.verdict(
                &quiet,
                LATE_MOVE_THRESHOLD,
                LATE_MOVE_MIN_DEPTH - 1,
                -100,
                100
            ),
            Verdict::Scout(0)
        );
        // nor under the reference, whatever else is true of the move
        let mut off = Stand::new(fens::A_CAPTURE_AND_QUIETS, SearchConfig::reference());
        assert_eq!(
            off.verdict(&quiet, LATE_MOVE_THRESHOLD, LATE_MOVE_MIN_DEPTH, -100, 100),
            Verdict::Scout(0)
        );
    }

    #[test]
    fn a_capture_is_never_reduced() {
        // the same call the quiet is reduced under, with the capture in
        // its place. This one wins a pawn; a losing capture is exempt the
        // same way, since the test reads the capture and not the swap
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, reducing());
        let quiet = play_named(&s.board, "a4a5");
        let capture = play_named(&s.board, "a4e4");
        assert!(quiet.capture.is_none() && capture.capture.is_some());
        assert_eq!(
            s.verdict(&quiet, LATE_MOVE_THRESHOLD, LATE_MOVE_MIN_DEPTH, -100, 100),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
        assert_eq!(
            s.verdict(
                &capture,
                LATE_MOVE_THRESHOLD,
                LATE_MOVE_MIN_DEPTH,
                -100,
                100
            ),
            Verdict::Scout(0)
        );

        // a promotion is a pawn move with no victim, and is exempt on its
        // own account
        let mut s = Stand::new("7k/1P6/8/8/8/8/8/7K w - - 0 1", reducing());
        let promotes = play_named(&s.board, "b7b8q");
        assert!(promotes.capture.is_none() && promotes.promote.is_some());
        assert_eq!(
            s.verdict(
                &promotes,
                LATE_MOVE_THRESHOLD,
                LATE_MOVE_MIN_DEPTH,
                -100,
                100
            ),
            Verdict::Scout(0)
        );
    }

    #[test]
    fn a_node_in_check_reduces_nothing() {
        // in check and not, with everything else about the two questions
        // the same
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, reducing());
        let quiet = play_named(&s.board, "a4a5");
        s.in_check = true;
        assert_eq!(
            s.verdict(&quiet, LATE_MOVE_THRESHOLD, LATE_MOVE_MIN_DEPTH, -100, 100),
            Verdict::Scout(0)
        );
        s.in_check = false;
        assert_eq!(
            s.verdict(&quiet, LATE_MOVE_THRESHOLD, LATE_MOVE_MIN_DEPTH, -100, 100),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
    }

    #[test]
    fn the_mate_window_stands_the_reduction_down() {
        // either edge: a mate in hand as alpha, or one being proved
        // against the side to move as beta
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, reducing());
        let quiet = play_named(&s.board, "a4a5");
        assert_eq!(
            s.verdict(
                &quiet,
                LATE_MOVE_THRESHOLD,
                LATE_MOVE_MIN_DEPTH,
                29_500,
                29_501
            ),
            Verdict::Scout(0)
        );
        assert_eq!(
            s.verdict(
                &quiet,
                LATE_MOVE_THRESHOLD,
                LATE_MOVE_MIN_DEPTH,
                -29_501,
                -29_500
            ),
            Verdict::Scout(0)
        );
    }

    /// The same bounds four ways, so what moves is the flag and nothing
    /// else. Beta is the bound read: marked, the reduction is refused, and
    /// what alpha's bit says makes no difference. The fourth case pins
    /// that alpha's bit is never read.
    #[test]
    fn a_beta_that_is_still_the_roots_stands_the_reduction_down() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, reducing());
        let quiet = play_named(&s.board, "a4a5");
        let mut verdict = |root_bounds| {
            s.root_bounds = root_bounds;
            s.verdict(&quiet, LATE_MOVE_THRESHOLD, LATE_MOVE_MIN_DEPTH, -100, 100)
        };
        assert_eq!(
            verdict(RootBounds::NEITHER),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
        assert_eq!(verdict(RootBounds::BOTH), Verdict::Scout(0));
        assert_eq!(
            verdict(RootBounds {
                alpha: false,
                beta: true
            }),
            Verdict::Scout(0)
        );
        assert_eq!(
            verdict(RootBounds {
                alpha: true,
                beta: false
            }),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
    }

    /// Rows ported from the training table, asserted to the exact integer
    /// the python fit's quantized score gives them, so a sign or an off by
    /// one anywhere in `attention_score` fails here rather than in a
    /// match.
    #[test]
    fn the_attention_score_matches_the_training_fit_on_ported_rows() {
        let row = |depth, index, hist_milli, killer, tt, eval_beta, alpha_gap, generated| {
            attention_score(&AttentionFeatures {
                depth,
                index,
                hist_milli,
                killer,
                tt,
                eval_beta,
                alpha_gap,
                generated,
            })
        };
        // a killer deep in a lost window: dead despite its slot
        assert_eq!(
            row(4, 7, 146, true, Table::ScoreOnly, -1755, 1754, 34),
            -22097
        );
        // the row the threshold's percentile landed on exactly
        assert_eq!(row(6, 13, 0, false, Table::Move, -9, 8, 32), -4637);
        // the deadest row of the training half
        assert_eq!(
            row(3, 30, 0, false, Table::ScoreOnly, -1983, 1982, 33),
            -28088
        );
        // the most alive attention row: an eval standing over beta
        assert_eq!(row(4, 6, 10, false, Table::Miss, 303, -304, 16), 280);
        // a dead row of the depths the gate fires at
        assert_eq!(row(5, 29, 0, false, Table::ScoreOnly, -223, 222, 43), -8746);
        // the killer weight raises a row by exactly its coefficient, and
        // on the threshold row that is the whole distance out of the dead
        // region. It is a weight and not an exemption: the first row
        // above is a killer and dead all the same, so the model may deepen
        // the reduction of a killer whose bounds bury it
        let on_edge = row(6, 13, 0, false, Table::Move, -9, 8, 32);
        let as_killer = row(6, 13, 0, true, Table::Move, -9, 8, 32);
        assert_eq!(as_killer - on_edge, ATTENTION_KILLER);
        assert!(on_edge <= DEEP_REDUCTION_THRESHOLD);
        assert!(as_killer > DEEP_REDUCTION_THRESHOLD);
    }

    /// The gate driven with the bounds solved to land the score exactly
    /// on the threshold, and one over it: at or under fires, one over does
    /// not.
    #[test]
    fn the_deep_reduction_fires_at_the_threshold_and_not_one_over_it() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, deep_reducing());
        let quiet = play_named(&s.board, "a4a5");
        let eval = s.eval();
        let generated = s.moves.len();
        const SEARCHED: usize = 10;
        const DEPTH: u8 = 6;
        let score_at = |alpha: Score, beta: Score| {
            attention_score(&AttentionFeatures {
                depth: DEPTH,
                index: SEARCHED,
                hist_milli: 0,
                killer: false,
                tt: Table::Miss,
                eval_beta: eval - i64::from(beta),
                alpha_gap: i64::from(alpha) - eval,
                generated,
            })
        };
        let (alpha, beta) = solved(score_at, DEEP_REDUCTION_THRESHOLD);
        assert_eq!(score_at(alpha, beta), DEEP_REDUCTION_THRESHOLD);
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, alpha, beta),
            Verdict::Scout(DEEP_REDUCTION)
        );
        // a point of alpha up and two of beta down move the score one over
        assert_eq!(score_at(alpha + 1, beta - 2), DEEP_REDUCTION_THRESHOLD + 1);
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, alpha + 1, beta - 2),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
    }

    #[test]
    fn a_checking_quiet_is_never_reduced_two_plies() {
        // the rook to the eighth checks along the rank and the push to a5
        // does not, under bounds that put the score far under the
        // threshold, so the check test alone tells them apart
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, deep_reducing());
        let quiet = play_named(&s.board, "a4a5");
        let checks = play_named(&s.board, "a4a8");
        assert!(!s.board.gives_check(&quiet));
        assert!(s.board.gives_check(&checks));
        assert!(checks.capture.is_none());
        let (alpha, beta): (Score, Score) = (20_000, 20_001);
        assert!(!crate::value::is_mate(alpha));
        assert_eq!(
            s.verdict(&quiet, 10, 6, alpha, beta),
            Verdict::Scout(DEEP_REDUCTION)
        );
        assert_eq!(
            s.verdict(&checks, 10, 6, alpha, beta),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
    }

    /// The gate's history feature under signed entries: a marked down
    /// move reads as nothing, and a list of nothing but marked down moves
    /// has no denominator to divide by.
    #[test]
    fn a_marked_down_move_reads_the_gate_s_history_as_nothing() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, deep_reducing());
        let quiet = play_named(&s.board, "a4a5");
        let rival = play_named(&s.board, "a4a6");
        let color = s.board.active_color;
        const SEARCHED: usize = 10;
        const DEPTH: u8 = 6;
        // bounds solved so that the score with no history lands exactly on
        // the threshold
        let eval = s.eval();
        let generated = s.moves.len();
        let score_at = |alpha: Score, beta: Score, hist_milli: i64| {
            attention_score(&AttentionFeatures {
                depth: DEPTH,
                index: SEARCHED,
                hist_milli,
                killer: false,
                tt: Table::Miss,
                eval_beta: eval - i64::from(beta),
                alpha_gap: i64::from(alpha) - eval,
                generated,
            })
        };
        let (alpha, beta) = solved(
            |alpha, beta| score_at(alpha, beta, 0),
            DEEP_REDUCTION_THRESHOLD,
        );
        assert_eq!(score_at(alpha, beta, 0), DEEP_REDUCTION_THRESHOLD);
        // and a second pair one point over it. A feature of nothing fires
        // the gate at the first pair and not at the second, and a
        // thousandth either way moves the verdict at one of them, so the
        // two together say the feature is zero exactly rather than merely
        // small
        let (over_alpha, over_beta) = (alpha + 1, beta - 2);
        assert_eq!(
            score_at(over_alpha, over_beta, 0),
            DEEP_REDUCTION_THRESHOLD + 1
        );
        assert!(score_at(alpha, beta, 1000) > DEEP_REDUCTION_THRESHOLD);
        assert!(score_at(over_alpha, over_beta, -1000) <= DEEP_REDUCTION_THRESHOLD);
        // what a move the table knows nothing about reads, which is what
        // the two cases below have to match
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, alpha, beta),
            Verdict::Scout(DEEP_REDUCTION)
        );
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, over_alpha, over_beta),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );

        // the move holding the whole of the node's history reads the top
        // of the feature instead
        s.ordering.cutoff(color, &quiet, &[], 0, 8);
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, alpha, beta),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );

        // the same move marked down, under a rival that holds the history
        // instead, reads as the unknown move did at both pairs
        s.ordering.forget();
        s.ordering.cutoff(color, &rival, &[quiet], 0, 8);
        assert!(s.ordering.history_score(color, &quiet) < 0);
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, alpha, beta),
            Verdict::Scout(DEEP_REDUCTION)
        );
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, over_alpha, over_beta),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );

        // and with every quiet in the list marked down there is no
        // denominator and nothing is divided. The move that did the
        // marking is one no generator produces here, so nothing in the
        // list holds the bonus it earned
        s.ordering.forget();
        let elsewhere = Play::new(0, 1, None, None, false, false);
        assert!(!s.moves.contains(&elsewhere));
        let all_quiets: Vec<Play> = s
            .moves
            .iter()
            .filter(|m| m.capture.is_none())
            .copied()
            .collect();
        s.ordering.cutoff(color, &elsewhere, &all_quiets, 0, 8);
        assert!(
            all_quiets
                .iter()
                .all(|m| s.ordering.history_score(color, m) < 0)
        );
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, alpha, beta),
            Verdict::Scout(DEEP_REDUCTION)
        );
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, over_alpha, over_beta),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
    }

    #[test]
    fn the_deep_reduction_stands_down_off_switch_and_under_its_floor() {
        // under the reference's switch nothing deepens, and the refusal
        // comes before the node features, so a node the gate never fires
        // at never pays for the eval or the history scan
        let mut off = Stand::new(fens::A_CAPTURE_AND_QUIETS, reducing());
        let quiet = play_named(&off.board, "a4a5");
        let (alpha, beta): (Score, Score) = (20_000, 20_001);
        assert_eq!(
            off.verdict(&quiet, 10, 6, alpha, beta),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
        assert!(
            off.eval.is_none() && off.history_max.is_none(),
            "the refused gate computed the features"
        );
        // under the floor the scout would lose its full width ply
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, deep_reducing());
        assert_eq!(
            s.verdict(&quiet, 10, DEEP_REDUCTION_MIN_DEPTH - 1, alpha, beta),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
        assert!(
            s.eval.is_none() && s.history_max.is_none(),
            "the refused gate computed the features"
        );
        // and at the floor with the same bounds it fires, filling the
        // node features for the moves after it
        assert_eq!(
            s.verdict(&quiet, 10, DEEP_REDUCTION_MIN_DEPTH, alpha, beta),
            Verdict::Scout(DEEP_REDUCTION)
        );
        assert!(
            s.eval.is_some() && s.history_max.is_some(),
            "the fired gate left nothing to reuse"
        );
    }

    /// The gate driven with the bounds solved to land the score exactly
    /// on the pruning threshold, and one over it: at or under the move
    /// is skipped, one over it falls through to the deep reduction,
    /// whose threshold it is still far under.
    #[test]
    fn the_pruning_fires_at_its_threshold_and_not_one_over_it() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, pruning());
        let quiet = play_named(&s.board, "a4a5");
        let eval = s.eval();
        let generated = s.moves.len();
        const SEARCHED: usize = 10;
        const DEPTH: u8 = 6;
        let score_at = |alpha: Score, beta: Score| {
            attention_score(&AttentionFeatures {
                depth: DEPTH,
                index: SEARCHED,
                hist_milli: 0,
                killer: false,
                tt: Table::Miss,
                eval_beta: eval - i64::from(beta),
                alpha_gap: i64::from(alpha) - eval,
                generated,
            })
        };
        let (alpha, beta) = solved(score_at, LATE_MOVE_PRUNING_THRESHOLD);
        assert_eq!(score_at(alpha, beta), LATE_MOVE_PRUNING_THRESHOLD);
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, alpha, beta),
            Verdict::Skip
        );
        // a point of alpha up and two of beta down move the score one over
        assert_eq!(
            score_at(alpha + 1, beta - 2),
            LATE_MOVE_PRUNING_THRESHOLD + 1
        );
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, alpha + 1, beta - 2),
            Verdict::Scout(DEEP_REDUCTION)
        );
    }

    #[test]
    fn a_checking_quiet_is_never_skipped() {
        // the deep exemption's two moves under bounds that price both
        // far under the pruning threshold: the push is skipped and the
        // check is not, and the check is not handed to the deep
        // reduction either, which carries the same exemption
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, pruning());
        let quiet = play_named(&s.board, "a4a5");
        let checks = play_named(&s.board, "a4a8");
        assert!(s.board.gives_check(&checks));
        let (alpha, beta): (Score, Score) = (20_000, 20_001);
        assert_eq!(s.verdict(&quiet, 10, 6, alpha, beta), Verdict::Skip);
        assert_eq!(
            s.verdict(&checks, 10, 6, alpha, beta),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
    }

    #[test]
    fn the_pruning_stands_down_off_switch_and_scores_without_the_deep_reduction() {
        // the same dead score with the pruning off deepens the scout
        // rather than dropping the move, which is what keeps the two
        // arms separable in an ablation
        let mut deep = Stand::new(fens::A_CAPTURE_AND_QUIETS, deep_reducing());
        let quiet = play_named(&deep.board, "a4a5");
        let (alpha, beta): (Score, Score) = (20_000, 20_001);
        assert_eq!(
            deep.verdict(&quiet, 10, 6, alpha, beta),
            Verdict::Scout(DEEP_REDUCTION)
        );
        // with the pruning alone on, the model is still asked and the
        // move is still skipped: the score serves whichever of the two
        // gates wants it
        let mut alone = Stand::new(
            fens::A_CAPTURE_AND_QUIETS,
            SearchConfig {
                late_move_reductions: true,
                late_move_pruning: true,
                ..SearchConfig::reference()
            },
        );
        assert_eq!(alone.verdict(&quiet, 10, 6, alpha, beta), Verdict::Skip);
        // and under the shared floor nothing is scored at all
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, pruning());
        assert_eq!(
            s.verdict(&quiet, 10, DEEP_REDUCTION_MIN_DEPTH - 1, alpha, beta),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
        assert!(
            s.eval.is_none() && s.history_max.is_none(),
            "the refused gate computed the features"
        );
    }

    /// The features the gate scores are the features the ledger records.
    /// The pair that can part is the history and its denominator, one
    /// signed and one clamped, so this teaches the move part of the
    /// node's history and holds the gate's edge to the score the recorded
    /// row gives.
    #[test]
    fn the_gate_scores_the_features_the_ledger_records() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, deep_reducing());
        let quiet = play_named(&s.board, "a4a5");
        let rival = play_named(&s.board, "a4a6");
        let color = s.board.active_color;
        // the bonus is the square of the depth it was earned at, so the
        // move holds sixteen of the node's twenty five and the fraction
        // is neither nothing nor the whole. The slot it took at ply zero
        // is the killer column, which the node reads here
        s.ply = Some(0);
        s.ordering.cutoff(color, &quiet, &[], 0, 4);
        s.ordering.cutoff(color, &rival, &[], 1, 5);
        const SEARCHED: usize = 10;
        const DEPTH: u8 = 6;
        let eval = s.eval();
        let generated = s.moves.len();
        let f = s.features(&quiet, SEARCHED);
        assert_eq!(f.index, SEARCHED);
        assert_eq!(f.generated, generated);
        assert_eq!(f.history, 16);
        assert_eq!(f.history_max, 25);
        assert!(f.killer);
        assert_eq!(f.tt, Table::Miss);
        assert_eq!(f.hist_milli(), 640);
        // the row the ledger would record, scored, with the bounds solved
        // to land it exactly on the deep threshold: a place the gate
        // reaches only by reading this same fraction
        let score_at = |alpha: Score, beta: Score| {
            attention_score(&AttentionFeatures {
                depth: DEPTH,
                index: f.index,
                hist_milli: f.hist_milli(),
                killer: f.killer,
                tt: f.tt,
                eval_beta: eval - i64::from(beta),
                alpha_gap: i64::from(alpha) - eval,
                generated: f.generated,
            })
        };
        let (alpha, beta) = solved(score_at, DEEP_REDUCTION_THRESHOLD);
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, alpha, beta),
            Verdict::Scout(DEEP_REDUCTION)
        );
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, alpha + 1, beta - 2),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
    }

    /// The denominator is read once for the node and held: a row records
    /// the largest the gate scored against, not the largest by the time
    /// the staging ran, and a child search between two of a node's moves
    /// can teach the history.
    #[test]
    fn the_history_denominator_is_read_once_for_the_node() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, deep_reducing());
        let quiet = play_named(&s.board, "a4a5");
        let rival = play_named(&s.board, "a4a6");
        let color = s.board.active_color;
        s.ordering.cutoff(color, &rival, &[], 1, 5);
        assert_eq!(s.features(&quiet, 10).history_max, 25);
        // taught again and deeper, as a child search between two of the
        // node's moves would teach it
        s.ordering.cutoff(color, &rival, &[], 1, 8);
        let taught = s.ordering.history_score(color, &rival);
        assert!(taught > 25, "the second cutoff taught nothing: {}", taught);
        assert_eq!(s.features(&quiet, 10).history_max, 25);
        // and the node after this one reads what the history now holds
        s.history_max = None;
        assert_eq!(s.features(&quiet, 10).history_max, taught);
    }
}
