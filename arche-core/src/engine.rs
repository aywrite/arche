// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use crate::board::{Board, MOVE_LIST_INLINE, MoveList, Unplayable};
use crate::census;
use crate::effort;
use crate::eval;
use crate::forced;
use crate::ghi::GhiCounters;
use crate::late_move;
use crate::limits::{Limits, RootNodes};
use crate::misc::{Color, Piece, Score};
use crate::ordering::{MoveOrdering, Ordered};
use crate::play::Play;
use crate::recorder::{Sampler, Window};
use crate::reduction;
use crate::residual::{Sample, Shortcut};
use crate::transposition::{
    DEFAULT_TABLE_BYTES, Floor, NO_EVAL, Probe, SignatureCounters, TranspositionTable,
};
use crate::value::{
    MateDistanceWindow, Taint, Value, below_the_mate_window, is_mate, mate_distance_window,
};
use std::fmt;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time;

/// The ply every search stops at: a requested depth is held to it, the
/// full width search ends a line at it whatever depth the check extension
/// has left, and the reported line is walked no further. It also sizes the
/// ordering's per ply tables.
///
/// It sits inside the board's history ring (256 plies, less the fifty
/// move window) and the mate score window (a thousand under the mate
/// score). Sixty four would fit; it was set where a play change need not
/// move it again.
pub const MAX_PLY: u8 = 128;
// the root deepens by one more when it is in check, so a depth held to the
// rail has to leave room for that inside a byte
const _: () = assert!(MAX_PLY < u8::MAX);
// How far above beta the static eval has to stand, per ply still to
// search, for a node to be answered from it: a pawn a ply. The bench
// prefers a little less (sixty to a hundred and twenty span about five
// percent of the count, not monotone). The figure is held above the margin
// at which a mate in two is lost. With this shortcut alone on the reference
// that mate is lost at depth five at seventy one or less, which
// the_reverse_futility_margin_keeps_the_depth_five_mate pins. That test
// guards only a cut below seventy two, so re-measure before moving the
// figure. At depth four the boundary is a hundred and one, above this
// figure; the default search loses that one at every margin from sixty to
// a hundred. docs/ROADMAP.md has the shadow lane's reading.
const REVERSE_FUTILITY_MARGIN: Score = 100;
// The deepest node the margin may answer. Four, six and eight give the
// same bench count to a tenth of a percent.
const REVERSE_FUTILITY_MAX_DEPTH: u8 = 4;
// How many plies shallower than the node the pass is searched. An opening
// value; what moves it is a match, not the bench.
const NULL_MOVE_REDUCTION: u8 = 2;
// One more than the base reduction, so the pass at the shallowest depth it
// is offered at is searched at depth zero (quiescence). The deeper terms
// are clamped to the depth by `null_move_reduction` rather than raising
// this floor.
const NULL_MOVE_MIN_DEPTH: u8 = NULL_MOVE_REDUCTION + 1;
// How many plies of depth buy one more ply of reduction: the conventional
// step, taking the reduction to three from depth six. The bench did not
// choose it (ba921e1 has a sweep that ranks nothing). It leaves depths
// three to five alone, which gives the residual sampler a band the term
// does not touch to be read against.
const NULL_MOVE_DEPTH_DIVISOR: u8 = 6;
// How far the static evaluation must stand above beta to buy one more ply
// of reduction: a ply for every two pawns of clearance. The residual
// sampler's split at this margin (ba921e1) supports the direction of the
// bet, not its size.
const NULL_MOVE_EVAL_UNIT: Score = 200;
// The most plies the margin alone may add. Past three the pass proves
// almost nothing, whatever the margin says.
const NULL_MOVE_EVAL_CAP: u8 = 3;
// How far short of alpha a capture may leave the standing eval, with the
// captured piece counted as fully won, and still be searched in
// quiescence. The conventional figure for conventional piece values.
const DELTA_MARGIN: Score = 200;
// How far either side of the previous iteration's score the root opens.
// Thirty came from a bench node sweep at depth nine (5902681 has the
// table) and was not played against another width until fifteen beat it
// at 10+0.1 (sprt [0, 10] passed, +13 ±9 over 3,400 games, 36ecabc).
const ASPIRATION_WIDTH: Score = 15;
// The first depth the root opens narrow at. Below it the whole iteration
// costs less than one re-search deeper down. A judgment rather than a
// swept figure.
const ASPIRATION_MIN_DEPTH: u8 = 5;
// How many times one side of the window may fail before that side opens to
// the edge. The width doubles each time, so the sides tried are the width,
// twice it, four times it, and then the edge.
const ASPIRATION_FAILURES: u8 = 3;
// The shallowest node that asks whether its table move is singular. The
// excluded search at `(depth - 1) / 2` is depth three here, the shallowest
// at which it is a search rather than a capture tree. The conventional
// trigger; not swept.
const SINGULAR_MIN_DEPTH: u8 = 8;
// How many plies shallower than the node its entry may be and still be
// asked about: the floor is usually the table move's own cutoff a depth
// or two back. The conventional slack; not swept.
const SINGULAR_ENTRY_SLACK: u8 = 3;
// How far under the table move's floor, per ply of depth, the excluded
// search is held to. A second move that comes within it is close enough
// that the table move is not singular. An opening value; not swept.
const SINGULAR_MARGIN: Score = 2;

/// How many plies shallower than the node a pass at `depth` is searched.
/// `eval_beta` is how far the static evaluation stands above beta at the
/// node, which the pass gate has already found to be at least zero.
///
/// The clamp to `depth - 1` is the safety here: the reduced search's depth
/// is `depth - 1 - r` and unsigned, so a larger `r` would wrap to an
/// enormous depth rather than over-reduce. The margin term reaches the
/// clamp at the shallowest depths, where the reduced search is quiescence.
///
/// Off the flag this is the flat base, which holds the bench identical to
/// the flat reduction's.
fn null_move_reduction(config: SearchConfig, depth: u8, eval_beta: Score) -> u8 {
    if !config.adaptive_null_move {
        return NULL_MOVE_REDUCTION;
    }
    let margin = (eval_beta.max(0) / NULL_MOVE_EVAL_UNIT).min(NULL_MOVE_EVAL_CAP as Score) as u8;
    let grown = NULL_MOVE_REDUCTION + depth / NULL_MOVE_DEPTH_DIVISOR + margin;
    grown.min(depth - 1)
}

/// Quiescence's delta test: a capture short of alpha with its piece counted
/// as fully won is expected to be worth less than alpha. Which captures it
/// applies to is the caller's, and the move loop says why.
fn short_of_alpha(standing: Score, captured: Piece, alpha: Score) -> bool {
    standing + eval::material(captured) as Score + DELTA_MARGIN < alpha
}

/// Which places in a node's move list the node made and searched, a bit
/// each: under a cutoff the quiet moves with a bit below the cutting
/// move's place are the history's malus.
///
/// Four words, because a position can hold two hundred and eighteen legal
/// moves. The list is pseudo-legal, and a composed position with many
/// promoted pieces can make it longer.
#[derive(Default)]
struct Searched([u64; 4]);

impl Searched {
    /// Inline, since the call cost more than the mark. The word is taken
    /// modulo four rather than checked: a release build gives a list of
    /// more than 256 moves the bits of the places 256 below, so its malus
    /// may name a move it did not search, where the checked index
    /// panicked. The debug build asserts.
    #[inline(always)]
    fn mark(&mut self, place: usize) {
        debug_assert!(place < 64 * 4, "a move list wider than the mask");
        self.0[place / 64 % 4] |= 1 << (place % 64);
    }

    #[inline(always)]
    fn holds(&self, place: usize) -> bool {
        self.0[place / 64 % 4] >> (place % 64) & 1 == 1
    }

    /// How many places are marked. The move loop asserts this against its
    /// own searched count, so a stray mark fails at the next move rather
    /// than showing up as a malus elsewhere in the tree.
    fn count(&self) -> usize {
        self.0.iter().map(|word| word.count_ones() as usize).sum()
    }
}

/// A search's fail soft answer as its moves come back: the window as it
/// stands, the best score whether or not it reached alpha, the move that
/// scored it, which is what the table remembers, the taint of every value
/// it took, and how many moves it searched. A full width node holds one,
/// and quiescence and the root each open their own.
pub(crate) struct FailSoft {
    /// The bounds as they stand, alpha raised by every move that beat it.
    pub(crate) alpha: Score,
    pub(crate) beta: Score,
    /// Which of the two bounds are still the root's, moved with alpha.
    pub(crate) root_bounds: RootBounds,
    /// How many moves have been made and searched: at a full width node
    /// the table's move when it was legal, and never a move that turned out
    /// illegal. A stand pat is not a move.
    pub(crate) searched: usize,
    /// The alpha the search opened with, which says whether the answer is
    /// a ceiling and is the window the census records.
    opening_alpha: Score,
    best: Score,
    best_move: Option<Play>,
    taint: Taint,
}

/// What a searched move did to the bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reached {
    /// At or above beta: the search is cut off.
    Beta,
    /// Above alpha and under beta: alpha is raised to it.
    Alpha,
    Neither,
}

impl FailSoft {
    /// The answer before its first move, with whatever taint the search
    /// read on the way to it.
    #[inline(always)]
    pub(crate) fn open(alpha: Score, beta: Score, root_bounds: RootBounds, taint: Taint) -> Self {
        Self {
            alpha,
            beta,
            root_bounds,
            searched: 0,
            opening_alpha: alpha,
            best: Score::MIN + 1,
            best_move: None,
            taint,
        }
    }

    /// Quiescence's stand pat: the static score, already under beta, taken
    /// as the best so far and as a floor under alpha, with no move and no
    /// count. It is a floor the search stands on rather than a move that
    /// beat alpha, so the alpha the answer opened with rises with it, and
    /// the answer is a ceiling until a capture beats it.
    #[inline(always)]
    fn stand_pat(&mut self, score: Score) {
        debug_assert!(score < self.beta, "a stand pat at beta answers alone");
        debug_assert_eq!(self.searched, 0, "the stand pat comes before every move");
        self.best = score;
        self.alpha = self.alpha.max(score);
        self.opening_alpha = self.alpha;
    }

    /// One searched move's value, absorbed into the answer. Beta is asked
    /// only of a score above alpha, which with alpha under beta is every
    /// score at or above it, so most moves (which fail low) ask one
    /// question. Asking beta first measured 0.1% more instructions over
    /// the bench, most of it in quiescence.
    #[inline(always)]
    fn absorb(&mut self, m: &Play, value: Value) -> Reached {
        debug_assert!(self.alpha < self.beta, "an empty window");
        self.searched += 1;
        self.taint.absorb(value);
        let score = value.score;
        if score > self.best {
            self.best = score;
            self.best_move = Some(*m);
        }
        if score > self.alpha {
            if score >= self.beta {
                return Reached::Beta;
            }
            self.alpha = score;
            self.root_bounds = self.root_bounds.alpha_raised();
            return Reached::Alpha;
        }
        Reached::Neither
    }

    /// Whether a move raised alpha: the answer is then a score or a floor
    /// rather than a ceiling.
    #[inline(always)]
    fn raised_alpha(&self) -> bool {
        self.alpha != self.opening_alpha
    }
}

/// A full width node: what it settled before its move loop, and its answer
/// as its moves come back. The late move rules, the census, the effort
/// instrument and the ledger all read it.
pub(crate) struct Node {
    /// The node's depth, the check extension included.
    pub(crate) depth: u8,
    pub(crate) in_check: bool,
    /// The ply the quiet memories are read at, or none.
    pub(crate) ply: Option<usize>,
    pub(crate) tt: census::Table,
    /// The node count on entry, which prices what the node cost.
    entered_at: u64,
    /// The move the node is searched without, or none: a node asking
    /// whether its table move is singular searches itself again with that
    /// move left out, and that search stores nothing under the node's key.
    excluded: Option<Play>,
    pub(crate) answer: FailSoft,
}

