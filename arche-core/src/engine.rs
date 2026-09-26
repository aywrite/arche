// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use crate::board::{Board, MOVE_LIST_INLINE, Unplayable};
use crate::census;
use crate::effort;
use crate::eval;
use crate::late_move;
use crate::limits::Limits;
use crate::misc::{Color, Piece, Score};
use crate::ordering::{MoveOrdering, Ordered};
use crate::play::Play;
use crate::recorder::{Sampler, Window};
use crate::reduction;
use crate::residual::{Sample, Shortcut};
use crate::transposition::{
    DEFAULT_TABLE_BYTES, GhiCounters, Probe, SignatureCounters, TranspositionTable,
};
use crate::value::{
    MateDistanceWindow, Taint, Value, below_the_mate_window, is_mate, mate_distance_window,
};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time;

/// The ply every search stops at: a requested depth is held to it, the
/// full width search ends a line at it whatever depth the check extension
/// has left, and the reported line is walked no further. It also sizes the
/// ordering's per ply tables.
///
/// It sits well inside the board's history ring (1024 plies, less the
/// fifty move window) and the mate score window (a thousand under the mate
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
// at which a depth four mate in two is lost. On the default search that
// boundary read between eighty five and ninety one while it could be read
// there; the default's other shortcuts now lose the mate at every margin
// from sixty to a hundred. With this shortcut alone on the reference the
// boundary is seventy seven, which
// the_reverse_futility_margin_keeps_the_depth_four_mate pins. That test
// guards only a cut below seventy seven, so re-measure before moving the
// figure. docs/ROADMAP.md has the shadow lane's reading.
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
// Chosen by a bench sweep of ten to forty at depth nine as the widest
// width within a percent of the cheapest that also cost less than opening
// full (5902681 has the table).
const ASPIRATION_WIDTH: Score = 30;
// The first depth the root opens narrow at. Below it the whole iteration
// costs less than one re-search deeper down. A judgment rather than a
// swept figure.
const ASPIRATION_MIN_DEPTH: u8 = 5;
// How many times one side of the window may fail before that side opens to
// the edge. The width doubles each time, so the sides tried are the width,
// twice it, four times it, and then the edge.
const ASPIRATION_FAILURES: u8 = 3;

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
/// Four words, because a position can hold two hundred and eighteen moves.
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

    /// How many places are marked. The move loop asserts this against its
    /// own searched count, so a stray mark fails at the next move rather
    /// than showing up as a malus elsewhere in the tree.
    fn count(&self) -> usize {
        self.0.iter().map(|word| word.count_ones() as usize).sum()
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
    /// Whether the scout of a late quiet the gate prices as dead runs a ply
    /// shallower still. Rides on `late_move_reductions`.
    pub deep_reductions: bool,
    /// Whether a late quiet the attention model prices in its deadest band
    /// is searched at all. Rides on `late_move_reductions`.
    pub late_move_pruning: bool,
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
    /// Whether the deep reduction's extra ply is decided by the move's
    /// index against a floor that rises with depth rather than by the
    /// attention model's threshold. Rides on `deep_reductions`.
    pub deep_index_rule: bool,
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
    pub const SWITCHES: [(&'static str, TurnOff); 14] = [
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
        ("quiet_futility", |config| config.quiet_futility = false),
        ("late_move_count", |config| config.late_move_count = false),
        ("reduction_table", |config| config.reduction_table = false),
        ("deep_index_rule", |config| config.deep_index_rule = false),
        ("move_memory", |config| config.move_memory = false),
        ("aspiration", |config| config.aspiration = false),
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
            quiet_futility: false,
            late_move_count: false,
            reduction_table: false,
            deep_index_rule: false,
            move_memory: false,
            aspiration: false,
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
            quiet_futility: true,
            late_move_count: true,
            reduction_table: true,
            deep_index_rule: true,
            move_memory: true,
            aspiration: true,
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
        let outers: Vec<(&str, Vec<u64>)> =
            ["null_move", "late_move_reductions", "deep_reductions"]
                .into_iter()
                .map(|outer| (outer, nodes(one(outer).config())))
                .collect();
        for (outer, inner) in [
            ("null_move", "adaptive_null_move"),
            ("late_move_reductions", "deep_reductions"),
            ("late_move_reductions", "late_move_pruning"),
            ("late_move_reductions", "reduction_table"),
            ("late_move_reductions", "deep_index_rule"),
            ("deep_reductions", "deep_index_rule"),
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

/// Which of a node's two bounds is still the one the root opened with,
/// rather than a score a search returned. That is narrower than being a
/// principal variation node: a node searched at an open window can have
/// neither bit set, and a node at a zero window never has one.
///
/// A bound the root opened with is one the tree under it has said nothing
/// about, so the shortcuts and the late move reduction are refused wherever
/// beta is one: the principal variation exemption. The shortcuts are also
/// refused at every open window, which `shortcuts` reads from the bounds.
/// The bits change only in `child` and `alpha_raised`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RootBounds {
    pub(crate) alpha: bool,
    pub(crate) beta: bool,
}

/// The searches a node asks of a child, which the bits a child carries are
/// keyed on.
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
    /// round with it. The other three take a zero window, which is this
    /// node's own question about alpha, so they take neither bound.
    fn child(self, search: ChildSearch) -> Self {
        match search {
            ChildSearch::FirstMove | ChildSearch::Proof => Self {
                alpha: self.beta,
                beta: self.alpha,
            },
            ChildSearch::Scout | ChildSearch::Probe | ChildSearch::Pass => Self::NEITHER,
        }
    }

    /// What a raise of alpha leaves: alpha is a returned score from there
    /// on, and beta is untouched.
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

    /// At a full window every bit the search reads sits beside a mate
    /// gate that answers the same way, so the reference's pinned counts
    /// cannot see a wrong bit and this is what holds the rule. The cases
    /// start from asymmetric bounds because a flip the wrong way round is
    /// invisible on a pair that agree.
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

    /// What the node knew about a move at the gate, gathered for a
    /// ledger row: the staged half of a scouted event, and the whole of
    /// a skipped one.
    // cold and out of line behind a bare is_some, for `sample`'s reason
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
    /// low one.
    #[cold]
    #[inline(never)]
    fn ledger_skip(&mut self, staged: reduction::Staged, depth: u8, alpha: Score, beta: Score) {
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
    /// which move cut it off, or none when the loop ran out.
    ///
    /// The killers and the history are read before `cutoff` teaches them
    /// the move, so a row says what the node knew when it chose. The
    /// evaluation is computed only for kept events, exact rather than a
    /// cache read.
    // cold and out of line behind a bare is_some, for `sample`'s reason
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

    /// Whether a result may be stored under the taint policy. A refused
    /// store is counted as skipped.
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

    pub fn config(&self) -> SearchConfig {
        self.config
    }

    /// How much of the search's use of the transposition table depended on
    /// the path taken rather than on the position.
    pub fn ghi(&self) -> GhiCounters {
        self.transpositions.ghi()
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

    /// What a capture search makes of the position this engine holds, over
    /// the open window and under no limits, so what comes back is a value.
    /// For the tuner's quiet test. `quiescence` itself stays private,
    /// because a caller free to choose the window could read a bound as a
    /// value.
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

        // a side in check cannot stand pat, so its static eval is no floor.
        // The full search never enters here in check (the extension
        // searches those nodes full width), so a check seen here was
        // delivered by a capture searched here. Fail soft.
        let mut best = Score::MIN + 1;
        let in_check = self.board.in_check();
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
        // the delta test at the starting alpha, before the order prices
        // each capture with the swap. Alpha only rises, so the loop would
        // skip every capture dropped here. Two cases are left to the loop
        // because filtering them would change the search: under a mate beta
        // a mating capture can lift alpha into the mate window, after which
        // the loop searches every capture, and a list that spills the
        // buffer is ordered with no losing band (`MoveOrdering::order`)
        if let Some(standing) = standing {
            if self.config.delta_margin
                && !is_mate(alpha)
                && !is_mate(beta)
                && moves.len() <= MOVE_LIST_INLINE
            {
                moves.retain(|m| match m.capture {
                    Some(captured) if m.promote.is_none() => {
                        !short_of_alpha(standing, captured, alpha)
                    }
                    _ => true,
                });
            }
        }
        // no memories here: they say nothing about captures or evasions
        let Ordered { front, .. } = self.ordering.order(&self.board, &mut moves, pv_play, None);

        // quiescence never reads a draw itself, but a probe trusting
        // tainted scores can cut on one inside a capture tree
        let mut taint = Taint::default();
        let mut found_legal_move = false;
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
                if !is_mate(alpha) && m.promote.is_none() {
                    if self.config.delta_margin && short_of_alpha(standing, captured, alpha) {
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
                found_legal_move = true;
                // undo before an abort can propagate
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
    /// window asks for the node's score rather than a bound on it. The bit
    /// alone would leave the proof after a probe fails high open to both
    /// shortcuts, since it takes the window turned round with its beta bit
    /// clear. The reductions and the shallow rules still read the bit alone:
    /// exempting every open window from them as well measured a loss.
    ///
    /// A `Some` answers the node. A pass that failed answers nothing but
    /// leaves whatever it read in the node's taint.
    ///
    /// `eval_memo` is filled wherever the gates passed and an evaluation
    /// was read, fired or not, so the move loop does not evaluate twice.
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
        eval_memo: &mut Option<i64>,
    ) -> Result<Option<Value>, Aborted> {
        let margin = self.config.reverse_futility && depth <= REVERSE_FUTILITY_MAX_DEPTH;
        // no pass directly under a pass, or the search would answer a
        // position from a line neither side moved in. Unreachable while the
        // eval gate below stands (the window and the eval turn round under a
        // pass), and kept for the day that gate is dropped or given a margin
        let pass = self.config.null_move && can_null && depth >= NULL_MOVE_MIN_DEPTH;
        if (!margin && !pass)
            || in_check
            || !self.board.has_non_pawn_material()
            || is_mate(beta)
            || root_bounds.beta
            // the open window, spelt without the subtraction, which
            // overflows a Score at the full window
            || alpha + 1 < beta
        {
            return Ok(None);
        }
        let eval = self.eval();
        *eval_memo = Some(i64::from(eval));

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
                if self.sampler.is_some() {
                    self.sample(Shortcut::ReverseFutility, depth, floor, alpha, beta, eval);
                }
                return Ok(Some(Value::clean(floor)));
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
            );
            // undo before an abort can propagate
            self.board.undo_null_move();
            let value = -result?;
            if value.score >= beta {
                // a mate found through a pass is not a mate, since the pass
                // is not a legal move, so the score is held under the window
                // a caller reads mates in
                let score = below_the_mate_window(value.score);
                if self.sampler.is_some() {
                    self.sample(Shortcut::NullMove, depth, score, alpha, beta, eval);
                }
                return Ok(Some(Value::with_taint(score, value.tainted)));
            }
            taint.absorb(value);
        }
        Ok(None)
    }

    /// One child of a full width node, or nothing when the move is not
    /// legal here. The undo comes before the abort propagates; propagating
    /// is what keeps an aborted frame's meaningless score away from every
    /// store above. `reduction` is how many plies shallower the scout runs,
    /// zero for no scout, and `staged` is what the ledger has about the
    /// move, or nothing.
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
            // `late_move::amount` clamps the reduction to `depth - 2`
            debug_assert!(depth > reduction + 1, "the scout would be quiescence");
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
            // the scout's fail high asked for the searches below, so they
            // carry its taint
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
        )?;
        Ok(Value::with_taint(
            proof.score,
            proof.tainted || probe.tainted || tainted,
        ))
    }

    /// A fail high at a full width node: the move that proved it goes to
    /// the quiet memories with the moves the node searched before it and,
    /// when the taint policy allows, to the table.
    ///
    /// `tried` is the moves the node searched, not the whole list: the
    /// history marks down what the node asked and got nothing from.
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
    /// pass. `root_bounds` says which of the two bounds handed in is still
    /// the root's own.
    #[allow(clippy::too_many_lines)]
    fn alpha_beta(
        &mut self,
        alpha: Score,
        beta: Score,
        mut depth: u8,
        can_null: bool,
        mut root_bounds: RootBounds,
    ) -> Result<Value, Aborted> {
        self.poll_deadline()?;
        self.selective_depth = self.selective_depth.max(self.board.line_ply as u8);
        self.nodes += 1;
        let entered_at = self.nodes;

        // every node here sits below the root, so a repetition is a draw
        // either side can take; at the root the engine still has to move
        let in_check = self.board.in_check();
        if self.board.fifty_move_expired() {
            // a mate delivered by the hundredth half move is a mate. A
            // repeated position cannot be one: it would have ended the game
            // the first time it came up
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
        // a line of checks that keeps capturing is ended by neither draw
        // rule nor depth, since the extension holds the depth, so the rail
        // ends it. A static eval, clean because it consulted no path; it
        // gives up the mate a node standing here may be in
        if self.board.line_ply >= MAX_PLY as usize {
            return Ok(Value::clean(self.eval()));
        }

        // mate distance pruning
        let (mut alpha, beta) = match mate_distance_window(alpha, beta, self.board.line_ply) {
            MateDistanceWindow::Open { alpha, beta } => (alpha, beta),
            // clean: how far a mate can be from here is a property of the
            // position and not of the path that reached it
            MateDistanceWindow::Closed(score) => return Ok(Value::clean(score)),
        };

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
        // fail soft, as in quiescence
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

        // the node's static evaluation, filled by the shortcuts and read by
        // the late move decision
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

        // the table's move sorts ahead of everything else, so it is searched
        // before the rest are generated: the nodes it cuts never generate or
        // sort at all, and the tree searched is unchanged
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
                            // before the cutoff teaches the memories
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
                            return Ok(self.cutoff(&tt, &[], taint, tt_score, depth));
                        }
                        alpha = tt_score;
                        root_bounds = root_bounds.alpha_raised();
                    }
                }
            }
        }

        let tt_searched = found_legal_move;

        let mut moves = if in_check {
            self.board.evasions()
        } else {
            self.board.generate_moves()
        };
        let ply = self.memory_ply();
        let Ordered {
            front,
            table_at,
            losing,
        } = self.ordering.order(&self.board, &mut moves, pv_play, ply);
        // the place the loop passes over, since the table's move was searched
        // above. `order` sorts by `pv_play` and the search played
        // `tt_tried`, which differ when `is_pseudo_legal` refused the move
        let tt_at = if tt_tried.is_some() { table_at } else { None };

        let mut searched = usize::from(found_legal_move);
        // a skipped move has no bit, and nor has one that turned out illegal
        let mut made = Searched::default();
        let mut quiets_scored = false;
        // computed by the first move that needs it and read back for the
        // rest, since the node facts below are built afresh for each move
        let mut history_max: Option<i32> = None;
        let mut check_info: Option<crate::board::CheckInfo> = None;
        let tt = census::Table::of(pv_play.is_some(), tt_tried.is_some());
        let mut shallow = late_move::shallow(
            &self.config,
            &self.board,
            depth,
            in_check,
            beta,
            root_bounds,
        );
        // the quiet run still being taken in order: where it ends and how
        // many moves have been picked from it one at a time. After four
        // picks the rest is sorted whole
        let mut lazy: Option<(usize, u8)> = None;
        let mut filtered = false;
        // the places from which the rest of the run is known to be dropped
        let mut dropped = usize::MAX..usize::MAX;
        for i in 0..moves.len() {
            // the front did not cut this node off, so the quiets are keyed
            // and put in order only as far as the node reads them
            if i == front {
                if let Some(ply) = ply {
                    // the ledger records a skipped move at the place the full
                    // sort gives it, so a node it watches is ordered whole
                    if self.ledger.is_some() {
                        self.ordering
                            .order_quiets(&self.board, &mut moves[front..], losing, ply);
                    } else {
                        let run =
                            self.ordering
                                .key_quiets(&self.board, &mut moves[front..], losing, ply);
                        if run > 1 {
                            lazy = Some((front + run, 0));
                        }
                    }
                    quiets_scored = true;
                }
            }
            if let Some((end, picks)) = lazy.as_mut() {
                let ply = ply.expect("a keyed run has a ply");
                let end = *end;
                if i + 1 >= end {
                    lazy = None;
                } else if shallow.active(&self.deciding(), &mut eval, searched, alpha) {
                    let kept = self.ordering.keep_unskippable(
                        &self.board,
                        &mut moves[front..end],
                        i - front,
                        ply,
                        &mut check_info,
                    );
                    dropped = front + kept..end;
                    lazy = None;
                    filtered = true;
                } else if *picks >= 4 {
                    self.ordering
                        .sort_rest(&mut moves[front..end], i - front, ply);
                    lazy = None;
                } else {
                    self.ordering.pick(&mut moves[front..end], i - front, ply);
                    *picks += 1;
                }
            }
            if dropped.contains(&i) {
                continue;
            }
            let m = &moves[i];
            if tt_at == Some(i) {
                debug_assert_eq!(tt_tried, Some(*m), "the place is not the table's move");
                if tt_searched {
                    made.mark(i);
                }
                continue;
            }
            // the shallow rules read none of the node facts below, and reach
            // depths the reduction does not, so they are asked first
            if shallow.skips(
                &self.deciding(),
                &mut eval,
                &mut check_info,
                m,
                searched,
                alpha,
            ) {
                // never made, so whether it was legal is never learned, and
                // nothing is taught about it
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
                        check: &mut check_info,
                    };
                    let staged = self.staged_reduction(m, searched, &mut node);
                    self.ledger_skip(staged, depth, alpha, beta);
                }
                continue;
            }
            // the node's facts, built per move because alpha rises and the
            // list is sorted under the loop, and only where the node admits
            // a reduction: most moves are searched whole. The ledger's
            // staged half travels to the scout as a parameter so the
            // reduced moves inside it cannot mistake it for their own
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
                    check: &mut check_info,
                };
                match late_move::decide(&self.deciding(), &mut node, m, searched) {
                    late_move::Verdict::Skip => {
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
                    // before the cutoff teaches the memories
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
                    let tried = moves[..i]
                        .iter()
                        .enumerate()
                        .filter(|(place, _)| made.holds(*place))
                        .map(|(_, tried)| tried);
                    return Ok(self.cutoff(m, tried, taint, score, depth));
                }
                alpha = score;
                // the dropped moves stay dropped only while alpha is short of
                // a mate. The rules admit no node whose beta is a mate score,
                // so any mate a survivor finds is at or above beta and has
                // cut the node off before reaching here
                debug_assert!(
                    !(filtered && is_mate(alpha)),
                    "a filtered node raised alpha to a mate without cutting off"
                );
                root_bounds = root_bounds.alpha_raised();
            }
        }

        // the held half, at the same rate: a cut-only stream would
        // reproduce the censoring the census measures
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
        // the held half: a rule moves effort between held nodes and cut ones
        // as well as away from both
        if self.effort.is_some() {
            self.effort_event(depth, false, entered_at);
        }

        if !found_legal_move {
            // clean: mate and stalemate are properties of the position
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

    /// One fixed depth search under the limits given.
    ///
    /// The caller owns the table's generation and the quiet memories. A
    /// caller that never starts a generation with `new_search` leaves every
    /// entry looking current, so the oldest entries are never the ones
    /// given up. A search through here keeps whatever the memories learned
    /// before it.
    pub fn search_within(&mut self, depth: u8, limits: Limits) -> SearchOutcome {
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
        let mut root_bounds = RootBounds::BOTH;
        // the fail soft answer, whether or not anything reached alpha
        let mut top: Option<(Play, Score)> = None;
        let mut found_legal_move = false;
        let mut taint = Taint::default();

        // the previous depth's answer is tried first, which the aborted
        // iteration's swap in `iterative_deepening_search` rests on. The
        // debug assertion in `order` holds the table's move at the head
        let pv_play = self.transpositions.ordering_play(&self.board);
        let mut moves = self.board.generate_moves();
        self.ordering.order(&self.board, &mut moves, pv_play, None);

        // the root reduces nothing
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
                    // only a move that beat the opening alpha may be
                    // answered with
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
                        root_bounds = root_bounds.alpha_raised();
                    }
                    if score >= beta {
                        // the rest are the wider re-search's to ask
                        break;
                    }
                }
            }
        }

        if !found_legal_move {
            // checkmate or stalemate. An expired fifty move counter is not
            // a way out: that draw is claimable and not automatic (FIDE
            // 9.3), so the side to move may still play
            return SearchOutcome::GameOver;
        }

        let (play, score) = top.expect("a legal move was found, so one of them scored best");
        let value = taint.stamp(score);
        // the answer and the floor are stored past the depth contest,
        // because the reported line is read back from this slot. A ceiling
        // is not stored, so the closest move is never promoted over a move
        // it was not shown to beat
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
        let max_depth = match search_options.depth {
            // held to the rail, or the depths past it would each rerun it
            Some(depth) => depth.min(MAX_PLY),
            None => MAX_PLY,
        };
        // one generation for every iteration, and the memories kept from
        // one iteration to the next
        self.transpositions.new_search();
        self.ordering.forget();

        for depth in 1..=max_depth {
            // the soft bound, asked once a depth rather than before each
            // re-search: giving up inside a fail low would answer with the
            // move the search has just found worse than it believed. The
            // deadline stays as the backstop
            if !search_options
                .limits
                .worth_another_iteration(best.is_some())
            {
                return SearchOutcome::Aborted(best);
            }
            let mut window =
                Aspiration::open(self.config.aspiration.then_some(exact).flatten(), depth);
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

    /// The default table is 256MB, and one per test dominated the suite's
    /// memory and run time.
    const TABLE_BYTES: usize = 16 * 1024 * 1024;

    fn engine(board: Board) -> AlphaBeta {
        AlphaBeta::with_table_bytes(board, TABLE_BYTES)
    }

    #[test]
    fn a_resized_table_is_the_size_asked_for_and_still_searched_on() {
        let mut e = engine(Board::new());
        assert!(e.set_table_bytes(1024 * 1024));

        // whole buckets, as a new engine asked for a megabyte would have
        assert_eq!(
            e.table_bytes(),
            AlphaBeta::with_table_bytes(Board::new(), 1024 * 1024).table_bytes()
        );
        assert!(e.table_bytes() <= 1024 * 1024);
        assert!(matches!(e.search(4), SearchOutcome::Complete(_, _)));
    }

    #[test]
    fn a_table_there_is_no_memory_for_leaves_the_old_one_in_place() {
        // both ways of failing: usize::MAX is refused before the allocator
        // is reached, while isize::MAX is a size it may describe and no
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
        let mut e = engine(Board::new());
        assert!(e.set_table_bytes(0));
        assert!(e.table_bytes() > 0);
        assert!(matches!(e.search(3), SearchOutcome::Complete(_, _)));
    }

    /// The reference search, for the tests that hold it to answering the
    /// same whatever the table holds.
    fn reference(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(board, TABLE_BYTES, SearchConfig::reference())
    }

    /// The reference with reverse futility on and nothing else touched.
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

    /// `passing` with the adaptive reduction on.
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

    /// The reference with the quiet memories on, which move the tree and
    /// never the answer.
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
        // a search of another position first once left entries that made
        // this losing position look better
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
        // back. A ceiling planted one point inside the probe's window and
        // outside the re-search's makes the probe fail high short of the
        // exact score, which comes from an engine of its own; only the
        // re-search's answer matches it.
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
    fn a_proven_mate_is_not_searched_again_a_ply_deeper() {
        // The bench position `wac 4`, a mate in two. When mate distance
        // pruning landed (c7730f1) depth nine searched 385 times depth
        // five's nodes without it and 15 times with it. With the attention
        // weights refitted on game positions it reads 98 times with it and
        // 384 without. The bound holds that shape loosely; bench.rs pins
        // the exact counts.
        const FEN: &str = "r1bq2rk/pp3pbp/2p1p1pQ/7P/3P4/2PB1N2/PP3PPR/2KR4 w - - 0 1";
        let at_five = completed(engine(Board::from_fen(FEN).unwrap()).search(5));
        let at_nine = completed(engine(Board::from_fen(FEN).unwrap()).search(9));
        assert_eq!(
            at_five.checkmate_in(),
            Some(2),
            "the mate moved at depth five"
        );
        assert_eq!(
            at_nine.checkmate_in(),
            Some(2),
            "the mate moved at depth nine"
        );
        assert!(
            at_nine.nodes < at_five.nodes * 150,
            "depth nine searched {} nodes against depth five's {}",
            at_nine.nodes,
            at_five.nodes
        );
    }

    #[test]
    fn the_mate_distance_survives_a_deeper_warm_search() {
        // Searching again deeper off a warm table reuses mate scores stored
        // at other plies, and the distance reported must not move. Whether
        // a given depth finds this mate under the shortcuts is not monotone
        // in the depth (the late move count loses it at three to five), so
        // the test asks that no depth disagree and that several find it.
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - - 0 0").unwrap();
        let mut e = engine(game);
        let mut found = 0;
        for depth in 3..=8 {
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
        assert!(found > 1, "{found} of the depths saw the mate");
    }

    /// What holds `REVERSE_FUTILITY_MARGIN` above the boundary its comment
    /// gives: with the shortcut the only thing added to the reference, a
    /// margin of seventy six or less cuts off the line this mate is found
    /// in. Cold, so no table decides it.
    #[test]
    fn the_reverse_futility_margin_keeps_the_depth_four_mate() {
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - - 0 0").unwrap();
        let result = completed(shortcut(game).search(4));
        assert_eq!(result.checkmate_in(), Some(2));
        assert_eq!(format!("{}", result.best_move), "g3g6");
    }

    #[test]
    fn checkmate_in_one_is_found_for_black() {
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbNQp/3pN3/2pP4/2P5/PPB4P/R4RK1 b - - 1 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(4));
        assert_eq!(result.checkmate_in(), Some(-1));
    }

    /// Material that cannot mate is searched as the draw it is. Before the
    /// rule each of these read as a win of three pawns or more.
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
    /// drawn, which is why the rule sits in the evaluation and not at the
    /// node: a `Value::clean(0)` returned from `alpha_beta` before the moves
    /// were generated would lose this. Two knights cannot force mate, but
    /// the black king here stands in a helpmate.
    #[test]
    fn a_helpmate_survives_the_rule() {
        let mut e = engine(Board::from_fen("k7/3N4/1K6/1N6/8/8/8/8 w - - 0 1").unwrap());
        let result = completed(e.search(5));
        assert_eq!(result.checkmate_in(), Some(1));
        assert_eq!(format!("{}", result.best_move), "b5c7");
    }

    #[test]
    fn quiescence_does_not_stand_pat_out_of_a_mate() {
        // the queen on a8 hangs and taking it loses: Rxa8 Nxf2 is mate, by a
        // capture two plies into quiescence, where the mated node used to
        // stand pat as though it could decline to move
        let game = Board::from_fen("q7/7k/8/8/6n1/8/5PPP/R5RK w - - 0 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(1));
        assert_ne!(format!("{}", result.best_move), "a1a8");
    }

    #[test]
    fn a_capture_that_cannot_reach_alpha_is_not_searched() {
        // one capture on the board: one node when it is skipped, two when
        // it is searched
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

        // at the edge the capture is searched
        let mut e = engine(Board::from_fen(fen).unwrap());
        let alpha = standing + gain + super::DELTA_MARGIN;
        assert!(e.quiescence(alpha, alpha + 1).is_ok());
        assert_eq!(e.nodes, 2);
    }

    #[test]
    fn an_evasion_is_searched_whatever_the_margin_says() {
        // taking the checking queen is the one evasion, at an alpha no
        // capture could reach under the margin
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
        // past what any margin allows
        let fen = "r6k/1P6/8/8/8/8/8/7K w - - 0 1";
        let mut e = engine(Board::from_fen(fen).unwrap());
        assert!(e.quiescence(10_000, 10_001).is_ok());
        assert!(e.nodes > 1, "no promotion was searched");
    }

    #[test]
    fn a_mating_capture_is_searched_whatever_the_margin_says() {
        // rook takes rook and mates on the back rank, asked under an alpha
        // inside the mate window, where the margin would call every capture
        // hopeless
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
        // the one capture, the rook taking the e4 pawn, loses the rook to
        // the d5 pawn
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
        // a winning capture and an even one
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
        // the queen taking the checking knight is the one evasion, and the
        // d3 pawn takes her back. Three nodes: this one, the capture and
        // the recapture
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
        // the pawn takes the rook on a8 and promotes, and the b8 rook takes
        // the queen back. Today's swap prices no promoting capture as
        // losing, so the exemption has nothing to catch; this holds the
        // promise for a swap that one day does
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
        // back is pinned, which the swap does not see, so it prices the
        // capture as losing. Under a mate window alpha the exemption stands
        // the skip down; under an ordinary window the same capture is
        // skipped, which says the exemption and not the swap saved it
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

        // wide, because a narrow window would cut the node off on the even
        // capture of the knight before either arm reached the queen's
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
        // the rook can win the knight or take the pawn one step from
        // promoting. The push captures nothing, so quiescence used not to
        // generate it and the knight looked free to take
        let game = Board::from_fen("4k3/8/8/R5n1/8/8/p5K1/8 w - - 0 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(1));
        assert_eq!(format!("{}", result.best_move), "a5a2");
    }

    #[test]
    fn a_shallow_search_still_sees_the_recapture() {
        // the queen can take a defended pawn, and at depth one only
        // quiescence sees the recapture
        let game = Board::from_fen("4k3/8/3p4/2p5/8/2Q5/8/4K3 w - - 0 1").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(1));
        assert_ne!(format!("{}", result.best_move), "c3c5");
    }

    #[test]
    fn deepening_through_shallow_depths_matches_a_cold_search() {
        // iterations shallower than four used to store scores whose leaves
        // were never quiesced, so the same depth answered differently warm
        // than cold. a_warm_cache_matches_a_cold_search searches each depth
        // directly and cannot see this
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
        // the reference with the window on: a depth reached by deepening
        // under a window answers as one searched directly, so a failure
        // here is a failure of the schedule
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
            // the pinned positions have one best move each with the fitted
            // table; the test rank's ties a queen and a rook promotion, and
            // the two searches break the tie differently
            if !cfg!(feature = "machine-test") {
                assert_eq!(
                    format!("{}", result.best_move),
                    format!("{}", expected.best_move),
                    "best move differs for {}",
                    fen
                );
            }
            narrowed_somewhere |= result.nodes != full.nodes;
        }
        assert!(
            narrowed_somewhere,
            "the window cost nothing anywhere, so the answers prove nothing"
        );
    }

    #[test]
    fn a_losing_side_plays_for_the_fifty_move_draw() {
        // white is a bishop down, and every move but a pawn push or a
        // capture takes the clock to a hundred
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/8/8/8 w - - 99 112").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(3));
        assert_eq!(result.score, 0);
    }

    /// A fifty move draw is claimable and not automatic (FIDE 9.3), so a
    /// root whose counter has expired still answers with a move rather
    /// than `bestmove 0000`. The score is zero because every move here
    /// leaves the counter running.
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

    /// The same position one ply before expiry, so the pair says the
    /// counter is what changed and not the position.
    #[test]
    fn the_same_root_one_ply_before_expiry_answers_the_same_way() {
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/8/8/8 w - - 99 112").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(3));
        assert_eq!(result.score, 0);
    }

    /// A root mated on the hundredth half move is still game over.
    #[test]
    fn a_mate_on_the_hundredth_half_move_is_still_game_over() {
        let game = Board::from_fen("7k/6Q1/6K1/8/8/8/8/8 b - - 100 112").unwrap();
        let mut e = engine(game);
        assert!(matches!(e.search(3), SearchOutcome::GameOver));
    }

    /// A move that resets the counter is worth what it wins: with the
    /// counter expired, every quiet move here scores zero and the queen
    /// capture resets it.
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
        // search() is public, and the check extension on u8::MAX used to
        // overflow
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
        // a node on the rail answers from the static eval whatever depth
        // it still holds
        let mut e = engine(Board::from_fen(IN_CHECK).unwrap());
        assert!(e.board.in_check());
        e.board.line_ply = MAX_PLY as usize;

        let Ok(railed) = e.alpha_beta(Score::MIN + 1, Score::MAX - 1, 4, true, RootBounds::BOTH)
        else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, 1, "the node on the rail searched on");
        assert_eq!(railed.score, e.eval());
        assert!(!railed.tainted);
    }

    #[test]
    fn the_ply_under_the_rail_is_searched() {
        // the ply under the rail searches its one evasion, which rails: a
        // rail a ply early would count one, and no rail would search on
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
        // Rh8 mates on the hundredth half move, and the mate outranks the
        // fifty move rule
        let game = Board::from_fen("k7/8/1K6/8/8/8/8/7R w - - 99 100").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(2));
        assert_eq!(result.checkmate_in(), Some(1));
        assert_eq!(format!("{}", result.best_move), "h1h8");
    }

    #[test]
    fn a_check_that_does_not_mate_on_the_hundredth_half_move_is_still_a_draw() {
        // the same check, but the king slips out to a7
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
        // the first poll happens before the root is counted
        let mut e = engine(Board::new());
        assert!(matches!(
            e.search_within(5, already_spent()),
            SearchOutcome::Aborted(None)
        ));
        assert_eq!(e.nodes, 0, "it searched past a clock that had run out");
    }

    #[test]
    fn deepening_with_no_time_budget_still_answers_depth_one() {
        // depth one runs whatever the clock says, so a spent clock still
        // gets a legal move back
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
        // a spread of budgets, so the last node falls in the full search,
        // in quiescence and now and then exactly on an iteration's end
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
            // either the last report is the last finished search and the
            // aborted search's nodes are still on the engine, or the report
            // is a swapped move's and already covers the whole deepening
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
        // wherever the root finished a move before the budget ran out, that
        // move answers, and its count is the budget to the node
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
                // no move to swap in. A ceiling finishes and does not
                // answer, so the answer's own count can be shallower than
                // the last report; the test above covers this arm
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
        // a budget of exactly what three depths cost aborts the fourth on
        // its first poll
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

    #[test]
    fn an_answer_the_deepening_rewrote_times_the_nodes_it_reports() {
        // the rows the test above skips: an answer from before the aborted
        // iteration has its count raised to cover it, and its time has to
        // be raised with it
        let mut rewritten = 0;
        for limit in (50..6_000).step_by(97) {
            let mut e = engine(Board::new());
            let options = SearchParameters::new(None, nodes_only(limit));
            let mut reported = None;
            let outcome = e.iterative_deepening_search(options, |_, result, _, _| {
                reported = Some((result.nodes, result.elapsed));
            });
            let SearchOutcome::Aborted(Some(result)) = outcome else {
                panic!(
                    "expected a move under a budget of {}, got {:?}",
                    limit, outcome
                )
            };
            let (nodes, elapsed) = reported.expect("a search reported no depth");
            if nodes + e.nodes != limit {
                continue;
            }
            assert!(
                result.elapsed > elapsed,
                "budget {}: {} nodes against the {} last reported, over the same {:?}",
                limit,
                result.nodes,
                nodes,
                elapsed
            );
            rewritten += 1;
        }
        assert!(
            rewritten > 0,
            "no budget in the sweep answered from before the aborted iteration"
        );
    }

    /// What a fresh engine answers depth four from the opening with, and
    /// what a depth five search under `budget` answers after it. Built anew
    /// for every budget so the answer depends on the budget alone.
    fn five_after_four(budget: u64) -> (Play, SearchOutcome) {
        let mut e = engine(Board::new());
        let four = completed(e.search(4));
        let five = e.search_within(5, nodes_only(budget));
        (four.best_move, five)
    }

    #[test]
    fn the_root_searches_the_previous_depths_best_move_first() {
        // what makes the swap sound. Whatever answers at the smallest budget
        // with a move in hand is the move the root tried first, and it has
        // to be the one the depth before answered with
        let finished = |budget| !matches!(five_after_four(budget).1, SearchOutcome::Aborted(None));
        // more nodes is never fewer root moves finished, so bisect
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
        // the swap is the one answer no completed depth reported. A sweep of
        // budgets, because which of them ends an iteration on a better move
        // is a fact about this position
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
                // the deepest completed depth answered. A later ceiling names
                // no answer; a later floor must name the move in hand
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
    /// of the two aborted answers it came back with. A caller cannot recover
    /// the count itself, since the two arrive in the same shape.
    #[test]
    fn a_search_stopped_by_its_budget_counts_the_iteration_it_gave_up() {
        for limit in [1_000u64, 2_500, 5_000, 7_500, 10_000, 25_000, 50_000] {
            let mut e = engine(Board::new());
            let outcome = e.iterative_deepening_search(
                SearchParameters::new(None, Limits::starting_now(None, Some(limit))),
                |_, _, _, _| {},
            );
            let SearchOutcome::Aborted(Some(result)) = outcome else {
                panic!("budget {limit}: an unlimited depth under a budget aborts with an answer");
            };
            assert_eq!(
                result.nodes, limit,
                "budget {limit}: the search says it spent other than its budget"
            );
        }
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
        // the clock wins, after depth one
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

        // the budget wins, on its node
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
        // the flag is armed the way the clock is, so depth one runs
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
        // an unlimited search of a sharp position, stopped from the report
        // of depth three
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
        // nothing but the deepening loop ever arms a flag
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
        // the count covers the whole deepening so far
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
        // fool's mate and a stalemate
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

    /// The move loop reads no legal move found as mate or stalemate, so the
    /// shallow rules exempt the first move searched: a node whose every
    /// quiet the rule would prune still answers with what a move is worth
    /// rather than the stalemate's zero. Asked of the default, since the
    /// reference has the rule off.
    #[test]
    fn a_node_that_prunes_every_late_quiet_is_not_stalemated() {
        // no capture and no move that gives check, so the rule can reach
        // every move the node has
        let mut e = engine(Board::from_fen("7k/8/8/8/8/8/8/KN1B4 w - - 0 1").unwrap());
        assert!(e.config.quiet_futility, "the default carries the rule");
        let eval = e.eval();
        assert!(eval > 100, "the side to move is a piece up twice: {eval}");
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
        // the one clock in the suite that is not already spent, so the only
        // test of the clock being read again thousands of nodes on
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
        // itself has not. The fraction belongs to Limits; this asserts the
        // deepening loop asks it, and only of a share of a game clock
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
        // positions. No tainted stores means propagation broke; a tainted
        // cutoff means a probe path without the refusal guard was added
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
        assert!(
            e.ghi().refused_cutoffs > 0,
            "the refusal refused nothing, or refused without counting"
        );
    }

    #[test]
    fn every_taint_word_names_a_policy_and_the_policy_names_it_back() {
        // a report has to be rerunnable from the word its header prints
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
        // tree must taint what flows out of it. The queen forks rook and
        // pawn; the rook is taken first, into a seeded tainted entry, and
        // the pawn capture searched after it must not launder the flag
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
            // the root's entry names the king move, so the seeded line is
            // searched first, at the open window, before standing pat could
            // end the frame
            let king = play_named(&e.board, "a1b1");
            e.transpositions
                .record_best(&e.board, king, Value::clean(0), 9);
            // the seeding itself counts one tainted store
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
        // the switch has to reach the probe
        let fen = fens::PAWN_ENDGAME;
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
        // the switch has to reach the search
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
        // hxg7+ Kxg7 Rxh7+ Kxh7 Qf6 mates. Every node of the line after the
        // first is in check with black the material up, so a shortcut that
        // fired in check would answer from that material and lose the mate
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
        // once a mate is in hand, later siblings are searched against a
        // mate beta that every eval clears. A canary rather than a
        // discrimination: on every position tried, dropping the guard
        // changed the tree and not the answers, so this holds that the mate
        // distances stay right, not that the guard alone keeps them so
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
        // the trebuchet, mutual zugzwang, where a static floor is exactly
        // what is not true. Neither side ever has a piece, so the arm
        // searches what the reference searches, node for node
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
        // `depth - 1 - r` is unsigned at the call site, so every depth a
        // pass is offered at is held to leaving it a number
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
        // off the switch every depth reads the base, on it the base plus a
        // sixth of the depth
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
        // at depth 18 the clamp cannot reach and the depth term gives 5
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
        // the clamp leaves the reduced search at depth zero, quiescence
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
        // first moves at a node of depth 6
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
        // `a_mate_found_through_a_pass_does_not_come_back_as_one` at a
        // depth the term moves
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
        // the trebuchet again: in zugzwang a pass is better than every move
        // there is, so a reduced search of one proves nothing
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
        // the sacrifice line above, with the pass in place of the margin
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
        // after a pass black has to take the knight and Qh2 mates, so the
        // reduced search comes back with a mate white never had. Beta is
        // the eval, the largest the pass is tried under, so only the mate
        // clears it. Asked of the node directly, because by the time the
        // root has searched its moves the real mate outscores the invented
        // one
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

    /// One ply short of the fifty move horizon. Every move white has resets
    /// the counter and the pass does not, so whatever taint comes out of a
    /// node here came out of the pass.
    const ONLY_A_PASS_READS_THE_DRAW: &str = "1k6/8/8/8/8/5p1p/4P1PP/6NK w - - 99 60";

    #[test]
    fn a_cutoff_from_a_pass_carries_the_pass_taint() {
        // the pass reads the draw, which clears a beta of zero
        let board = Board::from_fen(ONLY_A_PASS_READS_THE_DRAW).unwrap();
        let mut e = passing(board);
        let Ok(value) = e.alpha_beta(-1, 0, 3, true, RootBounds::NEITHER) else {
            panic!("nothing was armed to abort this search");
        };
        assert_eq!(value, Value::tainted(0));
    }

    #[test]
    fn a_pass_that_failed_still_taints_the_node() {
        // beta at the eval, so the pass fails and the moves are searched,
        // and the node still carries the draw the pass read
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

    /// The scout on its own, as `windowed` asks it: what it costs and what
    /// it answers, from the parent's side.
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
        // `windowed` driven at the child with the reduction asked for
        // directly. Alpha stands well above the move, so the scout fails
        // low and the call costs exactly the scout's nodes
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
        // a window the move sits inside, so the scout fails high. The
        // reduced call costs the scout and then an unreduced call on the
        // table the scout left (the second engine), and answers the exact
        // score
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
        // seven quiet evasions. Asked at depth two the node extends to the
        // floor and its children can reduce nothing, so the arm searches
        // what the reference searches if and only if the node in check
        // declines to reduce (`late_move::tests::a_node_in_check_reduces_nothing`)
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
        // a zero window inside the mate scores puts every node at a mate
        // edge, so none reduces, where an ordinary window reduces plenty.
        // Depth five puts the grandchildren at the floor with alpha the
        // mate in hand (`late_move::tests::the_mate_window_stands_the_reduction_down`)
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
        // the seam driven with two plies at the deep floor: the scout fails
        // low, the call costs exactly its nodes, and the one ply scout
        // beside it is dearer
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
        // measured against the rung below it
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
        // a different tree rather than a smaller one: skipping a move
        // changes what the table and the ordering hold, so the pruning can
        // cost nodes, as it does here
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
        // a skipped move can lower a node's answer and never raise it, so
        // where the first moves carry the answer the skip changes only the
        // cost
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
        // nothing here prunes, so the root's score stands and only the tree
        // may move
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
        // a search no longer arrives past the rail, so the ply is set by hand
        let mut e = remembering(Board::new());
        assert_eq!(e.memory_ply(), Some(0));
        e.board.line_ply = MAX_PLY as usize - 1;
        assert_eq!(e.memory_ply(), Some(MAX_PLY as usize - 1));
        e.board.line_ply = MAX_PLY as usize;
        assert_eq!(e.memory_ply(), None);

        let mut off = reference(Board::new());
        off.board.line_ply = 3;
        assert_eq!(off.memory_ply(), None);
    }

    #[test]
    fn a_cutoff_is_credited_to_the_side_that_played_it() {
        // at depth two every full width cutoff is a quiet black reply to a
        // white root move, so the history must be black's alone and the
        // killers at ply one exactly: the update sites read the board after
        // the move is unmade. Black is asked for a credited entry rather than
        // a positive total, since the malus on the moves tried before a
        // cutoff can outweigh the credit
        let mut e = remembering(Board::new());
        completed(e.search(2));
        assert_eq!(e.ordering.history_total(Color::White), 0);
        assert!(e.ordering.history_credited(Color::Black) > 0);
        assert_eq!(e.ordering.killers_at(0), [None; 2]);
        assert!(e.ordering.killers_at(1).iter().any(|k| k.is_some()));
        assert_eq!(e.ordering.killers_at(2), [None; 2]);
    }

    #[test]
    fn the_moves_a_node_tried_before_its_cutoff_reach_the_history() {
        // only a marked down move puts an entry under zero, so a negative
        // entry says the move loop hands the table the moves it tried and
        // not only the one that cut
        let mut e = engine(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(e.search(3));
        let marked = e.ordering.history_marked_down(Color::White)
            + e.ordering.history_marked_down(Color::Black);
        assert!(marked > 0, "nothing was marked down");

        let mut cold = reference(Board::from_fen(SHARP_MIDDLEGAME).unwrap());
        completed(cold.search(3));
        assert_eq!(cold.ordering.history_marked_down(Color::White), 0);
        assert_eq!(cold.ordering.history_marked_down(Color::Black), 0);
    }

    #[test]
    fn every_search_starts_with_the_quiet_memories_empty() {
        // a killer from the position before would order this one. Both
        // entry points empty them
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

    /// Whether `played` is worth `score` to the side to move in `fen`, asked
    /// a ply below the root of an engine with nothing in its table.
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
        // the key ignores the fifty move counter, so a search a few plies
        // from the draw fills the table with scores true of that path only.
        // Six of white's moves are worth the answer and the near-draw table
        // reorders the search, so the move is asserted by what it is worth
        // rather than by name
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
        // the skip policy keeps tainted scores out of the table, so it owes
        // the same answer warm as cold without the refusal firing. On the
        // reference, since the answer is only owed there
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
        // the root's answer slot is stored whatever its taint
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
            assert_eq!(e.make_move_str(m), Ok(()), "failed to play {}", m);
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
        // a shuffle leaves the table holding a line that goes round for ever
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

        assert_eq!(format!("{}", e.pv_line()), "c3d4");
    }

    #[test]
    fn the_pv_line_does_not_follow_a_move_which_is_illegal_here() {
        // a colliding entry's move need not belong to this position
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
        // a depth zero entry's move is fit for ordering and not for saying
        // what the engine means to play
        let mut e = engine(Board::new());
        let play = play_named(&e.board, "e2e4");
        e.transpositions
            .record_best(&e.board, play, Value::clean(0), 0);

        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_does_not_follow_a_move_which_leaves_the_king_in_check() {
        // a pinned piece's move is in the pseudo legal list and still cannot
        // be played
        let board = Board::from_fen("4r2k/8/8/8/8/8/4N3/4K3 w - - 0 1").unwrap();
        let mut e = engine(board);
        let pinned = play_named(&e.board, "e2d4");
        e.transpositions
            .record_best(&e.board, pinned, Value::clean(0), SEEDED_DEPTH);

        assert_eq!(format!("{}", e.pv_line()), "");
    }

    #[test]
    fn the_pv_line_is_bounded_by_the_ply_rail() {
        // a line longer than the rail that never repeats and never runs the
        // fifty move counter out, so nothing but the bound can end it. Pawn
        // moves first, since they reset the counter
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
        // the old cap was twenty plies from the root. Which search depth
        // clears it with room moves with every pruning change
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

/// The residual sampler seen from the search: off unless asked for, and
/// recording the nodes the shortcuts answered and the candidates the margin
/// was measured against.
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

    /// The shortcut frame driven on its own with the bounds named, and what
    /// the sampler recorded of it. The only way to hold the beta column to
    /// the beta the gate read: a sample's evaluation column is measured
    /// against the beta column, so every identity between them survives a
    /// wrong bound.
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

    /// An engine nobody asked samples of holds no sampler.
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

    fn collected(e: &mut AlphaBeta) -> Sampled<Sample> {
        e.disarm::<Sample>()
            .expect("a sampler was installed")
            .drain()
    }

    /// The one sample of a kind in what was taken.
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
            Board::from_fen(&sample.fen)
                .unwrap_or_else(|e| panic!("{} does not parse: {}", sample.fen, e));
            assert!(Shortcut::KINDS.contains(&sample.kind), "{:?}", sample);
            // the root deepens by one when it is in check
            assert!(sample.depth >= 1, "{:?}", sample);
            assert!(sample.depth <= DEPTH + 1, "{:?}", sample);
        }
    }

    /// The decision columns, taken from the node the shortcut answered. A
    /// live claim clears its beta; a shadow claim need not, but its
    /// evaluation stands at or above beta.
    ///
    /// Reverse futility claims `eval - margin * depth`, so `claimed - beta`
    /// and `eval_beta - margin * depth` are the same number. That catches a
    /// column built from the wrong evaluation or depth, not the wrong bound,
    /// which `the_recorded_beta_is_the_one_the_gate_cleared` holds.
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
    /// four keys into the first quarter of the range.
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
    /// beside it is read from the node's real bounds, which are a zero
    /// window, since an open one is exempt. Both shortcuts, because they
    /// are two call sites. The evaluation is read from the engine, so the
    /// test knows the number the columns are held to before the shortcut
    /// runs.
    #[test]
    fn the_recorded_beta_is_the_one_the_gate_cleared() {
        let eval = engine(SHARP_MIDDLEGAME).eval();

        // the margin at depth one claims `eval - 100`, which clears a beta
        // two hundred under the evaluation
        let beta = eval - 200;
        let taken = shortcut_at(SearchConfig::default(), beta - 1, beta, 1);
        assert_eq!(taken.len(), 2);
        let fired = one_of(&taken, Shortcut::ReverseFutility);
        assert_eq!(fired.beta, beta);
        assert_eq!(fired.eval_beta, 200);
        assert_eq!(fired.claimed, eval - REVERSE_FUTILITY_MARGIN);
        assert_eq!(fired.window, Window::Zero);

        // the pass, with the margin off so nothing answers the node first
        let passing = SearchConfig {
            reverse_futility: false,
            ..SearchConfig::default()
        };
        let beta = eval - 600;
        let taken = shortcut_at(passing, beta - 1, beta, 3);
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].kind, Shortcut::NullMove);
        assert_eq!(taken[0].beta, beta);
        assert_eq!(taken[0].eval_beta, 600);
        assert_eq!(taken[0].window, Window::Zero);
    }

    /// The same beta answers the node at a zero window with neither bound
    /// marked, and nothing when it is still the root's own or the window is
    /// open, before the eval is read.
    #[test]
    fn an_exempt_node_answers_no_shortcut() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        // past the margin's depth, so the pass is what answers
        let beta = eval - 600;
        let taken = shortcut_at(SearchConfig::default(), beta - 1, beta, 5);
        assert!(taken.iter().any(|s| s.kind == Shortcut::NullMove));

        // the root's beta at a zero window, then the proof's bits at an
        // open one (its alpha is the root's and its beta is not), then one
        // under the open window and the other side of it
        let roots_beta = RootBounds {
            alpha: false,
            beta: true,
        };
        let proof = RootBounds {
            alpha: true,
            beta: false,
        };
        for (alpha, root_bounds) in [
            (beta - 1, roots_beta),
            (beta - 500, proof),
            (beta - 2, RootBounds::NEITHER),
        ] {
            let mut e = engine(SHARP_MIDDLEGAME);
            e.arm(Sampler::<Sample>::every(1));
            let mut taint = Taint::default();
            let Ok(answered) = e.shortcuts(
                alpha,
                beta,
                5,
                false,
                true,
                root_bounds,
                &mut taint,
                &mut None,
            ) else {
                panic!("nothing here searches under a limit, so nothing can abort");
            };
            assert!(
                answered.is_none(),
                "a shortcut answered {root_bounds:?} at alpha {alpha}"
            );
            assert_eq!(e.nodes, 0, "the refusal searched something");
            assert!(collected(&mut e).taken.is_empty());
        }
    }

    /// A candidate the margin declines is recorded all the same: only the
    /// shadow sees the candidates a smaller margin would add.
    #[test]
    fn a_candidate_under_the_margin_is_shadowed_and_not_answered() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        // a beta fifty under the evaluation is a candidate the margin
        // declines at depth one
        let beta = eval - 50;
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        let mut taint = Taint::default();
        let Ok(answered) = e.shortcuts(
            beta - 1,
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
        assert_eq!(taken[0].claimed, eval - REVERSE_FUTILITY_MARGIN);
        assert!(taken[0].claimed < taken[0].beta);
    }

    /// A node the evaluation leaves below beta is no candidate, since no
    /// non-negative margin can fire on it.
    #[test]
    fn a_node_below_beta_is_not_a_candidate() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        let beta = eval + 50;
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        let mut taint = Taint::default();
        let Ok(answered) = e.shortcuts(
            beta - 1,
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

    /// The evaluation the shortcut frame hands back to the move loop comes
    /// off the memoised door, and the late move decision's own read is
    /// `eval::eval`; a node would decide on a different number if the two
    /// ever parted.
    #[test]
    fn the_evaluation_handed_to_the_loop_is_the_direct_one() {
        let mut e =
            AlphaBeta::with_table_bytes(Board::from_fen(SHARP_MIDDLEGAME).unwrap(), TABLE_BYTES);
        assert!(e.config.quiet_futility, "the default carries the rule");
        let direct = i64::from(crate::eval::eval(&e.board));
        // beta at the evaluation, so the gates pass and the margin does not
        // answer
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
    /// claiming the same number against the same beta.
    #[test]
    fn a_fired_candidate_is_shadowed_with_the_same_claim() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        let beta = eval - 200;
        let taken = shortcut_at(SearchConfig::default(), beta - 1, beta, 1);
        assert_eq!(taken.len(), 2);
        let live = one_of(&taken, Shortcut::ReverseFutility);
        let shadow = one_of(&taken, Shortcut::ShadowFutility);
        assert_eq!(shadow.claimed, live.claimed);
        assert_eq!(shadow.beta, live.beta);
        assert_eq!(shadow.eval_beta, live.eval_beta);
        assert_eq!(shadow.window, live.window);
        assert_eq!(shadow.fen, live.fen);
    }

    /// The margin's depth gate bounds the shadow too. The shallower samples
    /// come from the pass's reduced search.
    #[test]
    fn the_shadow_keeps_to_the_margins_depths() {
        let eval = engine(SHARP_MIDDLEGAME).eval();
        let beta = eval - 600;
        let taken = shortcut_at(SearchConfig::default(), beta - 1, beta, 5);
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

    /// The window a real search hands the hook. An open window is exempt
    /// from both shortcuts, so every row a search records carries a zero
    /// one, the shadow's among them.
    #[test]
    fn the_windows_a_search_records_are_the_zero_ones() {
        let mut e = engine(SHARP_MIDDLEGAME);
        e.arm(Sampler::<Sample>::every(1));
        e.search(6);
        let taken = collected(&mut e).taken;
        assert!(!taken.is_empty());
        let open: Vec<_> = taken.iter().filter(|s| s.window == Window::Open).collect();
        assert!(open.is_empty(), "an open window was sampled: {open:?}");
    }

    /// Every kind reaches the hook, not only whichever fires first.
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

    /// A key and not a draw, so a distribution is reproducible.
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

    /// The cap holds and says how much it dropped.
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
    /// at: at least zero for the pass, at least the whole margin for
    /// reverse futility. The weaker bound would pass on a column that had
    /// lost its depth scaling.
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
                Shortcut::NullMove | Shortcut::ShadowFutility => 0,
            };
            assert!(sample.eval_beta >= floor, "{:?} under {}", sample, floor);
        }
    }
}

