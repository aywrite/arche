// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use crate::board::Board;
use crate::census;
use crate::effort;
use crate::eval;
use crate::late_move;
use crate::limits::Limits;
use crate::misc::{Color, Score};
use crate::ordering::MoveOrdering;
use crate::play::Play;
use crate::recorder::{Sampler, Window};
use crate::reduction;
use crate::residual::{Sample, Shortcut};
use crate::transposition::{
    DEFAULT_TABLE_BYTES, GhiCounters, Probe, SignatureCounters, TranspositionTable,
};
use crate::value::{Taint, Value, below_the_mate_window, is_mate};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time;

/// The ply every search stops at: a requested depth is held to it, the
/// full width search ends a line at it whatever depth the check extension
/// has left, and the reported line is walked no further. It is also the
/// killer table's length, so a node past it would order its quiets without
/// one.
///
/// It sits well inside the two things a longer line would break: the
/// board's history ring (1024 plies, less the fifty move window) and the
/// mate score window (a thousand under the mate score). Sixty four would
/// fit; moving the rail is a play change with a match behind it, so it was
/// set where it need not move again.
pub const MAX_PLY: u8 = 128;
// the root deepens by one more when it is in check, so a depth held to the
// rail has to leave room for that inside a byte
const _: () = assert!(MAX_PLY < u8::MAX);
// How far above beta the static eval has to stand, per ply still to
// search, for a node to be answered from it: what the opponent may win
// back over those plies, a pawn a ply. The bench argues for less and not
// by much (sixty through a hundred and twenty span about five percent of
// the count, not monotone). What fixed the figure was the depth four mate
// in two in the_mate_distance_survives_a_deeper_warm_search: eighty nine
// lost it and ninety kept it, and a margin one notch from a mate it can
// miss is no margin, so this is the round number above that boundary, at
// about two thirds of a percent of the tree over ninety. The boundary was
// between eighty five and ninety when the figure was chosen, ninety one
// before the piece square tables were fitted, and ninety at the last
// reading, so it is re-measured rather than read off this line; the round
// number above it has been a hundred each time. That test no longer holds
// the boundary. The late move count prunes the quiet that begins the mate
// at depths three to five, so the test's floor is one of its four depths,
// and a margin of eighty nine passes it with the count on and with it off.
// Re-measure the boundary on this position before moving the figure.
// docs/ROADMAP.md has the shadow lane's reading.
const REVERSE_FUTILITY_MARGIN: Score = 100;
// The deepest node the margin may answer. The margin grows a fixed step a
// ply, and the bench says the plies past this prune nothing: four, six and
// eight are the same count to a tenth of a percent.
const REVERSE_FUTILITY_MAX_DEPTH: u8 = 4;
// How many plies shallower than the node the pass itself is searched: what
// the shortcut costs. Too large and the reduced search proves nothing, too
// small and it costs what searching the moves would have. Two is the
// opening value; what moves it is a match, not the bench.
const NULL_MOVE_REDUCTION: u8 = 2;
// One more than the base reduction, so the pass at the shallowest depth it
// is offered at is searched at depth zero and no lower. At that floor the
// reduced search is quiescence. The floor is the base and not whatever the
// depth term grows the reduction to, because the reduction is clamped to
// what the depth leaves (`null_move_reduction`) rather than the depth being
// raised to fit it.
const NULL_MOVE_MIN_DEPTH: u8 = NULL_MOVE_REDUCTION + 1;
// How many plies of depth buy one more ply of reduction. A pass proves less
// the shallower it is searched, and what it costs to search is what the
// depth below the node costs, which grows with that depth: so the plies
// worth spending on the proof grow slower than the node's own depth. Six is
// the conventional step and takes the reduction to three from depth six.
// The bench did not choose it: three, four, five, six and eight read
// -3.23%, -3.13%, -1.42%, -1.57% and -0.63% at depth nine, which is not
// monotone and so is not a ranking. What moves it is a match. It leaves
// depths three to five alone, which is what gives the residual sampler a
// band the arm does not touch to be read against.
const NULL_MOVE_DEPTH_DIVISOR: u8 = 6;
// How far the static evaluation must stand above beta to buy one more ply
// of reduction. A pawn is a hundred on this scale, so this is a ply for
// every two pawns of clearance. The term is a bet that the wider the
// margin the safer the pass, and the residual sampler is what the bet was
// read against before it was taken: over the pass's own rows the wide band
// crossed less often than the narrow one.
const NULL_MOVE_EVAL_UNIT: Score = 200;
// The most plies the margin alone may add. Past three the pass proves
// almost nothing whatever the margin says, and the margins that reach that
// far are the positions a pass was never the cheap answer to.
const NULL_MOVE_EVAL_CAP: u8 = 3;
// How far short of alpha a capture may leave the standing eval, with the
// captured piece counted as fully won, and still be searched in
// quiescence: the positional ground a capture can make up beyond the
// piece. Two hundred is the conventional figure for conventional piece
// values.
const DELTA_MARGIN: Score = 200;
// How far either side of the previous iteration's score the root opens.
// A pawn is a hundred, so this is three tenths of one: wide enough that
// most iterations land inside it, and narrow enough that the first root
// move's own subtree is searched under bounds a search can reach rather
// than under the mate edges. Read off the bench at depth nine over ten,
// fifteen, twenty, thirty and forty, which prices the re-searches a narrow
// window pays for: only fifteen, twenty and thirty cost less there than
// opening full, and of those this is the widest inside a percent of the
// cheapest, a window being the thing that fails on the positions the suite
// does not hold. It is also the cheapest of all five at depth seven. What
// moves it is another sweep, not a guess.
const ASPIRATION_WIDTH: Score = 30;
// The first depth the root opens narrow at. Below it the whole iteration
// costs less than one re-search deeper down, and the score at depth two
// predicts depth three badly. A judgment rather than a swept figure.
const ASPIRATION_MIN_DEPTH: u8 = 5;
// How many times one side of the window may fail before that side opens to
// the edge. The width doubles each time, so the sides tried are the width,
// twice it, four times it, and then the edge; a fifth try buys little over
// the edge and costs a whole re-search on the positions that swing that
// far.
const ASPIRATION_FAILURES: u8 = 3;

/// How many plies shallower than the node a pass at `depth` is searched.
/// `eval_beta` is how far the static evaluation stands above beta at the
/// node, which the pass gate has already found to be at least zero.
///
/// The flat base with two terms on top of it, one on the depth and one on
/// that margin, held to what the depth leaves. The clamp is the whole of
/// the safety here: `depth - 1 - r` is the reduced search's depth and is
/// unsigned, so an `r` past `depth - 1` would not be an over-reduction but
/// a wrap to an enormous depth. The depth term alone never reaches it (a
/// sixth of the depth never catches the depth), the margin term does at
/// the shallowest depths, and the reduced search is then quiescence, as it
/// is at the floor.
///
/// Off the flag this is the base and nothing else, which is what holds the
/// bench identical to the flat reduction's.
fn null_move_reduction(config: SearchConfig, depth: u8, eval_beta: Score) -> u8 {
    if !config.adaptive_null_move {
        return NULL_MOVE_REDUCTION;
    }
    let margin = (eval_beta.max(0) / NULL_MOVE_EVAL_UNIT).min(NULL_MOVE_EVAL_CAP as Score) as u8;
    let grown = NULL_MOVE_REDUCTION + depth / NULL_MOVE_DEPTH_DIVISOR + margin;
    grown.min(depth - 1)
}

/// Which places in a node's move list the node made and searched, a bit
/// each: under a cutoff the quiet moves with a bit below the cutting
/// move's place are the history's malus.
///
/// Four words: a position can hold two hundred and eighteen moves, and the
/// list buffer is sixty four wide and spills past that.
#[derive(Default)]
struct Searched([u64; 4]);

impl Searched {
    fn mark(&mut self, place: usize) {
        debug_assert!(place < 64 * 4, "a move list wider than the mask");
        self.0[place / 64] |= 1 << (place % 64);
    }

    fn holds(&self, place: usize) -> bool {
        self.0[place / 64] >> (place % 64) & 1 == 1
    }

    /// How many places are marked. The move loop holds this to its own
    /// count of the moves it searched, so a stray mark shows up at the next
    /// move searched rather than as a malus elsewhere in the tree; a move
    /// the model skipped and a move that turned out illegal raise neither.
    fn count(&self) -> usize {
        self.0.iter().map(|word| word.count_ones() as usize).sum()
    }
}

/// What the protocol interface asks of an engine: positions in, answers
/// out. The deepening loop is required rather than provided because how an
/// implementation searches is its own business.
pub trait Engine {
    fn parse_fen(&mut self, fen_string: &str) -> Result<(), String>;

    /// Forget what was learned from the game just finished. Stored scores do not
    /// account for repetition or the fifty move counter, so a position that
    /// comes up again in a new game would otherwise be scored from a line that
    /// no longer applies to it.
    fn new_game(&mut self);

    fn make_move_str(&mut self, play: &str) -> bool;

    /// Give the engine a transposition table of `bytes` bytes, discarding
    /// whatever the old one held: a bucket is chosen from the number of
    /// buckets there are, so every entry moves when that number does.
    ///
    /// False if the buckets could not be reserved, in which case the engine
    /// keeps the table it had: the size arrives from an interface, which
    /// may ask for more than the machine has, and a game is better carried
    /// on with the old table than lost with no engine.
    ///
    /// The answer is the allocator's. It is not a promise that the memory
    /// is there to use: where the kernel overcommits, a size that fits in
    /// ram and swap is granted here and the process killed later as the
    /// entries are written.
    #[must_use]
    fn set_table_bytes(&mut self, bytes: usize) -> bool;

    /// Empty the transposition table and leave everything else as it is:
    /// the protocol's `Clear Hash`. A size change empties the table too, by
    /// building another; this keeps the buckets.
    fn clear_table(&mut self);

    /// The position, printed the way the board prints itself. A string
    /// rather than a write: the library never prints, and the adapter owns
    /// where its bytes go and what lock they take.
    fn board_display(&self) -> String;

    fn perft(&mut self, depth: u8) -> u64;

    fn active_color(&self) -> Color;

    /// Search each depth in turn until one is the last to finish. Every
    /// completed iteration is reported through `on_depth`, which is where a
    /// protocol adapter reports progress from; the library never prints. A
    /// result's node count covers the whole deepening so far, not the one
    /// iteration, which is what the uci info convention expects and what
    /// makes it divisible by the time since the search began.
    ///
    /// One report is not a completed iteration: the answer an aborted
    /// iteration replaces a completed one with is reported too, as a lower
    /// bound, since nothing else would have named it.
    fn iterative_deepening_search(
        &mut self,
        search_options: SearchParameters,
        on_depth: impl FnMut(u8, &SearchResult, PvLine, ScoreBound),
    ) -> SearchOutcome;
}

pub struct SearchParameters {
    /// The depth to deepen to, or none for as deep as the engine goes.
    /// Reaching it is how a search finishes, which is why it is not one of
    /// the limits below.
    pub depth: Option<u8>,
    /// What the search may spend: the clock it started on and the nodes it
    /// may visit.
    pub limits: Limits,
    /// Set by another thread to stop the search at the next poll, which is
    /// how the protocol's `stop` reaches a search already running. None for
    /// a search nobody can interrupt. Beside the limits rather than inside
    /// them so that `Limits` stays `Copy`.
    pub stop: Option<Arc<AtomicBool>>,
}

impl SearchParameters {
    /// A search to the depth given, under the limits given, which nothing
    /// can stop early.
    pub fn new(depth: Option<u8>, limits: Limits) -> Self {
        Self {
            depth,
            limits,
            stop: None,
        }
    }

    /// The same, with a flag another thread may set to stop it.
    pub fn stoppable(depth: Option<u8>, limits: Limits, stop: Arc<AtomicBool>) -> Self {
        Self {
            depth,
            limits,
            stop: Some(stop),
        }
    }

    /// Everything one iteration may be stopped by. Until a depth has been
    /// answered there is nothing to answer with, so nothing (clock, budget
    /// or flag) may stop the search; that is what makes every `go` end in a
    /// real move.
    fn for_iteration(&self, answered: bool, spent: u64) -> (Limits, Option<Arc<AtomicBool>>) {
        let stop = if answered { self.stop.clone() } else { None };
        (self.limits.for_iteration(answered, spent), stop)
    }

    /// A search to a fixed depth and nothing else: no clock, no budget.
    pub fn to_depth(depth: u8) -> Self {
        Self::new(Some(depth), Limits::unlimited())
    }
}

/// The policies a search runs under: the shortcuts it takes and the scores
/// it trusts. Each is a fact about the tree searched, so changing one
/// moves the bench.
///
/// Two configurations are named. The reference has every shortcut off and
/// every refusal on: alpha-beta with a table that only speeds it up, so a
/// position searched warm answers as it does cold, deepened as it does
/// direct, and with a small table as with a large one. The exactness tests
/// hold the reference to that and a shortcut leaves it alone. The default
/// is what the engine plays with, every shortcut on, and the two played
/// against each other say what the shortcuts are worth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchConfig {
    /// What the search does about draw tainted transposition scores, the
    /// ones that describe the path that stored them rather than the
    /// position. The policies are the graph history experiment's arms;
    /// what each costs is measured, not written here: run
    /// `bench hash <MB> taint <word>` against another word.
    pub taint: TaintPolicy,
    /// Whether a node near the leaves may answer from its static evaluation
    /// alone when that stands far enough above beta.
    pub reverse_futility: bool,
    /// Whether a node whose eval already stands above beta may hand the
    /// move to the other side and answer from a reduced search of that.
    pub null_move: bool,
    /// Whether the plies the pass is searched shallower by grow with the
    /// node's depth and with how far the static evaluation stands above
    /// beta, rather than being the flat two. It changes no node's
    /// eligibility to pass and nothing the four gates decide, only how
    /// dearly a node that passes buys its proof. Rides on `null_move`: a
    /// node that never passes is never asked. Off it the reduction is read
    /// as it was, which is what the bench identity holds it to.
    pub adaptive_null_move: bool,
    /// Whether quiescence may skip a capture that leaves the standing eval
    /// a margin short of alpha with its piece counted as fully won.
    pub delta_margin: bool,
    /// Whether quiescence may skip a capture the swap prices as losing. The
    /// swap sees no pins and nothing beyond its square.
    pub see_pruning: bool,
    /// Whether a quiet move searched late at a full width node is scouted
    /// shallower first, by what the reduction table reads or by a flat ply
    /// with that off, and searched at full depth only when the scout comes
    /// back above alpha. A scout that fails low is trusted.
    pub late_move_reductions: bool,
    /// Whether the scout of a late quiet the gate prices as dead runs a ply
    /// shallower still, at nodes deep enough for the scout to keep its full
    /// width ply, and never for a move that gives check. What prices it is
    /// the attention model's threshold, or the index rule below when that is
    /// on. Rides on `late_move_reductions`: a move the reduction never
    /// touches is never asked.
    pub deep_reductions: bool,
    /// Whether a late quiet the attention model prices in its deadest band
    /// is searched at all. Rides on the reduction's eligibility and the
    /// deep reduction's depth floor and checking exemption; its threshold is
    /// a deeper cut of the attention model's score.
    pub late_move_pruning: bool,
    /// Whether a quiet move after the node's first is dropped at depths one
    /// to three because the node's static evaluation plus
    /// `QUIET_FUTILITY_MARGIN` a ply cannot reach alpha. Its ceiling is a
    /// ply under `DEEP_REDUCTION_MIN_DEPTH`, so the rule and the attention
    /// model never decide at one depth, and it carries the shortcuts'
    /// exemptions: not in check, no mate window, beta not the root's, and a
    /// side with a piece besides pawns. A capture, a promotion and a quiet
    /// that gives check are exempt as the reduction's are. Off, the search
    /// is the one that was there before, which the bench identity holds it
    /// to.
    pub quiet_futility: bool,
    /// Whether a quiet move after the node's first is dropped at depths one
    /// to three because the node has already searched `LATE_MOVE_COUNT`
    /// moves a ply. It shares the rule above's ceiling and exemptions and
    /// reads none of the evaluation: a node this alone decides never
    /// computes one. A switch of its own rather than the one above, so an
    /// ablation can tell the two apart. Off, the search is the one that was
    /// there before, which the bench identity holds it to.
    pub late_move_count: bool,
    /// Whether the amount a late quiet is scouted shallower by grows with
    /// the node's depth and the move's place in the order, rather than
    /// being the flat ply and the gate's second one. It changes no move's
    /// eligibility and nothing the gate decides, only how far the scout of
    /// a move already reduced is stood back. On in the default, off in the
    /// reference, and off it the two constants are read as they were,
    /// which is what the bench identity holds it to.
    pub reduction_table: bool,
    /// Whether the deep reduction's extra ply is decided by the move's index
    /// against a floor that rises with depth, rather than by the attention
    /// model's threshold. It changes nothing the skip decides and nothing
    /// about the amount. On in the default, off in the reference, and off it
    /// the model's threshold is read as it was, which is what the bench
    /// identity holds it to.
    pub deep_index_rule: bool,
    /// Whether a node orders its quiet moves by what other nodes have
    /// learned: the killers for its distance from the root, and the history
    /// table under them. Off in the reference, which keeps the pinned
    /// reference tree the one alpha-beta and the capture ordering produce.
    ///
    /// Ordering rather than pruning, so under the reference the answer is
    /// the same either way and only the tree moves. Under the default a
    /// shortcut fires against the window the parent's search order
    /// produced, so the default's move and score may move where the
    /// reference's may not.
    pub move_memory: bool,
    /// Whether the deepening loop opens each iteration from
    /// `ASPIRATION_MIN_DEPTH` on at a window around the last one's score
    /// rather than at the full one, widening the side that fails until the
    /// score lands inside.
    ///
    /// Off in the reference. A window is a cost policy: it changes how
    /// dearly a depth is reached rather than what the depth answers, so it
    /// belongs on the measured side and the reference's pinned tree stays
    /// the control the default's is read against. Nothing outside the
    /// deepening loop reads it, so a search asked for a fixed depth opens
    /// full whatever this says.
    pub aspiration: bool,
}

/// What to do with a draw tainted score: one stored by a search that read
/// a repetition or fifty move draw below it, which a search arriving down
/// another path may not be able to reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaintPolicy {
    /// Store tainted scores and refuse their cutoffs, trusting only the
    /// move. The reference: the table only ever speeds the search up, and
    /// a warm answer is a cold one.
    Refuse,
    /// Store tainted scores and take their cutoffs as if the taint were
    /// not there: the control arm, what an engine with no taint bit does.
    Trust,
    /// Never store a tainted score, so a probe cannot read one; the slot
    /// keeps whatever it had. The root's answer still stores whatever it
    /// is, since the reported line is read back from its slot, so the rare
    /// tainted cutoff that offers is refused as under `Refuse`.
    Skip,
    /// No taint accounting at the probe; instead every cutoff is refused
    /// once the fifty move counter reaches the guard, which is how
    /// Stockfish's main search plays; this engine applies the guard in
    /// quiescence besides. The taint still travels and is still counted,
    /// so the figures say what this policy trusts that the others refuse.
    Rule50,
}

impl TaintPolicy {
    /// Whether a probe refuses the cutoff a tainted entry offers.
    pub(crate) fn refuses_tainted_cutoffs(self) -> bool {
        matches!(self, TaintPolicy::Refuse | TaintPolicy::Skip)
    }

    /// Whether a probe refuses every cutoff near the fifty move horizon.
    pub(crate) fn guards_rule50(self) -> bool {
        matches!(self, TaintPolicy::Rule50)
    }

    /// Whether a tainted result is stored at all.
    pub(crate) fn stores_tainted(self) -> bool {
        !matches!(self, TaintPolicy::Skip)
    }
}

impl SearchConfig {
    /// The search with every shortcut off: what the exactness tests hold
    /// the search to, and the side a shortcut is measured against.
    pub const fn reference() -> Self {
        Self {
            taint: TaintPolicy::Refuse,
            reverse_futility: false,
            null_move: false,
            adaptive_null_move: false,
            delta_margin: false,
            see_pruning: false,
            late_move_reductions: false,
            deep_reductions: false,
            late_move_pruning: false,
            quiet_futility: false,
            late_move_count: false,
            reduction_table: false,
            deep_index_rule: false,
            move_memory: false,
            aspiration: false,
        }
    }

    /// The word the bench prints for what this configuration does with a
    /// draw tainted score, and reads back with `with_taint`. The words are
    /// the policies the graph history experiments compare.
    pub fn taint_word(self) -> &'static str {
        match self.taint {
            TaintPolicy::Refuse => "refuse",
            TaintPolicy::Trust => "trust",
            TaintPolicy::Skip => "skip",
            TaintPolicy::Rule50 => "rule50",
        }
    }

    /// The default with its taint policy set by word, or none for a word
    /// that is no policy. The default rather than the reference, so the
    /// word a bench's header prints names what it ran and can be handed
    /// back to it.
    pub fn with_taint(word: &str) -> Option<Self> {
        let taint = match word {
            "refuse" => TaintPolicy::Refuse,
            "trust" => TaintPolicy::Trust,
            "skip" => TaintPolicy::Skip,
            "rule50" => TaintPolicy::Rule50,
            _ => return None,
        };
        Some(Self {
            taint,
            ..Self::default()
        })
    }
}

impl Default for SearchConfig {
    /// What the engine plays with: every shortcut on, the memories on, and
    /// the table trusted behind the fifty move guard. The four taint
    /// policies played each other and trusting won, at +48 ±23 over 308
    /// games at 5+0.05, paid in shallower endgame search; the guard
    /// cost nothing a match could see and covers the one regime where a
    /// wrong cutoff provably loses. The shortcuts are guesses about the
    /// tree rather than rules about a score, which is why the reference
    /// keeps them off; the memories prune nothing and are off there only
    /// so its tree stays the one the capture ordering produces.
    ///
    /// The pruning cuts nothing falsely on its own account: skipping a move
    /// can only lower this node's answer, never raise it, and the lowered
    /// answer travels as every bound does. What it risks is a good move
    /// written off, which the threshold's band prices.
    fn default() -> Self {
        Self {
            taint: TaintPolicy::Rule50,
            reverse_futility: true,
            null_move: true,
            adaptive_null_move: true,
            delta_margin: true,
            see_pruning: true,
            late_move_reductions: true,
            deep_reductions: true,
            late_move_pruning: true,
            quiet_futility: true,
            late_move_count: true,
            reduction_table: true,
            deep_index_rule: true,
            move_memory: true,
            aspiration: true,
        }
    }
}

/// Which of a node's two bounds is still the one the root opened with,
/// rather than a score a search returned. That is narrower than being a
/// principal variation node: a node searched at an open window can have
/// neither bit set, which is what the proof of a node whose beta is a
/// returned score is, and a node at a zero window never has one.
///
/// A bound the root opened with is one the tree under it has said nothing
/// about, which is what the shortcuts and the late move reduction are
/// refused on wherever beta is one: the principal variation exemption,
/// written here rather than left to the mate window gates beside it.
///
/// Both bits at the root. Where they go from there is `child` and
/// `alpha_raised` and nowhere else, so no call site spells the rule out
/// for itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RootBounds {
    pub(crate) alpha: bool,
    pub(crate) beta: bool,
}

/// The searches a node asks of a child, which is what the bits a child
/// carries are keyed on.
#[derive(Clone, Copy)]
enum ChildSearch {
    /// The node's first move, at the window as it stands.
    FirstMove,
    /// A late move's zero width search, run shallower.
    Scout,
    /// A later move's zero width search at the full depth.
    Probe,
    /// The full window search a probe that beat alpha asks for.
    Proof,
    /// The null move pass's zero width search.
    Pass,
}

impl RootBounds {
    /// The root's own window: both bounds are the ones it opened with.
    pub(crate) const BOTH: Self = Self {
        alpha: true,
        beta: true,
    };
    /// Neither bound is the root's.
    pub(crate) const NEITHER: Self = Self {
        alpha: false,
        beta: false,
    };

    /// What a child search carries. The first move and the proof take
    /// this node's window turned round, so they take these bits turned
    /// round with it: the child's alpha is this node's beta negated, and
    /// the other way about. The other three take a zero window, which is
    /// this node's own question about alpha rather than the window the
    /// root opened, so they take neither bound.
    fn child(self, search: ChildSearch) -> Self {
        match search {
            ChildSearch::FirstMove | ChildSearch::Proof => Self {
                alpha: self.beta,
                beta: self.alpha,
            },
            ChildSearch::Scout | ChildSearch::Probe | ChildSearch::Pass => Self::NEITHER,
        }
    }

    /// What a raise of alpha leaves. Alpha is a score a child returned
    /// from there on, so its bit goes; beta is untouched, since a raised
    /// alpha proves nothing about it.
    fn alpha_raised(self) -> Self {
        Self {
            alpha: false,
            beta: self.beta,
        }
    }
}

#[cfg(test)]
mod root_bounds {
    use super::{ChildSearch, RootBounds};
    use pretty_assertions::assert_eq;

    /// Where the propagation rule lives is `child` and `alpha_raised`,
    /// and this is what holds them to it. The bench cannot: at the full
    /// window every bit the search reads is read beside a mate gate that
    /// answers the same way, so a child handed the wrong bits moves no
    /// node count. The bounds the cases start from are asymmetric for
    /// the same reason, since a flip the wrong way round is invisible on
    /// a pair that agree.
    #[test]
    fn what_a_child_carries_and_what_a_raise_leaves() {
        let alpha_only = RootBounds {
            alpha: true,
            beta: false,
        };
        let beta_only = RootBounds {
            alpha: false,
            beta: true,
        };

        assert_eq!(alpha_only.child(ChildSearch::FirstMove), beta_only);
        assert_eq!(alpha_only.child(ChildSearch::Proof), beta_only);
        assert_eq!(alpha_only.child(ChildSearch::Scout), RootBounds::NEITHER);
        assert_eq!(alpha_only.child(ChildSearch::Probe), RootBounds::NEITHER);
        assert_eq!(alpha_only.child(ChildSearch::Pass), RootBounds::NEITHER);
        // the leftmost line: the root's own window turned round is still
        // the root's own window
        assert_eq!(
            RootBounds::BOTH.child(ChildSearch::FirstMove),
            RootBounds::BOTH
        );
        assert_eq!(
            RootBounds::NEITHER.child(ChildSearch::Proof),
            RootBounds::NEITHER
        );

        assert_eq!(RootBounds::BOTH.alpha_raised(), beta_only);
        assert_eq!(alpha_only.alpha_raised(), RootBounds::NEITHER);
        assert_eq!(beta_only.alpha_raised(), beta_only);
        assert_eq!(RootBounds::NEITHER.alpha_raised(), RootBounds::NEITHER);
    }
}

/// The window the deepening loop opens an iteration at, and what a failed
/// iteration widens it to.
///
/// A deepening search knows roughly what the next iteration is worth: the
/// last one's score. Two things come of opening around it. The first root
/// move's own subtree is searched under bounds a search can reach instead
/// of the mate edges, which is where most of the saving is; the later
/// moves were already scouted at a zero window against the first move's
/// score, since the first move always raised alpha at the full window. And
/// where the first move comes back under the window, alpha stays at the
/// window's floor rather than dropping to that score, so the moves after
/// it are scouted against the tighter of the two. The price is an
/// iteration whose score lands outside the window, which proves only a
/// bound and has to be searched again wider.
///
/// A value of its own rather than a pair of scores in the loop, so the
/// rule is a thing that can be tested without running a search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Aspiration {
    alpha: Score,
    beta: Score,
    /// The score the window is centred on, which each widening is measured
    /// from. Meaningless where both sides are already at the edge.
    centre: Score,
    /// How often each side has failed. A side at `ASPIRATION_FAILURES` is
    /// at the edge and stays there.
    low_failures: u8,
    high_failures: u8,
}

impl Aspiration {
    /// The widest the root is ever opened: one inside the score type on
    /// each side, so both bounds can be negated.
    const FULL_ALPHA: Score = Score::MIN + 1;
    const FULL_BETA: Score = Score::MAX - 1;

    /// The window a depth opens at. `previous` is the last completed
    /// iteration's score, or none where there is none to aim at or the
    /// configuration does not aspire.
    ///
    /// Fully open below the starting depth, and fully open around a mate
    /// score, which is not a centipawn estimate: a window around one would
    /// refuse the alternatives to the mate for nothing.
    fn open(previous: Option<Score>, depth: u8) -> Self {
        let full = Self {
            alpha: Self::FULL_ALPHA,
            beta: Self::FULL_BETA,
            centre: 0,
            low_failures: ASPIRATION_FAILURES,
            high_failures: ASPIRATION_FAILURES,
        };
        let Some(centre) = previous else {
            return full;
        };
        if depth < ASPIRATION_MIN_DEPTH || is_mate(centre) {
            return full;
        }
        Self {
            alpha: Self::below(centre, 0),
            beta: Self::above(centre, 0),
            centre,
            low_failures: 0,
            high_failures: 0,
        }
    }

    /// The window after an iteration answered `bound` rather than a score
    /// inside this one. Only the side that failed moves: the other proved
    /// nothing, and widening both on every failure would reach the full
    /// window in three failures where this takes six.
    fn widen(self, bound: ScoreBound) -> Self {
        match bound {
            ScoreBound::Upper => {
                let low_failures = self.low_failures.saturating_add(1);
                Self {
                    alpha: Self::below(self.centre, low_failures),
                    low_failures,
                    ..self
                }
            }
            ScoreBound::Lower => {
                let high_failures = self.high_failures.saturating_add(1);
                Self {
                    beta: Self::above(self.centre, high_failures),
                    high_failures,
                    ..self
                }
            }
            // an iteration whose score landed inside its window is the one
            // the loop answers with, so nothing widens after it
            ScoreBound::Exact => self,
        }
    }