impl Node {
    /// The node before its first move, its answer opened with the taint
    /// the shortcuts left.
    pub(crate) fn open(
        depth: u8,
        in_check: bool,
        ply: Option<usize>,
        tt: census::Table,
        entered_at: u64,
        excluded: Option<Play>,
        answer: FailSoft,
    ) -> Self {
        Self {
            depth,
            in_check,
            ply,
            tt,
            entered_at,
            excluded,
            answer,
        }
    }
}

/// What the move loop does with one move, which is also how it asks for
/// the child search.
enum Decision {
    /// Not searched at all.
    Skip,
    /// The first move the node searches, at the window as it stands.
    First,
    /// A later move, scouted `reduction` plies shallower first when that is
    /// not zero, with the ledger's staging of the scout when one is armed.
    Search {
        reduction: u8,
        staged: Option<reduction::Staged>,
    },
}

impl Decision {
    /// A later move searched at the node's depth, with no scout and
    /// nothing staged.
    const UNREDUCED: Decision = Decision::Search {
        reduction: 0,
        staged: None,
    };
}

/// The lazy ordering of a node's quiet run, as far as the loop has read
/// it. `order_quiets_at` drives it a place at a time.
struct QuietOrder {
    /// Where the quiet run starts and how many losing captures follow it.
    front: usize,
    losing: usize,
    /// The ply the memories are read at, or none when they are off.
    ply: Option<usize>,
    /// The run still being taken in order: where it ends and how many moves
    /// have been picked from it one at a time. After four picks the rest is
    /// sorted whole.
    lazy: Option<(usize, u8)>,
    /// The places from which the rest of the run is known to be dropped.
    dropped: Range<usize>,
    /// Whether the quiets were keyed before the node answered.
    scored: bool,
    /// Whether a shallow rule filtered the run.
    filtered: bool,
}

impl QuietOrder {
    fn new(ordered: &Ordered, ply: Option<usize>) -> Self {
        Self {
            front: ordered.front,
            losing: ordered.losing,
            ply,
            lazy: None,
            dropped: usize::MAX..usize::MAX,
            scored: false,
            filtered: false,
        }
    }
}

/// What the protocol interface asks of an engine: positions in, answers
/// out.
pub trait Engine {
    fn parse_fen(&mut self, fen_string: &str) -> Result<(), String>;

    /// Forget what was learned from the game just finished. Stored scores do not
    /// account for repetition or the fifty move counter, so a position that
    /// comes up again in a new game would otherwise be scored from a line that
    /// no longer applies to it.
    fn new_game(&mut self);

    /// Play the move of this name, in the coordinate notation the protocol
    /// sends. Refused when no move of that name exists here, or when the
    /// move would leave the king in check, and a refused move changes
    /// nothing.
    fn make_move_str(&mut self, play: &str) -> Result<(), Unplayable>;

    /// Give the engine a transposition table of `bytes` bytes, discarding
    /// whatever the old one held.
    ///
    /// False if the buckets could not be reserved, in which case the engine
    /// keeps the table it had: an interface may ask for more than the
    /// machine has, and a game is better carried on with the old table.
    ///
    /// The answer is the allocator's. Where the kernel overcommits, a size
    /// that fits in ram and swap is granted here and the process killed
    /// later as the entries are written.
    #[must_use]
    fn set_table_bytes(&mut self, bytes: usize) -> bool;

    /// Empty the transposition table and leave everything else as it is:
    /// the protocol's `Clear Hash`.
    fn clear_table(&mut self);

    /// The position, printed the way the board prints itself. A string,
    /// because the library never prints.
    fn board_display(&self) -> String;

    fn perft(&mut self, depth: u8) -> u64;

    fn active_color(&self) -> Color;

    /// Search each depth in turn until one is the last to finish. Every
    /// completed iteration is reported through `on_depth`. A result's node
    /// count covers the whole deepening so far, as the uci info convention
    /// expects.
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
    pub depth: Option<u8>,
    pub limits: Limits,
    /// Set by another thread to stop the search at the next poll: the
    /// protocol's `stop`. None for a search nobody can interrupt. Beside
    /// the limits rather than inside them so that `Limits` stays `Copy`.
    pub stop: Option<Arc<AtomicBool>>,
}

impl SearchParameters {
    /// A search nothing can stop early.
    pub fn new(depth: Option<u8>, limits: Limits) -> Self {
        Self {
            depth,
            limits,
            stop: None,
        }
    }

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
/// it trusts. Each changes the tree searched, so changing one moves the
/// bench.
///
/// Two configurations are named. The reference has every shortcut off and
/// every refusal on: alpha-beta with a table that only speeds it up, so a
/// position searched warm answers as it does cold, deepened as it does
/// direct, and with a small table as with a large one. The exactness tests
/// hold the reference to that. The default is what the engine plays with.
///
/// A switch that rides on another is never asked with that one off, which
/// `a_rule_asked_only_under_another_has_no_site_with_that_one_off` holds.
/// The late move switches are described where they are decided, in
/// `late_move.rs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchConfig {
    /// What the search does about draw tainted transposition scores. What
    /// each policy costs is measured with `bench hash <MB> taint <word>`.
    pub taint: TaintPolicy,
    /// Whether a node near the leaves may answer from its static evaluation
    /// alone when that stands far enough above beta.
    pub reverse_futility: bool,
    /// Whether a node whose eval already stands above beta may hand the
    /// move to the other side and answer from a reduced search of that.
    pub null_move: bool,
    /// Whether the null move reduction grows with depth and with the eval's
    /// margin over beta rather than being the flat two. Rides on
    /// `null_move`, and changes no node's eligibility to pass.
    pub adaptive_null_move: bool,
    /// Whether quiescence may skip a capture that leaves the standing eval
    /// a margin short of alpha with its piece counted as fully won.
    pub delta_margin: bool,
    /// Whether quiescence may skip a capture the swap prices as losing. The
    /// swap sees no pins and nothing beyond its square.
    pub see_pruning: bool,
    /// Whether a late quiet at a full width node is scouted shallower
    /// first and searched at full depth only when the scout beats alpha.
    pub late_move_reductions: bool,
    /// Whether the scout of a late quiet whose index reaches a floor rising
    /// with depth runs a ply shallower still. Rides on
    /// `late_move_reductions`.
    pub deep_reductions: bool,
    /// Whether a late quiet at depth four and up that the skip rules out
    /// (`skip_margin` says by what) is searched at all. Rides on
    /// `late_move_reductions`.
    pub late_move_pruning: bool,
    /// Whether that skip reads a margin on the static evaluation under beta
    /// in place of the attention model's score. Rides on
    /// `late_move_pruning`.
    pub skip_margin: bool,
    /// Whether a quiet move at depths one to three is dropped when the
    /// static evaluation plus a margin a ply cannot reach alpha.
    pub quiet_futility: bool,
    /// Whether a quiet move at depths one to three is dropped once the node
    /// has searched `LATE_MOVE_COUNT` moves a ply. Separate from
    /// `quiet_futility` so an ablation can tell the two apart.
    pub late_move_count: bool,
    /// Whether the late move reduction's amount is read off the table by
    /// depth and move index rather than being the flat ply. Rides on
    /// `late_move_reductions`.
    pub reduction_table: bool,
    /// Whether a node orders its quiet moves by the killers and the history
    /// table. Off in the reference, which keeps the pinned reference tree
    /// the one alpha-beta and the capture ordering produce.
    ///
    /// Ordering rather than pruning, so under the reference the answer is
    /// the same either way and only the tree moves. Under the default a
    /// shortcut fires against the window the parent's search order
    /// produced, so the default's move and score may move too.
    pub move_memory: bool,
    /// Whether the deepening loop opens each iteration from
    /// `ASPIRATION_MIN_DEPTH` on at a window around the last one's score,
    /// widening the side that fails until the score lands inside.
    ///
    /// Off in the reference: a window changes how dearly a depth is reached
    /// rather than what it answers, so the reference's pinned tree stays
    /// the control. Only the deepening loop reads it, so a search asked for
    /// a fixed depth opens full.
    pub aspiration: bool,
    /// Whether a deep node whose table move stands a margin above every
    /// other move, shown by a half depth search with that move excluded,
    /// searches the table move a ply deeper.
    pub singular_extensions: bool,
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

/// What a row of `SearchConfig::SWITCHES` does: turn that row's switch off,
/// leaving the rest of the configuration alone. A setter rather than a
/// builder, so a run that turns two switches off is a fold over rows.
pub type TurnOff = fn(&mut SearchConfig);

/// One switch or two that were named against `SearchConfig::SWITCHES`, and
/// the configuration that turns them off. The fields are private, so only
/// `SearchConfig::without` and `Ablation::and` build one, and a run handed
/// one was handed names the table carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ablation {
    name: &'static str,
    /// The second switch of a pair.
    also: Option<&'static str>,
    config: SearchConfig,
}

impl Ablation {
    /// The table's spelling of the switches, which a report's header prints:
    /// the one name, or the two joined by a comma in the order they were
    /// named, which is how the argument takes them back.
    pub fn name(self) -> String {
        match self.also {
            Some(also) => format!("{},{also}", self.name),
            None => self.name.to_string(),
        }
    }

    /// This switch and another off together, or none when the other is the
    /// same switch or either side is already a pair. The order the two were
    /// named in changes the header and nothing else.
    pub fn and(self, other: Ablation) -> Option<Ablation> {
        if self.also.is_some() || other.also.is_some() || self.name == other.name {
            return None;
        }
        let (_, turn_off) = SearchConfig::SWITCHES
            .into_iter()
            .find(|(switch, _)| *switch == other.name)
            .expect("an ablation is only ever built from a row of the table");
        let mut config = self.config;
        turn_off(&mut config);
        Some(Ablation {
            name: self.name,
            also: Some(other.name),
            config,
        })
    }

    /// The default with its switch or its two off.
    pub fn config(self) -> SearchConfig {
        self.config
    }
}

impl SearchConfig {
    /// The switches a run may name, each beside the function that turns it
    /// off, in field order. A switch the table leaves out fails
    /// `turning_every_switch_off_gives_the_reference`.
    ///
    /// `taint` is not among them: it is a policy with four values rather
    /// than a switch, and `residuals` already takes it.
    pub const SWITCHES: [(&'static str, TurnOff); 15] = [
        ("reverse_futility", |config| config.reverse_futility = false),
        ("null_move", |config| config.null_move = false),
        ("adaptive_null_move", |config| {
            config.adaptive_null_move = false
        }),
        ("delta_margin", |config| config.delta_margin = false),
        ("see_pruning", |config| config.see_pruning = false),
        ("late_move_reductions", |config| {
            config.late_move_reductions = false
        }),
        ("deep_reductions", |config| config.deep_reductions = false),
        ("late_move_pruning", |config| {
            config.late_move_pruning = false
        }),
        ("skip_margin", |config| config.skip_margin = false),
        ("quiet_futility", |config| config.quiet_futility = false),
        ("late_move_count", |config| config.late_move_count = false),
        ("reduction_table", |config| config.reduction_table = false),
        ("move_memory", |config| config.move_memory = false),
        ("aspiration", |config| config.aspiration = false),
        ("singular_extensions", |config| {
            config.singular_extensions = false
        }),
    ];

    /// The default with one switch off, or none for a name the table does
    /// not carry.
    pub fn without(name: &str) -> Option<Ablation> {
        let (name, turn_off) = Self::SWITCHES
            .into_iter()
            .find(|(switch, _)| *switch == name)?;
        let mut config = Self::default();
        turn_off(&mut config);
        Some(Ablation {
            name,
            also: None,
            config,
        })
    }

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
            skip_margin: false,
            quiet_futility: false,
            late_move_count: false,
            reduction_table: false,
            move_memory: false,
            aspiration: false,
            singular_extensions: false,
        }
    }

    /// The word the bench prints for the taint policy, and reads back with
    /// `with_taint`.
    pub fn taint_word(self) -> &'static str {
        match self.taint {
            TaintPolicy::Refuse => "refuse",
            TaintPolicy::Trust => "trust",
            TaintPolicy::Skip => "skip",
            TaintPolicy::Rule50 => "rule50",
        }
    }

    /// The default with its taint policy set by word, or none for a word
    /// that is no policy.
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
    /// the table trusted behind the fifty move guard. Trusting tainted
    /// scores beat refusing them by +48 ±23 over 308 games at 5+0.05
    /// (286c60b); refusing paid in shallower endgame search. The guard cost
    /// nothing a match could see and covers the one regime where a wrong
    /// cutoff provably loses.
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
            skip_margin: true,
            quiet_futility: true,
            late_move_count: true,
            reduction_table: true,
            move_memory: true,
            aspiration: true,
            singular_extensions: true,
        }
    }
}

