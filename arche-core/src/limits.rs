// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What bounds a single search: the clock it may spend and the nodes it may
//! visit, measured from the moment the search began.
//!
//! One value answers every question the search asks about stopping, so
//! that the node count and the time a search reports are read from the
//! same place. A limit reached is what `SearchOutcome::Aborted` means; the
//! depth asked for is not a limit, since reaching it is how a search
//! finishes.

use std::time::{Duration, Instant};

/// How many nodes pass between reads of the clock. Reading it on every node
/// costs more than the few thousand nodes an overrun can add.
const POLL_INTERVAL: u64 = 3000;

/// The share of a budget past which another iteration is not begun, as a
/// percentage, before the root has said how settled it is.
///
/// An iteration cut short still answers with the root moves it got through,
/// so with r the time through depth d over the time through d+1, an
/// iteration begun at share f gets (1-f)·r / (f·(1-r)) of itself done. The
/// line was put where that is a half, at a median r of 0.29 measured before
/// any pruning beyond the transposition table. The pruning since has raised
/// the median to 0.51 in games at 10+0.1, where the same rule would put the
/// line near two thirds, but the games keep it here: 55% lost and 65%
/// measured nothing (the roadmap has both). Those moved the line for every
/// move. The constants below move it a depth at a time, on what the root
/// says, and leave it here where the root says nothing.
const SOFT_LIMIT_PERCENT: u128 = 45;

/// Once a depth has been answered the line moves with how much of that
/// depth's nodes went to the move it chose: down to `SETTLED_PERCENT` when
/// the move took `SETTLED_PERMILLE` of them or more, up to
/// `UNSETTLED_PERCENT` when it took `UNSETTLED_PERMILLE` or less (down to
/// `TABLE_ANSWERED_PERMILLE`), and on a straight line between, which crosses `SOFT_LIMIT_PERCENT` at 700. A
/// move that took most of the tree has had its alternatives refuted
/// cheaply and seldom changes at the next depth; one that took a minority
/// has rivals that cost as much as it did.
const SETTLED_PERCENT: u128 = 30;
const SETTLED_PERMILLE: u128 = 900;
const UNSETTLED_PERCENT: u128 = 60;
const UNSETTLED_PERMILLE: u128 = 500;

/// Under this many thousandths the chosen move's nodes say nothing either
/// way and the line stays at `SOFT_LIMIT_PERCENT`. The table takes cutoffs
/// at open windows, so a move it answered from an entry costs a handful of
/// nodes while its rivals cost a search: cheap to confirm, not contested.
/// In 200 games of a 10+0.1 match replayed at a node budget, over the
/// depths ending where the line decides, a move under a tenth kept its
/// answer at the next depth 96% of the time, one over nine tenths 98.5%,
/// and one between a tenth and a half 80% to 85%.
const TABLE_ANSWERED_PERMILLE: u128 = 100;

/// The line after a depth that chose another move than the depth before
/// it, whatever its nodes say. Still under the deadline, which stays the
/// share.
const CHANGED_PERCENT: u128 = 65;

/// How many times the last completed depth's time the next is assumed to
/// cost. A depth is not begun when that much would run past the share: it
/// would most likely be cut at the deadline, and a cut depth's time is
/// spent without an answer. Replayed from 200 games of a 10+0.1 match,
/// 22.4% of all thinking went to depths the share cut, and the next depth
/// cost 1.45 times the last at the median and 4.35 at the ninth decile. The
/// last depth took no longer than all that has gone, so this binds only
/// past a third of the share, under a soft line set above that.
const DEPTH_GROWTH: u32 = 2;

/// How a completed depth spent its nodes at the root, which is what the
/// soft line reads.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RootNodes {
    /// The nodes searched under the move the depth chose, over every
    /// search of the depth (the aspiration re-searches included).
    pub chosen: u64,
    /// The nodes searched under every root move, over the same searches.
    pub total: u64,
    /// Whether the move chosen differs from the one the depth before chose.
    pub changed: bool,
}

impl RootNodes {
    /// The soft line this depth sets for the next, as a percentage of the
    /// share.
    pub fn soft_line_percent(self) -> u128 {
        if self.changed {
            return CHANGED_PERCENT;
        }
        if self.total == 0 {
            return SOFT_LIMIT_PERCENT;
        }
        let permille = self.chosen as u128 * 1000 / self.total as u128;
        if permille < TABLE_ANSWERED_PERMILLE {
            return SOFT_LIMIT_PERCENT;
        }
        let permille = permille.clamp(UNSETTLED_PERMILLE, SETTLED_PERMILLE);
        SETTLED_PERCENT
            + (UNSETTLED_PERCENT - SETTLED_PERCENT) * (SETTLED_PERMILLE - permille)
                / (SETTLED_PERMILLE - UNSETTLED_PERMILLE)
    }
}