    /// How far under the centre the alpha side stands after `failures`
    /// failures: the width doubled once per failure, and the edge once the
    /// doublings are spent.
    ///
    /// No clamp against the edge, because no width the sweep chooses from
    /// can reach it. A centre the mate gate let through is inside the mate
    /// window, so under thirty thousand, and the widest the doublings
    /// reach is four times the width; a width of a few hundred still
    /// leaves thousands of room. The arithmetic saturates rather than
    /// wrapping, which is not a licence to set the width by the thousand:
    /// a width that large wants the clamp back.
    fn below(centre: Score, failures: u8) -> Score {
        if failures >= ASPIRATION_FAILURES {
            return Self::FULL_ALPHA;
        }
        centre.saturating_sub(Self::reach(failures))
    }

    /// The beta side of the same.
    fn above(centre: Score, failures: u8) -> Score {
        if failures >= ASPIRATION_FAILURES {
            return Self::FULL_BETA;
        }
        centre.saturating_add(Self::reach(failures))
    }

    /// The width after `failures` doublings.
    fn reach(failures: u8) -> Score {
        ASPIRATION_WIDTH.saturating_mul(1 << failures)
    }
}

#[cfg(test)]
mod aspiration {
    use super::{ASPIRATION_MIN_DEPTH, ASPIRATION_WIDTH, Aspiration, Score, ScoreBound};
    use pretty_assertions::assert_eq;

    /// The width the whole schedule is written in, so a sweep that moves
    /// the constant moves these cases with it.
    const W: Score = ASPIRATION_WIDTH;
    const AT: u8 = ASPIRATION_MIN_DEPTH;

    fn full() -> Aspiration {
        Aspiration::open(None, AT)
    }

    #[test]
    fn the_window_a_depth_opens_at() {
        // below the starting depth the score predicts the next one badly
        // and the whole iteration costs less than one re-search deeper
        assert_eq!(Aspiration::open(Some(30), AT - 1), full());
        // and with nothing to aim at there is no centre
        assert_eq!(Aspiration::open(None, AT + 4), full());

        let opened = Aspiration::open(Some(30), AT);
        assert_eq!((opened.alpha, opened.beta), (30 - W, 30 + W));

        // a mate score is not a centipawn estimate, so a window round it
        // would refuse the alternatives to the mate for nothing
        assert_eq!(Aspiration::open(Some(29_995), AT + 4), full());
        assert_eq!(Aspiration::open(Some(-29_995), AT + 4), full());
    }

    #[test]
    fn a_failure_moves_the_side_that_failed_and_no_other() {
        let opened = Aspiration::open(Some(30), AT);

        let low = opened.widen(ScoreBound::Upper);
        assert_eq!((low.alpha, low.beta), (30 - 2 * W, 30 + W));
        let high = opened.widen(ScoreBound::Lower);
        assert_eq!((high.alpha, high.beta), (30 - W, 30 + 2 * W));

        // and each doubling is measured from the centre, not from where
        // the last one left the bound
        let twice = low.widen(ScoreBound::Upper);
        assert_eq!((twice.alpha, twice.beta), (30 - 4 * W, 30 + W));
    }

    #[test]
    fn three_failures_open_that_side_to_the_edge() {
        let mut window = Aspiration::open(Some(30), AT);
        for _ in 0..3 {
            window = window.widen(ScoreBound::Upper);
        }
        assert_eq!(window.alpha, Aspiration::FULL_ALPHA);
        // the other side is where it was opened: a fail low says nothing
        // about beta
        assert_eq!(window.beta, 30 + W);

        for _ in 0..3 {
            window = window.widen(ScoreBound::Lower);
        }
        // both sides spent, which is the full window and where the
        // widening stops
        assert_eq!((window.alpha, window.beta), (full().alpha, full().beta));
    }
}

pub struct AlphaBeta {
    pub(crate) board: Board,
    config: SearchConfig,
    nodes: u64,
    transpositions: TranspositionTable,
    selective_depth: u8,
    // search state
    /// What the search call under way may spend. The deepening loop hands
    /// each iteration its own, which is how depth one runs with none.
    limits: Limits,
    /// The node count at which the limits are looked at next, which the
    /// limits themselves decide.
    next_check: u64,
    /// The flag another thread sets to stop the search, or none while
    /// nothing may. Armed by `SearchParameters::for_iteration` exactly as
    /// the clock is, and written by `search_root` from its signature, so a
    /// search asked directly for a depth never reads one.
    stop: Option<Arc<AtomicBool>>,
    /// The nodes quiescence visited, a part of `nodes`. Counted for the
    /// bench, which reports what share of the tree the captures are. Never
    /// reset, like the ghi counters: the bench reads it from an engine made
    /// for the one search.
    quiescence_nodes: u64,
    /// The move ordering and its scratch buffer: one per engine, reused by
    /// every node.
    ordering: MoveOrdering,
    /// The leaf terms' memos. Never cleared between searches: an entry is
    /// read only against the key that wrote it, so what the last search
    /// left is a warm start and not a stale answer.
    caches: eval::Caches,
    /// The residual sampler, or none, which is what every constructor
    /// builds. An engine with none takes no branch a search without a
    /// sampler did not take, which is what the pinned node counts stand on.
    sampler: Option<Sampler<Sample>>,
    /// The cutoff census's reservoir, or none, on the sampler's terms.
    census: Option<Sampler<census::Event>>,
    /// The reduction ledger's reservoir, or none, on the same terms.
    ledger: Option<Sampler<reduction::Event>>,
    /// The effort instrument's reservoir, or none, on the same terms.
    effort: Option<Sampler<effort::Event>>,
    /// Every event the effort instrument has been offered, by depth. Beside
    /// its reservoir rather than inside it because the reservoir holds a
    /// sample and this counts the population. Bumped only behind the
    /// reservoir's own check, so an engine that was never armed counts
    /// nothing.
    effort_depths: effort::Depths,
}

/// What a search can be armed to record: the residual's sample, the cutoff
/// census's event, the reduction ledger's or the effort instrument's.
/// Implemented here rather than beside the event types because what each
/// names is a field of the engine.
pub(crate) trait Recorded: Sized {
    /// What the shared recording loop calls a run of this kind, which is
    /// how it names a position it cannot read.
    const WHAT: &'static str;

    /// The engine's slot for a reservoir of this kind.
    fn slot(engine: &mut AlphaBeta) -> &mut Option<Sampler<Self>>;
}

impl Recorded for Sample {
    const WHAT: &'static str = "residual";

    fn slot(engine: &mut AlphaBeta) -> &mut Option<Sampler<Self>> {
        &mut engine.sampler
    }
}

impl Recorded for census::Event {
    const WHAT: &'static str = "census";

    fn slot(engine: &mut AlphaBeta) -> &mut Option<Sampler<Self>> {
        &mut engine.census
    }
}

impl Recorded for reduction::Event {
    const WHAT: &'static str = "ledger";

    fn slot(engine: &mut AlphaBeta) -> &mut Option<Sampler<Self>> {
        &mut engine.ledger
    }
}

impl Recorded for effort::Event {
    const WHAT: &'static str = "effort";

    fn slot(engine: &mut AlphaBeta) -> &mut Option<Sampler<Self>> {
        &mut engine.effort
    }
}

impl AlphaBeta {
    pub fn with_table_bytes(board: Board, bytes: usize) -> Self {
        Self::with_config(board, bytes, SearchConfig::default())
    }

    /// An engine searching under the policies given, with a table of the
    /// size given.
    pub fn with_config(board: Board, bytes: usize, config: SearchConfig) -> Self {
        Self::with_table(board, TranspositionTable::of_bytes(bytes), config)
    }

    /// The table itself rather than a size, which is the one thing a
    /// session's engine and a named size's engine differ in.
    fn with_table(board: Board, transpositions: TranspositionTable, config: SearchConfig) -> Self {
        Self {
            board,
            config,
            nodes: 0,
            transpositions,
            selective_depth: 0,
            limits: Limits::unlimited(),
            next_check: 0,
            stop: None,
            quiescence_nodes: 0,
            ordering: MoveOrdering::new(),
            caches: eval::Caches::default(),
            sampler: None,
            census: None,
            ledger: None,
            effort: None,
            effort_depths: effort::Depths::default(),
        }
    }

    /// Arm a reservoir: have the search record what it does at the nodes
    /// the reservoir's key picks. Off until this is called; the callers are
    /// the four recorders and the tests, and nothing the engine plays or
    /// benches with arms one.
    pub(crate) fn arm<T: Recorded>(&mut self, sampler: Sampler<T>) {
        *T::slot(self) = Some(sampler);
    }

    /// The reservoir back with everything it collected, leaving the engine
    /// recording nothing. None from an engine that was never armed. Handed
    /// back rather than emptied in place, so one reservoir can be carried
    /// across a run of searches with its cap describing the whole run.
    pub(crate) fn disarm<T: Recorded>(&mut self) -> Option<Sampler<T>> {
        T::slot(self).take()
    }

    /// What the node knew about a move at the gate, gathered for a
    /// ledger row: the staged half of a scouted event, and the whole of
    /// a skipped one.
    // cold and out of line behind a bare is_some, as `sample` is and for
    // `sample`'s measured reason.
    #[cold]
    #[inline(never)]
    fn staged_reduction(
        &self,
        m: &Play,
        searched: usize,
        node: &mut late_move::Node<'_>,
    ) -> reduction::Staged {
        reduction::Staged {
            play: *m,
            features: late_move::features(&self.deciding(), node, m, searched),
        }
    }