#[cfg(test)]
mod switches {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Every name the table carries turns a switch off, and no two names
    /// turn the same one off. So a row whose setter touches the field
    /// another row names reads here as two names with one configuration.
    #[test]
    fn every_switch_named_turns_one_of_its_own_off() {
        let default = SearchConfig::default();
        let mut seen: Vec<SearchConfig> = Vec::new();
        for (switch, _) in SearchConfig::SWITCHES {
            let ablation =
                SearchConfig::without(switch).unwrap_or_else(|| panic!("{switch} is not taken"));
            assert_eq!(ablation.name(), switch);
            let config = ablation.config();
            assert_ne!(config, default, "{switch} turned nothing off");
            assert!(!seen.contains(&config), "{switch} repeats another switch");
            seen.push(config);
        }
        assert_eq!(SearchConfig::without("taint"), None);
        assert_eq!(SearchConfig::without("quiet_futilty"), None);
        assert_eq!(SearchConfig::without(""), None);
    }

    /// Every setter folded over the default is the reference, less the
    /// taint policy, which no row names.
    #[test]
    fn turning_every_switch_off_gives_the_reference() {
        let mut folded = SearchConfig::default();
        for (_, turn_off) in SearchConfig::SWITCHES {
            turn_off(&mut folded);
        }
        folded.taint = SearchConfig::reference().taint;
        assert_eq!(folded, SearchConfig::reference());
    }

    /// Each row's name is the field its setter turns off, read out of the
    /// derived `Debug`. Every other test takes the name from the table, so
    /// a misspelt row would pass all of them.
    #[test]
    fn every_switch_names_the_field_its_setter_turns_off() {
        for (switch, turn_off) in SearchConfig::SWITCHES {
            let mut config = SearchConfig::default();
            turn_off(&mut config);
            let printed = format!("{config:?}");
            // the leading space, so `null_move` does not read as the tail of
            // `adaptive_null_move`
            assert!(
                printed.contains(&format!(" {switch}: false")),
                "{switch} is not the field it turns off: {printed}"
            );
            // the default has every switch on, so one setter leaves one off
            assert_eq!(
                printed.matches(": false").count(),
                1,
                "{switch} turned more than its own field off: {printed}"
            );
        }
    }

    /// A pair turns off both of its switches and nothing else, whichever
    /// order it was named in, and the header names them in that order.
    #[test]
    fn a_pair_turns_both_of_its_switches_off() {
        let switches = SearchConfig::SWITCHES.map(|(name, _)| name);
        for (i, first) in switches.into_iter().enumerate() {
            for second in switches.into_iter().skip(i + 1) {
                let a = SearchConfig::without(first).expect(first);
                let b = SearchConfig::without(second).expect(second);
                let pair = a.and(b).unwrap_or_else(|| panic!("{first},{second}"));
                assert_eq!(pair.name(), format!("{first},{second}"));
                assert_eq!(
                    b.and(a).map(Ablation::config),
                    Some(pair.config()),
                    "{first},{second} depends on its order"
                );
                let printed = format!("{:?}", pair.config());
                for switch in [first, second] {
                    assert!(printed.contains(&format!(" {switch}: false")), "{printed}");
                }
                assert_eq!(printed.matches(": false").count(), 2, "{printed}");
            }
        }
    }

    /// Where one switch is only asked under another, the pair with the outer
    /// one off searches as many nodes as the outer single, position by
    /// position, and the inner one alone still moves the count.
    #[test]
    fn a_rule_asked_only_under_another_has_no_site_with_that_one_off() {
        const DEPTH: u8 = 6;
        let positions = crate::bench::positions();
        let nodes = |config| {
            crate::bench::run_suite(&positions, DEPTH, crate::bench::TABLE_BYTES, config)
                .positions
                .iter()
                .map(|p| p.nodes)
                .collect::<Vec<u64>>()
        };
        let one = |name| SearchConfig::without(name).expect(name);
        let default = nodes(SearchConfig::default());
        let outers: Vec<(&str, Vec<u64>)> = ["null_move", "late_move_reductions"]
            .into_iter()
            .map(|outer| (outer, nodes(one(outer).config())))
            .collect();
        for (outer, inner) in [
            ("null_move", "adaptive_null_move"),
            ("late_move_reductions", "deep_reductions"),
            ("late_move_reductions", "late_move_pruning"),
            ("late_move_reductions", "skip_margin"),
            ("late_move_reductions", "reduction_table"),
        ] {
            assert_ne!(nodes(one(inner).config()), default, "{inner} did nothing");
            let pair = one(outer).and(one(inner)).expect("a pair");
            let (_, alone) = outers
                .iter()
                .find(|(name, _)| *name == outer)
                .expect("an outer switch");
            assert_eq!(
                &nodes(pair.config()),
                alone,
                "{inner} has a site with {outer} off"
            );
        }
    }

    /// The same switch twice is the single run under a pair's name, and a
    /// third switch is more than a pair.
    #[test]
    fn a_pair_is_two_different_switches() {
        let one = |name| SearchConfig::without(name).expect(name);
        let null_move = one("null_move");
        assert_eq!(null_move.and(null_move), None);
        let pair = null_move.and(one("aspiration")).expect("a pair");
        assert_eq!(pair.and(one("quiet_futility")), None);
        assert_eq!(one("quiet_futility").and(pair), None);
    }
}

/// Which of a node's two bounds are still the ones the root opened with,
/// rather than scores a search returned.
///
/// A bound the root opened with is one the tree under the root has said
/// nothing about, so the shortcuts and the late move rules stand down
/// wherever beta is such a bound (`beta_is_roots`): the principal
/// variation exemption, as this engine draws it. It is narrower than the
/// open window test other engines use for the same exemption, which would
/// also cover the proof of a later move (`Alpha` here) and every open
/// window under one. Exempting every open window from the reduction and
/// the shallow rules as well was measured and lost (docs/ROADMAP.md); the
/// shortcuts alone refuse every open window besides. Whether alpha is the
/// root's is read by no rule. It is
/// carried because the first move and the proof take the window turned
/// round, so whether the child's beta is the root's is whether this
/// node's alpha was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RootBounds {
    /// Both bounds are the root's: the root before a move raises its
    /// alpha, and every full window search under such a node.
    Both,
    /// Alpha is the root's and beta a returned score: the window of a
    /// `Beta` node turned round, for its first move or the proof of a
    /// later one.
    Alpha,
    /// Beta is the root's and alpha a returned score: a node whose alpha
    /// a move has raised with its beta untouched, or the window of an
    /// `Alpha` node turned round.
    Beta,
    /// Neither: every zero window search, and everything under one.
    Neither,
}

/// The searches a node asks of a child, which the bounds a child carries
/// are keyed on.
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
    /// Whether beta is still the root's own bound: the one question the
    /// rules ask.
    #[inline(always)]
    pub(crate) fn beta_is_roots(self) -> bool {
        matches!(self, Self::Both | Self::Beta)
    }

    /// What a child search carries. The first move and the proof take
    /// this node's window turned round, so they take these bounds turned
    /// round with it. The other three take a zero window, which is this
    /// node's own question about alpha, so they take neither bound.
    fn child(self, search: ChildSearch) -> Self {
        match search {
            ChildSearch::FirstMove | ChildSearch::Proof => self.turned_round(),
            ChildSearch::Scout | ChildSearch::Probe | ChildSearch::Pass => Self::Neither,
        }
    }

    /// The bounds as the child of a full window search sees them: its
    /// alpha is this node's beta negated, and its beta this node's alpha.
    fn turned_round(self) -> Self {
        match self {
            Self::Both => Self::Both,
            Self::Alpha => Self::Beta,
            Self::Beta => Self::Alpha,
            Self::Neither => Self::Neither,
        }
    }

    /// What a raise of alpha leaves: alpha is a returned score from there
    /// on, and beta is untouched.
    fn alpha_raised(self) -> Self {
        match self {
            Self::Both => Self::Beta,
            Self::Alpha => Self::Neither,
            Self::Beta => Self::Beta,
            Self::Neither => Self::Neither,
        }
    }
}

#[cfg(test)]
mod root_bounds {
    use super::{ChildSearch, RootBounds};
    use pretty_assertions::assert_eq;

    /// At a full window every bound the search reads sits beside a mate
    /// gate that answers the same way, so the reference's pinned counts
    /// cannot see a wrong state and this is what holds the rule. The
    /// cases start from the one sided states because a turn the wrong way
    /// round is invisible on the two that are symmetric.
    #[test]
    fn what_a_child_carries_and_what_a_raise_leaves() {
        assert_eq!(
            RootBounds::Alpha.child(ChildSearch::FirstMove),
            RootBounds::Beta
        );
        assert_eq!(
            RootBounds::Alpha.child(ChildSearch::Proof),
            RootBounds::Beta
        );
        assert_eq!(
            RootBounds::Alpha.child(ChildSearch::Scout),
            RootBounds::Neither
        );
        assert_eq!(
            RootBounds::Alpha.child(ChildSearch::Probe),
            RootBounds::Neither
        );
        assert_eq!(
            RootBounds::Alpha.child(ChildSearch::Pass),
            RootBounds::Neither
        );
        // the leftmost line: the root's own window turned round is still
        // the root's own window
        assert_eq!(
            RootBounds::Both.child(ChildSearch::FirstMove),
            RootBounds::Both
        );
        assert_eq!(
            RootBounds::Neither.child(ChildSearch::Proof),
            RootBounds::Neither
        );

        assert_eq!(RootBounds::Both.alpha_raised(), RootBounds::Beta);
        assert_eq!(RootBounds::Alpha.alpha_raised(), RootBounds::Neither);
        assert_eq!(RootBounds::Beta.alpha_raised(), RootBounds::Beta);
        assert_eq!(RootBounds::Neither.alpha_raised(), RootBounds::Neither);

        assert!(RootBounds::Both.beta_is_roots());
        assert!(RootBounds::Beta.beta_is_roots());
        assert!(!RootBounds::Alpha.beta_is_roots());
        assert!(!RootBounds::Neither.beta_is_roots());
    }
}

/// The window the deepening loop opens an iteration at, and what a failed
/// iteration widens it to.
///
/// Opening around the last iteration's score searches the first root
/// move's subtree under bounds a search can reach instead of the mate
/// edges, which is where most of the saving is. The price is an iteration
/// whose score lands outside the window, which proves only a bound and has
/// to be searched again wider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Aspiration {
    alpha: Score,
    beta: Score,
    /// The score each widening is measured from. Meaningless where both
    /// sides are already at the edge.
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
    /// inside this one. Only the side that failed moves, since the other
    /// proved nothing.
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
    /// No clamp against the edge: a centre the mate gate let through is
    /// under thirty thousand and the doublings reach four times the width.
    /// A width in the thousands would want a clamp; the saturating
    /// arithmetic only stops a wrap.
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

    const W: Score = ASPIRATION_WIDTH;
    const AT: u8 = ASPIRATION_MIN_DEPTH;

    fn full() -> Aspiration {
        Aspiration::open(None, AT)
    }

    #[test]
    fn the_window_a_depth_opens_at() {
        assert_eq!(Aspiration::open(Some(30), AT - 1), full());
        assert_eq!(Aspiration::open(None, AT + 4), full());

        let opened = Aspiration::open(Some(30), AT);
        assert_eq!((opened.alpha, opened.beta), (30 - W, 30 + W));

        // never around a mate score
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

        // each doubling is measured from the centre, not from the last bound
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
        // a fail low says nothing about beta
        assert_eq!(window.beta, 30 + W);

        for _ in 0..3 {
            window = window.widen(ScoreBound::Lower);
        }
        // both sides spent is the full window
        assert_eq!((window.alpha, window.beta), (full().alpha, full().beta));
    }
}

