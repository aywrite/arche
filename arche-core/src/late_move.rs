// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What a full width node does with a quiet move its ordering put late:
//! search it whole, scout it some plies shallower first, or not search it
//! at all. `decide_admitted` answers with a `Verdict`; the scout itself
//! is `windowed`'s, in `engine.rs`.
//!
//! At depths one to three `Rules::skips` asks two pruning rules of a quiet
//! move after the node's first: quiet futility, when the evaluation plus a
//! margin a ply cannot reach alpha, and the late move count, when the node
//! has searched `LATE_MOVE_COUNT` moves a ply. Everything in them but the
//! alpha and the searched count is settled by the node before its moves,
//! so `Rules` holds it and reads those two off the engine's `Node` per
//! move. They stop a ply under the deep skip's floor, so a shallow rule
//! and the deep skip never decide at one depth.
//!
//! `Rules::reduces` and `decide_admitted` cover the rest. The late move
//! reduction scouts a quiet move searched after the fourth, with the
//! exemptions `Admission::reduces` lists. From `DEEP_REDUCTION_MIN_DEPTH`
//! a move is dropped when a logistic model of the node and the move scores
//! it under a threshold. The scout of a move the skip keeps
//! gets an extra ply when
//! the move's index reaches a floor rising with depth. The ply is added to
//! the amount rather than naming a depth, so the table's depth scaling
//! carries it. `amount` reads the plies off a table by depth and index.
//!
//! `features` derives the reduction ledger's columns once, for the ledger,
//! the forced decision instrument and the skip's model.

use crate::board::{Board, CheckInfo, Position};
use crate::census;
use crate::engine::{Node, SearchConfig};
use crate::misc::Score;
use crate::ordering::MoveOrdering;
use crate::play::Play;
use crate::value::is_mate;

// How many plies shallower a late quiet is scouted with the reduction
// table off. The table reads the same one ply at the corner most reduced
// scouts sit in.
pub(crate) const LATE_MOVE_REDUCTION: u8 = 1;
// Two more than the reduction, so the scout keeps a full width ply under
// it. Below this the scout is quiescence or a ply above it, and what it
// saves is noise.
pub(crate) const LATE_MOVE_MIN_DEPTH: u8 = LATE_MOVE_REDUCTION + 2;
// How many moves a node searches at full depth before a quiet move after
// them is scouted shallower. An opening value, not a tuned one.
pub(crate) const LATE_MOVE_THRESHOLD: usize = 4;
// How far under alpha a node's static evaluation may stand, per ply to
// search, and a quiet move still be searched. A pawn a ply, the figure and
// scale of `REVERSE_FUTILITY_MARGIN`: both bet on how far the static
// evaluation can be from the search's answer at the depth left, from
// opposite bounds. Where the rule starts rather than where a fit put it;
// only games can say which way it should move.
pub(crate) const QUIET_FUTILITY_MARGIN: Score = 100;
// How many moves a node searches per ply of depth before a later quiet is
// not searched at all: cutoffs of 4, 8 and 12 at depths one to three. At
// depth one that is `LATE_MOVE_THRESHOLD`, where the reduction would start
// scouting if it reached depth one. At depth three it sits above the 7 the
// deep index rule's line (`DEEP_INDEX_FLOOR`, `DEEP_INDEX_SLOPE`) would
// reach there. That is the conservative side on purpose: at depth four the
// deep skip stands behind the cutoff and at depth three nothing does. Untuned;
// a match is what would move it.
pub(crate) const LATE_MOVE_COUNT: usize = 4;
// How many plies shallower the deep reduction scouts a late quiet the gate
// deepens, with the table off: a ply over the flat amount.
pub(crate) const DEEP_REDUCTION: u8 = 2;
// Two more than the reduction, for the late move floor's reason.
pub(crate) const DEEP_REDUCTION_MIN_DEPTH: u8 = DEEP_REDUCTION + 2;
// The gate's extra ply over the flat amount, written as the difference so
// the table cannot drift from the pair of constants it replaced.
const DEEP_REDUCTION_BONUS: u8 = DEEP_REDUCTION - LATE_MOVE_REDUCTION;
// The deepest node either shallow rule decides: a ply under the deep
// skip's floor, so the two never decide at one depth.
pub(crate) const SHALLOW_MAX_DEPTH: u8 = DEEP_REDUCTION_MIN_DEPTH - 1;
const _: () = assert!(SHALLOW_MAX_DEPTH < DEEP_REDUCTION_MIN_DEPTH);
// ln(x) at a scale of 1024 for each index of the table below, as integers
// so the table is built at compile time and no two targets disagree. ln 0
// is taken as zero, which with ln 1 puts the first row and column on the
// floor of one.
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
// 2.6 at the squared scale (2.6 × 1024²), which sets how fast the
// reduction grows with the product of the two logs. The conventional
// figure; a match moves it, not the bench. Half is added before dividing
// so the result rounds to nearest.
const REDUCTION_DIV: u32 = 2_726_298;
const REDUCTION_HALF: u32 = REDUCTION_DIV / 2;

/// How many plies shallower a late quiet is scouted, by the node's depth
/// and the move's index, before the gate's ply and the clamp.
///
/// Built at compile time so the source holds the formula rather than four
/// thousand numbers. The floor of one holds the shallow corner at the flat
/// ply.
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