    /// The three references the late move decision reads the search
    /// through. Built at the call and nowhere held, so the borrow ends
    /// with the question and the move loop can hand the board straight on
    /// to `search_child`.
    fn deciding(&self) -> late_move::Search<'_> {
        late_move::Search {
            board: &self.board,
            ordering: &self.ordering,
            config: &self.config,
        }
    }

    /// A skipped move offered to the ledger: the third outcome, with no
    /// scout behind it. The search never makes a skipped move, so its
    /// legality is unknown at the decision; it is made and unmade around
    /// the record alone, and one that turns out illegal is not recorded,
    /// since the skip denied it nothing. The fen and the sampling key are
    /// the position the move leaves, as for a scouted move, so the replay
    /// reads a skipped row as it reads a low one. The move is not among
    /// the searched, so the row's searched count is its index and not one
    /// past it.
    #[cold]
    #[inline(never)]
    fn ledger_skip(&mut self, staged: reduction::Staged, depth: u8, alpha: Score, beta: Score) {
        // the fields are borrowed apart rather than through `self`, for
        // `ledger_event`'s reason
        let board = &mut self.board;
        let Some(ledger) = self.ledger.as_mut() else {
            return;
        };
        if !board.make_move(&staged.play) {
            return;
        }
        let key = reduction::sample_key(board.key, depth);
        ledger.event(key, || {
            let fen = board.to_fen();
            // the node's own eval, by stepping back and replaying: undo and
            // make are exact inverses, which the debug builds assert
            board.undo_move();
            let eval = i32::from(crate::eval::eval(board));
            assert!(
                board.make_move(&staged.play),
                "the skipped move was made once already"
            );
            reduction::Event {
                fen,
                depth,
                window: Window::of(alpha, beta),
                index: staged.features.index,
                searched: staged.features.index,
                generated: staged.features.generated,
                history: staged.features.history,
                history_max: staged.features.history_max,
                killer: staged.features.killer,
                tt: staged.features.tt,
                eval_beta: eval - i32::from(beta),
                alpha_gap: i32::from(alpha) - eval,
                alpha,
                scout: reduction::Scout::Skipped,
                cost: 0,
                reduction: 0,
            }
        });
        board.undo_move();
    }

    /// The scout's answer joining what the move loop staged: one reduced
    /// scout, offered to the ledger. The board here is the position the
    /// reduced move left, which is the fen the row carries; the eval is the
    /// reducing node's own, taken for kept events alone by stepping the
    /// staged move back and replaying it.
    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn ledger_event(
        &mut self,
        staged: reduction::Staged,
        depth: u8,
        reduction: u8,
        alpha: Score,
        beta: Score,
        scout: Score,
        entered_at: u64,
    ) {
        let cost = self.nodes - entered_at;
        let board = &mut self.board;
        let Some(ledger) = self.ledger.as_mut() else {
            return;
        };
        let key = reduction::sample_key(board.key, depth);
        ledger.event(key, || {
            let fen = board.to_fen();
            // undo and make are exact inverses, which the debug builds
            // assert
            board.undo_move();
            let eval = i32::from(crate::eval::eval(board));
            assert!(
                board.make_move(&staged.play),
                "the staged move was made once already"
            );
            reduction::Event {
                fen,
                depth,
                window: Window::of(alpha, beta),
                index: staged.features.index,
                searched: staged.features.index + 1,
                generated: staged.features.generated,
                history: staged.features.history,
                history_max: staged.features.history_max,
                killer: staged.features.killer,
                tt: staged.features.tt,
                eval_beta: eval - i32::from(beta),
                alpha_gap: i32::from(alpha) - eval,
                alpha,
                scout: if scout <= alpha {
                    reduction::Scout::Low
                } else {
                    reduction::Scout::High
                },
                cost,
                reduction,
            }
        });
    }

    /// One node answering out of the move loop, offered to the census:
    /// which move cut it off and what it cut ahead of, or the same portrait
    /// with no cutting move when the loop ran out.
    ///
    /// `cutting` is none for a held node. The killers and the history are
    /// read before `cutoff` teaches them the move, so a row says what the
    /// node knew when it chose. The evaluation is computed inside the
    /// closure, for kept events alone: exact rather than a cache read, and
    /// off the measured path.
    // cold and out of line behind a bare is_some at each call site, for
    // `sample`'s measured reason
    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn census_event(
        &mut self,
        depth: u8,
        alpha: Score,
        beta: Score,
        in_check: bool,
        moves: &[Play],
        searched: usize,
        quiets_scored: bool,
        ply: Option<usize>,
        tt: census::Table,
        entered_at: u64,
        cutting: Option<census::Cutting<'_>>,
    ) {
        let board = &self.board;
        let ordering = &self.ordering;
        let cost = self.nodes - entered_at;
        let Some(census) = self.census.as_mut() else {
            return;
        };
        let key = census::sample_key(board.key, depth);
        census.event(key, || {
            // the memories score quiet moves alone, so a capture or a
            // promotion reads 0 here and is priced by its class instead
            let quiet_history = |m: &Play| {
                if m.capture.is_none() && m.promote.is_none() {
                    Some(ordering.history_score(board.active_color, m))
                } else {
                    None
                }
            };
            let killers = ply.map_or([None, None], |ply| ordering.killers_at(ply));
            census::Event {
                fen: board.to_fen(),
                depth,
                window: Window::of(alpha, beta),
                in_check,
                generated: moves.len(),
                searched,
                cut: cutting.map(|cutting| census::Cut {
                    index: searched - 1,
                    class: census::Class::of(cutting.play, cutting.table, killers),
                    history: quiet_history(cutting.play).unwrap_or(0),
                    reduced: cutting.reduced,
                }),
                // clamped where the cutting move's own score is not, so a
                // negative history is read against nothing rather than
                // against another negative
                history_max: moves
                    .iter()
                    .filter_map(quiet_history)
                    .max()
                    .unwrap_or(0)
                    .max(0),
                quiets_scored,
                tt,
                eval_beta: i32::from(crate::eval::eval(board)) - i32::from(beta),
                cost,
            }
        });
    }

    /// What the effort instrument has counted by depth. Read after a
    /// search and before the next engine, since a run totals its positions.
    pub(crate) fn effort_tally(&self) -> &effort::Depths {
        &self.effort_depths
    }

    /// One node of the move loop answering, offered to the effort
    /// instrument's reservoir and counted in its per depth tally.
    ///
    /// The tally is bumped in front of the key test, where `Sampler::event`
    /// bumps `events`, so a rate rejection is still an event and the node
    /// counts a run reports do not move with `every`. The fen is built
    /// inside the closure, so an event the key turns away costs a hash and
    /// nothing else.
    // cold and out of line behind a bare is_some at each call site, for
    // `sample`'s measured reason
    #[cold]
    #[inline(never)]
    fn effort_event(&mut self, depth: u8, cut: bool, entered_at: u64) {
        let AlphaBeta {
            board,
            nodes,
            effort,
            effort_depths,
            ..
        } = self;
        let Some(effort) = effort.as_mut() else {
            return;
        };
        effort_depths.count(depth);
        let cost = *nodes - entered_at;
        let key = effort::sample_key(board.key, depth);
        effort.event(key, || effort::Event {
            key,
            fen: board.to_fen(),
            depth,
            cut,
            cost,
        });
    }

    /// One node a shortcut has just answered, or a shadow candidate it was
    /// measured against, offered to the sampler. The fen is built inside
    /// the closure, so an event the key turns away costs a hash and nothing
    /// else. The evaluation is passed in rather than taken again, so a row
    /// states the number the gate really read.
    // cold and out of line, behind a bare is_some at each call site:
    // inlining the sample body grew alpha_beta enough to move other code,
    // and the moved jump tables aliased in the branch predictor for half a
    // million extra mispredicts on a bench 5, counted with callgrind. The
    // option check is all the hot path keeps.
    #[cold]
    #[inline(never)]
    fn sample(
        &mut self,
        kind: Shortcut,
        depth: u8,
        claimed: Score,
        alpha: Score,
        beta: Score,
        eval: Score,
    ) {
        let board = &self.board;
        let Some(sampler) = self.sampler.as_mut() else {
            return;
        };
        // the guard stays here as well as at the call sites, so an engine
        // with no sampler pays for no hash whoever calls
        let key = crate::residual::sample_key(board.key, kind, depth);
        sampler.event(key, || Sample {
            fen: board.to_fen(),
            depth,
            kind,
            claimed,
            beta,
            eval_beta: i32::from(eval) - i32::from(beta),
            window: Window::of(alpha, beta),
            halfmove: board.halfmove_clock(),
        });
    }

    /// The score at this node, with the memoised terms read from the
    /// engine's caches. `&mut self` for the memos alone: the score is the
    /// one `eval::eval` gives.
    fn eval(&mut self) -> Score {
        crate::eval::eval_cached(&self.board, &mut self.caches)
    }

    /// The ply the quiet memories are indexed by at this node, or none when
    /// the configuration has them off or the ply is past the killer table.
    /// The rail stops every line inside the table, so the second test never
    /// fires in a search; it is what makes the index safe here rather than
    /// at every caller.
    fn memory_ply(&self) -> Option<usize> {
        if !self.config.move_memory {
            return None;
        }
        let ply = self.board.line_ply;
        if ply >= MAX_PLY as usize {
            return None;
        }
        Some(ply)
    }

    /// The move that cut this node off and the moves the node searched
    /// before it, offered to the quiet memories. Called with the move
    /// unmade, so the board says which side played it and the node's ply.
    fn remember_cutoff<'a>(
        &mut self,
        m: &Play,
        tried: impl IntoIterator<Item = &'a Play>,
        depth: u8,
    ) {
        if let Some(ply) = self.memory_ply() {
            self.ordering
                .cutoff(self.board.active_color, m, tried, ply, depth);
        }
    }

    /// Whether a result may be stored under the taint policy. A tainted
    /// one that may not be is counted as skipped, so the policy's cost is
    /// a figure rather than an absence.
    fn keeps(&mut self, value: Value) -> bool {
        if value.tainted && !self.config.taint.stores_tainted() {
            self.transpositions.count_skipped_store();
            return false;
        }
        true
    }

    pub fn clear_transpositions(&mut self) {
        self.transpositions.clear();
    }

    /// The bytes the transposition table occupies. Whole buckets, so a size
    /// that does not divide by one reads back as the next size up.
    pub fn table_bytes(&self) -> usize {
        self.transpositions.bytes()
    }

    /// The policies this engine searches under. Asked by the residuals
    /// replay's tests, which have to say the search answering the sampled
    /// positions is the reference and not the default.
    pub fn config(&self) -> SearchConfig {
        self.config
    }

    /// How much of the search's use of the transposition table depended on
    /// the path taken rather than on the position: see the graph history
    /// notes on the counters.
    pub fn ghi(&self) -> GhiCounters {
        self.transpositions.ghi()
    }

    /// Have the table keep the full key of every entry, so what its thirty
    /// two bit signature costs can be counted: see
    /// `TranspositionTable::audit_signatures`. The search is the same
    /// either way, and the table starts empty. False if there was not the
    /// memory for the keys, in which case nothing is counted.
    #[must_use]
    pub fn audit_signatures(&mut self) -> bool {
        self.transpositions.audit_signatures()
    }

    /// What the signature audit counted, or none when the table was never
    /// asked to keep the keys.
    pub fn signatures(&self) -> Option<SignatureCounters> {
        self.transpositions.signatures()
    }

    /// How many of the nodes visited so far were quiescence's, over every
    /// search this engine has run.
    pub fn quiescence_nodes(&self) -> u64 {
        self.quiescence_nodes
    }

    /// Cooperative limit check. The limits say when to look at them again:
    /// every few thousand nodes for the clock, and the node budget itself,
    /// exactly. The count is incremented after this is asked, so a budget
    /// of n is n nodes visited.
    fn poll_deadline(&mut self) -> Result<(), Aborted> {
        if self.nodes < self.next_check {
            return Ok(());
        }
        self.check_limits()
    }

    /// The slow half of `poll_deadline`: reading the clock and arming the
    /// next check happen once in thousands of nodes, and inlined at every
    /// poll they only made the hot loop larger.
    #[cold]
    #[inline(never)]
    fn check_limits(&mut self) -> Result<(), Aborted> {
        if self.limits.expired(self.nodes) {
            return Err(Aborted);
        }
        // relaxed: the flag is the whole of what the two threads share, so
        // there is nothing to order it against, and a few thousand nodes
        // of latency is well under what an interface can notice
        if self
            .stop
            .as_ref()
            .is_some_and(|stop| stop.load(Ordering::Relaxed))
        {
            return Err(Aborted);
        }
        self.next_check = self.limits.next_check_after(self.nodes);
        Ok(())
    }

    fn result_for(&self, best_move: Play, score: Score) -> SearchResult {
        SearchResult {
            nodes: self.nodes,
            elapsed: self.limits.elapsed(),
            score,
            selective_depth: self.selective_depth,
            best_move,
        }
    }

    /// What a capture search makes of the position this engine holds, over
    /// the open window and under no limits, so what comes back is a value
    /// and the search cannot be interrupted. Whether the shortcuts inside
    /// it are on is the engine's configuration.
    ///
    /// A door for the laboratory, not the search: the tuner's quiet test
    /// keeps a position when this comes back at the static evaluation.
    /// `quiescence` itself stays private, because a caller free to choose
    /// the window could be handed a bound and read it as a value.
    pub(crate) fn quiescence_value(&mut self) -> Score {
        self.limits = Limits::unlimited();
        self.stop = None;
        self.next_check = 0;
        self.nodes = 0;
        self.board.start_line();
        match self.quiescence(Score::MIN + 1, Score::MAX - 1) {
            Ok(value) => value.score,
            Err(Aborted) => unreachable!("an unlimited capture search runs to the end"),
        }
    }

    fn quiescence(&mut self, mut alpha: Score, beta: Score) -> Result<Value, Aborted> {
        // no repetition check: a capture cannot repeat a position and the
        // only quiet moves here are evasions, so a cycle needs a line of
        // nothing but mutual quiet checks, which the rail bounds
        self.selective_depth = self.selective_depth.max(self.board.line_ply as u8);
        if self.board.line_ply >= MAX_PLY.into() {
            return Ok(Value::clean(self.eval()));
        }

        self.poll_deadline()?;
        self.nodes += 1;
        self.quiescence_nodes += 1;

        // standing pat is declining to move, which a side in check cannot,
        // so its static eval is no floor. The full search never enters here
        // in check (the extension searches those nodes full width), so a
        // check seen here was delivered by a capture searched here.
        // fail soft: what leaves is the best score seen, not the window edge
        let mut best = Score::MIN + 1;
        let in_check = self.board.in_check();
        // none in check, which is what exempts evasions from the margin below
        let standing = if in_check { None } else { Some(self.eval()) };
        if let Some(score) = standing {
            if score >= beta {
                return Ok(Value::clean(score));
            }
            best = score;
            if score >= alpha {
                alpha = score;
            }
        }

        let mut best_move: Option<Play> = None;
        let old_alpha = alpha;
        // a probe at depth zero: any stored bound is deep enough here
        let pv_play = match self.transpositions.probe(
            &self.board,
            alpha,
            beta,
            0,
            self.config.taint.refuses_tainted_cutoffs(),
            self.config.taint.guards_rule50(),
        ) {
            Probe::Cut(value) => return Ok(value),
            Probe::Order(play) => Some(play),
            Probe::Miss => None,
        };
        // in check every evasion is searched, quiet or not
        let mut moves = if in_check {
            self.board.evasions()
        } else {
            self.board.generate_captures()
        };
        // no memories here: they say nothing about captures or evasions.
        // `front` is the table's move and the captures the swap prices as
        // winning or even; every capture behind it is a losing one. Read
        // now, because the sort's keys do not survive the recursion below
        let front = self.ordering.order(&self.board, &mut moves, pv_play, None);

        // quiescence never reads a draw itself, but a search trusting
        // tainted scores can cut on one inside a capture tree
        let mut taint = Taint::default();
        let mut found_legal_move = false;
        for (i, m) in moves.iter().enumerate() {
            // two skips the reference does not make, under one set of
            // exemptions. A promotion is exempt because the swap prices the
            // arriving piece as the pawn that left. An evasion is exempt
            // because a side in check has no standing eval to measure from.
            // A mate window alpha is exempt because a static eval cannot
            // come near it, so the margin's arithmetic would skip every
            // capture, the mating one included; the stand pat lifts alpha
            // to the eval, so alpha is inside the window only when it
            // arrived there positive, a mate already in hand. A sacrifice
            // that would find a first mate, with alpha nowhere near one, is
            // skipped like any other losing capture
            if let (Some(standing), Some(captured)) = (standing, m.capture) {
                if !is_mate(alpha) && m.promote.is_none() {
                    // a capture short of alpha with its piece counted as
                    // fully won is expected to be worth less than alpha
                    if self.config.delta_margin
                        && standing + crate::eval::material(captured) as Score + DELTA_MARGIN
                            < alpha
                    {
                        continue;
                    }
                    // every capture behind the front is one the swap priced
                    // as losing, so the class is read off the order rather
                    // than from a second swap
                    if self.config.see_pruning && i >= front {
                        continue;
                    }
                }
            }
            if self.board.make_move(m) {
                found_legal_move = true;
                // undo before an abort can propagate, or the board would keep
                // the aborted line
                let result = self.quiescence(-beta, -alpha);
                self.board.undo_move();
                let value = -result?;
                taint.absorb(value);
                let score = value.score;
                if score > best {
                    best = score;
                    best_move = Some(*m);
                }
                if score > alpha {
                    if score >= beta {
                        let value = taint.stamp(score);
                        if self.keeps(value) {
                            self.transpositions.record_cutoff(&self.board, *m, value, 0);
                        }
                        return Ok(value);
                    }
                    alpha = score;
                }
            }
        }

        if in_check && !found_legal_move {
            // checkmate at the end of a capture sequence, scored as the full
            // search scores one
            return Ok(Value::mated(self.board.line_ply));
        }

        let value = taint.stamp(best);
        if let Some(play) = best_move {
            if self.keeps(value) {
                if alpha != old_alpha {
                    self.transpositions.record_best(&self.board, play, value, 0);
                } else {
                    self.transpositions
                        .record_ceiling(&self.board, play, value, 0);
                }
            }
        }
        Ok(value)
    }

    /// What can answer a full width node before a move of it is searched:
    /// the margin, then the pass. Each claims the position already stands
    /// above beta, the margin from the static eval alone and the pass from
    /// a reduced search, which is dearer and reaches where a margin cannot.
    /// Nothing is stored on either: an entry names the play it was reached
    /// by, and no move was searched here.
    ///
    /// Both rest on quiescence's standing pat: the side to move need not
    /// make things worse, so its static eval is a floor. The three shared
    /// gates are where that floor gives way. A side in check cannot decline
    /// to move. A side down to pawns and a king is the material zugzwang
    /// happens to. And a beta inside the mate window is cleared by every
    /// eval, so a cutoff against one would leave a faster mate unsearched;
    /// the positive half of that gate is redundant while material bounds
    /// the eval, and stands in case the eval grows terms that reach higher.
    /// A fourth gate stands beside them: a beta that is still the root's
    /// own bound, which `root_bounds` says. Nothing has claimed that
    /// bound, so there is nothing here for the node to stand above.
    ///
    /// A `Some` answers the node. A pass that failed answers nothing but
    /// leaves whatever it read in the node's taint.
    ///
    /// `eval` is filled wherever the gates passed and an evaluation was
    /// read, fired or not, so the move loop can seed the memo the late move
    /// decision reads and a node this answered nothing at evaluates once
    /// rather than twice. It is the same score `eval::eval` gives, through
    /// the memoised door.
    ///
    /// Alpha is read by neither shortcut, and nor is its bit. Alpha is
    /// here for the sampler, which records the window the node was asked
    /// under.
    // two arguments past clippy's limit: the root bounds and the evaluation
    // handed back to the loop.
    #[allow(clippy::too_many_arguments)]
    fn shortcuts(
        &mut self,
        alpha: Score,
        beta: Score,
        depth: u8,
        in_check: bool,
        can_null: bool,
        root_bounds: RootBounds,
        taint: &mut Taint,
        eval_memo: &mut Option<i64>,
    ) -> Result<Option<Value>, Aborted> {
        let margin = self.config.reverse_futility && depth <= REVERSE_FUTILITY_MAX_DEPTH;
        // no pass directly under a pass, or the search would answer a
        // position from a line neither side moved in. Unreachable while the
        // eval gate below stands (a node passes only with its eval at or
        // above beta, and the window and the eval turn round under it) and
        // kept for the day that gate is dropped or given a margin
        let pass = self.config.null_move && can_null && depth >= NULL_MOVE_MIN_DEPTH;
        if (!margin && !pass)
            || in_check
            || !self.board.has_non_pawn_material()
            || is_mate(beta)
            || root_bounds.beta
        {
            return Ok(None);
        }
        let eval = self.eval();
        *eval_memo = Some(i64::from(eval));

        // what the margin proves is a lower bound, `eval - margin`, and fail
        // soft returns that rather than beta or the whole eval, which
        // nothing here argues for. Clean: a static eval consulted no path
        if margin {
            let floor = eval.saturating_sub(REVERSE_FUTILITY_MARGIN * depth as Score);
            // the shadow row: every candidate the margin test reads, fired
            // or not, since the fired rows alone say nothing about the
            // region a tighter margin would newly fire on
            if self.sampler.is_some() && eval >= beta {
                self.sample(Shortcut::ShadowFutility, depth, floor, alpha, beta, eval);
            }
            if floor >= beta {
                if self.sampler.is_some() {
                    self.sample(Shortcut::ReverseFutility, depth, floor, alpha, beta, eval);
                }
                return Ok(Some(Value::clean(floor)));
            }
        }

        // the pass spends a reduced search, so it is asked for only with
        // the eval already above beta, and over a zero window: the question
        // is only whether a pass beats beta
        if pass && eval >= beta {
            let reduction = null_move_reduction(self.config, depth, eval - beta);
            self.board.make_null_move();
            let result = self.alpha_beta(
                -beta,
                -beta + 1,
                depth - 1 - reduction,
                false,
                root_bounds.child(ChildSearch::Pass),
            );
            // undo before an abort can propagate, or the board would keep
            // the passed line
            self.board.undo_null_move();
            let value = -result?;
            if value.score >= beta {
                // a mate found through a pass is not a mate: the pass is
                // not a move either side has, so what was proved is that
                // the position is very good, and the score is held under
                // the window a caller reads mates in
                let score = below_the_mate_window(value.score);
                // the board is back from the pass, so the fen the sampler
                // prints is the node itself
                if self.sampler.is_some() {
                    self.sample(Shortcut::NullMove, depth, score, alpha, beta, eval);
                }
                return Ok(Some(Value::with_taint(score, value.tainted)));
            }
            // a pass that failed still read whatever it read on the way
            taint.absorb(value);
        }
        Ok(None)
    }

    /// One child of a full width node, or nothing when the move is not
    /// legal here. The undo comes before the abort can propagate, or the
    /// board would keep the aborted line; propagating is what keeps an
    /// aborted frame's meaningless score away from every store above. Every
    /// child of the full width search enters through here, so the window
    /// discipline in `windowed` is written once; `reduction` is how many
    /// plies shallower the scout runs, zero for no scout, and `staged` is
    /// what the ledger has about the move, or nothing.
    // two arguments past clippy's limit: the staging travelling as a
    // parameter rather than as a field of the engine, and the root bounds.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn search_child(
        &mut self,
        m: &Play,
        alpha: Score,
        beta: Score,
        depth: u8,
        first: bool,
        reduction: u8,
        root_bounds: RootBounds,
        staged: Option<&reduction::Staged>,
    ) -> Result<Option<Value>, Aborted> {
        if !self.board.make_move(m) {
            return Ok(None);
        }
        let result = self.windowed(alpha, beta, depth, first, reduction, root_bounds, staged);
        self.board.undo_move();
        Ok(Some(result?))
    }

    /// Principal variation search: the recursion behind one made move. The
    /// node's first move is searched with the window as it stands. Every
    /// later move is probed with a zero width search first, which can only
    /// say whether it beats alpha; one that does, at a node whose window is
    /// wider than the zero one, is searched again with the full window. A
    /// zero width node never asks twice.
    ///
    /// A reduced move is scouted before any of that: the same zero width
    /// search, `reduction` plies shallower. A scout that fails low answers
    /// for the move, which is the late move reduction's guess; one that
    /// fails high goes on to the probe and the proof as an unreduced move
    /// does.
    ///
    /// The root bounds travel with the windows, so each of the three
    /// searches asks `RootBounds::child` what it carries rather than
    /// saying so here.
    ///
    /// A body of its own rather than `search_child`'s so that an abort from
    /// any pass runs through the one undo there.
    #[allow(clippy::too_many_arguments)]
    fn windowed(
        &mut self,
        alpha: Score,
        beta: Score,
        depth: u8,
        first: bool,
        reduction: u8,
        root_bounds: RootBounds,
        staged: Option<&reduction::Staged>,
    ) -> Result<Value, Aborted> {
        if first {
            debug_assert!(reduction == 0, "a node's first move is never reduced");
            return Ok(-self.alpha_beta(
                -beta,
                -alpha,
                depth - 1,
                true,
                root_bounds.child(ChildSearch::FirstMove),
            )?);
        }
        let mut tainted = false;
        if reduction > 0 {
            // the floors on the two reductions keep a full width ply under
            // the scout, so the subtraction below never wraps
            debug_assert!(depth > reduction + 1, "the scout would be quiescence");
            // what the scout's cost is measured from: a read of a field,
            // no branch, so the disarmed search is unchanged
            let entered_at = self.nodes;
            let scout = -self.alpha_beta(
                -alpha - 1,
                -alpha,
                depth - 1 - reduction,
                true,
                root_bounds.child(ChildSearch::Scout),
            )?;
            if let Some(staged) = staged {
                self.ledger_event(
                    *staged,
                    depth,
                    reduction,
                    alpha,
                    beta,
                    scout.score,
                    entered_at,
                );
            }
            if scout.score <= alpha {
                return Ok(scout);
            }
            // the scout's fail high asked for the full depth, so whatever
            // it depended on, the passes below do too
            tainted = scout.tainted;
        }
        let probe = -self.alpha_beta(
            -alpha - 1,
            -alpha,
            depth - 1,
            true,
            root_bounds.child(ChildSearch::Probe),
        )?;
        // `alpha + 1 >= beta` is the zero window, spelt without the
        // subtraction: `beta - alpha` overflows a Score at the full window,
        // which is what the root opens at below the aspiration depth and
        // wherever its window has widened to the edge
        if probe.score <= alpha || alpha + 1 >= beta {
            return Ok(Value::with_taint(probe.score, probe.tainted || tainted));
        }
        let proof = -self.alpha_beta(
            -beta,
            -alpha,
            depth - 1,
            true,
            root_bounds.child(ChildSearch::Proof),
        )?;
        // the probe's fail high asked for the proof, so the proof depends
        // on whatever the probe did
        Ok(Value::with_taint(
            proof.score,
            proof.tainted || probe.tainted || tainted,
        ))
    }

    /// A fail high at a full width node: the move that proved it goes to
    /// the quiet memories with the moves the node searched before it and,
    /// when the taint policy allows, to the table. The one place a full
    /// width cutoff is acted on.
    ///
    /// `tried` is the moves the node searched, not the whole list: what the
    /// history marks down is what the node asked and got nothing from. The
    /// captures among them are the memories' to pass over.
    fn cutoff<'a>(
        &mut self,
        m: &Play,
        tried: impl IntoIterator<Item = &'a Play>,
        taint: Taint,
        score: Score,
        depth: u8,
    ) -> Value {
        self.remember_cutoff(m, tried, depth);
        let value = taint.stamp(score);
        if self.keeps(value) {
            self.transpositions
                .record_cutoff(&self.board, *m, value, depth);
        }
        value
    }

    /// One full width node. `can_null` is false only directly under a
    /// pass: a position neither side has moved in is not one a reduced
    /// search says anything about. A parameter rather than a field toggled
    /// around the call, so reading the recursion says which nodes may pass.
    ///
    /// `root_bounds` says which of the two bounds handed in is still the
    /// root's own, which is what stands the shortcuts and the reduction
    /// down on the principal variation. Carried the same way and for the
    /// same reason: a register per node rather than a field written and
    /// read back around every child.
    fn alpha_beta(
        &mut self,
        mut alpha: Score,
        beta: Score,
        mut depth: u8,
        can_null: bool,
        mut root_bounds: RootBounds,
    ) -> Result<Value, Aborted> {
        self.poll_deadline()?;
        self.selective_depth = self.selective_depth.max(self.board.line_ply as u8);
        self.nodes += 1;
        // what the census's cost column is measured from; read
        // unconditionally so the disarmed search is unchanged
        let entered_at = self.nodes;

        // every node here sits below the root, so a repetition is a draw
        // either side can take; at the root the engine still has to move
        let in_check = self.board.in_check();
        if self.board.fifty_move_expired() {
            // a mate delivered by the hundredth half move is a mate: the
            // game ends on it before the side mated can claim the draw. Not
            // asked of a repetition, which cannot be a mate (the position
            // would have ended the game the first time it came up)
            if in_check && !self.board.has_legal_move() {
                return Ok(Value::mated(self.board.line_ply));
            }
            // where the taint starts: the draw is true of the path that
            // reached this position, not of the position itself
            return Ok(Value::tainted(0));
        }
        if self.board.has_repeated() {
            return Ok(Value::tainted(0));
        }
        // the check extension holds a node's depth where it was, and a line
        // of checks that keeps capturing and never repeats is ended by
        // neither draw rule, so the rail ends it here, before the ring a
        // repetition is read from and the ply a mate score stops being told
        // from an eval by. The answer is quiescence's at its own rail: a
        // static eval, clean because it consulted no path. It gives up the
        // mate a node standing here may be in
        if self.board.line_ply >= MAX_PLY as usize {
            return Ok(Value::clean(self.eval()));
        }
        let mut taint = Taint::default();
        if in_check {
            depth += 1;
        }

        if depth == 0 {
            return self.quiescence(alpha, beta);
        }

        let old_alpha = alpha;
        let mut found_legal_move = false;
        let mut best_move: Option<Play> = None;
        // fail soft, as in quiescence: a cutoff stores the best score seen,
        // a floor at least as tight as beta
        let mut best = Score::MIN + 1;
        let pv_play = match self.transpositions.probe(
            &self.board,
            alpha,
            beta,
            depth,
            self.config.taint.refuses_tainted_cutoffs(),
            self.config.taint.guards_rule50(),
        ) {
            Probe::Cut(value) => return Ok(value),
            Probe::Order(play) => Some(play),
            Probe::Miss => None,
        };

        // the node's static evaluation, read by the shortcuts and seeded
        // into the late move decision's memo below rather than read a
        // second time there. Declared here so the shortcuts can fill it
        let mut eval: Option<i64> = None;
        if let Some(value) = self.shortcuts(
            alpha,
            beta,
            depth,
            in_check,
            can_null,
            root_bounds,
            &mut taint,
            &mut eval,
        )? {
            return Ok(value);
        }

        // the table's move sorts ahead of everything else, and when there
        // is one it takes the cutoff nine times in ten, so it is searched
        // before the rest are generated: the nodes it cuts never generate
        // or sort at all, and the tree searched is unchanged
        let mut tt_tried: Option<Play> = None;
        if let Some(tt) = pv_play {
            if self.board.is_pseudo_legal(&tt) {
                tt_tried = Some(tt);
                if let Some(value) =
                    self.search_child(&tt, alpha, beta, depth, true, 0, root_bounds, None)?
                {
                    found_legal_move = true;
                    taint.absorb(value);
                    let tt_score = value.score;
                    if tt_score > best {
                        best = tt_score;
                        best_move = Some(tt);
                    }
                    if tt_score > alpha {
                        if tt_score >= beta {
                            // before the cutoff teaches the memories; the
                            // list is empty because nothing was generated
                            if self.census.is_some() {
                                self.census_event(
                                    depth,
                                    alpha,
                                    beta,
                                    in_check,
                                    &[],
                                    1,
                                    false,
                                    None,
                                    census::Table::Move,
                                    entered_at,
                                    Some(census::Cutting {
                                        play: &tt,
                                        reduced: false,
                                        table: true,
                                    }),
                                );
                            }
                            if self.effort.is_some() {
                                self.effort_event(depth, true, entered_at);
                            }
                            // the table's move earns its killer slot as any
                            // other cutting move does; nothing was searched
                            // before it, so there is nothing to mark down
                            return Ok(self.cutoff(&tt, &[], taint, tt_score, depth));
                        }
                        alpha = tt_score;
                        // a score a search returned, so alpha is no longer
                        // the bound the node arrived with
                        root_bounds = root_bounds.alpha_raised();
                    }
                }
            }
        }

        // read before the loop moves `found_legal_move` past it: the
        // table's move's bit in the list below stands on this
        let tt_searched = found_legal_move;

        let mut moves = if in_check {
            self.board.evasions()
        } else {
            self.board.generate_moves()
        };
        let ply = self.memory_ply();
        let front = self.ordering.order(&self.board, &mut moves, pv_play, ply);

        // how many moves the node has searched, which is what makes a quiet
        // move late; the table's move, when searched, is the first
        let mut searched = usize::from(found_legal_move);
        // which places the node made and searched, the history's malus
        // under a cutoff. A move either skipping rule passed over, the
        // model's at depth four and up or a shallow rule's below it, has
        // no bit, and nor has one that turned out illegal
        let mut made = Searched::default();
        // whether the second stage ran here, read by the census
        let mut quiets_scored = false;
        // the two dear features, computed by the first move that needs
        // them and read back for the rest; locals rather than fields of
        // the node facts below, which are built afresh for each move. The
        // evaluation is seeded above by whatever the shortcuts read
        let mut history_max: Option<i32> = None;
        // what the node's table probe gave it, settled here: the probe and
        // the table's move are behind it
        let tt = census::Table::of(pv_play.is_some(), tt_tried.is_some());
        // the two shallow rules' node half, settled once here. Every part
        // of them but the margin's own test is the node's rather than the
        // move's, and that test is a latch: alpha only rises, so it is
        // false until it becomes true and then stays true
        let mut shallow = late_move::shallow(
            &self.config,
            &self.board,
            depth,
            in_check,
            beta,
            root_bounds,
        );
        for i in 0..moves.len() {
            // the front did not cut this node off, so the rest of the list
            // is scored and sorted before the first move past it is tried
            if i == front {
                if let Some(ply) = ply {
                    self.ordering
                        .order_quiets(&self.board, &mut moves[front..], ply);
                    quiets_scored = true;
                }
            }
            let m = &moves[i];
            if tt_tried == Some(*m) {
                // searched before the list existed; this is where its place
                // in the list is known
                if tt_searched {
                    made.mark(i);
                }
                continue;
            }
            // the shallow rules, asked before the node facts below because
            // they read none of them: they reach depths the reduction does
            // not, so facts built for them would be facts built at most of
            // the interior of the tree
            if shallow.skips(&self.deciding(), &mut eval, m, searched, alpha) {
                // never made, so whether it was even legal is never
                // learned; skipping an illegal move is a no-op, since the
                // loop would have passed over it anyway. `searched` stands
                // where it was and nothing is taught about the move
                if self.ledger.is_some() {
                    let mut node = late_move::Node {
                        depth,
                        alpha,
                        beta,
                        root_bounds,
                        in_check,
                        ply,
                        tt,
                        moves: &moves,
                        eval: &mut eval,
                        history_max: &mut history_max,
                    };
                    let staged = self.staged_reduction(m, searched, &mut node);
                    self.ledger_skip(staged, depth, alpha, beta);
                }
                continue;
            }
            // the node's facts as this move is decided, built here rather
            // than once for the node because alpha rises as the node
            // searches and the list is sorted under the loop above, and
            // only where the node admits a reduction at all: most moves
            // are searched whole, and writing the facts out for each of
            // them measured a percent of the run. The ledger's staged half
            // is carried to the scout as a parameter so the reduced moves
            // inside it cannot mistake it for their own
            let (reduction, staged) = if late_move::admits(
                &self.config,
                depth,
                searched,
                in_check,
                alpha,
                beta,
                root_bounds,
            ) {
                let mut node = late_move::Node {
                    depth,
                    alpha,
                    beta,
                    root_bounds,
                    in_check,
                    ply,
                    tt,
                    moves: &moves,
                    eval: &mut eval,
                    history_max: &mut history_max,
                };
                match late_move::decide(&self.deciding(), &mut node, m, searched) {
                    late_move::Verdict::Skip => {
                        // never made, as above
                        if self.ledger.is_some() {
                            let staged = self.staged_reduction(m, searched, &mut node);
                            self.ledger_skip(staged, depth, alpha, beta);
                        }
                        continue;
                    }
                    late_move::Verdict::Scout(reduction) => {
                        let staged = if reduction > 0 && self.ledger.is_some() {
                            Some(self.staged_reduction(m, searched, &mut node))
                        } else {
                            None
                        };
                        (reduction, staged)
                    }
                }
            } else {
                (0, None)
            };
            // read by the census's row
            let reduced = reduction > 0;
            let Some(value) = self.search_child(
                m,
                alpha,
                beta,
                depth,
                !found_legal_move,
                reduction,
                root_bounds,
                staged.as_ref(),
            )?
            else {
                continue;
            };
            found_legal_move = true;
            made.mark(i);
            searched += 1;
            debug_assert_eq!(
                made.count(),
                searched,
                "a bit for every move made and searched, and for no other"
            );
            taint.absorb(value);
            let score = value.score;
            if score > best {
                best = score;
                best_move = Some(*m);
            }
            if score > alpha {
                if score >= beta {
                    // before the cutoff teaches the memories, so the row
                    // reads what the node chose under
                    if self.census.is_some() {
                        self.census_event(
                            depth,
                            old_alpha,
                            beta,
                            in_check,
                            &moves,
                            searched,
                            quiets_scored,
                            ply,
                            tt,
                            entered_at,
                            Some(census::Cutting {
                                play: m,
                                reduced,
                                table: false,
                            }),
                        );
                    }
                    if self.effort.is_some() {
                        self.effort_event(depth, true, entered_at);
                    }
                    // the moves searched before the one that answered,
                    // captures and all, which the memories pass over
                    let tried = moves[..i]
                        .iter()
                        .enumerate()
                        .filter(|(place, _)| made.holds(*place))
                        .map(|(_, tried)| tried);
                    return Ok(self.cutoff(m, tried, taint, score, depth));
                }
                alpha = score;
                // as at the table's move above
                root_bounds = root_bounds.alpha_raised();
            }
        }

        // the held half of the census, recorded at the same rate: a
        // cut-only stream would reproduce the censoring the census measures
        if self.census.is_some() {
            self.census_event(
                depth,
                old_alpha,
                beta,
                in_check,
                &moves,
                searched,
                quiets_scored,
                ply,
                tt,
                entered_at,
                None,
            );
        }
        // the held half, at the same rate as the two cut ones above: a rule
        // moves effort between held nodes and cut ones as well as away from
        // both
        if self.effort.is_some() {
            self.effort_event(depth, false, entered_at);
        }

        if !found_legal_move {
            // mate and stalemate are properties of the position, not of the
            // path that reached it
            if in_check {
                return Ok(Value::mated(self.board.line_ply));
            }
            return Ok(Value::clean(0));
        }

        let play = best_move.expect("a legal move was found, so one of them is best");
        let value = taint.stamp(best);
        if self.keeps(value) {
            if alpha != old_alpha {
                self.transpositions
                    .record_best(&self.board, play, value, depth);
            } else {
                self.transpositions
                    .record_ceiling(&self.board, play, value, depth);
            }
        }
        Ok(value)
    }

    /// The engine a session starts with, and the size its table was asked
    /// for when the host would not give it. A machine with less memory
    /// than the default assumes plays with a smaller table rather than
    /// failing to start, and `table_bytes` says what it got.
    ///
    /// The ask is handed back rather than left to be inferred, so that
    /// what the adapter reports and what the engine asked for are the one
    /// fact.
    pub fn new(board: Board) -> (Self, Option<usize>) {
        let (transpositions, asked) = TranspositionTable::up_to_bytes(DEFAULT_TABLE_BYTES);
        (
            Self::with_table(board, transpositions, SearchConfig::default()),
            asked,
        )
    }

    /// One fixed depth search of the root, the one node whose answer must
    /// include a play. The root probes the table to order moves and stores
    /// its entry when done, but never takes a stored score in place of
    /// searching: a stored score can come from a line whose repetition and
    /// fifty move context differ from the game being played.
    pub fn search(&mut self, depth: u8) -> SearchOutcome {
        self.transpositions.new_search();
        self.ordering.forget();
        self.search_within(depth, Limits::unlimited())
    }

    /// One fixed depth search under the limits given. The deepening loop
    /// hands each iteration its own, which is how depth one runs with none.
    ///
    /// The caller owns the table's generation and the quiet memories.
    /// `search` and the deepening loop each start a generation with
    /// `new_search`, so what their iterations store ages together from the
    /// next search; a caller that skips that leaves every entry looking
    /// current, so nothing goes stale and the oldest entries are never the
    /// ones given up. A search through here keeps whatever the memories
    /// learned before it.
    pub fn search_within(&mut self, depth: u8, limits: Limits) -> SearchOutcome {
        self.search_root(depth, limits, None, Aspiration::open(None, depth))
    }

    /// The body of one fixed depth search, under the window `window`
    /// opens. Everything that may interrupt it arrives in the signature,
    /// and the prologue, not the caller, writes the fields the poll reads.
    ///
    /// Fail soft, and the answer says which of three things its score is.
    /// A score inside the window is the position's worth and the move
    /// beside it is the best of them. A move that reached beta makes the
    /// score a floor: the rest of the moves were never tried. A window no
    /// move reached alpha in makes it a ceiling, and the move beside it is
    /// only the one that came closest, which proves nothing and is never
    /// answered with. At the full window the last of the three cannot
    /// happen: every score beats an alpha at the end of the score type.
    fn search_root(
        &mut self,
        mut depth: u8,
        limits: Limits,
        stop: Option<Arc<AtomicBool>>,
        window: Aspiration,
    ) -> SearchOutcome {
        // held to the rail here and not only at the interface, so a library
        // caller cannot ask the check extension below to overflow
        depth = depth.min(MAX_PLY);
        self.limits = limits;
        self.stop = stop;
        self.next_check = 0;
        self.nodes = 0;
        self.selective_depth = depth;
        self.board.start_line();

        if self.poll_deadline().is_err() {
            return SearchOutcome::Aborted(None);
        }
        self.nodes += 1;

        if self.board.in_check() {
            depth += 1;
        }

        let opening_alpha = window.alpha;
        let beta = window.beta;
        let mut alpha = opening_alpha;
        // where the exemption starts: the root opened with both of these
        // and proved neither, and they stay marked down the leftmost line.
        // Nothing here reads how wide they are, which is what lets the
        // window narrow without the exemption moving
        let mut root_bounds = RootBounds::BOTH;
        // the best any move scored and the move that scored it, which is
        // the fail soft answer whether or not anything reached alpha
        let mut top: Option<(Play, Score)> = None;
        let mut found_legal_move = false;
        let mut taint = Taint::default();

        // the entry here is the one the last iteration answered with, stored
        // past the depth contest, and `order` puts the table's move first.
        // So a deepening search tries the previous depth's best first,
        // which is what lets an aborted iteration's best replace it: see
        // the Aborted arm of iterative_deepening_search
        let pv_play = self.transpositions.ordering_play(&self.board);
        let mut moves = self.board.generate_moves();
        // no memories at the root: the swap reasons about this order
        self.ordering.order(&self.board, &mut moves, pv_play, None);
        // the swap's soundness rests on that ordering, so a change that
        // breaks it (a root bonus outbidding the table move, say) fails
        // here rather than answering with a move never compared to the old
        if let Some(previous) = pv_play {
            debug_assert!(
                moves
                    .iter()
                    .position(|m| *m == previous)
                    .is_none_or(|at| at == 0),
                "the table's move is no longer first at the root"
            );
        }

        // the root reduces nothing: it has one window to answer under and
        // its moves are few enough to search whole
        for m in &moves {
            match self.search_child(
                m,
                alpha,
                beta,
                depth,
                !found_legal_move,
                0,
                root_bounds,
                None,
            ) {
                Err(Aborted) => {
                    // a move that beat the opening alpha is a floor under
                    // the position and may be answered with. The closest
                    // move of a window nothing reached is not: it was
                    // never shown better than anything
                    let answerable = (alpha != opening_alpha).then_some(top).flatten();
                    return SearchOutcome::Aborted(
                        answerable.map(|(play, score)| self.result_for(play, score)),
                    );
                }
                Ok(None) => {}
                Ok(Some(value)) => {
                    found_legal_move = true;
                    taint.absorb(value);
                    let score = value.score;
                    if top.is_none_or(|(_, best)| score > best) {
                        top = Some((*m, score));
                    }
                    if score > alpha {
                        alpha = score;
                        // the root's lower bound is a score from here on
                        root_bounds = root_bounds.alpha_raised();
                    }
                    if score >= beta {
                        // the window asked whether anything here is worth
                        // beta and this move answers it. What the rest are
                        // worth is a question the wider re-search asks
                        break;
                    }
                }
            }
        }

        if !found_legal_move {
            // checkmate or stalemate, the only way out of here without a
            // move. An expired fifty move counter is not another: that draw
            // is claimable and not automatic (FIDE 9.3), so the side to
            // move may still play, and the tree below scores the draw at
            // every child the move does not reset
            return SearchOutcome::GameOver;
        }

        let (play, score) = top.expect("a legal move was found, so one of them scored best");
        let value = taint.stamp(score);
        // what the root leaves for the re-search and for the next
        // iteration to order by. The answer and the floor are stored past
        // the depth contest, because the reported line is read back from
        // this slot; a ceiling is not stored at all, so the table keeps
        // the last answer and the closest move is never promoted over a
        // move it was not shown to beat
        let bound = if score >= beta {
            self.transpositions
                .record_floor_answer(&self.board, play, value, depth);
            ScoreBound::Lower
        } else if score <= opening_alpha {
            ScoreBound::Upper
        } else {
            self.transpositions
                .record_answer(&self.board, play, value, depth);
            ScoreBound::Exact
        };
        SearchOutcome::Complete(self.result_for(play, score), bound)
    }

    /// Replay the line the table holds on a copy of the board, one stored
    /// move at a time. Walking the positions is what lets the line be
    /// checked as it is built: whether a stored move is legal here, and
    /// whether the line has reached a draw.
    pub fn pv_line(&self) -> PvLine {
        self.pv_line_from(self.transpositions.intended_play(&self.board))
    }

    /// The same line read from a first move given rather than from the
    /// table's. An aborted iteration stores nothing at the root, so the
    /// entry there still holds the move the depth before it answered, and
    /// the move the swap answers with has to be handed in.
    fn pv_line_from(&self, first: Option<Play>) -> PvLine {
        let mut line = Vec::new();
        let mut board = self.board.clone();
        let mut next = first;
        while line.len() < MAX_PLY as usize {
            let Some(play) = next else {
                break;
            };
            // a probe compares thirty two bits of the key, so the move may
            // belong to another position; the signature audit counts how
            // often
            if !board.generate_moves().contains(&play) {
                break;
            }
            if !board.make_move(&play) {
                break;
            }
            line.push(play);
            // a draw from here, so whatever the table holds next would
            // never be played
            if board.fifty_move_expired() || board.has_repeated() {
                break;
            }
            next = self.transpositions.intended_play(&board);
        }
        PvLine { line }
    }
}

impl Engine for AlphaBeta {
    fn perft(&mut self, depth: u8) -> u64 {
        self.board.perft(depth)
    }

    fn active_color(&self) -> Color {
        self.board.active_color
    }

    fn parse_fen(&mut self, fen_string: &str) -> Result<(), String> {
        self.nodes = 0;
        self.board = Board::from_fen(fen_string)?;
        Ok(())
    }

    fn new_game(&mut self) {
        self.clear_transpositions();
    }

    fn clear_table(&mut self) {
        self.clear_transpositions();
    }

    fn set_table_bytes(&mut self, bytes: usize) -> bool {
        match TranspositionTable::with_capacity_bytes(bytes) {
            Some(table) => {
                self.transpositions = table;
                true
            }
            None => false,
        }
    }

