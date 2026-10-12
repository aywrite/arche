// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use crate::params::Params;
use arche_core::Clock;
use arche_core::Color;
use std::time::Duration;

/// The `Move Overhead` option's default in milliseconds, held back from every
/// budget. Fifty covers a local interface; a network costs more.
pub const DEFAULT_MOVE_OVERHEAD_MS: u64 = 50;

/// The moves a side is taken to have left when the time control does not
/// say: `OPENING_MOVES_LEFT` at the first move searched in a game, falling
/// in a straight line to `LATE_MOVES_LEFT` at `LATE_MOVE` and held there.
/// An assumption, not an estimate of this game: over 1,000 games at 10+0.1
/// a side had 67 moves left on average at its first searched move (median
/// 63) and 29 at move 60 (median 21). A fitted estimate from material and
/// score placed the same time no better (see docs/ROADMAP.md).
const OPENING_MOVES_LEFT: u64 = 60;
const LATE_MOVES_LEFT: u64 = 30;

/// The move number by which the moves left have fallen to `LATE_MOVES_LEFT`.
/// A game first searched at or past it falls there on the move after.
const LATE_MOVE: u64 = 60;

/// The clock is planned over this fraction of the moves left, a third, so
/// the horizon is 20 at the first searched move and 10 from move 60.
/// Planning over all of them would bank most of the clock: the search
/// stops short of its share on most moves, and matches favoured spending
/// faster than an even spread.
///
/// With an increment the clock falls until a move's spend equals the
/// increment and then holds, so the horizon decides how much clock is unspent
/// at the end rather than how fast it runs out. At a fortieth, two self play
/// games at 10+0.1 ended with about half the clock banked; a twentieth halves
/// the floor, to about 1.9 seconds (7913356). At a twentieth throughout,
/// games at 10+0.1 still ended with about 3.5 seconds of the 10 left, so the
/// horizon falls as the game goes on and spends that on the middle and late
/// game: a re-run of 500 games' clocks with it ends them 1.27 seconds lower,
/// with 1.10 of the time and no clock under 750 ms.
const PLANNED_FRACTION_OF_MOVES_LEFT: (u64, u64) = (1, 3);

/// Share of the increment we count on. It is credited only after the move,
/// so spending all of it leaves nothing to cover the overhead.
const INCREMENT_PERCENT: u64 = 75;

/// The most of the remaining clock a single move may take, which keeps a
/// large increment from spending time we have not been given yet.
const MAX_CLOCK_PERCENT: u64 = 33;

/// The part of the share above that a move on a game clock plans for, as a
/// percentage, taken before the overhead. Below the whole of it because a
/// depth under way at the share runs on to the limit: replayed over 100
/// games at a node budget, the search spent 0.71 of each share stopping
/// there and 0.91 running on, and at 81% the replayed clocks end where they
/// did. With the overhead taken after, a share at 10+0.1 is about 0.79 of
/// what it was.
const SHARE_PERCENT: u64 = 81;

/// The limit a depth begun inside the share may run on to: this many
/// shares, and no more than `LIMIT_CLOCK_PERCENT` of the clock. Only a depth
/// already under way uses it, since the soft line still reads the share.
const LIMIT_SHARES: u32 = 3;
const LIMIT_CLOCK_PERCENT: u64 = 25;

/// Searched even on a spent clock, so that a legal move comes back.
const MIN_BUDGET_MS: u64 = 1;

/// Where a game stands for the horizon: the move number of the first
/// position searched on a clock in it, and of this one.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GameMoves {
    pub first: u64,
    pub now: u64,
}

impl GameMoves {
    /// The clock over the horizon: `PLANNED_FRACTION_OF_MOVES_LEFT` of the
    /// moves taken to be left, which fall from `OPENING_MOVES_LEFT` at the
    /// first move to `LATE_MOVES_LEFT` at `LATE_MOVE` and stay there.
    /// The moves left times the span are kept whole, so the share is exact.
    fn share_of(&self, time: u64) -> u64 {
        let span = LATE_MOVE.saturating_sub(self.first).max(1);
        let gone = self.now.saturating_sub(self.first);
        let moves_left_by_span = (OPENING_MOVES_LEFT * span)
            .saturating_sub((OPENING_MOVES_LEFT - LATE_MOVES_LEFT).saturating_mul(gone))
            .max(LATE_MOVES_LEFT * span);
        let (planned, of) = PLANNED_FRACTION_OF_MOVES_LEFT;
        (time as u128 * span as u128 * of as u128 / (moves_left_by_span as u128 * planned as u128))
            as u64
    }
}

