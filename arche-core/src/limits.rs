// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What bounds a single search: the clock it may spend and the nodes it may
//! visit, measured from the moment the search began.
//!
//! One value answers every question the search asks about stopping — whether
//! it has run out, when to look again, whether another iteration is worth
//! beginning, and how long it has been going — so that the count of nodes a
//! search reports and the time it reports are read from the same place. A
//! limit reached is what `SearchOutcome::Aborted` means; the depth asked for
//! is not one of these, because reaching that is how a search finishes rather
//! than how it is cut short.

use std::time::{Duration, Instant};

/// How many nodes pass between reads of the clock. Reading it on every node
/// costs more than the few thousand nodes an overrun can add.
const POLL_INTERVAL: u64 = 3000;

/// The share of a budget past which another iteration is not begun, as a
/// percentage of it.
///
/// An iteration of this search costs three or four times the whole of the
/// deepening before it: over nine positions searched from cold, the elapsed
/// time through depth d divided by the time through d+1 has a median of 0.29
/// and quartiles of 0.21 and 0.33. So an iteration begun much past three
/// tenths of the budget will not finish inside it, which is far short of the
/// half a modern engine manages and is what a search with no pruning beyond
/// the transposition table costs.
///
/// Finishing is no longer the line between worth and worthless, though, since
/// an iteration cut short now answers with the root moves it did get through.
/// What an iteration begun at share f gets done, at the median ratio, is
/// (1-f)·0.29 / (f·0.71): all of itself at 0.29, three quarters at 0.35, half
/// at 0.45 and two fifths at 0.5. The line goes where the iteration given up
/// would have searched less than half of itself.
const SOFT_LIMIT_PERCENT: u128 = 45;

/// The clock a search runs under, and what the caller meant by it.
///
/// A share of a game clock is this side's own guess at what the move is
/// worth, so time left unspent on it is time still there for the moves after
/// this one. A move time is not a guess and there is nothing to save it for:
/// `go movetime 5000` asked for five seconds of thinking.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Clock {
    /// A share of a running game clock, worked out from what is left of it.
    Share(Duration),
    /// A time the caller named, spent as named.
    Fixed(Duration),
}

