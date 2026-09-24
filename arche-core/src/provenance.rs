// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! Which of the search's approximations a score leaned on. A diagnostic,
//! built only under the `provenance` feature, and read by nothing the
//! search decides.
//!
//! A score carries a mask of gates, one bit for each shortcut that can be
//! switched off. A node's mask is the gates it used itself, or'd with the
//! mask of the child whose score it returned: the best move's, or at a fail
//! high the cutting move's. So the root's mask names the gates along the
//! line that produced its score. The draw taint is accumulated the other
//! way, from every child a node looked at, and a mask accumulated like that
//! would name every gate that fired anywhere in the tree. So the root's mask
//! traces one line rather than everything its score depends on: a node that
//! failed low answers with its best child, and a gate that held a sibling
//! under it goes unnamed. The union is kept as well, once per search, as
//! `fired`: a gate it does not name changed nothing, and switching it off
//! leaves the tree as it was. That holds for a search from an empty table,
//! since an entry an earlier search stored carries that search's mask.
//!
//! Without the feature `Mask` has no field, so every operation on it is
//! empty, the table's reserved bytes are written with zero as before, and
//! the search compiles to what it was.

use std::fmt;
use std::ops::BitOr;

/// A shortcut a mask can name. Each is named by the search switch that
/// removes it, which is what a reading of the mask is tested against.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    ReverseFutility,
    NullMove,
    /// A pass reduced further than the flat reduction.
    AdaptiveNullMove,
    /// A quiet dropped by the late move count.
    LateMoveCount,
    /// A quiet dropped by the quiet futility margin and not by the count,
    /// which is asked first.
    QuietFutility,
    LateMovePruning,
    /// A reduced scout that failed low and was returned, or a late move
    /// pruned: `late_move_reductions` off removes both, since the pruning
    /// is decided only where the reduction is admitted.
    LateMoveReductions,
    DeltaMargin,
    /// A losing capture passed over in quiescence that the delta margin
    /// had not already passed over.
    SeePruning,
}

impl Gate {
    pub const ALL: [Gate; 9] = [
        Gate::ReverseFutility,
        Gate::NullMove,
        Gate::AdaptiveNullMove,
        Gate::LateMoveCount,
        Gate::QuietFutility,
        Gate::LateMovePruning,
        Gate::LateMoveReductions,
        Gate::DeltaMargin,
        Gate::SeePruning,
    ];

    /// The `SearchConfig` switch that removes the gate.
    pub fn word(self) -> &'static str {
        match self {
            Gate::ReverseFutility => "reverse_futility",
            Gate::NullMove => "null_move",
            Gate::AdaptiveNullMove => "adaptive_null_move",
            Gate::LateMoveCount => "late_move_count",
            Gate::QuietFutility => "quiet_futility",
            Gate::LateMovePruning => "late_move_pruning",
            Gate::LateMoveReductions => "late_move_reductions",
            Gate::DeltaMargin => "delta_margin",
            Gate::SeePruning => "see_pruning",
        }
    }
}

// a mask is kept in sixteen bits, in the table and out of it
const _: () = assert!(Gate::ALL.len() <= 16);

/// A set of gates, or nothing at all without the feature.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Mask(#[cfg(feature = "provenance")] u16);

#[cfg(feature = "provenance")]
impl Mask {
    /// The mask with one more gate.
    #[inline(always)]
    #[must_use]
    pub(crate) fn with(self, gate: Gate) -> Self {
        Mask(self.0 | 1 << gate as u16)
    }

    /// Whether the mask names the gate.
    pub fn names(self, gate: Gate) -> bool {
        self.0 & 1 << gate as u16 != 0
    }

    /// What the table keeps in its entry's two reserved bytes.
    #[inline(always)]
    pub(crate) fn stored(self) -> i16 {
        self.0 as i16
    }

    /// The mask the table kept.
    #[inline(always)]
    pub(crate) fn from_stored(bits: i16) -> Self {
        Mask(bits as u16)
    }
}

#[cfg(not(feature = "provenance"))]
impl Mask {
    #[inline(always)]
    #[must_use]
    pub(crate) fn with(self, _: Gate) -> Self {
        self
    }

    /// Never, without the feature.
    pub fn names(self, _: Gate) -> bool {
        false
    }

    #[inline(always)]
    pub(crate) fn stored(self) -> i16 {
        0
    }

    #[inline(always)]
    pub(crate) fn from_stored(_: i16) -> Self {
        Mask()
    }
}

