// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The reservoir the three recorders share.
//!
//! The residual sampler, the cutoff census and the reduction ledger each
//! hang a reservoir off an engine, search the bench's positions with it
//! armed, and take back what it kept. What differs between them is the event
//! recorded and the report printed, and each has a module of its own for
//! that. What is here is the part that does not differ: the loop that
//! searches a suite with a reservoir armed, the reservoir itself, the key
//! spread the three key by, and the window a sample reads off the node.
//!
//! An engine with no reservoir armed searches the tree it searched before
//! there was a reservoir at all, which is the claim the pinned bench counts
//! stand behind.

use crate::bench::{self, Position};
use crate::board::Board;
use crate::engine::{AlphaBeta, Engine, Recorded, SearchConfig, SearchParameters};
use crate::misc::Score;
use std::collections::BinaryHeap;

/// The window a node was searched with, as far as a sample can know it.
///
/// A zero width window asks whether the position beats one score; a wider
/// one asks what it is worth. The two are different questions and a shortcut
/// answering them wrongly costs different things, so the rows are filtered
/// by this. It is read from alpha and beta at the sample and nothing else.
/// The fuller pv, cut and all classification needs the node's outcome, which
/// a sample taken at a cutoff cannot know, so this is what is knowable here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    /// Beta is one above alpha: the node was asked a yes or no question.
    Zero,
    /// Anything wider.
    Open,
}

impl Window {
    /// The window a node with these bounds was searched with.
    pub fn of(alpha: Score, beta: Score) -> Self {
        if i32::from(beta) - i32::from(alpha) <= 1 {
            Window::Zero
        } else {
            Window::Open
        }
    }

    /// The word a row prints.
    pub fn word(self) -> &'static str {
        match self {
            Window::Zero => "zw",
            Window::Open => "open",
        }
    }
}

/// The odd multiplier the depth is spread by before it joins the key. Odd,
/// so multiplying by it loses no bits, and the fractional part of the golden
/// ratio, which is the usual choice for a constant with no structure the
/// position key could share. All three recorders key by the same spread,
/// which is why it lives here.
pub(crate) const DEPTH_SPREAD: u64 = 0x9e37_79b9_7f4a_7c15;

/// A held record and the key it was drawn by.
///
/// The key is the sampler's business rather than the record's, so it lives
/// here and not in the record a reader gets. Ordered by the key alone,
/// which is what makes a heap of these the reservoir below.
#[derive(Clone, Debug)]
struct Kept<T> {
    key: u64,
    sample: T,
}

// The four below are key-only on purpose. The reservoir ranks records by
// their key and by nothing else, and two records that key alike are
// interchangeable to it, so the sample they carry is deliberately not part
// of the comparison. They are written out rather than derived because a
// derive would order by the sample too and quietly make the heap depend on
// what a fen sorts like.
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
    /// Every node offered to the sampler, kept or not. The denominator: a
    /// run that recorded no crossings has said nothing until this says how
    /// many chances it had to record one.
    pub events: u64,
    /// Samples the buffer had no room for. Counted rather than kept, so a
    /// long run says how much of itself it is not describing.
    pub overflowed: u64,
}

/// Records about one node in every n it is offered, picked by the key of
/// the node rather than by its place in the stream. The record type is the
/// caller's: each of the three recorders offers its own event, and the
/// reservoir holds any of them without reading one.
///
/// Deterministic twice over. Two runs of the same search record the same
/// nodes, so a distribution can be reproduced from the command that printed
/// it; and the nodes recorded do not depend on the order the search reached
/// them in, so a change that reorders the tree without changing which nodes
/// are in it samples the same nodes.
#[derive(Clone, Debug)]
pub(crate) struct Sampler<T> {
    /// The largest key kept, which is the rate in the form the events are
    /// tested against. A key is spread over the whole range, so a share of
    /// one in `every` of them sits at or below this. The rate itself is not
    /// held: the report prints the one the run was asked for.
    threshold: u64,
    /// The most samples the buffer will hold. A cap rather than a growing
    /// vector: a run at a low rate over a deep search would otherwise ask
    /// for gigabytes of fens.
    cap: usize,
    /// What is held, as a heap on the key so the largest is the one at hand
    /// to give up. See `event` for why the largest is the one to give up.
    kept: BinaryHeap<Kept<T>>,
    /// Every node offered, whatever became of it.
    events: u64,
    overflowed: u64,
}

