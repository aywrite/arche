// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The reservoir the recorders share: each hangs one off an engine, searches
//! a suite with it armed, and takes back what it kept. Each recorder's event
//! and report are in its own module.
//!
//! An engine with no reservoir armed searches the tree it searched before
//! there was a reservoir at all, which the pinned bench counts stand behind.

use crate::bench::{self, Position};
use crate::board::Board;
use crate::engine::{AlphaBeta, Engine, Recorded, SearchConfig, SearchParameters};
use crate::misc::Score;
use std::collections::BinaryHeap;

/// The window a node was searched with, read from alpha and beta alone.
///
/// A shortcut answering a zero width window wrongly costs something
/// different from one answering an open window wrongly, so rows are filtered
/// by this. The fuller pv, cut and all classification needs the node's
/// outcome, which a sample taken at a cutoff cannot know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    /// Beta is at most one above alpha.
    Zero,
    Open,
}

impl Window {
    pub fn of(alpha: Score, beta: Score) -> Self {
        if i32::from(beta) - i32::from(alpha) <= 1 {
            Window::Zero
        } else {
            Window::Open
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Window::Zero => "zw",
            Window::Open => "open",
        }
    }
}

/// The multiplier the depth is spread by before it joins the key: odd, so
/// multiplying by it loses no bits, and the fractional part of the golden
/// ratio, which shares no structure with the position key.
const DEPTH_SPREAD: u64 = 0x9e37_79b9_7f4a_7c15;

/// The lane each recorder keys under. Arbitrary constants, declared together
/// so the assertion below can see them all. Not called a salt, which a
/// scanner reads as a secret.
///
/// They differ within their top three bits, so at any rate coarser than one
/// in eight a node kept under one lane is not one another lane keeps. A new
/// lane takes a top three bit pattern none of these uses.
pub(crate) const REVERSE_FUTILITY_LANE: u64 = 0x51ed_2701_c3f8_4d95;
pub(crate) const NULL_MOVE_LANE: u64 = 0xa24b_af09_7d16_e8c3;
pub(crate) const SHADOW_FUTILITY_LANE: u64 = 0x38c6_54da_0b9e_7f12;
pub(crate) const CENSUS_LANE: u64 = 0xc5b9_128e_66d0_3a47;
pub(crate) const LEDGER_LANE: u64 = 0x6d84_3b2f_51c9_07ea;
/// Used on both of the effort instrument's sides, which join on the key.
pub(crate) const EFFORT_LANE: u64 = 0xf3b7_0c95_a41e_d682;

pub(crate) const LANES: [u64; 6] = [
    REVERSE_FUTILITY_LANE,
    NULL_MOVE_LANE,
    SHADOW_FUTILITY_LANE,
    CENSUS_LANE,
    LEDGER_LANE,
    EFFORT_LANE,
];

/// The invariant, checked by the compiler: a lane copied from another would
/// otherwise compile and quietly halve what either recorder saw.
const _: () = {
    let mut lane = 0;
    while lane < LANES.len() {
        let mut other = lane + 1;
        while other < LANES.len() {
            assert!(
                LANES[lane] >> 61 != LANES[other] >> 61,
                "two sampling lanes share their top three bits"
            );
            other += 1;
        }
        lane += 1;
    }
};

/// The key an event is sampled by: the position, the depth and the
/// recorder's lane, and nothing about the run, so two runs of the same
/// search record the same nodes.
pub(crate) fn sample_key(position_key: u64, lane: u64, depth: u8) -> u64 {
    position_key ^ lane ^ u64::from(depth).wrapping_mul(DEPTH_SPREAD)
}

/// A share as a summary prints one, or a `-` when nothing stands under it:
/// a figure with no denominator is not a zero.
pub(crate) fn share(part: usize, of: usize) -> String {
    if of == 0 {
        "-".to_string()
    } else {
        format!("{:.2}%", 100.0 * part as f64 / of as f64)
    }
}

/// A held record and the key it was drawn by, ordered by the key alone.
#[derive(Clone, Debug)]
struct Kept<T> {
    key: u64,
    sample: T,
}