    fn iterative_deepening_search(
        &mut self,
        search_options: SearchParameters,
        mut on_depth: impl FnMut(u8, &SearchResult, PvLine, ScoreBound),
    ) -> SearchOutcome {
        // what answers if the search stops here: the deepest score that
        // landed inside its window, or a move a later depth proved worth
        // more than it
        let mut best: Option<SearchResult> = None;
        // the deepest score that landed inside its window, which is what
        // the next window is opened around. A floor is not one: it says a
        // move is worth at least beta and not what it is worth
        let mut exact: Option<Score> = None;
        // each iteration counts its own nodes, so the deepening totals them
        let mut total_nodes: u64 = 0;
        // no depth means as deep as the engine goes, which the rail ends
        // for a search under neither clock nor budget
        let max_depth = match search_options.depth {
            // held to the rail, or the depths past it would each rerun it
            Some(depth) => depth.min(MAX_PLY),
            None => MAX_PLY,
        };
        // one search, however many iterations: what they store is one
        // generation's, and the memories are kept from one iteration to the
        // next, since a killer found at depth six is what orders seven
        self.transpositions.new_search();
        self.ordering.forget();

        for depth in 1..=max_depth {
            // the soft bound: an iteration there is not enough clock left
            // for is not begun, and what is in hand answers. The deadline
            // stays as the backstop for an iteration that is begun. Asked
            // once a depth and not once a search: giving up inside a fail
            // low would answer with the move the search has just found
            // worse than it believed
            if !search_options
                .limits
                .worth_another_iteration(best.is_some())
            {
                return SearchOutcome::Aborted(best);
            }
            // the window this depth opens at, from the last exact score,
            // and the widening it is searched again under when the score
            // lands outside
            let mut window =
                Aspiration::open(self.config.aspiration.then_some(exact).flatten(), depth);
            loop {
                let (limits, stop) = search_options.for_iteration(best.is_some(), total_nodes);
                match self.search_root(depth, limits, stop, window) {
                    SearchOutcome::Aborted(deeper) => {
                        // the interrupted search's best outranks what
                        // answers now, whenever it has one. The move it
                        // hands back beat the window's alpha, which is
                        // what `search_root` checks before it hands one
                        // back at all; the moves it never reached could
                        // only raise the score further, so the score is a
                        // floor under the position. Some of the moves it
                        // did reach fell under that alpha, and nothing
                        // here claims otherwise. The swap is sound because
                        // the move it replaces is among the moves
                        // searched: the root orders by the table's entry,
                        // which is the last answer or a move shown better
                        // than it, so what answers now was the first move
                        // tried. Without that the new move would be better
                        // only over a subset the old one need not belong
                        // to. A search that reached nothing above its
                        // alpha has no move to swap in and says so, and
                        // then whatever answered going in answers still:
                        // see the arm below.
                        return SearchOutcome::Aborted(match deeper {
                            Some(mut result) => {
                                result.nodes += total_nodes;
                                // no completed depth named this move, so it
                                // is reported here, as the bound it is,
                                // before it is answered with
                                if best.as_ref().map(|had| had.best_move) != Some(result.best_move)
                                {
                                    let pv = self.pv_line_from(Some(result.best_move));
                                    debug_assert_eq!(
                                        pv.line.first(),
                                        Some(&result.best_move),
                                        "the reported line disagrees with the swapped move"
                                    );
                                    on_depth(depth, &result, pv, ScoreBound::Lower);
                                }
                                Some(result)
                            }
                            // nothing beat this search's alpha, so what
                            // answers is what answered before it: the last
                            // depth to land inside its window, or a floor
                            // this depth reported above it. Depth one runs
                            // without limits, so there always is one.
                            //
                            // The answer comes from before this iteration
                            // and the nodes it spent do not: they were
                            // spent, and the arm above carries its own, so
                            // a count that left them out would say a search
                            // stopped on its budget visited fewer nodes
                            // than the budget. `self.nodes` is this
                            // iteration's, `search_root` having zeroed it
                            // at the start and nothing having zeroed it
                            // since, and `total_nodes` is every iteration
                            // before it.
                            None => best.map(|mut answered| {
                                answered.nodes = total_nodes + self.nodes;
                                answered
                            }),
                        });
                    }
                    SearchOutcome::GameOver => {
                        return SearchOutcome::GameOver;
                    }
                    SearchOutcome::Complete(mut result, bound) => {
                        // a failed search is a search: it spent its nodes
                        // and the limits are handed what is left
                        total_nodes += result.nodes;
                        result.nodes = total_nodes;
                        // the answer and the floor were both stored past
                        // any leftover, so the table's line opens with the
                        // move reported. A ceiling stored nothing, and its
                        // closest move is not the table's, so its line is
                        // read from the move itself
                        let pv = match bound {
                            ScoreBound::Upper => self.pv_line_from(Some(result.best_move)),
                            _ => self.pv_line(),
                        };
                        debug_assert_eq!(
                            pv.line.first(),
                            Some(&result.best_move),
                            "the reported line disagrees with the move reported"
                        );
                        on_depth(depth, &result, pv, bound);
                        if bound == ScoreBound::Exact {
                            exact = Some(result.score);
                            best = Some(result);
                            break;
                        }
                        if bound == ScoreBound::Lower {
                            // a move worth at least beta, searched whole at
                            // this depth, so it outranks what answers now:
                            // that move was tried first here and came back
                            // under beta. Held as the answer in case the
                            // wider search is interrupted before it reaches
                            // the move again, which would otherwise give up
                            // a move the search has just proved better. It
                            // is not what the next window aims at, which is
                            // a score and not a floor
                            best = Some(result);
                        }
                        // the score landed outside the window, so the depth
                        // is searched again with the side that failed
                        // widened. A side that has failed its last opens to
                        // the edge, which is where this ends
                        window = window.widen(bound);
                    }
                }
            }
        }
        match best {
            Some(result) => SearchOutcome::Complete(result, ScoreBound::Exact),
            // a depth of zero runs no iterations
            None => SearchOutcome::Aborted(None),
        }
    }

    fn make_move_str(&mut self, play: &str) -> bool {
        // a `Play` prints itself as the coordinate notation the protocol
        // sends
        for p in self.board.generate_moves() {
            if play == p.to_string() {
                return self.board.make_move(&p);
            }
        }
        false
    }

    fn board_display(&self) -> String {
        self.board.to_string()
    }
}

pub struct PvLine {
    line: Vec<Play>,
}

impl PvLine {
    /// A line built by hand, which is how a protocol adapter's tests pin the
    /// format of a reported line without running a search.
    pub fn new(line: Vec<Play>) -> Self {
        Self { line }
    }
}

impl fmt::Display for PvLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let moves: Vec<String> = self.line.iter().map(|p| p.to_string()).collect();
        write!(f, "{}", moves.join(" "))
    }
}

/// The verdict of one fixed-depth search of the root.
#[derive(Debug)]
pub enum SearchOutcome {
    /// The search finished the requested depth, with what its score says
    /// about the position beside it: exact where the score landed inside
    /// the window it opened at, and a bound where it did not. A search at
    /// the full window is always exact.
    Complete(SearchResult, ScoreBound),
    /// The root has no play to make: checkmate or stalemate. Searching
    /// deeper cannot change it.
    GameOver,
    /// A limit ran out partway through, carrying a best-so-far when the
    /// root got far enough to have one. Its score is a lower bound on the
    /// position, `ScoreBound::Lower` in a report: exact over the moves the
    /// root searched, and the moves it never reached could only raise it.
    Aborted(Option<SearchResult>),
}

/// What a reported score says about the position.
///
/// `Exact` is the whole worth of it: every root move was searched and the
/// score landed inside the window. `Lower` is a floor, from an aborted
/// iteration whose remaining moves could only raise it or from a root move
/// that reached beta. `Upper` is a ceiling, from an iteration no root move
/// reached alpha in: the position is worth this or less, and the move
/// beside it is only the one that came closest. Uci prints the last two as
/// `lowerbound` and `upperbound`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoreBound {
    Exact,
    Lower,
    Upper,
}

/// The search hit a limit and unwound without finishing. The score of an
/// aborted frame is meaningless, and returning this instead of a score is
/// what keeps it out of the transposition table: propagation with `?`
/// never reaches the stores.
struct Aborted;

#[derive(Debug)]
pub struct SearchResult {
    pub nodes: u64,
    /// How long the search took, measured over the same interval as the
    /// nodes beside it, so that one divides the other.
    pub elapsed: time::Duration,
    /// How deep the deepest line went, quiescence's captures included: the
    /// `seldepth` an info line reports.
    pub selective_depth: u8,
    pub best_move: Play,
    /// What the search made of `best_move`, from the side to move.
    pub score: Score,
}

impl SearchResult {
    pub fn checkmate_in(&self) -> Option<Score> {
        crate::value::checkmate_in(self.score)
    }
}

#[cfg(test)]
mod search {
    use super::AlphaBeta;
    use super::Board;
    use super::Engine;
    use super::{
        Limits, MAX_PLY, NULL_MOVE_MIN_DEPTH, NULL_MOVE_REDUCTION, Play, RootBounds, Score,
        ScoreBound, SearchConfig, SearchOutcome, SearchParameters, SearchResult, TaintPolicy,
        Value, null_move_reduction,
    };
    use crate::board::{fens, fens::SHARP_MIDDLEGAME, play_named};
    use crate::late_move::{
        DEEP_REDUCTION, DEEP_REDUCTION_MIN_DEPTH, LATE_MOVE_MIN_DEPTH, LATE_MOVE_REDUCTION,
        LATE_MOVE_THRESHOLD,
    };
    use crate::limits::Clock;
    use crate::misc::{Color, Piece};
    use crate::value::CHECKMATE_THRESHOLD;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time;

    /// The default table is 256MB, and one per test dominated the memory
    /// and the run time of the suite. This is still far larger than
    /// anything here searches deeply enough to fill.
    const TABLE_BYTES: usize = 16 * 1024 * 1024;

    fn engine(board: Board) -> AlphaBeta {
        AlphaBeta::with_table_bytes(board, TABLE_BYTES)
    }

    #[test]
    fn a_resized_table_is_the_size_asked_for_and_still_searched_on() {
        let mut e = engine(Board::new());
        assert!(e.set_table_bytes(1024 * 1024));

        // the table a new engine asked for a megabyte would have been given,
        // which is the whole buckets that fit in one rather than the megabyte
        assert_eq!(
            e.table_bytes(),
            AlphaBeta::with_table_bytes(Board::new(), 1024 * 1024).table_bytes()
        );
        assert!(e.table_bytes() <= 1024 * 1024);
        assert!(matches!(e.search(4), SearchOutcome::Complete(_, _)));
    }

    #[test]
    fn a_table_there_is_no_memory_for_leaves_the_old_one_in_place() {
        // the size arrives from an interface, which may ask for more than the
        // machine has. Losing the engine mid game over it would be worse than
        // playing on with the table we already had.
        //
        // both ways of failing: usize::MAX asks for more bytes than an
        // allocation may describe and is refused before the allocator is
        // reached, while isize::MAX asks for a number it may describe and no
        // machine can meet, which is the refusal a real oversized Hash hits
        for bytes in [usize::MAX, isize::MAX as usize] {
            let mut e = engine(Board::new());
            assert!(!e.set_table_bytes(bytes), "{}", bytes);
            assert_eq!(e.table_bytes(), engine(Board::new()).table_bytes());
            assert!(matches!(e.search(3), SearchOutcome::Complete(_, _)));
        }
    }

    #[test]
    fn a_table_too_small_to_hold_an_entry_is_still_a_table() {
        // the size arrives from the protocol, so a nonsense one has to be
        // survivable rather than a panic in the middle of a game
        let mut e = engine(Board::new());
        assert!(e.set_table_bytes(0));
        assert!(e.table_bytes() > 0);
        assert!(matches!(e.search(3), SearchOutcome::Complete(_, _)));
    }

    /// The reference search, for the tests that hold it to answering the
    /// same whatever the table holds: the exactness contract, which every
    /// shortcut leaves green because a shortcut moves the default and not
    /// this.
    fn reference(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(board, TABLE_BYTES, SearchConfig::reference())
    }

