// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The forced decision instrument: what one shortcut decision cost the
//! root.
//!
//! The ledger and the residuals label a decision at its own node, by
//! replaying it under the reference. Whether being wrong there cost the
//! root anything is a different question, and most such errors cost
//! nothing: a re-search recovers them, or the root's move survives them.
//! This searches each root once under the default, sampling the decisions
//! it takes, and then once more for each sampled decision with that one
//! decision inverted, and reports what the root answered both times.
//!
//! A decision is addressed by its kind, a position key and a depth (see
//! `Address`), and the inversion applies wherever the search meets that
//! address: every visit, in every iteration of the deepening. The two
//! searches agree until the first such visit, since each starts from a
//! fresh engine and table with no clock.

use crate::bench::{self, Position};
use crate::engine::{AlphaBeta, Engine, SearchOutcome, SearchParameters};
use crate::late_move::Features;
use crate::misc::Score;
use crate::play::Play;
use crate::recorder::{self, Sampler};
use std::fmt;

/// About one decision in every this many, unless the command says
/// otherwise. Every kept decision costs a search of its root, so the rate
/// is far sparser than the recorders' whose rows cost nothing.
pub const DEFAULT_EVERY: u32 = 100_000;

/// What can be inverted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// The margin answered the node from its static evaluation. Inverted,
    /// the node does not answer from the margin, and the null move then
    /// gets its turn, as it would with the margin off.
    ReverseFutility,
    /// The pass cleared beta and answered the node. Inverted, the node goes
    /// on to its moves.
    NullMove,
    /// A late quiet was passed over, by the model at depth four and up or
    /// by either shallow rule below. Inverted, it is searched unreduced.
    Skip,
    /// A reduced scout came back at or below alpha and answered for its
    /// move. Inverted, the move goes on to the probe and the proof as if
    /// the scout had failed high.
    TrustedScout,
}

impl Kind {
    pub const ALL: [Kind; 4] = [
        Kind::ReverseFutility,
        Kind::NullMove,
        Kind::Skip,
        Kind::TrustedScout,
    ];

    pub fn word(self) -> &'static str {
        match self {
            Kind::ReverseFutility => "reverse_futility",
            Kind::NullMove => "null_move",
            Kind::Skip => "skip",
            Kind::TrustedScout => "trusted_scout",
        }
    }

    pub fn of_word(word: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.word() == word)
    }

    /// Spread into the sampling key, so the kinds at one position and
    /// depth are sampled apart.
    fn spread(self) -> u64 {
        (self as u64 + 1).wrapping_mul(0xd6e8_feb8_6659_fd93)
    }

    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// Which kinds a run samples, as a set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kinds(u8);

impl Kinds {
    pub const ALL: Kinds = {
        let mut set = 0;
        let mut i = 0;
        while i < Kind::ALL.len() {
            set |= Kind::ALL[i].bit();
            i += 1;
        }
        Kinds(set)
    };

    pub fn of(kinds: &[Kind]) -> Kinds {
        Kinds(kinds.iter().fold(0, |set, kind| set | kind.bit()))
    }

    pub fn holds(self, kind: Kind) -> bool {
        self.0 & kind.bit() != 0
    }

    /// The kinds named, comma separated, in the declared order.
    pub fn words(self) -> String {
        Kind::ALL
            .into_iter()
            .filter(|kind| self.holds(*kind))
            .map(Kind::word)
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// One decision, wherever the search meets it.
///
/// A node decision is keyed by the node's position. A move decision is
/// keyed by the position the move leaves, as the ledger keys its rows, so
/// for a given parent the move is part of the address without being
/// stored. Two parents that reach one child at one depth share an address,
/// and a forced run then inverts both; the row's columns describe the
/// first. The depth is the deciding node's, the check extension included.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Address {
    pub kind: Kind,
    pub key: u64,
    pub depth: u8,
}

impl Address {
    pub(crate) fn sample_key(self) -> u64 {
        recorder::sample_key(
            self.key ^ self.kind.spread(),
            recorder::FORCED_LANE,
            self.depth,
        )
    }
}

/// What answered a node once its decision was inverted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answered {
    /// The null move, where the margin was inverted.
    NullMove,
    /// The node's moves.
    Moves,
}