// Key-only on purpose, and written out rather than derived: a derive would
// order by the sample too and make the heap depend on what a fen sorts like.
impl<T> Ord for Kept<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}

impl<T> PartialOrd for Kept<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> PartialEq for Kept<T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl<T> Eq for Kept<T> {}

/// What a sampler collected, taken away from it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sampled<T> {
    pub taken: Vec<T>,
    /// Every node offered to the sampler, kept or not: the denominator the
    /// samples are a share of.
    pub events: u64,
    /// Samples the buffer had no room for, so a long run says how much of
    /// itself it is not describing.
    pub overflowed: u64,
}

/// Records about one node in every n it is offered, picked by the node's key
/// rather than its place in the stream, so a change that reorders the tree
/// without changing which nodes are in it samples the same nodes.
#[derive(Clone, Debug)]
pub(crate) struct Sampler<T> {
    /// The largest key kept: keys spread over the whole range, so one in
    /// `every` sits at or below this.
    threshold: u64,
    /// A cap rather than a growing vector: a low rate over a deep search
    /// would otherwise ask for gigabytes of fens.
    cap: usize,
    /// A heap on the key, so the largest is at hand to give up.
    kept: BinaryHeap<Kept<T>>,
    events: u64,
    overflowed: u64,
}

/// What a sampler holds when nothing says otherwise, a megabyte or so of
/// fens.
pub const DEFAULT_CAP: usize = 10_000;

impl<T> Sampler<T> {
    #[cfg(test)]
    pub(crate) fn every(every: u32) -> Self {
        Self::with_cap(every, DEFAULT_CAP)
    }

    /// One sampler is carried across a whole run of searches, so the cap
    /// bounds the run and not any one search in it.
    pub(crate) fn with_cap(every: u32, cap: usize) -> Self {
        Self {
            // a rate of zero records everything rather than dividing by
            // zero, and `event` keeps a key equal to the threshold, so a
            // rate of one keeps u64::MAX too
            threshold: u64::MAX / u64::from(every.max(1)),
            cap,
            kept: BinaryHeap::new(),
            events: 0,
            overflowed: 0,
        }
    }

    /// Offer one node, keyed.
    ///
    /// At the cap the record with the largest key is given up, so what is
    /// left is the `cap` smallest keys of the run: a uniform draw from the
    /// whole run whatever order the events arrived in. Keeping the first
    /// arrivals instead would describe the first position of a suite and
    /// call it the suite.
    ///
    /// That holds up to ties, which are not rare: a deepening search
    /// revisits a node, and the samples behind one key differ (a revisit has
    /// its own beta and window). Which member of a tied group survives the
    /// cap depends on arrival order. The set of keys does not.
    ///
    /// The record is a closure because building one prints a fen.
    pub(crate) fn event(&mut self, key: u64, describe: impl FnOnce() -> T) {
        self.events += 1;
        if key > self.threshold {
            return;
        }
        if self.kept.len() < self.cap {
            self.kept.push(Kept {
                key,
                sample: describe(),
            });
            return;
        }
        // past the cap every wanted event costs one record, the new one or
        // the one it displaces
        self.overflowed += 1;
        let largest = match self.kept.peek() {
            Some(held) => held.key,
            // a cap of zero holds nothing and displaces nothing
            None => return,
        };
        // an equal key does not displace, which keeps the set of keys right
        if key >= largest {
            return;
        }
        self.kept.pop();
        self.kept.push(Kept {
            key,
            sample: describe(),
        });
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.kept.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.kept.is_empty()
    }

    /// Everything collected, in key order (a hash order, and unordered
    /// within a tied key), leaving the sampler empty.
    pub(crate) fn drain(&mut self) -> Sampled<T> {
        Sampled {
            taken: std::mem::take(&mut self.kept)
                .into_sorted_vec()
                .into_iter()
                .map(|held| held.sample)
                .collect(),
            events: std::mem::replace(&mut self.events, 0),
            overflowed: std::mem::replace(&mut self.overflowed, 0),
        }
    }
}

