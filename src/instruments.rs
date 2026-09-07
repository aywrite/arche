// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The measurement instruments, as a command line asks for them.
//!
//! Three of them: the residual sampler, the cutoff census and the reduction
//! ledger. Each searches the bench's positions, records a sample of what the
//! search did, and prints a report. What each one measures is on its module in
//! `arche-core`; what is here is only how a command line spells it.
//!
//! Their own module rather than `uci`, because none of them is the protocol.
//! An interface will never send `cutoffs`, and would not wait for the answer
//! if it did — these take minutes and answer a research question, which is why
//! they are arguments rather than commands. They were spelled with the same
//! `Params` reader as the uci commands and had come to live beside them, which
//! left the module named for the protocol about half full of things no
//! interface can ask for.
//!
//! `bench` stays in `uci`, because the engine really does answer it as a
//! command as well as as an argument.
//!
//! All three take the same three settings and used to read them three times
//! over, in three functions that differed by a keyword and two defaults.
//! `sampling` is that reading, once.

use crate::command::{Command, Keyword};
use crate::params::{Param, Params};
use arche_core::Board;
use arche_core::SearchConfig;
use arche_core::bench;
use arche_core::census;
use arche_core::recorder;
use arche_core::reduction;
use arche_core::residual;

/// A depth, a rate and a cap: what all three arguments take, and the whole of
/// what the cutoffs and reductions arguments take.
struct Sampling {
    depth: u8,
    every: u32,
    cap: usize,
}

/// What each instrument takes. One declaration apiece, because the same list
/// says which words may stand where the depth would, which words are known at
/// all, and how the usage spells the line.
pub const RESIDUALS: Command = Command {
    name: "residuals",
    keywords: &[
        Keyword {
            word: "every",
            value: "<n>",
        },
        Keyword {
            word: "cap",
            value: "<n>",
        },
        Keyword {
            word: "epd",
            value: "<file>",
        },
        Keyword {
            word: "taint",
            value: "refuse|trust|skip|rule50",
        },
    ],
    flags: &[],
    summary: &[
        "search the bench's suite, or the one named, then ask the",
        "reference search what the nodes the shortcuts answered",
        "were worth",
    ],
};

pub const CUTOFFS: Command = Command {
    name: "cutoffs",
    keywords: &[
        Keyword {
            word: "every",
            value: "<n>",
        },
        Keyword {
            word: "cap",
            value: "<n>",
        },
    ],
    flags: &[],
    summary: &[
        "search the same suite and print which move cut each",
        "sampled node off, or that none did",
    ],
};

pub const REDUCTIONS: Command = Command {
    name: "reductions",
    keywords: &[
        Keyword {
            word: "every",
            value: "<n>",
        },
        Keyword {
            word: "cap",
            value: "<n>",
        },
        Keyword {
            word: "epd",
            value: "<file>",
        },
    ],
    flags: &[],
    summary: &[
        "search the bench's suite, or the one named, sample the",
        "reduced scouts, and ask a full depth search whether each",
        "trusted fail low threw a move away",
    ],
};

/// How many rows a run keeps when it was not told. One number for all three,
/// and the recorder's, because the thing it bounds is one thing: all three
/// record through the reservoir that module defines, so the cap is the
/// reservoir's rather than any one instrument's.
const DEFAULT_CAP: usize = recorder::DEFAULT_CAP;

/// Reads the three settings the instruments share, or says which word could
/// not be read: the setting's name and the word, for the caller to report.
/// Running the default in place of a word nobody typed would take minutes and
/// explain nothing.
///
/// `command` says both what word the depth follows and which words may stand
/// in its place. `default_every` is the rate that instrument samples at when
/// the line names none, which is the one thing they do not share.
fn sampling(params: &Params, command: &Command, default_every: u32) -> Result<Sampling, String> {
    let depth = match params.parse::<u8>(command.name) {
        Param::Absent => bench::DEPTH,
        Param::Read(depth) => depth,
        Param::Unreadable(word) if command.takes(word) => bench::DEPTH,
        Param::Unreadable(word) => return Err(format!("depth: {word}")),
    };
    let every = match params.parse::<u32>("every") {
        Param::Absent => default_every,
        // zero records every event the instrument offers, up to the cap, which
        // is a thing to ask for rather than a mistake
        Param::Read(every) => every,
        Param::Unreadable(word) => return Err(format!("every: {word}")),
    };
    let cap = match params.parse::<usize>("cap") {
        Param::Absent => DEFAULT_CAP,
        Param::Read(cap) => cap,
        Param::Unreadable(word) => return Err(format!("cap: {word}")),
    };
    // last, so a word that was going to be read as the depth has already
    // been refused under the better name
    command.claim(params)?;
    Ok(Sampling { depth, every, cap })
}

