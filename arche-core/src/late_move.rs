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
//! Five rungs stand behind what a node does with a late quiet. The first
//! two reach the depths the rest do not, and both act at depth one to
//! three on a quiet move after the node's first. Quiet futility drops it
//! when the node's evaluation plus a margin a ply cannot reach alpha. The
//! late move count drops it when the node has already searched
//! `LATE_MOVE_COUNT` moves a ply, and reads no evaluation at all. They are
//! `Shallow` and not part of `decide`, because everything in them but one
//! comparison against alpha is settled by the node rather than by the
//! move: the loop builds it once and asks it per move. The third is the
//! late move reduction: a scout for a quiet move searched after the
//! fourth at a node deep enough to keep a full width ply under it, with
//! the exemptions `reduces` lists. The fourth is the attention model, a logistic
//! regression over the reduction ledger's feature columns quantized to
//! fixed point, whose score is read against two thresholds: under the
//! first the scout runs a ply deeper than it otherwise would, under the
//! second the move is dropped from the node. Each threshold is read only
//! where its own rule switch is off. Under the rules, which the default
//! carries both of, the deeper scout and the skip are decided by the
//! move's index against a floor that rises with the node's depth, and the
//! default derives no score at all. That ply is relative because the
//! model ranks how dead a move is and never names a depth. The fifth is `amount`, which reads how many
//! plies a scout gives up off a table by the node's depth and the move's
//! index.
//!
//! The two shallow rungs stop a ply under the model's floor, so a shallow
//! rule and the model never decide at one depth.
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
// How far under alpha a node's static evaluation may stand, per ply still
// to search, and a quiet move still be searched. A pawn a ply, which is
// `REVERSE_FUTILITY_MARGIN`'s figure and scale on purpose: both margins bet
// on how far the static evaluation can be from the full search's answer at
// the depth left, one from above beta and one from below alpha, and a pawn
// a ply is the only reading of that error this tree has. A hundred is
// where the rule starts rather than where a fit put it, and only games
// can say whether it belongs higher or lower.
pub(crate) const QUIET_FUTILITY_MARGIN: Score = 100;
// How many moves a node searches per ply of its depth before a quiet move
// after them is not searched at all. Four, so the cutoffs at the three
// depths the rule reaches are 4, 8 and 12. At depth one that is
// `LATE_MOVE_THRESHOLD`, the module's own definition of a late move: the
// count prunes at depth one exactly where the reduction would begin
// scouting if the reduction reached depth one. The top of the line is not
// pinned to anything: 12 at depth three sits well above the 8 that
// `DEEP_INDEX_FLOOR` and `DEEP_INDEX_SLOPE` put on the deeper scout at
// depth four, and above the 7 that rule's line would reach at depth three.
// That is deliberate and it is the conservative direction, because at
// depth four the model stands behind the cutoff and at depth three nothing
// does. An opening value, not a tuned one: what moves it is a match.
pub(crate) const LATE_MOVE_COUNT: usize = 4;
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
// The deepest node either shallow rule decides, written as a ply under the
// model's own floor rather than as a three. The shallow rules and the model
// never decide at one depth: from `DEEP_REDUCTION_MIN_DEPTH` the model has
// the skip and both shallow rules are silent.
pub(crate) const SHALLOW_MAX_DEPTH: u8 = DEEP_REDUCTION_MIN_DEPTH - 1;
const _: () = assert!(SHALLOW_MAX_DEPTH < DEEP_REDUCTION_MIN_DEPTH);
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

// The attention model the pruning is gated by, and the deep reduction
// where `deep_index_rule` is off: a logistic regression over the reduction
// ledger's feature columns, quantized to fixed point at a scale of 1024,
// so the gate is an integer dot product and a compare. Fitted by
// `fit_logistic` in `scripts/fit_attention.py` over the ledger `arche
// reductions 8 every 32` printed at commit 4e8ab28 on 75,024 positions from
// 43,123 opening pairs of our own strength games, on the rows the skip
// decides: depth four and up, the move not giving check, the skipped rows
// beside the scouted ones, since the replay labels both. A row deserves
// attention when its scout failed high or the replay called the denial
// harmful, and a skipped move that would have failed high is a move lost.
// Fitted on the pairs of one key parity (5,473,351 rows) and read on the
// other (5,448,365) once.
const ATTENTION_DEPTH: i64 = 76;
const ATTENTION_INDEX: i64 = -3;
const ATTENTION_BAND8_15: i64 = -284;
const ATTENTION_BAND16P: i64 = -362;
const ATTENTION_HIST_MILLI: i64 = 1;
const ATTENTION_KILLER: i64 = 265;
const ATTENTION_TT_MOVE: i64 = -137;
const ATTENTION_TT_SCORE_ONLY: i64 = 192;
const ATTENTION_EVAL_BETA: i64 = -5;
const ATTENTION_ALPHA_GAP: i64 = -13;
const ATTENTION_GENERATED: i64 = -6;
const ATTENTION_SEARCHED: i64 = -3;
const ATTENTION_INTERCEPT: i64 = -3540;
// The deep reduction's threshold where `deep_index_rule` is off, which no
// default reaches. Chosen on the weights these replaced, as their 90%
// coverage operating point, and not chosen again for these.
const DEEP_REDUCTION_THRESHOLD: i64 = -4637;
// The score at or under which a late quiet is not searched at all: the
// largest whose region covers no more of the fitting half's rows than the
// weights these replaced covered at their own threshold, -7954 (41.47%).
// On the other half it skips 41.32% of the rows at 0.0303% attention,
// against 41.29% at 0.0373% for the weights it replaced, a ratio of 1.230
// (95% 1.157 to 1.308 over resampled pairs).
pub(crate) const LATE_MOVE_PRUNING_THRESHOLD: i64 = -5932;
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
// rate. Held out, the model then skipped 41.54% at 0.070% attention and
// this rule 37.67% at 0.578%. On the ledger the model's current weights
// were fitted on, read on the half they were not fitted on, this rule
// skips 37.75% at 1.537% attention and those weights 41.32% at 0.030%.
// Depth and index cannot find the model's dead region.
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
    /// What the check test reads of the position, taken by the first move
    /// that asks and read back for the rest.
    pub(crate) check: &'a mut Option<crate::board::CheckInfo>,
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
    /// The gate prices the move as dead, by the index rule or the model's
    /// deadest band: the move is not searched at all.
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