/// Search the positions with a reservoir of this kind armed, and hand back
/// what it kept. One reservoir for the whole suite, so the cap describes a
/// share of the whole run. The table is the bench's and there is no clock,
/// so what a run records does not depend on the machine.
pub(crate) fn record<T: Recorded>(
    positions: &[Position],
    depth: u8,
    every: u32,
    cap: usize,
    config: SearchConfig,
) -> Sampled<T> {
    let mut sampler = Sampler::with_cap(every, cap);
    for position in positions {
        let board = Board::from_fen(&position.fen).unwrap_or_else(|e| {
            panic!("{} position {} does not parse: {}", T::WHAT, position.id, e)
        });
        let mut engine = AlphaBeta::with_config(board, bench::TABLE_BYTES, config);
        engine.arm(sampler);
        engine.iterative_deepening_search(SearchParameters::to_depth(depth), |_, _, _, _| {});
        sampler = engine
            .disarm()
            .expect("the sampler just handed to the engine comes back");
    }
    sampler.drain()
}

/// What the recorders' tests share.
#[cfg(test)]
pub(crate) mod fixtures {
    use crate::bench::{self, Position};
    use crate::board::Board;
    use crate::engine::{AlphaBeta, Engine, SearchConfig, SearchOutcome, SearchParameters};

    /// Two positions: enough to record something, few enough for a replay
    /// to finish inside a test.
    pub(crate) fn suite() -> Vec<Position> {
        bench::parse_epd(
            "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - id \"sharp\";\n\
             r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - id \"kiwipete\";",
        )
    }

