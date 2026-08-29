// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What the search's shortcuts cost in accuracy.
//!
//! A shortcut answers a node from something cheaper than searching it: a
//! margin above beta, or a pass searched shallow. Both are guesses, and the
//! question this module asks is how far off the guesses are. The residual is
//! the reference search's answer to the node less the score the shortcut
//! claimed for it, and a distribution of those, by kind and by depth, is what
//! a later change to either shortcut has to be argued from.
//!
//! The sampler is the recording half. It hangs off an engine as an option,
//! and an engine without one searches exactly the tree it searched before
//! there was a sampler at all, which is what the pinned bench counts say.
//! The replaying half lives beside it and runs after the search, never
//! during: see the module's `run`.

use crate::misc::Score;

/// The shortcut that answered a node.
///
/// Two of them today, which are the two the default configuration turns on.
/// A third would be one arm here and one call at wherever it returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    /// The node stood far enough above beta by its static evaluation alone.
    ReverseFutility,
    /// A reduced search of the position left by passing came back above beta.
    NullMove,
}

impl Shortcut {
    /// The kinds, for a reader summarising by them.
    pub const KINDS: [Shortcut; 2] = [Shortcut::ReverseFutility, Shortcut::NullMove];

    /// The word a row prints, which names the `SearchConfig` switch that
    /// turns the shortcut on.
    pub fn word(self) -> &'static str {
        match self {
            Shortcut::ReverseFutility => "reverse_futility",
            Shortcut::NullMove => "null_move",
        }
    }
}

/// One node a shortcut answered, with enough of the node to search it again.
///
/// Everything is owned. A sample outlives the search that took it, and the
/// board it was taken from has moved on by then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    /// The position, as the board prints one. The fifty move counter travels
    /// in it; the path does not, and see `run` for what that costs.
    pub fen: String,
    /// The depth the node had left to search, which is the depth the
    /// shortcut was trusted over and so the depth the replay searches to.
    pub depth: u8,
    pub kind: Shortcut,
    /// The score the shortcut returned for the node.
    pub claimed: Score,
    /// How far the static evaluation stood above beta when the shortcut
    /// fired, which is the margin each of them is really betting on. Widened
    /// past a `Score` because the difference of two of them is not one.
    pub eval_beta: i32,
}

/// What a sampler collected, taken away from it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sampled {
    pub taken: Vec<Sample>,
    /// Samples the buffer had no room for. Counted rather than kept, so a
    /// long run says how much of itself it is not describing.
    pub overflowed: u64,
}

/// Records every nth node a shortcut answers.
///
/// Deterministic: a countdown, not a draw, so two runs of the same search
/// record the same nodes and a distribution can be reproduced from the
/// command that printed it.
#[derive(Clone, Debug)]
pub struct Sampler {
    /// One sample every this many events. Held at one or more, so a rate of
    /// zero records everything rather than dividing by nothing.
    every: u32,
    /// Events still to go before the next sample.
    countdown: u32,
    /// The most samples the buffer will hold. A cap rather than a growing
    /// vector: a run at a low rate over a deep search would otherwise ask
    /// for gigabytes of fens.
    cap: usize,
    taken: Vec<Sample>,
    overflowed: u64,
}

impl Sampler {
    /// What a sampler holds when nothing says otherwise. Ten thousand fens
    /// is a megabyte or so, and a run wanting more of the tree than that
    /// should lower its rate rather than raise this.
    pub const DEFAULT_CAP: usize = 10_000;

    /// Records one node in every `every`.
    pub fn every(every: u32) -> Self {
        Self::with_cap(every, Self::DEFAULT_CAP)
    }

    /// The same, holding at most `cap` samples. One sampler is meant to be
    /// carried across a whole run of searches, so the cap it is built with
    /// bounds the run rather than any one search in it.
    pub fn with_cap(every: u32, cap: usize) -> Self {
        let every = every.max(1);
        Self {
            every,
            countdown: every,
            cap,
            taken: Vec::new(),
            overflowed: 0,
        }
    }

    /// Count one event, and on the nth of them ask for the sample.
    ///
    /// The sample arrives as a closure because building one prints a fen and
    /// evaluates a position, and neither is worth doing on the events that
    /// are not kept.
    pub fn event(&mut self, describe: impl FnOnce() -> Sample) {
        self.countdown -= 1;
        if self.countdown > 0 {
            return;
        }
        self.countdown = self.every;
        if self.taken.len() >= self.cap {
            self.overflowed += 1;
            return;
        }
        self.taken.push(describe());
    }

    /// How many samples are held.
    pub fn len(&self) -> usize {
        self.taken.len()
    }

    pub fn is_empty(&self) -> bool {
        self.taken.is_empty()
    }

    /// Everything collected, leaving the sampler empty and ready to record
    /// again. The overflow count goes with it: it describes the samples
    /// handed over and not the sampler.
    pub fn drain(&mut self) -> Sampled {
        Sampled {
            taken: std::mem::take(&mut self.taken),
            overflowed: std::mem::replace(&mut self.overflowed, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(fen: &str) -> Sample {
        Sample {
            fen: fen.to_string(),
            depth: 3,
            kind: Shortcut::NullMove,
            claimed: 12,
            eval_beta: 40,
        }
    }

    #[test]
    fn one_event_in_every_n_is_kept() {
        let mut sampler = Sampler::every(3);
        for event in 0..9 {
            sampler.event(|| sample(&event.to_string()));
        }
        let sampled = sampler.drain();
        let fens: Vec<&str> = sampled.taken.iter().map(|s| s.fen.as_str()).collect();
        assert_eq!(fens, vec!["2", "5", "8"]);
        assert_eq!(sampled.overflowed, 0);
    }

    #[test]
    fn a_rate_of_zero_keeps_every_event() {
        let mut sampler = Sampler::every(0);
        for event in 0..3 {
            sampler.event(|| sample(&event.to_string()));
        }
        assert_eq!(sampler.len(), 3);
    }

    #[test]
    fn a_full_buffer_stops_recording_and_counts_the_rest() {
        let mut sampler = Sampler::with_cap(1, 2);
        for event in 0..7 {
            sampler.event(|| sample(&event.to_string()));
        }
        let sampled = sampler.drain();
        assert_eq!(sampled.taken.len(), 2);
        assert_eq!(sampled.overflowed, 5);
    }

    #[test]
    fn draining_leaves_the_sampler_ready_to_record_again() {
        let mut sampler = Sampler::with_cap(1, 1);
        sampler.event(|| sample("first"));
        sampler.event(|| sample("dropped"));
        let first = sampler.drain();
        assert_eq!(first.taken.len(), 1);
        assert_eq!(first.overflowed, 1);
        assert!(sampler.is_empty());
        sampler.event(|| sample("second"));
        let second = sampler.drain();
        assert_eq!(second.taken[0].fen, "second");
        assert_eq!(second.overflowed, 0);
    }

    #[test]
    fn the_kinds_print_the_switches_that_turn_them_on() {
        assert_eq!(Shortcut::ReverseFutility.word(), "reverse_futility");
        assert_eq!(Shortcut::NullMove.word(), "null_move");
    }
}