// The skip's model: a logistic model of whether a late quiet at depth four
// and up deserves attention (a scout that fails high, or a fail low the
// full depth replay calls harmful), in ten thousandths of a logit. A move
// scoring at or under the threshold is not searched. Fitted on the
// training half (by opening) of a ledger `reductions 8 every 64` over
// 10,000 positions from 5,000 games at 10+0.1, two a game, checking moves
// left out since the skip never reads them: 515,864 rows, with a unit
// ridge on the standardized weights. The threshold skips as many training
// rows as the eval minus beta margin it replaced (244,259). On the other
// half, at the margin's skip count, its skip region held 37 attention rows
// against the margin's 47 (a ratio of 0.79, 95% interval 0.63 to 1.00).
// History enters twice: as thousandths of the node's largest when it is
// positive, and as log2(1 + |history|) when it is negative, where 96% of
// late quiets stand.
const SKIP_DEPTH: i64 = -769;
const SKIP_EVAL_BETA: i64 = 83;
const SKIP_WIDTH: i64 = 81;
const SKIP_INDEX: i64 = -86;
const SKIP_BAND8_15: i64 = -1829;
const SKIP_BAND16P: i64 = -4105;
const SKIP_GENERATED: i64 = 88;
const SKIP_HIST_MILLI: i64 = 17;
const SKIP_HIST_NEGATIVE: f64 = 524.0;
const SKIP_KILLER: i64 = -2087;
const SKIP_TT_MOVE: i64 = -1641;
const SKIP_TT_SCORE_ONLY: i64 = -3009;
const SKIP_INTERCEPT: i64 = -36613;
pub(crate) const SKIP_THRESHOLD: i64 = -56313;
// The index the deep reduction's rule wants a move to have reached, and
// how much further along the order per ply of depth over the floor the
// deeper scout starts at. Chosen offline on the training half of a ledger
// `arche reductions 8 every 4 cap 2000000 epd <root>` over 1,428 roots
// from 1,340 of our own games at 10+0.1 (corpus sha256
// c5fd032b7e0992d22dc38e29be1528080533dcd9387d0d0d8d36808e73d4faa0):
// 929,539 depth four and up scouted non-checking rows, split by source
// game into 457,908 and 471,631. Of 36 candidates (floor 4 to 14, slope 0
// to 3) this pair covers within three points of the attention model the
// skip read then, at the lowest attention rate, 77.18% at 0.664% against
// 79.07% at 0.448%. Held out and
// read once after the choice, the model deepens 76.84% at 0.502% attention
// (0.312% harmful) and this rule 77.17% at 0.728% (0.375% harmful).
pub(crate) const DEEP_INDEX_FLOOR: usize = 8;
pub(crate) const DEEP_INDEX_SLOPE: usize = 1;

/// What the decision reads of the search: references rather than the
/// engine, so a test can build one with no search behind it.
pub(crate) struct Search<'a> {
    pub(crate) board: &'a Board,
    pub(crate) ordering: &'a MoveOrdering,
    pub(crate) config: &'a SearchConfig,
}

/// What the late move rules hold across one full width node's move loop:
/// the memos its moves fill and the node's half of each rule. Built once
/// the node's table move has been searched. The node's facts, its alpha as
/// it stands and its searched count are the engine's `Node`, handed to
/// every call, so nothing here has to be kept in step with a rise of
/// alpha. The list is not held either, since the loop sorts it under the
/// decision; the calls that read it are handed it.
pub(crate) struct Rules {
    /// The node's static evaluation: what the table's entry held or the
    /// shortcuts read, or none until the first move that needs it. The
    /// recorders take their own, so a node the gate never reaches never
    /// computes one.
    pub(crate) eval: Option<Score>,
    /// The node's history denominator, for the recorders. Held rather than
    /// walked again so that every row a node records is read against one
    /// denominator: a child search between two of the node's moves can
    /// teach the history. The first move a recorder samples fills it.
    pub(crate) history_max: Option<i32>,
    /// What the check test reads of the position, computed once.
    pub(crate) check: Option<CheckInfo>,
    shallow: Shallow,
    admission: Admission,
}

impl Rules {
    /// The rules once the node's table move has been searched, with `eval`
    /// as the table and the shortcuts left it. The two rule halves are read
    /// off the node's facts here.
    pub(crate) fn new(search: &Search, node: &Node, eval: Option<Score>) -> Self {
        Self {
            eval,
            history_max: None,
            check: None,
            shallow: shallow(search.config, search.board, node),
            admission: admission(search.config, node),
        }
    }

    /// Whether either shallow rule drops this move, at the node's alpha and
    /// searched count as the move is reached.
    #[inline]
    pub(crate) fn skips(&mut self, search: &Search, node: &Node, m: &Play) -> bool {
        self.shallow
            .skips(search, &mut self.eval, &mut self.check, m, node)
    }

    /// Whether either shallow rule would drop every later quiet that
    /// neither gives check nor promotes: the half of `skips` that does not
    /// read the move. It stays true once true while alpha is short of a
    /// mate, since the searched count and alpha only rise; the lazy quiet
    /// ordering relies on that.
    #[inline]
    pub(crate) fn shallow_active(&mut self, search: &Search, node: &Node) -> bool {
        self.shallow.active(search, &mut self.eval, node)
    }

    /// Whether the late move reduction may scout this move, at the node's
    /// alpha and searched count as the move is reached.
    #[inline]
    pub(crate) fn reduces(&self, node: &Node, m: &Play) -> bool {
        self.admission.reduces(node, m)
    }
}

/// What the decision settled for one late quiet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Scouted this many plies shallower, and searched at the node's
    /// depth only when the scout comes back above alpha. Zero is no
    /// scout: the reduction did not apply.
    Scout(u8),
    /// Not searched at all: the skip's model scores the move at or under
    /// its threshold.
    Skip,
}

/// What the node knew about a late quiet at the decision, in the units
/// the reduction ledger prints. The ledger and the forced decision
/// instrument record them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Features {
    /// The move's place among the searched moves: the table's move, when
    /// it was searched, is 0.
    pub(crate) index: usize,
    pub(crate) generated: usize,
    /// The history table's score for the move at the decision, signed.
    pub(crate) history: i32,
    /// The largest history score among the node's generated quiets,
    /// clamped at zero: the denominator `history` is read against.
    pub(crate) history_max: i32,
    pub(crate) killer: bool,
    pub(crate) tt: census::Table,
}