/// The clock a search runs under, and what the caller meant by it. A share
/// of a game clock is this side's guess at what the move is worth, so time
/// left unspent is there for the moves after; a move time asked for
/// exactly that much thinking and there is nothing to save it for.
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
    /// When the search began. The clock and the elapsed time reported
    /// beside a node count are both measured from here.
    started: Instant,
    /// The clock the search runs under, or none for no clock.
    clock: Option<Clock>,
    /// The most nodes it may visit, u64::MAX for no budget.
    nodes: u64,
}

impl Limits {
    /// A search starting now under the clock and node budget given: what a
    /// protocol adapter calls when a `go` arrives, so the clock starts with
    /// the command.
    pub fn starting_now(clock: Option<Clock>, nodes: Option<u64>) -> Self {
        Self::starting_at(Instant::now(), clock, nodes.unwrap_or(u64::MAX))
    }

    /// The same from a stated moment, which lets a test start a clock in
    /// the past rather than wait for one to run out.
    pub fn starting_at(started: Instant, clock: Option<Clock>, nodes: u64) -> Self {
        Self {
            started,
            clock,
            nodes,
        }
    }

    /// No clock and no node budget: the search runs to the depth asked of
    /// it. Not the protocol's `go infinite`, whose `stop` comes from
    /// another thread and rides on `SearchParameters`, read at the same
    /// poll.
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

    /// Whether another iteration of a deepening search is worth beginning,
    /// with `expired` as the backstop, given how the last completed depth
    /// spent its nodes at the root. Only a share of a game clock is given
    /// up early, at the line `RootNodes::soft_line_percent` reads, since
    /// only that leaves the rest for the moves after this one. Nothing is
    /// given up before a depth has been answered, which is `None`.
    pub fn worth_another_iteration(&self, last: Option<RootNodes>) -> bool {
        let Some(last) = last else {
            return true;
        };
        match self.clock {
            // nanoseconds in a u128: multiplying the durations themselves
            // panics on a clock large enough to overflow
            Some(Clock::Share(budget)) => {
                self.started.elapsed().as_nanos() * 100
                    < budget.as_nanos() * last.soft_line_percent()
            }
            _ => true,
        }
    }

    /// Whether the share has room for another depth, given how long the
    /// last completed one took. Only a share of a game clock is asked; a
    /// move time and a node budget are spent as named.
    pub fn can_pay_for_another_depth(&self, took: Duration) -> bool {
        match self.clock {
            Some(Clock::Share(budget)) => {
                self.started.elapsed().as_nanos() + took.as_nanos() * DEPTH_GROWTH as u128
                    <= budget.as_nanos()
            }
            _ => true,
        }
    }

    /// The node count at which to look at the limits again: every
    /// `POLL_INTERVAL` nodes, or the node budget itself if that comes
    /// first, so a fixed node search stops on the node it names.
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

