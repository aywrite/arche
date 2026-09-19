// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What a full width node does with a quiet move its ordering put late:
//! search it whole, scout it some plies shallower first, or not search it
//! at all.
//!
//! `decide` answers with a `Verdict`: how many plies shallower to scout,
//! zero meaning the full search, or `Skip`. The scout itself is
//! `windowed`'s, in `engine.rs`.
//!
//! Three rungs stand behind the verdict. The first is the late move
//! reduction: a scout for a quiet move searched after the fourth at a node
//! deep enough to keep a full width ply under it, with the exemptions
//! `reduces` lists. The second is the attention model, a logistic
//! regression over the reduction ledger's feature columns quantized to
//! fixed point, whose score is read against two thresholds: under the
//! first the scout runs a ply deeper than it otherwise would, under the
//! second the move is dropped from the node. Each threshold is read only
//! where its own rule switch is off. Under the rules, which the default
//! carries both of, the deeper scout and the skip are decided by the
//! move's index against a floor that rises with the node's depth, and the
//! default derives no score at all. That ply is relative because the model
//! ranks how dead a move is and never names a depth. The third is
//! `amount`, which reads how many plies a scout gives up off a table by
//! the node's depth and the move's index.
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
// searched at full depth, off the reduction table: one and flat, which is
// also what the table reads at the corner most reduced scouts sit in.
pub(crate) const LATE_MOVE_REDUCTION: u8 = 1;
// Two more than the reduction, so the scout keeps a full width ply under
// it. Below this the scout is quiescence or a ply above it, and what it
// saves is noise.
pub(crate) const LATE_MOVE_MIN_DEPTH: u8 = LATE_MOVE_REDUCTION + 2;
// How many moves a node searches at full depth before a quiet move after
// them is scouted shallower. An opening value, not a tuned one.
pub(crate) const LATE_MOVE_THRESHOLD: usize = 4;
// How many plies shallower the deep reduction scouts a late quiet the gate
// deepens, off the table: a ply over the flat amount.
pub(crate) const DEEP_REDUCTION: u8 = 2;
// Two more than the reduction, on the late move floor's reasoning: the
// scout keeps a full width ply, so `depth - 1 - DEEP_REDUCTION` never
// falls under one.
pub(crate) const DEEP_REDUCTION_MIN_DEPTH: u8 = DEEP_REDUCTION + 2;
// What the gate's word is worth over the flat amount: one ply, the
// difference between the two constants above. Written as that difference
// so the table below cannot drift away from the pair it replaced.
const DEEP_REDUCTION_BONUS: u8 = DEEP_REDUCTION - LATE_MOVE_REDUCTION;
// ln(x) at a scale of 1024, for every index the table below has. Held as
// integers, so the table is built at compile time and two targets cannot
// disagree about it. Zero at both ends of the bottom, which puts the
// table's first row and column on its floor of one.
#[rustfmt::skip]
const LN: [u16; 64] = [
        0,     0,   710,  1125,  1420,  1648,  1835,  1993,
     2129,  2250,  2358,  2455,  2545,  2627,  2702,  2773,
     2839,  2901,  2960,  3015,  3068,  3118,  3165,  3211,
     3254,  3296,  3336,  3375,  3412,  3448,  3483,  3516,
     3549,  3580,  3611,  3641,  3670,  3698,  3725,  3751,
     3777,  3803,  3827,  3851,  3875,  3898,  3921,  3943,
     3964,  3985,  4006,  4026,  4046,  4066,  4085,  4104,
     4122,  4140,  4158,  4175,  4193,  4210,  4226,  4243,
];
// 2.6 at the scale above squared, which is what sets how fast the
// reduction grows with the two logs multiplied. The conventional figure,
// and what moves it is a match rather than the bench. The half is added
// before the divide so it rounds to nearest rather than toward zero.
const REDUCTION_DIV: u32 = 2_726_298;
const REDUCTION_HALF: u32 = REDUCTION_DIV / 2;

/// How many plies shallower a late quiet is scouted, by the node's depth
/// and the move's place in the order, before the gate's word and the floor
/// are applied.
///
/// Built at compile time from `LN` and the divisor, so the formula stays in
/// the source rather than four thousand numbers and the search pays one
/// array read. The floor of one is what holds the table's shallow corner at
/// the flat ply the search applies there already.
const REDUCTION: [[u8; 64]; 64] = reduction_table();