/// Moves for teaching the move memories by hand, shared by the census and
/// ledger tests.
#[cfg(test)]
mod taught {
    use super::AlphaBeta;
    use crate::play::Play;

    /// A from and to square pair no move in the list uses, for teaching
    /// the (butterfly indexed) history an entry the list cannot read.
    pub(super) fn unmade_journey(moves: &[Play]) -> Play {
        (0u8..64)
            .flat_map(|from| (0u8..64).map(move |to| (from, to)))
            .find(|(from, to)| from != to && !moves.iter().any(|m| m.from == *from && m.to == *to))
            .map(|(from, to)| Play::new(from, to, None, None, false, false))
            .expect("a list cannot hold every journey")
    }

    /// Two quiet moves of the position, for teaching the memories.
    pub(super) fn quiets(e: &AlphaBeta) -> (Play, Play) {
        let moves = e.board.generate_moves();
        let mut quiets = moves
            .iter()
            .filter(|m| m.capture.is_none() && m.promote.is_none());
        let first = *quiets.next().expect("a quiet move");
        let second = *quiets.next().expect("another quiet move");
        (first, second)
    }
}

/// The cutoff census seen from the search: off unless asked for, and a row
/// reads the node as it stood when it answered. The recorder is driven
/// directly, with the memories taught by hand, so a row's history column
/// can be held to a history the test chose.
#[cfg(test)]
mod cutoffs {
    use super::taught::{quiets, unmade_journey};
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