    /// The limits one iteration of a deepening search runs under. Until a
    /// depth has completed there is no move to answer with, so nothing is
    /// armed. After that the clock applies as it stands, and the budget is
    /// what the iterations before this one left.
    pub fn for_iteration(&self, answered: bool, spent: u64) -> Self {
        if !answered {
            return Self::starting_at(self.started, None, u64::MAX);
        }
        Self {
            started: self.started,
            clock: self.clock,
            // a search with no budget keeps none
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
    use super::{Clock, Limits, POLL_INTERVAL, RootNodes, SOFT_LIMIT_PERCENT};
    use pretty_assertions::assert_eq;
    use std::time::{Duration, Instant};

    /// A depth whose chosen move took this many of its thousand nodes.
    fn took(chosen: u64) -> RootNodes {
        RootNodes {
            chosen,
            total: 1000,
            changed: false,
        }
    }

    /// A depth answered with the line where it stood before the root was
    /// read.
    fn answered() -> Option<RootNodes> {
        Some(took(700))
    }

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
        // well clear of the boundary on both sides: the call reads the
        // clock again, so a test a millisecond from the share would fail on
        // any scheduling stall that long
        let soft = Duration::from_millis(SOFT_LIMIT_PERCENT as u64 * 10);
        assert!(
            a_second_of(Clock::Share, soft - Duration::from_millis(50))
                .worth_another_iteration(answered())
        );
        assert!(
            !a_second_of(Clock::Share, soft + Duration::from_millis(50))
                .worth_another_iteration(answered())
        );
    }

    #[test]
    fn the_line_falls_as_the_chosen_move_takes_more_of_the_depth() {
        // written out rather than computed, so moving a constant fails here
        let lines: Vec<u128> = [100, 500, 600, 700, 800, 900, 1000]
            .into_iter()
            .map(|chosen| took(chosen).soft_line_percent())
            .collect();
        assert_eq!(lines, vec![60, 60, 52, 45, 37, 30, 30]);
    }

    #[test]
    fn a_move_the_table_answered_leaves_the_line_where_it_was() {
        for chosen in [0, 1, 99] {
            assert_eq!(took(chosen).soft_line_percent(), SOFT_LIMIT_PERCENT);
        }
    }

    #[test]
    fn a_depth_that_changed_its_move_sets_the_highest_line() {
        for chosen in [0, 50, 700, 1000] {
            let changed = RootNodes {
                changed: true,
                ..took(chosen)
            };
            assert_eq!(changed.soft_line_percent(), 65);
        }
    }

    #[test]
    fn a_depth_with_no_root_nodes_keeps_the_line_where_it_was() {
        let empty = RootNodes {
            chosen: 0,
            total: 0,
            changed: false,
        };
        assert_eq!(empty.soft_line_percent(), SOFT_LIMIT_PERCENT);
    }

    #[test]
    fn the_root_moves_the_line_an_iteration_is_begun_under() {
        // each elapsed share sits at least five points from every line it
        // is read against, since the call reads the clock again
        let at = |percent: u64| a_second_of(Clock::Share, Duration::from_millis(percent * 10));
        // a settled root gives up where the line before it did not
        assert!(at(38).worth_another_iteration(answered()));
        assert!(!at(38).worth_another_iteration(Some(took(950))));
        // a contested root carries on where the line before it gave up
        assert!(!at(52).worth_another_iteration(answered()));
        assert!(at(52).worth_another_iteration(Some(took(400))));
        assert!(!at(52).worth_another_iteration(Some(took(50))));
        // and a changed move further still, short of the deadline
        let changed = RootNodes {
            changed: true,
            ..took(950)
        };
        assert!(at(60).worth_another_iteration(Some(changed)));
        assert!(!at(70).worth_another_iteration(Some(changed)));
    }

    #[test]
    fn a_depth_is_not_begun_when_twice_the_last_one_overruns_the_share() {
        // 300 ms gone of a second: a last depth of 300 ms leaves room for
        // 600 more, one of 400 ms does not. Each sits 100 ms from the
        // boundary, since the call reads the clock again
        let gone = Duration::from_millis(300);
        let share = a_second_of(Clock::Share, gone);
        assert!(share.can_pay_for_another_depth(Duration::from_millis(300)));
        assert!(!share.can_pay_for_another_depth(Duration::from_millis(400)));
    }

    #[test]
    fn a_named_move_time_and_a_node_budget_pay_for_every_depth() {
        let gone = Duration::from_millis(900);
        let took = Duration::from_millis(500);
        assert!(a_second_of(Clock::Fixed, gone).can_pay_for_another_depth(took));
        assert!(Limits::starting_at(Instant::now(), None, 1_000).can_pay_for_another_depth(took));
    }

    #[test]
    fn a_named_move_time_is_spent_to_its_deadline() {
        // an elapsed share that a clock budget would refuse is still begun
        let late = Duration::from_millis(990);
        assert!(a_second_of(Clock::Fixed, late).worth_another_iteration(answered()));
        assert!(!a_second_of(Clock::Share, late).worth_another_iteration(answered()));
    }

    #[test]
    fn a_search_on_no_clock_begins_every_iteration_it_is_asked_for() {
        assert!(
            Limits::starting_at(Instant::now(), None, 1_000).worth_another_iteration(answered()),
            "a node budget was cut short by the clock"
        );
        assert!(Limits::unlimited().worth_another_iteration(answered()));
    }

    #[test]
    fn the_first_iteration_is_begun_whatever_the_clock_says() {
        assert!(already_spent().worth_another_iteration(None));
        assert!(!already_spent().worth_another_iteration(answered()));
    }

    #[test]
    fn an_iteration_measures_the_clock_from_when_the_search_began() {
        let started = Instant::now() - Duration::from_secs(1);
        let limits = Limits::starting_at(
            started,
            Some(Clock::Share(Duration::from_secs(2))),
            u64::MAX,
        );
        // the second iteration does not start the two seconds again
        assert!(limits.for_iteration(true, 0).elapsed() >= Duration::from_secs(1));
    }

    #[test]
    fn a_search_with_no_node_limit_never_takes_one_from_the_deepening() {
        let limits = Limits::starting_now(Some(Clock::Share(Duration::from_secs(1))), None);
        assert_eq!(limits.node_budget(), u64::MAX);
        assert_eq!(limits.for_iteration(true, 5_000).node_budget(), u64::MAX);
    }
}