const fn reduction_table() -> [[u8; 64]; 64] {
    let mut table = [[1u8; 64]; 64];
    let mut depth = 0;
    while depth < 64 {
        let mut index = 0;
        while index < 64 {
            let product = LN[depth] as u32 * LN[index] as u32;
            let value = (product + REDUCTION_HALF) / REDUCTION_DIV;
            if value > 1 {
                table[depth][index] = value as u8;
            }
            index += 1;
        }
        depth += 1;
    }
    table
}

// The attention model each rung is gated by where its own rule switch is
// off: a logistic regression over the reduction ledger's feature columns,
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
// The score at or under which a late quiet is not searched at all where
// `index_rule_pruning` is off: the deadest quartile of a census of our own
// games (17,057,552 scouts from 1,428 positions out of 1,814 games at
// 10+0.1, recorded on master at c13a6ed), but of a model refitted to the
// census rather than of the weights above. Under these weights, over the
// rows this gate can reach (depth four and up, the move not giving check),
// it skips 39% of them at 0.031% attention, and the quartile would be
// -9513. The +18 over 2,000 games is the gate at 39%, so moving it to the
// quartile is an arm of its own. The corpus rather than the bench because
// a skip spends the model's word where the games go.
const LATE_MOVE_PRUNING_THRESHOLD: i64 = -7954;
// The index the deep reduction's rule wants a move to have reached, and
// how much further along the order per ply of depth over the floor the
// deeper scout starts at. Chosen offline on the training half of a ledger
// `arche reductions 8 every 4 cap 2000000 epd <root>` over 1,428 roots
// from 1,340 of our own games at 10+0.1 (corpus sha256
// c5fd032b7e0992d22dc38e29be1528080533dcd9387d0d0d8d36808e73d4faa0):
// 929,539 depth four and up scouted non-checking rows, split by source
// game into 457,908 and 471,631. Of 36 candidates (floor 4 to 14, slope 0
// to 3) this pair covers within three points of the model at the lowest
// attention rate, 77.18% at 0.664% against 79.07% at 0.448%. Held out and
// read once after the choice, the model deepens 76.84% at 0.502% attention
// (0.312% harmful) and this rule 77.17% at 0.728% (0.375% harmful).
pub(crate) const DEEP_INDEX_FLOOR: usize = 8;
pub(crate) const DEEP_INDEX_SLOPE: usize = 1;
// The index the skip's rule wants a move to have reached, and how much
// further along the order per ply of depth over the one the gate starts
// at. Chosen on the ledger and the split above, over the 1,580,640 depth
// four and up non-checking rows the skip decides, the scouted and the
// skipped together. Of 36 candidates (floor 8 to 28, slope 0 to 3) this
// pair covers within three points of the model at the lowest attention
// rate, 38.54% of the rows at 0.535% against the model's 40.83% at
// 0.037%. Held out and read once after the choice, the model skips 41.54%
// at 0.070% attention and this rule 37.67% at 0.578%, a rate 8.3 times
// the model's. Depth and index cannot find the model's dead region: every
// pair in the family reads about half a percent at every coverage. The
// recomputed model decision agreed with the ledger's own skip word on
// every row.
pub(crate) const PRUNE_INDEX_FLOOR: usize = 12;
pub(crate) const PRUNE_INDEX_SLOPE: usize = 2;

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
    /// The gate prices the move as dead: the move is not searched at
    /// all.
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

/// Whether a move at a full width node is scouted shallower before it is
/// searched at the node's depth: the late move reduction.
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