impl Clock {
    /// How long the search may run, whichever kind of clock this is.
    pub fn deadline(self) -> Duration {
        match self {
            Clock::Share(budget) | Clock::Fixed(budget) => budget,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct Limits {
    /// When the search began. The clock is measured from here, and so is the
    /// elapsed time reported beside a node count, which is what makes one
    /// divisible by the other.
    started: Instant,
    /// The clock the search runs under, or none for no clock.
    clock: Option<Clock>,
    /// The most nodes it may visit, u64::MAX for no budget.
    nodes: u64,
}

impl Limits {
    /// A search starting now under the clock and node budget given. This is
    /// the constructor a protocol adapter calls when a `go` arrives, so that
    /// the clock starts when the command does.
    pub fn starting_now(clock: Option<Clock>, nodes: Option<u64>) -> Self {
        Self::starting_at(Instant::now(), clock, nodes.unwrap_or(u64::MAX))
    }

    /// The same, from a stated moment rather than from now. A search whose
    /// clock has already run out is one of these, which is how a test asks
    /// for one without waiting for a real clock to pass.
    pub fn starting_at(started: Instant, clock: Option<Clock>, nodes: u64) -> Self {
        Self {
            started,
            clock,
            nodes,
        }
    }

    /// No clock and no node budget: the search runs to the depth asked of it.
    ///
    /// Not the protocol's `go infinite`, which means search until `stop`.
    /// Nothing here expresses that and nothing here should: a stop comes
    /// from another thread rather than from a number, so it rides on
    /// `SearchParameters` beside these and is read at the same poll.
    pub fn unlimited() -> Self {
        Self::starting_at(Instant::now(), None, u64::MAX)
    }

    /// Whether the search must stop now, having visited this many nodes.
    pub fn expired(&self, nodes: u64) -> bool {
        nodes >= self.nodes
            || self
                .clock
                .is_some_and(|clock| self.started.elapsed() >= clock.deadline())
    }

    /// Whether another iteration of a deepening search is worth beginning.
    ///
    /// The deadline above is the backstop, which stops a search wherever it
    /// happens to be; this is the deepening loop asking beforehand whether
    /// there is enough of the budget left for the next depth to be worth
    /// starting at all. What "enough" is, and the measurement behind it, is
    /// `SOFT_LIMIT_PERCENT`.
    ///
    /// Only a share of a game clock is given up early, since only that leaves
    /// the rest of it for the moves after this one. A named move time asked
    /// for exactly that much thinking, and a node budget, a depth and an
    /// unlimited search are not clocks at all. Neither is anything given up
    /// before a depth has been answered, because until then there is nothing
    /// to answer with.
    pub fn worth_another_iteration(&self, answered: bool) -> bool {
        if !answered {
            return true;
        }
        match self.clock {
            // nanoseconds in a u128 rather than multiplying the durations
            // themselves, which panics on a clock large enough to overflow
            Some(Clock::Share(budget)) => {
                self.started.elapsed().as_nanos() * 100 < budget.as_nanos() * SOFT_LIMIT_PERCENT
            }
            _ => true,
        }
    }

    /// The node count at which to look at the limits again: every
    /// POLL_INTERVAL nodes for the clock, and the node budget itself,
    /// exactly, which is what lets a fixed node search stop on the node it
    /// names rather than at the next poll after it.
    pub fn next_check_after(&self, nodes: u64) -> u64 {
        nodes.saturating_add(POLL_INTERVAL).min(self.nodes)
    }

    /// How long the search has been running.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// The clock, for a protocol adapter's tests to read back.
    pub fn clock(&self) -> Option<Clock> {
        self.clock
    }

    /// The node budget, likewise.
    pub fn node_budget(&self) -> u64 {
        self.nodes
    }

    /// The limits one iteration of a deepening search runs under.
    ///
    /// Until a depth has completed there is no move to answer with, so
    /// nothing may stop the search: neither the clock nor the budget is armed
    /// and depth one runs to its end. After that the clock applies as it
    /// stands, and the budget is what the iterations before this one left.
    pub fn for_iteration(&self, answered: bool, spent: u64) -> Self {
        if !answered {
            return Self::starting_at(self.started, None, u64::MAX);
        }
        Self {
            started: self.started,
            clock: self.clock,
            // a search with no budget keeps none: subtracting from the
            // sentinel would leave a number still past any real search, but
            // saying so plainly costs less than explaining that
            nodes: if self.nodes == u64::MAX {
                u64::MAX
            } else {
                self.nodes.saturating_sub(spent)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Clock, Limits, POLL_INTERVAL, SOFT_LIMIT_PERCENT};
    use pretty_assertions::assert_eq;
    use std::time::{Duration, Instant};

    /// A search whose clock ran out before it started.
    fn already_spent() -> Limits {
        Limits::starting_at(
            Instant::now() - Duration::from_secs(1),
            Some(Clock::Share(Duration::from_millis(1))),
            u64::MAX,
        )
    }

    /// A search on a budget of a second, that much of it already gone.
    fn a_second_of(kind: fn(Duration) -> Clock, spent: Duration) -> Limits {
        Limits::starting_at(
            Instant::now() - spent,
            Some(kind(Duration::from_secs(1))),
            u64::MAX,
        )
    }

    #[test]
    fn an_unlimited_search_never_expires() {
        let limits = Limits::unlimited();
        assert!(!limits.expired(0));
        assert!(!limits.expired(u64::MAX - 1));
    }

    #[test]
    fn a_spent_clock_expires_before_a_node_is_visited() {
        assert!(already_spent().expired(0));
    }

    #[test]
    fn a_node_budget_expires_on_the_node_it_names() {
        let limits = Limits::starting_at(Instant::now(), None, 100);
        assert!(!limits.expired(99));
        assert!(limits.expired(100));
    }

    #[test]
    fn the_next_check_is_a_poll_interval_away_or_the_budget() {
        let unlimited = Limits::unlimited();
        assert_eq!(unlimited.next_check_after(0), POLL_INTERVAL);
        assert_eq!(unlimited.next_check_after(10), POLL_INTERVAL + 10);

        // the budget lands exactly rather than at the poll after it
        let budgeted = Limits::starting_at(Instant::now(), None, 50);
        assert_eq!(budgeted.next_check_after(0), 50);
    }

    #[test]
    fn the_next_check_does_not_overflow_at_the_end_of_the_count() {
        let limits = Limits::unlimited();
        assert_eq!(limits.next_check_after(u64::MAX), u64::MAX);
    }

    #[test]
    fn nothing_is_armed_until_a_depth_has_been_answered() {
        let limits = Limits::starting_at(Instant::now(), Some(Clock::Share(Duration::ZERO)), 10);
        let first = limits.for_iteration(false, 0);
        assert!(!first.expired(1_000_000), "depth one was stoppable");
    }

    #[test]
    fn a_spent_clock_is_armed_once_a_depth_has_been_answered() {
        let later = already_spent().for_iteration(true, 0);
        assert!(later.expired(0));
    }

    #[test]
    fn an_iteration_gets_what_the_ones_before_it_left() {
        let limits = Limits::starting_at(Instant::now(), None, 1000);
        assert_eq!(limits.for_iteration(true, 400).node_budget(), 600);
    }

    #[test]
    fn an_iteration_after_the_budget_is_gone_has_none_left() {
        let limits = Limits::starting_at(Instant::now(), None, 100);
        let left = limits.for_iteration(true, 500);
        assert_eq!(left.node_budget(), 0);
        assert!(left.expired(0));
    }

    #[test]
    fn an_iteration_is_not_begun_once_the_soft_share_of_a_clock_has_gone() {
        // well clear of the boundary on both sides: the helper reads the
        // clock again inside the call, so a test standing a millisecond
        // from the share would fail on any scheduling stall that long
        let soft = Duration::from_millis(SOFT_LIMIT_PERCENT as u64 * 10);
        assert!(
            a_second_of(Clock::Share, soft - Duration::from_millis(50))
                .worth_another_iteration(true)
        );
        assert!(
            !a_second_of(Clock::Share, soft + Duration::from_millis(50))
                .worth_another_iteration(true)
        );
    }

    #[test]
    fn a_named_move_time_is_spent_to_its_deadline() {
        // the interface asked for this much thinking rather than for a move
        // by some clock of its own, so there is nothing to save by stopping:
        // an elapsed share that a clock budget would refuse is still begun
        let late = Duration::from_millis(990);
        assert!(a_second_of(Clock::Fixed, late).worth_another_iteration(true));
        assert!(!a_second_of(Clock::Share, late).worth_another_iteration(true));
    }

    #[test]
    fn a_search_on_no_clock_begins_every_iteration_it_is_asked_for() {
        // a node budget, a bare depth and an unlimited search all stop on
        // something the elapsed time says nothing about
        assert!(
            Limits::starting_at(Instant::now(), None, 1_000).worth_another_iteration(true),
            "a node budget was cut short by the clock"
        );
        assert!(Limits::unlimited().worth_another_iteration(true));
    }

    #[test]
    fn the_first_iteration_is_begun_whatever_the_clock_says() {
        // nothing has been answered yet, so stopping here would answer with
        // no move at all
        assert!(already_spent().worth_another_iteration(false));
        assert!(!already_spent().worth_another_iteration(true));
    }

    #[test]
    fn an_iteration_measures_the_clock_from_when_the_search_began() {
        let started = Instant::now() - Duration::from_secs(1);
        let limits = Limits::starting_at(
            started,
            Some(Clock::Share(Duration::from_secs(2))),
            u64::MAX,
        );
        // a second has gone already, so the second iteration has one left and
        // does not start the two seconds again
        assert!(limits.for_iteration(true, 0).elapsed() >= Duration::from_secs(1));
    }

    #[test]
    fn a_search_with_no_node_limit_never_takes_one_from_the_deepening() {
        let limits = Limits::starting_now(Some(Clock::Share(Duration::from_secs(1))), None);
        assert_eq!(limits.node_budget(), u64::MAX);
        assert_eq!(limits.for_iteration(true, 5_000).node_budget(), u64::MAX);
    }
}
