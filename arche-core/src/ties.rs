// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The ordering instrument: at a node whose quiet band was scored, which of
//! the quiet moves the memories key zero were tried, in what order, and
//! which of them cut.
//!
//! Those moves are tied, and `order_quiets` leaves a tie in generation
//! order, so a cutoff rate counted over them measures the generator as well
//! as the moves. With `SearchConfig::ordering_exploration` on, the order of
//! the group is drawn from the seed instead, each member of a group of `k`
//! is first at one node in `k`, and the first member's outcome is a cutoff
//! rate with nothing censored. The rows here are that data. What reads them
//! is offline.
//!
//! One event per sampled node, taken where the node answers: the cutoff and
//! the loop's natural end, as the census is. A node whose quiet band was
//! never scored, or whose band had no tie, offers nothing. The recorder
//! hangs off an engine on the reservoir's terms, and an engine without one
//! searches exactly the tree it searched before.

use crate::bench::Position;
use crate::engine::SearchConfig;
use crate::play::Play;
use crate::recorder::{self, Window, share};
use std::fmt;

/// The key a node's answer is sampled by, under this recorder's own lane.
pub fn sample_key(position_key: u64, depth: u8) -> u64 {
    recorder::sample_key(position_key, recorder::TIES_LANE, depth)
}

/// About one record in every this many events, unless the command says
/// otherwise. An event is a node and prints a row a member, a dozen or so,
/// so the rate is the census's.
pub const DEFAULT_EVERY: u32 = 1_000;

/// What became of one member of the group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// This member was searched and cut the node off.
    Cut,
    /// It was searched and did not.
    No,
    /// The node reached it and a pruning rule passed over it unsearched.
    Skipped,
    /// The node reached it and it was not a legal move.
    Illegal,
    /// The node answered before reaching it.
    Unreached,
}

impl Outcome {
    /// The word a row prints.
    pub fn word(self) -> &'static str {
        match self {
            Outcome::Cut => "cut",
            Outcome::No => "no",
            Outcome::Skipped => "skip",
            Outcome::Illegal => "illegal",
            Outcome::Unreached => "-",
        }
    }
}

/// One member of a node's group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub play: Play,
    /// Where the member stood in the group in generation order, counting
    /// from zero: the place it would have been tried in with the
    /// exploration off. The uniformity check reads it.
    pub place: usize,
    pub outcome: Outcome,
}

/// One node answering with a tie in its quiet band. Everything is owned: an
/// event outlives the search that took it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// The position, as the board prints one, last on each row.
    pub fen: String,
    /// The depth the node searched with, the check extension included.
    pub depth: u8,
    pub window: Window,
    /// The moves the node generated.
    pub generated: usize,
    /// The moves the node made and searched, the cutting move among them.
    pub searched: usize,
    /// The group, in the order the node would try it: the member at rank
    /// zero is the first draw.
    pub members: Vec<Member>,
}

impl Event {
    /// Whether the node reached the group at all. One it never reached is
    /// no evidence about any member, and its members all read unreached.
    pub fn reached(&self) -> bool {
        self.members
            .first()
            .is_some_and(|m| m.outcome != Outcome::Unreached)
    }
}

/// A whole run: what it was asked for and what it recorded.
#[derive(Clone, Debug)]
pub struct Report {
    pub depth: u8,
    pub every: u32,
    /// Stated in the header only when it is not the default.
    pub cap: usize,
    /// The seed the tie break was drawn from, or none for a run in
    /// generation order.
    pub seed: Option<u64>,
    /// The file the suite was read from, or none for the bench's own.
    pub epd: Option<String>,
    pub positions: usize,
    /// Every node offered, kept or not.
    pub events: u64,
    pub overflowed: u64,
    pub rows: Vec<Event>,
}