/// Whether a move `reduces` already accepted is scouted a ply shallower
/// than the amount alone would give it, or not searched at all: the skip is
/// asked first, off whichever rule `skips` reads, and the deeper scout
/// after it, off whichever rule `deepens` reads. The node must be deep
/// enough for the deeper scout to keep its full width ply, a floor the
/// skip inherits, and the move must not give check: the exemption arm
/// measured checks as the scout's blind spot, so neither the deeper scout
/// nor the skip is offered one. The check test runs last because the
/// slider probes cost more than everything before them.
///
/// The model is scored only for a rung that still reads it. With both
/// rules on, which is the default, the move costs no evaluation, no
/// history scan and no dot product.
fn gate(search: &Search, node: &mut Node, m: &Play, searched: usize) -> Verdict {
    let config = search.config;
    if (!config.deep_reductions && !config.late_move_pruning)
        || node.depth < DEEP_REDUCTION_MIN_DEPTH
    {
        return Verdict::Scout(amount(config, node.depth, searched, 0));
    }
    let score = reads_the_model(config).then(|| {
        let eval = evaluation(search, node);
        let f = features(search, node, m, searched);
        attention_score(&AttentionFeatures {
            depth: node.depth,
            index: f.index,
            hist_milli: f.hist_milli(),
            killer: f.killer,
            tt: f.tt,
            eval_beta: eval - i64::from(node.beta),
            alpha_gap: i64::from(node.alpha) - eval,
            generated: f.generated,
        })
    });
    if config.late_move_pruning && skips(config, node.depth, searched, score) {
        return if search.board.gives_check(m) {
            Verdict::Scout(amount(config, node.depth, searched, 0))
        } else {
            Verdict::Skip
        };
    }
    if config.deep_reductions
        && deepens(config, node.depth, searched, score)
        && !search.board.gives_check(m)
    {
        return Verdict::Scout(amount(config, node.depth, searched, DEEP_REDUCTION_BONUS));
    }
    Verdict::Scout(amount(config, node.depth, searched, 0))
}

/// Whether either rung still reads the attention model. A rung whose rule
/// switch is on reads the node's depth and the move's index alone, and
/// where both are on nothing at this gate reads the score, so nothing
/// derives it.
fn reads_the_model(config: &SearchConfig) -> bool {
    (config.late_move_pruning && !config.index_rule_pruning)
        || (config.deep_reductions && !config.deep_index_rule)
}

/// Whether a move the gate has accepted is not searched at all, the
/// checking exemption aside.
///
/// Under `index_rule_pruning` the decision reads the node's depth and the
/// move's index and nothing else, which is what the arm asks: whether the
/// model's other features earn their place at this gate. Off the rule the
/// model's deadest band decides, as it did. The floor the index is read
/// against is the deeper scout's own, so the two rules count from the same
/// depth.
fn skips(config: &SearchConfig, depth: u8, searched: usize, score: Option<i64>) -> bool {
    debug_assert!(
        depth >= DEEP_REDUCTION_MIN_DEPTH,
        "the skip is only asked about at the deeper scout's floor and above"
    );
    if config.index_rule_pruning {
        searched
            >= PRUNE_INDEX_FLOOR + PRUNE_INDEX_SLOPE * usize::from(depth - DEEP_REDUCTION_MIN_DEPTH)
    } else {
        score.expect("the gate scores every move a threshold decides")
            <= LATE_MOVE_PRUNING_THRESHOLD
    }
}

/// Whether the gate gives a move it has already accepted the deeper
/// scout's extra ply.
///
/// Under `deep_index_rule` the decision reads the node's depth and the
/// move's index and nothing else, which is what the arm asks: whether the
/// model's other features earn their place at this gate. Off the rule the
/// model's threshold decides, as it did. Neither the amount that extra ply
/// is worth nor the skip's own threshold moves either way.
fn deepens(config: &SearchConfig, depth: u8, searched: usize, score: Option<i64>) -> bool {
    debug_assert!(
        depth >= DEEP_REDUCTION_MIN_DEPTH,
        "the deeper scout is only asked about at a depth it keeps a ply under"
    );
    if config.deep_index_rule {
        searched
            >= DEEP_INDEX_FLOOR + DEEP_INDEX_SLOPE * usize::from(depth - DEEP_REDUCTION_MIN_DEPTH)
    } else {
        score.expect("the gate scores every move a threshold decides") <= DEEP_REDUCTION_THRESHOLD
    }
}

