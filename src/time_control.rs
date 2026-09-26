// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use crate::params::Params;
use arche_core::Clock;
use arche_core::Color;
use std::time::Duration;

/// The `Move Overhead` option's default in milliseconds, held back from every
/// budget. Fifty covers a local interface; a network costs more.
pub const DEFAULT_MOVE_OVERHEAD_MS: u64 = 50;

/// Moves we plan for when the time control does not say how many are left.
///
/// With an increment the clock falls until a move's spend equals the
/// increment and then holds, so the horizon decides how much clock is unspent
/// at the end rather than how fast it runs out. At a fortieth, two self play
/// games at 10+0.1 ended with about half the clock banked; a twentieth halves
/// the floor, to about 1.9 seconds (7913356).
const ASSUMED_MOVES_TO_GO: u64 = 20;

/// Share of the increment we count on. It is credited only after the move,
/// so spending all of it leaves nothing to cover the overhead.
const INCREMENT_PERCENT: u64 = 75;

/// The most of the remaining clock a single move may take, which keeps a
/// large increment from spending time we have not been given yet.
const MAX_CLOCK_PERCENT: u64 = 33;

/// Searched even on a spent clock, so that a legal move comes back.
const MIN_BUDGET_MS: u64 = 1;

/// The time part of a `go` command for the side to move, in milliseconds.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TimeControl {
    pub time: Option<u64>,
    pub increment: Option<u64>,
    pub moves_to_go: Option<u64>,
    pub move_time: Option<u64>,
    pub infinite: bool,
}

impl TimeControl {
    /// A clock or a move time that was sent but cannot be read, or has
    /// nothing after it, reads as spent: read as absent, a `go` with no time
    /// searches without a limit. A weak move is recoverable and thinking for
    /// ever is not. A missing count of moves is already a number assumed.
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

    /// How long to search for, or `None` for no time limit, less the
    /// overhead. A move time is `Fixed`; a share worked out from the clock is
    /// a guess the search may spend less of.
    pub fn budget(&self, overhead: u64) -> Option<Clock> {
        if self.infinite {
            return None;
        }
        let spend = match (self.move_time, self.time, self.increment) {
            (Some(move_time), _, _) => move_time,
            (None, Some(time), increment) => self.clock_share(time, increment.unwrap_or(0)),
            // some interfaces send an increment with no clock
            (None, None, Some(increment)) => percent(increment, INCREMENT_PERCENT),
            (None, None, None) => return None,
        };
        let budget = Duration::from_millis(spend.saturating_sub(overhead).max(MIN_BUDGET_MS));
        Some(match self.move_time {
            Some(_) => Clock::Fixed(budget),
            None => Clock::Share(budget),
        })
    }

    fn clock_share(&self, time: u64, increment: u64) -> u64 {
        // the protocol does not define a count of zero, so it is no answer
        // rather than "this is the last move"
        let moves_to_go = self
            .moves_to_go
            .filter(|&moves| moves > 0)
            .unwrap_or(ASSUMED_MOVES_TO_GO);
        let share = (time / moves_to_go).saturating_add(percent(increment, INCREMENT_PERCENT));
        share.min(percent(time, MAX_CLOCK_PERCENT))
    }
}

/// In `u128`, so a huge clock does not wrap to a tiny share.
fn percent(value: u64, percent: u64) -> u64 {
    debug_assert!(percent <= 100);
    (value as u128 * percent as u128 / 100) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn millis_at(control: &TimeControl, overhead: u64) -> Option<u64> {
        control
            .budget(overhead)
            .map(|clock| clock.deadline().as_millis() as u64)
    }

    fn millis(control: &TimeControl) -> Option<u64> {
        millis_at(control, DEFAULT_MOVE_OVERHEAD_MS)
    }

    fn clock(time: u64) -> TimeControl {
        TimeControl {
            time: Some(time),
            ..Default::default()
        }
    }

    // Expected values are written out rather than computed from the
    // constants, so that changing a constant fails its test.

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
        // the deepening may answer early only on a share
        assert_eq!(
            TimeControl {
                move_time: Some(500),
                ..Default::default()
            }
            .budget(DEFAULT_MOVE_OVERHEAD_MS),
            Some(Clock::Fixed(Duration::from_millis(450)))
        );
        assert_eq!(
            clock(60_000).budget(DEFAULT_MOVE_OVERHEAD_MS),
            Some(Clock::Share(Duration::from_millis(2_950)))
        );
        assert_eq!(
            TimeControl {
                increment: Some(1_000),
                ..Default::default()
            }
            .budget(DEFAULT_MOVE_OVERHEAD_MS),
            Some(Clock::Share(Duration::from_millis(700)))
        );
    }

    #[test]
    fn the_overhead_is_whatever_the_option_was_set_to() {
        assert_eq!(millis_at(&clock(60_000), 0), Some(3_000));
        assert_eq!(millis_at(&clock(60_000), 500), Some(2_500));
        let move_time = TimeControl {
            move_time: Some(500),
            ..Default::default()
        };
        assert_eq!(millis_at(&move_time, 0), Some(500));
        // an overhead larger than the budget still leaves a move to play
        assert_eq!(millis_at(&move_time, 5_000), Some(MIN_BUDGET_MS));
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
    fn sudden_death_plans_for_twenty_more_moves() {
        // 60000 / 20 - 50
        assert_eq!(millis(&clock(60_000)), Some(2_950));
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
        // the cap decides: 33% - 50
        assert_eq!(millis(&control), Some(19_750));
    }

    #[test]
    fn only_part_of_the_increment_is_banked() {
        let control = TimeControl {
            increment: Some(1_000),
            ..clock(60_000)
        };
        // 60000 / 20 + 750 of the increment - 50
        assert_eq!(millis(&control), Some(3_700));
    }

    #[test]
    fn an_increment_never_spends_more_than_the_clock() {
        // the increment dwarfs what is left
        for time in [50, 100, 500, 1_000, 5_000] {
            let control = TimeControl {
                increment: Some(10_000),
                ..clock(time)
            };
            let budget = millis(&control).unwrap();
            assert!(budget < time, "spent {} of {} left", budget, time);
        }
        // near the floor 7913356 measured at 10+0.1: 1900 / 20 + 75 - 50
        let control = TimeControl {
            increment: Some(100),
            ..clock(1_900)
        };
        assert_eq!(millis(&control), Some(120));
    }

    #[test]
    fn the_cap_decides_only_on_a_nearly_spent_clock() {
        // a twentieth plus three quarters of the increment passes 33% of the
        // clock at about 268 ms: the cap decides below that
        let nearly_spent = TimeControl {
            increment: Some(100),
            ..clock(200)
        };
        // 33% of 200 is 66, which is less than 200 / 20 + 75
        assert_eq!(millis(&nearly_spent), Some(16));
        let a_little_more = TimeControl {
            increment: Some(100),
            ..clock(300)
        };
        // 300 / 20 + 75 is 90, which is under the 99 the cap allows
        assert_eq!(millis(&a_little_more), Some(40));
    }

    #[test]
    fn an_increment_with_no_clock_is_partly_spent() {
        let control = TimeControl {
            increment: Some(1_000),
            ..Default::default()
        };
        // 750 of the increment - 50
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