    /// Every recorder's contract: an armed engine searches the tree an
    /// unarmed one does, position by position. `take` says how many events
    /// were kept, so an armed run that recorded nothing fails.
    pub(crate) fn recording_leaves_the_search_where_it_was(
        depth: u8,
        arm: impl Fn(&mut AlphaBeta),
        take: impl Fn(&mut AlphaBeta) -> usize,
    ) {
        let searched_nodes = |engine: &mut AlphaBeta, id: &str| {
            let outcome = engine
                .iterative_deepening_search(SearchParameters::to_depth(depth), |_, _, _, _| {});
            let SearchOutcome::Complete(result, _) = outcome else {
                panic!("{id}: an unlimited search did not complete");
            };
            result.nodes
        };
        let mut kept = 0;
        for position in &suite() {
            let board = Board::from_fen(&position.fen).unwrap();
            let mut plain =
                AlphaBeta::with_config(board.clone(), bench::TABLE_BYTES, SearchConfig::default());
            let plain_nodes = searched_nodes(&mut plain, &position.id);
            let mut armed =
                AlphaBeta::with_config(board, bench::TABLE_BYTES, SearchConfig::default());
            arm(&mut armed);
            let armed_nodes = searched_nodes(&mut armed, &position.id);
            assert_eq!(armed_nodes, plain_nodes, "{}", position.id);
            kept += take(&mut armed);
        }
        assert!(kept > 0, "the armed runs recorded nothing");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn held(sampled: &Sampled<String>) -> Vec<&str> {
        sampled.taken.iter().map(String::as_str).collect()
    }

    fn key_at(share: f64) -> u64 {
        (u64::MAX as f64 * share) as u64
    }

    #[test]
    fn only_the_keys_under_the_rate_are_kept() {
        let mut sampler = Sampler::every(10);
        for share in [0.0, 0.09, 0.11, 0.5, 0.99] {
            sampler.event(key_at(share), || share.to_string());
        }
        let sampled = sampler.drain();
        assert_eq!(held(&sampled), vec!["0", "0.09"]);
        assert_eq!(sampled.overflowed, 0);
        assert_eq!(sampled.events, 5);
    }

    #[test]
    fn every_event_offered_is_counted_whatever_became_of_it() {
        let mut sampler = Sampler::with_cap(4, 1);
        for key in [1, 2, 3, u64::MAX] {
            sampler.event(key, || key.to_string());
        }
        let sampled = sampler.drain();
        assert_eq!(sampled.events, 4);
        // one held, two wanted and displaced, one the rate never wanted
        assert_eq!(sampled.taken.len(), 1);
        assert_eq!(sampled.overflowed, 2);
        sampler.event(1, || "after".to_string());
        assert_eq!(sampler.drain().events, 1);
    }

    #[test]
    fn a_rate_of_zero_keeps_every_event() {
        let mut sampler = Sampler::every(0);
        for key in [0, u64::MAX / 2, u64::MAX] {
            sampler.event(key, || key.to_string());
        }
        assert_eq!(sampler.len(), 3);
    }

    #[test]
    fn the_cap_keeps_the_smallest_keys_and_counts_the_rest() {
        let mut sampler = Sampler::with_cap(1, 3);
        for key in [50, 10, 90, 20, 70, 30, 60] {
            sampler.event(key, || key.to_string());
        }
        let sampled = sampler.drain();
        assert_eq!(held(&sampled), vec!["10", "20", "30"]);
        assert_eq!(sampled.overflowed, 4);
    }

    /// A tie is where the order independence stops, so only a fixed order is
    /// pinned: an equal key does not displace the record already held.
    #[test]
    fn a_tie_at_the_cap_is_settled_the_same_way_every_run() {
        let run = || {
            let mut sampler = Sampler::with_cap(1, 2);
            sampler.event(1, || "small".to_string());
            sampler.event(5, || "first of the tie".to_string());
            sampler.event(5, || "second of the tie".to_string());
            sampler.drain()
        };
        let sampled = run();
        assert_eq!(held(&sampled), vec!["small", "first of the tie"]);
        assert_eq!(sampled.overflowed, 1);
        assert_eq!(sampled, run());
    }

    #[test]
    fn two_orderings_of_the_same_events_keep_the_same_set() {
        let keys = [50u64, 10, 90, 20, 70, 30, 60];
        let run = |order: &[u64]| {
            let mut sampler = Sampler::with_cap(1, 4);
            for key in order {
                sampler.event(*key, || key.to_string());
            }
            sampler.drain()
        };
        let forward = run(&keys);
        let mut backward: Vec<u64> = keys.to_vec();
        backward.reverse();
        assert_eq!(forward, run(&backward));
        let mut shuffled = vec![90u64, 20, 60, 50, 30, 70, 10];
        assert_eq!(forward, run(&shuffled));
        shuffled.sort_unstable();
        assert_eq!(forward, run(&shuffled));
        assert_eq!(held(&forward), vec!["10", "20", "30", "50"]);
    }

    #[test]
    fn draining_leaves_the_reservoir_ready_to_record_again() {
        let mut sampler = Sampler::with_cap(1, 1);
        sampler.event(1, || "first".to_string());
        sampler.event(2, || "dropped".to_string());
        let first = sampler.drain();
        assert_eq!(held(&first), vec!["first"]);
        assert_eq!(first.overflowed, 1);
        assert!(sampler.is_empty());
        sampler.event(9, || "second".to_string());
        let second = sampler.drain();
        assert_eq!(held(&second), vec!["second"]);
        assert_eq!(second.overflowed, 0);
    }

    #[test]
    fn a_cap_of_nothing_counts_every_event_and_keeps_none() {
        let mut sampler = Sampler::with_cap(1, 0);
        for key in [3, 1, 2] {
            sampler.event(key, || key.to_string());
        }
        let sampled = sampler.drain();
        assert!(sampled.taken.is_empty());
        assert_eq!(sampled.overflowed, 3);
    }

    #[test]
    fn a_window_one_wide_is_a_zero_width_one() {
        assert_eq!(Window::of(10, 11), Window::Zero);
        assert_eq!(Window::of(-1, 0), Window::Zero);
        assert_eq!(Window::of(10, 12), Window::Open);
        assert_eq!(Window::of(Score::MIN + 1, Score::MAX - 1), Window::Open);
        assert_eq!(Window::Zero.word(), "zw");
        assert_eq!(Window::Open.word(), "open");
    }
}