/// How many plies shallower the scout runs. `bonus` is the gate's ply,
/// which stands over the flat amount rather than naming a depth of its
/// own, for the reason the module doc gives.
///
/// Off the table the two constants are read as they were. On it the amount
/// grows with the node's depth and the move's index.
///
/// The clamp is the floor the two minimum depths used to guarantee on
/// their own: `depth - 1 - reduction` never falls under one, so the scout
/// keeps a full width ply. It is applied after the bonus rather than
/// before, because what has to stay above zero is the depth the scout
/// actually runs at. Clamping is chosen over raising the minimum depths so
/// that the eligible population does not move with the amount.
fn amount(config: &SearchConfig, depth: u8, searched: usize, bonus: u8) -> u8 {
    debug_assert!(
        depth >= LATE_MOVE_MIN_DEPTH,
        "a move is only reduced at a depth the scout keeps a ply under"
    );
    let base = if config.reduction_table {
        REDUCTION[usize::from(depth).min(63)][searched.min(63)]
    } else {
        LATE_MOVE_REDUCTION
    };
    (base + bonus).min(depth - 2)
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
        ATTENTION_KILLER, AttentionFeatures, DEEP_INDEX_FLOOR, DEEP_INDEX_SLOPE, DEEP_REDUCTION,
        DEEP_REDUCTION_BONUS, DEEP_REDUCTION_MIN_DEPTH, DEEP_REDUCTION_THRESHOLD, Features,
        LATE_MOVE_MIN_DEPTH, LATE_MOVE_PRUNING_THRESHOLD, LATE_MOVE_REDUCTION, LATE_MOVE_THRESHOLD,
        Node, PRUNE_INDEX_FLOOR, PRUNE_INDEX_SLOPE, REDUCTION, Search, Verdict, amount,
        attention_score, decide, features,
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

    /// What the model scores a plain late quiet at: no history, no killer
    /// slot and nothing from the table. The index rule's tests read it to
    /// say where their bounds stand against the model's thresholds, so a row
    /// the rule deepens is not one the model would have deepened anyway.
    fn model_score(
        eval: i64,
        generated: usize,
        depth: u8,
        searched: usize,
        alpha: Score,
        beta: Score,
    ) -> i64 {
        attention_score(&AttentionFeatures {
            depth,
            index: searched,
            hist_milli: 0,
            killer: false,
            tt: Table::Miss,
            eval_beta: eval - i64::from(beta),
            alpha_gap: i64::from(alpha) - eval,
            generated,
        })
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

    /// The index rule at the depth the deeper scout starts at: deepened at
    /// the floor and not one place earlier in the order. The bounds put the
    /// score far over both thresholds, so the model would deepen neither
    /// index and what fires is the rule.
    #[test]
    fn the_index_rule_deepens_a_late_quiet_at_its_floor() {
        let config = SearchConfig::default();
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
        let quiet = play_named(&s.board, "a4a5");
        const DEPTH: u8 = DEEP_REDUCTION_MIN_DEPTH;
        // a floor at or under the reduction's own threshold would deepen
        // every move the reduction reaches at this depth, and the index
        // under it is not reduced at all, so this test would be asking
        // about a move no gate sees
        const { assert!(DEEP_INDEX_FLOOR > LATE_MOVE_THRESHOLD) };
        // an eval standing far over beta, which the model reads as alive
        let (alpha, beta): (Score, Score) = (-5_000, -4_999);
        let eval = s.eval();
        let generated = s.moves.len();
        assert!(
            model_score(eval, generated, DEPTH, DEEP_INDEX_FLOOR, alpha, beta)
                > DEEP_REDUCTION_THRESHOLD
        );
        let flat = amount(&config, DEPTH, DEEP_INDEX_FLOOR, 0);
        let deeper = amount(&config, DEPTH, DEEP_INDEX_FLOOR, DEEP_REDUCTION_BONUS);
        assert_eq!(
            deeper,
            flat + DEEP_REDUCTION_BONUS,
            "the ply is not visible"
        );
        assert_eq!(
            s.verdict(&quiet, DEEP_INDEX_FLOOR, DEPTH, alpha, beta),
            Verdict::Scout(deeper)
        );
        assert_eq!(
            s.verdict(&quiet, DEEP_INDEX_FLOOR - 1, DEPTH, alpha, beta),
            Verdict::Scout(amount(&config, DEPTH, DEEP_INDEX_FLOOR - 1, 0))
        );
    }

    /// The floor rises by the slope for each ply of depth over the one the
    /// deeper scout starts at, read two plies up.
    #[test]
    fn the_index_rules_floor_rises_with_the_nodes_depth() {
        let config = SearchConfig::default();
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
        let quiet = play_named(&s.board, "a4a5");
        const DEPTH: u8 = DEEP_REDUCTION_MIN_DEPTH + 2;
        let floor = DEEP_INDEX_FLOOR + 2 * DEEP_INDEX_SLOPE;
        let (alpha, beta): (Score, Score) = (-5_000, -4_999);
        let eval = s.eval();
        let generated = s.moves.len();
        assert!(model_score(eval, generated, DEPTH, floor, alpha, beta) > DEEP_REDUCTION_THRESHOLD);
        let under = amount(&config, DEPTH, floor - 1, 0);
        let deeper = amount(&config, DEPTH, floor, DEEP_REDUCTION_BONUS);
        assert_ne!(under, deeper, "the two indexes read the same amount");
        assert_eq!(
            s.verdict(&quiet, floor, DEPTH, alpha, beta),
            Verdict::Scout(deeper)
        );
        assert_eq!(
            s.verdict(&quiet, floor - 1, DEPTH, alpha, beta),
            Verdict::Scout(under)
        );
    }

    #[test]
    fn a_checking_quiet_is_never_deepened_by_the_index_rule() {
        // the rook to the eighth checks along the rank and the push to a5
        // does not, both of them well past the floor, so the check test
        // alone tells them apart
        let config = SearchConfig::default();
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
        let quiet = play_named(&s.board, "a4a5");
        let checks = play_named(&s.board, "a4a8");
        assert!(!s.board.gives_check(&quiet));
        assert!(s.board.gives_check(&checks) && checks.capture.is_none());
        const DEPTH: u8 = DEEP_REDUCTION_MIN_DEPTH;
        let searched = DEEP_INDEX_FLOOR + 2;
        // and under the skip's own floor, so what the two moves are asked
        // about here is the deeper scout
        assert!(searched < PRUNE_INDEX_FLOOR);
        let (alpha, beta): (Score, Score) = (-5_000, -4_999);
        assert_eq!(
            s.verdict(&quiet, searched, DEPTH, alpha, beta),
            Verdict::Scout(amount(&config, DEPTH, searched, DEEP_REDUCTION_BONUS))
        );
        assert_eq!(
            s.verdict(&checks, searched, DEPTH, alpha, beta),
            Verdict::Scout(amount(&config, DEPTH, searched, 0))
        );
    }

    /// Off the switch the default reads the model's threshold as it did: a
    /// row solved onto it deepens and one over it does not. The threshold
    /// test above says the same of the reference derived configurations,
    /// which have the table off; this one is the default with one switch
    /// flipped, which is where the bench identity is read.
    #[test]
    fn the_index_rule_off_reads_the_models_threshold() {
        let config = SearchConfig {
            deep_index_rule: false,
            ..SearchConfig::default()
        };
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
        let quiet = play_named(&s.board, "a4a5");
        const SEARCHED: usize = 10;
        const DEPTH: u8 = 6;
        let eval = s.eval();
        let generated = s.moves.len();
        let score_at = |alpha, beta| model_score(eval, generated, DEPTH, SEARCHED, alpha, beta);
        let (alpha, beta) = solved(score_at, DEEP_REDUCTION_THRESHOLD);
        assert_eq!(score_at(alpha, beta), DEEP_REDUCTION_THRESHOLD);
        // a point of alpha up and two of beta down move the score one over
        let (over_alpha, over_beta) = (alpha + 1, beta - 2);
        assert_eq!(
            score_at(over_alpha, over_beta),
            DEEP_REDUCTION_THRESHOLD + 1
        );
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, alpha, beta),
            Verdict::Scout(amount(&config, DEPTH, SEARCHED, DEEP_REDUCTION_BONUS))
        );
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, over_alpha, over_beta),
            Verdict::Scout(amount(&config, DEPTH, SEARCHED, 0))
        );
    }

    /// One row the two policies read differently, so the switch is what the
    /// verdict turns on: an index under the floor, under bounds that put the
    /// score well under the model's threshold and well over the skip's. The
    /// model deepens that row and the rule does not.
    #[test]
    fn the_switch_settles_a_row_the_two_policies_disagree_about() {
        let config = SearchConfig::default();
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
        let quiet = play_named(&s.board, "a4a5");
        const DEPTH: u8 = DEEP_REDUCTION_MIN_DEPTH + 2;
        let searched = DEEP_INDEX_FLOOR + 2 * DEEP_INDEX_SLOPE - 1;
        assert!(searched >= LATE_MOVE_THRESHOLD, "the index is not reduced");
        let eval = s.eval();
        let generated = s.moves.len();
        let score_at = |alpha, beta| model_score(eval, generated, DEPTH, searched, alpha, beta);
        let (on_threshold, beta) = solved(score_at, DEEP_REDUCTION_THRESHOLD);
        // two hundred points of alpha past it puts the score fourteen hundred
        // under the model's threshold and still well over the skip's
        let alpha = on_threshold + 200;
        assert_eq!(score_at(alpha, beta), DEEP_REDUCTION_THRESHOLD - 1400);
        assert!(score_at(alpha, beta) > LATE_MOVE_PRUNING_THRESHOLD);
        let flat = amount(&config, DEPTH, searched, 0);
        let deeper = amount(&config, DEPTH, searched, DEEP_REDUCTION_BONUS);
        assert_ne!(flat, deeper, "the ply is not visible");
        assert_eq!(
            s.verdict(&quiet, searched, DEPTH, alpha, beta),
            Verdict::Scout(flat)
        );
        s.config = SearchConfig {
            deep_index_rule: false,
            ..config
        };
        assert_eq!(
            s.verdict(&quiet, searched, DEPTH, alpha, beta),
            Verdict::Scout(deeper)
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

    /// The skip's rule at the depth the gate starts at: the move at the
    /// floor is dropped and the one a place earlier is not. The bounds put
    /// the score far over the model's band, so the move the rule drops is
    /// one the model would have searched.
    #[test]
    fn the_index_rule_skips_a_late_quiet_at_its_floor() {
        let config = SearchConfig::default();
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
        let quiet = play_named(&s.board, "a4a5");
        const DEPTH: u8 = DEEP_REDUCTION_MIN_DEPTH;
        // the place before the floor is past the deeper scout's floor, so
        // what it reads is the extra ply and not the flat amount
        const { assert!(PRUNE_INDEX_FLOOR - 1 > DEEP_INDEX_FLOOR) };
        // an eval standing far over beta, which the model reads as alive
        let (alpha, beta): (Score, Score) = (-5_000, -4_999);
        let eval = s.eval();
        let generated = s.moves.len();
        assert!(
            model_score(eval, generated, DEPTH, PRUNE_INDEX_FLOOR, alpha, beta)
                > LATE_MOVE_PRUNING_THRESHOLD
        );
        assert_eq!(
            s.verdict(&quiet, PRUNE_INDEX_FLOOR, DEPTH, alpha, beta),
            Verdict::Skip
        );
        assert_eq!(
            s.verdict(&quiet, PRUNE_INDEX_FLOOR - 1, DEPTH, alpha, beta),
            Verdict::Scout(amount(
                &config,
                DEPTH,
                PRUNE_INDEX_FLOOR - 1,
                DEEP_REDUCTION_BONUS
            ))
        );
    }

    /// The skip's floor rises by its slope for each ply of depth over the
    /// one the gate starts at, read two plies up.
    #[test]
    fn the_index_rules_skip_floor_rises_with_the_nodes_depth() {
        let config = SearchConfig::default();
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
        let quiet = play_named(&s.board, "a4a5");
        const DEPTH: u8 = DEEP_REDUCTION_MIN_DEPTH + 2;
        let floor = PRUNE_INDEX_FLOOR + 2 * PRUNE_INDEX_SLOPE;
        let (alpha, beta): (Score, Score) = (-5_000, -4_999);
        let eval = s.eval();
        let generated = s.moves.len();
        assert!(
            model_score(eval, generated, DEPTH, floor, alpha, beta) > LATE_MOVE_PRUNING_THRESHOLD
        );
        assert_eq!(s.verdict(&quiet, floor, DEPTH, alpha, beta), Verdict::Skip);
        assert_eq!(
            s.verdict(&quiet, floor - 1, DEPTH, alpha, beta),
            Verdict::Scout(amount(&config, DEPTH, floor - 1, DEEP_REDUCTION_BONUS))
        );
    }

    #[test]
    fn a_checking_quiet_is_never_skipped_by_the_index_rule() {
        // the rook to the eighth checks along the rank and the push to a5
        // does not, both of them past the skip's floor, so the check test
        // alone tells them apart. The check is not handed the deeper scout
        // either, which carries the same exemption
        let config = SearchConfig::default();
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
        let quiet = play_named(&s.board, "a4a5");
        let checks = play_named(&s.board, "a4a8");
        assert!(!s.board.gives_check(&quiet));
        assert!(s.board.gives_check(&checks) && checks.capture.is_none());
        const DEPTH: u8 = DEEP_REDUCTION_MIN_DEPTH;
        let searched = PRUNE_INDEX_FLOOR + 4;
        let (alpha, beta): (Score, Score) = (-5_000, -4_999);
        assert_eq!(
            s.verdict(&quiet, searched, DEPTH, alpha, beta),
            Verdict::Skip
        );
        assert_eq!(
            s.verdict(&checks, searched, DEPTH, alpha, beta),
            Verdict::Scout(amount(&config, DEPTH, searched, 0))
        );
    }

    /// Off the switch the default reads the model's band as it did: a row
    /// solved onto the pruning threshold is dropped and one over it is not.
    /// The threshold test above says the same of the reference derived
    /// configurations; this one is the default with one switch flipped,
    /// which is where the bench identity is read.
    #[test]
    fn the_index_rule_off_reads_the_models_band() {
        let config = SearchConfig {
            index_rule_pruning: false,
            ..SearchConfig::default()
        };
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
        let quiet = play_named(&s.board, "a4a5");
        const SEARCHED: usize = 10;
        const DEPTH: u8 = 6;
        let eval = s.eval();
        let generated = s.moves.len();
        let score_at = |alpha, beta| model_score(eval, generated, DEPTH, SEARCHED, alpha, beta);
        let (alpha, beta) = solved(score_at, LATE_MOVE_PRUNING_THRESHOLD);
        assert_eq!(score_at(alpha, beta), LATE_MOVE_PRUNING_THRESHOLD);
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, alpha, beta),
            Verdict::Skip
        );
        // a point of alpha up and two of beta down move the score one over
        let (over_alpha, over_beta) = (alpha + 1, beta - 2);
        assert_eq!(
            score_at(over_alpha, over_beta),
            LATE_MOVE_PRUNING_THRESHOLD + 1
        );
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, over_alpha, over_beta),
            Verdict::Scout(amount(&config, DEPTH, SEARCHED, DEEP_REDUCTION_BONUS))
        );
    }

    /// What the default's gate costs a move it decides, as a test: with
    /// both rules on it derives no evaluation and no history denominator,
    /// which is the whole of what the model would have read. Off the skip's
    /// rule the same row derives both, because the score is wanted again.
    #[test]
    fn the_two_rules_together_derive_nothing_the_gate_reads() {
        let config = SearchConfig::default();
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
        let quiet = play_named(&s.board, "a4a5");
        const DEPTH: u8 = DEEP_REDUCTION_MIN_DEPTH;
        let searched = PRUNE_INDEX_FLOOR + 4;
        let (alpha, beta): (Score, Score) = (-5_000, -4_999);
        assert_eq!(
            s.verdict(&quiet, searched, DEPTH, alpha, beta),
            Verdict::Skip
        );
        assert!(
            s.eval.is_none() && s.history_max.is_none(),
            "the gate derived what no rule reads"
        );
        let scoring = SearchConfig {
            index_rule_pruning: false,
            ..config
        };
        s.config = scoring;
        assert_eq!(
            s.verdict(&quiet, searched, DEPTH, alpha, beta),
            Verdict::Scout(amount(&scoring, DEPTH, searched, DEEP_REDUCTION_BONUS))
        );
        assert!(
            s.eval.is_some() && s.history_max.is_some(),
            "the scoring gate left nothing to reuse"
        );
    }

    /// One row the two skip policies read differently, so the switch is
    /// what the verdict turns on: an index at the rule's floor under bounds
    /// that put the score well over the model's band. The rule drops the
    /// move and the model searches it.
    #[test]
    fn the_switch_settles_a_row_the_two_skip_policies_disagree_about() {
        let config = SearchConfig::default();
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
        let quiet = play_named(&s.board, "a4a5");
        const DEPTH: u8 = DEEP_REDUCTION_MIN_DEPTH + 1;
        let searched = PRUNE_INDEX_FLOOR + PRUNE_INDEX_SLOPE;
        let (alpha, beta): (Score, Score) = (-5_000, -4_999);
        let eval = s.eval();
        let generated = s.moves.len();
        assert!(
            model_score(eval, generated, DEPTH, searched, alpha, beta)
                > LATE_MOVE_PRUNING_THRESHOLD
        );
        assert_eq!(
            s.verdict(&quiet, searched, DEPTH, alpha, beta),
            Verdict::Skip
        );
        let scoring = SearchConfig {
            index_rule_pruning: false,
            ..config
        };
        s.config = scoring;
        assert_eq!(
            s.verdict(&quiet, searched, DEPTH, alpha, beta),
            Verdict::Scout(amount(&scoring, DEPTH, searched, DEEP_REDUCTION_BONUS))
        );
    }

    /// Off the switch the amount is the two constants the table replaced,
    /// at every depth and index the gate can reach. This is the identity
    /// the arm's bench claim rests on: a search with the table off is the
    /// search that was there before it existed.
    #[test]
    fn the_table_off_reads_the_two_constants_it_replaced() {
        let config = SearchConfig {
            reduction_table: false,
            ..SearchConfig::default()
        };
        for depth in LATE_MOVE_MIN_DEPTH..64 {
            for searched in [LATE_MOVE_THRESHOLD, 8, 20, 63, 200] {
                assert_eq!(
                    amount(&config, depth, searched, 0),
                    LATE_MOVE_REDUCTION,
                    "flat at depth {depth} index {searched}"
                );
                if depth >= DEEP_REDUCTION_MIN_DEPTH {
                    assert_eq!(
                        amount(&config, depth, searched, DEEP_REDUCTION_BONUS),
                        DEEP_REDUCTION,
                        "deeper at depth {depth} index {searched}"
                    );
                }
            }
        }
    }

    /// The table's shape rather than its numbers, on the piece square
    /// tables' precedent: a pin on every entry would fail on any change to
    /// the divisor and say nothing about whether the change was wrong.
    #[test]
    fn the_reduction_table_grows_with_depth_and_with_move_count() {
        assert_eq!(REDUCTION[0][0], 1, "the floor is a ply, never nothing");
        // the corner most reduced scouts sit in reads what the search did
        // before the table, which is what holds the shallow tree
        assert_eq!(
            REDUCTION[usize::from(LATE_MOVE_MIN_DEPTH)][LATE_MOVE_THRESHOLD],
            1
        );
        assert_eq!(REDUCTION[4][4], 1);
        for depth in 0..64 {
            for index in 0..64 {
                assert!(REDUCTION[depth][index] >= 1, "at {depth}, {index}");
                if depth > 0 {
                    assert!(
                        REDUCTION[depth][index] >= REDUCTION[depth - 1][index],
                        "depth {depth} at index {index} reduces less than {}",
                        depth - 1
                    );
                }
                if index > 0 {
                    assert!(
                        REDUCTION[depth][index] >= REDUCTION[depth][index - 1],
                        "index {index} at depth {depth} reduces less than {}",
                        index - 1
                    );
                }
            }
        }
        assert_eq!(REDUCTION[63][63], 7, "the deepest and latest corner");
    }

    /// The clamp is what the two minimum depths used to guarantee on their
    /// own: whatever the table says, the scout keeps a full width ply
    /// under it. A table entry of seven at a depth of five would search at
    /// a depth below zero without this, and the subtraction is on unsigned
    /// plies.
    #[test]
    fn the_scout_keeps_a_full_width_ply_at_every_depth_the_table_reaches() {
        let config = SearchConfig::default();
        for depth in LATE_MOVE_MIN_DEPTH..64 {
            for searched in [LATE_MOVE_THRESHOLD, 12, 40, 63, 500] {
                for bonus in [0, DEEP_REDUCTION_BONUS] {
                    if bonus > 0 && depth < DEEP_REDUCTION_MIN_DEPTH {
                        continue;
                    }
                    let reduction = amount(&config, depth, searched, bonus);
                    assert!(
                        reduction >= LATE_MOVE_REDUCTION,
                        "never under the flat ply at depth {depth} index {searched}"
                    );
                    assert!(
                        depth - 1 - reduction >= 1,
                        "depth {depth} index {searched} bonus {bonus} scouts at {}",
                        i32::from(depth) - 1 - i32::from(reduction)
                    );
                }
            }
        }
    }

    /// The gate's word stays worth a ply over the flat amount rather than
    /// becoming a depth of its own, which is the composition the arm was
    /// built on. At depth four and index four that is today's two.
    #[test]
    fn the_model_gate_is_worth_one_ply_over_the_flat_amount() {
        let config = SearchConfig::default();
        for depth in DEEP_REDUCTION_MIN_DEPTH..64 {
            for searched in [LATE_MOVE_THRESHOLD, 10, 30, 63] {
                let flat = amount(&config, depth, searched, 0);
                let deeper = amount(&config, depth, searched, DEEP_REDUCTION_BONUS);
                assert_eq!(
                    deeper,
                    (flat + DEEP_REDUCTION_BONUS).min(depth - 2),
                    "at depth {depth} index {searched}"
                );
            }
        }
        assert_eq!(
            amount(
                &config,
                DEEP_REDUCTION_MIN_DEPTH,
                LATE_MOVE_THRESHOLD,
                DEEP_REDUCTION_BONUS
            ),
            DEEP_REDUCTION,
            "the shallow corner is what the constants did"
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