impl Answered {
    fn word(self) -> &'static str {
        match self {
            Answered::NullMove => "null_move",
            Answered::Moves => "moves",
        }
    }
}

/// One decision taken under the default, kept by the sampler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub address: Address,
    /// The root's place in the suite.
    pub root: usize,
    /// The decision's place among those offered over the root's search,
    /// so the first visit to an address can be told from its revisits. Not
    /// the node count, which every iteration and re-search starts again.
    pub at: u64,
    /// The move's features at the decision, for a move decision.
    pub(crate) features: Option<Features>,
    pub eval_beta: i32,
    pub alpha_gap: i32,
    /// The model's score, where the gate reads one.
    pub attention: Option<i64>,
    /// The deciding node's position.
    pub fen: String,
}

/// What an engine carries while the instrument is armed: the reservoir the
/// default run samples into, or the address a forced run inverts.
#[derive(Debug)]
pub(crate) struct Arm {
    pub(crate) sampler: Option<Sampler<Event>>,
    pub(crate) kinds: Kinds,
    /// The shallowest depth a decision is sampled at.
    pub(crate) from: u8,
    pub(crate) root: usize,
    /// Decisions offered so far, which numbers them.
    offered: u64,
    pub(crate) invert: Option<Address>,
    /// How often the search met the inverted address.
    pub(crate) visits: u64,
    /// For a node decision, what answered the node at the first inverted
    /// visit.
    pub(crate) answered: Option<Answered>,
}

impl Arm {
    pub(crate) fn recording(sampler: Sampler<Event>, kinds: Kinds, from: u8, root: usize) -> Self {
        Arm {
            sampler: Some(sampler),
            kinds,
            from,
            root,
            offered: 0,
            invert: None,
            visits: 0,
            answered: None,
        }
    }

    pub(crate) fn inverting(address: Address) -> Self {
        Arm {
            sampler: None,
            kinds: Kinds(0),
            from: 0,
            root: 0,
            offered: 0,
            invert: Some(address),
            visits: 0,
            answered: None,
        }
    }

    /// Whether this address is the one inverted, counting the visit. A
    /// pass not taken leaves the node only its moves; what answers a node
    /// whose margin is not taken is the search's to say.
    pub(crate) fn inverts(&mut self, address: Address) -> bool {
        if self.invert == Some(address) {
            self.visits += 1;
            if address.kind == Kind::NullMove {
                self.answered.get_or_insert(Answered::Moves);
            }
            true
        } else {
            false
        }
    }

    /// Offer a decision taken to the reservoir, when one is armed and the
    /// run samples its kind at its depth. `describe` is handed the root's
    /// place and the decision's number.
    pub(crate) fn offer(&mut self, address: Address, describe: impl FnOnce(usize, u64) -> Event) {
        let (root, at) = (self.root, self.offered);
        self.offered += 1;
        if !self.kinds.holds(address.kind) || address.depth < self.from {
            return;
        }
        if let Some(sampler) = self.sampler.as_mut() {
            sampler.event(address.sample_key(), || describe(root, at));
        }
    }
}

/// What one search of a root answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Answer {
    pub best: Play,
    pub score: Score,
    pub nodes: u64,
}

/// One root searched with the arm given, to the depth. The table is the
/// bench's and there is no clock, so two searches of a root agree until
/// the arm makes them differ.
fn search(position: &Position, depth: u8, arm: Arm) -> (Answer, Arm) {
    let mut engine = AlphaBeta::with_table_bytes(position.board("forced"), bench::TABLE_BYTES);
    engine.arm_forced(arm);
    let outcome =
        engine.iterative_deepening_search(SearchParameters::to_depth(depth), |_, _, _, _| {});
    let arm = engine
        .disarm_forced()
        .expect("the arm just handed to the engine comes back");
    let result = match outcome {
        SearchOutcome::Complete(result, _) => result,
        other => panic!("forced position {} answered {:?}", position.id, other),
    };
    (
        Answer {
            best: result.best_move,
            score: result.score,
            nodes: result.nodes,
        },
        arm,
    )
}