/// What a residuals argument asked for: `residuals [depth] [every <n>]
/// [cap <n>] [epd <file>] [taint refuse|trust|skip|rule50]`. The depth, the
/// suite and the policy are the bench's own when absent, so a residual
/// distribution is measured over the tree the bench describes; the rate is
/// how much of that tree is sampled, and the cap is the most of it the run
/// keeps.
///
/// The suite is a setting here and on the reduction ledger, which is the
/// other instrument a number gets chosen off; the taint policy is this
/// one's alone. What wants the suite is a margin chosen off these rows: a
/// rule fitted on the bench's positions and then read back on the same
/// positions has checked nothing, so the fit and the check are given
/// separate files.
pub struct ResidualSettings {
    pub depth: u8,
    pub every: u32,
    pub cap: usize,
    pub config: SearchConfig,
    /// The file the suite was read from, or none for the bench's own.
    pub epd: Option<String>,
    /// The positions themselves, read while the settings are, so a file
    /// that is no suite is refused before the minutes are spent.
    pub positions: Vec<bench::Position>,
}

pub fn residual_settings(params: &Params) -> Result<ResidualSettings, String> {
    let Sampling { depth, every, cap } = sampling(params, &RESIDUALS, residual::DEFAULT_EVERY)?;
    let config = match params.value("taint") {
        None => SearchConfig::default(),
        Some(word) => SearchConfig::with_taint(word).ok_or_else(|| format!("taint: {word}"))?,
    };
    let epd = params.value("epd").map(str::to_string);
    let positions = match &epd {
        None => bench::positions(),
        Some(path) => read_epd(path)?,
    };
    Ok(ResidualSettings {
        depth,
        every,
        cap,
        config,
        epd,
        positions,
    })
}

/// The positions of an epd file, or the path that could not be read as a
/// suite.
///
/// Three failures read alike, because none of them leaves a suite to search:
/// a file that will not open, one that holds no position, and one that holds
/// a position the board will not take. The third is answered here rather
/// than left to the run, which panics on it partway through a search that
/// has already cost minutes.
fn read_epd(path: &str) -> Result<Vec<bench::Position>, String> {
    let refused = || format!("epd: {path}");
    let text = std::fs::read_to_string(path).map_err(|_| refused())?;
    let positions = bench::parse_epd(&text);
    if positions.is_empty() {
        return Err(refused());
    }
    if positions
        .iter()
        .any(|position| Board::from_fen(&position.fen).is_err())
    {
        return Err(refused());
    }
    Ok(positions)
}

impl ResidualSettings {
    /// Runs the residual measurement these settings describe, over the
    /// bench's own positions unless the line named a file, so the
    /// distribution is measured over the tree the header names.
    pub fn run(&self) -> residual::Report {
        residual::run(
            &self.positions,
            self.epd.as_deref(),
            self.depth,
            self.every,
            self.cap,
            self.config,
        )
    }
}

/// What a cutoffs argument asked for: `cutoffs [depth] [every <n>]
/// [cap <n>]`. The census records the search the engine plays with and
/// nothing else, so there is no policy to choose.
pub struct CutoffSettings {
    pub depth: u8,
    pub every: u32,
    pub cap: usize,
}

pub fn cutoff_settings(params: &Params) -> Result<CutoffSettings, String> {
    let Sampling { depth, every, cap } = sampling(params, &CUTOFFS, census::DEFAULT_EVERY)?;
    Ok(CutoffSettings { depth, every, cap })
}

impl CutoffSettings {
    /// Runs the census these settings describe, over the bench's own
    /// positions, so the rows describe the tree the bench describes.
    pub fn run(&self) -> census::Report {
        census::run(&bench::positions(), self.depth, self.every, self.cap)
    }
}

/// What a reductions argument asked for: `reductions [depth] [every <n>]
/// [cap <n>] [epd <file>]`. The ledger records the search the engine plays
/// with and replays it with the reference, so there is no policy to choose.
///
/// The suite is a setting here for the residual sampler's reason. A
/// reduction threshold chosen off these rows and then read back on the
/// same positions has checked nothing, so the fit and the check are given
/// separate files.
pub struct ReductionSettings {
    pub depth: u8,
    pub every: u32,
    pub cap: usize,
    /// The file the suite was read from, or none for the bench's own.
    pub epd: Option<String>,
    /// The positions themselves, read while the settings are, so a file
    /// that is no suite is refused before the minutes are spent.
    pub positions: Vec<bench::Position>,
}

