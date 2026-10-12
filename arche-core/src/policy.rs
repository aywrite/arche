// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The re-search policy instrument: each root searched once at a node
//! budget, with a rule deciding at every trusted scout and null move cut
//! whether to invert it.
//!
//! `forced` inverts one decision wherever the search meets it and asks what
//! that cost the root. A rule inverts every decision it selects in one
//! search, so the decisions act on each other, a re-search leaves its
//! entries in the table for later visits, and the work the inversions add
//! is paid out of the same budget as the rest of the search. What the root
//! answers at that budget is the reading, beside the same roots under no
//! rule.
//!
//! The inversions are the forced instrument's: a trusted scout goes on to
//! the probe and the proof, and a null move cut is not taken.

use crate::bench::{self, Position};
use crate::engine::{AlphaBeta, Engine, ScoreBound, SearchOutcome, SearchParameters};
use crate::forced::{Address, Arm, Entry, Kind, Place};
use crate::late_move::Features;
use crate::limits::Limits;
use crate::misc::Score;
use crate::play::Play;
use std::fmt;

/// What a rule can read at a decision, each as the value it reads, after
/// the transform named on it. The move inputs exist on a trusted scout
/// alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// The deciding node's depth.
    Depth,
    /// log2(1 + the decision's gap), the gap as `forced` prints it.
    Gap,
    /// The deciding node's distance from the root.
    Ply,
    /// 1 at an open window.
    Pv,
    /// The root move's place in the root's order, at most 3.
    RootIndex,
    /// 1 when the node was entered by a capture that did not check.
    Capture,
    /// 1 when the node was entered by a check.
    Check,
    /// The move's place among the searched moves, at most 16.
    Index,
    /// The history score's sign times log2(1 + its size).
    History,
    /// log2(1 + the node's largest quiet history).
    HistoryMax,
    /// The history score over the node's largest, at least one, held to
    /// [-1, 1].
    HistoryRatio,
    /// 1 for a killer.
    Killer,
    /// 1 when the node had a table move.
    Tt,
    /// The node's evaluation less beta, held to ±1000, in pawns.
    EvalBeta,
}

impl Input {
    pub const ALL: [Input; 14] = [
        Input::Depth,
        Input::Gap,
        Input::Ply,
        Input::Pv,
        Input::RootIndex,
        Input::Capture,
        Input::Check,
        Input::Index,
        Input::History,
        Input::HistoryMax,
        Input::HistoryRatio,
        Input::Killer,
        Input::Tt,
        Input::EvalBeta,
    ];

    pub fn word(self) -> &'static str {
        match self {
            Input::Depth => "depth",
            Input::Gap => "gap",
            Input::Ply => "ply",
            Input::Pv => "pv",
            Input::RootIndex => "root_index",
            Input::Capture => "capture",
            Input::Check => "check",
            Input::Index => "index",
            Input::History => "history",
            Input::HistoryMax => "history_max",
            Input::HistoryRatio => "history_ratio",
            Input::Killer => "killer",
            Input::Tt => "tt",
            Input::EvalBeta => "eval_beta",
        }
    }

    pub fn of_word(word: &str) -> Option<Input> {
        Input::ALL.into_iter().find(|input| input.word() == word)
    }

    /// Whether only a move decision has it.
    fn of_moves(self) -> bool {
        matches!(
            self,
            Input::Index
                | Input::History
                | Input::HistoryMax
                | Input::HistoryRatio
                | Input::Killer
                | Input::Tt
        )
    }
}

/// What a decision hands a rule. `eval_beta` is none where the rule does
/// not read it, since a scout pays a make and an evaluation to know it.
pub(crate) struct Offer {
    pub(crate) depth: u8,
    pub(crate) gap: i32,
    pub(crate) place: Place,
    pub(crate) features: Option<Features>,
    pub(crate) eval_beta: Option<i32>,
}