pub struct AlphaBeta {
    pub(crate) board: Board,
    config: SearchConfig,
    nodes: u64,
    transpositions: TranspositionTable,
    ghi: GhiCounters,
    selective_depth: u8,
    // search state
    /// What the search call under way may spend. The deepening loop hands
    /// each iteration its own.
    limits: Limits,
    /// The node count at which the limits are looked at next.
    next_check: u64,
    /// The flag another thread sets to stop the search, or none while
    /// nothing may. Armed by `SearchParameters::for_iteration` as the clock
    /// is, so a search asked directly for a depth never reads one.
    stop: Option<Arc<AtomicBool>>,
    /// The nodes quiescence visited, a part of `nodes`, for the bench. Never
    /// reset: the bench reads it from an engine made for the one search.
    quiescence_nodes: u64,
    ordering: MoveOrdering,
    /// The leaf terms' memos. Never cleared between searches: an entry is
    /// read only against the key that wrote it.
    caches: eval::Caches,
    /// The residual sampler, or none, which is what every constructor
    /// builds. An engine with none takes no branch a search without a
    /// sampler did not take, which the pinned node counts stand on.
    sampler: Option<Sampler<Sample>>,
    /// The cutoff census's reservoir, or none, on the sampler's terms.
    census: Option<Sampler<census::Event>>,
    /// The reduction ledger's reservoir, or none, on the same terms.
    ledger: Option<Sampler<reduction::Event>>,
    /// The effort instrument's reservoir, or none, on the same terms.
    effort: Option<Sampler<effort::Event>>,
    /// Every event the effort instrument has been offered, by depth: the
    /// population its reservoir samples. Bumped only when the reservoir is
    /// armed.
    effort_depths: effort::Depths,
    /// The forced decision instrument's arm, or none, on the sampler's
    /// terms: read behind a bare check where each decision it can invert
    /// is taken, and in the two places that set those decisions up (the
    /// quiet ordering asked move by move and the scout's staged reduction).
    forced: Option<Box<forced::Arm>>,
    /// The nodes searched under each root move over every search of the
    /// depth under way, which the soft line reads once the depth completes.
    /// Cleared by the deepening loop at each depth and by a fixed depth
    /// search.
    root_nodes: Vec<(Play, u64)>,
    /// What the singular test did. Never reset, as `quiescence_nodes`.
    singular: SingularCounts,
    /// What each completed depth handed the soft line, for the tests.
    #[cfg(test)]
    soft_lines: Vec<RootNodes>,
}

/// How often the singular test was reached, run and answered yes, which
/// prices the extension offline: a test that is rarely run or rarely fires
/// cannot be worth much, and one that fires everywhere is a reduction's
/// price for an extension's name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SingularCounts {
    /// Nodes that could ask: a table move at a depth the test reads.
    pub asked: u64,
    /// Nodes that ran the excluded search: the entry was deep enough and
    /// its score a floor.
    pub tested: u64,
    /// Nodes whose table move proved singular and was searched deeper.
    pub extended: u64,
}

/// What a search can be armed to record. Implemented here rather than
/// beside the event types because each names a field of the engine.
pub(crate) trait Recorded: Sized {
    /// What the shared recording loop calls a run of this kind when a
    /// position does not parse.
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

    pub fn with_config(board: Board, bytes: usize, config: SearchConfig) -> Self {
        Self::with_table(board, TranspositionTable::of_bytes(bytes), config)
    }

    fn with_table(board: Board, transpositions: TranspositionTable, config: SearchConfig) -> Self {
        Self {
            board,
            config,
            nodes: 0,
            transpositions,
            ghi: GhiCounters::default(),
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
            forced: None,
            root_nodes: Vec::new(),
            singular: SingularCounts::default(),
            #[cfg(test)]
            soft_lines: Vec::new(),
        }
    }

    /// Arm a reservoir: have the search record what it does at the nodes
    /// the reservoir's key picks. Nothing the engine plays or benches with
    /// arms one.
    pub(crate) fn arm<T: Recorded>(&mut self, sampler: Sampler<T>) {
        *T::slot(self) = Some(sampler);
    }

    /// The reservoir back with everything it collected, leaving the engine
    /// recording nothing. None from an engine that was never armed. Handed
    /// back so one reservoir can be carried across a run of searches with
    /// its cap describing the whole run.
    pub(crate) fn disarm<T: Recorded>(&mut self) -> Option<Sampler<T>> {
        T::slot(self).take()
    }

    /// Arm the forced decision instrument, to sample the decisions taken
    /// or to invert one.
    pub(crate) fn arm_forced(&mut self, arm: forced::Arm) {
        self.forced = Some(Box::new(arm));
    }

    /// The arm back, with what it sampled or counted.
    pub(crate) fn disarm_forced(&mut self) -> Option<forced::Arm> {
        self.forced.take().map(|arm| *arm)
    }

    /// A node decision just taken, the margin's or the pass's, offered to
    /// the forced decision instrument. True when it is the decision being
    /// inverted, which the caller then does not take.
    // cold and out of line behind a bare is_some, for `sample`'s reason
    #[cold]
    #[inline(never)]
    fn forced_node(
        &mut self,
        kind: forced::Kind,
        depth: u8,
        eval: Score,
        alpha: Score,
        beta: Score,
    ) -> bool {
        let address = forced::Address {
            kind,
            key: self.board.key,
            depth,
        };
        let board = &self.board;
        let Some(arm) = self.forced.as_mut() else {
            return false;
        };
        if arm.inverts(address) {
            return true;
        }
        arm.offer(address, |root, at| forced::Event {
            address,
            root,
            at,
            features: None,
            eval_beta: i32::from(eval) - i32::from(beta),
            alpha_gap: i32::from(alpha) - i32::from(eval),
            attention: None,
            fen: board.to_fen(),
        });
        false
    }

    /// What answered a node whose margin was inverted, kept from the first
    /// such visit.
    #[cold]
    #[inline(never)]
    fn forced_answered(&mut self, answered: forced::Answered) {
        if let Some(arm) = self.forced.as_mut() {
            arm.answered.get_or_insert(answered);
        }
    }

    /// A late quiet a rule just passed over, offered to the forced decision
    /// instrument. True when it is the decision being inverted. The move is
    /// made and unmade to read the key of the position it leaves, which is
    /// the address; one that turns out illegal denied nothing.
    #[cold]
    #[inline(never)]
    fn forced_skip(
        &mut self,
        node: &Node,
        rules: &mut late_move::Rules,
        moves: &[Play],
        m: &Play,
    ) -> bool {
        let features = late_move::features(&self.deciding(), node, rules, moves, m);
        if !self.board.make_move(m) {
            return false;
        }
        let key = self.board.key;
        self.board.undo_move();
        let address = forced::Address {
            kind: forced::Kind::Skip,
            key,
            depth: node.depth,
        };
        let board = &self.board;
        let Some(arm) = self.forced.as_mut() else {
            return false;
        };
        if arm.inverts(address) {
            return true;
        }
        arm.offer(address, |root, at| {
            let eval = i64::from(crate::eval::eval(board));
            let (eval_beta, alpha_gap) = (
                eval - i64::from(node.answer.beta),
                i64::from(node.answer.alpha) - eval,
            );
            forced::Event {
                address,
                root,
                at,
                features: Some(features),
                eval_beta: eval_beta as i32,
                alpha_gap: alpha_gap as i32,
                attention: late_move::attention(node.depth, &features, eval_beta, alpha_gap),
                fen: board.to_fen(),
            }
        });
        false
    }

    /// A reduced scout that came back at or below alpha, offered to the
    /// forced decision instrument before it answers for its move. True when
    /// it is the decision being inverted. The board is the position the
    /// move left, which is the address, and the node's own evaluation is
    /// read by stepping the move back, for kept events alone.
    #[cold]
    #[inline(never)]
    fn forced_scout(
        &mut self,
        staged: Option<&reduction::Staged>,
        depth: u8,
        alpha: Score,
        beta: Score,
    ) -> bool {
        let address = forced::Address {
            kind: forced::Kind::TrustedScout,
            key: self.board.key,
            depth,
        };
        let board = &mut self.board;
        let Some(arm) = self.forced.as_mut() else {
            return false;
        };
        if arm.inverts(address) {
            return true;
        }
        let Some(staged) = staged else {
            return false;
        };
        arm.offer(address, |root, at| {
            board.undo_move();
            let eval = i64::from(crate::eval::eval(board));
            let fen = board.to_fen();
            assert!(
                board.make_move(&staged.play),
                "the scouted move was made once already"
            );
            let (eval_beta, alpha_gap) = (eval - i64::from(beta), i64::from(alpha) - eval);
            forced::Event {
                address,
                root,
                at,
                features: Some(staged.features),
                eval_beta: eval_beta as i32,
                alpha_gap: alpha_gap as i32,
                attention: late_move::attention(depth, &staged.features, eval_beta, alpha_gap),
                fen,
            }
        });
        false
    }

    /// What the node knew about a move at the gate, gathered for a
    /// ledger row: the staged half of a scouted event, and the whole of
    /// a skipped one.
    // cold and out of line behind a bare is_some, for `sample`'s reason
    #[cold]
    #[inline(never)]
    fn staged_reduction(
        &self,
        m: &Play,
        node: &Node,
        rules: &mut late_move::Rules,
        moves: &[Play],
    ) -> reduction::Staged {
        reduction::Staged {
            play: *m,
            features: late_move::features(&self.deciding(), node, rules, moves, m),
        }
    }