/// The verdict for one move at a full width node, asked as the loop asks
/// it once the shallow rules have passed the move: `Rules::reduces`, then
/// the gate. The node's searched count says how many of its moves it has
/// searched already, the table's move among them. The reduction's
/// exemptions are asked first, so a node that reduces nothing pays for no
/// evaluation. `moves` is the node's list, which the skip's history
/// denominator walks.
#[cfg(test)]
pub(crate) fn decide(
    search: &Search,
    node: &Node,
    rules: &mut Rules,
    moves: &[Play],
    m: &Play,
) -> Verdict {
    if !rules.reduces(node, m) {
        return Verdict::Scout(0);
    }
    decide_admitted(search, node, rules, moves, m)
}

/// The node's half of the two shallow rules, held across its move loop.
///
/// Quiet futility guesses that a move cannot reach alpha when the node's
/// evaluation plus `QUIET_FUTILITY_MARGIN` a ply does not. The late move
/// count guesses that a node which has searched `LATE_MOVE_COUNT` moves a
/// ply has searched those worth searching, and reads no evaluation. Each
/// has a switch of its own.
///
/// The exemptions are `Admission::reduces`'s without its depth and count
/// floors, plus the material gate and a move that gives check. The first
/// move searched is exempt, because the loop reads a node with no legal
/// move searched as mate or stalemate. A side with no piece but pawns is
/// exempt for the reason `shortcuts` refuses it, which also means the
/// margin reads an evaluation the node already has.
///
/// Alpha only rises, so the margin's test is a latch once it holds
/// (`under`). A rising alpha can reach the mate window, which is an
/// exemption, and a margin short at one alpha can hold at a higher one, so
/// both are read against the alpha handed in: the exemption at every move,
/// and the margin again whenever alpha differs from the one it was last
/// found short at.
struct Shallow {
    /// The searched count from which either rule may drop a move while
    /// alpha is short of a mate: one where the node's own facts admit them,
    /// else never.
    from: usize,
    /// `QUIET_FUTILITY_MARGIN` at this node's depth.
    margin: i32,
    /// The searched count at or past which the count drops a quiet, or
    /// never with its switch off.
    count: usize,
    /// The margin's answer as far as it has been asked.
    under: Under,
}

/// The quiet futility margin's answer at a node, as far as it has been
/// asked.
#[derive(Clone, Copy)]
enum Under {
    /// Never asked: the margin's switch is off, or the node is exempt.
    Off,
    /// Not asked yet.
    Ask,
    /// Short at this alpha, and asked again at any other.
    Short(Score),
    /// Held, which stands while alpha rises.
    Held,
}

/// The node's half of the two rules, read once before the loop off the
/// node's facts.
fn shallow(config: &SearchConfig, board: &Position, node: &Node) -> Shallow {
    let admits = (config.quiet_futility || config.late_move_count)
        && (1..=SHALLOW_MAX_DEPTH).contains(&node.depth)
        && !node.in_check
        && !is_mate(node.answer.beta)
        && !node.answer.root_bounds.beta_is_roots()
        && board.has_non_pawn_material();
    Shallow {
        from: if admits { 1 } else { usize::MAX },
        margin: i32::from(QUIET_FUTILITY_MARGIN) * i32::from(node.depth),
        count: if admits && config.late_move_count {
            LATE_MOVE_COUNT * usize::from(node.depth)
        } else {
            usize::MAX
        },
        under: if admits && config.quiet_futility {
            Under::Ask
        } else {
            Under::Off
        },
    }
}

impl Shallow {
    /// Whether either rule may drop a move at this searched count and
    /// alpha. The mate test is asked after the count, so a node the rules
    /// do not admit never asks it.
    #[inline]
    fn reached(&self, searched: usize, alpha: Score) -> bool {
        searched >= self.from && !is_mate(alpha)
    }

    /// Whether either rule drops this move, at the node's alpha and
    /// searched count as the move is reached.
    ///
    /// The order of the tests is the cost order: the count before the
    /// margin, so a move the count drops needs no evaluation, and
    /// `survives_shallow` last, since its slider probes cost more than
    /// everything before them.
    #[inline]
    fn skips(
        &mut self,
        search: &Search,
        eval: &mut Option<Score>,
        check: &mut Option<CheckInfo>,
        m: &Play,
        node: &Node,
    ) -> bool {
        let searched = node.answer.searched;
        self.reached(searched, node.answer.alpha)
            && m.capture.is_none()
            && (searched >= self.count || self.under_alpha(search, eval, node.answer.alpha))
            && !survives_shallow(
                search.board,
                check.get_or_insert_with(|| search.board.check_info()),
                m,
            )
    }

    /// The half of `skips` that does not read the move.
    #[inline]
    fn active(&mut self, search: &Search, eval: &mut Option<Score>, node: &Node) -> bool {
        let searched = node.answer.searched;
        self.reached(searched, node.answer.alpha)
            && (searched >= self.count || self.under_alpha(search, eval, node.answer.alpha))
    }

    #[inline]
    fn under_alpha(&mut self, search: &Search, eval: &mut Option<Score>, alpha: Score) -> bool {
        match self.under {
            Under::Held => true,
            Under::Off => false,
            Under::Short(at) if at == alpha => false,
            Under::Ask | Under::Short(_) => {
                let held =
                    i32::from(eval_memo(search.board, eval)) + self.margin <= i32::from(alpha);
                self.under = if held {
                    Under::Held
                } else {
                    Under::Short(alpha)
                };
                held
            }
        }
    }
}

/// Whether a quiet move survives the shallow rules however late it comes
/// and wherever alpha stands: it promotes or gives check. A promotion is
/// priced on material rather than on its place in the order, and a pruned
/// check is never seen at all, where a scouted one is seen shallower.
/// `Shallow::skips` asks it of each move, and once the rules are on for the
/// rest of the node the lazy quiet ordering keeps the moves it accepts.
#[inline]
pub(crate) fn survives_shallow(board: &Position, info: &CheckInfo, m: &Play) -> bool {
    m.promote.is_some() || board.gives_check_with(info, m)
}