    /// The reference with reverse futility on and nothing else touched,
    /// which is what an arm is: whatever moves between this and
    /// `reference` is the one switch.
    fn shortcut(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                reverse_futility: true,
                ..SearchConfig::reference()
            },
        )
    }

    /// The reference with the pass on.
    fn passing(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                null_move: true,
                ..SearchConfig::reference()
            },
        )
    }

    /// `passing` with the two terms on, which is the one switch between the
    /// two: the same nodes pass, and what moves is how shallow the proof
    /// each of them buys is searched.
    fn passing_adaptively(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                null_move: true,
                adaptive_null_move: true,
                ..SearchConfig::reference()
            },
        )
    }

    /// The reference with the quiet memories on. They order rather than
    /// prune, so whatever moves between this and `reference` is the tree
    /// and never the answer.
    fn remembering(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                move_memory: true,
                ..SearchConfig::reference()
            },
        )
    }

    /// The reference with the losing capture skip on.
    fn skipping(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                see_pruning: true,
                ..SearchConfig::reference()
            },
        )
    }

    /// The reference with the late move reductions on, and no memories to
    /// order the quiets it reduces.
    fn reducing(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                late_move_reductions: true,
                ..SearchConfig::reference()
            },
        )
    }

    /// `reducing` with the deep reduction on top.
    fn deep_reducing(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                late_move_reductions: true,
                deep_reductions: true,
                ..SearchConfig::reference()
            },
        )
    }

    /// `deep_reducing` with the pruning on top.
    fn pruning(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(
            board,
            TABLE_BYTES,
            SearchConfig {
                late_move_reductions: true,
                deep_reductions: true,
                late_move_pruning: true,
                ..SearchConfig::reference()
            },
        )
    }

    /// Unwrap the outcome these tests expect: a search that ran to the depth
    /// asked of it.
    fn completed(outcome: SearchOutcome) -> SearchResult {
        match outcome {
            SearchOutcome::Complete(result, _) => result,
            other => panic!("expected a completed search, got {:?}", other),
        }
    }

    /// The depth a seeded entry claims: deep enough that nothing these tests
    /// search can outrank it.
    const SEEDED_DEPTH: u8 = 5;

    #[test]
    fn a_losing_position_is_still_losing_with_a_warm_table() {
        // This is a losing position but running a search on a previous position then the losing
        // position seems to cause hash/cache collisions in some cases.
        let game =
            Board::from_fen("r4rk1/pppb1ppp/4pn2/6N1/3P4/2qBP3/P4PPP/3R1R1K w - - 2 16").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(7));
        assert!(
            result.score < -800,
            "expect bad score (first) got {}",
            result.score
        );

        let game = Board::from_fen(SHARP_MIDDLEGAME).unwrap();
        let mut e = engine(game);
        completed(e.search(7));
        let _ = e.parse_fen("r4rk1/pppb1ppp/4pn2/6N1/3P4/2qBP3/P4PPP/3R1R1K w - - 2 16");
        let result = completed(e.search(7));
        assert!(result.score < -800, "expect bad score got {}", result.score);
    }

    #[test]
    fn the_reported_line_opens_with_the_move_actually_answered() {
        // A deeper entry for the root position, left by an earlier search of
        // it, used to win the depth contest against the root's own store:
        // the table then told a line opening with the leftover's move while
        // bestmove answered the fresh one, and the two disagreed in front of
        // whatever was relaying the search. Here the queen hangs, so a fresh
        // search must answer with the capture, while the planted leftover
        // claims a quiet king move from a depth no shallow search can beat.
        let game = Board::from_fen("k7/8/8/3q4/8/8/3R4/K7 w - - 0 1").unwrap();
        let mut e = engine(game);
        let quiet = play_named(&e.board, "a1b1");
        e.transpositions
            .record_best(&e.board, quiet, Value::clean(0), 14);
        let result = completed(e.search(2));
        let takes = play_named(&e.board, "d2d5");
        assert_eq!(result.best_move, takes);
        assert_eq!(e.pv_line().line.first(), Some(&takes));
    }

    #[test]
    fn a_fail_high_on_a_later_move_is_re_searched_once_at_the_full_window() {
        // Three quiet moves and nothing for either side to capture, so at
        // depth one every child visit is exactly two nodes: the full width
        // frame and the quiescence stand pat under it. That makes the
        // re-search countable. With the middle scoring move planted as the
        // table's, the root searches it first with the full window, the
        // worse move fails its zero width search, and the best move alone
        // comes back above alpha and is searched a second time: the root's
        // node plus four child visits. Planted with the best move instead,
        // nothing fails high and the second search never happens.
        const FEN: &str = "8/8/8/8/8/8/2k4P/K7 w - - 0 1";
        let mut b = Board::from_fen(FEN).unwrap();
        let moves = b.generate_moves();
        let mut scored: Vec<(Play, Score)> = Vec::new();
        for m in &moves {
            // generation is pseudo legal, and the king stepping next to the
            // other one is refused here the way the search refuses it
            if !b.make_move(m) {
                continue;
            }
            scored.push((*m, -crate::eval::eval(&b)));
            b.undo_move();
        }
        assert_eq!(scored.len(), 3);
        scored.sort_by_key(|(_, score)| *score);
        // distinct scores, or there is no middle one to plant
        assert!(scored[0].1 < scored[1].1 && scored[1].1 < scored[2].1);
        let (middle, _) = scored[1];
        let (best, best_score) = scored[2];

        let mut e = engine(Board::from_fen(FEN).unwrap());
        e.transpositions
            .record_best(&e.board, middle, Value::clean(0), SEEDED_DEPTH);
        let result = completed(e.search(1));
        assert_eq!(result.best_move, best);
        assert_eq!(result.score, best_score);
        assert_eq!(e.nodes, 9);

        let mut e = engine(Board::from_fen(FEN).unwrap());
        e.transpositions
            .record_best(&e.board, best, Value::clean(0), SEEDED_DEPTH);
        let result = completed(e.search(1));
        assert_eq!(result.best_move, best);
        assert_eq!(result.score, best_score);
        assert_eq!(e.nodes, 7);
    }

    #[test]
    fn a_re_search_answers_with_its_own_score_not_the_probes() {
        // The node count above says a re-search ran, not whose answer came
        // back: a windowed that re-searched and then returned the probe's
        // bound would count the same. So the probe is made to fail high
        // short of the exact score, with a ceiling planted at the position
        // the move leaves, one point inside the probe's window and outside
        // the re-search's. The probe cuts on it and comes back at alpha
        // plus one; the re-search cannot cut and has to look. The exact
        // score comes from an engine of its own, whose table nothing here
        // reads, and only the re-search's answer matches it.
        const FEN: &str = "8/8/8/8/8/8/2k4P/K7 w - - 0 1";
        let mut oracle = reference(Board::from_fen(FEN).unwrap());
        let m = play_named(&oracle.board, "h2h4");
        assert!(oracle.board.make_move(&m));
        let Ok(exact) = oracle.windowed(
            Score::MIN + 2,
            Score::MAX,
            2,
            true,
            0,
            RootBounds::BOTH,
            None,
        ) else {
            panic!("an unlimited search aborted");
        };

        let alpha = exact.score - 50;
        let beta = exact.score + 50;
        let mut e = reference(Board::from_fen(FEN).unwrap());
        let m = play_named(&e.board, "h2h4");
        assert!(e.board.make_move(&m));
        let reply = play_named(&e.board, "c2c3");
        e.transpositions
            .record_ceiling(&e.board, reply, Value::clean(-alpha - 1), SEEDED_DEPTH);
        let Ok(value) = e.windowed(alpha, beta, 2, false, 0, RootBounds::NEITHER, None) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(value.score, exact.score);
    }

    #[test]
    fn the_mate_distance_survives_a_deeper_warm_search() {
        // Searching again deeper off a warm table reuses mate scores stored
        // at other plies, and the distance reported must not move when it
        // does. That is what this holds, and it holds it at every depth the
        // one engine reaches rather than at one chosen depth.
        //
        // A chosen depth would be testing the search's shortcuts instead.
        // Whether a given depth finds this mate at all is not monotone in
        // the depth: the 2026-09-13 mobility refit finds it at three,
        // misses it at four and finds it again from five. So the test asks
        // that no depth disagree with another, and that some depth find it.
        //
        // The floor is one of the four rather than three, because the late
        // move count prunes the quiet that begins the line. Measured on
        // this build, the count on finds the mate at six, seven and eight
        // and misses it at three, four and five; the count off finds it at
        // every depth from three to eight. Losing a shallow mate is what
        // pruning a late quiet by the count of moves searched does, and
        // one of four is what the shipped switches read here.
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - - 0 0").unwrap();
        let mut e = engine(game);
        let mut found = 0;
        for depth in 3..=6 {
            let result = completed(e.search(depth));
            let Some(mate) = result.checkmate_in() else {
                continue;
            };
            found += 1;
            assert_eq!(mate, 2, "the distance moved at depth {}", depth);
            assert_eq!(
                format!("{}", result.best_move),
                "g3g6",
                "the move moved at depth {}",
                depth
            );
        }
        assert!(found > 0, "no depth of the four saw the mate");
    }

    #[test]
    fn checkmate_in_one_is_found_for_black() {
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbNQp/3pN3/2pP4/2P5/PPB4P/R4RK1 b - - 1 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(4));
        assert_eq!(result.checkmate_in(), Some(-1));
    }

    /// Material that cannot mate is searched as the draw it is.
    ///
    /// Each of these was played as a win before the rule: the knight read
    /// +327 at depth twelve from its own side and -336 from the bare one, the
    /// bishop +341, and the two knights +663. Nothing in the tree knew the
    /// position was dead, so the engine spent the fifty move rule looking for
    /// a mate that is not there.
    #[test]
    fn material_that_cannot_mate_is_searched_as_a_draw() {
        for fen in [
            "8/8/8/8/8/4k3/8/4K1N1 w - - 0 1",
            "8/8/8/8/8/4k3/8/4K1N1 b - - 0 1",
            "8/8/8/8/8/4k3/8/4KB2 w - - 0 1",
            "8/8/8/8/8/4k3/8/4KB2 b - - 0 1",
            "8/8/8/8/8/4k3/8/4K1NN w - - 0 1",
            "8/8/8/8/8/4k3/8/4K1NN b - - 0 1",
        ] {
            let mut e = engine(Board::from_fen(fen).unwrap());
            let result = completed(e.search(12));
            assert_eq!(result.score, 0, "{}", fen);
        }
    }

    /// A mate inside the horizon is still found in a position the rule calls
    /// drawn.
    ///
    /// This is why the rule sits in the evaluation and not at the node. Mate
    /// comes from the move generator, so a static zero leaves it reachable; a
    /// `Value::clean(0)` returned from `alpha_beta` before the moves were
    /// generated would save the subtree and lose this.
    ///
    /// Two knights against a bare king cannot force mate, which is why the
    /// signature returns zero, but helpmates exist and the black king here
    /// stands in one.
    #[test]
    fn a_helpmate_survives_the_rule() {
        let mut e = engine(Board::from_fen("k7/3N4/1K6/1N6/8/8/8/8 w - - 0 1").unwrap());
        let result = completed(e.search(5));
        assert_eq!(result.checkmate_in(), Some(1));
        assert_eq!(format!("{}", result.best_move), "b5c7");
    }

    #[test]
    fn quiescence_does_not_stand_pat_out_of_a_mate() {
        // the queen on a8 hangs, and taking it is losing: Rxa8 Nxf2 is mate,
        // the knight covered by nothing and capturable by nothing, the king
        // shut in by its own rook and pawns. The mate arrives by a capture
        // two plies into quiescence, where the mated node used to stand pat
        // as though it could decline to move: the capture only cost a pawn,
        // so taking the queen read as winning it, and the search took it.
        let game = Board::from_fen("q7/7k/8/8/6n1/8/5PPP/R5RK w - - 0 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(1));
        assert_ne!(format!("{}", result.best_move), "a1a8");
    }

    #[test]
    fn a_capture_that_cannot_reach_alpha_is_not_searched() {
        // the rook can take the pawn and nothing else can take anything,
        // so what quiescence does with the one capture is the node count:
        // one node when it is skipped, two when it is searched down to the
        // stand pat below it
        let fen = "7k/8/8/8/R3p3/8/8/7K w - - 0 1";
        let mut e = engine(Board::from_fen(fen).unwrap());
        let standing = e.eval();
        let gain = crate::eval::material(Piece::Pawn) as Score;

        // one point past what the pawn and the whole margin can make up
        let alpha = standing + gain + super::DELTA_MARGIN + 1;
        let Ok(value) = e.quiescence(alpha, alpha + 1) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, 1);
        assert_eq!(value, Value::clean(standing));

        // at the edge, where the pawn and the margin reach alpha exactly,
        // the capture is searched
        let mut e = engine(Board::from_fen(fen).unwrap());
        let alpha = standing + gain + super::DELTA_MARGIN;
        assert!(e.quiescence(alpha, alpha + 1).is_ok());
        assert_eq!(e.nodes, 2);
    }

    #[test]
    fn an_evasion_is_searched_whatever_the_margin_says() {
        // the queen gives check and taking it is the one evasion, at an
        // alpha no capture could reach under the margin: a side in check
        // has no standing eval, so the margin does not apply and the
        // evasion is searched for what it is really worth
        let fen = "7k/8/8/8/8/8/1q6/K7 w - - 0 1";
        let mut b = Board::from_fen(fen).unwrap();
        let takes = play_named(&b, "a1b2");
        assert!(b.make_move(&takes));
        let expected = -crate::eval::eval(&b);

        let mut e = engine(Board::from_fen(fen).unwrap());
        let Ok(value) = e.quiescence(20_000, 20_001) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, 2);
        assert_eq!(value, Value::clean(expected));
    }

    #[test]
    fn a_promotion_is_searched_whatever_the_margin_says() {
        // the pawn can promote, taking the rook or pushing, at an alpha far
        // past what any margin allows. A promotion is exempt because the
        // piece that arrives is not the pawn that left, so the node visits
        // children rather than answering from its standing eval alone
        let fen = "r6k/1P6/8/8/8/8/8/7K w - - 0 1";
        let mut e = engine(Board::from_fen(fen).unwrap());
        assert!(e.quiescence(10_000, 10_001).is_ok());
        assert!(e.nodes > 1, "no promotion was searched");
    }

    #[test]
    fn a_mating_capture_is_searched_whatever_the_margin_says() {
        // rook takes rook and mates on the back rank, asked under an alpha
        // inside the mate window. A static eval is bounded by the material
        // on the board, far under any mate score, so without the exemption
        // the margin's arithmetic would call every capture hopeless here,
        // the mating one included, and the node would answer from its
        // standing eval with the mate unfound
        let fen = "3r3k/6pp/8/8/8/8/8/3R3K w - - 0 1";
        let mut e = engine(Board::from_fen(fen).unwrap());
        let Ok(value) = e.quiescence(29_500, 29_501) else {
            panic!("an unlimited search aborted");
        };
        assert!(e.nodes > 1, "the mating capture was not searched");
        assert!(
            super::is_mate(value.score) && value.score > 29_500,
            "no mate found: {}",
            value.score
        );
    }

    /// Quiescence at a window one point wide around the standing eval, so
    /// the stand pat neither cuts the node nor leaves the captures out of
    /// the window: what the node does with them is the node count.
    fn quiet_nodes(mut e: AlphaBeta) -> (u64, Value) {
        let standing = e.eval();
        let Ok(value) = e.quiescence(standing, standing + 1) else {
            panic!("an unlimited search aborted");
        };
        (e.nodes, value)
    }

    #[test]
    fn a_losing_capture_is_not_searched() {
        // the rook can take the pawn on e4 and the pawn on d5 takes it
        // back: a rook for a pawn, which the swap prices as losing, and the
        // one capture on the board. The reference searches it down to the
        // exchange; the skip answers from the stand pat alone, in one node
        let fen = "7k/8/8/3p4/R3p3/8/8/7K w - - 0 1";
        let (searched, _) = quiet_nodes(reference(Board::from_fen(fen).unwrap()));
        assert!(searched > 1, "the reference did not search the capture");

        let mut e = skipping(Board::from_fen(fen).unwrap());
        let standing = e.eval();
        let (skipped, value) = quiet_nodes(e);
        assert_eq!(skipped, 1);
        assert_eq!(value, Value::clean(standing));
    }

    #[test]
    fn a_winning_and_an_even_capture_are_searched_whatever_the_swap_says() {
        // the rook takes a pawn nothing defends, and a rook takes a rook
        // that a rook takes back: winning and even, and the skip leaves
        // both trees exactly as the reference searches them
        for fen in [
            "7k/8/8/8/R3p3/8/8/7K w - - 0 1",
            "3rr2k/8/8/8/8/8/8/4R2K w - - 0 1",
        ] {
            let (searched, answer) = quiet_nodes(reference(Board::from_fen(fen).unwrap()));
            assert!(searched > 1, "the reference did not search {fen}");
            assert_eq!(
                quiet_nodes(skipping(Board::from_fen(fen).unwrap())),
                (searched, answer),
                "{fen}"
            );
        }
    }

    #[test]
    fn a_side_in_check_searches_a_losing_evasion() {
        // the knight gives check and the queen taking it is the one
        // evasion, with the pawn on d3 taking the queen back: a losing
        // swap, and searched all the same, because a side in check has no
        // stand pat to answer from and the evasion path is not pruned. Three
        // nodes: this one, the queen's capture and the pawn's recapture
        let fen = "7k/8/8/8/8/3p4/PPn5/KQ6 w - - 0 1";
        for mut e in [
            reference(Board::from_fen(fen).unwrap()),
            skipping(Board::from_fen(fen).unwrap()),
        ] {
            let Ok(value) = e.quiescence(-10_000, 10_000) else {
                panic!("an unlimited search aborted");
            };
            assert_eq!(e.nodes, 3);
            assert!(
                !super::is_mate(value.score),
                "read as mated: {}",
                value.score
            );
        }
    }

    #[test]
    fn a_promoting_capture_is_never_skipped() {
        // the pawn takes the rook on a8 and promotes, and the rook on b8
        // takes the queen back. The swap prices a promoting capture at
        // its victim less a pawn, since the piece that arrives is counted
        // as the pawn that left, so today no promoting capture prices as
        // losing and the exemption has nothing to catch; it is there so a
        // swap that one day prices the promotion cannot skip one. What the
        // test holds is the promise: the capture is searched under the skip
        let fen = "rr5k/1P6/8/8/8/8/8/7K w - - 0 1";
        let board = Board::from_fen(fen).unwrap();
        let promotes = play_named(&board, "b7a8q");
        assert!(
            board.see(&promotes) >= 0,
            "the swap priced the promotion as losing"
        );

        let (searched, answer) = quiet_nodes(reference(Board::from_fen(fen).unwrap()));
        assert!(searched > 1, "the reference did not search the promotion");
        assert_eq!(
            quiet_nodes(skipping(Board::from_fen(fen).unwrap())),
            (searched, answer)
        );
    }

    #[test]
    fn the_mate_window_stands_the_skip_down() {
        // queen takes rook on e8 and mates: the knight that could take her
        // back is pinned to its king by the bishop, which the swap does not
        // see, so the swap prices the capture as a queen for a rook and the
        // skip would throw the mate away. Asked under an alpha inside the
        // mate window, a mate already in hand that this one would beat,
        // the exemption stands the skip down and the mate is found; asked
        // under a window nowhere near a mate, the same capture is skipped,
        // which is what says the exemption and not the swap saved it
        let fen = "4r2k/5pnp/8/8/8/2B5/8/K3Q3 w - - 0 1";
        let board = Board::from_fen(fen).unwrap();
        assert!(board.see(&play_named(&board, "e1e8")) < 0);

        let mut e = skipping(Board::from_fen(fen).unwrap());
        let Ok(value) = e.quiescence(29_500, 29_501) else {
            panic!("an unlimited search aborted");
        };
        assert!(e.nodes > 1, "the mating capture was not searched");
        assert!(
            super::is_mate(value.score) && value.score > 29_500,
            "no mate found: {}",
            value.score
        );

        // wide rather than one point around the standing eval: the even
        // capture of the knight sorts first and wins the rook a move later,
        // and a narrow window would cut the node off on it before either
        // arm reached the queen's capture
        let wide = |mut e: AlphaBeta| {
            assert!(e.quiescence(-10_000, 10_000).is_ok());
            e.nodes
        };
        let searched = wide(reference(Board::from_fen(fen).unwrap()));
        let skipped = wide(skipping(Board::from_fen(fen).unwrap()));
        assert!(
            skipped < searched,
            "nothing was skipped outside the mate window"
        );
    }

    #[test]
    fn the_horizon_sees_a_promotion_coming() {
        // the rook can win the knight across the board or take the pawn one
        // step from promoting. The pawn's push captures nothing, so
        // quiescence used not to generate it: the knight looked free to take
        // and the queen appeared only after the horizon. Taking the pawn is
        // the move.
        let game = Board::from_fen("4k3/8/8/R5n1/8/8/p5K1/8 w - - 0 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(1));
        assert_eq!(format!("{}", result.best_move), "a5a2");
    }

    #[test]
    fn a_shallow_search_still_sees_the_recapture() {
        // the queen can take a pawn which another pawn defends. A depth one
        // search ends on the capture, so only quiescence sees the recapture
        // that loses the queen for it. Shallow searches used to skip
        // quiescence and walk into it.
        let game = Board::from_fen("4k3/8/3p4/2p5/8/2Q5/8/4K3 w - - 0 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(1));
        assert_ne!(format!("{}", result.best_move), "c3c5");
    }

    #[test]
    fn deepening_through_shallow_depths_matches_a_cold_search() {
        // iterations shallower than four used to store scores whose leaves
        // were never quiesced, and deeper iterations then read those entries
        // back as if they had been: the same depth then answered differently
        // warm than cold. On the promotions position the difference was
        // visible at the root, deepening promoted to a queen where a cold
        // search of the same depth chose the rook.
        // a_warm_cache_matches_a_cold_search cannot see any of this because
        // it searches each depth directly rather than deepening to it.
        let positions = [
            fens::KIWIPETE,
            // the pawn endgame, with the fifty move counter wound on
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 10 10",
            fens::PROMOTIONS,
        ];
        for fen in positions {
            let mut cold = reference(Board::from_fen(fen).unwrap());
            let expected = completed(cold.search(5));
            let mut warm = reference(Board::from_fen(fen).unwrap());
            let result = (1..=5)
                .map(|depth| completed(warm.search(depth)))
                .next_back()
                .unwrap();
            assert_eq!(result.score, expected.score, "score differs for {}", fen);
            assert_eq!(
                format!("{}", result.best_move),
                format!("{}", expected.best_move),
                "best move differs for {}",
                fen
            );
        }
    }

    #[test]
    fn a_narrowed_root_answers_what_the_full_one_does() {
        // the reference plays with the window switched on, which nothing
        // else does. Its contract is that a depth reached by deepening
        // answers as one searched directly, and a window is meant to cost
        // less rather than to answer differently: a score inside it is the
        // position's worth, and one outside is a bound the widening
        // searches again. So this is the exactness contract asked of the
        // mechanism rather than of the table, and a failure here is a
        // failure of the schedule.
        let aspiring = SearchConfig {
            aspiration: true,
            ..SearchConfig::reference()
        };
        const DEPTH: u8 = 6;
        let mut narrowed_somewhere = false;
        for fen in [
            fens::KIWIPETE,
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 10 10",
            fens::PROMOTIONS,
        ] {
            let mut direct = reference(Board::from_fen(fen).unwrap());
            let expected = completed(direct.search(DEPTH));

            let mut open = reference(Board::from_fen(fen).unwrap());
            let full = completed(
                open.iterative_deepening_search(SearchParameters::to_depth(DEPTH), |_, _, _, _| {}),
            );
            let mut narrow =
                AlphaBeta::with_config(Board::from_fen(fen).unwrap(), TABLE_BYTES, aspiring);
            let result = completed(
                narrow
                    .iterative_deepening_search(SearchParameters::to_depth(DEPTH), |_, _, _, _| {}),
            );

            assert_eq!(result.score, expected.score, "score differs for {}", fen);
            assert_eq!(
                format!("{}", result.best_move),
                format!("{}", expected.best_move),
                "best move differs for {}",
                fen
            );
            narrowed_somewhere |= result.nodes != full.nodes;
        }
        // and the answers above are the same because the window is exact
        // rather than because nothing was narrowed
        assert!(
            narrowed_somewhere,
            "the window cost nothing anywhere, so the answers prove nothing"
        );
    }

    #[test]
    fn a_losing_side_plays_for_the_fifty_move_draw() {
        // white is a bishop down here, and every move but a pawn push or a
        // capture takes the clock to a hundred, so the draw is the best of it
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/8/8/8 w - - 99 112").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(3));
        assert_eq!(result.score, 0);
    }

    /// A fifty move draw is claimable and not automatic (FIDE 9.3; only
    /// seventy five moves under 9.6 ends a game without a claim), so a root
    /// whose counter has expired still answers with a move. It used to
    /// report game over, and the interface printed `bestmove 0000`, a
    /// forfeit in any GUI that asks rather than adjudicating. The score is
    /// zero because every move here leaves the counter running.
    #[test]
    fn a_root_whose_fifty_move_counter_has_expired_still_answers_with_a_move() {
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/8/8/8 w - - 100 112").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(3));
        assert_eq!(result.score, 0);
        assert!(
            e.board.generate_moves().contains(&result.best_move),
            "{} is not a legal move here",
            result.best_move
        );
    }

    /// The same position one ply before expiry, which always answered a move,
    /// so the pair says the counter is what changed and not the position.
    #[test]
    fn the_same_root_one_ply_before_expiry_answers_the_same_way() {
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/8/8/8 w - - 99 112").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(3));
        assert_eq!(result.score, 0);
    }

    /// A root mated on the hundredth half move is still game over: there is
    /// no legal move, and the game ended on the mate before the side mated
    /// had a move to claim on.
    #[test]
    fn a_mate_on_the_hundredth_half_move_is_still_game_over() {
        let game = Board::from_fen("7k/6Q1/6K1/8/8/8/8/8 b - - 100 112").unwrap();
        let mut e = engine(game);
        assert!(matches!(e.search(3), SearchOutcome::GameOver));
    }

    /// A move that resets the counter is worth what it wins.
    ///
    /// Queens face each other down the d file with the counter expired. Every
    /// quiet move leaves the counter running, and the tree below answers zero
    /// for those. The capture resets it and wins a queen. So the search picks
    /// between the draw and the win on their scores, which is what a root
    /// that answered the null move could not do.
    #[test]
    fn an_expired_counter_does_not_cost_a_win_a_capture_is_worth() {
        let game = Board::from_fen("3q3k/8/8/8/8/8/8/3Q2K1 w - - 100 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(6));
        assert_eq!(format!("{}", result.best_move), "d1d8");
        assert!(
            result.score > 500,
            "winning a queen scored {}",
            result.score
        );
    }

    #[test]
    fn a_depth_past_the_rail_from_a_check_is_clamped_in_the_library_too() {
        // the interface clamps what it parses, but search() is public and
        // the root deepens by one more when it is in check: the largest
        // depth a byte holds plus that one used to overflow it before the
        // search could answer
        let game = Board::from_fen("3R2k1/5ppp/8/8/8/8/8/6K1 b - - 0 1").unwrap();
        let mut e = engine(game);
        assert!(matches!(e.search(u8::MAX), SearchOutcome::GameOver));
    }

    /// The position the two below start from: black to move and in check,
    /// so the extension fires at the first node either of them searches,
    /// and the king has h7 to step out to, so there is a tree under it.
    const IN_CHECK: &str = "3R2k1/5pp1/7p/8/8/8/8/6K1 b - - 0 1";

    #[test]
    fn a_full_width_line_stops_at_the_rail() {
        // a node standing on the rail answers from the static eval, and
        // answers from it whatever depth it still holds, which is the
        // depth a line of checks would have left it
        let mut e = engine(Board::from_fen(IN_CHECK).unwrap());
        assert!(e.board.in_check());
        e.board.line_ply = MAX_PLY as usize;

        let Ok(railed) = e.alpha_beta(Score::MIN + 1, Score::MAX - 1, 4, true, RootBounds::BOTH)
        else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, 1, "the node on the rail searched on");
        assert_eq!(railed.score, e.eval());
        // the one judgement in the rail: a static eval consulted no path
        assert!(!railed.tainted);
    }

    #[test]
    fn the_ply_under_the_rail_is_searched() {
        // the boundary from the other side. The ply under the rail
        // searches, and the one evasion it has is the node that rails, so
        // the two of them are the whole count: a rail a ply early would
        // answer here instead, and no rail at all would search on.
        let mut e = engine(Board::from_fen(IN_CHECK).unwrap());
        e.board.line_ply = MAX_PLY as usize - 1;

        assert!(
            e.alpha_beta(Score::MIN + 1, Score::MAX - 1, 4, true, RootBounds::BOTH)
                .is_ok(),
            "an unlimited search aborted"
        );
        assert_eq!(e.nodes, 2, "the ply under the rail and the one it rails");
    }

    #[test]
    fn a_mate_on_the_hundredth_half_move_is_a_mate_not_a_draw() {
        // Rh8 mates, and it is the hundredth half move since anything
        // irreversible. Checkmate ends the game on the spot, before the mated
        // side has a move on which to claim the draw, so the mate outranks
        // the fifty move rule rather than the other way round
        let game = Board::from_fen("k7/8/1K6/8/8/8/8/7R w - - 99 100").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(2));
        assert_eq!(result.checkmate_in(), Some(1));
        assert_eq!(format!("{}", result.best_move), "h1h8");
    }

    #[test]
    fn a_check_that_does_not_mate_on_the_hundredth_half_move_is_still_a_draw() {
        // the same rook gives check on h8 but the king slips out to a7, so
        // the hundredth half move ends the game as a draw after all
        let game = Board::from_fen("k7/8/2K5/8/8/8/8/7R w - - 99 100").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(3));
        assert_eq!(result.score, 0);
    }

    /// A budget of nodes and no clock.
    fn nodes_only(nodes: u64) -> Limits {
        Limits::starting_at(time::Instant::now(), None, nodes)
    }

    /// A search whose clock ran out before it began.
    fn already_spent() -> Limits {
        Limits::starting_at(
            time::Instant::now() - time::Duration::from_secs(1),
            Some(Clock::Share(time::Duration::from_millis(1))),
            u64::MAX,
        )
    }

    #[test]
    fn a_blown_deadline_stops_before_it_searches() {
        // the first poll happens before the root is counted, so a clock
        // already gone costs nothing at all rather than a poll interval
        let mut e = engine(Board::new());
        assert!(matches!(
            e.search_within(5, already_spent()),
            SearchOutcome::Aborted(None)
        ));
        assert_eq!(e.nodes, 0, "it searched past a clock that had run out");
    }

    #[test]
    fn deepening_with_no_time_budget_still_answers_depth_one() {
        // a clock that has already run out must still get a legal move back:
        // depth one is a few dozen nodes, so it runs whatever the clock says,
        // and only then is the budget allowed to stop anything
        let mut e = engine(Board::new());
        let options = SearchParameters::new(None, already_spent());
        let mut depths = Vec::new();
        let outcome = e.iterative_deepening_search(options, |depth, _, _, _| depths.push(depth));
        assert!(
            matches!(outcome, SearchOutcome::Aborted(Some(_))),
            "expected a move from depth one, got {:?}",
            outcome
        );
        assert_eq!(depths, vec![1]);
    }

    #[test]
    fn a_node_budget_stops_the_search_on_exactly_that_node() {
        // a spread of budgets, so the last node falls in the full search and
        // in quiescence by turns, and now and then exactly on the end of an
        // iteration, which leaves the next one nothing and aborts it at once
        for limit in (50..6_000).step_by(97) {
            let mut e = engine(Board::new());
            let options = SearchParameters::new(None, nodes_only(limit));
            let mut completed: u64 = 0;
            let outcome =
                e.iterative_deepening_search(options, |_, result, _, _| completed = result.nodes);
            assert!(
                matches!(outcome, SearchOutcome::Aborted(Some(_))),
                "{}",
                limit
            );
            // an abort leaves the count in one of two shapes, and both say
            // every node visited is the budget to the node. Usually the
            // last thing reported is the last search that finished, and the
            // aborted search's own nodes are still on the engine, so the
            // two add up. Where the aborted search found a move to swap in,
            // it reported that move at the node it was interrupted on, and
            // that report already covers the whole deepening
            assert!(
                completed + e.nodes == limit || completed == limit,
                "budget {}: {} reported with {} left on the engine",
                limit,
                completed,
                e.nodes
            );
        }
    }

    #[test]
    fn an_aborted_iteration_still_counts_the_whole_deepening() {
        // the same sweep as above read the other way round: wherever the root
        // finished a move before the budget ran out, that move answers rather
        // than the completed depth's, and its count covers the aborted
        // iteration as well as the depths before it, which together are the
        // budget to the node
        let mut deeper = 0;
        for limit in (50..6_000).step_by(97) {
            let mut e = engine(Board::new());
            let options = SearchParameters::new(None, nodes_only(limit));
            let mut reported = 0;
            let outcome =
                e.iterative_deepening_search(options, |_, result, _, _| reported = result.nodes);
            let SearchOutcome::Aborted(Some(result)) = outcome else {
                panic!(
                    "expected a move under a budget of {}, got {:?}",
                    limit, outcome
                )
            };
            if reported + e.nodes == limit {
                // the aborted search reached no move to swap in, so what
                // answers is what answered going in. The last thing
                // reported is the last search that finished, which need not
                // be the answer: a ceiling finishes and does not answer, so
                // the answer's own count can be shallower than this. The
                // test above is what says these two add up
                continue;
            }
            assert_eq!(result.nodes, limit, "budget {}", limit);
            deeper += 1;
        }
        assert!(
            deeper > 0,
            "no budget in the sweep aborted with a root move in hand"
        );
    }

    #[test]
    fn an_iteration_that_searched_no_root_move_leaves_the_completed_depth_answering() {
        // a budget of exactly what three depths cost leaves the fourth
        // nothing at all: it aborts on its first poll with no move of its
        // own, so the answer is still the one depth three completed
        let mut e = engine(Board::new());
        let three = completed(e.iterative_deepening_search(
            SearchParameters::new(Some(3), Limits::unlimited()),
            |_, _, _, _| {},
        ));

        let mut e = engine(Board::new());
        let options = SearchParameters::new(None, nodes_only(three.nodes));
        let outcome = e.iterative_deepening_search(options, |_, _, _, _| {});
        let SearchOutcome::Aborted(Some(result)) = outcome else {
            panic!("expected the completed depth's move, got {:?}", outcome)
        };
        assert_eq!(result.nodes, three.nodes);
        assert_eq!(result.best_move, three.best_move);
    }

    /// What a fresh engine answers depth four from the opening with, and
    /// what a depth five search under `budget` answers after it. Built anew
    /// for every budget so that the table each one searches with is the
    /// same and the answer depends on the budget alone.
    fn five_after_four(budget: u64) -> (Play, SearchOutcome) {
        let mut e = engine(Board::new());
        let four = completed(e.search(4));
        let five = e.search_within(5, nodes_only(budget));
        (four.best_move, five)
    }

    #[test]
    fn the_root_searches_the_previous_depths_best_move_first() {
        // what makes the swap above sound, and the one thing that would
        // silently unmake it. The smallest budget an aborted iteration has a
        // move to answer with is the one that just covers the first root
        // move it tried, so whatever answers at that budget is the move the
        // root tried first, and it has to be the one the depth before
        // answered with
        let finished = |budget| !matches!(five_after_four(budget).1, SearchOutcome::Aborted(None));
        // more nodes is never fewer root moves finished, so the smallest
        // budget with a move in hand can be bisected for
        let (mut none, mut some) = (0, 100_000);
        assert!(
            finished(some),
            "depth five finished no root move in {} nodes",
            some
        );
        while none + 1 < some {
            let mid = (none + some) / 2;
            if finished(mid) {
                some = mid
            } else {
                none = mid
            }
        }

        let (previous, outcome) = five_after_four(some);
        let SearchOutcome::Aborted(Some(result)) = outcome else {
            panic!(
                "one root move is not the whole of depth five, got {:?}",
                outcome
            )
        };
        assert_eq!(result.best_move, previous);
    }

    #[test]
    fn a_swapped_answer_is_reported_before_the_search_ends() {
        // the swap is the one answer no completed depth reported, so without
        // a report of its own the last line the caller heard opens with the
        // move being given up. A sweep of budgets, because which of them
        // ends an iteration on a better move is a fact about this position
        // rather than one to work out here
        let mut swaps = 0;
        for limit in (500..40_000).step_by(311) {
            let mut e = engine(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
            let options = SearchParameters::new(None, nodes_only(limit));
            let mut reports: Vec<(Play, Option<Play>, ScoreBound)> = Vec::new();
            let outcome = e.iterative_deepening_search(options, |_, result, pv, bound| {
                reports.push((result.best_move, pv.line.first().copied(), bound));
            });
            let SearchOutcome::Aborted(Some(result)) = outcome else {
                continue;
            };
            let completed = reports
                .iter()
                .rfind(|(_, _, bound)| *bound == ScoreBound::Exact)
                .map(|(play, _, _)| *play);
            if completed == Some(result.best_move) {
                // the deepest completed depth answered, which its own report
                // already described. A depth may still say something after
                // it: a ceiling names the move that came closest and not an
                // answer, and a fail high that finished names a floor under
                // the move already in hand. What would be wrong is a floor
                // opening with a move the search does not answer with, since
                // that is the line an unreported swap would leave the caller
                // holding
                if let Some((play, _, ScoreBound::Lower)) = reports.last() {
                    assert_eq!(
                        *play, result.best_move,
                        "budget {}: a floor named a move the search did not answer with",
                        limit
                    );
                }
                continue;
            }
            swaps += 1;
            assert_eq!(
                reports
                    .last()
                    .map(|(play, first, bound)| (*play, *first, *bound)),
                Some((result.best_move, Some(result.best_move), ScoreBound::Lower)),
                "budget {}: the swapped move was never reported",
                limit
            );
        }
        assert!(swaps > 0, "no budget in the sweep swapped a move in");
    }

    /// A search stopped by its budget says it spent the budget, whichever
    /// of the two aborted answers it came back with.
    ///
    /// An iteration begun and then cut short spends nodes whether or not
    /// anything in it beat the window's alpha. The answer comes from the
    /// depth before it when nothing did, and the nodes are still the
    /// search's: a count that left them out would say a search held to a
    /// budget visited fewer nodes than the budget, and a caller cannot
    /// recover them, since the two aborted answers arrive in the same
    /// shape. Swept over budgets, because which of the two fires depends
    /// on where inside the iteration the budget ran out.
    #[test]
    fn a_search_stopped_by_its_budget_counts_the_iteration_it_gave_up() {
        let mut bound = 0;
        for limit in [1_000u64, 2_500, 5_000, 7_500, 10_000, 25_000, 50_000] {
            let mut e = engine(Board::new());
            let outcome = e.iterative_deepening_search(
                SearchParameters::new(None, Limits::starting_now(None, Some(limit))),
                |_, _, _, _| {},
            );
            let SearchOutcome::Aborted(Some(result)) = outcome else {
                panic!("budget {limit}: an unlimited depth under a budget aborts with an answer");
            };
            bound += 1;
            assert_eq!(
                result.nodes, limit,
                "budget {limit}: the search says it spent other than its budget"
            );
        }
        assert!(bound > 0, "no budget in the sweep bound the search");
    }

    #[test]
    fn a_completed_depth_is_reported_as_an_exact_score() {
        let mut e = engine(Board::new());
        let mut bounds = Vec::new();
        let outcome = e.iterative_deepening_search(
            SearchParameters::new(Some(4), Limits::unlimited()),
            |_, _, _, bound| bounds.push(bound),
        );
        assert!(matches!(outcome, SearchOutcome::Complete(_, _)));
        assert_eq!(bounds, vec![ScoreBound::Exact; 4]);
    }

    #[test]
    fn a_node_budget_and_a_clock_stop_at_whichever_comes_first() {
        // the clock wins: a spent clock and a generous budget end after
        // depth one, which runs whatever either says
        let mut e = engine(Board::new());
        let options = SearchParameters::new(
            None,
            Limits::starting_at(
                time::Instant::now() - time::Duration::from_secs(1),
                Some(Clock::Share(time::Duration::from_millis(1))),
                1_000_000,
            ),
        );
        let mut depths = Vec::new();
        let outcome = e.iterative_deepening_search(options, |depth, _, _, _| depths.push(depth));
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
        assert_eq!(depths, vec![1]);

        // the budget wins: a clock with time to spare and a small budget stop
        // on the budget's node
        let mut e = engine(Board::new());
        let limit = 1_000;
        let options = SearchParameters::new(
            None,
            Limits::starting_now(
                Some(Clock::Share(time::Duration::from_secs(10))),
                Some(limit),
            ),
        );
        let mut completed: u64 = 0;
        let outcome =
            e.iterative_deepening_search(options, |_, result, _, _| completed = result.nodes);
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
        assert_eq!(completed + e.nodes, limit);
    }

    #[test]
    fn a_stop_flag_already_set_still_answers_the_first_depth() {
        // the flag is armed the way the clock is, so depth one runs whatever
        // it says and there is always a real move to answer with
        let mut e = engine(Board::new());
        let stop = Arc::new(AtomicBool::new(true));
        let options = SearchParameters::stoppable(None, Limits::unlimited(), Arc::clone(&stop));
        let mut depths = Vec::new();
        let outcome = e.iterative_deepening_search(options, |depth, _, _, _| depths.push(depth));
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
        assert_eq!(
            depths,
            vec![1],
            "the flag stopped the search before it had a move"
        );
    }

    #[test]
    fn a_stop_flag_set_mid_search_ends_the_deepening_with_a_move() {
        // an unlimited search of a sharp position would run for minutes.
        // A thread sets the flag once a depth has been reported, and what
        // comes back is the move in hand rather than nothing
        let mut e = engine(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let options = SearchParameters::stoppable(None, Limits::unlimited(), Arc::clone(&stop));
        let mut deepest = 0;
        let outcome = e.iterative_deepening_search(options, |depth, _, _, _| {
            deepest = depth;
            if depth >= 3 {
                stop.store(true, Ordering::Relaxed);
            }
        });
        let (SearchOutcome::Aborted(Some(result)) | SearchOutcome::Complete(result, _)) = outcome
        else {
            panic!("a stopped search must still answer, got {:?}", outcome)
        };
        assert!(deepest < super::MAX_PLY, "the flag stopped nothing");
        assert!(result.nodes > 0);
    }

    /// The clock and the node budget are armed the same way, which `Limits`
    /// says of itself; this is the flag.
    #[test]
    fn the_stop_flag_is_not_armed_until_a_depth_has_been_answered() {
        let stop = Arc::new(AtomicBool::new(false));
        let options = SearchParameters::stoppable(None, Limits::unlimited(), Arc::clone(&stop));
        let (_, unarmed) = options.for_iteration(false, 0);
        assert!(unarmed.is_none(), "the first iteration was stoppable");
        let (_, armed) = options.for_iteration(true, 0);
        let armed = armed.expect("an answered search was not stoppable");
        assert!(
            Arc::ptr_eq(&armed, &stop),
            "the armed flag is not the caller's"
        );
    }

    #[test]
    fn a_search_asked_for_directly_carries_no_flag_to_read() {
        // what keeps the bench counting what it counted before there was a
        // flag: nothing but the deepening loop ever arms one
        let mut e = engine(Board::new());
        e.stop = Some(Arc::new(AtomicBool::new(true)));
        assert!(matches!(e.search(2), SearchOutcome::Complete(_, _)));
        assert!(e.stop.is_none(), "a leftover flag outlived the search");
        assert!(
            SearchParameters::new(Some(2), Limits::unlimited())
                .stop
                .is_none()
        );
    }

    #[test]
    fn a_node_budget_too_small_for_depth_one_still_answers_a_move() {
        let mut e = engine(Board::new());
        let options = SearchParameters::new(None, nodes_only(0));
        let mut depths = Vec::new();
        let outcome = e.iterative_deepening_search(options, |depth, _, _, _| depths.push(depth));
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
        assert_eq!(depths, vec![1]);
    }

    #[test]
    fn a_node_budget_and_a_depth_stop_at_whichever_comes_first() {
        let mut e = engine(Board::new());
        let options = SearchParameters::new(Some(2), nodes_only(1_000_000));
        assert!(matches!(
            e.iterative_deepening_search(options, |_, _, _, _| {}),
            SearchOutcome::Complete(_, _)
        ));

        let mut e = engine(Board::new());
        let options = SearchParameters::new(Some(super::MAX_PLY), nodes_only(1_000));
        let mut last_depth = 0;
        let outcome = e.iterative_deepening_search(options, |depth, _, _, _| last_depth = depth);
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
        assert!(last_depth < super::MAX_PLY);
    }

    #[test]
    fn deepening_reports_each_completed_depth() {
        let mut e = engine(Board::new());
        let mut depths = Vec::new();
        let mut node_counts = Vec::new();
        let outcome =
            e.iterative_deepening_search(SearchParameters::to_depth(3), |depth, result, _, _| {
                assert!(result.nodes > 0);
                depths.push(depth);
                node_counts.push(result.nodes);
            });
        assert_eq!(depths, vec![1, 2, 3]);
        // the count covers the whole deepening so far, so each report says
        // more than the one before: reporting one iteration's count against
        // the whole search's clock is the bug this pins shut
        assert!(
            node_counts.windows(2).all(|w| w[0] < w[1]),
            "node counts must grow with each depth: {:?}",
            node_counts
        );
        let SearchOutcome::Complete(result, _) = outcome else {
            panic!("expected a completed search, got {:?}", outcome);
        };
        assert_eq!(
            Some(result.nodes),
            node_counts.last().copied(),
            "the returned result must carry the same total the last report did"
        );
    }

    #[test]
    fn a_finished_game_is_game_over_with_no_depth_to_report() {
        // fool's mate, white to move with no reply, and a stalemate: there is
        // nothing to play, so a search says so and deepening reports nothing
        let fens = [
            "rnb1kbnr/pppp1ppp/8/4p3/6Pq/5P2/PPPPP2P/RNBQKBNR w KQkq - 1 3",
            "k7/8/1Q6/8/8/8/8/7K b - - 0 1",
        ];
        for fen in fens {
            let mut e = engine(Board::from_fen(fen).unwrap());
            assert!(matches!(e.search(3), SearchOutcome::GameOver), "{fen}");
            let outcome = e
                .iterative_deepening_search(SearchParameters::to_depth(3), |_, _, _, _| {
                    panic!("a finished game has no depths to report")
                });
            assert!(matches!(outcome, SearchOutcome::GameOver), "{fen}");
        }
    }

    /// Every quiet past the node's first may be pruned at depths one to
    /// three, and the move loop reads no legal move found as mate or
    /// stalemate. So the first move searched is exempt, and this is the
    /// test of that: a node whose every quiet the rule would prune still
    /// answers with what a move of it is worth rather than with the
    /// stalemate's zero. The exactness tests cannot see this, because the
    /// reference has the rule off, so it is asked of the default here.
    #[test]
    fn a_node_that_prunes_every_late_quiet_is_not_stalemated() {
        // white a knight and a bishop against a bare king, with no capture
        // and no move that gives check, so every move the node has is one
        // the rule can reach. The knight's own moves and the light squared
        // bishop's both stay off the dark corner the black king stands on,
        // which is what keeps the check exemption out of the test.
        let mut e = engine(Board::from_fen("7k/8/8/8/8/8/8/KN1B4 w - - 0 1").unwrap());
        assert!(e.config.quiet_futility, "the default carries the rule");
        let eval = e.eval();
        assert!(eval > 100, "the side to move is a piece up twice: {eval}");
        // alpha far past anything the margin can reach, so every quiet
        // past the first is futile at every depth the rule decides
        let alpha = eval + 10_000;
        assert!(!crate::value::is_mate(alpha));
        for depth in 1..=crate::late_move::SHALLOW_MAX_DEPTH {
            let Ok(value) = e.alpha_beta(alpha, alpha + 1, depth, true, RootBounds::NEITHER) else {
                panic!("nothing was armed to abort this search");
            };
            assert!(
                value.score > 100,
                "depth {depth} answered {}, which is the stalemate a pruned first move leaves",
                value.score
            );
        }
    }

    #[test]
    fn a_clock_that_runs_out_mid_deepening_still_answers_with_a_move() {
        // the one clock in the suite that is not zero. A zero budget aborts
        // on the first poll, before the next check is ever armed, so this
        // is the only test of the clock being read again thousands of nodes
        // on. Fifty milliseconds from the opening is orders of magnitude
        // short of the ply rail, so the clock wins, and whether the move comes
        // from the last depth to finish or from the one it stopped in the
        // middle of, there has to be one
        let mut e = engine(Board::new());
        let params = SearchParameters::new(
            None,
            Limits::starting_now(Some(Clock::Share(time::Duration::from_millis(50))), None),
        );
        let outcome = e.iterative_deepening_search(params, |_, _, _, _| {});
        assert!(matches!(outcome, SearchOutcome::Aborted(Some(_))));
    }

    #[test]
    fn a_deepening_stops_before_an_iteration_the_clock_will_not_cover() {
        // more than the soft share of a second has gone and the second
        // itself has not, so the deadline would let a second depth start and
        // the soft bound does not. Which fraction of the budget that is, and
        // why, belongs to Limits; what is asserted here is that the deepening
        // loop asks it at all, and asks it only of a share of a game clock
        for (kind, depths) in [
            (Clock::Share as fn(time::Duration) -> Clock, vec![1]),
            (Clock::Fixed as fn(time::Duration) -> Clock, vec![1, 2]),
        ] {
            let mut e = engine(Board::new());
            let params = SearchParameters::new(
                Some(2),
                Limits::starting_at(
                    time::Instant::now() - time::Duration::from_millis(600),
                    Some(kind(time::Duration::from_secs(1))),
                    u64::MAX,
                ),
            );
            let mut reached = Vec::new();
            e.iterative_deepening_search(params, |depth, _, _, _| reached.push(depth));
            assert_eq!(reached, depths, "{:?}", kind(time::Duration::from_secs(1)));
        }
    }

    #[test]
    fn deepening_to_depth_zero_finds_nothing() {
        let mut e = engine(Board::new());
        let outcome = e.iterative_deepening_search(SearchParameters::to_depth(0), |_, _, _, _| {});
        assert!(matches!(outcome, SearchOutcome::Aborted(None)));
    }

    #[test]
    fn draw_taint_is_still_recorded_and_never_trusted() {
        // the pawn endgame carries the most draw traffic of the bench
        // positions. tainted_stores at zero means taint propagation broke
        // and the probe's refusal is refusing nothing; tainted_score_cutoffs
        // moving off zero means a probe path without the refusal guard was
        // added. The refusal is the reference's policy, so the reference is
        // what is built
        let fen = fens::PAWN_ENDGAME;
        let mut e = reference(Board::from_fen(fen).unwrap());
        for depth in 1..=7 {
            completed(e.search(depth));
        }
        assert!(e.ghi().stores > 0, "the search stored nothing");
        assert!(
            e.ghi().tainted_stores > 0,
            "no draw taint was recorded: propagation is broken"
        );
        assert_eq!(
            e.ghi().tainted_score_cutoffs,
            0,
            "a path dependent score was trusted"
        );
        // the refusals are what the policy costs, so they are counted as
        // the cutoffs would have been: a refusal that went uncounted would
        // make the policy look free
        assert!(
            e.ghi().refused_cutoffs > 0,
            "the refusal refused nothing, or refused without counting"
        );
    }

    #[test]
    fn every_taint_word_names_a_policy_and_the_policy_names_it_back() {
        // the bench reads a word and prints one, and they have to be the
        // same word or a report could not be rerun from its header; and
        // the word the default prints has to name the default, or a bench
        // told nothing and a bench told that word would run apart
        for word in ["refuse", "trust", "skip", "rule50"] {
            let config =
                SearchConfig::with_taint(word).unwrap_or_else(|| panic!("{word} is not a policy"));
            assert_eq!(config.taint_word(), word);
        }
        let default = SearchConfig::default();
        assert_eq!(
            SearchConfig::with_taint(default.taint_word()),
            Some(default)
        );
        assert_eq!(SearchConfig::with_taint("maybe"), None);
    }

    #[test]
    fn taint_crosses_a_quiescence_frame_whose_tainted_capture_is_not_last() {
        // a trusting search that cuts on a tainted entry inside a capture
        // tree must taint what flows out of that tree. The queen forks the
        // rook and the pawn, and no white move saves both, so every line
        // concedes something and the capture tree is really searched. The
        // rook is taken first, into a seeded tainted entry; the pawn
        // capture searched after it must not launder the flag on its way
        // out, and the stores that follow say whether it did
        let fen = "7k/3q4/8/8/R5P1/8/8/K7 w - - 0 1";
        let seeded = |config: SearchConfig| {
            let mut e = AlphaBeta::with_config(Board::from_fen(fen).unwrap(), TABLE_BYTES, config);
            let mut board = e.board.clone();
            for name in ["a1b1", "d7a4"] {
                let play = play_named(&board, name);
                assert!(board.make_move(&play), "failed to play {}", name);
            }
            let any = play_named(&board, "b1c1");
            e.transpositions
                .record_best(&board, any, Value::tainted(0), 9);
            // the root's own entry names the king move, so the seeded line
            // is searched first, with the whole window still open: no
            // sibling's value has yet shrunk it to where standing pat ends
            // the frame before the captures run
            let king = play_named(&e.board, "a1b1");
            e.transpositions
                .record_best(&e.board, king, Value::clean(0), 9);
            // the seeding itself counts one tainted store, so the search's
            // own contribution is what the two policies are compared on
            let seeded = e.ghi().tainted_stores;
            completed(e.search(1));
            e.ghi().tainted_stores - seeded
        };
        let trusting = SearchConfig {
            taint: TaintPolicy::Trust,
            ..SearchConfig::reference()
        };
        assert!(
            seeded(trusting) > 0,
            "the taint was laundered between the capture and the root"
        );
        assert_eq!(
            seeded(SearchConfig::reference()),
            0,
            "a refusing search took the tainted cutoff after all"
        );
    }

    #[test]
    fn a_search_told_to_trust_tainted_scores_takes_their_cutoffs() {
        // the refusal is what the reference carries, and a search told to
        // trust those scores is the control arm of the graph history
        // experiments. The switch has to reach the probe: a field the search
        // never reads would make every comparison against the reference a
        // comparison of the reference with itself
        let fen = fens::PAWN_ENDGAME;
        // the reference with the one switch flipped
        let trusting = SearchConfig {
            taint: TaintPolicy::Trust,
            ..SearchConfig::reference()
        };
        let mut e = AlphaBeta::with_config(Board::from_fen(fen).unwrap(), TABLE_BYTES, trusting);
        for depth in 1..=7 {
            completed(e.search(depth));
        }
        assert!(
            e.ghi().tainted_score_cutoffs > 0,
            "the scores the search was told to trust cut nothing"
        );
        assert_eq!(
            e.ghi().refused_cutoffs,
            0,
            "a search trusting tainted scores refused one"
        );
    }

    #[test]
    fn the_static_shortcut_looks_at_less_of_the_tree() {
        // the switch has to reach the search, or every comparison with the
        // reference would be of the reference with itself
        let mut e = shortcut(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(e.search(6));
        let mut cold = reference(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(cold.search(6));
        assert!(
            e.nodes < cold.nodes,
            "the shortcut searched {} nodes against the reference's {}",
            e.nodes,
            cold.nodes
        );
    }

    #[test]
    fn the_static_shortcut_is_never_taken_by_a_node_in_check() {
        // hxg7+ Kxg7 Rxh7+ Kxh7 Qf6 mates, and every node of that line
        // after the first is a node in check: white hands over a pawn and
        // a rook to open the king up, so at each of them black is the
        // material to the good and a static eval standing still there
        // reads as winning. A shortcut that fired in check would answer
        // those nodes from that material and the mate would go with them,
        // the position reading a piece down for white instead. In check
        // there is no declining to move, so no static floor exists to be
        // answered from, which is the same reason quiescence does not
        // stand pat there.
        let fen = "r5rk/2p1Nppp/3p3P/pp2p1P1/4P3/2qnPQK1/8/R6R w - - 0 1";
        let mut e = shortcut(Board::from_fen(fen).unwrap());
        let result = completed(e.search(4));
        assert_eq!(result.checkmate_in(), Some(4));

        let mut cold = reference(Board::from_fen(fen).unwrap());
        let expected = completed(cold.search(4));
        assert_eq!(result.score, expected.score);
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
        );
    }

    #[test]
    fn a_mate_in_the_window_is_searched_for_rather_than_guessed_at() {
        // Once a mate is in hand, everything searched after it is searched
        // with minus that mate for a beta, and every eval of a board stands
        // above a number like that: a shortcut that did not look at beta
        // would answer the whole of the rest of the list from the static
        // eval, and a faster mate hiding in it would never be looked for.
        // A canary rather than a discrimination: on every position tried,
        // dropping the guard changed the tree by a seventh and the answers
        // not at all, so what this holds is that the mate distances stay
        // right, not that the guard alone keeps them so.
        let fens = [
            "5n1k/5Kpp/8/8/8/8/8/2Q4R w - - 0 1",
            "2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - - 0 1",
            "2r3k1/p4p2/3Rp2p/1p2P1pK/8/1P4P1/P3Q2P/1q6 b - - 0 1",
        ];
        for fen in fens {
            let mut e = shortcut(Board::from_fen(fen).unwrap());
            let result = completed(e.search(5));
            let mut cold = reference(Board::from_fen(fen).unwrap());
            let expected = completed(cold.search(5));
            assert!(expected.checkmate_in().is_some(), "{} mates nobody", fen);
            assert_eq!(result.checkmate_in(), expected.checkmate_in(), "{}", fen);
            assert_eq!(
                format!("{}", result.best_move),
                format!("{}", expected.best_move),
                "{}",
                fen
            );
        }
    }

    #[test]
    fn the_static_shortcut_is_never_taken_by_a_side_holding_only_pawns() {
        // the trebuchet, where whoever is to move loses: both kings are in
        // zugzwang, every move there is worsens the position, and a static
        // floor is exactly the thing that is not true of it. Neither side
        // has a piece and neither pawn can ever promote past the other, so
        // the shortcut is refused at every node of this tree and the arm
        // searches what the reference searches, node for node.
        let fen = "8/8/8/4p3/4Pk2/3K4/8/8 w - - 0 1";
        let mut e = shortcut(Board::from_fen(fen).unwrap());
        let result = completed(e.search(7));
        let mut cold = reference(Board::from_fen(fen).unwrap());
        let expected = completed(cold.search(7));
        assert_eq!(e.nodes, cold.nodes);
        assert_eq!(result.score, expected.score);
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
        );
    }

    #[test]
    fn passing_looks_at_less_of_the_tree() {
        // the switch has to reach the search, as above
        let mut e = passing(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(e.search(6));
        let mut cold = reference(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(cold.search(6));
        assert!(
            e.nodes < cold.nodes,
            "the pass searched {} nodes against the reference's {}",
            e.nodes,
            cold.nodes
        );
    }

    #[test]
    fn the_depth_term_leaves_the_reduced_search_a_depth_it_can_hold() {
        // `depth - 1 - r` is unsigned at the call site, so an `r` past
        // `depth - 1` is not an over-reduction but a wrap to a depth near
        // the top of the range, and the pass would then search deeper than
        // the node that asked for it. Every depth a pass is offered at,
        // and every depth past the bench's, held to leaving that
        // subtraction a number.
        let adaptive = SearchConfig::default();
        for depth in NULL_MOVE_MIN_DEPTH..=u8::MAX {
            for eval_beta in [0, 1, 199, 200, 399, 400, 599, 600, 5_000, Score::MAX] {
                let r = null_move_reduction(adaptive, depth, eval_beta);
                assert!(
                    r < depth,
                    "depth {depth} at a margin of {eval_beta} reduced by {r}, \
                     which the subtraction cannot hold"
                );
                assert!(
                    r >= NULL_MOVE_REDUCTION,
                    "depth {depth} at a margin of {eval_beta} reduced by {r}, \
                     under the flat base"
                );
            }
        }
    }

    #[test]
    fn the_depth_term_grows_the_reduction_and_the_switch_holds_it_flat() {
        // the arm is the growth and nothing else, so the two configs are
        // read at the same depths: off the switch every depth reads the
        // base, on it the base plus a sixth of the depth. Depth 6 is the
        // first the term moves, which is what the divisor says.
        let flat = SearchConfig {
            adaptive_null_move: false,
            ..SearchConfig::default()
        };
        for depth in [3, 4, 5, 6, 9, 11, 12, 18] {
            assert_eq!(
                null_move_reduction(flat, depth, 0),
                NULL_MOVE_REDUCTION,
                "depth {depth} off the switch"
            );
            assert_eq!(
                null_move_reduction(flat, depth, 600),
                NULL_MOVE_REDUCTION,
                "depth {depth} off the switch at a wide margin"
            );
        }
        for (depth, expected) in [(3, 2), (5, 2), (6, 3), (9, 3), (11, 3), (12, 4), (18, 5)] {
            assert_eq!(
                null_move_reduction(SearchConfig::default(), depth, 0),
                expected,
                "depth {depth} on the switch at no margin"
            );
        }
    }

    #[test]
    fn the_margin_term_steps_every_two_pawns_and_stops_at_its_cap() {
        // read at depth 18, where the clamp cannot reach and the two terms
        // are visible apart: the depth term gives 5 there and the margin
        // adds a ply for each whole `NULL_MOVE_EVAL_UNIT` of clearance,
        // three at most. The unit's own boundaries are the cases: a margin
        // one short of a unit buys nothing.
        let deep = 18;
        for (eval_beta, expected) in [
            (0, 5),
            (199, 5),
            (200, 6),
            (399, 6),
            (400, 7),
            (599, 7),
            (600, 8),
            (5_000, 8),
            (Score::MAX, 8),
        ] {
            assert_eq!(
                null_move_reduction(SearchConfig::default(), deep, eval_beta),
                expected,
                "depth {deep} at a margin of {eval_beta}"
            );
        }
    }

    #[test]
    fn the_margin_term_is_clamped_where_the_depth_cannot_hold_it() {
        // the shallowest depths are where the cap would ask for more plies
        // than there are. The clamp leaves the reduced search at depth
        // zero, which is quiescence, and is the same floor the pass has at
        // depth 3 today rather than a new behaviour.
        for (depth, expected) in [(3, 2), (4, 3), (5, 4), (6, 5), (7, 6), (8, 6)] {
            assert_eq!(
                null_move_reduction(SearchConfig::default(), depth, 600),
                expected,
                "depth {depth} at the cap's margin"
            );
        }
    }

    #[test]
    fn the_depth_term_looks_at_less_of_the_tree_than_the_flat_reduction() {
        // the switch has to reach the search. Depth 8, because the term
        // first moves at a node of depth 6 and a root of six reaches one
        // such node, the root itself.
        let mut e = passing_adaptively(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(e.search(8));
        let mut flat = passing(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(flat.search(8));
        assert!(
            e.nodes < flat.nodes,
            "the grown reduction searched {} nodes against the flat one's {}",
            e.nodes,
            flat.nodes
        );
    }

    #[test]
    fn a_mate_found_through_a_grown_pass_does_not_come_back_as_one() {
        // the same corner as `a_mate_found_through_a_pass_does_not_come_
        // back_as_one` and for the same reason, asked at a depth the term
        // moves: at six the pass is searched three plies shallower rather
        // than two. The clamp on the mate window is what holds the score
        // under the window a caller reads mates in, and a reduction that
        // varies must not be a way round it.
        let board = Board::from_fen("7k/5K1N/8/8/8/8/Q7/8 w - - 0 1").unwrap();
        let mut e = passing_adaptively(board);
        let beta = e.eval();
        let Ok(value) = e.alpha_beta(beta - 1, beta, 6, true, RootBounds::NEITHER) else {
            panic!("nothing was armed to abort this search");
        };
        assert!(
            value.score >= beta,
            "the pass did not fail high, so nothing was clamped: {}",
            value.score
        );
        assert!(
            value.score < CHECKMATE_THRESHOLD,
            "a mate proved only through a pass came back as one: {}",
            value.score
        );
    }

    #[test]
    fn a_side_holding_only_pawns_never_passes() {
        // the trebuchet again, and for the same reason: whoever is to move
        // loses, so passing is better there than every move there is and a
        // reduced search of one proves nothing about them. That is what
        // zugzwang is, and pawns and a king is the material it happens to.
        // The gate refuses the pass at every node of this tree, so the arm
        // searches what the reference searches, node for node.
        let fen = "8/8/8/4p3/4Pk2/3K4/8/8 w - - 0 1";
        let mut e = passing(Board::from_fen(fen).unwrap());
        let result = completed(e.search(7));
        let mut cold = reference(Board::from_fen(fen).unwrap());
        let expected = completed(cold.search(7));
        assert_eq!(e.nodes, cold.nodes);
        assert_eq!(result.score, expected.score);
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
        );
    }

    #[test]
    fn a_mate_only_a_move_refutes_survives_the_pass() {
        // hxg7+ Kxg7 Rxh7+ Kxh7 Qf6 mates, and white is a pawn and a rook
        // down along the way. Every node after the first is a node in
        // check, where there is no declining to move, and the sacrifices
        // are only answered by the moves that deliver them. A pass taken
        // in either place would answer those nodes from the material and
        // the mate would go with it.
        let fen = "r5rk/2p1Nppp/3p3P/pp2p1P1/4P3/2qnPQK1/8/R6R w - - 0 1";
        let mut e = passing(Board::from_fen(fen).unwrap());
        let result = completed(e.search(4));
        assert_eq!(result.checkmate_in(), Some(4));

        let mut cold = reference(Board::from_fen(fen).unwrap());
        let expected = completed(cold.search(4));
        assert_eq!(result.score, expected.score);
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
        );
    }

    #[test]
    fn a_mate_found_through_a_pass_does_not_come_back_as_one() {
        // Black's king stands in the corner with white's knight the only
        // square it can step to. Pass, and black has to take the knight,
        // after which Qh2 mates: the reduced search under the pass comes
        // back with a mate score. It is not a mate. White never had the
        // pass to play, so what was proved is that the position is very
        // good, and the node answers below the window a caller reads mates
        // in. The mate that is really there is found by searching the moves.
        //
        // Beta is the eval, which is the largest one the pass is tried
        // under, and that is what makes the mate the only score that can
        // come back: taking the knight puts white under beta, so nothing
        // short of the mate clears it. The node is asked for directly
        // because a mate invented here has to be caught where it is
        // invented. By the time the root has searched its moves the real
        // mate outscores the invented one and nothing above can tell them
        // apart.
        let board = Board::from_fen("7k/5K1N/8/8/8/8/Q7/8 w - - 0 1").unwrap();
        let mut e = passing(board);
        let beta = e.eval();
        let Ok(value) = e.alpha_beta(beta - 1, beta, 5, true, RootBounds::NEITHER) else {
            panic!("nothing was armed to abort this search");
        };
        assert!(
            value.score >= beta,
            "the pass did not fail high, so nothing was clamped: {}",
            value.score
        );
        assert!(
            value.score < CHECKMATE_THRESHOLD,
            "a mate proved only through a pass came back as one: {}",
            value.score
        );
    }

    /// One ply short of the fifty move horizon, and arranged so that the
    /// pass is the only way to reach it. Every move white has is a capture
    /// or a pawn move, which puts the counter back to nothing, so no line
    /// white can play reads a draw; the pass moves no piece and takes no
    /// pawn, so the counter runs on and the position under it is drawn.
    /// Whatever taint comes out of a node here came out of the pass.
    const ONLY_A_PASS_READS_THE_DRAW: &str = "1k6/8/8/8/8/5p1p/4P1PP/6NK w - - 99 60";

    #[test]
    fn a_cutoff_from_a_pass_carries_the_pass_taint() {
        // The pass runs the counter out, so what comes back is the draw,
        // which is a fact about the line and not about the position. Beta
        // is nothing, so that draw clears it and the node is answered from
        // the pass. The answer has to say what it depended on, or the
        // table stores a score as if the position were worth it whatever
        // the counter said.
        let board = Board::from_fen(ONLY_A_PASS_READS_THE_DRAW).unwrap();
        let mut e = passing(board);
        let Ok(value) = e.alpha_beta(-1, 0, 3, true, RootBounds::NEITHER) else {
            panic!("nothing was armed to abort this search");
        };
        assert_eq!(value, Value::tainted(0));
    }

    #[test]
    fn a_pass_that_failed_still_taints_the_node() {
        // The same position with beta at the eval, which is the largest one
        // the pass is tried under. The draw the pass reads is worth nothing
        // and beta is worth more, so the pass fails and the moves are
        // searched. It still read the draw on the way, and the node has to
        // carry that: white is winning here, so the score the node settles
        // on is one of its own moves and the taint is the pass's alone.
        let board = Board::from_fen(ONLY_A_PASS_READS_THE_DRAW).unwrap();
        let mut e = passing(board);
        let beta = e.eval();
        assert!(beta > 0, "the pass has to fail, so beta must beat a draw");
        let Ok(value) = e.alpha_beta(beta - 1, beta, 3, true, RootBounds::NEITHER) else {
            panic!("nothing was armed to abort this search");
        };
        assert!(value.tainted, "the failed pass left no taint behind it");
    }

    /// The child the reduction's seam is driven at: the pawn push from a
    /// position with three quiet moves and nothing to capture, so every
    /// leaf is one quiescence node and the counts below are exact.
    const REDUCIBLE_CHILD: &str = "8/8/8/8/8/8/2k4P/K7 w - - 0 1";

    /// An engine of the configuration given, stood on that child.
    fn at_reducible_child(config: SearchConfig) -> AlphaBeta {
        let mut e = AlphaBeta::with_config(
            Board::from_fen(REDUCIBLE_CHILD).unwrap(),
            TABLE_BYTES,
            config,
        );
        let m = play_named(&e.board, "h2h3");
        assert!(e.board.make_move(&m));
        e
    }

    /// The scout on its own: the zero width search of the child a ply
    /// shallower than the probe would be, as `windowed` asks it.
    /// What it costs and what it answers, from the parent's side.
    fn scout(e: &mut AlphaBeta, alpha: Score, depth: u8) -> (u64, Value) {
        let Ok(value) = e.alpha_beta(
            -alpha - 1,
            -alpha,
            depth - 1 - LATE_MOVE_REDUCTION,
            true,
            RootBounds::NEITHER,
        ) else {
            panic!("an unlimited search aborted");
        };
        (e.nodes, -value)
    }

    #[test]
    fn a_late_quiet_is_scouted_a_ply_shallower_and_answered_by_a_scout_that_fails_low() {
        // `windowed` driven at the child with the reduction asked for by
        // the flag rather than earned by a move count, so what is counted
        // is the seam and nothing else. Alpha stands well above anything
        // the move is worth, so the scout fails low, and the reduced call
        // then costs exactly the scout's nodes and answers with the scout's
        // value. The probe it stood in for is dearer, which is the saving.
        const DEPTH: u8 = 3;
        let mut oracle = at_reducible_child(SearchConfig::reference());
        let Ok(exact) = oracle.windowed(
            Score::MIN + 2,
            Score::MAX,
            DEPTH,
            true,
            0,
            RootBounds::BOTH,
            None,
        ) else {
            panic!("an unlimited search aborted");
        };
        let alpha = exact.score + 500;
        assert!(!super::is_mate(alpha));

        let mut alone = at_reducible_child(SearchConfig::reference());
        let (scout_nodes, scout_value) = scout(&mut alone, alpha, DEPTH);
        assert!(scout_value.score <= alpha, "the scout did not fail low");

        let mut e = at_reducible_child(SearchConfig::reference());
        let Ok(value) = e.windowed(
            alpha,
            alpha + 1,
            DEPTH,
            false,
            LATE_MOVE_REDUCTION,
            RootBounds::NEITHER,
            None,
        ) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, scout_nodes);
        assert_eq!(value, scout_value);

        let mut probe = at_reducible_child(SearchConfig::reference());
        let Ok(unreduced) =
            probe.windowed(alpha, alpha + 1, DEPTH, false, 0, RootBounds::NEITHER, None)
        else {
            panic!("an unlimited search aborted");
        };
        assert!(unreduced.score <= alpha);
        assert!(
            probe.nodes > scout_nodes,
            "the probe cost {} nodes against the scout's {}",
            probe.nodes,
            scout_nodes
        );
    }

    #[test]
    fn a_scout_that_fails_high_is_re_searched_at_full_depth() {
        // The same child under a window the move sits inside, so the scout
        // fails high and the move earns the depth it was denied. Reduced,
        // the call costs exactly what the scout costs and then what an
        // unreduced call costs on the table the scout left behind, which
        // is the second engine here, and it answers what the unreduced
        // call answers: the exact score, since the window is an open one
        // and the proof runs at the full window.
        const DEPTH: u8 = 3;
        let mut oracle = at_reducible_child(SearchConfig::reference());
        let Ok(exact) = oracle.windowed(
            Score::MIN + 2,
            Score::MAX,
            DEPTH,
            true,
            0,
            RootBounds::BOTH,
            None,
        ) else {
            panic!("an unlimited search aborted");
        };
        let alpha = exact.score - 500;
        let beta = exact.score + 50;
        assert!(!super::is_mate(alpha) && !super::is_mate(beta));

        let mut alone = at_reducible_child(SearchConfig::reference());
        let (scout_nodes, scout_value) = scout(&mut alone, alpha, DEPTH);
        assert!(scout_value.score > alpha, "the scout did not fail high");

        let mut then_probed = at_reducible_child(SearchConfig::reference());
        scout(&mut then_probed, alpha, DEPTH);
        let Ok(unreduced) =
            then_probed.windowed(alpha, beta, DEPTH, false, 0, RootBounds::NEITHER, None)
        else {
            panic!("an unlimited search aborted");
        };
        assert!(
            then_probed.nodes > scout_nodes,
            "nothing was searched after the scout"
        );

        let mut e = at_reducible_child(SearchConfig::reference());
        let Ok(value) = e.windowed(
            alpha,
            beta,
            DEPTH,
            false,
            LATE_MOVE_REDUCTION,
            RootBounds::NEITHER,
            None,
        ) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, then_probed.nodes);
        assert_eq!(value.score, unreduced.score);
        assert_eq!(value.score, exact.score);
    }

    #[test]
    fn a_node_in_check_searches_what_the_reference_searches() {
        // the rook checks along the file and white has seven evasions, four
        // king steps and three interpositions, every one of them quiet.
        // Asked at depth two the node extends to three, the floor, so a
        // reduction that ignored the check would scout the evasions after
        // the fourth, while the children stand at depth two and can reduce
        // nothing themselves. So the arm searches what the reference
        // searches, node for node, if and only if the node in check
        // declines to reduce; that it declines is
        // `late_move::tests::a_node_in_check_reduces_nothing`
        let fen = "4r2k/8/8/8/8/8/2Q2N2/4K3 w - - 0 1";
        let board = Board::from_fen(fen).unwrap();
        assert!(board.in_check());
        let evasions = board.evasions();
        assert!(evasions.len() > LATE_MOVE_THRESHOLD, "{}", evasions.len());
        assert!(evasions.iter().all(|m| m.capture.is_none()));

        let mut e = reducing(Board::from_fen(fen).unwrap());
        let Ok(value) = e.alpha_beta(
            -10_000,
            10_000,
            LATE_MOVE_MIN_DEPTH - 1,
            true,
            RootBounds::NEITHER,
        ) else {
            panic!("an unlimited search aborted");
        };
        let mut cold = reference(Board::from_fen(fen).unwrap());
        let Ok(expected) = cold.alpha_beta(
            -10_000,
            10_000,
            LATE_MOVE_MIN_DEPTH - 1,
            true,
            RootBounds::NEITHER,
        ) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, cold.nodes);
        assert_eq!(value, expected);
    }

    #[test]
    fn the_mate_window_searches_what_the_reference_searches() {
        // a zero width window inside the mate scores turns round at every
        // ply, so every node of the tree stands at one edge or the other
        // and none of them reduces: the arm searches what the reference
        // searches, node for node, where the same node under an ordinary
        // window reduces plenty. Depth five, so that the grandchildren,
        // which a child cutting off on its first move leaves searching
        // every move of theirs, stand at the floor with alpha the mate in
        // hand. That either edge stands the decision down is
        // `late_move::tests::the_mate_window_stands_the_reduction_down`
        let fen = SHARP_MIDDLEGAME;
        let mut e = reducing(Board::from_fen(fen).unwrap());
        let Ok(value) = e.alpha_beta(29_500, 29_501, 5, true, RootBounds::NEITHER) else {
            panic!("an unlimited search aborted");
        };
        let mut cold = reference(Board::from_fen(fen).unwrap());
        let Ok(expected) = cold.alpha_beta(29_500, 29_501, 5, true, RootBounds::NEITHER) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, cold.nodes);
        assert_eq!(value, expected);

        let mut e = reducing(Board::from_fen(fen).unwrap());
        assert!(e.alpha_beta(-1, 0, 5, true, RootBounds::NEITHER).is_ok());
        let mut cold = reference(Board::from_fen(fen).unwrap());
        assert!(cold.alpha_beta(-1, 0, 5, true, RootBounds::NEITHER).is_ok());
        assert!(
            e.nodes < cold.nodes,
            "nothing was reduced outside the mate window: {} against {}",
            e.nodes,
            cold.nodes
        );
    }

    #[test]
    fn reducing_looks_at_less_of_the_tree() {
        // the switch has to reach the search, as above
        let mut e = reducing(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(e.search(6));
        let mut cold = reference(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(cold.search(6));
        assert!(
            e.nodes < cold.nodes,
            "the reduction searched {} nodes against the reference's {}",
            e.nodes,
            cold.nodes
        );
    }

    #[test]
    fn a_dead_quiet_is_scouted_two_plies_shallower_and_answered_by_its_scout() {
        // the seam at the reduction the model asks for: `windowed` driven
        // with two plies rather than one, at the deep floor, where the
        // scout stands at depth one. Alpha is far above the position, the
        // scout fails low and is trusted, and the reduced call costs
        // exactly the two ply scout's nodes and answers with its value;
        // the one ply scout beside it is dearer, which is what the model
        // is spending its word on
        const DEPTH: u8 = DEEP_REDUCTION_MIN_DEPTH;
        let mut oracle = at_reducible_child(SearchConfig::reference());
        let Ok(exact) = oracle.windowed(
            Score::MIN + 2,
            Score::MAX,
            DEPTH,
            true,
            0,
            RootBounds::BOTH,
            None,
        ) else {
            panic!("an unlimited search aborted");
        };
        let alpha = exact.score + 500;
        assert!(!super::is_mate(alpha));

        let mut alone = at_reducible_child(SearchConfig::reference());
        let Ok(scout_value) = alone.alpha_beta(
            -alpha - 1,
            -alpha,
            DEPTH - 1 - DEEP_REDUCTION,
            true,
            RootBounds::NEITHER,
        ) else {
            panic!("an unlimited search aborted");
        };
        let scout_value = -scout_value;
        let scout_nodes = alone.nodes;
        assert!(scout_value.score <= alpha, "the scout did not fail low");

        let mut e = at_reducible_child(SearchConfig::reference());
        let Ok(value) = e.windowed(
            alpha,
            alpha + 1,
            DEPTH,
            false,
            DEEP_REDUCTION,
            RootBounds::NEITHER,
            None,
        ) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, scout_nodes);
        assert_eq!(value, scout_value);

        let mut shallower = at_reducible_child(SearchConfig::reference());
        let Ok(one_ply) = shallower.windowed(
            alpha,
            alpha + 1,
            DEPTH,
            false,
            LATE_MOVE_REDUCTION,
            RootBounds::NEITHER,
            None,
        ) else {
            panic!("an unlimited search aborted");
        };
        assert!(one_ply.score <= alpha);
        assert!(
            shallower.nodes > scout_nodes,
            "the one ply scout cost {} nodes against the two ply scout's {}",
            shallower.nodes,
            scout_nodes
        );
    }

    #[test]
    fn the_deep_reduction_looks_at_less_of_the_tree() {
        // the switch has to reach the search, measured against the rung
        // below it
        let mut e = deep_reducing(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(e.search(6));
        let mut one_ply = reducing(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(one_ply.search(6));
        assert!(
            e.nodes < one_ply.nodes,
            "the deep reduction searched {} nodes against the flat reduction's {}",
            e.nodes,
            one_ply.nodes
        );
    }

    #[test]
    fn the_pruning_reaches_the_search() {
        // the switch has to reach the search, measured against the rung
        // below it. A different tree rather than a smaller one: skipping a
        // move changes what the table holds and what the ordering does with
        // it, so the pruning can cost nodes as well as save them, and the
        // 2026-09-13 mobility refit made it cost them on three of the
        // bench's positions at this depth, this one among them
        let mut e = pruning(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(e.search(6));
        let mut deep = deep_reducing(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(deep.search(6));
        assert_ne!(
            e.nodes, deep.nodes,
            "the skip changed no node, so it did not reach the search"
        );
    }

    #[test]
    fn a_node_that_skips_its_dead_quiets_still_answers_from_the_front() {
        // the soundness shape the skip stands on: a skipped move can
        // lower a node's answer and never raise it, so at a position
        // whose answer the first moves carry, the search with the skip
        // answers what the search without it answers and pays fewer
        // nodes for it
        let mut e = pruning(Board::from_fen(fens::A_CAPTURE_AND_QUIETS).unwrap());
        let pruned = completed(e.search(6));
        let mut deep = deep_reducing(Board::from_fen(fens::A_CAPTURE_AND_QUIETS).unwrap());
        let whole = completed(deep.search(6));
        assert_eq!(pruned.score, whole.score);
        assert_eq!(pruned.best_move, whole.best_move);
        assert!(
            e.nodes <= deep.nodes,
            "the pruning searched {} nodes against the deep reduction's {}",
            e.nodes,
            deep.nodes
        );
    }

    #[test]
    fn the_quiet_memories_look_at_less_of_the_tree_for_the_same_answer() {
        // the switch has to reach the search, and this one owes more than
        // the shortcuts do: nothing here prunes, so the root's score stands
        // and only the tree that proved it may move
        let mut e = remembering(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        let result = completed(e.search(6));
        let mut cold = reference(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        let expected = completed(cold.search(6));
        assert_eq!(result.score, expected.score);
        assert!(
            e.nodes < cold.nodes,
            "the memories searched {} nodes against the reference's {}",
            e.nodes,
            cold.nodes
        );
    }

    #[test]
    fn the_quiet_memories_refuse_a_ply_past_the_rail() {
        // the killer table is as long as the rail, and every search stops
        // at the rail, so no node one reaches indexes past the table. The
        // ply is refused rather than indexed with anyway, which is what
        // makes the index safe here rather than at every caller, and this
        // is what says so, from a ply set by hand because a search no
        // longer arrives at one
        let mut e = remembering(Board::new());
        assert_eq!(e.memory_ply(), Some(0));
        e.board.line_ply = MAX_PLY as usize - 1;
        assert_eq!(e.memory_ply(), Some(MAX_PLY as usize - 1));
        e.board.line_ply = MAX_PLY as usize;
        assert_eq!(e.memory_ply(), None);

        // and the configuration is asked first, so the reference reads
        // nothing whatever the ply is
        let mut off = reference(Board::new());
        off.board.line_ply = 3;
        assert_eq!(off.memory_ply(), None);
    }

    #[test]
    fn a_cutoff_is_credited_to_the_side_that_played_it() {
        // at depth two every full width cutoff is a black reply refuting a
        // white root move, and from the start position every black reply is
        // quiet, so the history must be black's alone and the killers must
        // stand at ply one exactly. This is the search level check that the
        // update sites read the board after the move is unmade: the wrong
        // colour or the wrong ply lands the entries somewhere else
        let mut e = remembering(Board::new());
        completed(e.search(2));
        assert_eq!(e.ordering.history_total(Color::White), 0);
        assert!(e.ordering.history_total(Color::Black) > 0);
        assert_eq!(e.ordering.killers_at(0), [None; 2]);
        assert!(e.ordering.killers_at(1).iter().any(|k| k.is_some()));
        assert_eq!(e.ordering.killers_at(2), [None; 2]);
    }

    #[test]
    fn the_moves_a_node_tried_before_its_cutoff_reach_the_history() {
        // the malus is the half of the update the memories did not have
        // before, and nothing but a marked down move can put an entry
        // under zero, so a negative entry is the search level proof that
        // the move loop hands the table its own moves and not only the one
        // that cut. Most quiet cutoffs come with nothing searched before
        // them, so the position is one whose quiet band is contested
        let mut e = engine(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(e.search(3));
        let marked = e.ordering.history_marked_down(Color::White)
            + e.ordering.history_marked_down(Color::Black);
        assert!(marked > 0, "nothing was marked down");

        // and the reference neither reads nor writes the table, which is
        // what keeps its own pinned tree still under this change
        let mut cold = reference(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(cold.search(3));
        assert_eq!(cold.ordering.history_marked_down(Color::White), 0);
        assert_eq!(cold.ordering.history_marked_down(Color::Black), 0);
    }

    #[test]
    fn every_search_starts_with_the_quiet_memories_empty() {
        // a killer from the position before would order this one, and the
        // count would then say what the engine had been asked earlier
        // rather than what this position costs. Both entry points empty
        // them: the deepening loop is what a `go` runs, and `search` is
        // what a fixed depth measurement runs
        fn run(e: &mut AlphaBeta, deepened: bool) -> SearchResult {
            let depth = 5;
            if deepened {
                let options = SearchParameters::to_depth(depth);
                completed(e.iterative_deepening_search(options, |_, _, _, _| {}))
            } else {
                completed(e.search(depth))
            }
        }

        for deepened in [false, true] {
            let mut warm = remembering(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
            run(&mut warm, deepened);
            warm.board = Board::new();
            warm.clear_transpositions();
            let result = run(&mut warm, deepened);

            let mut cold = remembering(Board::new());
            let expected = run(&mut cold, deepened);
            assert_eq!(result.nodes, expected.nodes, "deepened: {deepened}");
            assert_eq!(result.score, expected.score, "deepened: {deepened}");
        }
    }

    /// Whether `played` is worth `score` to the side to move in `fen`.
    ///
    /// A root's answer is the best of its moves' negated replies, so a move
    /// that reaches it is one a correct search may return and a move that
    /// does not is one no correct search returns. Asked of an engine with
    /// nothing in its table, a ply below the root, so nothing the warm
    /// search saw can reach the answer.
    fn worth(fen: &str, played: &Play, score: Score) -> bool {
        let mut board = Board::from_fen(fen).unwrap();
        assert!(
            board.make_move(played),
            "{} is not legal in {}",
            played,
            fen
        );
        -completed(reference(board).search(5)).score == score
    }

    #[test]
    fn a_warm_cache_matches_cold_across_draw_context() {
        // the same pieces hash to the same key whatever the fifty move
        // counter says, so a search made a few plies from the draw fills
        // the table with scores true of that path only, and a fresh game
        // reaching the same position must not read them.
        //
        // Six of white's moves here are worth the answer (c3a1, c3e1, c3b2,
        // c3d2, c3b4 and c3d4, measured a ply down from cold engines). The
        // near-draw table reorders this search (20,579 nodes warm against
        // 16,179 cold at the last reading, where warming from an unrelated
        // position leaves the count identical to cold), so which of the six
        // comes back first is luck. So the move is asserted by what it is
        // worth rather than by name: a score read out of the draw context
        // would be one this position is not worth, and a move chosen on it
        // would not reach the answer
        let near_draw = "5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 96 112";
        let fresh = "5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 0 1";
        let mut warm = reference(Board::from_fen(near_draw).unwrap());
        completed(warm.search(6));
        warm.parse_fen(fresh).unwrap();
        let result = completed(warm.search(6));

        let mut cold = reference(Board::from_fen(fresh).unwrap());
        let expected = completed(cold.search(6));
        assert_eq!(result.score, expected.score);
        assert!(
            worth(fresh, &result.best_move, expected.score),
            "the warm search returned {}, which is not worth {}",
            result.best_move,
            expected.score
        );
    }

    #[test]
    fn a_skipping_search_matches_cold_across_draw_context_with_nothing_to_refuse() {
        // the skip policy keeps tainted scores out of the table instead of
        // refusing them on the way out, so it owes the same answer warm as
        // cold without the refusal ever firing. The root's answer slot is
        // the stated exception. On the reference, since the answer is only
        // owed there: the default's move may move with what the table
        // holds, and a reduction decided by the table's order did so here
        let near_draw = "5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 96 112";
        let fresh = "5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 0 1";
        let skipping = SearchConfig {
            taint: TaintPolicy::Skip,
            ..SearchConfig::reference()
        };
        let mut warm =
            AlphaBeta::with_config(Board::from_fen(near_draw).unwrap(), TABLE_BYTES, skipping);
        completed(warm.search(6));
        assert!(warm.ghi().skipped_stores > 0, "nothing was ever skipped");
        // the root's answer slot is the one stated exception, stored once
        // a search whatever its taint, so one search allows one
        assert!(
            warm.ghi().tainted_stores <= 1,
            "a tainted score was kept beyond the root's answer slot"
        );
        warm.parse_fen(fresh).unwrap();
        let result = completed(warm.search(6));

        let mut cold =
            AlphaBeta::with_config(Board::from_fen(fresh).unwrap(), TABLE_BYTES, skipping);
        let expected = completed(cold.search(6));
        assert_eq!(result.score, expected.score);
        assert!(
            worth(fresh, &result.best_move, expected.score),
            "the warm search returned {}, which is not worth {}",
            result.best_move,
            expected.score
        );
    }

    #[test]
    fn a_warm_cache_matches_a_cold_search() {
        // Searching a position with a cache warmed by unrelated positions must give the same
        // result as searching it with an empty cache
        let fens = [
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 10 10",
            "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
        ];
        let mut warm = reference(Board::new());
        for fen in fens {
            warm.parse_fen(fen).unwrap();
            completed(warm.search(5));
        }
        for fen in fens {
            let game = Board::from_fen(fen).unwrap();
            let mut cold = reference(game);
            let expected = completed(cold.search(5));
            warm.parse_fen(fen).unwrap();
            let result = completed(warm.search(5));
            assert_eq!(result.score, expected.score, "score differs for {}", fen);
            assert_eq!(
                format!("{}", result.best_move),
                format!("{}", expected.best_move),
                "best move differs for {}",
                fen
            );
        }
    }

    #[test]
    fn a_small_table_matches_a_large_table() {
        // a table small enough to force constant collisions must not change the result
        let fen = SHARP_MIDDLEGAME;
        let mut big = reference(Board::from_fen(fen).unwrap());
        let expected = completed(big.search(5));
        let mut small = AlphaBeta::with_config(
            Board::from_fen(fen).unwrap(),
            8 * 1024,
            SearchConfig::reference(),
        );
        let result = completed(small.search(5));
        assert_eq!(result.score, expected.score);
    }

    #[test]
    fn a_search_at_a_repetition_still_returns_a_move() {
        let game = Board::from_fen(fens::SHUFFLE).unwrap();
        let mut e = engine(game);
        for m in [
            "a8b8", "a1b1", "b8a8", "b1a1", "a8b8", "a1b1", "b8a8", "b1a1",
        ] {
            assert!(e.make_move_str(m), "failed to play {}", m);
        }
        assert!(e.board.is_repetition());
        assert!(matches!(e.search(3), SearchOutcome::Complete(_, _)));
    }

    #[test]
    fn a_new_game_forgets_the_previous_game() {
        let fen = SHARP_MIDDLEGAME;
        let mut e = engine(Board::from_fen(fen).unwrap());
        completed(e.search(4));
        assert!(
            e.transpositions.ordering_play(&e.board).is_some(),
            "nothing was stored"
        );
        assert_ne!(format!("{}", e.pv_line()), "");

        e.new_game();
        assert!(e.transpositions.ordering_play(&e.board).is_none());
        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_is_empty_without_a_cache_entry() {
        let game = Board::new();
        let e = engine(game);
        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_stops_at_a_repetition() {
        // a shuffle both sides are content with leaves the table holding a line
        // that goes round for ever. The line stops once the position comes back,
        // because from there it is a draw either side can take, rather than
        // reporting a continuation nobody would go on to play.
        let game = Board::from_fen(fens::SHUFFLE).unwrap();
        let mut e = engine(game);
        let cycle = ["a8b8", "a1b1", "b8a8", "b1a1"];
        let mut board = e.board.clone();
        for name in cycle.iter().cycle().take(16) {
            let play = play_named(&board, name);
            e.transpositions
                .record_best(&board, play, Value::clean(0), SEEDED_DEPTH);
            assert!(board.make_move(&play), "failed to play {}", name);
        }

        assert_eq!(format!("{}", e.pv_line()), "a8b8 a1b1 b8a8 b1a1");
    }

    #[test]
    fn the_pv_line_stops_when_the_fifty_move_counter_runs_out() {
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 99 112").unwrap();
        let mut e = engine(game);
        let mut board = e.board.clone();
        for name in ["c3d4", "f8g8"] {
            let play = play_named(&board, name);
            e.transpositions
                .record_best(&board, play, Value::clean(0), SEEDED_DEPTH);
            assert!(board.make_move(&play), "failed to play {}", name);
        }
        assert!(board.fifty_move_expired());

        // the first move draws by the fifty move rule, so the reply the table
        // holds is one the game never gets to
        assert_eq!(format!("{}", e.pv_line()), "c3d4");
    }

    #[test]
    fn the_pv_line_does_not_follow_a_move_which_is_illegal_here() {
        // two positions which hash to the same key share an entry, so the move
        // a probe comes back with is not always a move of the position asked
        // about
        let mut e = engine(Board::new());
        let a2 = 8;
        let a5 = 32;
        let colliding = Play::new(a2, a5, None, None, false, false);
        e.transpositions
            .record_best(&e.board, colliding, Value::clean(0), SEEDED_DEPTH);

        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_does_not_follow_a_quiescence_entry() {
        // quiescence looks at captures and promotions alone, so its move is
        // fit for ordering the next search and not for saying what the engine
        // means to play. Its entries are the depth zero ones, and that is
        // what the line walk refuses
        let mut e = engine(Board::new());
        let play = play_named(&e.board, "e2e4");
        e.transpositions
            .record_best(&e.board, play, Value::clean(0), 0);

        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_does_not_follow_a_move_which_leaves_the_king_in_check() {
        // moves are generated pseudo legally, so a pinned piece's move is in
        // the list for this position and still cannot be played. Asking whether
        // the move belongs to this position is not enough on its own, which is
        // why the walk goes on to check that making it succeeds.
        let board = Board::from_fen("4r2k/8/8/8/8/8/4N3/4K3 w - - 0 1").unwrap();
        let mut e = engine(board);
        let pinned = play_named(&e.board, "e2d4");
        e.transpositions
            .record_best(&e.board, pinned, Value::clean(0), SEEDED_DEPTH);

        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_is_bounded_by_the_ply_rail() {
        // a line longer than the rail, laid down a ply at a time. Each ply
        // takes a move that neither repeats a position nor lets the fifty
        // move counter run out, which are the two things the line walk
        // stops at of its own accord, so nothing but the bound can end this
        // one. A pawn move is preferred wherever there is one, because that
        // is what resets the counter, and the thirty two of them are spread
        // far enough through this to keep the rest of it inside the rule
        let mut e = engine(Board::new());
        let mut board = e.board.clone();
        let wanted = super::MAX_PLY as usize + 4;
        for ply in 0..wanted {
            let moves = board.generate_moves();
            let mut chosen: Option<Play> = None;
            for pawns_first in [true, false] {
                for m in &moves {
                    if (board.get_piece_index(m.from) == Some(Piece::Pawn)) != pawns_first {
                        continue;
                    }
                    if !board.make_move(m) {
                        continue;
                    }
                    let carries_on = !board.has_repeated() && !board.fifty_move_expired();
                    board.undo_move();
                    if carries_on {
                        chosen = Some(*m);
                        break;
                    }
                }
                if chosen.is_some() {
                    break;
                }
            }
            let play =
                chosen.unwrap_or_else(|| panic!("nothing carries the line on at ply {}", ply));
            e.transpositions
                .record_best(&board, play, Value::clean(0), SEEDED_DEPTH);
            assert!(board.make_move(&play), "failed to play {}", play);
        }

        assert_eq!(e.pv_line().line.len(), super::MAX_PLY as usize);
    }

    #[test]
    fn quiescence_resolves_captures_past_the_depth_it_used_to_stop_at() {
        // the old cap was twenty plies from the root, so no line could
        // report a selective depth past it. Each pruning change has moved
        // which depth clears it with room to spare; ten reaches twenty
        // seven under the late move reductions, where eight and nine clear
        // by a single ply
        let mut e = engine(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        let result = completed(e.search(10));
        assert!(
            result.selective_depth > 20,
            "quiescence stopped at {} plies",
            result.selective_depth
        );
    }

    #[test]
    fn a_stopped_search_does_not_poison_the_cache() {
        let fen = SHARP_MIDDLEGAME;
        let game = Board::from_fen(fen).unwrap();
        let mut cold = reference(game);
        let expected = completed(cold.search(6));

        // a search whose clock has run out stops immediately, and must not
        // leave partial results in the table which change the outcome of the
        // next search
        let game = Board::from_fen(fen).unwrap();
        let mut e = reference(game);
        assert!(matches!(
            e.search_within(6, already_spent()),
            SearchOutcome::Aborted(_)
        ));

        let result = completed(e.search(6));
        assert_eq!(result.score, expected.score);
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
        );
    }
}

/// The residual sampler seen from the search: that it is off unless it is
/// asked for, and that what it records describes the nodes the shortcuts
/// answered and the candidates the margin was measured against. What the
/// samples are worth is the residuals command's business.
#[cfg(test)]
mod sampling {
    use super::{
        AlphaBeta, Board, Engine, REVERSE_FUTILITY_MARGIN, REVERSE_FUTILITY_MAX_DEPTH, RootBounds,
        Score, SearchConfig, SearchParameters, Taint,
    };
    use crate::board::fens::SHARP_MIDDLEGAME;
    use crate::recorder::{Sampled, Sampler, Window};
    use crate::residual::{Sample, Shortcut, sample_key};
    use pretty_assertions::assert_eq;

    const TABLE_BYTES: usize = 1024 * 1024;

    fn engine(fen: &str) -> AlphaBeta {
        AlphaBeta::with_table_bytes(Board::from_fen(fen).unwrap(), TABLE_BYTES)
    }

    /// The shortcut frame driven on its own, with the bounds named rather
    /// than whatever a search happened to be carrying. What comes back is
    /// what the sampler recorded of it.
    ///
    /// The only way to hold the beta column to the beta the gate read. A
    /// sample cannot be asked: its evaluation column is measured against the
    /// same value the beta column states, so substituting another bound
    /// moves both together and every identity between them survives. The
    /// bound has to come from outside, which is what this does.
    fn shortcut_at(config: SearchConfig, alpha: Score, beta: Score, depth: u8) -> Vec<Sample> {
        let mut e = AlphaBeta::with_config(
            Board::from_fen(SHARP_MIDDLEGAME).unwrap(),
            TABLE_BYTES,
            config,
        );
        e.arm(Sampler::<Sample>::every(1));
        let mut taint = Taint::default();
        let Ok(answered) = e.shortcuts(
            alpha,
            beta,
            depth,
            false,
            true,
            RootBounds::NEITHER,
            &mut taint,
            &mut None,
        ) else {
            panic!("nothing here searches under a limit, so nothing can abort");
        };
        assert!(answered.is_some(), "no shortcut fired at depth {}", depth);
        collected(&mut e).taken
    }

    /// The gate the whole design rests on. An engine nobody asked samples of
    /// holds no sampler, and the bench's pinned counts beside this say the
    /// search it runs is the search it ran before there was one.
    #[test]
    fn an_engine_samples_nothing_until_it_is_asked_to() {
        let mut e = engine(SHARP_MIDDLEGAME);
        assert!(e.sampler.is_none());
        let reference =
            AlphaBeta::with_config(Board::new(), TABLE_BYTES, SearchConfig::reference());
        assert!(reference.sampler.is_none());
        e.search(5);
        assert!(e.disarm::<Sample>().is_none());
    }

    /// What every test below asks of an engine once it has searched: the
    /// sampler back, emptied into what it collected.
    fn collected(e: &mut AlphaBeta) -> Sampled<Sample> {
        e.disarm::<Sample>()
            .expect("a sampler was installed")
            .drain()
    }

    /// The one sample of a kind in what was taken. Drain hands samples back
    /// in key order, which says nothing, so a test reads a row by its kind.
    fn one_of(taken: &[Sample], kind: Shortcut) -> &Sample {
        let mut of_kind = taken.iter().filter(|s| s.kind == kind);
        let sample = of_kind
            .next()
            .unwrap_or_else(|| panic!("nothing taken for {}", kind.word()));
        assert!(
            of_kind.next().is_none(),
            "more than one {} sample",
            kind.word()
        );
        sample
    }

    #[test]
    fn every_sample_describes_a_node_a_hook_offered() {
        const DEPTH: u8 = 5;
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        e.search(DEPTH);
        let sampled = collected(&mut e);
        assert!(!sampled.taken.is_empty(), "the hooks offered nothing");
        for sample in &sampled.taken {
            // the fen is the whole point: a sample nothing can search again
            // measures nothing
            Board::from_fen(&sample.fen)
                .unwrap_or_else(|e| panic!("{} does not parse: {}", sample.fen, e));
            assert!(Shortcut::KINDS.contains(&sample.kind), "{:?}", sample);
            // a node has less depth left than the root it hangs under, and
            // the root deepens by one when it is in check
            assert!(sample.depth >= 1, "{:?}", sample);
            assert!(sample.depth <= DEPTH + 1, "{:?}", sample);
        }
    }

    /// The decision columns, taken from the node the shortcut answered
    /// rather than worked out afterwards. A live claim clears the beta
    /// beside it, because that is what a shortcut fires on; a shadow claim
    /// need not, which is the point of it, and its evaluation column stands
    /// at or above beta because that is what a candidate is. The fifty move
    /// column agrees with the fen it was taken from.
    ///
    /// The identity ties the claim, the evaluation column and the depth
    /// together: reverse futility claims `eval - margin * depth` and fires
    /// when that clears beta, so `claimed - beta` and
    /// `eval_beta - margin * depth` are the same number written two ways,
    /// and a shadow row claims the same expression. It catches a column
    /// built from the wrong evaluation or scaled by the wrong depth. It
    /// cannot catch the wrong bound, since both sides are measured against
    /// whatever the beta column states; that is what
    /// `the_recorded_beta_is_the_one_the_gate_cleared` is for.
    #[test]
    fn every_sample_carries_the_decision_it_was_taken_at() {
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        e.search(5);
        for sample in collected(&mut e).taken {
            match sample.kind {
                Shortcut::ReverseFutility | Shortcut::NullMove => {
                    assert!(sample.claimed >= sample.beta, "{:?}", sample);
                }
                Shortcut::ShadowFutility => {
                    assert!(sample.eval_beta >= 0, "{:?}", sample);
                }
            }
            let board = Board::from_fen(&sample.fen).expect("the fen parses");
            assert_eq!(sample.halfmove, board.halfmove_clock(), "{:?}", sample);
            if matches!(
                sample.kind,
                Shortcut::ReverseFutility | Shortcut::ShadowFutility
            ) {
                assert_eq!(
                    i32::from(sample.claimed) - i32::from(sample.beta),
                    sample.eval_beta - i32::from(REVERSE_FUTILITY_MARGIN) * i32::from(sample.depth),
                    "{:?}",
                    sample
                );
            }
        }
    }

    /// The rate is a key and not a counter: every node recorded at a rate of
    /// four keys into the first quarter of the range, and a sample is enough
    /// to recover the key it was drawn by.
    #[test]
    fn a_rate_records_only_the_nodes_its_keys_fall_under() {
        const EVERY: u32 = 4;
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(EVERY));
        e.search(5);
        let taken = collected(&mut e).taken;
        assert!(!taken.is_empty(), "the rate turned everything away");
        for sample in taken {
            let board = Board::from_fen(&sample.fen).expect("the fen parses");
            assert!(
                sample_key(board.key, sample.kind, sample.depth) <= u64::MAX / u64::from(EVERY),
                "{:?}",
                sample
            );
        }
    }

    /// The beta a row states is the beta the gate cleared, and the window
    /// beside it is read from the alpha and the beta the node really had.
    /// Both shortcuts, because they are two call sites and a bound can go
    /// astray at one of them.
    ///
    /// The evaluation is read from the engine, so what the columns are held
    /// to is a number the test knows before the shortcut runs: reverse
    /// futility claims a margin under it, and the distance recorded is the
    /// whole gap to the beta named here.
    #[test]
    fn the_recorded_beta_is_the_one_the_gate_cleared() {
        let eval = engine(SHARP_MIDDLEGAME).eval();

        // the margin at depth one claims `eval - 100`, which clears a beta
        // two hundred under the evaluation. The fired node arrives under
        // the live kind and the shadow, so the live row is picked out by
        // its kind rather than its place
        let beta = eval - 200;
        let taken = shortcut_at(SearchConfig::default(), beta - 500, beta, 1);
        assert_eq!(taken.len(), 2);
        let fired = one_of(&taken, Shortcut::ReverseFutility);
        assert_eq!(fired.beta, beta);
        assert_eq!(fired.eval_beta, 200);
        assert_eq!(fired.claimed, eval - REVERSE_FUTILITY_MARGIN);
        assert_eq!(fired.window, Window::Open);
        // and the window follows the bounds rather than the shortcut
        let narrow = shortcut_at(SearchConfig::default(), beta - 1, beta, 1);
        let fired = one_of(&narrow, Shortcut::ReverseFutility);
        assert_eq!(fired.beta, beta);
        assert_eq!(fired.window, Window::Zero);

        // the pass, with the margin switched off so that nothing answers the
        // node before it does
        let passing = SearchConfig {
            reverse_futility: false,
            ..SearchConfig::default()
        };
        let beta = eval - 600;
        let taken = shortcut_at(passing, beta - 500, beta, 3);
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].kind, Shortcut::NullMove);
        assert_eq!(taken[0].beta, beta);
        assert_eq!(taken[0].eval_beta, 600);
        assert_eq!(taken[0].window, Window::Open);
        let narrow = shortcut_at(passing, beta - 1, beta, 3);
        assert_eq!(narrow[0].beta, beta);
        assert_eq!(narrow[0].window, Window::Zero);
    }

    /// The exemption at this gate: the same beta answers the node with
    /// neither bound marked and answers nothing when it is still the
    /// root's own. The refusal comes before the eval is read, so the node
    /// spends no search and the sampler is offered no row.
    #[test]
    fn a_beta_that_is_still_the_roots_answers_no_shortcut() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        // past the margin's depth, so the pass is what answers, and six
        // hundred under the evaluation, so it answers comfortably. The
        // pass's own reduced search is sampled for the shortcuts it takes,
        // so the row looked for here is named rather than counted
        let beta = eval - 600;
        let taken = shortcut_at(SearchConfig::default(), beta - 500, beta, 5);
        assert!(taken.iter().any(|s| s.kind == Shortcut::NullMove));

        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        let mut taint = Taint::default();
        let Ok(answered) = e.shortcuts(
            beta - 500,
            beta,
            5,
            false,
            true,
            RootBounds {
                alpha: false,
                beta: true,
            },
            &mut taint,
            &mut None,
        ) else {
            panic!("nothing here searches under a limit, so nothing can abort");
        };
        assert!(answered.is_none(), "a shortcut answered the root's beta");
        assert_eq!(e.nodes, 0, "the refusal searched something");
        assert!(collected(&mut e).taken.is_empty());
    }

    /// The seam the shadow exists for: a candidate the margin declines is
    /// recorded all the same. The live rows cannot show one, since every
    /// node they describe cleared the margin; only the shadow sees the
    /// candidates a smaller margin would add.
    #[test]
    fn a_candidate_under_the_margin_is_shadowed_and_not_answered() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        // at depth one the margin claims `eval - 100`, so a beta fifty
        // under the evaluation is a candidate the test declines
        let beta = eval - 50;
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        let mut taint = Taint::default();
        let Ok(answered) = e.shortcuts(
            beta - 500,
            beta,
            1,
            false,
            true,
            RootBounds::NEITHER,
            &mut taint,
            &mut None,
        ) else {
            panic!("nothing here searches under a limit, so nothing can abort");
        };
        assert!(answered.is_none(), "the margin fired under its floor");
        let taken = collected(&mut e).taken;
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].kind, Shortcut::ShadowFutility);
        assert_eq!(taken[0].beta, beta);
        assert_eq!(taken[0].eval_beta, 50);
        // the claim is the margin's own expression, and here it sits under
        // beta, which no live row's can
        assert_eq!(taken[0].claimed, eval - REVERSE_FUTILITY_MARGIN);
        assert!(taken[0].claimed < taken[0].beta);
    }

    /// The eval gate: a node the evaluation leaves below beta is no
    /// candidate, because no margin schedule at or above nothing can fire
    /// on it, and the shadow does not record it.
    #[test]
    fn a_node_below_beta_is_not_a_candidate() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        let beta = eval + 50;
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        let mut taint = Taint::default();
        let Ok(answered) = e.shortcuts(
            beta - 500,
            beta,
            1,
            false,
            true,
            RootBounds::NEITHER,
            &mut taint,
            &mut None,
        ) else {
            panic!("nothing here searches under a limit, so nothing can abort");
        };
        assert!(answered.is_none());
        assert!(collected(&mut e).taken.is_empty());
    }

    /// The evaluation the shortcut frame hands back to the move loop,
    /// which seeds the memo the late move decision reads. It comes off the
    /// memoised door and the module's own reads `eval::eval` directly, so
    /// a node the quiet futility rule decides would be deciding on a
    /// different number if the two ever parted.
    #[test]
    fn the_evaluation_handed_to_the_loop_is_the_direct_one() {
        let mut e =
            AlphaBeta::with_table_bytes(Board::from_fen(SHARP_MIDDLEGAME).unwrap(), TABLE_BYTES);
        assert!(e.config.quiet_futility, "the default carries the rule");
        let direct = i64::from(crate::eval::eval(&e.board));
        // beta at the evaluation, so the gates pass and the margin's floor
        // a pawn under it does not answer the node: what is read is what
        // the loop would have been handed
        let beta = direct as Score;
        let mut taint = Taint::default();
        let mut eval = None;
        let Ok(answered) = e.shortcuts(
            beta - 1,
            beta,
            1,
            false,
            true,
            RootBounds::NEITHER,
            &mut taint,
            &mut eval,
        ) else {
            panic!("nothing here searches under a limit, so nothing can abort");
        };
        assert!(answered.is_none(), "the margin answered the node");
        assert_eq!(eval, Some(direct));
    }

    /// A fired candidate is two rows, the live kind's and the shadow's,
    /// claiming the same number against the same beta. The shadow
    /// population contains the fired nodes, so the two kinds agree
    /// wherever they overlap and the columns keep their meanings.
    #[test]
    fn a_fired_candidate_is_shadowed_with_the_same_claim() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        let beta = eval - 200;
        let taken = shortcut_at(SearchConfig::default(), beta - 500, beta, 1);
        assert_eq!(taken.len(), 2);
        let live = one_of(&taken, Shortcut::ReverseFutility);
        let shadow = one_of(&taken, Shortcut::ShadowFutility);
        assert_eq!(shadow.claimed, live.claimed);
        assert_eq!(shadow.beta, live.beta);
        assert_eq!(shadow.eval_beta, live.eval_beta);
        assert_eq!(shadow.window, live.window);
        assert_eq!(shadow.fen, live.fen);
    }

    /// The margin's depth gate bounds the shadow too. The node here is past
    /// it, so the pass answers and no shadow row is taken at its depth; the
    /// pass's reduced search runs under the same sampler, which is where
    /// every shallower sample comes from.
    #[test]
    fn the_shadow_keeps_to_the_margins_depths() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        let beta = eval - 600;
        let taken = shortcut_at(SearchConfig::default(), beta - 500, beta, 5);
        assert!(
            taken
                .iter()
                .any(|s| s.kind == Shortcut::NullMove && s.depth == 5),
            "the pass did not answer the node"
        );
        for sample in &taken {
            if sample.kind != Shortcut::NullMove {
                assert!(sample.depth <= REVERSE_FUTILITY_MAX_DEPTH, "{:?}", sample);
            }
        }
    }

    /// The window a real search hands the hook. Neither shortcut reads the
    /// width: the margin and the pass both read the eval against beta, so
    /// an open window node can be answered by either. Almost every node
    /// they answer carries a zero window all the same, because that is what
    /// the tree under a scout looks like, and the two counts here are this
    /// tree's numbers rather than rules.
    /// `the_recorded_beta_is_the_one_the_gate_cleared` drives the hook
    /// directly with both windows and pins the open column.
    #[test]
    fn the_windows_a_search_records_are_mostly_the_zero_ones() {
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        e.search(6);
        let taken = collected(&mut e).taken;
        assert!(!taken.is_empty());
        let open = |kind| {
            taken
                .iter()
                .filter(|s| s.kind == kind && s.window == Window::Open)
                .count()
        };
        assert_eq!(
            (open(Shortcut::ReverseFutility), open(Shortcut::NullMove)),
            (1, 2),
            "the open windows the two shortcuts answer moved"
        );
    }

    /// Every kind reaches the hook, not only whichever fires first. A kind
    /// that stopped being recorded would otherwise show up as a thinner
    /// distribution rather than as a failure.
    #[test]
    fn all_kinds_are_recorded() {
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        e.search(6);
        let sampled = collected(&mut e);
        for kind in Shortcut::KINDS {
            assert!(
                sampled.taken.iter().any(|s| s.kind == kind),
                "nothing recorded for {}",
                kind.word()
            );
        }
    }

    /// A key and not a draw, so a distribution printed today is printed
    /// again tomorrow by the same command.
    #[test]
    fn two_runs_of_the_same_search_record_the_same_samples() {
        let run = || {
            let mut e = engine(SHARP_MIDDLEGAME);
            e.arm(Sampler::<Sample>::every(7));
            e.iterative_deepening_search(SearchParameters::to_depth(5), |_, _, _, _| {});
            collected(&mut e)
        };
        assert_eq!(run(), run());
    }

    /// The cap holds and says how much it dropped, which is what keeps a
    /// long run at a low rate from asking for the memory of every fen in the
    /// tree. What survives it is the reservoir's business, tested there.
    #[test]
    fn a_search_past_the_cap_stops_growing_and_counts_the_rest() {
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::with_cap(1, 4));
        e.search(5);
        let sampled = collected(&mut e);
        assert_eq!(sampled.taken.len(), 4);
        assert!(sampled.overflowed > 0);
    }

    /// The margin each shortcut is betting on, recorded at the node it fired
    /// at. The pass gates on the evaluation standing at or above beta, so it
    /// cannot record a negative distance; the margin gates on the evaluation
    /// standing a whole margin above it, so it cannot record less than that.
    /// The weaker bound would pass on a column that had lost the depth it is
    /// scaled by.
    #[test]
    fn the_recorded_distance_is_the_evaluation_over_beta() {
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        e.search(5);
        for sample in collected(&mut e).taken {
            let floor = match sample.kind {
                Shortcut::ReverseFutility => {
                    i32::from(REVERSE_FUTILITY_MARGIN) * i32::from(sample.depth)
                }
                // the pass and the shadow both gate on the evaluation
                // standing at or above beta and nothing more
                Shortcut::NullMove | Shortcut::ShadowFutility => 0,
            };
            assert!(sample.eval_beta >= floor, "{:?} under {}", sample, floor);
        }
    }
}

/// The cutoff census seen from the search: that it is off unless it is
/// asked for, and that a row reads the node as it stood at the moment it
/// answered. The recorder is driven directly here, with the memories
/// taught by hand, which is the only way to hold a row's history column to
/// a history the test chose. What the rows are worth is the cutoffs
/// command's business.
#[cfg(test)]
mod cutoffs {
    use super::{AlphaBeta, Board, Score, SearchConfig};
    use crate::board::fens::SHARP_MIDDLEGAME;
    use crate::census::{self, Class, Cutting, Table};
    use crate::play::Play;
    use crate::recorder::{Sampler, Window};
    use pretty_assertions::assert_eq;

    const TABLE_BYTES: usize = 1024 * 1024;

    fn engine(fen: &str) -> AlphaBeta {
        let mut e = AlphaBeta::with_table_bytes(Board::from_fen(fen).unwrap(), TABLE_BYTES);
        e.arm(Sampler::<census::Event>::every(1));
        e
    }

    /// A from and to square pair no move in the list uses, for teaching
    /// the history an entry the list cannot read. The table is butterfly
    /// indexed, so the squares are the whole of what makes an entry.
    fn unmade_journey(moves: &[Play]) -> Play {
        (0u8..64)
            .flat_map(|from| (0u8..64).map(move |to| (from, to)))
            .find(|(from, to)| from != to && !moves.iter().any(|m| m.from == *from && m.to == *to))
            .map(|(from, to)| Play::new(from, to, None, None, false, false))
            .expect("a list cannot hold every journey")
    }

    /// Two quiet moves of the position, for teaching the memories.
    fn quiets(e: &AlphaBeta) -> (Play, Play) {
        let moves = e.board.generate_moves();
        let mut quiets = moves
            .iter()
            .filter(|m| m.capture.is_none() && m.promote.is_none());
        let first = *quiets.next().expect("a quiet move");
        let second = *quiets.next().expect("another quiet move");
        (first, second)
    }

    /// The same gate the residual sampler stands behind: an engine nobody
    /// asked a census of holds none, and the pinned bench counts beside
    /// this say the search it runs is the search it ran before there was
    /// a census at all.
    #[test]
    fn an_engine_records_no_census_until_it_is_asked_to() {
        let mut e =
            AlphaBeta::with_table_bytes(Board::from_fen(SHARP_MIDDLEGAME).unwrap(), TABLE_BYTES);
        assert!(e.census.is_none());
        let reference =
            AlphaBeta::with_config(Board::new(), TABLE_BYTES, SearchConfig::reference());
        assert!(reference.census.is_none());
        e.search(4);
        assert!(e.disarm::<census::Event>().is_none());
    }

    /// A killer cutting at index 1, with the memories taught by hand: the
    /// row says index 1 and class killer, its history is the table's entry
    /// for the move, and the largest history among the generated quiets
    /// stands beside it as the denominator.
    #[test]
    fn a_row_reads_the_memories_as_they_stood_at_the_cutoff() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (killer, cool) = quiets(&e);
        let color = e.board.active_color;
        // taught at another ply, so what makes the class a killer is the
        // slot at this one; its history entry is the larger of the two
        e.ordering.cutoff(color, &cool, &[], 1, 5);
        e.ordering.cutoff(color, &killer, &[], 0, 4);
        let moves = e.board.generate_moves();
        let (alpha, beta): (Score, Score) = (10, 11);
        e.census_event(
            3,
            alpha,
            beta,
            false,
            &moves,
            2,
            true,
            Some(0),
            Table::Miss,
            e.nodes,
            Some(Cutting {
                play: &killer,
                reduced: false,
                table: false,
            }),
        );
        let sampled = e
            .disarm::<census::Event>()
            .expect("a census was installed")
            .drain();
        assert_eq!(sampled.taken.len(), 1);
        assert_eq!(sampled.events, 1);
        let row = &sampled.taken[0];
        let cut = row.cut.as_ref().expect("the node cut");
        assert_eq!(cut.index, 1);
        assert_eq!(cut.class, Class::Killer);
        assert_eq!(cut.history, 16);
        assert!(!cut.reduced);
        assert_eq!(row.history_max, 25);
        assert_eq!(row.generated, moves.len());
        assert_eq!(row.searched, 2);
        assert_eq!(row.window, Window::Zero);
        assert!(!row.in_check);
        assert!(row.quiets_scored);
        assert_eq!(row.tt, Table::Miss);
        assert_eq!(row.fen, e.board.to_fen());
        // the eval column is computed at record time and exact
        assert_eq!(
            row.eval_beta,
            i32::from(crate::eval::eval(&e.board)) - i32::from(beta)
        );
        assert_eq!(row.cost, 0);
    }

    /// A cutting move the table has marked down: the row prints the signed
    /// entry, and its denominator is the largest clamped at zero, which is
    /// zero when every quiet in the list has been marked down.
    #[test]
    fn a_marked_down_cutting_move_is_read_against_no_denominator() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (cut, _) = quiets(&e);
        let color = e.board.active_color;
        let moves = e.board.generate_moves();
        // the move that took the bonus makes a journey no move here makes,
        // so nothing in the list holds the entry it landed on and every
        // quiet in the list is marked down
        let elsewhere = unmade_journey(&moves);
        let marked: Vec<Play> = moves
            .iter()
            .filter(|m| m.capture.is_none() && m.promote.is_none())
            .copied()
            .collect();
        e.ordering.cutoff(color, &elsewhere, &marked, 1, 4);
        e.census_event(
            3,
            10,
            11,
            false,
            &moves,
            2,
            true,
            Some(0),
            Table::Miss,
            e.nodes,
            Some(Cutting {
                play: &cut,
                reduced: false,
                table: false,
            }),
        );
        let sampled = e
            .disarm::<census::Event>()
            .expect("a census was installed")
            .drain();
        let row = &sampled.taken[0];
        let cutting = row.cut.as_ref().expect("the node cut");
        assert_eq!(cutting.class, Class::Quiet);
        assert_eq!(cutting.history, -16);
        assert_eq!(row.history_max, 0);
        assert!(cutting.history <= row.history_max);
    }

    /// The table's move cutting before anything was generated: index 0,
    /// class table whatever the move is, and a row that says the node
    /// holds no list.
    #[test]
    fn a_table_move_cutoff_is_recorded_with_nothing_generated() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let take = *e
            .board
            .generate_moves()
            .iter()
            .find(|m| m.capture.is_some())
            .expect("a capture");
        e.census_event(
            4,
            10,
            11,
            false,
            &[],
            1,
            false,
            None,
            Table::Move,
            e.nodes,
            Some(Cutting {
                play: &take,
                reduced: false,
                table: true,
            }),
        );
        let sampled = e
            .disarm::<census::Event>()
            .expect("a census was installed")
            .drain();
        let row = &sampled.taken[0];
        let cut = row.cut.as_ref().expect("the node cut");
        assert_eq!(cut.index, 0);
        assert_eq!(cut.class, Class::Table);
        // a capture reads no history, however it is classed
        assert_eq!(cut.history, 0);
        assert_eq!(row.generated, 0);
        assert_eq!(row.searched, 1);
        assert_eq!(row.history_max, 0);
        assert_eq!(row.tt, Table::Move);
    }

    /// A node the loop finished: no cutting move, and the rest of the
    /// portrait still there, the quiets' largest history included.
    #[test]
    fn a_held_node_records_no_cutting_move() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (taught, _) = quiets(&e);
        e.ordering.cutoff(e.board.active_color, &taught, &[], 0, 3);
        let moves = e.board.generate_moves();
        e.census_event(
            2,
            -50,
            60,
            false,
            &moves,
            moves.len(),
            true,
            Some(0),
            Table::ScoreOnly,
            e.nodes,
            None,
        );
        let sampled = e
            .disarm::<census::Event>()
            .expect("a census was installed")
            .drain();
        let row = &sampled.taken[0];
        assert!(row.cut.is_none());
        assert_eq!(row.searched, moves.len());
        assert_eq!(row.window, Window::Open);
        assert_eq!(row.history_max, 9);
        assert_eq!(row.tt, Table::ScoreOnly);
    }
}