/// The default configuration with the exploration drawn from `seed`, or the
/// default itself for none.
pub fn config(seed: Option<u64>) -> SearchConfig {
    match seed {
        Some(seed) => SearchConfig {
            ordering_exploration: true,
            exploration_seed: seed,
            ..SearchConfig::default()
        },
        None => SearchConfig::default(),
    }
}

/// Search the suite with the instrument armed, under the default
/// configuration with the exploration drawn from `seed`. A run with no seed
/// records the group in generation order, whose first draws the uniformity
/// check says are not a draw at all.
pub fn run(
    positions: &[Position],
    epd: Option<&str>,
    depth: u8,
    every: u32,
    cap: usize,
    seed: Option<u64>,
) -> Report {
    let depth = depth.max(1);
    let every = every.max(1);
    let sampled = recorder::record(positions, depth, every, cap, config(seed));
    Report {
        depth,
        every,
        cap,
        seed,
        epd: epd.map(str::to_string),
        positions: positions.len(),
        events: sampled.events,
        overflowed: sampled.overflowed,
        rows: sampled.taken,
    }
}

/// How many bins the uniformity check splits a group's generation order
/// into. A group of `k` puts `k` places into four bins as evenly as the
/// places allow, and the expected share of each bin is worked out node by
/// node from the `k` that node had, so an uneven split is not read as a
/// broken draw.
pub const BINS: usize = 4;

/// The bin a generation place falls in, in a group of `k`.
pub fn bin(place: usize, k: usize) -> usize {
    place * BINS / k
}

/// The events of one depth, as the summary line prints them.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub depth: u8,
    /// Nodes recorded, and those that reached the group.
    pub nodes: usize,
    pub reached: usize,
    /// Members of the reached groups, summed, which the mean `k` is read
    /// from.
    pub members: usize,
    /// First draws searched, and how many of them cut. A first draw a
    /// pruning rule passed over or that was illegal is neither.
    pub first_searched: usize,
    /// First draws a pruning rule passed over.
    pub first_skipped: usize,
    pub first_cut: usize,
    /// First draws by the bin their generation place falls in, and the
    /// count each bin should hold if the draw is uniform.
    pub first_bins: [usize; BINS],
    pub expected_bins: [f64; BINS],
}

impl Report {
    /// One depth's summary, or none when the run kept no row of it.
    pub fn summary(&self, depth: u8) -> Option<Summary> {
        let rows: Vec<&Event> = self.rows.iter().filter(|row| row.depth == depth).collect();
        if rows.is_empty() {
            return None;
        }
        let mut s = Summary {
            depth,
            nodes: rows.len(),
            reached: 0,
            members: 0,
            first_searched: 0,
            first_skipped: 0,
            first_cut: 0,
            first_bins: [0; BINS],
            expected_bins: [0.0; BINS],
        };
        for row in rows.into_iter().filter(|row| row.reached()) {
            let k = row.members.len();
            s.reached += 1;
            s.members += k;
            let first = &row.members[0];
            s.first_searched += usize::from(matches!(first.outcome, Outcome::Cut | Outcome::No));
            s.first_skipped += usize::from(first.outcome == Outcome::Skipped);
            s.first_cut += usize::from(first.outcome == Outcome::Cut);
            s.first_bins[bin(first.place, k)] += 1;
            for place in 0..k {
                s.expected_bins[bin(place, k)] += 1.0 / k as f64;
            }
        }
        Some(s)
    }

    /// Every summary the run has, shallowest depth first.
    pub fn summaries(&self) -> Vec<Summary> {
        let mut depths: Vec<u8> = self.rows.iter().map(|row| row.depth).collect();
        depths.sort_unstable();
        depths.dedup();
        depths
            .into_iter()
            .filter_map(|depth| self.summary(depth))
            .collect()
    }
}