impl Offer {
    fn value(&self, input: Input) -> f64 {
        let log = |x: i32| f64::from(x.max(0)).ln_1p() / std::f64::consts::LN_2;
        let flag = |b: bool| f64::from(u8::from(b));
        // a rule is refused a move input on a node decision when it is read
        let features = || {
            self.features
                .expect("a move input is read on move decisions alone")
        };
        match input {
            Input::Depth => f64::from(self.depth),
            Input::Gap => log(self.gap),
            Input::Ply => f64::from(self.place.ply),
            Input::Pv => flag(self.place.pv),
            Input::RootIndex => f64::from(self.place.root_index.min(3)),
            Input::Capture => flag(self.place.came_by == Some(Entry::Capture)),
            Input::Check => flag(self.place.came_by == Some(Entry::Check)),
            Input::Index => features().index.min(16) as f64,
            Input::History => {
                let history = features().history;
                f64::from(history.signum()) * log(history.saturating_abs())
            }
            Input::HistoryMax => log(features().history_max),
            Input::HistoryRatio => {
                let f = features();
                (f64::from(f.history) / f64::from(f.history_max.max(1))).clamp(-1.0, 1.0)
            }
            Input::Killer => flag(features().killer),
            Input::Tt => flag(features().tt == crate::census::Table::Move),
            Input::EvalBeta => {
                let eval_beta = self
                    .eval_beta
                    .expect("the evaluation is read where a rule asks for it");
                f64::from(eval_beta.clamp(-1000, 1000)) / 100.0
            }
        }
    }
}

/// One kind's rule: a decision is inverted when every bound holds and the
/// weighted sum of its inputs is above the threshold.
#[derive(Clone, Debug, PartialEq)]
struct Rule {
    kind: Kind,
    threshold: f64,
    bounds: Vec<(Input, Option<f64>, Option<f64>)>,
    weights: Vec<(Input, f64)>,
}

impl Rule {
    fn reads(&self, input: Input) -> bool {
        self.bounds.iter().any(|(i, _, _)| *i == input)
            || self.weights.iter().any(|(i, _)| *i == input)
    }

    /// Whether every bound holds, passing over those on an input the offer
    /// does not carry yet when `known_only`.
    fn within(&self, offer: &Offer, known_only: bool) -> bool {
        self.bounds.iter().all(|&(input, lo, hi)| {
            if known_only && input == Input::EvalBeta && offer.eval_beta.is_none() {
                return true;
            }
            let x = offer.value(input);
            lo.is_none_or(|lo| x >= lo) && hi.is_none_or(|hi| x <= hi)
        })
    }

    fn selects(&self, offer: &Offer) -> bool {
        let within = self.within(offer, false);
        within
            && self
                .weights
                .iter()
                .map(|&(input, weight)| weight * offer.value(input))
                .sum::<f64>()
                > self.threshold
    }
}

/// The rules a search runs under, a kind at most one each, and the
/// addresses a test names. Empty is the search under no rule.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Policy {
    rules: Vec<Rule>,
    addresses: Vec<Address>,
    /// The lines read, comments and blank lines dropped, for the header.
    lines: Vec<String>,
}

/// The kinds a policy decides.
const KINDS: [Kind; 2] = [Kind::NullMove, Kind::TrustedScout];