/// The reduction ledger seen from the search: that it is off unless it is
/// asked for, and that a row reads the decision as the node made it. The
/// two halves of the recorder are driven directly, the staging at the
/// parent and `windowed` at the child, with the memories taught by hand
/// for the census tests' reason. What the rows are worth is the
/// reductions command's business.
#[cfg(test)]
mod reductions {
    use super::{AlphaBeta, Board, RootBounds, Score};
    use crate::board::fens::SHARP_MIDDLEGAME;
    use crate::census::Table;
    use crate::late_move;
    use crate::play::Play;
    use crate::recorder::{Sampler, Window};
    use crate::reduction::{self, Scout};
    use pretty_assertions::assert_eq;

    const TABLE_BYTES: usize = 1024 * 1024;

    fn engine(fen: &str) -> AlphaBeta {
        let mut e = AlphaBeta::with_table_bytes(Board::from_fen(fen).unwrap(), TABLE_BYTES);
        e.arm(Sampler::<reduction::Event>::every(1));
        e
    }

    /// The staging the move loop would hand the scout, built by hand:
    /// these tests drive the recorder without a move loop. The features
    /// read neither the depth nor the bounds, so both stand at nothing.
    fn staged(e: &AlphaBeta, m: &Play, searched: usize, ply: Option<usize>) -> reduction::Staged {
        let moves = e.board.generate_moves();
        let mut eval = None;
        let mut history_max = None;
        let mut node = late_move::Node {
            depth: 0,
            alpha: 0,
            beta: 1,
            root_bounds: RootBounds::NEITHER,
            in_check: false,
            ply,
            tt: Table::Miss,
            moves: &moves,
            eval: &mut eval,
            history_max: &mut history_max,
        };
        e.staged_reduction(m, searched, &mut node)
    }