/// What a sampler holds when nothing says otherwise. Ten thousand fens is a
/// megabyte or so; a calibration run that wants more of the tree than that
/// asks its command for a larger cap. A module constant rather than an
/// associated one, so reading it does not mean naming a record type it
/// does not depend on.
pub const DEFAULT_CAP: usize = 10_000;

impl<T> Sampler<T> {
    /// Records about one node in every `every`, holding the default cap.
    /// The three recorders all state a cap, so this is the tests' way in.
    #[cfg(test)]
    pub(crate) fn every(every: u32) -> Self {
        Self::with_cap(every, DEFAULT_CAP)
    }

    /// The same, holding at most `cap` samples. One sampler is meant to be
    /// carried across a whole run of searches, so the cap it is built with
    /// bounds the run rather than any one search in it, and what survives
    /// the cap is drawn from the whole run rather than from its start.
    pub(crate) fn with_cap(every: u32, cap: usize) -> Self {
        Self {
            // held at one or more, so a rate of zero records everything
            // rather than dividing by nothing. At or below the threshold
            // rather than below it, so that a rate of one really keeps every
            // event rather than every event but the one key
            threshold: u64::MAX / u64::from(every.max(1)),
            cap,
            kept: BinaryHeap::new(),
            events: 0,
            overflowed: 0,
        }
    }

    /// Offer one node, keyed. The key decides whether it is wanted at all,
    /// and then whether it beats what the cap is already holding.
    ///
    /// At the cap the record with the largest key is the one given up, so
    /// what is left at the end is the `cap` smallest keys of the run. Those
    /// are a uniform draw from the whole run: the key says nothing about
    /// when the node was reached, so taking the smallest of them is taking
    /// an arbitrary fixed share, and it is the same share whichever order
    /// the events arrived in. Keeping the first arrivals instead would
    /// describe the first position of a suite and call it the suite.
    ///
    /// That holds up to ties, and the ties are not rare. A key names a
    /// position, a kind and a depth, and a deepening search revisits all
    /// three, so a run keys many events alike; the samples behind them are
    /// not interchangeable, since the beta and the window at a revisit are
    /// the node's second answer and not its first. Which member of a tied
    /// group survives the cap is whichever the heap happens to surface, not
    /// the earliest, and a run that offers the same events in another order
    /// can keep a different member of the same tie. The set of keys is
    /// order-independent; the samples behind a tied key are not.
    ///
    /// The record arrives as a closure because building one prints a fen,
    /// and that is not worth doing for an event that is not kept.
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
        // past the cap every wanted event costs one of them, whether it is
        // the new one or the one it displaces, so the count is the wanted
        // events the report is not describing
        self.overflowed += 1;
        let largest = match self.kept.peek() {
            Some(held) => held.key,
            // a cap of zero holds nothing and displaces nothing
            None => return,
        };
        // strictly smaller, so an equal key does not displace. That keeps
        // the set of keys right; which of a tied group is held is the heap's
        // business either way, and the doc above says so
        if key >= largest {
            return;
        }
        self.kept.pop();
        self.kept.push(Kept {
            key,
            sample: describe(),
        });
    }

    /// How many samples are held. Read by the tests; a recorder asks the
    /// drained result instead.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.kept.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.kept.is_empty()
    }

    /// Everything collected, in key order, leaving the sampler empty and
    /// ready to record again. The event and overflow counts go with it: they
    /// describe the samples handed over and not the sampler.
    ///
    /// Key order rather than the order the run met them, because a heap does
    /// not remember the latter. It is an order and not a ranking: a key is a
    /// hash of the node, so a reader gets the rows shuffled. Two records
    /// that key alike come out in no order worth relying on, which is the
    /// one thing here that is not a property of the events alone.
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
/// what it kept.
///
/// One reservoir for the whole suite, carried from each position's engine
/// to the next, so that the cap describes the run and not each position of
/// it. The rate would survive a reservoir built afresh per position, since
/// a key decides on its own node, but the cap would not: a cap per position
/// holds the first events of every position, and a reservoir over the whole
/// run holds a share of all of it.
///
/// The table is the bench's size and the search runs to a fixed depth with
/// no clock, so what a run records does not depend on the machine it ran
/// on. What each recorder does with the events afterwards, replay them or
/// print them as they stand, is its own module's business.
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