/// The two shallow rules' node half, held across a node's move loop:
/// whether a quiet move at a node of depth one to three is not searched at
/// all, less the parts that read the move.
///
/// The rules are quiet futility and the late move count, each behind a
/// switch of its own. The margin's guess is that the node's evaluation plus
/// `QUIET_FUTILITY_MARGIN` a ply failing to reach alpha means the move
/// cannot reach it either. The count's guess is that a node which has
/// already searched `LATE_MOVE_COUNT` moves a ply has searched the moves
/// worth searching. They share a node, their exemptions and this struct,
/// and nothing else: the count reads no evaluation at all.
///
/// The exemptions are `reduces`'s, less the reduction's depth and count
/// floors and plus the material one. The first move searched is exempt, so
/// a node that pruned every move and answered a mate it is not in cannot
/// happen: the loop reads no legal move as mate or stalemate. A side with
/// no piece but pawns is exempt for the reason `shortcuts` refuses it, and
/// that gate also puts every node the rules reach inside the set the
/// reverse margin evaluated, so the evaluation the margin reads is one the
/// node had already.
///
/// Only `under` moves as the node searches. Alpha rises and never falls,
/// so the margin's test is false until it becomes true and then stays
/// true, which makes it a latch rather than a question per move. A rising
/// alpha can climb into the mate window, though, and that window is an
/// exemption, so the mate test is asked in front of the latch rather than
/// folded into it. The count reads nothing that moves at all.
pub(crate) struct Shallow {
    /// Whether the node's own facts admit either rule: a switch on, the
    /// depth band, the check, beta and root exemptions and the material
    /// gate.
    admits: bool,
    /// `QUIET_FUTILITY_MARGIN` at this node's depth, in the evaluation's
    /// units, or none where the margin's switch is off.
    margin: Option<i64>,
    /// `LATE_MOVE_COUNT` a ply at this node's depth: the searched count at
    /// or past which the count drops a quiet. None where the count's switch
    /// is off.
    count: Option<usize>,
    /// Whether the evaluation plus the margin has already failed to reach
    /// alpha here.
    under: bool,
}

/// What the node settles about the two rules before it searches a move. The
/// board read is here and not in the per move question, which is what the
/// rest of the loop is left asking, and it is read once for both.
pub(crate) fn shallow(
    config: &SearchConfig,
    board: &Board,
    depth: u8,
    in_check: bool,
    beta: Score,
    root_bounds: RootBounds,
) -> Shallow {
    let admits = (config.quiet_futility || config.late_move_count)
        && (1..=SHALLOW_MAX_DEPTH).contains(&depth)
        && !in_check
        && !is_mate(beta)
        && !root_bounds.beta
        && board.has_non_pawn_material();
    Shallow {
        admits,
        margin: (admits && config.quiet_futility)
            .then(|| i64::from(QUIET_FUTILITY_MARGIN) * i64::from(depth)),
        count: (admits && config.late_move_count).then(|| LATE_MOVE_COUNT * usize::from(depth)),
        under: false,
    }
}

impl Shallow {
    /// Whether either rule drops this move. `searched` is how many moves the
    /// node has searched already and `alpha` its bound as the move is
    /// reached; `eval` is the move loop's evaluation memo.
    ///
    /// A capture and a promotion are asked about before either rule, so a
    /// node deciding nothing but those never evaluates. The count is asked
    /// before the margin, because it reads integers the node holds and the
    /// margin reads the board: a move the count drops is dropped without an
    /// evaluation. The check probe is asked last and only of a move one of
    /// the rules would otherwise prune: a pruned check is never seen at
    /// all, where a scouted one is seen shallower, and the slider probes
    /// cost more than everything before them.
    #[inline]
    pub(crate) fn skips(
        &mut self,
        search: &Search,
        eval: &mut Option<i64>,
        check: &mut Option<crate::board::CheckInfo>,
        m: &Play,
        searched: usize,
        alpha: Score,
    ) -> bool {
        self.admits
            && searched >= 1
            && m.capture.is_none()
            && m.promote.is_none()
            && !is_mate(alpha)
            && (self.counted(searched) || self.under_alpha(search, eval, alpha))
            && !search
                .board
                .gives_check_with(check.get_or_insert_with(|| search.board.check_info()), m)
    }