    /// A from and to square pair no move in the list uses, for teaching
    /// the history an entry the list cannot read.
    fn unmade_journey(moves: &[Play]) -> Play {
        (0u8..64)
            .flat_map(|from| (0u8..64).map(move |to| (from, to)))
            .find(|(from, to)| from != to && !moves.iter().any(|m| m.from == *from && m.to == *to))
            .map(|(from, to)| Play::new(from, to, None, None, false, false))
            .expect("a list cannot hold every journey")
    }

    /// Two quiet moves of the position, for teaching the memories.
    fn quiets(e: &AlphaBeta) -> (Play, Play) {
        let moves = e.board.generate_moves();
        let mut quiets = moves
            .iter()
            .filter(|m| m.capture.is_none() && m.promote.is_none());
        let first = *quiets.next().expect("a quiet move");
        let second = *quiets.next().expect("another quiet move");
        (first, second)
    }

    /// The same gate the census stands behind: an engine nobody asked a
    /// ledger of holds none, and the pinned bench counts beside this say
    /// the search it runs is the search it ran before there was a ledger
    /// at all.
    #[test]
    fn an_engine_records_no_ledger_until_it_is_asked_to() {
        let mut e =
            AlphaBeta::with_table_bytes(Board::from_fen(SHARP_MIDDLEGAME).unwrap(), TABLE_BYTES);
        assert!(e.ledger.is_none());
        e.search(4);
        assert!(e.disarm::<reduction::Event>().is_none());
    }