/// One kept decision, forced.
#[derive(Clone, Debug)]
pub struct Row {
    pub event: Event,
    pub on: Answer,
    pub forced: Answer,
    pub visits: u64,
    pub answered: Option<Answered>,
}

impl Row {
    /// Whether the root named another move.
    pub fn flipped(&self) -> bool {
        self.on.best != self.forced.best
    }
}

/// What a run kept and what each forced search answered.
#[derive(Clone, Debug)]
pub struct Report {
    pub depth: u8,
    pub every: u32,
    pub cap: usize,
    pub suite: Option<String>,
    pub kinds: Kinds,
    pub from: u8,
    pub positions: usize,
    /// Every decision of the sampled kinds, from the first sampled depth,
    /// that the reservoir was offered, kept or not.
    pub events: u64,
    pub overflowed: u64,
    /// Kept records, before the revisits of an address were folded.
    pub records: usize,
    /// Each root's id and what the default answered there.
    pub roots: Vec<(String, Answer)>,
    pub rows: Vec<Row>,
}

/// Search every root under the default with the reservoir armed, keep the
/// first visit's record of each address, and search each kept address's
/// root again with that decision inverted.
pub fn run(
    positions: &[Position],
    suite: Option<&str>,
    depth: u8,
    every: u32,
    cap: usize,
    kinds: Kinds,
    from: u8,
) -> Report {
    let depth = depth.max(1);
    let every = every.max(1);
    let mut sampler = Sampler::with_cap(every, cap);
    let mut answers = Vec::with_capacity(positions.len());
    for (root, position) in positions.iter().enumerate() {
        let (answer, arm) = search(position, depth, Arm::recording(sampler, kinds, from, root));
        sampler = arm
            .sampler
            .expect("the reservoir handed to the engine comes back");
        answers.push(answer);
    }
    let sampled = sampler.drain();
    let records = sampled.taken.len();
    let mut kept = sampled.taken;
    // the first visit's record of each address at a root: a revisit carries
    // its own window and features, and the forced search inverts them all
    kept.sort_by_key(|event| (event.root, event.address, event.at));
    kept.dedup_by_key(|event| (event.root, event.address));
    kept.sort_by_key(|event| (event.root, event.at));
    let rows = kept
        .into_iter()
        .map(|event| {
            let position = &positions[event.root];
            let (forced, arm) = search(position, depth, Arm::inverting(event.address));
            Row {
                on: answers[event.root],
                forced,
                visits: arm.visits,
                answered: arm.answered,
                event,
            }
        })
        .collect();
    Report {
        depth,
        every,
        cap,
        suite: suite.map(str::to_string),
        kinds,
        from,
        positions: positions.len(),
        events: sampled.events,
        overflowed: sampled.overflowed,
        records,
        roots: positions
            .iter()
            .map(|position| position.id.clone())
            .zip(answers)
            .collect(),
        rows,
    }
}