    /// The three references the late move decision reads the search
    /// through. Built at the call and never held, so the borrow ends with
    /// the question.
    fn deciding(&self) -> late_move::Search<'_> {
        late_move::Search {
            board: &self.board,
            ordering: &self.ordering,
            config: &self.config,
        }
    }

    /// A skipped move offered to the ledger, with no scout behind it. The
    /// search never makes a skipped move, so it is made and unmade around
    /// the record alone, and one that turns out illegal is not recorded.
    /// The fen and the sampling key are the position the move leaves, as
    /// for a scouted move, so the replay reads a skipped row as it reads a
    /// low one. The bounds are the node's as the move is reached.
    #[cold]
    #[inline(never)]
    fn ledger_skip(&mut self, staged: reduction::Staged, node: &Node) {
        let (depth, alpha, beta) = (node.depth, node.answer.alpha, node.answer.beta);
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
            // the node's own eval, by stepping back and replaying
            board.undo_move();
            let eval = i32::from(crate::eval::eval(board));
            assert!(
                board.make_move(&staged.play),
                "the skipped move was made once already"
            );
            reduction::Event::recorded(
                fen,
                depth,
                alpha,
                beta,
                eval,
                &staged.features,
                reduction::Scout::Skipped,
                0,
                0,
            )
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
            board.undo_move();
            let eval = i32::from(crate::eval::eval(board));
            assert!(
                board.make_move(&staged.play),
                "the staged move was made once already"
            );
            let scout = if scout <= alpha {
                reduction::Scout::Low
            } else {
                reduction::Scout::High
            };
            reduction::Event::recorded(
                fen,
                depth,
                alpha,
                beta,
                eval,
                &staged.features,
                scout,
                cost,
                reduction,
            )
        });
    }

    /// One node answering out of the move loop, offered to the census:
    /// which move cut it off, or none when the loop ran out. The window is
    /// the one the node opened with.
    ///
    /// The killers and the history are read before `cutoff` teaches them
    /// the move, so a row says what the node knew when it chose. The
    /// evaluation is computed only for kept events, exact rather than a
    /// cache read.
    // cold and out of line behind a bare is_some, for `sample`'s reason
    #[cold]
    #[inline(never)]
    fn census_event(
        &mut self,
        node: &Node,
        moves: &[Play],
        quiets_scored: bool,
        cutting: Option<census::Cutting<'_>>,
    ) {
        let Node {
            depth,
            in_check,
            ply,
            tt,
            entered_at,
            ..
        } = *node;
        let FailSoft {
            opening_alpha: alpha,
            beta,
            searched,
            ..
        } = node.answer;
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
            let quiet_history = |m: &Play| ordering.quiet_history(board.active_color, m);
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

    /// What the effort instrument has counted by depth.
    pub(crate) fn effort_tally(&self) -> &effort::Depths {
        &self.effort_depths
    }

    /// One node of the move loop answering, offered to the effort
    /// instrument's reservoir and counted in its per depth tally.
    ///
    /// The tally is bumped in front of the key test, as `Sampler::event`
    /// bumps `events`, so the counts a run reports do not move with
    /// `every`.
    // cold and out of line behind a bare is_some, for `sample`'s reason
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
    /// measured against, offered to the sampler. The evaluation is passed
    /// in, so a row states the number the gate read.
    // cold and out of line, behind a bare is_some at each call site:
    // inlined, the body grew alpha_beta enough that moved jump tables
    // aliased in the branch predictor, for half a million extra mispredicts
    // on a bench 5 under callgrind (6e8842a).
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
    /// engine's caches. The score is the one `eval::eval` gives.
    fn eval(&mut self) -> Score {
        crate::eval::eval_cached(&self.board, &mut self.caches)
    }

    /// The ply the quiet memories are indexed by at this node, or none when
    /// the configuration has them off or the ply is past the table. The
    /// rail keeps every line inside the table; the second test makes the
    /// index safe here rather than at every caller.
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

    /// What the table knows about the position, under the taint policy,
    /// counted.
    #[inline(always)]
    fn probe(&mut self, alpha: Score, beta: Score, depth: u8) -> Probe {
        let probe = self.transpositions.probe(
            &self.board,
            alpha,
            beta,
            depth,
            self.config.taint.refuses_tainted_cutoffs(),
            self.config.taint.guards_rule50(),
        );
        self.ghi.count_probe(probe);
        probe
    }

    /// The table's move for ordering and never its cutoff, counted: what a
    /// search with a move excluded asks, since the entry's score is the
    /// excluded move's.
    #[inline(always)]
    fn probe_for_ordering(&mut self) -> Probe {
        let probe = self.transpositions.probe_for_ordering(&self.board);
        self.ghi.count_probe(probe);
        probe
    }

    /// Whether a result may be stored under the taint policy. A refused
    /// store is counted as skipped.
    fn keeps(&mut self, value: Value) -> bool {
        if value.tainted && !self.config.taint.stores_tainted() {
            self.ghi.count_skipped_store();
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

    pub fn config(&self) -> SearchConfig {
        self.config
    }

    /// How much of the search's use of the transposition table depended on
    /// the path taken rather than on the position.
    pub fn ghi(&self) -> GhiCounters {
        self.ghi
    }

    /// Have the table keep the full key of every entry: see
    /// `TranspositionTable::audit_signatures`. False if there was not the
    /// memory for the keys.
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

    /// What the singular test has done over every search this engine ran.
    pub fn singular_counts(&self) -> SingularCounts {
        self.singular
    }

    /// Cooperative limit check. The limits say when to look at them again.
    /// The count is incremented after this is asked, so a budget of n is n
    /// nodes visited.
    fn poll_deadline(&mut self) -> Result<(), Aborted> {
        if self.nodes < self.next_check {
            return Ok(());
        }
        self.check_limits()
    }

    /// The slow half of `poll_deadline`, out of line because it runs once
    /// in thousands of nodes.
    #[cold]
    #[inline(never)]
    fn check_limits(&mut self) -> Result<(), Aborted> {
        if self.limits.expired(self.nodes) {
            return Err(Aborted);
        }
        // relaxed: the flag is all the two threads share, so there is
        // nothing to order it against
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

    /// What every search entry writes before its first node: the limits
    /// and the stop the deadline poll reads, the counters and the board's
    /// line.
    fn begin(&mut self, limits: Limits, stop: Option<Arc<AtomicBool>>) {
        self.limits = limits;
        self.stop = stop;
        self.next_check = 0;
        self.nodes = 0;
        self.board.start_line();
    }

    /// What a capture search makes of the position this engine holds, over
    /// the open window and under no limits, so what comes back is a value.
    /// For the tuner's quiet test. `quiescence` itself stays private,
    /// because a caller free to choose the window could read a bound as a
    /// value.
    pub(crate) fn quiescence_value(&mut self) -> Score {
        self.begin(Limits::unlimited(), None);
        match self.quiescence(Score::MIN + 1, Score::MAX - 1) {
            Ok(value) => value.score,
            Err(Aborted) => unreachable!("an unlimited capture search runs to the end"),
        }
    }

    fn quiescence(&mut self, alpha: Score, beta: Score) -> Result<Value, Aborted> {
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

        // a side in check cannot stand pat, so its static eval is no floor.
        // The full search never enters here in check (the extension
        // searches those nodes full width), so a check seen here was
        // delivered by a capture searched here. Fail soft.
        let in_check = self.board.in_check();
        // a stalemate is not in check, so standing pat would read it as the
        // eval. Only a side with nothing but pawns and a king is asked: a
        // piece almost always has a move, and where the king test fails the
        // answer costs a move generation
        if !in_check
            && !self.board.has_non_pawn_material()
            && !self.board.has_legal_move_out_of_check()
        {
            return Ok(Value::clean(0));
        }
        let standing = if in_check { None } else { Some(self.eval()) };
        // quiescence reads no draw by rule itself, but a probe trusting
        // tainted scores can cut on one inside a capture tree. The root
        // bounds are read by no rule here
        let mut answer = FailSoft::open(alpha, beta, RootBounds::Neither, Taint::default());
        if let Some(score) = standing {
            if score >= beta {
                return Ok(Value::clean(score));
            }
            answer.stand_pat(score);
        }

        // a probe at depth zero: any stored bound is deep enough here
        let pv_play = match self.probe(answer.alpha, beta, 0) {
            Probe::Cut(value) => return Ok(value),
            Probe::Order(play) | Probe::Refused(play) => Some(play),
            Probe::Miss => None,
        };
        // in check every evasion is searched, quiet or not
        let mut moves = MoveList::new();
        let mut captures = if in_check {
            self.board.evasions_into(&mut moves)
        } else {
            self.board.generate_captures_into(&mut moves)
        };
        // the delta test at the starting alpha, before the order prices
        // each capture with the swap. Alpha only rises, so the loop would
        // skip every capture dropped here. Two cases are left to the loop
        // because filtering them would change the search: under a mate beta
        // a mating capture can lift alpha into the mate window, after which
        // the loop searches every capture, and a list that spills the
        // buffer is ordered with no losing band (`MoveOrdering::order_split`)
        if let Some(standing) = standing {
            if self.config.delta_margin
                && !is_mate(answer.alpha)
                && !is_mate(beta)
                && moves.len() <= MOVE_LIST_INLINE
            {
                // only captures are dropped, and they lead the list: keep
                // them in order, then close the rest up behind them
                let mut kept = 0;
                for j in 0..captures {
                    let m = moves[j];
                    let keep = match m.capture {
                        Some(captured) if m.promote.is_none() => {
                            !short_of_alpha(standing, captured, answer.alpha)
                        }
                        _ => true,
                    };
                    moves[kept] = m;
                    kept += usize::from(keep);
                }
                if kept < captures {
                    let len = moves.len();
                    moves.copy_within(captures..len, kept);
                    moves.truncate(len - (captures - kept));
                    captures = kept;
                }
            }
        }
        // no memories here: they say nothing about captures or evasions
        let Ordered { front, .. } =
            self.ordering
                .order_split(&self.board, &mut moves, captures, pv_play, None);

        for (i, m) in moves.iter().enumerate() {
            // two skips the reference does not make. A promotion is exempt
            // because the swap prices the arriving piece as the pawn that
            // left, and an evasion because a side in check has no standing
            // eval. A mate window alpha is exempt because the margin's
            // arithmetic would skip every capture, the mating one included;
            // after the stand pat, alpha is in the window only with a mate
            // already in hand. A sacrifice that would find a first mate is
            // skipped like any other losing capture
            if let (Some(standing), Some(captured)) = (standing, m.capture) {
                if !is_mate(answer.alpha) && m.promote.is_none() {
                    if self.config.delta_margin && short_of_alpha(standing, captured, answer.alpha)
                    {
                        continue;
                    }
                    // every capture behind the front is one the swap priced
                    // as losing
                    if self.config.see_pruning && i >= front {
                        continue;
                    }
                }
            }
            if self.board.make_move(m) {
                // undo before an abort can propagate
                let result = self.quiescence(-beta, -answer.alpha);
                self.board.undo_move();
                let value = -result?;
                if answer.absorb(m, value) == Reached::Beta {
                    let value = answer.taint.stamp(value.score);
                    self.store_cutoff(m, value, 0, standing.unwrap_or(NO_EVAL));
                    return Ok(value);
                }
            }
        }

        // in check there is no stand pat, so nothing searched is no legal
        // move. The count is asked first: written the other way round, the
        // compiler kept the check flag on the stack rather than in a
        // register and quiescence ran more instructions
        if answer.searched == 0 && in_check {
            return Ok(Value::mated(self.board.line_ply));
        }

        let value = answer.taint.stamp(answer.best);
        if let Some(play) = answer.best_move {
            self.store_answer(
                play,
                value,
                0,
                answer.raised_alpha(),
                standing.unwrap_or(NO_EVAL),
            );
        }
        Ok(value)
    }

    /// What can answer a full width node before a move of it is searched:
    /// reverse futility, then the null move. Each claims the position
    /// already stands above beta, the margin from the static eval alone and
    /// the pass from a reduced search. Nothing is stored on either: an entry
    /// names the play it was reached by, and no move was searched here.
    ///
    /// Both rest on the static eval being a floor, which gives way in four
    /// gates. A side in check cannot decline to move. A side down to pawns
    /// and a king is where zugzwang happens. A beta inside the mate window
    /// is cleared by every eval, so a cutoff against one would leave a
    /// faster mate unsearched (the positive half is redundant while
    /// material bounds the eval, and kept in case the eval grows terms that
    /// reach higher). And a beta that is still the root's own bound, or an
    /// open window, has had nothing claimed of it to stand above: an open
    /// window asks for the node's score rather than a bound on it. The root
    /// bounds alone would leave the proof after a probe fails high open to
    /// both shortcuts, since it takes the window turned round, with alpha
    /// the root's and beta a returned score. The reductions and the shallow
    /// rules still read the root bounds alone: exempting every open window
    /// from them as well measured a loss.
    ///
    /// A `Some` answers the node. A pass that failed answers nothing but
    /// leaves whatever it read in the node's taint.
    ///
    /// `eval_memo` is filled wherever the gates passed and an evaluation
    /// was read, fired or not, so the move loop does not evaluate twice. It
    /// arrives filled where the node's table entry held the evaluation, and
    /// is then read rather than computed.
    /// Alpha is read only for the open window and by the sampler.
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
        eval_memo: &mut Option<Score>,
    ) -> Result<Option<Value>, Aborted> {
        let margin = self.config.reverse_futility && depth <= REVERSE_FUTILITY_MAX_DEPTH;
        // no pass directly under a pass, or the search would answer a
        // position from a line neither side moved in. The eval gate below
        // does not hold it: the window turns round under a pass but the
        // tempo goes to the other side, so a parent whose eval stood less
        // than twice the tempo above its beta has a child that passes the
        // gate too. `can_null` holds it, since a pass's child is searched
        // with it false
        let pass = self.config.null_move && can_null && depth >= NULL_MOVE_MIN_DEPTH;
        if (!margin && !pass)
            || in_check
            || !self.board.has_non_pawn_material()
            || is_mate(beta)
            || root_bounds.beta_is_roots()
            // the open window, spelt without the subtraction, which
            // overflows a Score at the full window
            || alpha + 1 < beta
        {
            return Ok(None);
        }
        let eval = match *eval_memo {
            Some(eval) => eval,
            None => self.eval(),
        };
        *eval_memo = Some(eval);
        // set only by the forced decision instrument, which then wants to
        // know what answered the node instead
        let mut margin_inverted = false;

        // the margin proves `eval - margin` as a lower bound, and fail soft
        // returns that. Clean: a static eval consulted no path
        if margin {
            let floor = eval.saturating_sub(REVERSE_FUTILITY_MARGIN * depth as Score);
            // the shadow row: every candidate, fired or not, since the fired
            // rows say nothing about where a tighter margin would fire
            if self.sampler.is_some() && eval >= beta {
                self.sample(Shortcut::ShadowFutility, depth, floor, alpha, beta, eval);
            }
            if floor >= beta {
                if self.forced.is_some()
                    && self.forced_node(forced::Kind::ReverseFutility, depth, eval, alpha, beta)
                {
                    margin_inverted = true;
                } else {
                    if self.sampler.is_some() {
                        self.sample(Shortcut::ReverseFutility, depth, floor, alpha, beta, eval);
                    }
                    return Ok(Some(Value::clean(floor)));
                }
            }
        }

        // a zero window: the question is only whether a pass beats beta
        if pass && eval >= beta {
            let reduction = null_move_reduction(self.config, depth, eval - beta);
            self.board.make_null_move();
            let result = self.alpha_beta(
                -beta,
                -beta + 1,
                depth - 1 - reduction,
                false,
                root_bounds.child(ChildSearch::Pass),
                None,
            );
            // undo before an abort can propagate
            self.board.undo_null_move();
            let value = -result?;
            if value.score >= beta
                && !(self.forced.is_some()
                    && self.forced_node(forced::Kind::NullMove, depth, eval, alpha, beta))
            {
                // a mate found through a pass is not a mate, since the pass
                // is not a legal move, so the score is held under the window
                // a caller reads mates in
                let score = below_the_mate_window(value.score);
                if self.sampler.is_some() {
                    self.sample(Shortcut::NullMove, depth, score, alpha, beta, eval);
                }
                if margin_inverted {
                    self.forced_answered(forced::Answered::NullMove);
                }
                return Ok(Some(Value::with_taint(score, value.tainted)));
            }
            taint.absorb(value);
        }
        if margin_inverted {
            self.forced_answered(forced::Answered::Moves);
        }
        Ok(None)
    }

    /// One child of a full width node, or nothing when the move is not
    /// legal here. The undo comes before the abort propagates; propagating
    /// is what keeps an aborted frame's meaningless score away from every
    /// store above. `decision` says whether the move is the node's first or
    /// a later one, and a later one carries the scout's reduction and the
    /// ledger's staging, which travels as a parameter rather than as a
    /// field of the engine. A skipped move never reaches here.
    #[inline(always)]
    fn search_child(
        &mut self,
        m: &Play,
        alpha: Score,
        beta: Score,
        depth: u8,
        decision: &Decision,
        root_bounds: RootBounds,
    ) -> Result<Option<Value>, Aborted> {
        debug_assert!(
            !matches!(decision, Decision::Skip),
            "the loop searched a move it decided to skip"
        );
        if !self.board.make_move(m) {
            return Ok(None);
        }
        let result = self.windowed(alpha, beta, depth, decision, root_bounds);
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
    /// A body of its own rather than `search_child`'s so that an abort from
    /// any pass runs through the one undo there.
    fn windowed(
        &mut self,
        alpha: Score,
        beta: Score,
        depth: u8,
        decision: &Decision,
        root_bounds: RootBounds,
    ) -> Result<Value, Aborted> {
        let (reduction, staged) = match decision {
            Decision::First => {
                return Ok(-self.alpha_beta(
                    -beta,
                    -alpha,
                    depth - 1,
                    true,
                    root_bounds.child(ChildSearch::FirstMove),
                    None,
                )?);
            }
            Decision::Search { reduction, staged } => (*reduction, staged.as_ref()),
            Decision::Skip => unreachable!("a skipped move is never searched"),
        };
        let mut tainted = false;
        if reduction > 0 {
            // `late_move::amount` clamps the reduction to `depth - 2`
            debug_assert!(depth > reduction + 1, "the scout would be quiescence");
            let entered_at = self.nodes;
            let scout = -self.alpha_beta(
                -alpha - 1,
                -alpha,
                depth - 1 - reduction,
                true,
                root_bounds.child(ChildSearch::Scout),
                None,
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
            if scout.score <= alpha
                && !(self.forced.is_some() && self.forced_scout(staged, depth, alpha, beta))
            {
                return Ok(scout);
            }
            // the scout's fail high asked for the searches below, so they
            // carry its taint, as does a trusted fail low the forced
            // decision instrument inverted
            tainted = scout.tainted;
        }
        let probe = -self.alpha_beta(
            -alpha - 1,
            -alpha,
            depth - 1,
            true,
            root_bounds.child(ChildSearch::Probe),
            None,
        )?;
        // `alpha + 1 >= beta` is the zero window, spelt without the
        // subtraction: `beta - alpha` overflows a Score at the full window
        if probe.score <= alpha || alpha + 1 >= beta {
            return Ok(Value::with_taint(probe.score, probe.tainted || tainted));
        }
        let proof = -self.alpha_beta(
            -beta,
            -alpha,
            depth - 1,
            true,
            root_bounds.child(ChildSearch::Proof),
            None,
        )?;
        Ok(Value::with_taint(
            proof.score,
            proof.tainted || probe.tainted || tainted,
        ))
    }

    /// A fail high at a full width node: the move that proved it goes to
    /// the quiet memories with the moves the node searched before it and,
    /// when the taint policy allows, to the table. A node searched with a
    /// move excluded stores nothing: its answer is about the rest of the
    /// moves, and the entry holds the excluded one.
    ///
    /// `tried` is the moves the node searched, not the whole list: the
    /// history marks down what the node asked and got nothing from.
    fn cutoff<'a>(
        &mut self,
        m: &Play,
        tried: impl IntoIterator<Item = &'a Play>,
        node: &Node,
        score: Score,
        static_eval: Score,
    ) -> Value {
        self.remember_cutoff(m, tried, node.depth);
        let value = node.answer.taint.stamp(score);
        if node.excluded.is_none() {
            self.store_cutoff(m, value, node.depth, static_eval);
        }
        value
    }

    /// The table's move, searched before the rest are generated: it sorts
    /// ahead of everything else, so the nodes it cuts never generate or
    /// sort at all, and the tree searched is unchanged. A cutoff answers
    /// the node. Otherwise the node absorbs what the move scored, or
    /// nothing when the move was not legal here. `extension` is the plies
    /// the singular test added to this move alone; the rest of the node,
    /// its stores included, keeps the node's depth.
    fn search_table_move(
        &mut self,
        tt: Play,
        node: &mut Node,
        static_eval: Score,
        extension: u8,
    ) -> Result<Option<Value>, Aborted> {
        let Some(value) = self.search_child(
            &tt,
            node.answer.alpha,
            node.answer.beta,
            node.depth + extension,
            &Decision::First,
            node.answer.root_bounds,
        )?
        else {
            return Ok(None);
        };
        if node.answer.absorb(&tt, value) != Reached::Beta {
            return Ok(None);
        }
        let cutting = census::Cutting {
            play: &tt,
            reduced: false,
            table: true,
        };
        self.record_node(node, &[], false, Some(cutting));
        Ok(Some(self.cutoff(&tt, &[], node, value.score, static_eval)))
    }

    /// Whether the table move is singular: whether a search of this node
    /// with that move excluded, at half the depth and over a zero window a
    /// margin under the move's floor, fails low, so that no other move
    /// comes close. Asked only of a node deep enough, under the rail, that
    /// is not itself such a search, whose entry names the table move from
    /// near the node's depth with a floor that is no mate. The floor is
    /// read whatever the taint policy made of the entry, since nothing of
    /// the excluded search is stored and the extended move's answer is
    /// stamped as any other. The excluded search's own taint is dropped
    /// for the same reason: its result is a decision about depth, not a
    /// score the node answers with. `depth` is the node's, check extension
    /// included, and the excluded search of a node in check extends itself
    /// again, so there it runs a ply over the half.
    fn table_move_is_singular(
        &mut self,
        tt: Play,
        floor: Option<Floor>,
        depth: u8,
        excluded: Option<Play>,
    ) -> Result<bool, Aborted> {
        if !self.config.singular_extensions
            || excluded.is_some()
            || !(SINGULAR_MIN_DEPTH..MAX_PLY).contains(&depth)
        {
            return Ok(false);
        }
        self.singular.asked += 1;
        let Some(floor) = floor.filter(|floor| {
            floor.play == tt
                && depth.saturating_sub(floor.depth) <= SINGULAR_ENTRY_SLACK
                && !is_mate(floor.score)
        }) else {
            return Ok(false);
        };
        self.singular.tested += 1;
        // a floor just under the mate threshold puts the window inside the
        // mate band, which the mate distance window and the shortcuts'
        // gates treat as any other mate window; the arithmetic stays in
        // range, the margin being at most two plies a byte
        let singular_beta = floor.score - SINGULAR_MARGIN * Score::from(depth);
        // no pass under the excluded search, which the frame refuses too
        let rest = self.alpha_beta(
            singular_beta - 1,
            singular_beta,
            (depth - 1) / 2,
            false,
            RootBounds::Neither,
            Some(tt),
        )?;
        let singular = rest.score < singular_beta;
        self.singular.extended += u64::from(singular);
        Ok(singular)
    }

    /// The quiet moves put in order as far as the loop reads them, asked
    /// at every place `i`. The front did not cut the node off, so at the
    /// front the quiets are keyed, or sorted whole at a node the ledger
    /// watches, since the ledger records a skipped move at the place the
    /// full sort gives it. From there one move is picked per place until
    /// the fourth, when the rest is sorted, or until a shallow rule turns
    /// on for the rest of the node, when only the moves it would still
    /// search are kept and the rest of the run is dropped. Returns the
    /// place the loop continues from when place `i` opens a dropped run,
    /// which the loop steps past whole.
    #[inline(always)]
    fn order_quiets_at(
        &mut self,
        moves: &mut MoveList,
        i: usize,
        quiets: &mut QuietOrder,
        node: &Node,
        rules: &mut late_move::Rules,
    ) -> Option<usize> {
        let front = quiets.front;
        if i == front {
            if let Some(ply) = quiets.ply {
                // the forced decision instrument asks the shallow rules
                // move by move as the ledger does, since it has to see
                // every skip it may invert
                if self.ledger.is_some() || self.forced.is_some() {
                    self.ordering.order_quiets(
                        &self.board,
                        &mut moves[front..],
                        quiets.losing,
                        ply,
                    );
                } else {
                    let run = self.ordering.key_quiets(
                        &self.board,
                        &mut moves[front..],
                        quiets.losing,
                        ply,
                    );
                    if run > 1 {
                        quiets.lazy = Some((front + run, 0));
                    }
                }
                quiets.scored = true;
            }
        }
        if let Some((end, picks)) = quiets.lazy.as_mut() {
            let ply = quiets.ply.expect("a keyed run has a ply");
            let end = *end;
            if i + 1 >= end {
                quiets.lazy = None;
            } else if rules.shallow_active(&self.deciding(), node) {
                let kept = self.ordering.keep_unskippable(
                    &self.board,
                    &mut moves[front..end],
                    i - front,
                    ply,
                    &mut rules.check,
                );
                // a run whose every move survives drops none, and leaves
                // no run to step past
                if front + kept < end {
                    quiets.dropped = front + kept..end;
                }
                quiets.lazy = None;
                quiets.filtered = true;
            } else if *picks >= 4 {
                self.ordering
                    .sort_rest(&mut moves[front..end], i - front, ply);
                quiets.lazy = None;
            } else {
                self.ordering.pick(&mut moves[front..end], i - front, ply);
                *picks += 1;
            }
        }
        if i >= quiets.dropped.start {
            debug_assert!(i < quiets.dropped.end, "the dropped run is behind the loop");
            let past = quiets.dropped.end;
            quiets.dropped = usize::MAX..usize::MAX;
            return Some(past);
        }
        None
    }

    /// What the node does with the move at hand. The shallow rules are
    /// asked first, since they are the cheaper question and reach depths
    /// the reduction does not. The reduction's gate is asked of a quiet
    /// move alone, since a capture or a promotion is never reduced, and
    /// only where the node admits one. A move passed over is never made, so
    /// whether it was legal is never learned and nothing is taught about
    /// it. The ledger's staged half travels to the scout as a parameter,
    /// so the reduced moves inside it cannot mistake it for their own. The
    /// first move the node searches is no late move, and none of this is
    /// asked of it.
    #[inline(always)]
    fn late_move_decision(
        &mut self,
        node: &Node,
        rules: &mut late_move::Rules,
        moves: &[Play],
        m: &Play,
    ) -> Decision {
        if node.answer.searched == 0 {
            return Decision::First;
        }
        if rules.skips(&self.deciding(), node, m) {
            if self.forced.is_some() && self.forced_skip(node, rules, moves, m) {
                return Decision::UNREDUCED;
            }
            self.record_skip(node, rules, moves, m);
            return Decision::Skip;
        }
        if !rules.admits(node) || m.capture.is_some() || m.promote.is_some() {
            return Decision::UNREDUCED;
        }
        match late_move::decide_admitted(&self.deciding(), node, rules, moves, m) {
            late_move::Verdict::Skip => {
                if self.forced.is_some() && self.forced_skip(node, rules, moves, m) {
                    return Decision::UNREDUCED;
                }
                self.record_skip(node, rules, moves, m);
                Decision::Skip
            }
            late_move::Verdict::Scout(reduction) => Decision::Search {
                reduction,
                staged: self.stage_scout(node, rules, moves, m, reduction),
            },
        }
    }

    /// What answers a node under the root before anything is searched: the
    /// fifty move rule and repetition, or the rail. Every node here sits
    /// below the root, so a
    /// repetition is a draw either side can take; at the root the engine
    /// still has to move.
    fn answered_by_rule(&mut self, in_check: bool) -> Option<Value> {
        if self.board.fifty_move_expired() {
            // a mate delivered by the hundredth half move is a mate. A
            // repeated position cannot be one: it would have ended the game
            // the first time it came up
            if in_check && !self.board.has_legal_move() {
                return Some(Value::mated(self.board.line_ply));
            }
            // where the taint starts: the draw is true of the path that
            // reached this position, not of the position itself
            return Some(Value::tainted(0));
        }
        if self.board.has_repeated() {
            return Some(Value::tainted(0));
        }
        // a line of checks that keeps capturing is ended by neither draw
        // rule nor depth, since the extension holds the depth, so the rail
        // ends it. A static eval, clean because it consulted no path; it
        // gives up the mate a node standing here may be in
        if self.board.line_ply >= MAX_PLY as usize {
            return Some(Value::clean(self.eval()));
        }
        None
    }

    /// A cutoff's value to the table, when the taint policy allows.
    fn store_cutoff(&mut self, m: &Play, value: Value, depth: u8, static_eval: Score) {
        if self.keeps(value) {
            let landed =
                self.transpositions
                    .record_cutoff(&self.board, *m, value, depth, static_eval);
            self.ghi.count_store(landed, value);
        }
    }

    /// A node's answer to the table, when the taint policy allows: the
    /// best move with its score where a move raised alpha, and as a
    /// ceiling where none did, the move beside it then being only the one
    /// that came closest.
    fn store_answer(
        &mut self,
        play: Play,
        value: Value,
        depth: u8,
        raised_alpha: bool,
        static_eval: Score,
    ) {
        if self.keeps(value) {
            let landed = if raised_alpha {
                self.transpositions
                    .record_best(&self.board, play, value, depth, static_eval)
            } else {
                self.transpositions
                    .record_ceiling(&self.board, play, value, depth, static_eval)
            };
            self.ghi.count_store(landed, value);
        }
    }

    /// One full width node answering, offered to the census and the effort
    /// instrument: `cutting` is the move that cut it off, or none when the
    /// loop ran out. Called before the cutoff teaches the memories, so a
    /// row says what the node knew when it chose. A bare check of each
    /// slot in front of a cold call, for `sample`'s reason.
    #[inline(always)]
    fn record_node(
        &mut self,
        node: &Node,
        moves: &[Play],
        quiets_scored: bool,
        cutting: Option<census::Cutting<'_>>,
    ) {
        if self.census.is_some() {
            self.census_event(node, moves, quiets_scored, cutting);
        }
        if self.effort.is_some() {
            self.effort_event(node.depth, cutting.is_some(), node.entered_at);
        }
    }

    /// A move the node passes over, offered to the ledger. Behind a bare
    /// check of the slot, so the search's own path stages nothing.
    #[inline(always)]
    fn record_skip(&mut self, node: &Node, rules: &mut late_move::Rules, moves: &[Play], m: &Play) {
        if self.ledger.is_some() {
            let staged = self.staged_reduction(m, node, rules, moves);
            self.ledger_skip(staged, node);
        }
    }

    /// The ledger's half of a reduced scout, staged at the node, or none
    /// when the move is not reduced or no ledger is armed.
    #[inline(always)]
    fn stage_scout(
        &mut self,
        node: &Node,
        rules: &mut late_move::Rules,
        moves: &[Play],
        m: &Play,
        reduction: u8,
    ) -> Option<reduction::Staged> {
        if reduction > 0 && (self.ledger.is_some() || self.forced.is_some()) {
            Some(self.staged_reduction(m, node, rules, moves))
        } else {
            None
        }
    }

    /// One full width node. `can_null` is false only directly under a
    /// pass. `root_bounds` says which of the two bounds handed in is still
    /// the root's own. `excluded` is the move the node is searched without,
    /// which only the singular test passes: such a node takes no cutoff
    /// from its entry, passes no null move, asks no singular test of its
    /// own and stores nothing.
    fn alpha_beta(
        &mut self,
        alpha: Score,
        beta: Score,
        mut depth: u8,
        can_null: bool,
        root_bounds: RootBounds,
        excluded: Option<Play>,
    ) -> Result<Value, Aborted> {
        self.poll_deadline()?;
        self.selective_depth = self.selective_depth.max(self.board.line_ply as u8);
        self.nodes += 1;
        let entered_at = self.nodes;

        let in_check = self.board.in_check();
        if let Some(value) = self.answered_by_rule(in_check) {
            return Ok(value);
        }
        // mate distance pruning
        let (alpha, beta) = match mate_distance_window(alpha, beta, self.board.line_ply) {
            MateDistanceWindow::Open { alpha, beta } => (alpha, beta),
            // clean: how far a mate can be from here is a property of the
            // position and not of the path that reached it
            MateDistanceWindow::Closed(score) => return Ok(Value::clean(score)),
        };
        if in_check {
            depth += 1;
        }
        if depth == 0 {
            return self.quiescence(alpha, beta);
        }

        let probe = match excluded {
            None => self.probe(alpha, beta, depth),
            Some(_) => self.probe_for_ordering(),
        };
        let pv_play = match probe {
            Probe::Cut(value) => return Ok(value),
            Probe::Order(play) | Probe::Refused(play) => Some(play),
            Probe::Miss => None,
        };
        let mut taint = Taint::default();
        // the node's static evaluation, filled by the shortcuts and read by
        // the late move decision, or found in the table's entry. The entry's
        // floor is read here too, before the shortcuts' searches probe
        // other keys
        let table_eval = self.transpositions.probed_eval(self.board.key);
        let mut eval: Option<Score> = (table_eval != NO_EVAL).then_some(table_eval);
        let floor = self
            .transpositions
            .probed_floor(self.board.key, self.board.line_ply);
        if let Some(value) = self.shortcuts(
            alpha,
            beta,
            depth,
            in_check,
            // a node searched without a move passes no null move: the pass
            // would answer for the excluded move too
            can_null && excluded.is_none(),
            root_bounds,
            &mut taint,
            &mut eval,
        )? {
            return Ok(value);
        }
        // what the node's stores carry into the table
        let static_eval = eval.unwrap_or(NO_EVAL);

        let table_move = pv_play
            .filter(|tt| Some(*tt) != excluded)
            .filter(|tt| self.board.is_pseudo_legal(tt));
        let mut node = Node::open(
            depth,
            in_check,
            self.memory_ply(),
            census::Table::of(pv_play.is_some(), table_move.is_some()),
            entered_at,
            excluded,
            FailSoft::open(alpha, beta, root_bounds, taint),
        );
        if let Some(tt) = table_move {
            let extension = u8::from(self.table_move_is_singular(tt, floor, depth, excluded)?);
            if let Some(value) = self.search_table_move(tt, &mut node, static_eval, extension)? {
                return Ok(value);
            }
        }
        let tt_searched = node.answer.searched > 0;

        let mut moves = MoveList::new();
        let captures = if in_check {
            self.board.evasions_into(&mut moves)
        } else {
            self.board.generate_moves_into(&mut moves)
        };
        let ordered =
            self.ordering
                .order_split(&self.board, &mut moves, captures, pv_play, node.ply);
        // `order_split` sorts by `pv_play`, and the search played
        // `table_move`, which differ when `is_pseudo_legal` refused the move
        let tt_at = if table_move.is_some() {
            ordered.table_at
        } else {
            None
        };
        let mut quiets = QuietOrder::new(&ordered, node.ply);
        let mut rules = late_move::Rules::new(&self.deciding(), &node, eval);
        // a skipped move has no bit, and nor has one that turned out illegal
        let mut made = Searched::default();
        let len = moves.len();
        let mut next = 0;
        while next < len {
            let i = next;
            next += 1;
            if let Some(past) = self.order_quiets_at(&mut moves, i, &mut quiets, &node, &mut rules)
            {
                next = past;
                continue;
            }
            let m = &moves[i];
            if tt_at == Some(i) {
                debug_assert_eq!(table_move, Some(*m), "the place is not the table's move");
                if tt_searched {
                    made.mark(i);
                }
                continue;
            }
            if Some(*m) == excluded {
                continue;
            }
            let decision = self.late_move_decision(&node, &mut rules, &moves, m);
            if let Decision::Skip = decision {
                continue;
            }
            let Some(value) = self.search_child(
                m,
                node.answer.alpha,
                node.answer.beta,
                depth,
                &decision,
                node.answer.root_bounds,
            )?
            else {
                continue;
            };
            made.mark(i);
            match node.answer.absorb(m, value) {
                Reached::Beta => {
                    let cutting = census::Cutting {
                        play: m,
                        reduced: matches!(decision, Decision::Search { reduction: 1.., .. }),
                        table: false,
                    };
                    self.record_node(&node, &moves, quiets.scored, Some(cutting));
                    // the moves the node searched, not the whole list: the
                    // history marks down what the node asked and got
                    // nothing from
                    let tried = moves[..i]
                        .iter()
                        .enumerate()
                        .filter(|(place, _)| made.holds(*place))
                        .map(|(_, tried)| tried);
                    return Ok(self.cutoff(m, tried, &node, value.score, static_eval));
                }
                Reached::Alpha => {
                    // the dropped moves stay dropped only while alpha is
                    // short of a mate. The rules admit no node whose beta
                    // is a mate score, so any mate a survivor finds is at
                    // or above beta and has cut the node off before
                    // reaching here
                    debug_assert!(
                        !(quiets.filtered && is_mate(node.answer.alpha)),
                        "a filtered node raised alpha to a mate without cutting off"
                    );
                }
                Reached::Neither => {}
            }
            debug_assert_eq!(
                made.count(),
                node.answer.searched,
                "a bit for every move made and searched, and for no other"
            );
        }

        // the held half, at the same rate: a cut-only stream would
        // reproduce the censoring the census measures, and a rule moves
        // effort between held nodes and cut ones as well as away from both
        self.record_node(&node, &moves, quiets.scored, None);

        if node.answer.searched == 0 {
            // clean: mate and stalemate are properties of the position
            if in_check {
                return Ok(Value::mated(self.board.line_ply));
            }
            return Ok(Value::clean(0));
        }
        let play = node
            .answer
            .best_move
            .expect("a legal move was found, so one of them is best");
        let value = node.answer.taint.stamp(node.answer.best);
        if excluded.is_none() {
            self.store_answer(play, value, depth, node.answer.raised_alpha(), static_eval);
        }
        Ok(value)
    }

    /// The engine a session starts with, and the size its table was asked
    /// for when the host would not give it. A machine with less memory
    /// than the default assumes plays with a smaller table rather than
    /// failing to start, and `table_bytes` says what it got.
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
        self.transpositions.new_search(self.board.eval.phase());
        self.ordering.forget();
        self.search_within(depth, Limits::unlimited())
    }

    /// One fixed depth search under the limits given.
    ///
    /// The caller owns the table's generation and the quiet memories. A
    /// caller that never starts a generation with `new_search` leaves every
    /// entry looking current, so the oldest entries are never the ones
    /// given up. A search through here keeps whatever the memories learned
    /// before it.
    pub fn search_within(&mut self, depth: u8, limits: Limits) -> SearchOutcome {
        self.root_nodes.clear();
        self.search_root(depth, limits, None, Aspiration::open(None, depth))
    }

    /// The body of one fixed depth search, under the window given.
    /// Everything that may interrupt it arrives in the signature, and the
    /// prologue writes the fields the poll reads.
    ///
    /// Fail soft, and the answer says which of three things its score is.
    /// A score inside the window is the position's worth. A move that
    /// reached beta makes the score a floor: the rest of the moves were
    /// never tried. A window no move reached alpha in makes it a ceiling,
    /// and the move beside it is only the one that came closest, which is
    /// never answered with. At the full window a ceiling cannot happen.
    fn search_root(
        &mut self,
        mut depth: u8,
        limits: Limits,
        stop: Option<Arc<AtomicBool>>,
        window: Aspiration,
    ) -> SearchOutcome {
        // held to the rail here too, so the check extension cannot overflow
        depth = depth.min(MAX_PLY);
        self.begin(limits, stop);
        self.selective_depth = depth;

        if self.poll_deadline().is_err() {
            return SearchOutcome::Aborted(None);
        }
        self.nodes += 1;

        if self.board.in_check() {
            depth += 1;
        }

        let mut answer = FailSoft::open(
            window.alpha,
            window.beta,
            RootBounds::Both,
            Taint::default(),
        );

        // the previous depth's answer is tried first, which the aborted
        // iteration's swap in `iterative_deepening_search` rests on. The
        // debug assertion in `order` holds the table's move at the head
        let pv_play = self.transpositions.ordering_play(&self.board);
        let mut moves = self.board.generate_moves();
        self.ordering.order(&self.board, &mut moves, pv_play, None);

        // the root reduces nothing
        for m in &moves {
            let decision = if answer.searched > 0 {
                Decision::UNREDUCED
            } else {
                Decision::First
            };
            let before = self.nodes;
            let child = self.search_child(
                m,
                answer.alpha,
                answer.beta,
                depth,
                &decision,
                answer.root_bounds,
            );
            self.count_root_nodes(*m, self.nodes - before);
            match child {
                Err(Aborted) => {
                    // only a move that beat the opening alpha may be
                    // answered with
                    let answerable = answer.best_move.filter(|_| answer.raised_alpha());
                    return SearchOutcome::Aborted(
                        answerable.map(|play| self.result_for(play, answer.best)),
                    );
                }
                Ok(None) => {}
                Ok(Some(value)) => {
                    if answer.absorb(m, value) == Reached::Beta {
                        // the rest are the wider re-search's to ask
                        break;
                    }
                }
            }
        }

        if answer.searched == 0 {
            // checkmate or stalemate. An expired fifty move counter is not
            // a way out: that draw is claimable and not automatic (FIDE
            // 9.3), so the side to move may still play
            return SearchOutcome::GameOver;
        }

        let play = answer
            .best_move
            .expect("a legal move was found, so one of them scored best");
        let score = answer.best;
        let bound = if score >= answer.beta {
            ScoreBound::Lower
        } else if answer.raised_alpha() {
            ScoreBound::Exact
        } else {
            ScoreBound::Upper
        };
        self.store_root_answer(play, answer.taint.stamp(score), depth, bound);
        SearchOutcome::Complete(self.result_for(play, score), bound)
    }

    /// Add a root move's nodes to the depth's count.
    fn count_root_nodes(&mut self, play: Play, nodes: u64) {
        match self.root_nodes.iter_mut().find(|(seen, _)| *seen == play) {
            Some((_, counted)) => *counted += nodes,
            None => self.root_nodes.push((play, nodes)),
        }
    }

    /// How the depth just completed spent its nodes at the root, for the
    /// move it chose and the move the depth before chose.
    fn root_nodes_for(&self, chosen: Play, before: Option<Play>) -> RootNodes {
        RootNodes {
            chosen: self
                .root_nodes
                .iter()
                .find(|(play, _)| *play == chosen)
                .map_or(0, |(_, nodes)| *nodes),
            total: self.root_nodes.iter().map(|(_, nodes)| nodes).sum(),
            changed: before.is_some_and(|before| before != chosen),
        }
    }

    /// The root's answer to the table, past the taint policy and the
    /// replacement contest, because the reported line is read back from
    /// this slot. Under `Skip` a tainted root answer is stored too, and the
    /// rare tainted cutoff it then offers is refused as under `Refuse`. A
    /// ceiling is not stored, so the closest move is never promoted over a
    /// move it was not shown to beat.
    fn store_root_answer(&mut self, play: Play, value: Value, depth: u8, bound: ScoreBound) {
        let landed = match bound {
            ScoreBound::Lower => {
                self.transpositions
                    .record_floor_answer(&self.board, play, value, depth)
            }
            ScoreBound::Exact => self
                .transpositions
                .record_answer(&self.board, play, value, depth),
            ScoreBound::Upper => return,
        };
        self.ghi.count_store(landed, value);
    }

    /// Replay the line the table holds on a copy of the board, checking
    /// each stored move is legal there and stopping at a draw.
    pub fn pv_line(&self) -> PvLine {
        self.pv_line_from(self.transpositions.intended_play(&self.board))
    }

    /// The same line read from a first move given rather than from the
    /// table's, for a root answer the table does not hold (an aborted
    /// iteration's swap, or a ceiling).
    fn pv_line_from(&self, first: Option<Play>) -> PvLine {
        let mut line = Vec::new();
        let mut board = self.board.detached();
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
        // landed inside its window, or a floor a later depth proved
        let mut best: Option<SearchResult> = None;
        // what the next window is opened around. Never a floor, which is a
        // bound and not a score
        let mut exact: Option<Score> = None;
        let mut total_nodes: u64 = 0;
        // how the last completed depth spent its nodes at the root, which
        // sets the soft line for the next
        let mut last: Option<RootNodes> = None;
        // the move the last completed depth chose. Not `best`, which a
        // floor in the depth under way can have replaced
        let mut chosen: Option<Play> = None;
        let max_depth = match search_options.depth {
            // held to the rail, or the depths past it would each rerun it
            Some(depth) => depth.min(MAX_PLY),
            None => MAX_PLY,
        };
        // one generation for every iteration, and the memories kept from
        // one iteration to the next
        self.transpositions.new_search(self.board.eval.phase());
        self.ordering.forget();

        for depth in 1..=max_depth {
            // the soft bound, asked once a depth rather than before each
            // re-search: giving up inside a fail low would answer with the
            // move the search has just found worse than it believed. The
            // deadline stays as the backstop
            if !search_options.limits.worth_another_iteration(last) {
                return SearchOutcome::Aborted(best);
            }
            let mut window =
                Aspiration::open(self.config.aspiration.then_some(exact).flatten(), depth);
            self.root_nodes.clear();
            loop {
                let (limits, stop) = search_options.for_iteration(best.is_some(), total_nodes);
                match self.search_root(depth, limits, stop, window) {
                    SearchOutcome::Aborted(deeper) => {
                        // the interrupted search's best outranks what
                        // answers now, whenever it has one. The move it
                        // hands back beat the window's alpha, and the moves
                        // it never reached could only raise the score, so
                        // the score is a floor. The swap is sound because
                        // the move it replaces was the first move tried:
                        // the root orders by the table's entry, which is
                        // the last answer or a floor shown better than it.
                        // Without that the new move would be better only
                        // over a subset the old one need not belong to.
                        return SearchOutcome::Aborted(match deeper {
                            Some(mut result) => {
                                result.nodes += total_nodes;
                                // no completed depth named this move, so it
                                // is reported here as the bound it is
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
                            // answered before it answers still. Depth one
                            // runs without limits, so there always is one.
                            //
                            // The nodes this iteration spent are counted
                            // all the same, or a search stopped on its
                            // budget would report fewer nodes than the
                            // budget. `self.nodes` is this iteration's and
                            // `total_nodes` every iteration before it. The
                            // elapsed time is rewritten with them, for the
                            // reason `SearchResult::elapsed` gives.
                            None => best.map(|mut answered| {
                                answered.nodes = total_nodes + self.nodes;
                                answered.elapsed = self.limits.elapsed();
                                answered
                            }),
                        });
                    }
                    SearchOutcome::GameOver => {
                        return SearchOutcome::GameOver;
                    }
                    SearchOutcome::Complete(mut result, bound) => {
                        // a failed search spent its nodes too
                        total_nodes += result.nodes;
                        result.nodes = total_nodes;
                        // a ceiling stored nothing, so its line is read
                        // from the move itself
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
                            last = Some(self.root_nodes_for(result.best_move, chosen));
                            #[cfg(test)]
                            self.soft_lines.extend(last);
                            chosen = Some(result.best_move);
                            exact = Some(result.score);
                            best = Some(result);
                            break;
                        }
                        if bound == ScoreBound::Lower {
                            // a move worth at least beta outranks what
                            // answers now, which was tried first here and
                            // came back under beta. Held as the answer in
                            // case the wider search is interrupted before it
                            // reaches the move again
                            best = Some(result);
                        }
                        // search the depth again with the failed side
                        // widened, which ends at the full window
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

    fn make_move_str(&mut self, play: &str) -> Result<(), Unplayable> {
        self.board.play_by_name(play)
    }

    fn board_display(&self) -> String {
        self.board.to_string()
    }
}

pub struct PvLine {
    line: Vec<Play>,
}

impl PvLine {
    /// A line built by hand, for a protocol adapter's tests.
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

/// The verdict of a search of the root, fixed depth or deepening.
#[derive(Debug)]
pub enum SearchOutcome {
    /// The search finished the requested depth, with what its score says
    /// about the position beside it. A search at the full window is always
    /// exact.
    Complete(SearchResult, ScoreBound),
    /// The root has no play to make: checkmate or stalemate.
    GameOver,
    /// A limit ran out partway through, carrying an answer when there is
    /// one. From one fixed depth search its score is a floor: the moves
    /// the root never reached could only raise it. From the deepening loop
    /// it is whatever answered last, which may be a completed depth's
    /// exact score.
    Aborted(Option<SearchResult>),
}

/// What a reported score says about the position.
///
/// `Exact`: every root move was searched and the score landed inside the
/// window. `Lower` is a floor, from an aborted iteration or from a root
/// move that reached beta. `Upper` is a ceiling, from an iteration no root
/// move reached alpha in, and the move beside it is only the one that came
/// closest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoreBound {
    Exact,
    Lower,
    Upper,
}

/// The search hit a limit and unwound without finishing. The score of an
/// aborted frame is meaningless; returning this instead keeps it out of the
/// transposition table, since propagation with `?` never reaches the
/// stores.
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
mod tests;