    /// A staged scout that fails low, driven through the two halves by
    /// hand: the row carries the features the node knew, the fen of the
    /// position the move left, and the node's own eval against its
    /// bounds. The board comes back exactly as the recorder found it,
    /// which is the step-back-and-replay the eval column is taken by.
    #[test]
    fn a_row_reads_the_decision_as_the_node_made_it() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (killer, cool) = quiets(&e);
        let color = e.board.active_color;
        // taught at another ply, so what makes the flag a killer is the
        // slot at this one; its history entry is the larger of the two
        e.ordering.cutoff(color, &cool, &[], 1, 5);
        e.ordering.cutoff(color, &killer, &[], 0, 4);
        let moves = e.board.generate_moves();
        let parent_eval = i32::from(crate::eval::eval(&e.board));
        let staged = staged(&e, &killer, 5, Some(0));
        assert!(e.board.make_move(&killer));
        let child_fen = e.board.to_fen();
        let child_key = e.board.key;
        // alpha stands far above anything the position is worth, and
        // under the mate window, so the scout fails low and is trusted
        let (alpha, beta): (Score, Score) = (5000, 5001);
        let Ok(value) = e.windowed(alpha, beta, 3, false, 1, RootBounds::NEITHER, Some(&staged))
        else {
            panic!("an unlimited search aborted");
        };
        assert!(value.score <= alpha, "the scout did not fail low");
        assert_eq!(e.board.to_fen(), child_fen);
        assert_eq!(e.board.key, child_key);
        let sampled = e
            .disarm::<reduction::Event>()
            .expect("a ledger was installed")
            .drain();
        assert_eq!(sampled.events, 1);
        assert_eq!(sampled.taken.len(), 1);
        let row = &sampled.taken[0];
        assert_eq!(row.fen, child_fen);
        assert_eq!(row.depth, 3);
        assert_eq!(row.window, Window::Zero);
        assert_eq!(row.index, 5);
        assert_eq!(row.searched, 6);
        assert_eq!(row.generated, moves.len());
        assert_eq!(row.history, 16);
        assert_eq!(row.history_max, 25);
        assert!(row.killer);
        assert_eq!(row.tt, Table::Miss);
        assert_eq!(row.eval_beta, parent_eval - i32::from(beta));
        assert_eq!(row.alpha_gap, i32::from(alpha) - parent_eval);
        assert_eq!(row.alpha, alpha);
        assert_eq!(row.scout, Scout::Low);
        assert!(row.cost >= 1);
        assert_eq!(row.reduction, 1);
    }

    /// A reduced move the table has marked down: the staged half carries
    /// the signed entry, and its denominator is the largest clamped at
    /// zero, which is zero when every quiet in the list is marked down.
    #[test]
    fn a_marked_down_move_stages_a_signed_history_and_no_denominator() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (m, _) = quiets(&e);
        let color = e.board.active_color;
        let moves = e.board.generate_moves();
        // the move that took the bonus makes a journey no move here makes,
        // so nothing in the list is left with a history to be read against
        let elsewhere = unmade_journey(&moves);
        let marked: Vec<Play> = moves
            .iter()
            .filter(|q| q.capture.is_none() && q.promote.is_none())
            .copied()
            .collect();
        e.ordering.cutoff(color, &elsewhere, &marked, 1, 4);
        let staged = staged(&e, &m, 5, Some(0));
        assert_eq!(staged.features.history, -16);
        assert_eq!(staged.features.history_max, 0);
    }

    /// The reduction column reads what `windowed` was handed: a scout run
    /// two plies shallower writes a two, so a run's rows say how far each
    /// scout was stood back.
    #[test]
    fn the_row_carries_the_reduction_the_scout_ran_at() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (m, _) = quiets(&e);
        let staged = staged(&e, &m, 6, None);
        assert!(e.board.make_move(&m));
        let (alpha, beta): (Score, Score) = (5000, 5001);
        let Ok(value) = e.windowed(alpha, beta, 4, false, 2, RootBounds::NEITHER, Some(&staged))
        else {
            panic!("an unlimited search aborted");
        };
        assert!(value.score <= alpha, "the scout did not fail low");
        let sampled = e
            .disarm::<reduction::Event>()
            .expect("a ledger was installed")
            .drain();
        assert_eq!(sampled.taken.len(), 1);
        let row = &sampled.taken[0];
        assert_eq!(row.depth, 4);
        assert_eq!(row.reduction, 2);
        // the replay's counterfactual is unchanged: the depth the move
        // was denied is the node's less one, however short the scout ran
        assert_eq!(row.replay_depth(), 3);
    }

    /// The same seam under bounds the move clears: the scout fails high,
    /// the row says so, and its cost counts the scout alone rather than
    /// the full depth search the fail high asked for.
    #[test]
    fn a_scout_that_fails_high_is_recorded_as_high() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (m, _) = quiets(&e);
        let staged = staged(&e, &m, 4, None);
        assert!(e.board.make_move(&m));
        // the tree under the scout records rows of its own here, since the
        // quiet futility rule skips at the depths it reaches, so the
        // staged scout's row is picked out by the position it left rather
        // than by being the only one
        let left = e.board.to_fen();
        let (alpha, beta): (Score, Score) = (-5000, -4999);
        let Ok(_) = e.windowed(alpha, beta, 3, false, 1, RootBounds::NEITHER, Some(&staged)) else {
            panic!("an unlimited search aborted");
        };
        let sampled = e
            .disarm::<reduction::Event>()
            .expect("a ledger was installed")
            .drain();
        let rows: Vec<&reduction::Event> =
            sampled.taken.iter().filter(|row| row.fen == left).collect();
        assert_eq!(rows.len(), 1);
        let row = rows[0];
        assert_eq!(row.scout, Scout::High);
        assert_eq!(row.index, 4);
        assert!(!row.killer);
        assert_eq!(row.history, 0);
        assert!(
            row.cost < e.nodes,
            "the cost {} counts more than the scout of a search of {}",
            row.cost,
            e.nodes
        );
    }

    /// The exemption threaded through the recursion rather than read at
    /// one gate. The bounds are real on both sides, far enough inside the
    /// mate scores that the mate window gates say nothing, so the only
    /// thing that can keep a scout off the root's beta is the flag. A bit
    /// dropped at a call site or a flip forgotten shows up as an open node
    /// reducing against a beta of twenty thousand, which the ledger
    /// records. The other error, a bit left set on a bound a search
    /// produced (a clear forgotten where alpha is raised), can only add
    /// refusals and is invisible here; what pins that one is
    /// `what_a_child_carries_and_what_a_raise_leaves`, on the rule itself.
    ///
    /// Twenty thousand is above anything the evaluation produces and under
    /// the mate threshold, so an open window carrying it can only have the
    /// root's beta: an open window's beta is either the root's or the
    /// negation of a raised alpha, and a raised alpha is a child's score. A
    /// zero window can carry it too, under a node whose first child was
    /// mated, which is why the count is of open rows.
    ///
    /// The second half is what makes the first one a claim: the same
    /// search with neither bound marked reduces against that beta plenty.
    #[test]
    fn no_open_node_reduces_against_a_beta_that_is_still_the_roots() {
        const ALPHA: Score = -20_000;
        const BETA: Score = 20_000;

        // every row of the search, since a count of none is a claim about
        // all of them and a reservoir at its cap describes a share
        fn rows_at_the_roots_beta(root_bounds: RootBounds) -> (usize, usize) {
            let mut e = AlphaBeta::with_table_bytes(
                Board::from_fen(SHARP_MIDDLEGAME).unwrap(),
                TABLE_BYTES,
            );
            e.arm(Sampler::<reduction::Event>::with_cap(1, usize::MAX));
            let Ok(_) = e.alpha_beta(ALPHA, BETA, 6, true, root_bounds) else {
                panic!("an unlimited search aborted");
            };
            let sampled = e
                .disarm::<reduction::Event>()
                .expect("a ledger was installed")
                .drain();
            assert_eq!(sampled.overflowed, 0, "the ledger described a share");
            let at_beta = sampled
                .taken
                .iter()
                .filter(|row| {
                    row.window == Window::Open
                        && i32::from(row.alpha) - row.alpha_gap - row.eval_beta == i32::from(BETA)
                })
                .count();
            (at_beta, sampled.taken.len())
        }

        let (marked, rows) = rows_at_the_roots_beta(RootBounds::BOTH);
        assert!(rows > 0, "the tree held no reduction to read either way");
        assert_eq!(marked, 0, "a scout was reduced against the root's beta");

        let (unmarked, _) = rows_at_the_roots_beta(RootBounds::NEITHER);
        assert!(
            unmarked > 0,
            "nothing reduced against that beta with neither bound marked, \
             so the count above was no claim"
        );
    }
}
