//! What bounds a single search: the clock it may spend and the nodes it may
//! visit, measured from the moment the search began.
//!
//! One value answers every question the search asks about stopping — whether
//! it has run out, when to look again, and how long it has been going — so
//! that the count of nodes a search reports and the time it reports are read
//! from the same place. A limit reached is what `SearchOutcome::Aborted`
//! means; the depth asked for is not one of these, because reaching that is
//! how a search finishes rather than how it is cut short.

use std::time::{Duration, Instant};

/// How many nodes pass between reads of the clock. Reading it on every node
/// costs more than the few thousand nodes an overrun can add.
const POLL_INTERVAL: u64 = 3000;

#[derive(Copy, Clone, Debug)]
pub struct Limits {
    /// When the search began. The clock is measured from here, and so is the
    /// elapsed time reported beside a node count, which is what makes one
    /// divisible by the other.
    started: Instant,
    /// How long the search may run, or none for no clock.
    clock: Option<Duration>,
    /// The most nodes it may visit, u64::MAX for no budget.
    nodes: u64,
}

impl Limits {
    /// A search starting now under the clock and node budget given. This is
    /// the constructor a protocol adapter calls when a `go` arrives, so that
    /// the clock starts when the command does.
    pub fn starting_now(clock: Option<Duration>, nodes: Option<u64>) -> Self {
        Self::starting_at(Instant::now(), clock, nodes.unwrap_or(u64::MAX))
    }

    /// The same, from a stated moment rather than from now. A search whose
    /// clock has already run out is one of these, which is how a test asks
    /// for one without waiting for a real clock to pass.
    pub fn starting_at(started: Instant, clock: Option<Duration>, nodes: u64) -> Self {
        Self {
            started,
            clock,
            nodes,
        }
    }

    /// No clock and no node budget: the search runs to the depth asked of it.
    ///
    /// Not the protocol's `go infinite`, which means search until `stop` and
    /// which this engine does not answer yet. Nothing here can express that,
    /// and `check_limits` is the one place that would ask.
    pub fn unlimited() -> Self {
        Self::starting_at(Instant::now(), None, u64::MAX)
    }

    /// Whether the search must stop now, having visited this many nodes.
    pub fn expired(&self, nodes: u64) -> bool {
        nodes >= self.nodes
            || self
                .clock
                .is_some_and(|clock| self.started.elapsed() >= clock)
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
    pub fn clock(&self) -> Option<Duration> {
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
    use super::{Limits, POLL_INTERVAL};
    use pretty_assertions::assert_eq;
    use std::time::{Duration, Instant};

    /// A search whose clock ran out before it started.
    fn already_spent() -> Limits {
        Limits::starting_at(
            Instant::now() - Duration::from_secs(1),
            Some(Duration::from_millis(1)),
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
        let limits = Limits::starting_at(Instant::now(), Some(Duration::ZERO), 10);
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
    fn an_iteration_measures_the_clock_from_when_the_search_began() {
        let started = Instant::now() - Duration::from_secs(1);
        let limits = Limits::starting_at(started, Some(Duration::from_secs(2)), u64::MAX);
        // a second has gone already, so the second iteration has one left and
        // does not start the two seconds again
        assert!(limits.for_iteration(true, 0).elapsed() >= Duration::from_secs(1));
    }

    #[test]
    fn a_search_with_no_node_limit_never_takes_one_from_the_deepening() {
        let limits = Limits::starting_now(Some(Duration::from_secs(1)), None);
        assert_eq!(limits.node_budget(), u64::MAX);
        assert_eq!(limits.for_iteration(true, 5_000).node_budget(), u64::MAX);
    }
}