impl Policy {
    /// The rules in a file's text, one a line:
    ///
    /// ```text
    /// <kind> <threshold> <input>=<lo>:<hi> ... | <input>*<weight> ...
    /// address <kind> <key> <depth>
    /// ```
    ///
    /// A bound may leave either end open, and one whose ends cross is
    /// refused. Every rule carries a lower bound of at least one on
    /// `depth`, so it acts nowhere it was not asked to. `address` selects
    /// one address as `forced` does, the key in hexadecimal, for the tests.
    /// `#` starts a comment.
    pub fn parse(text: &str) -> Result<Policy, String> {
        let mut policy = Policy::default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let refused = |why: &str| format!("rule: {line} ({why})");
            let mut words = line.split_whitespace();
            let first = words.next().unwrap_or("");
            if first == "address" {
                let kind = words
                    .next()
                    .and_then(Kind::of_word)
                    .filter(|k| KINDS.contains(k));
                let key = words
                    .next()
                    .filter(|w| w.chars().all(|c| c.is_ascii_hexdigit()))
                    .and_then(|w| u64::from_str_radix(w, 16).ok());
                let depth = words.next().and_then(|w| w.parse::<u8>().ok());
                match (kind, key, depth, words.next()) {
                    (Some(kind), Some(key), Some(depth), None) => {
                        policy.addresses.push(Address { kind, key, depth })
                    }
                    _ => return Err(refused("address <kind> <key> <depth>")),
                }
                policy.lines.push(line.to_string());
                continue;
            }
            let kind = Kind::of_word(first)
                .filter(|kind| KINDS.contains(kind))
                .ok_or_else(|| refused("a rule decides null_move or trusted_scout"))?;
            if policy.rules.iter().any(|rule| rule.kind == kind) {
                return Err(refused("a kind ruled twice"));
            }
            let threshold = words
                .next()
                .and_then(|w| w.parse::<f64>().ok())
                .filter(|t| t.is_finite())
                .ok_or_else(|| refused("no threshold"))?;
            let mut rule = Rule {
                kind,
                threshold,
                bounds: Vec::new(),
                weights: Vec::new(),
            };
            let mut scoring = false;
            for word in words {
                if word == "|" {
                    if scoring {
                        return Err(refused("a second bar"));
                    }
                    scoring = true;
                    continue;
                }
                let (name, rest, separator) = match word.split_once(if scoring { '*' } else { '=' })
                {
                    Some((name, rest)) => (name, rest, scoring),
                    None => return Err(refused(&format!("{word} is not a bound or a weight"))),
                };
                let input = Input::of_word(name)
                    .ok_or_else(|| refused(&format!("{name} is not an input")))?;
                if input.of_moves() && kind != Kind::TrustedScout {
                    return Err(refused(&format!("{name} is an input of moves")));
                }
                let number = |w: &str| -> Result<Option<f64>, String> {
                    if w.is_empty() {
                        return Ok(None);
                    }
                    w.parse::<f64>()
                        .ok()
                        .filter(|x| x.is_finite())
                        .map(Some)
                        .ok_or_else(|| refused(&format!("{w} is not a number")))
                };
                if separator {
                    let weight =
                        number(rest)?.ok_or_else(|| refused("a weight without a number"))?;
                    rule.weights.push((input, weight));
                } else {
                    let (lo, hi) = rest
                        .split_once(':')
                        .ok_or_else(|| refused(&format!("{word} is not <lo>:<hi>")))?;
                    let (lo, hi) = (number(lo)?, number(hi)?);
                    if let (Some(lo), Some(hi)) = (lo, hi)
                        && lo > hi
                    {
                        return Err(refused(&format!("{word} holds nothing")));
                    }
                    rule.bounds.push((input, lo, hi));
                }
            }
            if !rule
                .bounds
                .iter()
                .any(|(input, lo, _)| *input == Input::Depth && lo.is_some_and(|lo| lo >= 1.0))
            {
                return Err(refused("no depth floor"));
            }
            policy.rules.push(rule);
            policy.lines.push(line.to_string());
        }
        Ok(policy)
    }

    fn rule(&self, kind: Kind) -> Option<&Rule> {
        self.rules.iter().find(|rule| rule.kind == kind)
    }

    /// Whether a decision of this kind must say what its input is.
    pub(crate) fn reads(&self, kind: Kind, input: Input) -> bool {
        self.rule(kind).is_some_and(|rule| rule.reads(input))
    }

    /// Whether the decision could be inverted once the evaluation is known:
    /// false only where a bound on another input already fails.
    pub(crate) fn may_select(&self, address: Address, offer: &Offer) -> bool {
        self.addresses.contains(&address)
            || self
                .rule(address.kind)
                .is_some_and(|rule| rule.within(offer, true))
    }

    /// Whether the decision at this address is inverted.
    pub(crate) fn selects(&self, address: Address, offer: &Offer) -> bool {
        self.addresses.contains(&address)
            || self
                .rule(address.kind)
                .is_some_and(|rule| rule.selects(offer))
    }
}

