// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use crate::board::Board;
use crate::limits::Limits;
use crate::misc::{Color, Score};
use crate::ordering::MoveOrdering;
use crate::play::Play;
use crate::residual::{Sample, Sampler, Shortcut, Window};
use crate::transposition::{
    DEFAULT_TABLE_BYTES, GhiCounters, Probe, SignatureCounters, TranspositionTable,
};
use crate::value::{Taint, Value, below_the_mate_window, is_mate};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time;

/// The rail a requested depth is held to, quiescence stops at, and the
/// reported line is walked to. It is also how long the killer table is, so
/// a node past it orders its quiet moves without one.
///
/// Not a bound on every line: a chain of check extensions can carry the
/// full width search past any constant, and what really ends one is the
/// repetition and fifty move rules. It is bounded well inside the two
/// things that would break if a line outran them: the history ring, which
/// holds a thousand and twenty four plies less the fifty move window, and
/// the mate score window, which begins a thousand under the mate score.
/// Sixty four would have fitted as comfortably; a hundred and twenty eight
/// is bought because moving it later is a play change with a match behind
/// it, and this is enough that it need never move again.
pub const MAX_PLY: u8 = 128;
// the root deepens by one more when it is in check, so a depth held to the
// rail has to leave room for that inside a byte. A rail raised past this
// fails the build rather than overflowing partway through a game.
const _: () = assert!(MAX_PLY < u8::MAX);
// How far above beta a static eval has to stand, per ply still to search,
// before a node is answered from it instead of searched: what the opponent
// is allowed to win back over those plies. A pawn a ply. The bench barely
// argues either way, sixty through a hundred and twenty spanning a tenth
// of the count between them, so what chose this figure is the depth four
// mate in two the_mate_distance_survives_a_deeper_warm_search pins:
// eighty five loses it and ninety keeps it. A margin standing one notch
// from a mate it can miss is not a margin, so this is the round number
// above that, and it costs about two percent of the tree over ninety.
const REVERSE_FUTILITY_MARGIN: Score = 100;
// The depth the shortcut stops at. The margin grows by a fixed step a ply
// and a straight line stops describing a tree quickly, which the bench
// agrees with: four, six and eight are the same count to a tenth of a
// percent, so the plies past this one are pruning nothing the shallower
// ones had not already pruned, at a guess that gets worse the deeper it
// is made.
const REVERSE_FUTILITY_MAX_DEPTH: u8 = 4;
// How many plies shallower than the node that passes the pass itself is
// searched. The whole point of a pass is that it is answered cheaply, so
// this is what the shortcut costs; too large and the reduced search proves
// nothing, too small and it costs what searching the moves would have. Two
// is where this starts, and what moves it is a match rather than the bench.
const NULL_MOVE_REDUCTION: u8 = 2;
// The shallowest depth a node may pass at, one more than the reduction so
// that the reduced search is never asked for a depth below zero. At the
// floor that search is quiescence, which still answers the question asked,
// only from the captures alone.
const NULL_MOVE_MIN_DEPTH: u8 = NULL_MOVE_REDUCTION + 1;
// What a capture may still fall short of alpha by and be searched in
// quiescence, with the captured piece counted as fully won: the positional
// ground a capture can make up besides the piece it takes. One that falls
// further is skipped rather than searched. Two hundred is the conventional
// figure, and these piece values are the conventional ones, so nothing
// argues for another.
const DELTA_MARGIN: Score = 200;
// How many plies shallower a late quiet move is scouted before it is
// searched at full depth. One, and flat: the smallest reduction there is,
// so that what the arm prices is the mechanism and not a formula. A table
// by depth and move count is a follow-up with a match of its own.
const LATE_MOVE_REDUCTION: u8 = 1;
// The shallowest depth a node may reduce at, two more than the reduction
// so that the scout keeps a full width ply under it. At the depths below
// this the scout is quiescence, or a ply above it, and what it saves is
// noise; the null move floor stands on the same reasoning.
const LATE_MOVE_MIN_DEPTH: u8 = LATE_MOVE_REDUCTION + 2;
// How many moves a node searches at full depth before a quiet move after
// them is scouted shallower. Four is an opening value rather than a tuned
// one, and moving it is a match rather than a bench.
const LATE_MOVE_THRESHOLD: usize = 4;

/// What the protocol interface asks of an engine: positions in, answers out.
/// How an implementation searches is its own business, which is why the
/// deepening loop is required here rather than provided.
pub trait Engine {
    fn parse_fen(&mut self, fen_string: &str) -> Result<(), String>;

    /// Forget what was learned from the game just finished. Stored scores do not
    /// account for repetition or the fifty move counter, so a position that
    /// comes up again in a new game would otherwise be scored from a line that
    /// no longer applies to it.
    fn new_game(&mut self);

    fn make_move_str(&mut self, play: &str) -> bool;

    /// Give the engine a transposition table of `bytes` bytes, discarding
    /// whatever the old one held. Reallocating rather than resizing in place
    /// is what the protocol expects of a size change, since a bucket is
    /// chosen from the number of buckets there are and every entry moves
    /// when that number does.
    ///
    /// False if the buckets could not be reserved, in which case the engine
    /// keeps the table it had: the size arrives from an interface, which is
    /// free to ask for more than the machine running us has, and a game is
    /// better carried on with the old table than lost with no engine.
    ///
    /// The answer is the allocator's, so it catches a size larger than the
    /// machine can address or than it will hand out. It is not a promise that
    /// the memory is there to use: where the kernel overcommits, a size that
    /// fits in ram and swap is granted here and the process killed later as
    /// the entries are written.
    #[must_use]
    fn set_table_bytes(&mut self, bytes: usize) -> bool;

    /// Empty the transposition table and leave everything else as it is.
    /// This is the protocol's `Clear Hash` button, which an interface
    /// presses so that what comes next owes nothing to what was searched
    /// before it. A size change empties the table too, by building another
    /// one; this asks for the emptying alone and keeps the buckets that are
    /// already there.
    fn clear_table(&mut self);

    /// The position, printed the way the board prints itself, for the
    /// adapter to show a person. A string rather than a write to a handle:
    /// the library never prints, and the adapter owns where its bytes go
    /// and what lock they take to get there.
    fn board_display(&self) -> String;

    fn perft(&mut self, depth: u8) -> u64;

    fn active_color(&self) -> Color;

    /// Search each depth in turn until one is the last to finish. The caller
    /// hears about every completed iteration through on_depth, which is where
    /// a protocol adapter reports progress from; the library itself never
    /// prints. A result's node count covers the whole deepening so far, not
    /// the one iteration, which is what the uci info convention expects and
    /// what makes it divisible by the time since the search began.
    ///
    /// One report is not a completed iteration: the answer an aborted
    /// iteration replaces a completed one with is reported too, as a lower
    /// bound, since it is what the search will be answering with and nothing
    /// else would have named it.
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
    /// a search nobody can interrupt, and a search with none counts exactly
    /// the nodes it counted before there was a flag to read.
    ///
    /// It rides beside the limits rather than inside them because a limit
    /// is a number the search owns and this is a word from elsewhere;
    /// `Limits` stays `Copy` for it.
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