/// The late move reduction's node half: what the node's facts settle, held
/// across the loop, so the loop asks the searched count, the mate test and
/// the move per move.
struct Admission {
    /// The searched count from which the node admits a reduction while
    /// alpha is short of a mate, or never.
    from: usize,
}

fn admission(config: &SearchConfig, node: &Node) -> Admission {
    let admits = config.late_move_reductions
        && node.depth >= LATE_MOVE_MIN_DEPTH
        && !node.in_check
        && !is_mate(node.answer.beta)
        && !node.answer.root_bounds.beta_is_roots();
    Admission {
        from: if admits {
            LATE_MOVE_THRESHOLD
        } else {
            usize::MAX
        },
    }
}

impl Admission {
    /// Whether the node admits a reduction of its next move, whatever the
    /// move: the searched count and alpha against the half held.
    #[inline]
    fn admits(&self, node: &Node) -> bool {
        node.answer.searched >= self.from && !is_mate(node.answer.alpha)
    }

    /// Whether a move at a full width node is scouted shallower before it
    /// is searched at the node's depth: the late move reduction.
    ///
    /// The reduction guesses that a move the ordering put late is worth
    /// less than alpha, and is refused wherever that guess has nothing to
    /// stand on. The first moves are searched whole. A capture or a
    /// promotion was priced on material, not on its place in the order;
    /// that takes in the losing captures, which sort behind the quiets, and
    /// reducing them is a follow-up. A side in check has evasions, not late
    /// moves. A scout a ply short of a mate at either bound can only say no.
    ///
    /// A beta that is still the root's own bound stands the reduction down
    /// as a policy rather than a proof: that node's answer is what the root
    /// reports, so a late move trusted a ply short there costs the answer
    /// and not a bound. An arm that wants to reduce there lifts the flag
    /// and plays a match. Whether alpha is the root's is not read, because
    /// at such a node every move failing low is the reduction's own guess.
    ///
    /// A quiet move that gives check is reduced like any other: exempting
    /// checks was measured and lost (docs/ROADMAP.md).
    #[inline]
    fn reduces(&self, node: &Node, m: &Play) -> bool {
        self.admits(node) && m.capture.is_none() && m.promote.is_none()
    }
}