/// What the three recorders' tests share: the positions they record over,
/// and the contract each of them is held to.
#[cfg(test)]
pub(crate) mod fixtures {
    use crate::bench::{self, Position};
    use crate::board::Board;
    use crate::engine::{AlphaBeta, Engine, SearchConfig, SearchOutcome, SearchParameters};

    /// Two positions, enough for a run to have something to record and
    /// little enough for a replay to finish inside a test.
    pub(crate) fn suite() -> Vec<Position> {
        bench::parse_epd(
            "r1b2rk1/ppp1qppp/4pn2/6N1/Qn1P4/2NBP3/PP3PPP/R3K2R w KQ - id \"sharp\";\n\
             r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - id \"kiwipete\";",
        )
    }

    /// Every recorder's contract: an engine with one searches the tree an
    /// engine without one searches. Asked of the armed engine itself,
    /// position by position, rather than of two disarmed runs either side
    /// of it, since neither of those is the search under test.
    ///
    /// `arm` turns the recorder on and `take` takes it back and says how
    /// many events it kept. The count is what says the armed runs recorded
    /// at all, rather than agreeing with the plain ones by doing nothing.
    pub(crate) fn recording_leaves_the_search_where_it_was(
        depth: u8,
        arm: impl Fn(&mut AlphaBeta),
        take: impl Fn(&mut AlphaBeta) -> usize,
    ) {
        let searched_nodes = |engine: &mut AlphaBeta, id: &str| {
            let outcome = engine
                .iterative_deepening_search(SearchParameters::to_depth(depth), |_, _, _, _| {});
            let SearchOutcome::Complete(result) = outcome else {
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

    /// What a reservoir is holding, which is how every test below says which
    /// events survived. A record here is a name and nothing else: the
    /// reservoir never reads what it holds, so anything the three recorders
    /// record would say the same thing at more length.
    fn held(sampled: &Sampled<String>) -> Vec<&str> {
        sampled.taken.iter().map(String::as_str).collect()
    }

    /// A key a share of the way up the range, so a test can say where an
    /// event sits against a rate without writing sixteen hex digits out.
    fn key_at(share: f64) -> u64 {
        (u64::MAX as f64 * share) as u64
    }

    #[test]
    fn only_the_keys_under_the_rate_are_kept() {
        let mut sampler = Sampler::every(10);
        // a tenth of the range is kept, so the first two of these are in and
        // the rest are out
        for share in [0.0, 0.09, 0.11, 0.5, 0.99] {
            sampler.event(key_at(share), || share.to_string());
        }
        let sampled = sampler.drain();
        assert_eq!(held(&sampled), vec!["0", "0.09"]);
        assert_eq!(sampled.overflowed, 0);
        // every event offered is counted, kept or not, which is what makes
        // two records out of five a rate rather than a number
        assert_eq!(sampled.events, 5);
    }

    /// The event count is the denominator, so it counts what was offered and
    /// not what survived: the rate turns some away, the cap turns more away,
    /// and the count is deaf to both. It goes with the samples when they are
    /// drained, leaving the reservoir counting afresh.
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

    /// The reservoir: past the cap the largest keys are the ones given up,
    /// so what survives is the smallest keys of everything offered.
    #[test]
    fn the_cap_keeps_the_smallest_keys_and_counts_the_rest() {
        let mut sampler = Sampler::with_cap(1, 3);
        for key in [50, 10, 90, 20, 70, 30, 60] {
            sampler.event(key, || key.to_string());
        }
        let sampled = sampler.drain();
        // in key order, which is the order drain hands them over in
        assert_eq!(held(&sampled), vec!["10", "20", "30"]);
        assert_eq!(sampled.overflowed, 4);
    }

    /// Keys tie, and the tie is where the order-independence above stops.
    ///
    /// A key names a position, a kind and a depth, all three of which a
    /// deepening search revisits, so a run offers the same key more than
    /// once and the samples behind those offers differ: the beta and the
    /// window at a revisit are the node's second answer. This pins what the
    /// cap does with a tie for one fixed order, which is all that is
    /// promised. An equal key does not displace, so the record already held
    /// is the one that survives here, and a run that offered these three in
    /// another order could keep the other.
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

    /// What the reservoir is for. The retained set is a property of the
    /// events and not of when they arrived, so a change that reorders the
    /// tree without changing what is in it samples the same nodes.
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

    /// A cap of nothing holds nothing, rather than reaching into an empty
    /// heap for something to give up.
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

    /// The window is read from the bounds and says which of the two
    /// questions the node was asked.
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
