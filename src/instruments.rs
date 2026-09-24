// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The measurement instruments, as a command line asks for them.
//!
//! Five of them: the residual sampler, the cutoff census, the reduction
//! ledger, the effort instrument and the term extraction. The first four
//! search the bench's positions, record a sample of what the search did,
//! and print a report; the fifth searches nothing and writes down what each
//! position's evaluation is made of. What each one measures is on its
//! module in `arche-core`; what is here is only how a command line spells
//! it.
//!
//! Not in `uci`, because none of them is the protocol: they take minutes and
//! answer a research question, which is why they are arguments rather than
//! commands. `bench` stays in `uci` because the engine answers it as both.

use crate::command::{Command, Keyword};
use crate::params::{Param, Params};
use arche_core::Ablation;
use arche_core::Board;
use arche_core::SearchConfig;
use arche_core::bench;
use arche_core::census;
use arche_core::effort;
use arche_core::recorder;
use arche_core::reduction;
use arche_core::residual;
use arche_core::tune;

/// The settings the four searching instruments share.
struct Sampling {
    depth: u8,
    every: u32,
    cap: usize,
}

/// What each instrument takes: the usage's spelling and the words it refuses.
pub const RESIDUALS: Command = Command {
    name: "residuals",
    depth: true,
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
    depth: true,
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
    depth: true,
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

pub const EFFORT: Command = Command {
    name: "effort",
    depth: true,
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
            word: "off",
            value: "<switch>",
        },
        Keyword {
            word: "budget",
            value: "<n>",
        },
    ],
    flags: &[],
    summary: &[
        "search the bench's suite, or the one named, twice, the",
        "second time with one search switch off, and print what the",
        "rule removed and where the effort it freed went",
    ],
};

pub const TERMS: Command = Command {
    name: "terms",
    // the walk reads the board and the quiet test runs a capture search,
    // neither of which takes a depth
    depth: false,
    keywords: &[Keyword {
        word: "epd",
        value: "<file>",
    }],
    flags: &[],
    summary: &[
        "print what each quiet position of the bench's suite, or",
        "the one named, makes its evaluation out of",
    ],
};

/// How many rows a run keeps when it was not told. The recorder's, because
/// all four record through the reservoir that module defines.
const DEFAULT_CAP: usize = recorder::DEFAULT_CAP;