    /// Whether either rule would drop every quiet move from here on that
    /// neither gives check nor promotes: the half of `skips` that does not
    /// read the move. Both parts are latches while alpha stays short of a
    /// mate, since `searched` and alpha only rise.
    #[inline]
    pub(crate) fn active(
        &mut self,
        search: &Search,
        eval: &mut Option<i64>,
        searched: usize,
        alpha: Score,
    ) -> bool {
        self.admits
            && searched >= 1
            && !is_mate(alpha)
            && (self.counted(searched) || self.under_alpha(search, eval, alpha))
    }

    /// Whether the node has searched the count's moves a ply already. The
    /// node's own integers and nothing else: no board read, no evaluation
    /// and no history.
    #[inline]
    fn counted(&self, searched: usize) -> bool {
        self.count.is_some_and(|count| searched >= count)
    }

    /// Whether the node stands under alpha by more than the margin, read
    /// off the latch once it is set.
    #[inline]
    fn under_alpha(&mut self, search: &Search, eval: &mut Option<i64>, alpha: Score) -> bool {
        let Some(margin) = self.margin else {
            return false;
        };
        if !self.under {
            self.under = eval_memo(search.board, eval) + margin <= i64::from(alpha);
        }
        self.under
    }
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

/// Whether the reduction can decide a move at this node: its half of
/// `reduces` that reads the node and the count rather than the move, on
/// the node's own facts so that a move loop can ask it before it builds a
/// `Node`. Where this is false every move is searched whole, and there is
/// nothing for `decide` to read.
///
/// The two shallow rules are not here. They reach depths this does not and
/// read none of the node facts, so the loop settles them in `Shallow` and
/// asks them before this.
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
        && node_admits(in_check, alpha, beta, root_bounds)
}

/// The exemptions the reduction and the two shallow rules share: a side in
/// check has evasions rather than late moves, a mate window on either
/// bound is the margin family's exemption, and a beta that is still the
/// root's own bound is the principal variation exemption `reduces` sets
/// out above. `Shallow` reads the three fixed ones at the node and alpha
/// per move.
#[inline]
fn node_admits(in_check: bool, alpha: Score, beta: Score, root_bounds: RootBounds) -> bool {
    !in_check && !is_mate(alpha) && !is_mate(beta) && !root_bounds.beta
}

/// Whether a move `reduces` already accepted is scouted a ply shallower
/// than the amount alone would give it, or not searched at all: the skip is
/// asked first, off whichever rule `prunes` reads, and the deeper scout
/// after it, off whichever rule `deepens` reads. The model's score is
/// computed only where one of them reads it, so with both index rules on
/// the gate evaluates nothing. The node must be deep enough for the
/// deeper scout to keep its full width ply, a floor the skip inherits,
/// and the move must not give check: the exemption arm measured checks
/// as the scout's blind spot, so neither the deeper scout nor the skip
/// is offered one. The check test runs last because the slider probes
/// cost more than everything before them.
fn gate(search: &Search, node: &mut Node, m: &Play, searched: usize) -> Verdict {
    if (!search.config.deep_reductions && !search.config.late_move_pruning)
        || node.depth < DEEP_REDUCTION_MIN_DEPTH
    {
        return Verdict::Scout(amount(search.config, node.depth, searched, 0));
    }
    let mut scored = None;
    let mut score = |node: &mut Node| {
        *scored.get_or_insert_with(|| {
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
        })
    };
    if search.config.late_move_pruning
        && prunes(search.config, node.depth, searched, || score(node))
    {
        let info = node.check.get_or_insert_with(|| search.board.check_info());
        return if search.board.gives_check_with(info, m) {
            Verdict::Scout(amount(search.config, node.depth, searched, 0))
        } else {
            Verdict::Skip
        };
    }
    if search.config.deep_reductions
        && deepens(search.config, node.depth, searched, || score(node))
        && !search.board.gives_check_with(
            node.check.get_or_insert_with(|| search.board.check_info()),
            m,
        )
    {
        return Verdict::Scout(amount(
            search.config,
            node.depth,
            searched,
            DEEP_REDUCTION_BONUS,
        ));
    }
    Verdict::Scout(amount(search.config, node.depth, searched, 0))
}