impl Mask {
    /// Whether every gate this names, `other` names too.
    pub fn within(self, other: Mask) -> bool {
        Gate::ALL
            .iter()
            .all(|&gate| !self.names(gate) || other.names(gate))
    }
}

#[cfg(feature = "provenance")]
impl BitOr for Mask {
    type Output = Self;

    #[inline(always)]
    fn bitor(self, other: Self) -> Self {
        Mask(self.0 | other.0)
    }
}

#[cfg(not(feature = "provenance"))]
impl BitOr for Mask {
    type Output = Self;

    #[inline(always)]
    fn bitor(self, _: Self) -> Self {
        self
    }
}

/// The gates by their switches' names, joined by commas, or `none`.
impl fmt::Display for Mask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut named = Gate::ALL.iter().filter(|&&gate| self.names(gate));
        match named.next() {
            None => write!(f, "none"),
            Some(first) => {
                write!(f, "{}", first.word())?;
                named.try_for_each(|gate| write!(f, ",{}", gate.word()))
            }
        }
    }
}

/// What one search read: the mask of the score the root answered with, from
/// the last iteration that finished inside its window, and every gate that
/// fired anywhere in any iteration.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    pub chosen: Mask,
    pub fired: Mask,
}

#[cfg(test)]
mod tests {
    use super::{Gate, Mask};

    #[cfg(not(feature = "provenance"))]
    #[test]
    fn without_the_feature_a_mask_is_nothing() {
        assert_eq!(std::mem::size_of::<Mask>(), 0);
        assert!(!Mask::default().with(Gate::NullMove).names(Gate::NullMove));
        assert_eq!(Mask::default().with(Gate::NullMove).stored(), 0);
    }

    #[cfg(feature = "provenance")]
    #[test]
    fn a_mask_keeps_what_it_is_given_through_the_table() {
        let mask = Mask::default().with(Gate::NullMove).with(Gate::SeePruning);
        assert!(mask.names(Gate::NullMove) && mask.names(Gate::SeePruning));
        assert!(!mask.names(Gate::ReverseFutility));
        assert_eq!(Mask::from_stored(mask.stored()), mask);
        assert_eq!(mask.to_string(), "null_move,see_pruning");
        assert!(Mask::default().with(Gate::NullMove).within(mask));
        assert!(!mask.within(Mask::default().with(Gate::NullMove)));
    }

    /// The search's side of the contract, over the bench's positions at a
    /// depth every gate reaches: a root leans only on gates that fired, and
    /// a gate whose switch is off never fires.
    #[cfg(feature = "provenance")]
    mod search {
        use super::super::{Gate, Mask, Reading};
        use crate::engine::{AlphaBeta, SearchConfig};
        use crate::{Board, bench, effort};

        const DEPTH: u8 = 6;
        const TABLE_BYTES: usize = 1 << 20;

        fn readings(config: SearchConfig) -> Vec<Reading> {
            bench::positions()
                .iter()
                .map(|position| {
                    let board = Board::from_fen(&position.fen).expect("a bench fen parses");
                    let mut engine = AlphaBeta::with_config(board, TABLE_BYTES, config);
                    engine.search(DEPTH);
                    engine.provenance()
                })
                .collect()
        }

        #[test]
        fn a_root_leans_only_on_what_fired() {
            let readings = readings(SearchConfig::default());
            let mut fired = Mask::default();
            let mut chosen = Mask::default();
            for reading in &readings {
                assert!(
                    reading.chosen.within(reading.fired),
                    "chosen {} is not within fired {}",
                    reading.chosen,
                    reading.fired
                );
                fired = fired | reading.fired;
                chosen = chosen | reading.chosen;
            }
            // a mask that never names anything would pass the check above
            // and read nothing
            for gate in Gate::ALL {
                assert!(fired.names(gate), "{} never fired", gate.word());
            }
            assert_ne!(chosen, Mask::default(), "no root leaned on anything");
        }

        #[test]
        fn a_gate_switched_off_never_fires() {
            for gate in Gate::ALL {
                let config = effort::without(gate.word()).expect("a switch");
                for reading in readings(config) {
                    assert!(
                        !reading.fired.names(gate),
                        "{} fired with its switch off",
                        gate.word()
                    );
                }
            }
        }
    }

    #[test]
    fn every_gate_is_named_by_a_switch() {
        for gate in Gate::ALL {
            assert!(
                crate::effort::without(gate.word()).is_some(),
                "{} is not a switch",
                gate.word()
            );
        }
    }
}
