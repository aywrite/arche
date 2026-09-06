// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use crate::params::Params;
use arche_core::Clock;
use arche_core::Color;
use std::time::Duration;

/// Held back from every budget, so that we are not still thinking when the
/// clock we were given has already run out.
const MOVE_OVERHEAD_MS: u64 = 50;

/// Moves we plan for when the time control does not say how many are left.
const ASSUMED_MOVES_TO_GO: u64 = 40;

/// Share of the increment we count on. It is only credited once we have moved,
/// so banking all of it leaves nothing to cover the overhead.
const INCREMENT_PERCENT: u64 = 75;

/// The most of the remaining clock a single move may ever take. This is what
/// keeps a large increment from spending time we have not been given yet.
const MAX_CLOCK_PERCENT: u64 = 33;

/// Searched even when the clock says there is nothing left, so that we return a
/// legal move rather than nothing at all.
const MIN_BUDGET_MS: u64 = 1;

/// The time part of a `go` command, from the point of view of the side to move.
/// All values are milliseconds.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TimeControl {
    pub time: Option<u64>,
    pub increment: Option<u64>,
    pub moves_to_go: Option<u64>,
    pub move_time: Option<u64>,
    pub infinite: bool,
}

impl TimeControl {
    /// The time part of a `go` command, from the point of view of the side to
    /// move.
    ///
    /// A clock or a move time that was sent but cannot be read stands in as spent
    /// rather than being discarded, because discarding it would read as the
    /// keyword having been absent, and a `go` with no time at all searches without
    /// a limit. That trades one bad outcome for a smaller one: an unreadable move
    /// time beside a good clock spends the clock rather than reading it, and moves
    /// almost at once. Playing a weak move is recoverable and thinking for ever is
    /// not. A count of moves is left alone instead: it only divides the clock, and
    /// the time control already treats a missing one as a number to assume.
    pub fn of(params: &Params, color: Color) -> Self {
        let (clock, increment) = match color {
            Color::White => ("wtime", "winc"),
            Color::Black => ("btime", "binc"),
        };
        TimeControl {
            time: params.count(clock).read_or(0),
            increment: params.count(increment).read_or(0),
            moves_to_go: params.count("movestogo").read(),
            move_time: params.count("movetime").read_or(0),
            infinite: params.flag("infinite"),
        }
    }

    /// How long to search for, or `None` to search without a time limit.
    ///
    /// Which kind of clock it is goes with it: a move time is a time the
    /// interface named, and everything else here is a share this side worked
    /// out for itself from a clock that keeps running. Only the second is a
    /// guess the search may spend less of, which is what `Clock` is for.
    pub fn budget(&self) -> Option<Clock> {
        if self.infinite {
            return None;
        }
        let spend = match (self.move_time, self.time, self.increment) {
            (Some(move_time), _, _) => move_time,
            (None, Some(time), increment) => self.clock_share(time, increment.unwrap_or(0)),
            // Some interfaces send an increment with no clock. Playing the move
            // earns it back, so spending it roughly breaks even.
            (None, None, Some(increment)) => percent(increment, INCREMENT_PERCENT),
            (None, None, None) => return None,
        };
        let budget =
            Duration::from_millis(spend.saturating_sub(MOVE_OVERHEAD_MS).max(MIN_BUDGET_MS));
        Some(match self.move_time {
            Some(_) => Clock::Fixed(budget),
            None => Clock::Share(budget),
        })
    }

    fn clock_share(&self, time: u64, increment: u64) -> u64 {
        // A count of zero is not something the protocol defines, so treat it as
        // no answer rather than as "this is the last move", which would be the
        // most spendthrift reading available.
        let moves_to_go = self
            .moves_to_go
            .filter(|&moves| moves > 0)
            .unwrap_or(ASSUMED_MOVES_TO_GO);
        let share = (time / moves_to_go).saturating_add(percent(increment, INCREMENT_PERCENT));
        share.min(percent(time, MAX_CLOCK_PERCENT))
    }
}