/// The report as the command prints it: a header, a row a member of every
/// recorded group, and a summary line a depth.
///
/// A row is `depth window generated searched k rank place outcome move
/// fen`, whitespace separated with the fen last. `rank` is the member's
/// place in the order the node would try the group, so rank 0 is the first
/// draw, and `place` its place in generation order. The rows of one node
/// are consecutive and in rank order.
///
/// The summary's `first` counts the first draws searched and `skipped`
/// those a pruning rule passed over, which `rate` leaves out. Its `bins` are the first draws by generation place, split in
/// four, beside the count a uniform draw puts in each. A run whose two
/// columns disagree beyond what the counts allow has a broken draw and is
/// not a reading.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ordering depth {} every {}", self.depth, self.every)?;
        if self.cap != recorder::DEFAULT_CAP {
            write!(f, " cap {}", self.cap)?;
        }
        match self.seed {
            Some(seed) => write!(f, " seed {seed}")?,
            None => write!(f, " seed none")?,
        }
        if let Some(epd) = &self.epd {
            write!(f, " epd {epd}")?;
        }
        write!(
            f,
            " positions {} events {} records {}",
            self.positions,
            self.events,
            self.rows.len(),
        )?;
        if self.overflowed > 0 {
            write!(f, " overflow {}", self.overflowed)?;
        }
        writeln!(f)?;
        for row in &self.rows {
            let k = row.members.len();
            for (rank, member) in row.members.iter().enumerate() {
                writeln!(
                    f,
                    "{} {} {} {} {} {} {} {} {} {}",
                    row.depth,
                    row.window.word(),
                    row.generated,
                    row.searched,
                    k,
                    rank,
                    member.place,
                    member.outcome.word(),
                    member.play,
                    row.fen,
                )?;
            }
        }
        writeln!(f)?;
        writeln!(f, "summary")?;
        let summaries = self.summaries();
        if summaries.is_empty() {
            writeln!(f, "records 0")?;
        }
        for s in summaries {
            write!(
                f,
                "depth {} nodes {} reached {} mean_k {} first {} skipped {} first_cut {} rate {} bins",
                s.depth,
                s.nodes,
                s.reached,
                if s.reached == 0 {
                    "-".to_string()
                } else {
                    format!("{:.1}", s.members as f64 / s.reached as f64)
                },
                s.first_searched,
                s.first_skipped,
                s.first_cut,
                share(s.first_cut, s.first_searched),
            )?;
            for (seen, expected) in s.first_bins.iter().zip(s.expected_bins) {
                write!(f, " {seen}/{expected:.0}")?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;
    use crate::engine::AlphaBeta;
    use crate::recorder::fixtures::{recording_leaves_the_search_where_it_was, suite};
    use crate::recorder::{DEFAULT_CAP, Sampler};
    use pretty_assertions::assert_eq;

    /// The key decides on the node alone, and the same node keys
    /// differently here from under every other lane.
    #[test]
    fn a_key_is_the_node_and_nothing_about_the_run() {
        let position = 0x0123_4567_89ab_cdef;
        let key = sample_key(position, 4);
        assert_eq!(key, sample_key(position, 4));
        assert_ne!(key, sample_key(position, 5));
        assert_ne!(key, sample_key(position ^ 1, 4));
        assert_ne!(key, crate::census::sample_key(position, 4));
        assert_ne!(key, crate::reduction::sample_key(position, 4));
        assert_ne!(key, crate::effort::sample_key(position, 4));
        for kind in crate::residual::Shortcut::KINDS {
            assert_ne!(key, crate::residual::sample_key(position, kind, 4));
        }
    }

    fn arm(engine: &mut AlphaBeta) {
        engine.arm(Sampler::<Event>::with_cap(1, DEFAULT_CAP));
    }

    fn take(engine: &mut AlphaBeta) -> usize {
        engine
            .disarm::<Event>()
            .expect("the reservoir comes back")
            .drain()
            .taken
            .len()
    }

    /// Under the default and under the exploring configuration this
    /// instrument searches with, which is its caller's choice.
    #[test]
    fn recording_leaves_the_search_where_it_was_under_either_configuration() {
        for seed in [None, Some(1)] {
            recording_leaves_the_search_where_it_was(4, config(seed), arm, take);
        }
    }

    /// What a row holds, read off a real run: its generation places are a
    /// permutation, one member at most cut, a group the node never reached
    /// is unreached throughout, no more members were tried than the node
    /// searched, and a member said to be skipped is a legal move where one
    /// said to be illegal is not.
    ///
    /// A group of one is recorded: its first draw is trivially uniform, and
    /// a reader that wants a choice filters on `k`.
    #[test]
    fn every_recorded_group_is_a_whole_group() {
        let report = run(&suite(), None, 5, 1, DEFAULT_CAP, Some(3));
        assert!(!report.rows.is_empty());
        let mut reached = 0;
        let mut skipped = 0;
        for row in &report.rows {
            let k = row.members.len();
            assert!(k >= 1, "{row:?}");
            let mut places: Vec<usize> = row.members.iter().map(|m| m.place).collect();
            places.sort_unstable();
            assert_eq!(places, (0..k).collect::<Vec<_>>(), "{row:?}");
            let cuts = row
                .members
                .iter()
                .filter(|m| m.outcome == Outcome::Cut)
                .count();
            assert!(cuts <= 1, "{row:?}");
            if !row.reached() {
                assert!(
                    row.members.iter().all(|m| m.outcome == Outcome::Unreached),
                    "{row:?}"
                );
                continue;
            }
            reached += 1;
            let tried = row
                .members
                .iter()
                .filter(|m| matches!(m.outcome, Outcome::Cut | Outcome::No))
                .count();
            assert!(tried <= row.searched, "{row:?}");
            let mut board = Board::from_fen(&row.fen).unwrap();
            for m in &row.members {
                let mut legal = || {
                    let made = board.make_move(&m.play);
                    if made {
                        board.undo_move();
                    }
                    made
                };
                match m.outcome {
                    Outcome::Skipped => {
                        skipped += 1;
                        assert!(legal(), "{row:?}");
                    }
                    Outcome::Illegal => assert!(!legal(), "{row:?}"),
                    _ => {}
                }
            }
            // past a cut nothing is reached, and before it everything is
            let cut_at = row.members.iter().position(|m| m.outcome == Outcome::Cut);
            for (rank, m) in row.members.iter().enumerate() {
                let past = cut_at.is_some_and(|at| rank > at);
                assert_eq!(m.outcome == Outcome::Unreached, past, "{row:?}");
            }
        }
        assert!(reached > 0, "no recorded group was reached");
        // the shallow rules skip quiet moves at this depth, so a run that
        // labelled none of them has lost the label
        assert!(skipped > 0, "no member was skipped");
    }

    /// The check the summary prints, asked of a real run: in generation
    /// order the first draw is always the first place, and drawn from a
    /// seed it is not.
    #[test]
    fn the_first_draw_sits_at_the_first_place_only_in_generation_order() {
        let first_places = |seed| {
            run(&suite(), None, 5, 1, DEFAULT_CAP, seed)
                .rows
                .iter()
                .filter(|row| row.reached())
                .map(|row| row.members[0].place)
                .collect::<Vec<_>>()
        };
        assert!(first_places(None).iter().all(|place| *place == 0));
        assert!(first_places(Some(3)).iter().any(|place| *place > 0));
    }

    /// Every place lands in a bin, and a group of four puts one in each.
    #[test]
    fn a_group_splits_into_four_bins() {
        assert_eq!((0..4).map(|p| bin(p, 4)).collect::<Vec<_>>(), [0, 1, 2, 3]);
        for k in 1..64 {
            for place in 0..k {
                assert!(bin(place, k) < BINS);
            }
        }
    }

    #[test]
    fn a_depth_of_zero_is_reported_as_the_depth_that_ran() {
        let report = run(&suite(), None, 0, 0, 50, Some(1));
        assert!(
            report
                .to_string()
                .starts_with("ordering depth 1 every 1 cap 50 seed 1 positions "),
            "{report}"
        );
    }
}