/// What one root answered at the budget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub id: String,
    /// None when the root has no move.
    pub best: Option<Play>,
    pub score: Score,
    /// The deepest depth completed with an exact score.
    pub depth: u8,
    /// Whether an interrupted iteration's move answered in place of that
    /// depth's.
    pub swapped: bool,
    pub nodes: u64,
    /// The trusted scouts and null move cuts the rule inverted, every
    /// visit counted.
    pub scouts: u64,
    pub nulls: u64,
}

/// One root searched under the rule to the budget, from a fresh table on
/// one thread. Under no rule the armed search answers as the unarmed one
/// does at the same budget, which a test holds.
pub fn search(position: &Position, policy: &Policy, nodes: u64) -> Root {
    let mut engine = AlphaBeta::with_table_bytes(position.board("policy"), bench::TABLE_BYTES);
    engine.arm_forced(Arm::deciding(policy.clone()));
    let mut completed: Option<(u8, Play)> = None;
    let outcome = engine.iterative_deepening_search(
        SearchParameters::new(None, Limits::starting_now(None, Some(nodes))),
        |depth, result, _, bound| {
            if bound == ScoreBound::Exact {
                completed = Some((depth, result.best_move));
            }
        },
    );
    let arm = engine
        .disarm_forced()
        .expect("the arm just handed to the engine comes back");
    let result = match outcome {
        SearchOutcome::Complete(result, _) | SearchOutcome::Aborted(Some(result)) => Some(result),
        SearchOutcome::GameOver | SearchOutcome::Aborted(None) => None,
    };
    let (depth, completed_move) = completed.map_or((0, None), |(d, m)| (d, Some(m)));
    Root {
        id: position.id.clone(),
        best: result.as_ref().map(|r| r.best_move),
        score: result.as_ref().map_or(0, |r| r.score),
        depth,
        swapped: result.is_some() && result.as_ref().map(|r| r.best_move) != completed_move,
        nodes: result.as_ref().map_or(0, |r| r.nodes),
        scouts: arm.decided[Kind::TrustedScout as usize],
        nulls: arm.decided[Kind::NullMove as usize],
    }
}

/// What a run asked for and what each root answered.
#[derive(Clone, Debug)]
pub struct Report {
    pub rule: Option<String>,
    pub nodes: u64,
    pub suite: Option<String>,
    pub lines: Vec<String>,
    pub roots: Vec<Root>,
}

/// Every root under the rule at the budget, in the suite's order.
pub fn run(
    positions: &[Position],
    suite: Option<&str>,
    rule: Option<&str>,
    policy: &Policy,
    nodes: u64,
) -> Report {
    Report {
        rule: rule.map(str::to_string),
        nodes,
        suite: suite.map(str::to_string),
        lines: policy.lines.clone(),
        roots: positions
            .iter()
            .map(|position| search(position, policy, nodes))
            .collect(),
    }
}