/// The gate: whether a move `Rules::reduces` accepted is skipped, or
/// scouted a ply shallower than the amount alone would give it. The skip is
/// asked first, off the model's score. Both need the depth
/// where the deeper
/// scout keeps its full width ply, and neither is offered a move that gives
/// check: the exemption arm measured checks as the scout's blind spot. The
/// check test runs last because the slider probes cost more than everything
/// before it.
pub(crate) fn decide_admitted(
    search: &Search,
    node: &Node,
    rules: &mut Rules,
    moves: &[Play],
    m: &Play,
) -> Verdict {
    let searched = node.answer.searched;
    let plain = || Verdict::Scout(amount(search.config, node.depth, searched, 0));
    if (!search.config.deep_reductions && !search.config.late_move_pruning)
        || node.depth < DEEP_REDUCTION_MIN_DEPTH
    {
        return plain();
    }
    if search.config.late_move_pruning
        && skip_score(search, node, rules, moves, m) <= SKIP_THRESHOLD
    {
        let info = rules.check.get_or_insert_with(|| search.board.check_info());
        return if search.board.gives_check_with(info, m) {
            plain()
        } else {
            Verdict::Skip
        };
    }
    if search.config.deep_reductions
        && deepens(node.depth, searched)
        && !search.board.gives_check_with(
            rules.check.get_or_insert_with(|| search.board.check_info()),
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
    plain()
}

/// The skip's model score for a late quiet at the gate: higher is more
/// likely to deserve attention.
fn skip_score(search: &Search, node: &Node, rules: &mut Rules, moves: &[Play], m: &Play) -> i64 {
    let eval = i64::from(eval_memo(search.board, &mut rules.eval));
    let f = features(search, node, rules, moves, m);
    let beta = i64::from(node.answer.beta);
    model_score(
        node.depth,
        &f,
        eval - beta,
        beta - i64::from(node.answer.alpha) - 1,
    )
}

/// The model's score from the ledger's columns: the node's depth, its
/// evaluation less beta and its window's width, and the move's features.
pub(crate) fn model_score(depth: u8, f: &Features, eval_beta: i64, width: i64) -> i64 {
    let index = f.index as i64;
    let hist_milli = if f.history_max > 0 {
        i64::from(f.history.max(0)) * 1000 / i64::from(f.history_max)
    } else {
        0
    };
    let hist_negative = if f.history < 0 {
        (SKIP_HIST_NEGATIVE * (1.0 + f64::from(f.history.unsigned_abs())).log2()).round() as i64
    } else {
        0
    };
    let (tt_move, tt_score_only) = match f.tt {
        census::Table::Miss => (0, 0),
        census::Table::Move => (1, 0),
        census::Table::ScoreOnly => (0, 1),
    };
    SKIP_DEPTH * i64::from(depth)
        + SKIP_EVAL_BETA * eval_beta
        + SKIP_WIDTH * width
        + SKIP_INDEX * index
        + SKIP_BAND8_15 * i64::from((8..=15).contains(&f.index))
        + SKIP_BAND16P * i64::from(f.index >= 16)
        + SKIP_GENERATED * f.generated as i64
        + SKIP_HIST_MILLI * hist_milli
        + hist_negative
        + SKIP_KILLER * i64::from(f.killer)
        + SKIP_TT_MOVE * tt_move
        + SKIP_TT_SCORE_ONLY * tt_score_only
        + SKIP_INTERCEPT
}

/// Whether the skip's model leaves a move unsearched, read from a recorded
/// row's columns: the window's width is what the evaluation's two gaps
/// leave of it.
#[cfg(test)]
pub(crate) fn row_skips(depth: u8, f: &Features, eval_beta: i64, alpha_gap: i64) -> bool {
    model_score(depth, f, eval_beta, -eval_beta - alpha_gap - 1) <= SKIP_THRESHOLD
}

/// Whether the gate gives a move the deeper scout's extra ply: by depth
/// and index alone.
fn deepens(depth: u8, searched: usize) -> bool {
    debug_assert!(
        depth >= DEEP_REDUCTION_MIN_DEPTH,
        "the deeper scout is only asked about at a depth it keeps a ply under"
    );
    searched >= DEEP_INDEX_FLOOR + DEEP_INDEX_SLOPE * usize::from(depth - DEEP_REDUCTION_MIN_DEPTH)
}

/// How many plies shallower the scout runs. `bonus` is the gate's ply,
/// added to the amount for the reason the module doc gives.
///
/// The clamp keeps `depth - 1 - reduction` at one or more, so the scout
/// keeps a full width ply. It is applied after the bonus because the
/// depth the scout actually runs at is what has to stay above zero, and
/// it is a clamp rather than higher minimum depths so that which moves are
/// eligible does not move with the amount.
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

/// What the node knows about one move for the ledger. `moves` is the
/// node's list, which the denominator walks once. The index is the node's
/// searched count as the move is reached.
pub(crate) fn features(
    search: &Search,
    node: &Node,
    rules: &mut Rules,
    moves: &[Play],
    m: &Play,
) -> Features {
    let history_max = denominator(search, rules, moves);
    let killers = node
        .ply
        .map_or([None, None], |ply| search.ordering.killers_at(ply));
    Features {
        index: node.answer.searched,
        generated: moves.len(),
        history: search.ordering.history_score(search.board.active_color, m),
        // a fraction of a marked down largest would be on no scale
        history_max: history_max.max(0),
        killer: killers.contains(&Some(*m)),
        tt: node.tt,
    }
}

/// The node's static evaluation, computed by the first call and read back
/// by the rest.
fn eval_memo(board: &Position, eval: &mut Option<Score>) -> Score {
    *eval.get_or_insert_with(|| crate::eval::eval(board))
}

/// The largest history score among the node's generated quiets, signed:
/// `Features` does the clamping.
fn denominator(search: &Search, rules: &mut Rules, moves: &[Play]) -> i32 {
    *rules.history_max.get_or_insert_with(|| {
        let color = search.board.active_color;
        moves
            .iter()
            .filter_map(|m| search.ordering.quiet_history(color, m))
            .max()
            .unwrap_or(0)
    })
}

#[cfg(test)]
mod tests {
    use super::{
        DEEP_INDEX_FLOOR, DEEP_INDEX_SLOPE, DEEP_REDUCTION, DEEP_REDUCTION_BONUS,
        DEEP_REDUCTION_MIN_DEPTH, Features, LATE_MOVE_COUNT, LATE_MOVE_MIN_DEPTH,
        LATE_MOVE_REDUCTION, LATE_MOVE_THRESHOLD, QUIET_FUTILITY_MARGIN, REDUCTION, Rules,
        SHALLOW_MAX_DEPTH, SKIP_EVAL_BETA, SKIP_THRESHOLD, Search, Verdict, admission, amount,
        decide, features, model_score, survives_shallow,
    };
    use crate::board::{Board, MoveList, fens, play_named};
    use crate::census::Table;
    use crate::engine::{FailSoft, MAX_PLY, Node, RootBounds, SearchConfig};
    use crate::misc::{Piece, PromotePiece, Score};
    use crate::ordering::MoveOrdering;
    use crate::play::Play;
    use crate::value::Taint;
    use pretty_assertions::assert_eq;

    /// The reference with the late move reductions alone on.
    fn reducing() -> SearchConfig {
        SearchConfig {
            late_move_reductions: true,
            ..SearchConfig::reference()
        }
    }

    /// `reducing` with the deep reduction on top.
    fn deep_reducing() -> SearchConfig {
        SearchConfig {
            late_move_reductions: true,
            deep_reductions: true,
            ..SearchConfig::reference()
        }
    }

    /// `reducing` with the model's skip on top and the deep reduction off,
    /// so a move the skip passes is scouted at the flat amount.
    fn skipping() -> SearchConfig {
        SearchConfig {
            late_move_reductions: true,
            late_move_pruning: true,
            ..SearchConfig::reference()
        }
    }

    /// `deep_reducing` with the model's skip on top.
    fn pruning() -> SearchConfig {
        SearchConfig {
            late_move_reductions: true,
            deep_reductions: true,
            late_move_pruning: true,
            ..SearchConfig::reference()
        }
    }

    /// The reference with quiet futility alone on.
    fn futility() -> SearchConfig {
        SearchConfig {
            quiet_futility: true,
            ..SearchConfig::reference()
        }
    }

    /// `pruning` with quiet futility on top.
    fn quiet_futile() -> SearchConfig {
        SearchConfig {
            quiet_futility: true,
            ..pruning()
        }
    }

    /// `pruning` with the late move count on top and the futility margin off, so a
    /// shallow skip here read no evaluation.
    fn counting() -> SearchConfig {
        SearchConfig {
            late_move_count: true,
            ..pruning()
        }
    }

    /// Both shallow rules on top of `pruning`, as the default has them.
    fn both_shallow() -> SearchConfig {
        SearchConfig {
            quiet_futility: true,
            late_move_count: true,
            ..pruning()
        }
    }

    /// A searched count past the count's cutoff at every depth it reaches.
    const PAST_THE_COUNT: usize = LATE_MOVE_COUNT * SHALLOW_MAX_DEPTH as usize;

    /// Everything a decision reads, owned by the test, with no engine and
    /// no search behind it.
    struct Stand {
        board: Board,
        ordering: MoveOrdering,
        config: SearchConfig,
        moves: MoveList,
        /// The node's facts besides its depth and bounds.
        in_check: bool,
        root_bounds: RootBounds,
        ply: Option<usize>,
        tt: Table,
        eval: Option<Score>,
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
                root_bounds: RootBounds::Neither,
                ply: None,
                tt: Table::Miss,
                eval: None,
                history_max: None,
            }
        }

        /// The search as the decision reads it.
        fn search(&self) -> Search<'_> {
            Search {
                board: &self.board,
                ordering: &self.ordering,
                config: &self.config,
            }
        }

        /// A node as the move loop builds it, with the rules seeded with
        /// the memos the stand holds.
        fn node(&self, depth: u8, alpha: Score, beta: Score) -> (Node, Rules) {
            let node = Node::open(
                depth,
                self.in_check,
                self.ply,
                self.tt,
                0,
                None,
                FailSoft::open(alpha, beta, self.root_bounds, Taint::default()),
            );
            let mut rules = Rules::new(&self.search(), &node, self.eval);
            rules.history_max = self.history_max;
            (node, rules)
        }

        /// What the rules left in their memos, kept for the next question.
        fn keep(&mut self, rules: &Rules) {
            self.eval = rules.eval;
            self.history_max = rules.history_max;
        }

        /// The decision about one move. Each call is a node of its own:
        /// the held features are cleared first, so memories taught between
        /// two calls are read.
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
            let (mut node, mut rules) = self.node(depth, alpha, beta);
            node.answer.searched = searched;
            let verdict = decide(&self.search(), &node, &mut rules, &self.moves, m);
            self.keep(&rules);
            verdict
        }

        /// A node at one of the shallow rules' depths, as the move loop
        /// builds it, before its alpha is raised.
        fn rule(&self, depth: u8, beta: Score) -> (Node, Rules) {
            self.node(depth, 0, beta)
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
            let mut held = self.rule(depth, beta);
            self.asks(&mut held, m, searched, alpha)
        }

        /// The same at a node whose evaluation the move loop already
        /// seeded, as it seeds it from what `shortcuts` read.
        fn skips_seeded(
            &mut self,
            seed: Score,
            m: &Play,
            searched: usize,
            depth: u8,
            alpha: Score,
            beta: Score,
        ) -> bool {
            self.eval = Some(seed);
            self.history_max = None;
            let mut held = self.rule(depth, beta);
            self.asks(&mut held, m, searched, alpha)
        }

        /// The question of a node the caller is holding, with its alpha
        /// and searched count set as the loop would have them, so a test
        /// can ask one node twice and see what the latch carried.
        fn asks(
            &mut self,
            (node, rules): &mut (Node, Rules),
            m: &Play,
            searched: usize,
            alpha: Score,
        ) -> bool {
            node.answer.alpha = alpha;
            node.answer.searched = searched;
            let skips = rules.skips(&self.search(), node, m);
            self.keep(rules);
            skips
        }

        /// What the node would tell the ledger about one move, read
        /// without clearing what a verdict left behind. The depth and the
        /// bounds stand at nothing, since the features read neither.
        fn features(&mut self, m: &Play, searched: usize) -> Features {
            let (mut node, mut rules) = self.node(0, 0, 1);
            node.answer.searched = searched;
            let features = features(&self.search(), &node, &mut rules, &self.moves, m);
            self.keep(&rules);
            features
        }

        /// The position's static evaluation, which the bounds in the
        /// skip's tests are set against.
        fn eval(&self) -> i64 {
            i64::from(crate::eval::eval(&self.board))
        }

        /// Zero window bounds that put the move's score at the skip's
        /// threshold, where it is skipped, and the same with beta a point
        /// lower, where it is not.
        fn on_threshold(
            &mut self,
            m: &Play,
            searched: usize,
            depth: u8,
        ) -> ((Score, Score), (Score, Score)) {
            let f = self.features(m, searched);
            let eval_beta =
                (SKIP_THRESHOLD - model_score(depth, &f, 0, 0)).div_euclid(SKIP_EVAL_BETA);
            let beta = (self.eval() - eval_beta) as Score;
            ((beta - 1, beta), (beta - 2, beta - 1))
        }
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

    /// The reduction's node half is built once, and asked per move with
    /// the alpha standing, so it reads the mate window off the alpha it is
    /// handed rather than the one it was built at. Read with no board: the
    /// half reads the node and the configuration alone.
    #[test]
    fn a_mate_alpha_admits_no_reduction_at_any_count() {
        let config = reducing();
        let mut node = Node::open(
            LATE_MOVE_MIN_DEPTH,
            false,
            None,
            Table::Miss,
            0,
            None,
            FailSoft::open(0, 100, RootBounds::Neither, Taint::default()),
        );
        let half = admission(&config, &node);
        for searched in 0..64 {
            node.answer.searched = searched;
            assert_eq!(
                half.admits(&node),
                searched >= LATE_MOVE_THRESHOLD,
                "an ordinary alpha at {searched} searched"
            );
        }
        // alpha a mate against the side to move, under an ordinary beta
        node.answer.alpha = -29_500;
        assert!(
            crate::value::is_mate(node.answer.alpha) && !crate::value::is_mate(node.answer.beta)
        );
        for searched in 0..64 {
            node.answer.searched = searched;
            assert!(!half.admits(&node), "a mate alpha at {searched} searched");
        }
        // and a node that opened there admits nothing either
        let half = admission(&config, &node);
        for searched in 0..64 {
            node.answer.searched = searched;
            assert!(!half.admits(&node), "opened at a mate, {searched} searched");
        }
    }

    /// At a node that admits a reduction the move decides it: a quiet move
    /// is reduced, and a capture or a promotion is not. Read with no board,
    /// since the rule reads the node half and the move alone.
    #[test]
    fn a_node_that_admits_reduces_a_quiet_and_not_a_capture_or_a_promotion() {
        let config = reducing();
        let mut node = Node::open(
            LATE_MOVE_MIN_DEPTH,
            false,
            None,
            Table::Miss,
            0,
            None,
            FailSoft::open(0, 100, RootBounds::Neither, Taint::default()),
        );
        node.answer.searched = LATE_MOVE_THRESHOLD;
        let half = admission(&config, &node);
        assert!(half.admits(&node));
        let quiet = Play::new(0, 8, None, None, false, false);
        let capture = Play::new(0, 8, Some(Piece::Pawn), None, false, false);
        let promotes = Play::new(49, 57, None, Some(PromotePiece::Queen), false, false);
        assert!(half.reduces(&node, &quiet));
        assert!(!half.reduces(&node, &capture));
        assert!(!half.reduces(&node, &promotes));
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
        // a mate at beta alone, which the alpha test does not answer
        assert_eq!(
            s.verdict(
                &quiet,
                LATE_MOVE_THRESHOLD,
                LATE_MOVE_MIN_DEPTH,
                -100,
                29_500
            ),
            Verdict::Scout(0)
        );
    }

    /// The same bounds four ways, so what moves is the root bounds and
    /// nothing else. Beta is the bound read: the root's, the reduction is
    /// refused, and whether alpha is the root's makes no difference. The
    /// fourth case pins that alpha is never read.
    #[test]
    fn a_beta_that_is_still_the_roots_stands_the_reduction_down() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, reducing());
        let quiet = play_named(&s.board, "a4a5");
        let mut verdict = |root_bounds| {
            s.root_bounds = root_bounds;
            s.verdict(&quiet, LATE_MOVE_THRESHOLD, LATE_MOVE_MIN_DEPTH, -100, 100)
        };
        assert_eq!(
            verdict(RootBounds::Neither),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
        assert_eq!(verdict(RootBounds::Both), Verdict::Scout(0));
        assert_eq!(verdict(RootBounds::Beta), Verdict::Scout(0));
        assert_eq!(
            verdict(RootBounds::Alpha),
            Verdict::Scout(LATE_MOVE_REDUCTION)
        );
    }

    #[test]
    fn the_deep_reduction_stands_down_off_switch_and_under_its_floor() {
        // under the reference's switch nothing deepens, and the refusal
        // comes before the evaluation, so a node the gate never fires at
        // never pays for it
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
        // and at the floor with the same bounds it fires. Only the skip's
        // model reads the evaluation and the history, and the skip is off,
        // so neither is computed
        assert_eq!(
            s.verdict(&quiet, 10, DEEP_REDUCTION_MIN_DEPTH, alpha, beta),
            Verdict::Scout(DEEP_REDUCTION)
        );
        assert!(
            s.eval.is_none() && s.history_max.is_none(),
            "the fired gate filled the wrong memos"
        );
    }

    /// The index rule at the depth the deeper scout starts at: deepened at
    /// the floor and not one place earlier in the order. The bounds put the
    /// evaluation far over beta, so neither index is skipped.
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
        // an eval standing far over beta, nowhere near the skip's threshold
        let (alpha, beta): (Score, Score) = (-5_000, -4_999);
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
        let searched = DEEP_INDEX_FLOOR + 4;
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

    /// The gate driven with the bounds that put the model's score at its
    /// threshold, and with beta a point lower: at the threshold the move is
    /// skipped, under it the move falls through to the deep reduction,
    /// which the index rule gives a tenth move at depths four to six. Three
    /// depths, so the test sees the boundary move with depth rather than
    /// only fire, and an alpha far under beta keeps the move, since the
    /// model reads the window's width.
    #[test]
    fn the_pruning_fires_at_its_threshold_and_not_a_point_under_it() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, pruning());
        let quiet = play_named(&s.board, "a4a5");
        const SEARCHED: usize = 10;
        for depth in [DEEP_REDUCTION_MIN_DEPTH, 5, 6] {
            let ((alpha, beta), (inside_alpha, inside_beta)) =
                s.on_threshold(&quiet, SEARCHED, depth);
            assert_eq!(
                s.verdict(&quiet, SEARCHED, depth, alpha, beta),
                Verdict::Skip
            );
            assert_eq!(
                s.verdict(&quiet, SEARCHED, depth, alpha - 300, beta),
                Verdict::Scout(DEEP_REDUCTION)
            );
            assert_eq!(
                s.verdict(&quiet, SEARCHED, depth, inside_alpha, inside_beta),
                Verdict::Scout(DEEP_REDUCTION)
            );
        }
    }

    #[test]
    fn a_checking_quiet_is_never_skipped() {
        // the deep exemption's two moves under bounds that put both far
        // past the skip's threshold: the push is skipped and the
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
        // with the pruning off the same move is kept and the index rule
        // deepens its scout, which is what keeps the two arms separable
        // in an ablation
        let mut deep = Stand::new(fens::A_CAPTURE_AND_QUIETS, deep_reducing());
        let quiet = play_named(&deep.board, "a4a5");
        let (alpha, beta): (Score, Score) = (20_000, 20_001);
        assert_eq!(
            deep.verdict(&quiet, 10, 6, alpha, beta),
            Verdict::Scout(DEEP_REDUCTION)
        );
        // with the pruning alone on the model is still asked and the move
        // is still skipped: only the skip reads it
        let mut alone = Stand::new(fens::A_CAPTURE_AND_QUIETS, skipping());
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
    /// at every depth and index the gate can reach: the identity the table
    /// switch's bench claim rests on.
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
    /// becoming a depth of its own. At depth four and index four that is
    /// `DEEP_REDUCTION`.
    #[test]
    fn the_deep_gate_is_worth_one_ply_over_the_flat_amount() {
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

    /// The features the ledger records read the node as it stands. The
    /// pair that can part is the history and its denominator, one signed
    /// and one clamped, so this teaches the move part of the node's
    /// history.
    #[test]
    fn the_ledger_s_features_read_the_node_s_history_and_killers() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, skipping());
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
        let generated = s.moves.len();
        let f = s.features(&quiet, SEARCHED);
        assert_eq!(f.index, SEARCHED);
        assert_eq!(f.generated, generated);
        assert_eq!(f.history, 16);
        assert_eq!(f.history_max, 25);
        assert!(f.killer);
        assert_eq!(f.tt, Table::Miss);
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

    /// A node the count decides never evaluates and never walks the
    /// history denominator, with the margin beside it too, because the
    /// count is asked first.
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

    /// At the deep skip's floor both shallow rules are silent and the
    /// skip's verdict stands. The skipping bounds hold an alpha the
    /// futility margin would fire on a ply lower and a searched count past
    /// the count's line, so a ceiling that leaked a ply would skip the move
    /// the deep skip lets through a point under its threshold.
    #[test]
    fn neither_shallow_rule_decides_at_the_deep_skips_floor() {
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
        // a beta far past the margin, so the alpha under it stands over
        // the evaluation by more than the futility margin reaches a ply
        // lower, and a ceiling that leaked would skip
        let beta = (eval + 1_000) as Score;
        let alpha = beta - 1;
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
        // a point under the threshold the move is kept and the index rule
        // deepens its scout, and the shallow rules must not turn that into
        // a skip
        let (_, (inside_alpha, inside_beta)) = without.on_threshold(&quiet, SEARCHED, DEPTH);
        assert_eq!(
            without.verdict(&quiet, SEARCHED, DEPTH, inside_alpha, inside_beta),
            Verdict::Scout(DEEP_REDUCTION)
        );
        assert_eq!(
            with.verdict(&quiet, SEARCHED, DEPTH, inside_alpha, inside_beta),
            Verdict::Scout(DEEP_REDUCTION)
        );
    }

    /// A capture and a promotion are priced on material rather than on
    /// their place in the order, so neither rule skips one, and a
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

    /// Under `survives_shallow` the lazy quiet ordering keeps the moves the
    /// shallow rules would search, in key order. A run of five quiets: one
    /// that promotes without checking, one that checks and three that do
    /// neither. The check holds the larger history, so key order puts it
    /// ahead of the promotion generated before it.
    #[test]
    fn the_quiet_ordering_keeps_what_the_shallow_rules_search() {
        let mut s = Stand::new("7k/1P6/8/8/R7/8/8/7K w - - 0 1", counting());
        let named = |s: &Stand, name| play_named(&s.board, name);
        let promotes = named(&s, "b7b8n");
        let checks = named(&s, "a4a8");
        let neither = [named(&s, "a4a5"), named(&s, "a4a6"), named(&s, "h1g1")];
        assert!(!s.board.gives_check(&promotes) && promotes.promote.is_some());
        assert!(s.board.gives_check(&checks) && checks.promote.is_none());
        for m in &neither {
            assert!(!s.board.gives_check(m) && m.promote.is_none(), "{m}");
        }
        assert!(promotes.capture.is_none() && checks.capture.is_none());

        // the check is taught at another ply, so it is no killer at this one
        const PLY: usize = 0;
        let color = s.board.active_color;
        s.ordering.cutoff(color, &checks, &[], PLY + 1, 5);
        let mut run = vec![neither[0], promotes, neither[1], checks, neither[2]];
        assert_eq!(s.ordering.key_quiets(&s.board, &mut run, 0, PLY), run.len());
        let info = s.board.check_info();
        let kept = s
            .ordering
            .keep_unskippable(&mut run, 0, PLY, |m| survives_shallow(&s.board, &info, m));
        assert_eq!(run[..kept], [checks, promotes]);
        let mut rest = run[kept..].to_vec();
        rest.sort_by_key(|m| (m.from, m.to));
        let mut expected = neither.to_vec();
        expected.sort_by_key(|m| (m.from, m.to));
        assert_eq!(rest, expected);

        // and the rules, at a node where either would fire, drop the three
        // and neither of the two
        for (rule, config) in [("margin", quiet_futile()), ("count", counting())] {
            s.config = config;
            let alpha = (s.eval() + 10_000) as Score;
            for m in [promotes, checks] {
                assert!(
                    !s.skips(&m, PAST_THE_COUNT, 1, alpha, alpha + 1),
                    "{rule}: {m}"
                );
            }
            for m in neither {
                assert!(
                    s.skips(&m, PAST_THE_COUNT, 1, alpha, alpha + 1),
                    "{rule}: {m}"
                );
            }
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
            // a mate at beta alone, which the alpha test does not answer
            assert!(!s.skips(&quiet, PAST_THE_COUNT, 2, alpha, 29_500), "{rule}");

            s.root_bounds = RootBounds::Both;
            assert!(
                !s.skips(&quiet, PAST_THE_COUNT, 2, alpha, alpha + 1),
                "{rule}"
            );
            s.root_bounds = RootBounds::Alpha;
            assert!(
                s.skips(&quiet, PAST_THE_COUNT, 2, alpha, alpha + 1),
                "{rule}: alpha is not the bound read"
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

    /// The margin reads the evaluation the move loop seeded from the
    /// shortcuts and computes none itself, and never walks the history
    /// denominator.
    #[test]
    fn the_rule_reads_the_seeded_evaluation_and_no_history() {
        let mut s = Stand::new(fens::A_CAPTURE_AND_QUIETS, quiet_futile());
        let quiet = play_named(&s.board, "a4a5");
        // depth two on a seeded evaluation. The position's own evaluation
        // stands well over these bounds, so a rule that computed one
        // rather than reading the seed would not fire
        let (alpha, beta): (Score, Score) = (0, 1);
        let seed = alpha - QUIET_FUTILITY_MARGIN * 2;
        assert!(s.eval() + i64::from(QUIET_FUTILITY_MARGIN) * 2 > i64::from(alpha));
        assert!(!s.skips(&quiet, 1, 2, alpha, beta));
        assert!(s.skips_seeded(seed, &quiet, 1, 2, alpha, beta));
        assert_eq!(s.eval, Some(seed), "the rule recomputed the evaluation");
        assert!(s.history_max.is_none(), "the rule walked the history");
    }

    /// The margin's test is a latch. Asked here of one node under the
    /// margin's reach, then past it, then under it again, which a search
    /// cannot do and the latch has to survive.
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

    /// A rising alpha can climb into the mate window, and the latch must
    /// not carry the rule past that exemption.
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

    /// The denominator is read once for the node and held: every row a
    /// node records reads the largest at the first sample, not the largest
    /// by the time each staging ran, and a child search between two of a
    /// node's moves can teach the history.
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