/// Reads the settings the instruments share, or names the setting and the
/// word that could not be read. Running the default in place of a word
/// nobody typed would take minutes and explain nothing.
///
/// `default_every` is the rate the instrument samples at when the line names
/// none, the one setting they do not share.
fn sampling(params: &Params, command: &Command, default_every: u32) -> Result<Sampling, String> {
    let depth = command.depth(params, bench::DEPTH)?;
    let every = match params.parse::<u32>("every") {
        Param::Absent => default_every,
        // zero records every event up to the cap, which is a thing to ask for
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
/// suite and the policy are the bench's own when absent; the rate is how
/// much of the tree is sampled, and the cap the most of it the run keeps.
///
/// The suite is a setting so that a margin fitted on one file can be checked
/// on another: a rule fitted on the bench's positions and read back on the
/// same positions has checked nothing.
pub struct ResidualSettings {
    pub depth: u8,
    pub every: u32,
    pub cap: usize,
    pub config: SearchConfig,
    /// The file the suite was read from, or none for the bench's own.
    pub epd: Option<String>,
    /// Read while the settings are, so a file that is no suite is refused
    /// before the minutes are spent.
    pub positions: Vec<bench::Position>,
}

pub fn residual_settings(params: &Params) -> Result<ResidualSettings, String> {
    let Sampling { depth, every, cap } = sampling(params, &RESIDUALS, residual::DEFAULT_EVERY)?;
    let config = crate::uci::taint(params)?;
    let (epd, positions) = suite(params)?;
    Ok(ResidualSettings {
        depth,
        every,
        cap,
        config,
        epd,
        positions,
    })
}

/// The suite an instrument was asked for: the file the line named, and the
/// positions read from it or the bench's own. The path is kept because the
/// report's header states it.
fn suite(params: &Params) -> Result<(Option<String>, Vec<bench::Position>), String> {
    let epd = params.value("epd").map(str::to_string);
    let positions = match &epd {
        None => bench::positions(),
        Some(path) => read_epd(path)?,
    };
    Ok((epd, positions))
}

/// The positions of an epd file, or the path that could not be read as a
/// suite: a file that will not open, one that holds no position, and one
/// that holds a position the board will not take. The third is refused here
/// rather than left to the run, which would panic on it minutes in.
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
/// [cap <n>]`. The census records the search the engine plays with, so
/// there is no policy to choose.
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
    pub fn run(&self) -> census::Report {
        census::run(&bench::positions(), self.depth, self.every, self.cap)
    }
}

/// What a reductions argument asked for: `reductions [depth] [every <n>]
/// [cap <n>] [epd <file>]`. The ledger records the search the engine plays
/// with and replays it with the reference, so there is no policy to choose.
/// The suite is a setting for the residual sampler's reason.
pub struct ReductionSettings {
    pub depth: u8,
    pub every: u32,
    pub cap: usize,
    /// The file the suite was read from, or none for the bench's own.
    pub epd: Option<String>,
    /// Read while the settings are, so a file that is no suite is refused
    /// before the minutes are spent.
    pub positions: Vec<bench::Position>,
}

pub fn reduction_settings(params: &Params) -> Result<ReductionSettings, String> {
    let Sampling { depth, every, cap } = sampling(params, &REDUCTIONS, reduction::DEFAULT_EVERY)?;
    let (epd, positions) = suite(params)?;
    Ok(ReductionSettings {
        depth,
        every,
        cap,
        epd,
        positions,
    })
}

impl ReductionSettings {
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

/// What an effort argument asked for: `effort [depth] [every <n>]
/// [cap <n>] [epd <file>] [off <switch>] [budget <n>]`.
///
/// `off` names the `SearchConfig` field the baseline side turns off, and is
/// absent for the null run, where both sides are the default. `budget`
/// holds both sides to a node count instead of to the depth alone, which
/// takes the speed channel out of the reading by construction.
///
/// The suite is a setting for the residual sampler's reason, and against
/// the census's precedent: the readings here will be quoted against game
/// results, and the bench's eighteen positions are recorded as not standing
/// for a game.
pub struct EffortSettings {
    pub depth: u8,
    pub every: u32,
    pub cap: usize,
    pub off: Option<Ablation>,
    pub budget: Option<u64>,
    /// The file the suite was read from, or none for the bench's own.
    pub epd: Option<String>,
    /// Read while the settings are, so a file that is no suite is refused
    /// before the minutes are spent.
    pub positions: Vec<bench::Position>,
}

/// What is said when `off` names something that is no switch: the word as it
/// was typed, and then the names the table carries. In the refusal rather
/// than in the usage line, which keeps `<switch>`: a fourteen name list there
/// is long and the join awkward for what it buys, and the reader who needs
/// the names is the one who got the word wrong.
fn no_such_switch(word: &str) -> String {
    let switches = SearchConfig::SWITCHES.map(|(name, _)| name).join(", ");
    format!("off: {word} (a switch is one of {switches})")
}

pub fn effort_settings(params: &Params) -> Result<EffortSettings, String> {
    let Sampling { depth, every, cap } = sampling(params, &EFFORT, effort::DEFAULT_EVERY)?;
    // refused against the field names, the way `tune.py --hold TERM` is
    // refused against the layout the extraction prints: a misspelling read as
    // the null would spend the minutes saying nothing
    let off = params
        .value("off")
        .map(|word| SearchConfig::without(word).ok_or_else(|| no_such_switch(word)))
        .transpose()?;
    let budget = match params.parse::<u64>("budget") {
        Param::Absent => None,
        Param::Read(nodes) => Some(nodes),
        Param::Unreadable(word) => return Err(format!("budget: {word}")),
    };
    let (epd, positions) = suite(params)?;
    Ok(EffortSettings {
        depth,
        every,
        cap,
        off,
        budget,
        epd,
        positions,
    })
}

impl EffortSettings {
    pub fn run(&self) -> effort::Report {
        effort::run(
            &self.positions,
            self.epd.as_deref(),
            self.depth,
            self.every,
            self.cap,
            self.off,
            self.budget,
        )
    }
}

/// What a terms argument asked for: `terms [epd <file>]`. The suite is the
/// bench's own when absent. No depth, rate or cap: a run states every quiet
/// position of the suite, because a corpus is the thing being built and a
/// share of one would only be a smaller corpus.
pub struct TermSettings {
    /// The file the suite was read from, or none for the bench's own.
    pub epd: Option<String>,
    /// Read while the settings are, so a file that is no suite is refused
    /// before the run.
    pub positions: Vec<bench::Position>,
}

pub fn term_settings(params: &Params) -> Result<TermSettings, String> {
    TERMS.claim(params)?;
    let (epd, positions) = suite(params)?;
    Ok(TermSettings { epd, positions })
}

impl TermSettings {
    pub fn run(&self) -> tune::Report {
        tune::run(&self.positions, self.epd.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The positions of a checked-in suite, for a test to hold what the
    /// argument read against.
    fn from_file(path: &str) -> Vec<bench::Position> {
        bench::parse_epd(&std::fs::read_to_string(path).expect(path))
    }

    /// A file that opens and holds no position. Nothing checked in is one,
    /// so it is written for the test that asks and removed after. The name
    /// carries the test's so two tests running at once do not share a file.
    struct Unpositioned {
        path: String,
    }

    impl Unpositioned {
        fn written(test: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "arche-{}-{}-unpositioned.epd",
                test,
                std::process::id()
            ));
            std::fs::write(
                &path,
                "# a comment and nothing else

",
            )
            .expect("the temp dir takes a file");
            Unpositioned {
                path: path.to_string_lossy().into_owned(),
            }
        }
    }

    impl Drop for Unpositioned {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// The settings alone, not a run: the run costs minutes.
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
        // a keyword where the depth would be means the depth was left out
        assert_eq!(
            read("residuals every 50 taint trust"),
            (bench::DEPTH, 50, CAP, "trust".to_string())
        );
        assert_eq!(
            read("residuals cap 500"),
            (bench::DEPTH, 1000, 500, "rule50".to_string())
        );
        // zero records every event, up to the cap
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

    #[test]
    fn a_residuals_argument_reads_the_suite_it_was_given() {
        let bench = residual_settings(&Params::of("residuals")).expect("residuals");
        assert_eq!(bench.epd, None);
        assert_eq!(bench.positions, bench::positions());

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/arche-core/tactics.epd");
        let line = format!("residuals 4 epd {path}");
        let named = residual_settings(&Params::of(&line)).expect(&line);
        assert_eq!(named.depth, 4);
        assert_eq!(named.epd.as_deref(), Some(path));
        // a file other than the bench's, so a reader that checked the file
        // and then handed back the bench's own positions is caught
        assert_eq!(named.positions, from_file(path));
        assert_ne!(named.positions, bench::positions());
    }

    /// The file that is not epd at all is the case worth pinning: it opens
    /// and parses into positions whose fens no board will take.
    #[test]
    fn a_residuals_suite_that_is_no_suite_is_named_rather_than_run() {
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let empty = Unpositioned::written("residuals");
        for (line, what) in [
            (
                "residuals 4 epd no/such/file.epd".to_string(),
                "epd: no/such/file.epd".to_string(),
            ),
            (
                format!("residuals 4 epd {manifest}"),
                format!("epd: {manifest}"),
            ),
            (
                format!("residuals 4 epd {}", empty.path),
                format!("epd: {}", empty.path),
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
        assert_eq!(read("cutoffs every 50 cap 500"), (bench::DEPTH, 50, 500));
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
        assert_eq!(read("reductions every 50 cap 500"), (bench::DEPTH, 50, 500));
        assert_eq!(read("reductions 2 every 0"), (2, 0, CAP));
    }

    #[test]
    fn a_reductions_argument_reads_the_suite_it_was_given() {
        let bench = reduction_settings(&Params::of("reductions")).expect("reductions");
        assert_eq!(bench.epd, None);
        assert_eq!(bench.positions, bench::positions());

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/arche-core/tactics.epd");
        let line = format!("reductions 4 epd {path}");
        let named = reduction_settings(&Params::of(&line)).expect(&line);
        assert_eq!(named.depth, 4);
        assert_eq!(named.epd.as_deref(), Some(path));
        assert_eq!(named.positions, from_file(path));
        assert_ne!(named.positions, bench::positions());
    }

    #[test]
    fn a_reductions_suite_that_is_no_suite_is_named_rather_than_run() {
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let empty = Unpositioned::written("reductions");
        for (line, what) in [
            (
                "reductions 4 epd no/such/file.epd".to_string(),
                "epd: no/such/file.epd".to_string(),
            ),
            (
                format!("reductions 4 epd {manifest}"),
                format!("epd: {manifest}"),
            ),
            (
                format!("reductions 4 epd {}", empty.path),
                format!("epd: {}", empty.path),
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

    #[test]
    fn an_effort_argument_reads_its_depth_rate_cap_switch_and_budget() {
        let read = |line: &str| {
            let settings = effort_settings(&Params::of(line)).expect(line);
            (
                settings.depth,
                settings.every,
                settings.cap,
                settings.off.map(|ablation| ablation.name()),
                settings.budget,
            )
        };
        const CAP: usize = recorder::DEFAULT_CAP;
        assert_eq!(read("effort"), (bench::DEPTH, 1000, CAP, None, None));
        assert_eq!(read("effort 4"), (4, 1000, CAP, None, None));
        assert_eq!(read("effort 4 every 50"), (4, 50, CAP, None, None));
        // a keyword where the depth would be means the depth was left out
        assert_eq!(
            read("effort off null_move"),
            (bench::DEPTH, 1000, CAP, Some("null_move"), None)
        );
        assert_eq!(
            read("effort 4 off quiet_futility budget 4000000 cap 500"),
            (4, 1000, 500, Some("quiet_futility"), Some(4_000_000))
        );
        // every field the run can turn off is one the argument takes
        for (switch, _) in SearchConfig::SWITCHES {
            let line = format!("effort 2 off {switch}");
            let settings = effort_settings(&Params::of(&line)).expect(&line);
            assert_eq!(settings.off.map(|ablation| ablation.name()), Some(switch));
        }
    }

    #[test]
    fn an_effort_argument_reads_the_suite_it_was_given() {
        let bench = effort_settings(&Params::of("effort")).expect("effort");
        assert_eq!(bench.epd, None);
        assert_eq!(bench.positions, bench::positions());

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/arche-core/tactics.epd");
        let line = format!("effort 4 epd {path}");
        let named = effort_settings(&Params::of(&line)).expect(&line);
        assert_eq!(named.depth, 4);
        assert_eq!(named.epd.as_deref(), Some(path));
        assert_eq!(named.positions, from_file(path));
        assert_ne!(named.positions, bench::positions());
    }

    /// A switch that is not one is named before the minutes are spent, the
    /// way a suite that is no suite is. Read as the null it would run for as
    /// long and answer a question nobody asked.
    #[test]
    fn an_unreadable_effort_setting_is_named_rather_than_run() {
        let empty = Unpositioned::written("effort");
        let switches = SearchConfig::SWITCHES.map(|(name, _)| name).join(", ");
        for (line, what) in [
            ("effort abc".to_string(), "depth: abc".to_string()),
            ("effort 4 every lots".to_string(), "every: lots".to_string()),
            ("effort 4 cap lots".to_string(), "cap: lots".to_string()),
            (
                "effort 4 off quiet_futilty".to_string(),
                format!("off: quiet_futilty (a switch is one of {switches})"),
            ),
            // a policy is not a switch, and the sampler that takes it says so
            (
                "effort 4 off taint".to_string(),
                format!("off: taint (a switch is one of {switches})"),
            ),
            (
                "effort 4 budget lots".to_string(),
                "budget: lots".to_string(),
            ),
            (
                format!("effort 4 epd {}", empty.path),
                format!("epd: {}", empty.path),
            ),
        ] {
            assert_eq!(
                effort_settings(&Params::of(&line)).err(),
                Some(what),
                "{line}"
            );
        }
    }

    #[test]
    fn a_terms_argument_reads_the_suite_it_was_given() {
        let bench = term_settings(&Params::of("terms")).expect("terms");
        assert_eq!(bench.epd, None);
        assert_eq!(bench.positions, bench::positions());

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/arche-core/tactics.epd");
        let line = format!("terms epd {path}");
        let named = term_settings(&Params::of(&line)).expect(&line);
        assert_eq!(named.epd.as_deref(), Some(path));
        assert_eq!(named.positions, from_file(path));
        assert_ne!(named.positions, bench::positions());
    }

    #[test]
    fn a_terms_suite_that_is_no_suite_is_named_rather_than_run() {
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let empty = Unpositioned::written("terms");
        for (line, what) in [
            (
                "terms epd no/such/file.epd".to_string(),
                "epd: no/such/file.epd".to_string(),
            ),
            (format!("terms epd {manifest}"), format!("epd: {manifest}")),
            (
                format!("terms epd {}", empty.path),
                format!("epd: {}", empty.path),
            ),
        ] {
            assert_eq!(
                term_settings(&Params::of(&line)).err(),
                Some(what),
                "{line}"
            );
        }
    }

    /// This argument has no depth, so a number where one would stand is a
    /// word it does not know.
    #[test]
    fn an_unreadable_terms_setting_is_named_rather_than_run() {
        for (line, what) in [
            ("terms 4", "word: 4"),
            ("terms every 50", "word: every"),
            ("terms epd suite.epd spare", "word: spare"),
        ] {
            assert_eq!(
                term_settings(&Params::of(line)).err(),
                Some(what.to_string()),
                "{line}"
            );
        }
    }

    /// The four rate defaults are the same number today, so the assertions
    /// on them cannot tell which one a caller passed. The loop below can, by
    /// handing the four commands four rates that differ.
    #[test]
    fn every_instrument_defaults_to_the_benchs_depth_and_its_own_rate() {
        let residuals = residual_settings(&Params::of("residuals")).unwrap();
        let cutoffs = cutoff_settings(&Params::of("cutoffs")).unwrap();
        let reductions = reduction_settings(&Params::of("reductions")).unwrap();
        let effort = effort_settings(&Params::of("effort")).unwrap();

        for depth in [
            residuals.depth,
            cutoffs.depth,
            reductions.depth,
            effort.depth,
        ] {
            assert_eq!(depth, bench::DEPTH);
        }
        for cap in [residuals.cap, cutoffs.cap, reductions.cap, effort.cap] {
            assert_eq!(cap, DEFAULT_CAP);
        }
        assert_eq!(residuals.every, residual::DEFAULT_EVERY);
        assert_eq!(cutoffs.every, census::DEFAULT_EVERY);
        assert_eq!(reductions.every, reduction::DEFAULT_EVERY);
        assert_eq!(effort.every, effort::DEFAULT_EVERY);

        for (command, word, default) in [
            (&RESIDUALS, "residuals", 11),
            (&CUTOFFS, "cutoffs", 22),
            (&REDUCTIONS, "reductions", 33),
            (&EFFORT, "effort", 44),
        ] {
            let read = sampling(&Params::of(word), command, default).expect(word);
            assert_eq!(read.every, default, "{word} took a rate not its own");
        }
    }
}
