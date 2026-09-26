// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What the search counts of its use of the transposition table. The table
//! reports what a probe found and whether a store landed, and the search
//! counts it here, beside the stores its taint policy declined, which the
//! table never sees.

use crate::transposition::Probe;
use crate::value::Value;

/// How often the table hands back a score that depended on the path taken
/// rather than on the position: the graph history interaction error. The
/// figures cover the whole search, quiescence included, since quiescence
/// stores real bounds and takes real cutoffs.
///
/// The search owns them, not the table, so they run over the engine's life
/// and survive a table replaced by `setoption Hash`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct GhiCounters {
    /// Entries which landed carrying a draw tainted score.
    pub tainted_stores: u64,
    /// Entries which landed and whose score a probe could later cut on. A
    /// store that loses the replacement contest is not one.
    pub stores: u64,
    /// Probes that returned a score, cutting the search off.
    pub score_cutoffs: u64,
    /// Probes that returned a tainted score, which is the error itself: the
    /// stored draw was reachable by the path that stored it and may not be
    /// reachable by this one. Zero while the search refuses them.
    pub tainted_score_cutoffs: u64,
    /// Tainted results the search declined to offer the table under a
    /// policy that keeps only clean scores, counted before the replacement
    /// contest: some would have lost it anyway, so this is the policy's
    /// reach rather than exactly what the table went without.
    pub skipped_stores: u64,
    /// Probes that found a tainted score deep enough to cut and refused it,
    /// handing back the move alone. Counted in the branch where a trusting
    /// search takes its cutoff, so the two are the same event under each
    /// policy, though not the same count: the refusing search is the larger
    /// tree. Zero while the search trusts them; under the rule50 policy
    /// this counts its horizon refusals instead, tainted or not.
    pub refused_cutoffs: u64,
}

impl GhiCounters {
    /// What a probe found: a score cutoff and its taint, or a refused one.
    /// A move alone or nothing at all is not counted.
    #[inline]
    pub(crate) fn count_probe(&mut self, probe: Probe) {
        match probe {
            Probe::Cut(value) => {
                self.score_cutoffs += 1;
                self.tainted_score_cutoffs += u64::from(value.tainted);
            }
            Probe::Refused(_) => self.refused_cutoffs += 1,
            Probe::Order(_) | Probe::Miss => {}
        }
    }

    /// A store offered to the table, counted with its taint if it landed.
    #[inline]
    pub(crate) fn count_store(&mut self, landed: bool, value: Value) {
        if landed {
            self.stores += 1;
            self.tainted_stores += u64::from(value.tainted);
        }
    }

    /// A store the taint policy declined before offering it to the table.
    #[inline]
    pub(crate) fn count_skipped_store(&mut self) {
        self.skipped_stores += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::GhiCounters;
    use crate::play::Play;
    use crate::transposition::Probe;
    use crate::value::Value;
    use pretty_assertions::assert_eq;

    fn play() -> Play {
        Play::new(0, 1, None, None, false, false)
    }

    fn counted(probe: Probe) -> GhiCounters {
        let mut counters = GhiCounters::default();
        counters.count_probe(probe);
        counters
    }

    #[test]
    fn a_clean_cutoff_counts_one_score_cutoff() {
        assert_eq!(
            counted(Probe::Cut(Value::clean(20))),
            GhiCounters {
                score_cutoffs: 1,
                ..Default::default()
            }
        );
    }

    #[test]
    fn a_tainted_cutoff_counts_as_a_cutoff_and_as_tainted() {
        assert_eq!(
            counted(Probe::Cut(Value::tainted(0))),
            GhiCounters {
                score_cutoffs: 1,
                tainted_score_cutoffs: 1,
                ..Default::default()
            }
        );
    }

    #[test]
    fn a_refused_cutoff_counts_only_the_refusal() {
        assert_eq!(
            counted(Probe::Refused(play())),
            GhiCounters {
                refused_cutoffs: 1,
                ..Default::default()
            }
        );
    }

    #[test]
    fn a_move_alone_or_a_miss_counts_nothing() {
        assert_eq!(counted(Probe::Order(play())), GhiCounters::default());
        assert_eq!(counted(Probe::Miss), GhiCounters::default());
    }

    #[test]
    fn a_landed_tainted_store_counts_both_figures() {
        let mut counters = GhiCounters::default();
        counters.count_store(true, Value::tainted(0));
        assert_eq!(
            counters,
            GhiCounters {
                stores: 1,
                tainted_stores: 1,
                ..Default::default()
            }
        );
        counters.count_store(true, Value::clean(0));
        assert_eq!((counters.stores, counters.tainted_stores), (2, 1));
    }

    #[test]
    fn a_store_that_did_not_land_counts_nothing() {
        let mut counters = GhiCounters::default();
        counters.count_store(false, Value::tainted(0));
        assert_eq!(counters, GhiCounters::default());
    }

    #[test]
    fn a_skipped_store_counts_only_its_own() {
        let mut counters = GhiCounters::default();
        counters.count_skipped_store();
        assert_eq!(
            counters,
            GhiCounters {
                skipped_stores: 1,
                ..Default::default()
            }
        );
    }
}