/// A header that reruns it, the rule's lines, and a line a root: `root <i>
/// best <move> score <cp> depth <d> swapped <0|1> nodes <n> scouts <n> nulls
/// <n> <id>`, with `best none` at a root that has no move and the root's id
/// last, since an id may hold spaces.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "policy rule {} nodes {} epd {} positions {}",
            self.rule.as_deref().unwrap_or("none"),
            self.nodes,
            self.suite.as_deref().unwrap_or("bench"),
            self.roots.len(),
        )?;
        for line in &self.lines {
            writeln!(f, "rule {line}")?;
        }
        for (index, root) in self.roots.iter().enumerate() {
            writeln!(
                f,
                "root {} best {} score {} depth {} swapped {} nodes {} scouts {} nulls {} {}",
                index,
                root.best
                    .map_or_else(|| "none".to_string(), |m| m.to_string()),
                root.score,
                root.depth,
                u8::from(root.swapped),
                root.nodes,
                root.scouts,
                root.nulls,
                root.id,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forced::{self, Kinds};
    use crate::recorder::fixtures::suite;
    use pretty_assertions::assert_eq;

    /// A budget the two positions reach depth seven or so on.
    const NODES: u64 = 60_000;

    fn parsed(text: &str) -> Policy {
        Policy::parse(text).unwrap_or_else(|why| panic!("{text}: {why}"))
    }

    /// The null run: under no rule a root answers as the unarmed search
    /// does at the same budget, move, score and nodes.
    #[test]
    fn no_rule_answers_as_the_unarmed_search() {
        for position in &suite() {
            let mut unarmed =
                AlphaBeta::with_table_bytes(position.board("policy"), bench::TABLE_BYTES);
            let mut depth = 0;
            let outcome = unarmed.iterative_deepening_search(
                SearchParameters::new(None, Limits::starting_now(None, Some(NODES))),
                |d, _, _, bound| {
                    if bound == ScoreBound::Exact {
                        depth = d;
                    }
                },
            );
            let SearchOutcome::Aborted(Some(result)) = outcome else {
                panic!("{}: the budget did not stop the search", position.id);
            };
            let root = search(position, &Policy::default(), NODES);
            assert_eq!(
                (root.best, root.score, root.nodes, root.depth),
                (Some(result.best_move), result.score, result.nodes, depth),
                "{}",
                position.id
            );
            assert_eq!((root.scouts, root.nulls), (0, 0));
            // a bound that never holds is no rule, and nor is a rule that
            // reads every input and never selects: reading them, the
            // scout's evaluation among them, leaves the tree alone
            let never = parsed("trusted_scout -1 depth=99:\nnull_move -1 depth=99:");
            assert_eq!(search(position, &never, NODES), root, "{}", position.id);
            let reads = parsed(
                "trusted_scout 1e9 depth=1: eval_beta=-100:100 | depth*1 gap*1 ply*1 pv*1 \
                 root_index*1 capture*1 check*1 index*1 history*1 history_max*1 \
                 history_ratio*1 killer*1 tt*1 eval_beta*1\n\
                 null_move 1e9 depth=1: | eval_beta*1 gap*1",
            );
            assert_eq!(search(position, &reads, NODES), root, "{}", position.id);
        }
    }

    /// An address names one decision as `forced` does: the visits it
    /// inverts and what the root answers are the forced row's.
    #[test]
    fn an_address_inverts_what_forced_inverts() {
        let positions = suite();
        let report = forced::run(
            &positions,
            None,
            forced::Reach::Depth(6),
            None,
            40,
            400,
            Kinds::of(&[Kind::TrustedScout, Kind::NullMove]),
            0,
        );
        let mut checked = [0; 2];
        for row in &report.rows {
            let a = row.event.address;
            let policy = parsed(&format!(
                "address {} {:x} {}",
                a.kind.word(),
                a.key,
                a.depth
            ));
            let (answer, arm) =
                forced::search(&positions[row.event.root], 6, Arm::deciding(policy));
            assert_eq!(answer, row.forced, "{a:?}");
            assert_eq!(arm.visits, row.visits, "{a:?}");
            assert_eq!(arm.decided[a.kind as usize], row.visits, "{a:?}");
            checked[usize::from(a.kind == Kind::NullMove)] += 1;
        }
        assert!(checked[0] > 0 && checked[1] > 0, "{checked:?}");
    }

    /// A rule that selects every decision of its kind from a depth inverts
    /// them and changes the tree, and one that reads every input of its
    /// kind can be asked them.
    #[test]
    fn a_rule_inverts_what_it_selects() {
        let all = parsed("trusted_scout -1 depth=3:\nnull_move -1 depth=3:");
        let every_input = parsed(
            "trusted_scout -100 depth=3: | depth*1 gap*1 ply*1 pv*1 root_index*1 capture*1 \
             check*1 index*1 history*1 history_max*1 history_ratio*1 killer*1 tt*1 eval_beta*1\n\
             null_move -100 depth=3: | depth*1 gap*1 ply*1 pv*1 root_index*1 capture*1 check*1 \
             eval_beta*1",
        );
        for position in &suite() {
            let plain = search(position, &Policy::default(), NODES);
            let root = search(position, &all, NODES);
            assert!(root.scouts > 0 && root.nulls > 0, "{root:?}");
            assert_eq!(root.nodes, plain.nodes, "a budget is a budget");
            let read = search(position, &every_input, NODES);
            assert!(read.scouts > 0, "{read:?}");
        }
    }

    #[test]
    fn a_rule_says_what_it_inverts_or_is_refused() {
        let refused = |text: &str| Policy::parse(text).expect_err(text);
        assert!(refused("skip 0 depth=4:").contains("decides null_move or trusted_scout"));
        assert!(refused("trusted_scout 0 index=:4").contains("no depth floor"));
        assert!(refused("trusted_scout 0 depth=:9").contains("no depth floor"));
        assert!(refused("null_move 0 depth=4: index=:4").contains("an input of moves"));
        assert!(refused("null_move 0 depth=4: | history*1").contains("an input of moves"));
        assert!(refused("null_move x depth=4:").contains("no threshold"));
        assert!(refused("null_move 0 depth=4: | gap*x").contains("x is not a number"));
        assert!(refused("null_move 0 depth=4: | | gap*1").contains("a second bar"));
        assert!(refused("null_move 0 depth=4: size=1:2").contains("size is not an input"));
        assert!(refused("null_move 0 depth=4\n").contains("is not <lo>:<hi>"));
        assert!(refused("null_move 0 depth=4:\nnull_move 1 depth=5:").contains("ruled twice"));
        assert!(refused("address skip 1f 4").contains("address <kind> <key> <depth>"));
        assert!(refused("address null_move +1f 4").contains("address <kind> <key> <depth>"));
        assert!(refused("null_move 0 depth=0:").contains("no depth floor"));
        assert!(refused("null_move 0 depth=-5:").contains("no depth floor"));
        assert!(refused("null_move 0 depth=4: gap=3:2").contains("gap=3:2 holds nothing"));
        let policy = parsed("# a comment\n\nnull_move 0.5 depth=4:8 | gap*-1 # and another\n");
        assert_eq!(
            policy.lines,
            vec!["null_move 0.5 depth=4:8 | gap*-1".to_string()]
        );
        assert!(policy.reads(Kind::NullMove, Input::Gap));
        assert!(!policy.reads(Kind::NullMove, Input::EvalBeta));
        assert!(!policy.reads(Kind::TrustedScout, Input::Depth));
    }

    /// Each input read off one offer through its transform.
    #[test]
    fn an_offer_reads_its_inputs_through_their_transforms() {
        let offer = Offer {
            depth: 7,
            gap: 15,
            place: Place {
                ply: 5,
                pv: true,
                root_index: 9,
                came_by: Some(Entry::Capture),
            },
            features: Some(Features {
                index: 30,
                generated: 40,
                history: -255,
                history_max: 1023,
                killer: true,
                tt: crate::census::Table::Move,
            }),
            eval_beta: Some(-1500),
        };
        let read: Vec<f64> = Input::ALL.iter().map(|i| offer.value(*i)).collect();
        assert_eq!(
            read,
            vec![
                7.0,
                4.0,
                5.0,
                1.0,
                3.0,
                1.0,
                0.0,
                16.0,
                -8.0,
                10.0,
                -255.0 / 1023.0,
                1.0,
                1.0,
                -10.0
            ]
        );
        for input in Input::ALL {
            assert_eq!(Input::of_word(input.word()), Some(input));
        }
    }
}