/// Kept in `u128` so that a clock large enough to overflow the multiplication
/// does not wrap round to a share far smaller than the one asked for.
fn percent(value: u64, percent: u64) -> u64 {
    debug_assert!(percent <= 100);
    (value as u128 * percent as u128 / 100) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn millis(control: &TimeControl) -> Option<u64> {
        control
            .budget()
            .map(|clock| clock.deadline().as_millis() as u64)
    }

    fn clock(time: u64) -> TimeControl {
        TimeControl {
            time: Some(time),
            ..Default::default()
        }
    }

    // The expected values below are written out rather than recomputed from the
    // constants, so that changing a constant fails the test that covers it.

    #[test]
    fn move_time_is_spent_less_the_overhead() {
        let control = TimeControl {
            move_time: Some(500),
            ..Default::default()
        };
        assert_eq!(millis(&control), Some(450));
    }

    #[test]
    fn move_time_ignores_the_clock_and_the_increment() {
        let control = TimeControl {
            time: Some(60_000),
            increment: Some(1_000),
            move_time: Some(500),
            ..Default::default()
        };
        assert_eq!(millis(&control), Some(450));
    }

    #[test]
    fn a_move_time_is_named_and_everything_else_is_a_share() {
        // what the deepening loop reads to decide whether it may answer
        // before the budget is spent: only a share of a running clock is
        // this side's own guess, and only a guess is worth stopping short of
        assert_eq!(
            TimeControl {
                move_time: Some(500),
                ..Default::default()
            }
            .budget(),
            Some(Clock::Fixed(Duration::from_millis(450)))
        );
        assert_eq!(
            clock(60_000).budget(),
            Some(Clock::Share(Duration::from_millis(1_450)))
        );
        assert_eq!(
            TimeControl {
                increment: Some(1_000),
                ..Default::default()
            }
            .budget(),
            Some(Clock::Share(Duration::from_millis(700)))
        );
    }

    #[test]
    fn infinite_beats_move_time() {
        let control = TimeControl {
            move_time: Some(500),
            infinite: true,
            ..Default::default()
        };
        assert_eq!(millis(&control), None);
    }

    #[test]
    fn infinite_beats_the_clock() {
        let control = TimeControl {
            time: Some(60_000),
            infinite: true,
            ..Default::default()
        };
        assert_eq!(millis(&control), None);
    }

    #[test]
    fn no_time_information_has_no_budget() {
        assert_eq!(millis(&TimeControl::default()), None);
    }

    #[test]
    fn sudden_death_plans_for_forty_more_moves() {
        // 60000 / 40 - 50
        assert_eq!(millis(&clock(60_000)), Some(1_450));
    }

    #[test]
    fn moves_to_go_divides_the_clock() {
        let control = TimeControl {
            moves_to_go: Some(10),
            ..clock(60_000)
        };
        // 60000 / 10 - 50
        assert_eq!(millis(&control), Some(5_950));
    }

    #[test]
    fn moves_to_go_of_zero_is_treated_as_no_answer() {
        let control = TimeControl {
            moves_to_go: Some(0),
            ..clock(60_000)
        };
        assert_eq!(millis(&control), millis(&clock(60_000)));
    }

    #[test]
    fn the_last_move_before_the_control_is_still_capped() {
        let control = TimeControl {
            moves_to_go: Some(1),
            ..clock(60_000)
        };
        // the whole clock is available, so the cap is what decides: 33% - 50
        assert_eq!(millis(&control), Some(19_750));
    }

    #[test]
    fn only_part_of_the_increment_is_banked() {
        let control = TimeControl {
            increment: Some(1_000),
            ..clock(60_000)
        };
        // 60000 / 40 + 750 of the increment - 50
        assert_eq!(millis(&control), Some(2_200));
    }

    #[test]
    fn an_increment_never_spends_more_than_the_clock() {
        // The increment dwarfs what is left, which is normal for an increment
        // only control such as 0+1.
        for time in [50, 100, 500, 1_000, 5_000] {
            let control = TimeControl {
                increment: Some(10_000),
                ..clock(time)
            };
            let budget = millis(&control).unwrap();
            assert!(budget < time, "spent {} of {} left", budget, time);
        }
    }

    #[test]
    fn an_increment_with_no_clock_is_partly_spent() {
        let control = TimeControl {
            increment: Some(1_000),
            ..Default::default()
        };
        // 750 of the increment - 50, the same share as when there is a clock
        assert_eq!(millis(&control), Some(700));
    }

    #[test]
    fn a_spent_clock_still_leaves_time_to_move() {
        for control in [
            clock(0),
            clock(1),
            TimeControl {
                increment: Some(0),
                ..clock(40)
            },
            TimeControl {
                move_time: Some(1),
                ..Default::default()
            },
            TimeControl {
                increment: Some(1),
                ..Default::default()
            },
        ] {
            assert_eq!(millis(&control), Some(MIN_BUDGET_MS), "{:?}", control);
        }
    }

    #[test]
    fn a_clock_too_large_to_multiply_is_still_capped() {
        let control = TimeControl {
            increment: Some(u64::MAX),
            moves_to_go: Some(0),
            ..clock(u64::MAX)
        };
        let budget = millis(&control).unwrap();
        assert!(budget < u64::MAX / 2, "spent {} of the clock", budget);
    }
}