    /// An engine nobody asked a census of holds none.
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
        // the first is taught at another ply with the larger history, so
        // only the killer slot makes the second the class
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
        // the bonus lands on a journey no move here makes, and every quiet
        // in the list is marked down
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

/// The reduction ledger seen from the search: off unless asked for, and a
/// row reads the decision as the node made it. The staging at the parent
/// and `windowed` at the child are driven directly, with the memories
/// taught by hand.
#[cfg(test)]
mod reductions {
    use super::taught::{quiets, unmade_journey};
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

    /// The staging the move loop would hand the scout. The features read
    /// neither the depth nor the bounds, so both stand at nothing.
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
            check: &mut None,
        };
        e.staged_reduction(m, searched, &mut node)
    }

    /// An engine nobody asked a ledger of holds none.
    #[test]
    fn an_engine_records_no_ledger_until_it_is_asked_to() {
        let mut e =
            AlphaBeta::with_table_bytes(Board::from_fen(SHARP_MIDDLEGAME).unwrap(), TABLE_BYTES);
        assert!(e.ledger.is_none());
        e.search(4);
        assert!(e.disarm::<reduction::Event>().is_none());
    }

    /// A staged scout that fails low: the row carries the features the node
    /// knew, the fen of the position the move left, and the node's own eval
    /// against its bounds. The board comes back exactly as the recorder
    /// found it after stepping back for that eval.
    #[test]
    fn a_row_reads_the_decision_as_the_node_made_it() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (killer, cool) = quiets(&e);
        let color = e.board.active_color;
        // the first is taught at another ply with the larger history, so
        // only the killer slot makes the second the class
        e.ordering.cutoff(color, &cool, &[], 1, 5);
        e.ordering.cutoff(color, &killer, &[], 0, 4);
        let moves = e.board.generate_moves();
        let parent_eval = i32::from(crate::eval::eval(&e.board));
        let staged = staged(&e, &killer, 5, Some(0));
        assert!(e.board.make_move(&killer));
        let child_fen = e.board.to_fen();
        let child_key = e.board.key;
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

    /// A reduced move the table has marked down stages the signed entry
    /// and a denominator clamped at zero.
    #[test]
    fn a_marked_down_move_stages_a_signed_history_and_no_denominator() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (m, _) = quiets(&e);
        let color = e.board.active_color;
        let moves = e.board.generate_moves();
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

    /// The reduction column reads what `windowed` was handed.
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
        // the depth the move was denied is the node's less one, however
        // short the scout ran
        assert_eq!(row.replay_depth(), 3);
    }

    /// A scout that fails high is recorded so, and its cost counts the
    /// scout alone rather than the full depth search it asked for.
    #[test]
    fn a_scout_that_fails_high_is_recorded_as_high() {
        let mut e = engine(SHARP_MIDDLEGAME);
        let (m, _) = quiets(&e);
        let staged = staged(&e, &m, 4, None);
        assert!(e.board.make_move(&m));
        // the tree under the scout records skip rows of its own, so the
        // staged row is picked out by the position it left
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

    /// The exemption threaded through the recursion. The bounds sit inside
    /// the mate scores, so only the bits can keep a scout off the root's
    /// beta, and a bit dropped or a flip forgotten shows up as an open node
    /// reducing against a beta of twenty thousand. A bit left set where it
    /// should clear only adds refusals and is invisible here;
    /// `what_a_child_carries_and_what_a_raise_leaves` pins that.
    ///
    /// Twenty thousand is above anything the evaluation produces, so an
    /// open window carrying it can only have the root's beta. A zero window
    /// can carry it under a node whose first child was mated, which is why
    /// the count is of open rows. The second half shows the same search
    /// with neither bound marked does reduce against that beta.
    #[test]
    fn no_open_node_reduces_against_a_beta_that_is_still_the_roots() {
        const ALPHA: Score = -20_000;
        const BETA: Score = 20_000;

        // every row, since a count of none is a claim about all of them
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