/// The time part of a `go` command for the side to move, in milliseconds.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TimeControl {
    pub time: Option<u64>,
    pub increment: Option<u64>,
    pub moves_to_go: Option<u64>,
    pub move_time: Option<u64>,
    pub infinite: bool,
    /// Not on the line: the interface sets it from the game.
    pub game: GameMoves,
}

impl TimeControl {
    /// A `go` whose clock cannot be read is refused before it gets here, so
    /// every value is read or absent. A missing count of moves is a number
    /// assumed.
    pub fn of(params: &Params, color: Color) -> Self {
        let (clock, increment) = match color {
            Color::White => ("wtime", "winc"),
            Color::Black => ("btime", "binc"),
        };
        TimeControl {
            time: params.count(clock).read(),
            increment: params.count(increment).read(),
            moves_to_go: params.count("movestogo").read(),
            move_time: params.count("movetime").read(),
            infinite: params.flag("infinite"),
            game: GameMoves::default(),
        }
    }

    /// How long to search for, or `None` for no time limit, less the
    /// overhead. A move time is `Fixed`; a share worked out from the clock is
    /// a guess the search may spend less of, with a limit past it that a
    /// depth under way may run on to.
    pub fn budget(&self, overhead: u64) -> Option<Clock> {
        if self.infinite {
            return None;
        }
        let less_overhead =
            |millis: u64| Duration::from_millis(millis.saturating_sub(overhead).max(MIN_BUDGET_MS));
        match (self.move_time, self.time, self.increment) {
            (Some(move_time), _, _) => Some(Clock::Fixed(less_overhead(move_time))),
            (None, Some(time), increment) => {
                let share = less_overhead(percent(
                    self.clock_share(time, increment.unwrap_or(0)),
                    SHARE_PERCENT,
                ));
                let limit = (share * LIMIT_SHARES)
                    .min(less_overhead(percent(time, LIMIT_CLOCK_PERCENT)))
                    .max(share);
                Some(Clock::Share { share, limit })
            }
            // some interfaces send an increment with no clock, and there is
            // no clock to run on into
            (None, None, Some(increment)) => Some(Clock::share(less_overhead(percent(
                increment,
                INCREMENT_PERCENT,
            )))),
            (None, None, None) => None,
        }
    }