/// Whether the gate skips a move it has already accepted.
///
/// Under `index_rule_pruning` the decision reads the node's depth and the
/// move's index and nothing else, which is what the arm asks: whether the
/// model earns its place at the one gate it still decides. Off the rule
/// the model's threshold decides, as it did.
fn prunes(config: &SearchConfig, depth: u8, searched: usize, score: impl FnOnce() -> i64) -> bool {
    debug_assert!(
        depth >= DEEP_REDUCTION_MIN_DEPTH,
        "the skip is only asked about at the deeper scout's floor"
    );
    if config.index_rule_pruning {
        searched
            >= PRUNE_INDEX_FLOOR + PRUNE_INDEX_SLOPE * usize::from(depth - DEEP_REDUCTION_MIN_DEPTH)
    } else {
        score() <= LATE_MOVE_PRUNING_THRESHOLD
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
fn deepens(config: &SearchConfig, depth: u8, searched: usize, score: impl FnOnce() -> i64) -> bool {
    debug_assert!(
        depth >= DEEP_REDUCTION_MIN_DEPTH,
        "the deeper scout is only asked about at a depth it keeps a ply under"
    );
    if config.deep_index_rule {
        searched
            >= DEEP_INDEX_FLOOR + DEEP_INDEX_SLOPE * usize::from(depth - DEEP_REDUCTION_MIN_DEPTH)
    } else {
        score() <= DEEP_REDUCTION_THRESHOLD
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
    eval_memo(search.board, node.eval)
}

/// The board's static evaluation through the move loop's memo, which the
/// node facts borrow and the quiet futility rule is handed directly.
fn eval_memo(board: &Board, eval: &mut Option<i64>) -> i64 {
    *eval.get_or_insert_with(|| i64::from(crate::eval::eval(board)))
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
        ATTENTION_ALPHA_GAP, ATTENTION_EVAL_BETA, ATTENTION_KILLER, AttentionFeatures,
        DEEP_INDEX_FLOOR, DEEP_INDEX_SLOPE, DEEP_REDUCTION, DEEP_REDUCTION_BONUS,
        DEEP_REDUCTION_MIN_DEPTH, DEEP_REDUCTION_THRESHOLD, Features, LATE_MOVE_COUNT,
        LATE_MOVE_MIN_DEPTH, LATE_MOVE_PRUNING_THRESHOLD, LATE_MOVE_REDUCTION, LATE_MOVE_THRESHOLD,
        Node, PRUNE_INDEX_FLOOR, PRUNE_INDEX_SLOPE, QUIET_FUTILITY_MARGIN, REDUCTION,
        SHALLOW_MAX_DEPTH, Search, Shallow, Verdict, amount, attention_score, decide, features,
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

    /// The quiet futility rule alone, with nothing else on: whatever moves
    /// between this and the reference at depths one to three is the rule.
    fn futility() -> SearchConfig {
        SearchConfig {
            quiet_futility: true,
            ..SearchConfig::reference()
        }
    }

    /// The pruning with the quiet futility rule on top: whatever moves
    /// between this and `pruning` is the margin.
    fn quiet_futile() -> SearchConfig {
        SearchConfig {
            quiet_futility: true,
            ..pruning()
        }
    }

    /// The pruning with the late move count on top, which is this arm's
    /// candidate shape: whatever moves between this and `pruning` is the
    /// count. The margin is off, so a skip here read no evaluation.
    fn counting() -> SearchConfig {
        SearchConfig {
            late_move_count: true,
            ..pruning()
        }
    }

    /// Both shallow rules on top of the pruning, which is the shape the
    /// default carries.
    fn both_shallow() -> SearchConfig {
        SearchConfig {
            quiet_futility: true,
            late_move_count: true,
            ..pruning()
        }
    }

    /// The searched count that fires the count at every depth it reaches:
    /// its cutoff at the deepest of them, which is the largest of the
    /// three. The tests that ask both rules one question use it.
    const PAST_THE_COUNT: usize = LATE_MOVE_COUNT * SHALLOW_MAX_DEPTH as usize;

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
                check: &mut None,
            };
            decide(&search, &mut node, m, searched)
        }

        /// The shallow rules' node half, built as the move loop builds it.
        fn rule(&self, depth: u8, beta: Score) -> Shallow {
            super::shallow(
                &self.config,
                &self.board,
                depth,
                self.in_check,
                beta,
                self.root_bounds,
            )
        }

        /// Whether the rule drops one move. Each call is a node of its
        /// own, as `verdict` is: the held features are cleared first.
        fn skips(
            &mut self,
            m: &Play,
            searched: usize,
            depth: u8,
            alpha: Score,
            beta: Score,
        ) -> bool {
            self.eval = None;
            self.history_max = None;
            let mut shallow = self.rule(depth, beta);
            self.asks(&mut shallow, m, searched, alpha)
        }

        /// The same at a node whose evaluation the move loop already
        /// seeded, as it seeds it from what `shortcuts` read.
        fn skips_seeded(
            &mut self,
            seed: i64,
            m: &Play,
            searched: usize,
            depth: u8,
            alpha: Score,
            beta: Score,
        ) -> bool {
            self.eval = Some(seed);
            self.history_max = None;
            let mut shallow = self.rule(depth, beta);
            self.asks(&mut shallow, m, searched, alpha)
        }

        /// The question of a node half the caller is holding, so a test
        /// can ask one node twice and see what the latch carried.
        fn asks(&mut self, shallow: &mut Shallow, m: &Play, searched: usize, alpha: Score) -> bool {
            let search = Search {
                board: &self.board,
                ordering: &self.ordering,
                config: &self.config,
            };
            shallow.skips(&search, &mut self.eval, &mut None, m, searched, alpha)
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
                check: &mut None,
            };
            features(&search, &mut node, m, searched)
        }

        /// The position's static evaluation, which the bounds in the
        /// threshold tests are solved against.
        fn eval(&self) -> i64 {
            i64::from(crate::eval::eval(&self.board))
        }
    }

    /// Bounds that land a score exactly on a threshold. A point of alpha
    /// moves the score by the alpha gap's weight and a point of beta by
    /// the eval gap's, negated. The two are coprime, so as many betas as
    /// the alpha weight's size cover every residue of it, and one of them
    /// leaves alpha a whole number to close the rest.
    fn solved(score_at: impl Fn(Score, Score) -> i64, threshold: i64) -> (Score, Score) {
        let per_alpha = -ATTENTION_ALPHA_GAP;
        for beta in 100..100 + per_alpha as Score {
            let over = score_at(0, beta) - threshold;
            if over % per_alpha == 0 {
                return ((over / per_alpha) as Score, beta);
            }
        }
        panic!("the betas tried cover every residue of the alpha weight");
    }

    /// The smallest move of the bounds that raises the score by exactly
    /// one, as a change to alpha and a change to beta, so a threshold test
    /// can stand one over the line it solved onto.
    fn one_over(alpha: Score, beta: Score) -> (Score, Score) {
        let (per_alpha, per_beta) = (ATTENTION_ALPHA_GAP, -ATTENTION_EVAL_BETA);
        (0..=16i64)
            .flat_map(|reach| {
                (-reach..=reach).flat_map(move |a| [(a, reach - a.abs()), (a, a.abs() - reach)])
            })
            .find(|&(a, b)| per_alpha * a + per_beta * b == 1)
            .map(|(a, b)| (alpha + a as Score, beta + b as Score))
            .expect("the two weights are coprime and small")
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
        // a killer with the whole of the node's history, deep in a lost
        // window: dead despite both
        assert_eq!(row(4, 4, 1000, true, Table::Miss, -1883, 1882, 32), -17241);
        // one of the 171 fitting rows at depth four and up that sit on the
        // skip's threshold exactly
        assert_eq!(row(6, 5, 107, false, Table::Move, -325, 324, 33), -5932);
        // the deadest row of the fitting half
        assert_eq!(row(4, 4, 0, false, Table::Move, -3744, 3743, 25), -33489);
        // the most alive attention row: an eval standing over beta
        assert_eq!(row(4, 7, 0, false, Table::Move, 368, -1343, 24), 12057);
        // a row the gate skipped at depth five
        assert_eq!(row(5, 8, 0, false, Table::Move, -338, 337, 40), -6563);
        // the killer weight raises a row by exactly its coefficient, and
        // on the threshold row that is the whole distance out of the dead
        // region. It is a weight and not an exemption: the first row
        // above is a killer and dead all the same, so the model may skip
        // a killer whose bounds bury it
        let on_edge = row(6, 5, 107, false, Table::Move, -325, 324, 33);
        let as_killer = row(6, 5, 107, true, Table::Move, -325, 324, 33);
        assert_eq!(as_killer - on_edge, ATTENTION_KILLER);
        assert!(on_edge <= LATE_MOVE_PRUNING_THRESHOLD);
        assert!(as_killer > LATE_MOVE_PRUNING_THRESHOLD);
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
        let (over_alpha, over_beta) = one_over(alpha, beta);
        assert_eq!(
            score_at(over_alpha, over_beta),
            DEEP_REDUCTION_THRESHOLD + 1
        );
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, over_alpha, over_beta),
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
        let (over_alpha, over_beta) = one_over(alpha, beta);
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
        // past the deep floor and short of the skip's, so the rule deepens
        // the push rather than dropping it
        let searched = DEEP_INDEX_FLOOR + 2;
        const { assert!(DEEP_INDEX_FLOOR + 2 < PRUNE_INDEX_FLOOR) };
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

    /// The skip's rule drops a quiet at its floor and scouts it one under,
    /// two plies up so the slope is read as well, under bounds the model
    /// would never skip at, so the index is the only thing that can drop
    /// it. A check at the same index is scouted, and off the switch the
    /// model's threshold decides again.
    #[test]
    fn the_skip_rule_drops_at_its_floor_and_never_a_check() {
        let config = SearchConfig::default();
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
        let quiet = play_named(&s.board, "a4a5");
        let checks = play_named(&s.board, "a4a8");
        assert!(s.board.gives_check(&checks) && checks.capture.is_none());
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
        assert_eq!(
            s.verdict(&checks, floor, DEPTH, alpha, beta),
            Verdict::Scout(amount(&config, DEPTH, floor, 0))
        );
        s.config = SearchConfig {
            index_rule_pruning: false,
            ..config
        };
        assert_eq!(
            s.verdict(&quiet, floor, DEPTH, alpha, beta),
            Verdict::Scout(amount(&config, DEPTH, floor, DEEP_REDUCTION_BONUS))
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
        let (over_alpha, over_beta) = one_over(alpha, beta);
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
        // fifty points of alpha past it puts the score fifty alpha weights
        // under the model's threshold and still well over the skip's
        let alpha = on_threshold + 50;
        assert_eq!(
            score_at(alpha, beta),
            DEEP_REDUCTION_THRESHOLD + 50 * ATTENTION_ALPHA_GAP
        );
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
        let (over_alpha, over_beta) = one_over(alpha, beta);
        assert_eq!(
            score_at(over_alpha, over_beta),
            LATE_MOVE_PRUNING_THRESHOLD + 1
        );
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, over_alpha, over_beta),
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
        let (over_alpha, over_beta) = one_over(alpha, beta);
        assert_eq!(
            s.verdict(&quiet, SEARCHED, DEPTH, over_alpha, over_beta),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
    }

    /// The margin fires where the evaluation plus a pawn a ply lands
    /// exactly on alpha, and not one centipawn over it, at both ends of
    /// the rule's depth range. Two depths rather than one, so the test
    /// sees the margin scale with the depth rather than only fire.
    #[test]
    fn the_margin_fires_where_it_reaches_alpha_and_not_one_over_it() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, futility());
        let quiet = play_named(&s.board, "a4a5");
        let eval = s.eval();
        for depth in [1, SHALLOW_MAX_DEPTH] {
            // the node's second move, which is the first the rule may
            // reach at all
            let searched = 1;
            let reaches = (eval + i64::from(QUIET_FUTILITY_MARGIN) * i64::from(depth)) as Score;
            assert!(
                s.skips(&quiet, searched, depth, reaches, reaches + 1),
                "at depth {depth}"
            );
            assert!(
                !s.skips(&quiet, searched, depth, reaches - 1, reaches),
                "at depth {depth}"
            );
        }
    }

    /// The count fires where the node has searched `LATE_MOVE_COUNT` moves
    /// a ply and not one move under it, at every depth it reaches. Three
    /// depths rather than one, so the test sees the line scale with the
    /// depth rather than only fire. The bounds are an ordinary open window
    /// the margin would never fire on, since the count reads neither of
    /// them.
    #[test]
    fn the_count_fires_at_its_line_and_not_one_move_under_it() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, counting());
        let quiet = play_named(&s.board, "a4a5");
        for depth in 1..=SHALLOW_MAX_DEPTH {
            let line = LATE_MOVE_COUNT * usize::from(depth);
            assert!(s.skips(&quiet, line, depth, -100, 100), "at depth {depth}");
            assert!(
                !s.skips(&quiet, line - 1, depth, -100, 100),
                "at depth {depth}"
            );
        }
        // the line at depth one is the module's own lateness threshold, so
        // the node's first searched move is out of the count's reach at
        // every depth without the exemption the margin needs
        assert_eq!(LATE_MOVE_COUNT, LATE_MOVE_THRESHOLD);
        assert!(!s.skips(&quiet, 0, 1, -100, 100));
    }

    /// What the count costs a node, as a test, and the one thing that sets
    /// it apart from the margin: it reads the node's integers and nothing
    /// else. A node the count decides never evaluates and never walks the
    /// history denominator, and that holds with the margin beside it,
    /// because the count is asked first.
    #[test]
    fn the_count_decides_without_an_evaluation() {
        for (shape, config) in [("alone", counting()), ("beside the margin", both_shallow())] {
            let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
            let quiet = play_named(&s.board, "a4a5");
            // alpha far under the evaluation, so the margin cannot fire
            // here and what answers is the count
            let alpha = (s.eval() - 10_000) as Score;
            assert!(!crate::value::is_mate(alpha));
            assert!(
                s.skips(&quiet, PAST_THE_COUNT, 2, alpha, alpha + 1),
                "{shape}"
            );
            assert!(s.eval.is_none(), "{shape}: the count evaluated the node");
            assert!(
                s.history_max.is_none(),
                "{shape}: the count walked the history"
            );
        }
    }

    /// The node's first legal move is searched whatever its evaluation
    /// says. A node that pruned every move would answer the mate or the
    /// stalemate the move loop reads from no legal move found.
    #[test]
    fn the_first_move_searched_is_never_skipped() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, futility());
        let quiet = play_named(&s.board, "a4a5");
        let eval = s.eval();
        for depth in 1..=SHALLOW_MAX_DEPTH {
            // alpha far past the margin's reach, so nothing but the count
            // of searched moves stands between the move and the rule
            let alpha = (eval + 10_000) as Score;
            assert!(
                !s.skips(&quiet, 0, depth, alpha, alpha + 1),
                "at depth {depth}"
            );
            assert!(
                s.skips(&quiet, 1, depth, alpha, alpha + 1),
                "at depth {depth}"
            );
        }
    }

    /// The ceiling: at the model's floor both shallow rules are silent and
    /// the model's verdict is what it was. The row is solved onto the
    /// pruning threshold and one over it, and the alpha that solves it
    /// stands far enough over the evaluation that the margin would fire on
    /// it, while the searched count stands past what the count's line would
    /// read at this depth, so a ceiling that leaked a ply would skip the
    /// move the model let through.
    #[test]
    fn neither_shallow_rule_decides_at_the_models_floor() {
        const DEPTH: u8 = DEEP_REDUCTION_MIN_DEPTH;
        const SEARCHED: usize = 20;
        assert!(
            SEARCHED >= LATE_MOVE_COUNT * DEPTH as usize,
            "the count's line at this depth is under the index asked"
        );
        let mut with = Stand::new(fens::A_CAPTURE_AND_QUIETS, both_shallow());
        let mut without = Stand::new(fens::A_CAPTURE_AND_QUIETS, pruning());
        let quiet = play_named(&with.board, "a4a5");
        let eval = with.eval();
        let generated = with.moves.len();
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
        // the row's alpha stands over the evaluation by more than the
        // margin reaches a ply lower, so a ceiling that leaked would skip
        assert!(with.skips(&quiet, SEARCHED, SHALLOW_MAX_DEPTH, alpha, beta));
        assert!(!with.skips(&quiet, SEARCHED, DEPTH, alpha, beta));
        // and neither rule on its own reaches it either
        let mut margin = Stand::new(fens::A_CAPTURE_AND_QUIETS, quiet_futile());
        let mut count = Stand::new(fens::A_CAPTURE_AND_QUIETS, counting());
        assert!(!margin.skips(&quiet, SEARCHED, DEPTH, alpha, beta));
        assert!(!count.skips(&quiet, SEARCHED, DEPTH, alpha, beta));
        assert_eq!(
            without.verdict(&quiet, SEARCHED, DEPTH, alpha, beta),
            Verdict::Skip
        );
        assert_eq!(
            with.verdict(&quiet, SEARCHED, DEPTH, alpha, beta),
            Verdict::Skip
        );
        // one over the threshold the model deepens the scout instead, and
        // the rule must not turn that back into a skip
        let (over_alpha, over_beta) = one_over(alpha, beta);
        assert_eq!(
            score_at(over_alpha, over_beta),
            LATE_MOVE_PRUNING_THRESHOLD + 1
        );
        assert_eq!(
            without.verdict(&quiet, SEARCHED, DEPTH, over_alpha, over_beta),
            Verdict::Scout(DEEP_REDUCTION)
        );
        assert_eq!(
            with.verdict(&quiet, SEARCHED, DEPTH, over_alpha, over_beta),
            Verdict::Scout(DEEP_REDUCTION)
        );
    }

    /// A capture and a promotion are priced on material rather than on
    /// their place in the order, so neither rule is asked of one, and a
    /// quiet that gives check is never skipped: a pruned check is never
    /// seen at all, where a scouted one is seen shallower.
    ///
    /// Asked of each rule on its own. Twelve moves searched at depth one is
    /// past the count's cutoff of four there, and the alpha is past
    /// anything the margin reaches, so the same question fires both.
    #[test]
    fn a_capture_a_promotion_and_a_checking_quiet_are_never_skipped() {
        for (rule, config) in [("margin", quiet_futile()), ("count", counting())] {
            let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
            let quiet = play_named(&s.board, "a4a5");
            let capture = play_named(&s.board, "a4e4");
            let checks = play_named(&s.board, "a4a8");
            assert!(capture.capture.is_some());
            assert!(checks.capture.is_none() && s.board.gives_check(&checks));
            let alpha = (s.eval() + 10_000) as Score;
            assert!(
                s.skips(&quiet, PAST_THE_COUNT, 1, alpha, alpha + 1),
                "{rule}"
            );
            assert!(
                !s.skips(&capture, PAST_THE_COUNT, 1, alpha, alpha + 1),
                "{rule}"
            );
            assert!(s.eval.is_none(), "{rule} priced the capture");
            assert!(
                !s.skips(&checks, PAST_THE_COUNT, 1, alpha, alpha + 1),
                "{rule}"
            );

            // a promotion at a node with a piece behind it, so the material
            // gate is not what refuses the move
            let mut s = Stand::new("7k/1P6/8/8/R7/8/8/7K w - - 0 1", config);
            let promotes = play_named(&s.board, "b7b8q");
            let push = play_named(&s.board, "a4a5");
            assert!(promotes.capture.is_none() && promotes.promote.is_some());
            let alpha = (s.eval() + 10_000) as Score;
            assert!(
                s.skips(&push, PAST_THE_COUNT, 1, alpha, alpha + 1),
                "{rule}"
            );
            assert!(
                !s.skips(&promotes, PAST_THE_COUNT, 1, alpha, alpha + 1),
                "{rule}"
            );
        }
    }

    /// The four node exemptions, each against the same question with
    /// nothing else moved: a side in check, a mate window on either bound,
    /// a beta that is still the root's, and a side with no piece but pawns.
    /// Asked of each rule on its own, since the two share them.
    #[test]
    fn the_node_exemptions_stand_both_shallow_rules_down() {
        for (rule, config) in [("margin", quiet_futile()), ("count", counting())] {
            let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
            let quiet = play_named(&s.board, "a4a5");
            let alpha = (s.eval() + 10_000) as Score;
            assert!(
                s.skips(&quiet, PAST_THE_COUNT, 2, alpha, alpha + 1),
                "{rule}"
            );

            s.in_check = true;
            assert!(
                !s.skips(&quiet, PAST_THE_COUNT, 2, alpha, alpha + 1),
                "{rule}"
            );
            s.in_check = false;

            // a mate in hand as alpha, and one being proved against the
            // side to move as beta
            assert!(
                !s.skips(&quiet, PAST_THE_COUNT, 2, 29_500, 29_501),
                "{rule}"
            );
            assert!(
                !s.skips(&quiet, PAST_THE_COUNT, 2, -29_501, -29_500),
                "{rule}"
            );

            s.root_bounds = RootBounds::BOTH;
            assert!(
                !s.skips(&quiet, PAST_THE_COUNT, 2, alpha, alpha + 1),
                "{rule}"
            );
            s.root_bounds = RootBounds {
                alpha: true,
                beta: false,
            };
            assert!(
                s.skips(&quiet, PAST_THE_COUNT, 2, alpha, alpha + 1),
                "{rule}: alpha's bit is not the one read"
            );

            // a side with nothing but pawns is the side zugzwang happens
            // to, which is what stands the shortcuts down and these rules
            // with them
            let mut pawns = Stand::new("7k/8/8/8/P7/8/8/7K w - - 0 1", config);
            let push = play_named(&pawns.board, "a4a5");
            assert!(!pawns.board.has_non_pawn_material());
            let alpha = (pawns.eval() + 10_000) as Score;
            assert!(
                !pawns.skips(&push, PAST_THE_COUNT, 2, alpha, alpha + 1),
                "{rule}"
            );
        }
    }

    /// Off its switch a rule leaves the search the one that was there
    /// before: nothing is decided at depth one or two, and at depth three
    /// the reduction answers as it always did. The row is one both rules
    /// skip on, and the off side carries neither, which is the search each
    /// arm's bench identity is read against.
    #[test]
    fn a_switch_off_leaves_the_shallow_depths_where_they_were() {
        let mut off = Stand::new(fens::A_CAPTURE_AND_QUIETS, pruning());
        let quiet = play_named(&off.board, "a4a5");
        let alpha = (off.eval() + 10_000) as Score;
        const SEARCHED: usize = PAST_THE_COUNT;
        for (rule, config) in [("margin", quiet_futile()), ("count", counting())] {
            let mut on = Stand::new(fens::A_CAPTURE_AND_QUIETS, config);
            for depth in 1..=SHALLOW_MAX_DEPTH {
                assert!(
                    on.skips(&quiet, SEARCHED, depth, alpha, alpha + 1),
                    "{rule} at depth {depth}"
                );
                assert!(
                    !off.skips(&quiet, SEARCHED, depth, alpha, alpha + 1),
                    "{rule} at depth {depth}"
                );
            }
        }
        assert_eq!(
            off.verdict(&quiet, SEARCHED, 1, alpha, alpha + 1),
            Verdict::Scout(0)
        );
        assert_eq!(
            off.verdict(&quiet, SEARCHED, 2, alpha, alpha + 1),
            Verdict::Scout(0)
        );
        assert_eq!(
            off.verdict(&quiet, SEARCHED, LATE_MOVE_MIN_DEPTH, alpha, alpha + 1),
            Verdict::Scout(LATE_MOVE_REDUCTION),
            "the reduction at depth three is not a shallow rule's to move"
        );
    }

    /// What the rule costs a node, as a test. The margin reads the
    /// evaluation the move loop seeded from what the shortcuts had already
    /// computed, and computes nothing itself. It never walks the history
    /// denominator, which the rule has no term for.
    #[test]
    fn the_rule_reads_the_seeded_evaluation_and_no_history() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, quiet_futile());
        let quiet = play_named(&s.board, "a4a5");
        // depth two on a seeded evaluation. The position's own evaluation
        // stands well over these bounds, so a rule that computed one
        // rather than reading the seed would not fire
        let (alpha, beta): (Score, Score) = (0, 1);
        let seed = i64::from(alpha) - i64::from(QUIET_FUTILITY_MARGIN) * 2;
        assert!(s.eval() + i64::from(QUIET_FUTILITY_MARGIN) * 2 > i64::from(alpha));
        assert!(!s.skips(&quiet, 1, 2, alpha, beta));
        assert!(s.skips_seeded(seed, &quiet, 1, 2, alpha, beta));
        assert_eq!(s.eval, Some(seed), "the rule recomputed the evaluation");
        assert!(s.history_max.is_none(), "the rule walked the history");
    }

    /// The margin's test is settled once for the node rather than once for
    /// the move. Alpha rises through a move loop and never falls, so the
    /// test is false until it becomes true and then stays true, and the
    /// rest of the node's moves read the latch. Asked here of one node
    /// under the margin's reach, then past it, then under it again, which
    /// a search cannot do and the latch has to survive.
    #[test]
    fn the_margin_is_latched_once_alpha_has_reached_it() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, futility());
        let quiet = play_named(&s.board, "a4a5");
        let reaches = (s.eval() + i64::from(QUIET_FUTILITY_MARGIN)) as Score;
        let mut node = s.rule(1, reaches + 1);
        assert!(!s.asks(&mut node, &quiet, 1, reaches - 1));
        assert!(s.asks(&mut node, &quiet, 1, reaches));
        assert!(
            s.asks(&mut node, &quiet, 1, reaches - 1),
            "the node's moves after the first read the latch"
        );
    }

    /// A rising alpha can climb into the mate window, which is one of the
    /// node exemptions. The latch must not carry the rule past it, so the
    /// mate test is asked of every move rather than folded into the latch.
    #[test]
    fn a_mate_window_stands_the_latched_rule_down() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, futility());
        let quiet = play_named(&s.board, "a4a5");
        let alpha = (s.eval() + 10_000) as Score;
        assert!(!crate::value::is_mate(alpha));
        let mut node = s.rule(2, alpha + 1);
        assert!(s.asks(&mut node, &quiet, 1, alpha));
        // a child answered a mate and alpha took its score
        assert!(crate::value::is_mate(29_500));
        assert!(!s.asks(&mut node, &quiet, 1, 29_500));
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