    /// Everything one iteration may be stopped by, under the one rule:
    /// until a depth has been answered there is nothing to answer with, so
    /// nothing — not the clock, not the budget, not the flag — may stop
    /// the search. Depth one is microseconds, and this is what makes every
    /// `go` end in a real move rather than in a null one from a position
    /// with moves.
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
/// it trusts. Each one is a fact about the tree searched, so changing one
/// moves the bench.
///
/// Two configurations are named. The reference has every shortcut off and
/// every refusal on: alpha-beta with a table that only speeds it up, which
/// is why a position searched warm answers as it does cold, deepened as it
/// does direct, and with a small table as with a large one. The tests hold
/// the reference to that and it stays as it is when shortcuts arrive. A
/// change claiming to be sound keeps those tests green whatever else it
/// moves; a change that prunes moves the default and leaves the reference
/// alone; and the two played against each other say what the shortcuts are
/// worth. The default is what the engine plays with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchConfig {
    /// What the search does about draw tainted transposition scores, the
    /// ones that describe the path that stored them rather than the
    /// position. The policies are the graph history experiment's arms;
    /// what each costs and buys is measured, not written here: run
    /// `bench hash <MB> taint <word>` against another word for the pair
    /// as they stand.
    pub taint: TaintPolicy,
    /// Whether a node near the leaves may answer from its static evaluation
    /// alone when that stands far enough above beta, rather than search for
    /// the fail high it is all but certain to find. A shortcut, and a guess:
    /// on in the default, off in the reference.
    pub reverse_futility: bool,
    /// Whether a node may hand the move to the other side and answer from a
    /// reduced search of that, rather than search its own moves, when the
    /// pass alone already stands above beta. The second shortcut, and a
    /// guess like the first: on in the default, off in the reference.
    pub null_move: bool,
    /// Whether quiescence may skip a capture that leaves the standing eval
    /// a margin short of alpha with its piece counted as fully won, rather
    /// than search it down to the answer it expects. The third shortcut,
    /// and a guess like the other two: on in the default, off in the
    /// reference.
    pub delta_margin: bool,
    /// Whether quiescence may skip a capture the swap prices as losing,
    /// rather than search the exchange it expects to lose. The fourth
    /// shortcut, and a guess like the margin beside it: the swap sees no
    /// pins and nothing beyond its square. On in the default, off in the
    /// reference.
    pub see_pruning: bool,
    /// Whether a quiet move searched late at a full width node is scouted
    /// a ply shallower first, and searched at full depth only when the
    /// scout comes back above alpha. The fifth shortcut, and a guess like
    /// the four before it: a scout that fails low is trusted, and the move
    /// it answered for is never searched at the depth the node has. On in
    /// the default, off in the reference.
    pub late_move_reductions: bool,
    /// Whether a node orders its quiet moves by what other nodes have
    /// learned: the killers for its distance from the root, and the history
    /// table under them. On in the default, off in the reference, which
    /// keeps the pinned reference tree the one alpha-beta and the capture
    /// ordering alone produce.
    ///
    /// Ordering rather than pruning, so under the reference the answer is
    /// the same either way and only the size of the tree moves. Not so
    /// under the default: a shortcut fires against the window a node was
    /// searched under, that window comes from what its parent had already
    /// found, and the order the parent searched in is what decides that.
    /// So the default's move and score may move where the reference's may
    /// not.
    pub move_memory: bool,
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
            delta_margin: false,
            see_pruning: false,
            late_move_reductions: false,
            move_memory: false,
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
    // the default with the taint switch set and the rest of them left as
    // the default has them: the word names a policy, not a configuration
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
    /// What the engine plays with: the table trusted behind the fifty
    /// move guard, the first place the default and the reference part
    /// company. The four policies played each other and refusing lost
    /// about forty five elo to either trusting arm, paid in shallower
    /// endgame search; the guard cost nothing a match could see and
    /// covers the one regime where a wrong cutoff provably loses.
    ///
    /// Reverse futility is the second place they part company, and the
    /// first that prunes: it halves the bench, and unlike the taint policy
    /// it is a guess about a tree rather than a rule about a score, which
    /// is why the reference keeps it off.
    ///
    /// The pass is the third, and the same kind of guess. It buys its
    /// confidence with a reduced search where reverse futility buys it with
    /// a margin, so it prunes where a margin cannot reach.
    ///
    /// The delta margin is the fourth, the same guess made in quiescence:
    /// a capture that cannot reach alpha with its piece counted as fully
    /// won is skipped rather than searched.
    ///
    /// The losing capture skip is the fifth, the same guess again: a
    /// capture the swap prices as losing is skipped rather than searched
    /// down to the exchange the swap already priced.
    ///
    /// The late move reductions are the sixth, the pass's guess made of a
    /// move rather than a node: a quiet move tried late is scouted a ply
    /// shallower, and a scout that fails low is trusted to have priced the
    /// move.
    ///
    /// The quiet memories are the seventh, and no kind of guess at all.
    /// They prune nothing. What they move is how soon a node finds the move
    /// that cuts it off, which is most of what alpha-beta costs. The
    /// reference keeps them off so that its tree stays the one the capture
    /// ordering and generation order produce.
    fn default() -> Self {
        Self {
            taint: TaintPolicy::Rule50,
            reverse_futility: true,
            null_move: true,
            delta_margin: true,
            see_pruning: true,
            late_move_reductions: true,
            move_memory: true,
        }
    }
}

pub struct AlphaBeta {
    pub board: Board,
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
    /// nothing may. `SearchParameters::for_iteration` arms it exactly as
    /// it arms the clock, and `search_root`'s prologue writes what arrived
    /// in its signature, so a search asked directly for a depth is never
    /// interruptible and never reads it.
    stop: Option<Arc<AtomicBool>>,
    /// The nodes quiescence visited, a part of nodes. Counted for the bench,
    /// which reports what share of the tree the captures are. Never reset,
    /// like the ghi counters: it runs over the engine's whole life, and the
    /// bench reads it from an engine made for the one search
    quiescence_nodes: u64,
    /// The move ordering and its scratch buffer, search state like the
    /// limits above: one per engine, reused by every node.
    ordering: MoveOrdering,
    /// The residual sampler, or none, which is what every constructor here
    /// builds and what the engine plays and benches with. An engine with
    /// none takes no branch a search without a sampler did not take, which
    /// is the claim the pinned node counts stand behind.
    sampler: Option<Sampler>,
}

impl AlphaBeta {
    pub fn with_table_bytes(board: Board, bytes: usize) -> Self {
        Self::with_config(board, bytes, SearchConfig::default())
    }

    /// An engine searching under the policies given, with a table of the
    /// size given.
    pub fn with_config(board: Board, bytes: usize, config: SearchConfig) -> Self {
        Self {
            board,
            config,
            nodes: 0,
            transpositions: TranspositionTable::of_bytes(bytes),
            selective_depth: 0,
            limits: Limits::unlimited(),
            next_check: 0,
            stop: None,
            quiescence_nodes: 0,
            ordering: MoveOrdering::new(),
            sampler: None,
        }
    }

    /// Have the search record the nodes its shortcuts answer, at the rate
    /// the sampler was built with. Off until this is called, and the only
    /// caller is the residuals command: nothing the engine plays or benches
    /// with turns it on.
    pub fn sample_shortcuts(&mut self, sampler: Sampler) {
        self.sampler = Some(sampler);
    }

    /// The sampler back with everything it collected, leaving the engine
    /// recording nothing. None from an engine that was never given one.
    ///
    /// Handed back rather than emptied in place, so that one sampler can be
    /// carried across a run of searches and its countdown and its cap then
    /// describe the whole run rather than restarting at each search in it.
    pub fn take_sampler(&mut self) -> Option<Sampler> {
        self.sampler.take()
    }

    /// One node a shortcut has just answered, or a shadow candidate it was
    /// measured against, offered to the sampler.
    ///
    /// The fen is built inside the closure, so an event the key turns away
    /// costs a hash and nothing else, and an engine with no sampler costs
    /// the option check alone. The live hooks sit on a return out of a node
    /// rather than in the loop, so neither is on a path the search takes
    /// more than once per cutoff; the shadow hook sits beside the margin
    /// test it watches, once per node the test reads.
    ///
    /// The evaluation is passed in rather than taken again here, so a row
    /// states the number the gate really read.
    // cold and out of line, and tested with a bare is_some at each call
    // site: inlining the sample body grew alpha_beta enough to move other
    // code, and the moved jump tables aliased in the branch predictor for
    // half a million extra mispredicts on a bench 5. Counted with
    // callgrind; the option check is all the hot path keeps.
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
        // the fields are borrowed apart rather than through `self`, which is
        // what lets the closure read the board while the sampler is held
        let board = &self.board;
        let Some(sampler) = self.sampler.as_mut() else {
            return;
        };
        // after the guard, so an engine with no sampler pays for no hash.
        // Every call site checks the option before calling, so nothing
        // reaches the arm above today; it stays because the guard belongs
        // here rather than in the caller's hands alone
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

    fn eval(&self) -> Score {
        crate::eval::eval(&self.board)
    }

    /// The ply the quiet memories are indexed by at this node, or none when
    /// they are not consulted: the configuration has them off, or a chain of
    /// check extensions has carried the line past the killer table. The rail
    /// bounds quiescence and the reported line, and it does not bound a full
    /// width line, so nothing proves the second case unreachable, though no
    /// measured run has crossed it. A node past the rail orders the way a
    /// node did before the memories arrived.
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