    fn clock_share(&self, time: u64, increment: u64) -> u64 {
        // the protocol does not define a count of zero, so it is no answer
        // rather than "this is the last move"
        let clock_part = match self.moves_to_go.filter(|&moves| moves > 0) {
            Some(moves) => time / moves,
            None => self.game.share_of(time),
        };
        let share = clock_part.saturating_add(percent(increment, INCREMENT_PERCENT));
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
            .map(|clock| clock.budget().as_millis() as u64)
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
            Some(Clock::Share {
                share: Duration::from_millis(2_380),
                limit: Duration::from_millis(7_140),
            })
        );
        assert_eq!(
            TimeControl {
                increment: Some(1_000),
                ..Default::default()
            }
            .budget(DEFAULT_MOVE_OVERHEAD_MS),
            Some(Clock::share(Duration::from_millis(700)))
        );
    }

    #[test]
    fn the_overhead_is_whatever_the_option_was_set_to() {
        assert_eq!(millis_at(&clock(60_000), 0), Some(2_430));
        assert_eq!(millis_at(&clock(60_000), 500), Some(1_930));
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
    fn sudden_death_plans_for_twenty_more_moves_at_the_first_move() {
        // 81% of 60000 / 20, less 50
        assert_eq!(millis(&clock(60_000)), Some(2_380));
    }

    fn at_move(first: u64, now: u64) -> TimeControl {
        TimeControl {
            game: GameMoves { first, now },
            ..clock(60_000)
        }
    }

    #[test]
    fn the_horizon_falls_from_twenty_to_ten_by_move_sixty() {
        // the first move searched, at 8moves' exit: 81% of 60000 / 20, less 50
        assert_eq!(millis(&at_move(9, 9)), Some(2_380));
        // 25 of 51 moves on, the horizon is 20 - 250/51: 81% of
        // 60000 * 51 / 770, less 50
        assert_eq!(millis(&at_move(9, 34)), Some(3_168));
        // 81% of 60000 / 10, less 50, and no further
        assert_eq!(millis(&at_move(9, 60)), Some(4_810));
        assert_eq!(millis(&at_move(9, 120)), Some(4_810));
    }

    #[test]
    fn a_first_move_at_or_past_sixty_falls_to_ten_a_move_later() {
        assert_eq!(millis(&at_move(70, 70)), Some(2_380));
        assert_eq!(millis(&at_move(70, 71)), Some(4_810));
    }

    #[test]
    fn moves_to_go_is_read_whatever_the_move() {
        let control = TimeControl {
            moves_to_go: Some(10),
            ..at_move(9, 9)
        };
        assert_eq!(millis(&control), Some(4_810));
    }

    #[test]
    fn moves_to_go_divides_the_clock() {
        let control = TimeControl {
            moves_to_go: Some(10),
            ..clock(60_000)
        };
        // 81% of 60000 / 10, less 50
        assert_eq!(millis(&control), Some(4_810));
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
        // the cap decides: 81% of 33%, less 50
        assert_eq!(millis(&control), Some(15_988));
    }

    #[test]
    fn only_part_of_the_increment_is_banked() {
        let control = TimeControl {
            increment: Some(1_000),
            ..clock(60_000)
        };
        // 81% of 60000 / 20 + 750 of the increment, less 50
        assert_eq!(millis(&control), Some(2_987));
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
        // near the floor 7913356 measured at 10+0.1: 81% of 1900 / 20 + 75,
        // less 50
        let control = TimeControl {
            increment: Some(100),
            ..clock(1_900)
        };
        assert_eq!(millis(&control), Some(87));
    }

    #[test]
    fn the_cap_decides_only_on_a_nearly_spent_clock() {
        // a twentieth plus three quarters of the increment passes 33% of the
        // clock at about 268 ms: the cap decides below that
        let nearly_spent = TimeControl {
            increment: Some(100),
            ..clock(200)
        };
        // 33% of 200 is 66, which is less than 200 / 20 + 75; 81% of it
        assert_eq!(millis(&nearly_spent), Some(3));
        let a_little_more = TimeControl {
            increment: Some(100),
            ..clock(300)
        };
        // 300 / 20 + 75 is 90, which is under the 99 the cap allows
        assert_eq!(millis(&a_little_more), Some(22));
    }

    fn limit_millis(control: &TimeControl) -> Option<u64> {
        control
            .budget(DEFAULT_MOVE_OVERHEAD_MS)
            .map(|clock| clock.deadline().as_millis() as u64)
    }

    #[test]
    fn a_depth_under_way_may_run_on_to_three_shares() {
        assert_eq!(limit_millis(&clock(60_000)), Some(3 * 2_380));
    }

    #[test]
    fn the_limit_takes_no_more_than_a_quarter_of_the_clock() {
        // a share of 81% of 60000 / 5, less 50, is 9670; three of them
        // would pass a quarter of the clock, less 50
        let control = TimeControl {
            moves_to_go: Some(5),
            ..clock(60_000)
        };
        assert_eq!(millis(&control), Some(9_670));
        assert_eq!(limit_millis(&control), Some(14_950));
    }

    #[test]
    fn the_limit_is_never_below_the_share() {
        // the share's own cap of a third is above a quarter
        let control = TimeControl {
            moves_to_go: Some(1),
            ..clock(60_000)
        };
        assert_eq!(limit_millis(&control), millis(&control));
    }

    #[test]
    fn a_move_time_and_a_bare_increment_have_nothing_past_the_budget() {
        let move_time = TimeControl {
            move_time: Some(500),
            ..clock(60_000)
        };
        assert_eq!(limit_millis(&move_time), Some(450));
        let increment = TimeControl {
            increment: Some(1_000),
            ..Default::default()
        };
        assert_eq!(limit_millis(&increment), Some(700));
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