/// A header and the rows, then under `summary` the tallies for each kind
/// and what the default answered at each root.
///
/// A row is `kind depth index searched generated history history_max
/// killer tt eval_beta alpha_gap attention answered visits root best_on
/// best_forced score_on score_forced nodes_on nodes_forced fen`,
/// whitespace separated with the deciding node's fen last. `root` is the
/// root's place in the suite, which its line names. A move decision's own
/// columns print `-` on a node decision, and `attention` prints `-` below
/// the depth the gate reads the model at.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "forced depth {} every {} cap {} epd {} kinds {} from {} positions {} events {} records {} rows {} overflow {}",
            self.depth,
            self.every,
            self.cap,
            self.suite.as_deref().unwrap_or("bench"),
            self.kinds.words(),
            self.from,
            self.positions,
            self.events,
            self.records,
            self.rows.len(),
            self.overflowed,
        )?;
        for row in &self.rows {
            let e = &row.event;
            let features = match &e.features {
                Some(m) => format!(
                    "{} {} {} {} {} {} {}",
                    m.index,
                    m.index + 1,
                    m.generated,
                    m.history,
                    m.history_max,
                    u8::from(m.killer),
                    m.tt.word(),
                ),
                None => ["-"; 7].join(" "),
            };
            writeln!(
                f,
                "{} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
                e.address.kind.word(),
                e.address.depth,
                features,
                e.eval_beta,
                e.alpha_gap,
                e.attention
                    .map_or_else(|| "-".to_string(), |score| score.to_string()),
                row.answered.map_or("-", Answered::word),
                row.visits,
                e.root,
                row.on.best,
                row.forced.best,
                row.on.score,
                row.forced.score,
                row.on.nodes,
                row.forced.nodes,
                e.fen,
            )?;
        }
        writeln!(f)?;
        writeln!(f, "summary")?;
        for kind in Kind::ALL {
            if !self.kinds.holds(kind) {
                continue;
            }
            let rows: Vec<&Row> = self
                .rows
                .iter()
                .filter(|row| row.event.address.kind == kind)
                .collect();
            let unmet = rows.iter().filter(|row| row.visits == 0).count();
            let flipped = rows.iter().filter(|row| row.flipped()).count();
            writeln!(
                f,
                "kind {} forced {} unmet {} flipped {} {}",
                kind.word(),
                rows.len(),
                unmet,
                flipped,
                recorder::share(flipped, rows.len()),
            )?;
        }
        for (root, (id, answer)) in self.roots.iter().enumerate() {
            writeln!(
                f,
                "root {} best {} score {} nodes {} {}",
                root, answer.best, answer.score, answer.nodes, id,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;
    use crate::recorder::DEFAULT_CAP;
    use crate::recorder::fixtures::{recording_leaves_the_search_where_it_was, suite};
    use pretty_assertions::assert_eq;

    #[test]
    fn recording_changes_nothing() {
        recording_leaves_the_search_where_it_was(
            6,
            |engine| {
                engine.arm_forced(Arm::recording(
                    Sampler::with_cap(1, DEFAULT_CAP),
                    Kinds::ALL,
                    0,
                    0,
                ))
            },
            |engine| {
                engine
                    .disarm_forced()
                    .and_then(|arm| arm.sampler)
                    .expect("the reservoir comes back")
                    .drain()
                    .taken
                    .len()
            },
        );
    }

    /// The null run: a recording arm answers as an unarmed search does,
    /// and inverting an address the search never meets reproduces that
    /// answer and counts no visit.
    #[test]
    fn an_address_never_met_changes_nothing() {
        for position in &suite() {
            let mut unarmed = AlphaBeta::with_table_bytes(
                Board::from_fen(&position.fen).unwrap(),
                bench::TABLE_BYTES,
            );
            let SearchOutcome::Complete(result, _) =
                unarmed.iterative_deepening_search(SearchParameters::to_depth(6), |_, _, _, _| {})
            else {
                panic!("{}: an unlimited search did not complete", position.id);
            };
            let (plain, _) = search(
                position,
                6,
                Arm::recording(Sampler::with_cap(1, 0), Kinds::ALL, 0, 0),
            );
            assert_eq!(
                plain,
                Answer {
                    best: result.best_move,
                    score: result.score,
                    nodes: result.nodes,
                },
                "{}",
                position.id
            );
            for kind in Kind::ALL {
                let (forced, arm) = search(
                    position,
                    6,
                    Arm::inverting(Address {
                        kind,
                        key: 0,
                        depth: 3,
                    }),
                );
                assert_eq!(forced, plain, "{} {}", position.id, kind.word());
                assert_eq!(arm.visits, 0, "{} {}", position.id, kind.word());
                assert_eq!(arm.answered, None, "{} {}", position.id, kind.word());
            }
        }
    }

    /// Every kept decision is met when its root is searched again, each
    /// kind that was kept is forced at least once, and each address of a
    /// root has one row. A row whose visits read zero would be a decision
    /// the forced search never reached, which is the instrument failing
    /// rather than a finding. A decision met and inverted changes the tree,
    /// so a run whose forced searches count the default's nodes is failing
    /// the same way.
    #[test]
    fn every_kept_decision_is_met_when_forced_and_rowed_once() {
        let report = run(&suite(), None, 6, 40, 400, Kinds::ALL, 0);
        each_row_was_met_and_inverted(&report);
        for kind in [Kind::ReverseFutility, Kind::Skip, Kind::TrustedScout] {
            assert!(
                report.rows.iter().any(|row| row.event.address.kind == kind),
                "no {} row",
                kind.word()
            );
        }
        // the run above keeps three null moves, too few to stand on, so the
        // kind is also sampled on its own
        let report = run(&suite(), None, 6, 10, 400, Kinds::of(&[Kind::NullMove]), 0);
        each_row_was_met_and_inverted(&report);
        assert!(
            report
                .rows
                .iter()
                .all(|row| row.event.address.kind == Kind::NullMove)
        );
    }

    /// The checks every row of a run is held to, whatever its kind.
    fn each_row_was_met_and_inverted(report: &Report) {
        assert!(!report.rows.is_empty(), "nothing was kept");
        let mut addresses: Vec<_> = report
            .rows
            .iter()
            .map(|row| (row.event.root, row.event.address))
            .collect();
        addresses.sort();
        addresses.dedup();
        assert_eq!(addresses.len(), report.rows.len());
        assert!(report.records >= report.rows.len());
        // a changed tree can still add up to the default's count: one null
        // move here searched five nodes more in one aspiration search and
        // five fewer in the next. So the count is read over a kind's rows,
        // and a kind of under ten rows may have one
        for kind in Kind::ALL {
            let rows = || {
                report
                    .rows
                    .iter()
                    .filter(|row| row.event.address.kind == kind)
            };
            let same = rows()
                .filter(|row| row.forced.nodes == row.on.nodes)
                .count();
            assert!(
                same * 10 < rows().count().max(10),
                "{same} of {} {} searches counted the default's nodes",
                rows().count(),
                kind.word()
            );
        }
        for row in &report.rows {
            assert!(row.visits > 0, "{:?} was never met", row.event.address);
            match row.event.address.kind {
                Kind::ReverseFutility => assert!(row.answered.is_some()),
                Kind::NullMove => assert_eq!(row.answered, Some(Answered::Moves)),
                Kind::Skip | Kind::TrustedScout => {
                    assert_eq!(row.answered, None);
                    assert!(row.event.features.is_some());
                }
            }
        }
    }

    #[test]
    fn the_kinds_asked_for_are_the_kinds_sampled() {
        let report = run(&suite(), None, 6, 40, 400, Kinds::of(&[Kind::Skip]), 2);
        assert!(!report.rows.is_empty());
        assert!(
            report
                .rows
                .iter()
                .all(|row| row.event.address.kind == Kind::Skip && row.event.address.depth >= 2)
        );
        assert_eq!(Kinds::of(&[Kind::Skip]).words(), "skip");
        assert_eq!(
            Kinds::ALL.words(),
            "reverse_futility,null_move,skip,trusted_scout"
        );
        for kind in Kind::ALL {
            assert_eq!(Kind::of_word(kind.word()), Some(kind));
        }
        assert_eq!(Kind::of_word("skips"), None);
    }

    /// The model's score is printed where the gate reads one, depth four
    /// and up, and nowhere else. A skip there stands at least the skip's
    /// margin under beta, since the default reads the margin.
    #[test]
    fn attention_is_read_where_the_gate_reads_it() {
        let report = run(&suite(), None, 7, 20, 2000, Kinds::ALL, 0);
        let (mut deep, mut skips) = (0, 0);
        for row in &report.rows {
            let e = &row.event;
            let gated = e.features.is_some()
                && e.address.depth >= crate::late_move::DEEP_REDUCTION_MIN_DEPTH;
            assert_eq!(e.attention.is_some(), gated, "{:?}", e.address);
            // a skip there is the pruning rule's, which skips on the score
            if gated && e.address.kind == Kind::Skip {
                assert!(
                    crate::late_move::under_skip_margin(e.address.depth, i64::from(e.eval_beta)),
                    "{:?} stood {} from beta",
                    e.address,
                    e.eval_beta
                );
                skips += 1;
            }
            deep += usize::from(gated);
        }
        assert!(deep > 0, "no row at the gate's depth");
        assert!(skips > 0, "no skip at the gate's depth");
    }
}