    /// The move that cut this node off, offered to the quiet memories. The
    /// move has been unmade by the time this is called, so the board says
    /// which side played it and how far the node stands from the root.
    fn remember_cutoff(&mut self, m: &Play, depth: u8) {
        if let Some(ply) = self.memory_ply() {
            self.ordering.cutoff(self.board.active_color, m, ply, depth);
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
    /// replay's tests, which have to be able to say that the search
    /// answering the sampled positions is the reference and not the default.
    pub fn config(&self) -> SearchConfig {
        self.config
    }

    /// How much of the search's use of the transposition table depended on
    /// the path taken rather than on the position. A measurement, not a
    /// result: see the graph history interaction notes on the counters.
    pub fn ghi(&self) -> GhiCounters {
        self.transpositions.ghi()
    }

    /// Have the table keep the full key of every entry, so that what its
    /// thirty two bit signature costs can be counted rather than guessed at.
    /// The search is the same search either way, and the table starts empty:
    /// see `TranspositionTable::audit_signatures`, which this is.
    ///
    /// False if there was not the memory for the keys, in which case nothing
    /// is counted and the caller decides what to do about it.
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

    /// Cooperative limit check. The limits say when to look at them again,
    /// which is every few thousand nodes for the clock and the node budget
    /// itself, exactly. The count is incremented after this is asked, so a
    /// budget of n is n nodes visited.
    fn poll_deadline(&mut self) -> Result<(), Aborted> {
        if self.nodes < self.next_check {
            return Ok(());
        }
        self.check_limits()
    }

    /// The slow half of poll_deadline, kept out of the search's own code:
    /// reading the clock and arming the next check happen once in thousands
    /// of nodes, and inlined at every poll they only made the hot loop larger.
    #[cold]
    #[inline(never)]
    fn check_limits(&mut self) -> Result<(), Aborted> {
        if self.limits.expired(self.nodes) {
            return Err(Aborted);
        }
        // relaxed: the flag is the whole of what the two threads share, so
        // there is nothing for it to publish and nothing to order it
        // against. A few thousand nodes of latency is well under the
        // millisecond an interface waiting on a bestmove can notice
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

    fn quiescence(&mut self, mut alpha: Score, beta: Score) -> Result<Value, Aborted> {
        // quiescence looks at captures and promotions, and evasions when in
        // check, and never checks for a repetition: a capture cannot repeat a
        // position, and the quiet moves here are evasions, so a cycle needs a
        // line of nothing but mutual quiet checks, which MAX_PLY bounds and
        // real positions do not sustain. That bound is the rail and not a
        // budget: what ends a capture search is running out of captures, and
        // the horizon gets the same resolution at whatever depth it sits
        self.selective_depth = self.selective_depth.max(self.board.line_ply as u8);
        if self.board.line_ply >= MAX_PLY.into() {
            return Ok(Value::clean(self.eval()));
        }

        self.poll_deadline()?;
        self.nodes += 1;
        self.quiescence_nodes += 1;

        // Standing pat is declining to move, which only the side not in check
        // may do: the static eval is no floor for a side that has to get out
        // of check and may have no quiet way to. The full search never enters
        // here in check, the check extension searches those nodes full width,
        // so a check seen here was delivered by a capture searched here.
        // fail soft: what leaves this node is the best score actually seen,
        // not the window edge it crossed. A caller learns how far past its
        // window the position landed, and the table stores the tighter bound
        let mut best = Score::MIN + 1;
        let in_check = self.board.in_check();
        // kept past the stand pat for the margin below: a side in check has
        // none, which is what exempts its evasions from the margin
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
        // a probe at depth zero: any stored bound is deep enough here, so
        // quiescence takes the cutoffs the table can prove, under the same
        // taint refusal the full width search applies
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
        // in check the position is not quiet whatever the material says, so
        // every evasion is searched, quiet or not. Most of what full width
        // generation returns cannot answer a check and would only be refused
        // by make_move, so it is dropped before it is even sorted
        let mut moves = if in_check {
            self.board.evasions()
        } else {
            self.board.generate_captures()
        };
        // quiescence orders captures and evasions, which is where the quiet
        // memories have nothing to say, so it never reads them and never
        // writes them either. What comes back is the front, the table's
        // move and the captures the swap prices as winning or even, and
        // every capture behind it is a losing one. Read now, because the
        // sort's keys do not survive the recursion below
        let front = self.ordering.order(&self.board, &mut moves, pv_play, None);

        // quiescence itself never reads a draw, but a search told to trust
        // tainted scores can cut on a tainted entry inside a capture tree,
        // and that taint travels up through here like anywhere else
        let mut taint = Taint::default();
        let mut found_legal_move = false;
        for (i, m) in moves.iter().enumerate() {
            // two skips, guesses about the capture tree that the reference
            // does not make, under one set of exemptions. Promotions are
            // exempt, because the piece that arrives is not the pawn that
            // left and the swap prices it as the pawn; an evasion is exempt
            // because a side in check has no standing eval to measure from
            // and must answer the check; and a mate window alpha is exempt
            // because a static eval cannot come near it, so against one the
            // margin's arithmetic would skip every capture, the mating one
            // included. The stand pat lifts alpha to the static eval, so
            // alpha is inside the window only when it arrived there
            // positive: a mate is already in hand and the question is
            // whether a capture here mates faster. That is the mate the
            // exemption rescues from the losing capture skip; a sacrifice
            // that would find the first mate, with alpha nowhere near one,
            // is skipped like any other capture the swap prices as losing
            if let (Some(standing), Some(captured)) = (standing, m.capture) {
                if !is_mate(alpha) && m.promote.is_none() {
                    // the delta margin: a capture that leaves the standing
                    // eval short of alpha even with its piece counted as
                    // fully won is expected to be worth less than alpha,
                    // which the stand pat already said of the node
                    if self.config.delta_margin
                        && standing + crate::eval::material(captured) as Score + DELTA_MARGIN
                            < alpha
                    {
                        continue;
                    }
                    // the losing captures: every capture the sort put
                    // behind the front is one the swap prices as losing, so
                    // the class is read off the order rather than from a
                    // second swap. The swap already says what the exchange
                    // comes to, and searching it would spend nodes finding
                    // out
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
            // checkmate, at the end of a capture sequence: report it as the
            // search does, so the line that forces it reads as the mate it is
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
    /// a reduced search, which is dearer and reaches where a margin
    /// cannot. Both are guesses about the tree, so the reference makes
    /// neither. Nothing is stored on either: a transposition entry names
    /// the play it was reached by, and no move was searched here to name
    /// one.
    ///
    /// The reasoning both stand on is quiescence's standing pat one ply
    /// higher up: the side to move is under no obligation to make things
    /// worse, so its static eval is a floor on what it can get. The three
    /// gates they share are where that floor gives way. A side answering a
    /// check cannot decline to move, so it has no floor. A side down to
    /// pawns and a king is the material zugzwang happens to, where every
    /// move is worse than the decline the rules refuse it. And a beta
    /// belonging to a mate the other side is proving is cleared by every
    /// eval, so a cutoff against one would leave the subtree holding a
    /// faster mate unsearched; the positive half of that gate is belt and
    /// braces, since material bounds every eval far below the window, and
    /// it stands so the gate stays right if the eval ever grows terms that
    /// reach higher. The eval is read once, under those gates, and serves
    /// both shortcuts.
    ///
    /// A `Some` answers the node. A pass that failed answers nothing but
    /// read whatever it read on the way, which it leaves in the node's
    /// taint before handing back.
    ///
    /// Alpha is read by neither shortcut. It is here for the sampler, which
    /// records what kind of window the node was asked under, and it costs a
    /// register on a call that already takes five.
    fn shortcuts(
        &mut self,
        alpha: Score,
        beta: Score,
        depth: u8,
        in_check: bool,
        can_null: bool,
        taint: &mut Taint,
    ) -> Result<Option<Value>, Aborted> {
        let margin = self.config.reverse_futility && depth <= REVERSE_FUTILITY_MAX_DEPTH;
        // a pass is refused twice in a row, or the search would be
        // answering a position from a line neither side played anything
        // in. Nothing reaches that gate as the eval gate below stands: a
        // node passes only with its eval at or above beta, the pass under
        // it is searched with the window turned round, and the eval turns
        // round with it, so the second node never asks. The rule stands
        // ready for the day that gate is dropped or given a margin. The
        // depth gate leaves the reduction a search to answer with
        let pass = self.config.null_move && can_null && depth >= NULL_MOVE_MIN_DEPTH;
        if (!margin && !pass) || in_check || !self.board.has_non_pawn_material() || is_mate(beta) {
            return Ok(None);
        }
        let eval = self.eval();

        // What the margin claims is a lower bound, and `eval - margin` is
        // it: the worst the assumption allows, and what has to clear beta
        // for the cutoff to be one. The margin is what the opponent is
        // allowed to win back over the plies left to search, and that part
        // is the guess. Fail soft returns the bound that was proved rather
        // than beta; returning `eval` would return a bound nothing here
        // argues for, since the search never looked for a line that keeps
        // the whole eval. The value is clean: a static eval is a fact
        // about the position and consulted no path to reach it
        if margin {
            let floor = eval.saturating_sub(REVERSE_FUTILITY_MARGIN * depth as Score);
            // the shadow row: every candidate the margin test is about to
            // read, offered whether or not it fires. The fired rows alone
            // all stood a whole margin above beta, so the region a tighter
            // margin would newly fire on has no data without these. Nothing
            // is answered here; the test below runs exactly as it would have
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

        // The pass spends a reduced search, so it is asked for only with
        // the eval already above beta, where what is spent is spent on
        // proving something that looks true. The window is a zero one: the
        // question is only whether a pass beats beta, and that is the
        // cheapest way to ask it
        if pass && eval >= beta {
            self.board.make_null_move();
            let result = self.alpha_beta(-beta, -beta + 1, depth - 1 - NULL_MOVE_REDUCTION, false);
            // undo before an abort can propagate, or the board would keep
            // the passed line
            self.board.undo_null_move();
            let value = -result?;
            if value.score >= beta {
                // a mate found through a pass is not a mate. The pass is not
                // a move either side has, so what was proved is that the
                // position is very good and not that it is won, and the
                // score is held below the window a caller reads mates in
                let score = below_the_mate_window(value.score);
                // the board is back from the pass, so the fen the sampler
                // prints here is the node itself, and the eval handed to it
                // is the one this node's gate read
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
    /// legal here: make the move, search what it leaves, and answer from
    /// this side of the board. The undo comes before the abort can
    /// propagate, or the board would keep the aborted line; propagating is
    /// what keeps the meaningless score of an aborted frame away from
    /// every store above. The full width search enters every child through
    /// here, so the window discipline in `windowed` is written once rather
    /// than at three sites, and every caller says only whether its move is
    /// the node's first and whether it is scouted shallower first.
    fn search_child(
        &mut self,
        m: &Play,
        alpha: Score,
        beta: Score,
        depth: u8,
        first: bool,
        reduced: bool,
    ) -> Result<Option<Value>, Aborted> {
        if !self.board.make_move(m) {
            return Ok(None);
        }
        let result = self.windowed(alpha, beta, depth, first, reduced);
        self.board.undo_move();
        Ok(Some(result?))
    }

    /// Principal variation search: the recursion behind one made move. The
    /// node's first move is searched with the window as it stands. Every
    /// later move is asked a cheaper question first, a zero width search at
    /// the same depth, which can only say whether the move beats alpha; one
    /// that does, at a node whose own window is wider than the zero one, is
    /// searched again with the full window for the score the node needs. A
    /// zero width node never asks twice, since its window is the re-search
    /// window already. Every pass negates through the same call, so a mate
    /// score is adjusted per ply the same way on each.
    ///
    /// A reduced move is asked the cheapest question of all before any of
    /// that: the same zero width search, a ply shallower. A scout that
    /// fails low answers for the move, which is the late move reduction's
    /// guess; one that fails high has earned the full depth, and the move
    /// goes on to the probe and the proof as an unreduced move does. The
    /// windows are decided here and nowhere else, so the scout
    /// is a stage in front of the probe rather than a copy of it.
    ///
    /// A body of its own rather than `search_child`'s so that an abort from
    /// any pass runs through the one undo there.
    fn windowed(
        &mut self,
        alpha: Score,
        beta: Score,
        depth: u8,
        first: bool,
        reduced: bool,
    ) -> Result<Value, Aborted> {
        if first {
            debug_assert!(!reduced, "a node's first move is never reduced");
            return Ok(-self.alpha_beta(-beta, -alpha, depth - 1, true)?);
        }
        let mut tainted = false;
        if reduced {
            let scout =
                -self.alpha_beta(-alpha - 1, -alpha, depth - 1 - LATE_MOVE_REDUCTION, true)?;
            if scout.score <= alpha {
                return Ok(scout);
            }
            // the scout's fail high is what asked for the full depth, so
            // whatever it depended on, the passes below do too
            tainted = scout.tainted;
        }
        let probe = -self.alpha_beta(-alpha - 1, -alpha, depth - 1, true)?;
        // `alpha + 1 >= beta` is the window being the zero one, spelt
        // without the subtraction: `beta - alpha` overflows a Score when
        // the window is the root's
        if probe.score <= alpha || alpha + 1 >= beta {
            return Ok(Value::with_taint(probe.score, probe.tainted || tainted));
        }
        let proof = -self.alpha_beta(-beta, -alpha, depth - 1, true)?;
        // the probe's fail high is what asked for the second search, so
        // whatever the probe's answer depended on, this one does too
        Ok(Value::with_taint(
            proof.score,
            proof.tainted || probe.tainted || tainted,
        ))
    }

    /// Whether a move at a full width node is scouted a ply shallower
    /// before it is searched at the node's depth: the late move reduction.
    /// `searched` is how many moves the node has searched already, the
    /// table's move among them.
    ///
    /// The exemptions are one rule: the reduction guesses that a move the
    /// ordering put late is worth less than alpha, and it is refused
    /// wherever that guess has nothing to stand on. The first moves are
    /// searched whole, since a node whose ordering is right cuts off on
    /// them and a node whose ordering is wrong has no late moves to speak
    /// of. A capture or a promotion changes the material, which is what
    /// the ordering priced it on, so the reduction is not asked about it;
    /// that takes in the losing captures too, which the ordering sorts
    /// behind the quiets, and reducing them is a follow-up. A side in
    /// check has evasions and not late moves. And a window at either edge
    /// of the mate scores is the margin family's exemption, made here for
    /// the same reason: a scout a ply short of the mate it is asked about
    /// can only say no. The table's move is the node's first, so it is
    /// never here.
    ///
    /// The root's open bounds sit inside the mate window, and a node's
    /// first child inherits them, so no node on the leftmost line
    /// reduces. That is a consequence of the exemption and not a
    /// decision about principal variation nodes; an arm that wants to
    /// reduce there has to lift it.
    ///
    /// A quiet move that gives check is reduced like any other. The board
    /// has no cheap test for one, and what the scout can miss is bounded
    /// by the re-search: a checking move that fails low a ply short is
    /// trusted the way a quiet one is.
    fn reduces(
        &self,
        m: &Play,
        searched: usize,
        depth: u8,
        in_check: bool,
        alpha: Score,
        beta: Score,
    ) -> bool {
        self.config.late_move_reductions
            && depth >= LATE_MOVE_MIN_DEPTH
            && searched >= LATE_MOVE_THRESHOLD
            && !in_check
            && m.capture.is_none()
            && m.promote.is_none()
            && !is_mate(alpha)
            && !is_mate(beta)
    }

    /// A fail high at a full width node: the move that proved it goes to
    /// the quiet memories and, when the taint policy allows, to the table,
    /// and what comes back is the node's answer. The one place a full
    /// width cutoff is acted on, which is where the next thing a cutoff
    /// should feed lands.
    fn cutoff(&mut self, m: &Play, taint: Taint, score: Score, depth: u8) -> Value {
        self.remember_cutoff(m, depth);
        let value = taint.stamp(score);
        if self.keeps(value) {
            self.transpositions
                .record_cutoff(&self.board, *m, value, depth);
        }
        value
    }

    /// One full width node. `can_null` is false only under a pass, which is
    /// how two of them in a row are refused: a position neither side has
    /// moved in is not one a reduced search says anything about. A parameter
    /// rather than a field toggled around the call, so that reading the
    /// recursion says which nodes may pass.
    fn alpha_beta(
        &mut self,
        mut alpha: Score,
        beta: Score,
        mut depth: u8,
        can_null: bool,
    ) -> Result<Value, Aborted> {
        self.poll_deadline()?;
        self.selective_depth = self.selective_depth.max(self.board.line_ply as u8);
        self.nodes += 1;

        // every node here sits below the root, which search() owns: a
        // repetition there is not a finished game because the engine still has
        // to move, but from here on it is a draw either side can take
        let in_check = self.board.in_check();
        if self.board.fifty_move_expired() {
            // a mate delivered by the hundredth half move is a mate: the game
            // ends on it, before the side mated has a move on which to claim
            // the draw. Only asked here and not of a repetition, which cannot
            // be a mate since the position would have ended the game the
            // first time it came up, so the cost stays off the lines that
            // repeat
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
        // fail soft, as in quiescence: the node answers with the best score
        // it saw, and a cutoff stores that score, a floor at least as tight
        // as beta and usually tighter
        let mut best = Score::MIN + 1;
        let pv_play = match self.transpositions.probe(
            &self.board,
            alpha,
            beta,
            depth,
            self.config.taint.refuses_tainted_cutoffs(),
            self.config.taint.guards_rule50(),
        ) {
            // whatever the stored score depended on, whoever takes it now
            // depends on too, which is what the value carries
            Probe::Cut(value) => return Ok(value),
            Probe::Order(play) => Some(play),
            Probe::Miss => None,
        };

        // the shortcuts, after the table has had its say
        if let Some(value) = self.shortcuts(alpha, beta, depth, in_check, can_null, &mut taint)? {
            return Ok(value);
        }

        // The table's move sorts ahead of everything else below, and when there
        // is one it takes the cutoff nine times in ten. Searching it before
        // generating means the nodes it cuts never generate or sort at all.
        // The order is the one the sort would have produced either way, so the
        // tree searched is unchanged.
        let mut tt_tried: Option<Play> = None;
        if let Some(tt) = pv_play {
            if self.board.is_pseudo_legal(&tt) {
                tt_tried = Some(tt);
                if let Some(value) = self.search_child(&tt, alpha, beta, depth, true, false)? {
                    found_legal_move = true;
                    // the table's move is searched before the rest are even
                    // generated, so it taints this node the same way any other
                    // child would
                    taint.absorb(value);
                    let tt_score = value.score;
                    if tt_score > best {
                        best = tt_score;
                        best_move = Some(tt);
                    }
                    if tt_score > alpha {
                        if tt_score >= beta {
                            // a cutoff is a cutoff wherever it is proved, so
                            // the table's move earns its killer slot here as
                            // any other move does in the loop below
                            return Ok(self.cutoff(&tt, taint, tt_score, depth));
                        }
                        alpha = tt_score;
                    }
                }
            }
        }

        let mut moves = if in_check {
            self.board.evasions()
        } else {
            self.board.generate_moves()
        };
        let ply = self.memory_ply();
        let front = self.ordering.order(&self.board, &mut moves, pv_play, ply);

        // how many moves this node has searched, which is what makes a
        // quiet move late. The table's move, when it was searched, is the
        // first of them
        let mut searched = usize::from(found_legal_move);
        for i in 0..moves.len() {
            // the front did not cut this node off, so the rest of the list
            // is scored and sorted before the first move past it is tried
            if i == front {
                if let Some(ply) = ply {
                    self.ordering
                        .order_quiets(&self.board, &mut moves[front..], ply);
                }
            }
            let m = &moves[i];
            if tt_tried == Some(*m) {
                continue;
            }
            let reduced = self.reduces(m, searched, depth, in_check, alpha, beta);
            let Some(value) =
                self.search_child(m, alpha, beta, depth, !found_legal_move, reduced)?
            else {
                continue;
            };
            found_legal_move = true;
            searched += 1;
            // a value built from a tainted child is tainted, whether or not
            // it turns out to be the best one here
            taint.absorb(value);
            let score = value.score;
            if score > best {
                best = score;
                best_move = Some(*m);
            }
            if score > alpha {
                if score >= beta {
                    return Ok(self.cutoff(m, taint, score, depth));
                }
                alpha = score;
            }
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

    pub fn new(board: Board) -> Self {
        AlphaBeta::with_table_bytes(board, DEFAULT_TABLE_BYTES)
    }

    /// The root loop: the one node whose answer must include a play, which is
    /// why it runs here rather than in alpha_beta. The root probes the
    /// transposition table to order moves and stores its entry when done, but
    /// never takes a stored score in place of searching: a stored score can
    /// come from a line whose repetition and fifty move context differ from
    /// the game being played, and the answer must not depend on one.
    pub fn search(&mut self, depth: u8) -> SearchOutcome {
        self.transpositions.new_search();
        self.ordering.forget();
        self.search_within(depth, Limits::unlimited())
    }

    /// One fixed depth search under the limits given for it. The deepening
    /// loop hands each iteration its own, which is how depth one runs with
    /// none.
    ///
    /// The caller owns the table's generation. `search` and the deepening
    /// loop each start one with `new_search`, so that however many
    /// iterations follow, what they store belongs to the one search and ages
    /// together from the next. Calling this on its own skips that, and a
    /// table whose generation never moves keeps every entry looking current:
    /// nothing goes stale and the oldest entries are never the ones given up.
    /// The quiet memories are the caller's on the same terms, and a search
    /// asked for through here keeps whatever the search before it learned.
    pub fn search_within(&mut self, depth: u8, limits: Limits) -> SearchOutcome {
        // nothing outside stops a search asked for directly
        self.search_root(depth, limits, None)
    }

    /// The body of one fixed depth search. Everything that may interrupt
    /// it arrives in the signature — the limits it spends, and the flag
    /// that is the one interrupter from outside — and the prologue, not
    /// the caller, writes the field the poll reads.
    fn search_root(
        &mut self,
        mut depth: u8,
        limits: Limits,
        stop: Option<Arc<AtomicBool>>,
    ) -> SearchOutcome {
        // held to the rail here and not only at the interface, so a caller
        // of the library cannot ask the check extension below to overflow
        depth = depth.min(MAX_PLY);
        self.limits = limits;
        self.stop = stop;
        self.next_check = 0;
        self.nodes = 0;
        self.selective_depth = depth;
        self.board.line_ply = 0;

        // the game is already drawn, there is no move to look for
        if self.board.fifty_move_expired() {
            return SearchOutcome::GameOver;
        }

        if self.poll_deadline().is_err() {
            return SearchOutcome::Aborted(None);
        }
        self.nodes += 1;

        if self.board.in_check() {
            depth += 1;
        }

        let mut alpha = Score::MIN + 1;
        let beta = Score::MAX - 1;
        let mut best: Option<Play> = None;
        let mut found_legal_move = false;
        let mut taint = Taint::default();

        // the entry here is the one the last iteration answered with, stored
        // past the depth contest, and `MoveOrdering::order` puts the table's
        // move ahead of every other. So a deepening search tries the previous
        // depth's best first, which is what lets an aborted iteration's best
        // replace it: see the Aborted arm of iterative_deepening_search
        let pv_play = self.transpositions.ordering_play(&self.board);
        let mut moves = self.board.generate_moves();
        // the root orders as it always has: the swap below reasons about
        // this list, and a killer moving a quiet move up it would be one
        // more thing that reasoning has to hold for
        self.ordering.order(&self.board, &mut moves, pv_play, None);
        // the soundness of replacing a completed answer with an aborted
        // iteration's rests on that ordering, so a change that breaks it,
        // say a root bonus outbidding the table move, fails here rather
        // than silently answering with a move never compared to the old one
        if let Some(previous) = pv_play {
            debug_assert!(
                moves
                    .iter()
                    .position(|m| *m == previous)
                    .is_none_or(|at| at == 0),
                "the table's move is no longer first at the root"
            );
        }

        // the root reduces nothing, by choice: its window is the full one,
        // and its moves are few enough to search whole
        for m in &moves {
            match self.search_child(m, alpha, beta, depth, !found_legal_move, false) {
                Err(Aborted) => {
                    return SearchOutcome::Aborted(best.map(|play| self.result_for(play, alpha)));
                }
                Ok(None) => {}
                Ok(Some(value)) => {
                    found_legal_move = true;
                    taint.absorb(value);
                    let score = value.score;
                    if score > alpha {
                        alpha = score;
                        best = Some(*m);
                    }
                }
            }
        }

        if !found_legal_move {
            // checkmate or stalemate: either way there is nothing to play
            return SearchOutcome::GameOver;
        }

        let play = best.expect("any legal move's score beats the opening alpha of Score::MIN + 1");
        // stored past the depth contest: this is the move about to be
        // answered, and the line reported for it is read back from this slot
        self.transpositions
            .record_answer(&self.board, play, taint.stamp(alpha), depth);
        SearchOutcome::Complete(self.result_for(play, alpha))
    }

    /// Replay the line the table holds on a copy of the board, one stored move
    /// at a time. Walking the positions rather than following a key from one
    /// entry to the next is what lets the line be checked as it is built: the
    /// board says whether a stored move is legal here, and whether the line has
    /// reached a position it would be a draw to play on from.
    pub fn pv_line(&self) -> PvLine {
        self.pv_line_from(self.transpositions.intended_play(&self.board))
    }

    /// The same line read from a first move given rather than from the
    /// table's. An aborted iteration stores nothing at the root, so the
    /// entry there still holds the move the depth before it answered, and
    /// the move the swap answers with has to be handed in.
    fn pv_line_from(&self, first: Option<Play>) -> PvLine {
        let mut line = Vec::new();
        // Board is Copy, so the search's own board is untouched by this.
        let mut board = self.board;
        let mut next = first;
        while line.len() < MAX_PLY as usize {
            let Some(play) = next else {
                break;
            };
            // a probe compares thirty two bits of the key, so what gets
            // through is another position agreeing on those. Its move belongs
            // to that position, and playing it here would print a line the
            // rules do not allow. How often that happens is what the bench's
            // signature audit counts
            if !board.generate_moves().contains(&play) {
                break;
            }
            if !board.make_move(&play) {
                break;
            }
            line.push(play);
            // the line is a draw from here, so whatever the table says comes
            // next is a continuation that would never be played
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
        let mut best: Option<SearchResult> = None;
        // each search() counts its own nodes, so the deepening totals them:
        // what leaves here describes the whole search so far, which is the
        // count the time elapsed so far can honestly divide
        let mut total_nodes: u64 = 0;
        // no depth asked for means as deep as the engine goes, and what
        // stops such a search is its clock or its budget. The rail is here
        // so that a search under neither still ends
        let max_depth = match search_options.depth {
            // held to the rail, or the depths past it would each rerun it
            Some(depth) => depth.min(MAX_PLY),
            None => MAX_PLY,
        };
        // one search, however many iterations: what the iterations store is
        // one generation's, and ages together from the next go. The quiet
        // memories start empty for the same reason and are kept from one
        // iteration to the next for the opposite one: a killer found at
        // depth six is what orders depth seven
        self.transpositions.new_search();
        self.ordering.forget();

        for depth in 1..=max_depth {
            // the soft bound: an iteration there is not enough of the clock
            // left to get through is not begun at all, and what is in hand
            // answers instead. The deadline stays as the backstop for the
            // iteration that is begun and turns out to have been the last
            if !search_options
                .limits
                .worth_another_iteration(best.is_some())
            {
                return SearchOutcome::Aborted(best);
            }
            let (limits, stop) = search_options.for_iteration(best.is_some(), total_nodes);
            match self.search_root(depth, limits, stop) {
                SearchOutcome::Aborted(deeper) => {
                    // the interrupted iteration's best outranks the completed
                    // depth's whenever it has one. The root searches full
                    // window, so that score is exact over the moves it did
                    // search, which makes it a lower bound on the position:
                    // a deeper answer over fewer moves, and one the moves
                    // never reached could only raise. ScoreBound is what
                    // says so to whoever is reading the reports.
                    //
                    // The move it replaces is one of the moves searched: the
                    // root orders by the table's entry for the position, which
                    // is the answer the last iteration stored, so the previous
                    // best is the first move this iteration tried. That
                    // ordering is the whole of why the swap is sound. Without
                    // it the new move would be better only over a subset the
                    // old one need not belong to, and preferring it would mean
                    // preferring a move that was never compared with the one
                    // being given up.
                    return SearchOutcome::Aborted(match deeper {
                        Some(mut result) => {
                            // its count covers the whole deepening, as a
                            // completed iteration's does
                            result.nodes += total_nodes;
                            // no completed depth named this move, so the last
                            // line the caller heard about opens with the one
                            // being given up. Say this one, from the depth
                            // that found it and as the bound it is, before it
                            // is answered with
                            if best.as_ref().map(|had| had.best_move) != Some(result.best_move) {
                                let pv = self.pv_line_from(Some(result.best_move));
                                // the line is built from the move rather than
                                // read from the root's entry, so it opens with
                                // it whatever the table holds. The completed
                                // depth checks the same thing of the line it
                                // reports
                                debug_assert_eq!(
                                    pv.line.first(),
                                    Some(&result.best_move),
                                    "the reported line disagrees with the swapped move"
                                );
                                on_depth(depth, &result, pv, ScoreBound::Lower);
                            }
                            Some(result)
                        }
                        // nothing finished at this depth, so the last
                        // completed one still answers; depth one runs without
                        // limits, so there always is one
                        None => best,
                    });
                }
                SearchOutcome::GameOver => {
                    // checkmate, stalemate or a rule draw: deeper searches
                    // cannot change it, so don't run them
                    return SearchOutcome::GameOver;
                }
                SearchOutcome::Complete(mut result) => {
                    total_nodes += result.nodes;
                    result.nodes = total_nodes;
                    let pv = self.pv_line();
                    // the root's entry was just stored past any leftover, so
                    // the line the table tells opens with the move answered
                    debug_assert_eq!(
                        pv.line.first(),
                        Some(&result.best_move),
                        "the reported line disagrees with the best move"
                    );
                    on_depth(depth, &result, pv, ScoreBound::Exact);
                    best = Some(result);
                }
            }
        }
        match best {
            Some(result) => SearchOutcome::Complete(result),
            // a depth of zero runs no iterations, so there is nothing to
            // report beyond that no move was looked for
            None => SearchOutcome::Aborted(None),
        }
    }

    fn make_move_str(&mut self, play: &str) -> bool {
        // a `Play` prints itself as the lower case coordinate notation the
        // protocol sends, so the name is compared with that
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
    /// The search finished the requested depth; its result can be trusted
    /// without checking anything else.
    Complete(SearchResult),
    /// The root has no play to make: checkmate, stalemate, or a rule draw.
    /// Searching deeper cannot change it.
    GameOver,
    /// A limit ran out partway through, carrying a best-so-far when the
    /// root got far enough to have one. The root searches full window, so
    /// that score is exact over the moves it did search, which makes it a
    /// lower bound on the position: what it leaves out is the moves it
    /// never reached, and they could only raise it. That is the
    /// `ScoreBound::Lower` a report of one carries.
    Aborted(Option<SearchResult>),
}

/// What a reported score says about the position.
///
/// The deepening loop reports a completed depth and, when an aborted
/// iteration's move replaces it, that move as well. The two are not the same
/// claim: a completed depth searched every root move, and an aborted one
/// searched some of them, so its score is what those are worth and the
/// position may be worth more. Uci has a word for the second, `lowerbound`,
/// which is what the adapter prints it as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoreBound {
    Exact,
    Lower,
}

/// The search hit a limit and unwound without finishing. The score of an
/// aborted frame is meaningless, and returning this instead of a score is what
/// keeps it out of the transposition table: propagation with `?` never reaches
/// the stores.
struct Aborted;

#[derive(Debug)]
pub struct SearchResult {
    pub nodes: u64,
    /// How long the search took, measured over the same interval as the
    /// nodes beside it, so that one divides the other honestly.
    pub elapsed: time::Duration,
    /// How deep the deepest line went, quiescence's captures included, which
    /// is the `seldepth` an info line reports.
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
        LATE_MOVE_MIN_DEPTH, LATE_MOVE_REDUCTION, LATE_MOVE_THRESHOLD, Limits, MAX_PLY, Play,
        Score, ScoreBound, SearchConfig, SearchOutcome, SearchParameters, SearchResult,
        TaintPolicy, Value,
    };
    use crate::board::{fens, play_named};
    use crate::limits::Clock;
    use crate::misc::{Color, Piece};
    use crate::value::CHECKMATE_THRESHOLD;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time;

    /// The default table is 256MB, which is the right size to play with and
    /// the wrong size to test with: one per test dominated both the memory and
    /// the run time of the suite. This is still far larger than anything here
    /// searches deeply enough to fill.
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
        assert!(matches!(e.search(4), SearchOutcome::Complete(_)));
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
            assert!(matches!(e.search(3), SearchOutcome::Complete(_)));
        }
    }

    #[test]
    fn a_table_too_small_to_hold_an_entry_is_still_a_table() {
        // the size arrives from the protocol, so a nonsense one has to be
        // survivable rather than a panic in the middle of a game
        let mut e = engine(Board::new());
        assert!(e.set_table_bytes(0));
        assert!(e.table_bytes() > 0);
        assert!(matches!(e.search(3), SearchOutcome::Complete(_)));
    }

    /// The reference search, for the tests that hold it to answering the
    /// same whatever the table holds. They are the search's exactness
    /// contract and stay green through every shortcut: a shortcut moves the
    /// default, which `engine` builds, and not this.
    fn reference(board: Board) -> AlphaBeta {
        AlphaBeta::with_config(board, TABLE_BYTES, SearchConfig::reference())
    }

    /// The reference with the static shortcut switched on and nothing else
    /// touched, which is what an arm is: whatever moves between this and
    /// `reference` is reverse futility and not the taint policy the default
    /// carries beside it.
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

    /// The reference with the pass switched on and nothing else touched,
    /// the arm reverse futility has one of beside it: whatever moves between
    /// this and `reference` is the pass and nothing else.
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

    /// The reference with the quiet memories switched on and nothing else
    /// touched, the third arm beside the two above. They order rather than
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

    /// The reference with the losing capture skip switched on and nothing
    /// else touched: whatever moves between this and `reference` is the
    /// skip, and not the delta margin the default carries beside it.
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

    /// The reference with the late move reductions switched on and nothing
    /// else touched: whatever moves between this and `reference` is the
    /// reduction, with no memories to order the quiets it reduces.
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

    /// A tactical middlegame the cache tests search over and over: sharp
    /// enough that a wrongly reused score would move the verdict.
    const SHARP_MIDDLEGAME: &str = "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12";

    /// Unwrap the outcome these tests expect: a search that ran to the depth
    /// asked of it.
    fn completed(outcome: SearchOutcome) -> SearchResult {
        match outcome {
            SearchOutcome::Complete(result) => result,
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
        let Ok(exact) = oracle.windowed(Score::MIN + 2, Score::MAX, 2, true, false) else {
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
        let Ok(value) = e.windowed(alpha, beta, 2, false, false) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(value.score, exact.score);
    }

    #[test]
    fn the_mate_distance_survives_a_deeper_warm_search() {
        let game =
            Board::from_fen("2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - - 0 0").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(4));
        assert_eq!(result.checkmate_in(), Some(2));
        // searching again deeper with a warm cache reuses mate scores stored
        // at different plies, the reported mate distance must not change
        let result = completed(e.search(6));
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

        let e = skipping(Board::from_fen(fen).unwrap());
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
        let fens = [
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 10 10",
            "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
        ];
        for fen in fens {
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
    fn a_losing_side_plays_for_the_fifty_move_draw() {
        // white is a bishop down here, and every move but a pawn push or a
        // capture takes the clock to a hundred, so the draw is the best of it
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/8/8/8 w - - 99 112").unwrap();
        let mut e = engine(game);
        let result = completed(e.search(3));
        assert_eq!(result.score, 0);
    }

    #[test]
    fn a_triggered_fifty_move_rule_is_game_over() {
        // The fifty move rule has been triggered - the game is already drawn,
        // there is no move to look for
        let game = Board::from_fen("5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/8/8/8 w - - 100 112").unwrap();
        let mut e = engine(game);
        assert!(matches!(e.search(3), SearchOutcome::GameOver));
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
            // the reported total is the last completed depth's, and the
            // aborted iteration's own count is still on the engine: together
            // they are every node visited, and that has to be the budget to
            // the node
            assert_eq!(completed + e.nodes, limit, "budget {}", limit);
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
            let mut last_report = 0;
            let outcome =
                e.iterative_deepening_search(options, |_, result, _, _| last_report = result.nodes);
            let SearchOutcome::Aborted(Some(result)) = outcome else {
                panic!(
                    "expected a move under a budget of {}, got {:?}",
                    limit, outcome
                )
            };
            if result.nodes == last_report {
                // the iteration aborted before any root move finished
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
                // already described. Nothing may have been said after it: a
                // bound where there was no swap would have the caller print
                // a line for an answer it already had
                assert_eq!(
                    reports.last().map(|(_, _, bound)| *bound),
                    Some(ScoreBound::Exact),
                    "budget {}: a bound was reported where nothing was swapped",
                    limit
                );
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

    #[test]
    fn a_completed_depth_is_reported_as_an_exact_score() {
        let mut e = engine(Board::new());
        let mut bounds = Vec::new();
        let outcome = e.iterative_deepening_search(
            SearchParameters::new(Some(4), Limits::unlimited()),
            |_, _, _, bound| bounds.push(bound),
        );
        assert!(matches!(outcome, SearchOutcome::Complete(_)));
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
        let (SearchOutcome::Aborted(Some(result)) | SearchOutcome::Complete(result)) = outcome
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
        assert!(matches!(e.search(2), SearchOutcome::Complete(_)));
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
            SearchOutcome::Complete(_)
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
        let SearchOutcome::Complete(result) = outcome else {
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
        // The pawn endgame carries the most draw traffic of the bench
        // positions, so it is the one that exercises both halves of the graph
        // history work. tainted_stores going to zero means taint propagation
        // broke, and then the refusal in get_transposition is refusing nothing
        // while the hole silently reopens. tainted_score_cutoffs is zero by
        // construction while the refusal holds, so it moving means a probe
        // path that consumes scores without the refusal guard was added.
        // The refusal is a policy of the reference, so the reference is what
        // is built: a default told one day to trust those scores is an
        // experiment to measure, not a hole to find here.
        let fen = "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1";
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
            let mut board = e.board;
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
        let fen = "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1";
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
        // the switch has to reach the search: one the search never read
        // would make every comparison with the reference a comparison of
        // the reference with itself, and the bench's two pinned counts
        // would move together instead of apart
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
        // the switch has to reach the search: one the search never read
        // would make every comparison with the reference a comparison of
        // the reference with itself, and the bench's two pinned counts
        // would move together instead of apart
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
        let Ok(value) = e.alpha_beta(beta - 1, beta, 5, true) else {
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
        let Ok(value) = e.alpha_beta(-1, 0, 3, true) else {
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
        let Ok(value) = e.alpha_beta(beta - 1, beta, 3, true) else {
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
        let Ok(value) = e.alpha_beta(-alpha - 1, -alpha, depth - 1 - LATE_MOVE_REDUCTION, true)
        else {
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
        let Ok(exact) = oracle.windowed(Score::MIN + 2, Score::MAX, DEPTH, true, false) else {
            panic!("an unlimited search aborted");
        };
        let alpha = exact.score + 500;
        assert!(!super::is_mate(alpha));

        let mut alone = at_reducible_child(SearchConfig::reference());
        let (scout_nodes, scout_value) = scout(&mut alone, alpha, DEPTH);
        assert!(scout_value.score <= alpha, "the scout did not fail low");

        let mut e = at_reducible_child(SearchConfig::reference());
        let Ok(value) = e.windowed(alpha, alpha + 1, DEPTH, false, true) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, scout_nodes);
        assert_eq!(value, scout_value);

        let mut probe = at_reducible_child(SearchConfig::reference());
        let Ok(unreduced) = probe.windowed(alpha, alpha + 1, DEPTH, false, false) else {
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
        let Ok(exact) = oracle.windowed(Score::MIN + 2, Score::MAX, DEPTH, true, false) else {
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
        let Ok(unreduced) = then_probed.windowed(alpha, beta, DEPTH, false, false) else {
            panic!("an unlimited search aborted");
        };
        assert!(
            then_probed.nodes > scout_nodes,
            "nothing was searched after the scout"
        );

        let mut e = at_reducible_child(SearchConfig::reference());
        let Ok(value) = e.windowed(alpha, beta, DEPTH, false, true) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, then_probed.nodes);
        assert_eq!(value.score, unreduced.score);
        assert_eq!(value.score, exact.score);
    }

    /// A node the reduction applies to, and the two kinds of move at it:
    /// the rook can take the pawn and has quiet moves besides. Each
    /// exemption test moves one thing about the call and holds the rest.
    const A_CAPTURE_AND_QUIETS: &str = "7k/8/8/8/R3p3/8/8/7K w - - 0 1";

    fn a_quiet_and_a_capture() -> (AlphaBeta, Play, Play) {
        let e = reducing(Board::from_fen(A_CAPTURE_AND_QUIETS).unwrap());
        let quiet = play_named(&e.board, "a4a5");
        let capture = play_named(&e.board, "a4e4");
        assert!(quiet.capture.is_none() && capture.capture.is_some());
        (e, quiet, capture)
    }

    #[test]
    fn a_late_quiet_is_reduced_and_the_first_moves_are_not() {
        // the threshold is the count of moves searched before this one,
        // so the move after the fourth is the first reduced
        let (e, quiet, _) = a_quiet_and_a_capture();
        for searched in 0..LATE_MOVE_THRESHOLD {
            assert!(
                !e.reduces(&quiet, searched, LATE_MOVE_MIN_DEPTH, false, -100, 100),
                "reduced with {} moves searched",
                searched
            );
        }
        assert!(e.reduces(
            &quiet,
            LATE_MOVE_THRESHOLD,
            LATE_MOVE_MIN_DEPTH,
            false,
            -100,
            100
        ));
        assert!(e.reduces(&quiet, LATE_MOVE_THRESHOLD + 10, MAX_PLY, false, -100, 100));
        // and not under the floor, where the scout would be quiescence
        assert!(!e.reduces(
            &quiet,
            LATE_MOVE_THRESHOLD,
            LATE_MOVE_MIN_DEPTH - 1,
            false,
            -100,
            100
        ));
        // nor under the reference, whatever else is true of the move
        let off = reference(Board::from_fen(A_CAPTURE_AND_QUIETS).unwrap());
        assert!(!off.reduces(
            &quiet,
            LATE_MOVE_THRESHOLD,
            LATE_MOVE_MIN_DEPTH,
            false,
            -100,
            100
        ));
    }

    #[test]
    fn a_capture_is_never_reduced() {
        // the same call the quiet move is reduced under, with the capture
        // in its place. Losing captures are not told apart from the rest
        // here: this one wins a pawn, and one the swap prices as losing is
        // still a capture to the test the reduction reads
        let (e, quiet, capture) = a_quiet_and_a_capture();
        assert!(e.reduces(
            &quiet,
            LATE_MOVE_THRESHOLD,
            LATE_MOVE_MIN_DEPTH,
            false,
            -100,
            100
        ));
        assert!(!e.reduces(
            &capture,
            LATE_MOVE_THRESHOLD,
            LATE_MOVE_MIN_DEPTH,
            false,
            -100,
            100
        ));

        // a promotion is a pawn move with no victim, and is exempt on its
        // own account
        let e = reducing(Board::from_fen("7k/1P6/8/8/8/8/8/7K w - - 0 1").unwrap());
        let promotes = play_named(&e.board, "b7b8q");
        assert!(promotes.capture.is_none() && promotes.promote.is_some());
        assert!(!e.reduces(
            &promotes,
            LATE_MOVE_THRESHOLD,
            LATE_MOVE_MIN_DEPTH,
            false,
            -100,
            100
        ));
    }

    #[test]
    fn a_node_in_check_reduces_nothing() {
        // the predicate first, then the search: the rook checks along the
        // file and white has seven evasions, four king steps and three
        // interpositions, every one of them quiet. Asked at depth two the
        // node extends to three, the floor, so a reduction that ignored
        // the check would scout the evasions after the fourth, while the
        // children stand at depth two and can reduce nothing themselves.
        // So the arm searches what the reference searches, node for node,
        // if and only if the node in check declines to reduce
        let (e, quiet, _) = a_quiet_and_a_capture();
        assert!(!e.reduces(
            &quiet,
            LATE_MOVE_THRESHOLD,
            LATE_MOVE_MIN_DEPTH,
            true,
            -100,
            100
        ));

        let fen = "4r2k/8/8/8/8/8/2Q2N2/4K3 w - - 0 1";
        let board = Board::from_fen(fen).unwrap();
        assert!(board.in_check());
        let evasions = board.evasions();
        assert!(evasions.len() > LATE_MOVE_THRESHOLD, "{}", evasions.len());
        assert!(evasions.iter().all(|m| m.capture.is_none()));

        let mut e = reducing(Board::from_fen(fen).unwrap());
        let Ok(value) = e.alpha_beta(-10_000, 10_000, LATE_MOVE_MIN_DEPTH - 1, true) else {
            panic!("an unlimited search aborted");
        };
        let mut cold = reference(Board::from_fen(fen).unwrap());
        let Ok(expected) = cold.alpha_beta(-10_000, 10_000, LATE_MOVE_MIN_DEPTH - 1, true) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, cold.nodes);
        assert_eq!(value, expected);
    }

    #[test]
    fn the_mate_window_stands_the_reduction_down() {
        // either edge: a mate in hand as alpha, or one being proved against
        // the side to move as beta
        let (e, quiet, _) = a_quiet_and_a_capture();
        assert!(!e.reduces(
            &quiet,
            LATE_MOVE_THRESHOLD,
            LATE_MOVE_MIN_DEPTH,
            false,
            29_500,
            29_501
        ));
        assert!(!e.reduces(
            &quiet,
            LATE_MOVE_THRESHOLD,
            LATE_MOVE_MIN_DEPTH,
            false,
            -29_501,
            -29_500
        ));

        // and through a search: a zero width window inside the mate scores
        // turns round at every ply, so every node of the tree stands at
        // one edge or the other and none of them reduces. The arm then
        // searches what the reference searches, node for node, where the
        // same node asked under an ordinary window reduces plenty. Depth
        // five, so that the grandchildren, which a child cutting off on
        // its first move leaves searching every move of theirs, stand at
        // the floor with alpha the mate in hand
        let fen = SHARP_MIDDLEGAME;
        let mut e = reducing(Board::from_fen(fen).unwrap());
        let Ok(value) = e.alpha_beta(29_500, 29_501, 5, true) else {
            panic!("an unlimited search aborted");
        };
        let mut cold = reference(Board::from_fen(fen).unwrap());
        let Ok(expected) = cold.alpha_beta(29_500, 29_501, 5, true) else {
            panic!("an unlimited search aborted");
        };
        assert_eq!(e.nodes, cold.nodes);
        assert_eq!(value, expected);

        let mut e = reducing(Board::from_fen(fen).unwrap());
        assert!(e.alpha_beta(-1, 0, 5, true).is_ok());
        let mut cold = reference(Board::from_fen(fen).unwrap());
        assert!(cold.alpha_beta(-1, 0, 5, true).is_ok());
        assert!(
            e.nodes < cold.nodes,
            "nothing was reduced outside the mate window: {} against {}",
            e.nodes,
            cold.nodes
        );
    }

    #[test]
    fn reducing_looks_at_less_of_the_tree() {
        // the switch has to reach the search: one the search never read
        // would make every comparison with the reference a comparison of
        // the reference with itself, and the bench's two pinned counts
        // would move together instead of apart
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
    fn the_quiet_memories_look_at_less_of_the_tree_for_the_same_answer() {
        // the switch has to reach the search, the way the two shortcuts'
        // arms say it of theirs. This one owes more than they do. Nothing
        // here prunes, so the root's full window search is worth what it
        // was worth and only the tree that proved it may move.
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
    fn a_line_past_the_rail_orders_without_the_killers() {
        // the killer table is as long as the rail, and a full width line is
        // not held to it: a chain of check extensions keeps the depth where
        // it was and can carry a node past. The ply is refused there rather
        // than indexed with, and this is what says so, since a bench at
        // depth seven never gets near it
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

    #[test]
    fn a_warm_cache_matches_cold_across_draw_context() {
        // The same pieces hash to the same key whatever the fifty move counter
        // says, so a search made a few plies from the draw fills the table
        // with scores that are true of that path only. A fresh game reaching
        // the same position must not read them: its search has the whole
        // clock ahead of it.
        let near_draw = "5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 96 112";
        let fresh = "5k2/1p3p1p/p3pK1P/P1P1P3/4bP2/2B5/8/8 w - - 0 1";
        let mut warm = reference(Board::from_fen(near_draw).unwrap());
        completed(warm.search(6));
        warm.parse_fen(fresh).unwrap();
        let result = completed(warm.search(6));

        let mut cold = reference(Board::from_fen(fresh).unwrap());
        let expected = completed(cold.search(6));
        assert_eq!(result.score, expected.score);
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
        );
    }

    #[test]
    fn a_skipping_search_matches_cold_across_draw_context_with_nothing_to_refuse() {
        // the skip policy keeps tainted scores out of the table instead of
        // refusing them on the way back out, so it owes the same answer
        // warm as cold, and it should get there without the refusal ever
        // firing: what was never stored cannot need refusing. The root's
        // answer slot is the stated exception, and the probe's guard on it
        // is what keeps this test honest rather than lucky.
        //
        // The policy on the reference, as its refusing sibling above runs,
        // since the answer is only owed there: the default's move may
        // move with what the table holds, which `SearchConfig` says of it,
        // and a reduction decided by the order the table produced is what
        // made it do so here
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
        assert_eq!(
            format!("{}", result.best_move),
            format!("{}", expected.best_move),
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
        assert!(matches!(e.search(3), SearchOutcome::Complete(_)));
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
        let mut board = e.board;
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
        let mut board = e.board;
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
        let mut board = e.board;
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
        // the old cap was twenty plies measured from the root, so no line
        // could report a selective depth past it whatever the position. A
        // sharp middlegame searched shallow reaches further than that in
        // captures alone now. Depth five used to be the shallowest search
        // that cleared the old cap with room to spare; principal variation
        // search cut the tree enough that it stops at twenty exactly, and
        // six clears by a single ply. Seven reached twenty eight until the
        // late move reductions, under which it stops at nineteen and eight
        // and nine clear by a single ply each, so this asks for ten, which
        // reaches twenty seven
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
        AlphaBeta, Board, Engine, REVERSE_FUTILITY_MARGIN, REVERSE_FUTILITY_MAX_DEPTH, Score,
        SearchConfig, SearchParameters, Taint,
    };
    use crate::residual::{Sample, Sampler, Shortcut, Window, sample_key};
    use pretty_assertions::assert_eq;

    const TABLE_BYTES: usize = 1024 * 1024;
    const SHARP_MIDDLEGAME: &str = "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - 9 12";

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
        e.sample_shortcuts(Sampler::every(1));
        let mut taint = Taint::default();
        let Ok(answered) = e.shortcuts(alpha, beta, depth, false, true, &mut taint) else {
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
        assert!(e.take_sampler().is_none());
    }

    /// What every test below asks of an engine once it has searched: the
    /// sampler back, emptied into what it collected.
    fn collected(e: &mut AlphaBeta) -> crate::residual::Sampled {
        e.take_sampler().expect("a sampler was installed").drain()
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
        e.sample_shortcuts(Sampler::every(1));
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
        e.sample_shortcuts(Sampler::every(1));
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
        e.sample_shortcuts(Sampler::every(EVERY));
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
        e.sample_shortcuts(Sampler::every(1));
        let mut taint = Taint::default();
        let Ok(answered) = e.shortcuts(beta - 500, beta, 1, false, true, &mut taint) else {
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
        e.sample_shortcuts(Sampler::every(1));
        let mut taint = Taint::default();
        let Ok(answered) = e.shortcuts(beta - 500, beta, 1, false, true, &mut taint) else {
            panic!("nothing here searches under a limit, so nothing can abort");
        };
        assert!(answered.is_none());
        assert!(collected(&mut e).taken.is_empty());
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

    /// The window a real search hands the hook. Principal variation search
    /// puts every child after a node's first inside a zero width window,
    /// and at this depth that is where every shortcut that fires sits: a
    /// search this size never answers a node through an open window. A
    /// shadow row is the one exception, a candidate recorded whether or
    /// not the margin test fires, so a handful arrive through open
    /// windows, inside the re-search a zero width fail high asks for:
    /// the proof pass reopens the window and is the one place an open
    /// window carries a finite beta. The open column is pinned by
    /// `the_recorded_beta_is_the_one_the_gate_cleared`, which drives the
    /// hook directly with both windows.
    #[test]
    fn the_windows_a_search_records_are_the_zero_ones() {
        let mut e = engine(SHARP_MIDDLEGAME);
        e.sample_shortcuts(Sampler::every(1));
        e.search(6);
        let taken = collected(&mut e).taken;
        assert!(!taken.is_empty());
        for sample in &taken {
            if sample.kind != Shortcut::ShadowFutility {
                assert_eq!(sample.window, Window::Zero, "{sample:?}");
            }
        }
    }

    /// Every kind reaches the hook, not only whichever fires first. A kind
    /// that stopped being recorded would otherwise show up as a thinner
    /// distribution rather than as a failure.
    #[test]
    fn all_kinds_are_recorded() {
        let mut e = engine(SHARP_MIDDLEGAME);
        e.sample_shortcuts(Sampler::every(1));
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
            e.sample_shortcuts(Sampler::every(7));
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
        e.sample_shortcuts(Sampler::with_cap(1, 4));
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
        e.sample_shortcuts(Sampler::every(1));
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