pub fn reduction_settings(params: &Params) -> Result<ReductionSettings, String> {
    let Sampling { depth, every, cap } = sampling(params, &REDUCTIONS, reduction::DEFAULT_EVERY)?;
    let epd = params.value("epd").map(str::to_string);
    let positions = match &epd {
        None => bench::positions(),
        Some(path) => read_epd(path)?,
    };
    Ok(ReductionSettings {
        depth,
        every,
        cap,
        epd,
        positions,
    })
}

impl ReductionSettings {
    /// Runs the ledger these settings describe, over the bench's own
    /// positions unless the line named a file, so the rows describe the
    /// tree the header names.
    pub fn run(&self) -> reduction::Report {
        reduction::run(
            &self.positions,
            self.epd.as_deref(),
            self.depth,
            self.every,
            self.cap,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The settings alone, not a run: the argument searches the suite twice
    /// over and reading what it was asked for is the part worth pinning.
    #[test]
    fn a_residuals_argument_reads_its_depth_rate_cap_and_policy() {
        const CAP: usize = recorder::DEFAULT_CAP;
        let read = |line: &str| {
            let settings = residual_settings(&Params::of(line)).expect(line);
            (
                settings.depth,
                settings.every,
                settings.cap,
                settings.config.taint_word().to_string(),
            )
        };
        assert_eq!(
            read("residuals"),
            (bench::DEPTH, 1000, CAP, "rule50".to_string())
        );
        assert_eq!(read("residuals 4"), (4, 1000, CAP, "rule50".to_string()));
        assert_eq!(
            read("residuals 4 every 50"),
            (4, 50, CAP, "rule50".to_string())
        );
        // a word standing where the depth would be means the depth was left
        // out rather than mistyped, the same rule the bench reads by
        assert_eq!(
            read("residuals every 50 taint trust"),
            (bench::DEPTH, 50, CAP, "trust".to_string())
        );
        assert_eq!(
            read("residuals cap 500"),
            (bench::DEPTH, 1000, 500, "rule50".to_string())
        );
        // and zero is a rate to ask for: it records every node a shortcut
        // answers, up to the cap
        assert_eq!(
            read("residuals 2 every 0"),
            (2, 0, CAP, "rule50".to_string())
        );
        // a calibration run raises the cap rather than patching the source
        assert_eq!(
            read("residuals 4 every 50 cap 200000"),
            (4, 50, 200_000, "rule50".to_string())
        );
    }

    /// The suite is the bench's own unless the line names a file, and a
    /// named one is read while the settings are rather than at the run.
    #[test]
    fn a_residuals_argument_reads_the_suite_it_was_given() {
        let bench = residual_settings(&Params::of("residuals")).expect("residuals");
        assert_eq!(bench.epd, None);
        assert_eq!(bench.positions, bench::positions());

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/arche-core/bench.epd");
        let line = format!("residuals 4 epd {path}");
        let named = residual_settings(&Params::of(&line)).expect(&line);
        assert_eq!(named.depth, 4);
        assert_eq!(named.epd.as_deref(), Some(path));
        // the same file the bench compiles in, so the two agree
        assert_eq!(named.positions, bench::positions());
    }

    /// A file that is no suite is named rather than searched, and the file
    /// that is not epd at all is the case worth pinning: it opens and
    /// parses into positions whose fens no board will take.
    #[test]
    fn a_residuals_suite_that_is_no_suite_is_named_rather_than_run() {
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        for (line, what) in [
            (
                "residuals 4 epd no/such/file.epd".to_string(),
                "epd: no/such/file.epd".to_string(),
            ),
            (
                format!("residuals 4 epd {manifest}"),
                format!("epd: {manifest}"),
            ),
        ] {
            assert_eq!(
                residual_settings(&Params::of(&line)).err(),
                Some(what),
                "{line}"
            );
        }
    }

    #[test]
    fn an_unreadable_residuals_setting_is_named_rather_than_run() {
        for (line, what) in [
            ("residuals abc", "depth: abc"),
            ("residuals 300", "depth: 300"),
            ("residuals 4 every lots", "every: lots"),
            ("residuals 4 cap lots", "cap: lots"),
            ("residuals 4 taint maybe", "taint: maybe"),
        ] {
            assert_eq!(
                residual_settings(&Params::of(line)).err(),
                Some(what.to_string()),
                "{line}"
            );
        }
    }

    /// The settings alone, not a run, for the residuals test's reason.
    #[test]
    fn a_cutoffs_argument_reads_its_depth_rate_and_cap() {
        let read = |line: &str| {
            let settings = cutoff_settings(&Params::of(line)).expect(line);
            (settings.depth, settings.every, settings.cap)
        };
        const CAP: usize = recorder::DEFAULT_CAP;
        assert_eq!(read("cutoffs"), (bench::DEPTH, 1000, CAP));
        assert_eq!(read("cutoffs 4"), (4, 1000, CAP));
        assert_eq!(read("cutoffs 4 every 50"), (4, 50, CAP));
        // a word standing where the depth would be means the depth was
        // left out rather than mistyped, the rule the bench reads by
        assert_eq!(read("cutoffs every 50 cap 500"), (bench::DEPTH, 50, 500));
        // and zero is a rate to ask for: it records every node the move
        // loop answers, up to the cap
        assert_eq!(read("cutoffs 2 every 0"), (2, 0, CAP));
    }

    #[test]
    fn an_unreadable_cutoffs_setting_is_named_rather_than_run() {
        for (line, what) in [
            ("cutoffs abc", "depth: abc"),
            ("cutoffs 300", "depth: 300"),
            ("cutoffs 4 every lots", "every: lots"),
            ("cutoffs 4 cap lots", "cap: lots"),
        ] {
            assert_eq!(
                cutoff_settings(&Params::of(line)).err(),
                Some(what.to_string()),
                "{line}"
            );
        }
    }

    /// The settings alone, not a run, for the residuals test's reason.
    #[test]
    fn a_reductions_argument_reads_its_depth_rate_and_cap() {
        let read = |line: &str| {
            let settings = reduction_settings(&Params::of(line)).expect(line);
            (settings.depth, settings.every, settings.cap)
        };
        const CAP: usize = recorder::DEFAULT_CAP;
        assert_eq!(read("reductions"), (bench::DEPTH, 1000, CAP));
        assert_eq!(read("reductions 4"), (4, 1000, CAP));
        assert_eq!(read("reductions 4 every 50"), (4, 50, CAP));
        // a word standing where the depth would be means the depth was
        // left out rather than mistyped, the rule the bench reads by
        assert_eq!(read("reductions every 50 cap 500"), (bench::DEPTH, 50, 500));
        // and zero is a rate to ask for: it records every reduced scout,
        // up to the cap
        assert_eq!(read("reductions 2 every 0"), (2, 0, CAP));
    }

    /// The ledger's own suite word, on the residuals test's terms: the
    /// bench's positions when none is named, and the named file's when one
    /// is.
    #[test]
    fn a_reductions_argument_reads_the_suite_it_was_given() {
        let bench = reduction_settings(&Params::of("reductions")).expect("reductions");
        assert_eq!(bench.epd, None);
        assert_eq!(bench.positions, bench::positions());

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/arche-core/bench.epd");
        let line = format!("reductions 4 epd {path}");
        let named = reduction_settings(&Params::of(&line)).expect(&line);
        assert_eq!(named.depth, 4);
        assert_eq!(named.epd.as_deref(), Some(path));
        // the same file the bench compiles in, so the two agree
        assert_eq!(named.positions, bench::positions());
    }

    /// A file that is no suite is refused before the run, the way the
    /// residual sampler refuses one.
    #[test]
    fn a_reductions_suite_that_is_no_suite_is_named_rather_than_run() {
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        for (line, what) in [
            (
                "reductions 4 epd no/such/file.epd".to_string(),
                "epd: no/such/file.epd".to_string(),
            ),
            (
                format!("reductions 4 epd {manifest}"),
                format!("epd: {manifest}"),
            ),
        ] {
            assert_eq!(
                reduction_settings(&Params::of(&line)).err(),
                Some(what),
                "{line}"
            );
        }
    }

    #[test]
    fn an_unreadable_reductions_setting_is_named_rather_than_run() {
        for (line, what) in [
            ("reductions abc", "depth: abc"),
            ("reductions 300", "depth: 300"),
            ("reductions 4 every lots", "every: lots"),
            ("reductions 4 cap lots", "cap: lots"),
        ] {
            assert_eq!(
                reduction_settings(&Params::of(line)).err(),
                Some(what.to_string()),
                "{line}"
            );
        }
    }

    /// The three read their depth by the same rule and their rate from their
    /// own default, which is the whole of what `sampling` had to keep true
    /// when it replaced three copies of it.
    #[test]
    fn every_instrument_defaults_to_the_benchs_depth_and_its_own_rate() {
        let residuals = residual_settings(&Params::of("residuals")).unwrap();
        let cutoffs = cutoff_settings(&Params::of("cutoffs")).unwrap();
        let reductions = reduction_settings(&Params::of("reductions")).unwrap();

        for depth in [residuals.depth, cutoffs.depth, reductions.depth] {
            assert_eq!(depth, bench::DEPTH);
        }
        for cap in [residuals.cap, cutoffs.cap, reductions.cap] {
            assert_eq!(cap, DEFAULT_CAP);
        }
        assert_eq!(residuals.every, residual::DEFAULT_EVERY);
        assert_eq!(cutoffs.every, census::DEFAULT_EVERY);
        assert_eq!(reductions.every, reduction::DEFAULT_EVERY);
    }
}
