// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! How a command line asks for the measurement instruments. What each one
//! measures is on its module in `arche-core`.
//!
//! Arguments rather than uci commands, since none of them is the protocol.
//! `bench` stays in `uci` because the engine answers it as both, and has its
//! entry in `INSTRUMENTS` beside the rest.

use crate::command::{Command, Keyword};
use crate::params::{NO_VALUE, Param, Params};
use crate::uci;
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
use std::fmt;

/// A line that has been read and not yet run. Running it does the work and
/// hands back what to print on stdout, or the failure it met after the
/// settings were read.
pub type Run = Box<dyn FnOnce() -> Result<Box<dyn fmt::Display>, String>>;

/// A command the binary takes and the reader of its settings, which refuses
/// a line with the setting it could not read.
pub struct Instrument {
    pub command: &'static Command,
    pub read: fn(&Params) -> Result<Run, String>,
}

/// Every command the binary takes, in usage order. The dispatch and the
/// usage both walk it, so a command cannot be listed without being taken.
/// The trace is listed only in a build with its feature.
pub const INSTRUMENTS: [Instrument; if cfg!(feature = "trace") { 7 } else { 6 }] = [
    Instrument {
        command: &uci::BENCH,
        read: read_bench,
    },
    Instrument {
        command: &RESIDUALS,
        read: |params| report(residual_settings(params)?, ResidualSettings::run),
    },
    Instrument {
        command: &CUTOFFS,
        read: |params| report(cutoff_settings(params)?, CutoffSettings::run),
    },
    Instrument {
        command: &REDUCTIONS,
        read: |params| report(reduction_settings(params)?, ReductionSettings::run),
    },
    Instrument {
        command: &EFFORT,
        read: |params| report(effort_settings(params)?, EffortSettings::run),
    },
    Instrument {
        command: &TERMS,
        read: |params| report(term_settings(params)?, TermSettings::run),
    },
    #[cfg(feature = "trace")]
    Instrument {
        command: &TRACE,
        read: read_trace,
    },
];

/// A run that prints the report as it stands.
fn report<S: 'static, R: fmt::Display + 'static>(
    settings: S,
    run: fn(&S) -> R,
) -> Result<Run, String> {
    Ok(Box::new(move || {
        Ok(Box::new(run(&settings)) as Box<dyn fmt::Display>)
    }))
}

/// The bench's run may find no memory for the audit's keys, a failure met
/// after the settings were read.
fn read_bench(params: &Params) -> Result<Run, String> {
    let settings = uci::bench_settings(params)?;
    Ok(Box::new(move || match settings.run() {
        Some(report) => Ok(Box::new(Line(report)) as Box<dyn fmt::Display>),
        None => Err(uci::NO_AUDIT_MEMORY.to_string()),
    }))
}

/// The trace's run may fail to write its directory, a failure met after
/// the settings were read.
#[cfg(feature = "trace")]
fn read_trace(params: &Params) -> Result<Run, String> {
    let settings = trace_settings(params)?;
    Ok(Box::new(move || match arche_core::trace::run(&settings) {
        Ok(report) => Ok(Box::new(report) as Box<dyn fmt::Display>),
        Err(e) => Err(format!("trace: {}", e)),
    }))
}

/// The bench's report and a newline. The report leaves its last line open,
/// since the uci loop ends each line it says; the other reports end in one.
struct Line(bench::Report);

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.0)
    }
}

/// The settings the four searching instruments share.
struct Sampling {
    depth: u8,
    every: u32,
    cap: usize,
}

/// What each instrument takes, for the usage and for refusing other words.
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

/// Taken only by a build with the trace feature, which is where its hooks
/// are compiled in.
#[cfg(feature = "trace")]
pub const TRACE: Command = Command {
    name: "trace",
    depth: true,
    keywords: &[
        Keyword {
            word: "every",
            value: "<n>",
        },
        Keyword {
            word: "window",
            value: "<plies>",
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
            word: "nodes",
            value: "<n>",
        },
        Keyword {
            word: "streams",
            value: "<stream>[,<stream>]",
        },
        Keyword {
            word: "out",
            value: "<dir>",
        },
    ],
    flags: &[],
    summary: &[
        "search the bench's suite, or the one named, and record",
        "what the hot functions are asked (built with --features trace)",
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
            value: "<switch>[,<switch>]",
        },
        Keyword {
            word: "budget",
            value: "<n>",
        },
    ],
    flags: &[],
    summary: &[
        "search the bench's suite, or the one named, twice, the",
        "second time with one search switch off or two, and print",
        "what the rules removed and where the effort they freed went",
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

/// The recorder's, since every searching instrument records through it.
const DEFAULT_CAP: usize = recorder::DEFAULT_CAP;

/// Reads the settings the instruments share, or names the setting and what
/// stood where its value would. `default_every` is the instrument's own rate.
fn sampling(params: &Params, command: &Command, default_every: u32) -> Result<Sampling, String> {
    let depth = command.depth(params, bench::DEPTH)?;
    // zero records every event up to the cap, which is a thing to ask for
    let every = params
        .parse::<u32>("every")
        .or_refuse("every")?
        .unwrap_or(default_every);
    let cap = params
        .parse::<usize>("cap")
        .or_refuse("cap")?
        .unwrap_or(DEFAULT_CAP);
    // last, so a bad depth is refused as `depth:` rather than as `word:`
    command.claim(params)?;
    Ok(Sampling { depth, every, cap })
}

/// What a residuals argument asked for. The depth, the suite and the policy
/// are the bench's own when absent.
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
    /// Read with the settings, so a file that is no suite is refused before
    /// the run.
    pub positions: Vec<bench::Position>,
}

pub fn residual_settings(params: &Params) -> Result<ResidualSettings, String> {
    let Sampling { depth, every, cap } = sampling(params, &RESIDUALS, residual::DEFAULT_EVERY)?;
    let config = uci::taint(params)?;
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

/// The file the line named, kept for the report's header, and the positions
/// read from it or the bench's own.
fn suite(params: &Params) -> Result<(Option<String>, Vec<bench::Position>), String> {
    let epd = params.value("epd").or_refuse("epd")?.map(str::to_string);
    let positions = match &epd {
        None => bench::positions(),
        Some(path) => read_epd(path)?,
    };
    Ok((epd, positions))
}

/// The positions of an epd file, or the path refused: a file that will not
/// open, holds no position, or holds one the board will not take (which the
/// run would otherwise panic on minutes in).
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

/// What a cutoffs argument asked for. The census records the search the
/// engine plays with, so there is no policy to choose.
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

/// What a reductions argument asked for. The ledger records the search the
/// engine plays with, so there is no policy to choose. The suite is a
/// setting for the residual sampler's reason.
pub struct ReductionSettings {
    pub depth: u8,
    pub every: u32,
    pub cap: usize,
    pub epd: Option<String>,
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

/// What an effort argument asked for.
///
/// `off` names the switch the baseline side turns off, or two joined by a
/// comma, and is absent for the null run. A pair is one word because a
/// keyword sent twice reads the first. `budget` holds both sides to a node
/// count as well as the depth, which takes speed out of the reading.
///
/// The suite is a setting, unlike the census's, because these readings are
/// quoted against game results and the bench's positions do not stand for
/// a game.
pub struct EffortSettings {
    pub depth: u8,
    pub every: u32,
    pub cap: usize,
    pub off: Option<Ablation>,
    pub budget: Option<u64>,
    pub epd: Option<String>,
    pub positions: Vec<bench::Position>,
}

/// The refusal for an `off` that names no switch, bare `off` included. It
/// lists the switches, which the usage line leaves as `<switch>`: the reader
/// who needs the names is the one who did not give one.
fn no_such_switch(word: &str) -> String {
    let switches = SearchConfig::SWITCHES.map(|(name, _)| name).join(", ");
    format!("off: {word} (a switch is one of {switches})")
}

/// The switch or the pair `off` names. A refusal echoes the whole word.
fn ablation(word: &str) -> Result<Ablation, String> {
    let named = |name: &str| SearchConfig::without(name).ok_or_else(|| no_such_switch(word));
    let mut names = word.split(',');
    let first = named(names.next().unwrap_or_default())?;
    let Some(second) = names.next() else {
        return Ok(first);
    };
    let second = named(second)?;
    if names.next().is_some() {
        return Err(format!("off: {word} (a pair is two switches)"));
    }
    first
        .and(second)
        .ok_or_else(|| format!("off: {word} (a pair is two different switches)"))
}

pub fn effort_settings(params: &Params) -> Result<EffortSettings, String> {
    let Sampling { depth, every, cap } = sampling(params, &EFFORT, effort::DEFAULT_EVERY)?;
    // a misspelling read as the null would spend the run saying nothing
    let off = match params.value("off") {
        Param::Absent => None,
        Param::Read(word) => Some(ablation(word)?),
        Param::Bare => return Err(no_such_switch(NO_VALUE)),
        Param::Unreadable(word) => return Err(no_such_switch(word)),
    };
    let budget = params.parse::<u64>("budget").or_refuse("budget")?;
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

/// What a terms argument asked for. No depth, rate or cap: a run states
/// every quiet position of the suite, because the corpus is what is being
/// built.
pub struct TermSettings {
    pub epd: Option<String>,
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

/// What a trace argument asked for. The output directory defaults to
/// `trace` under the working directory.
#[cfg(feature = "trace")]
pub fn trace_settings(params: &Params) -> Result<arche_core::trace::Settings, String> {
    use arche_core::trace;
    let depth = TRACE.depth(params, bench::DEPTH)?;
    let every = params
        .parse::<u64>("every")
        .or_refuse("every")?
        .unwrap_or(trace::DEFAULT_EVERY);
    let window = params
        .parse::<u8>("window")
        .or_refuse("window")?
        .unwrap_or(trace::DEFAULT_WINDOW);
    let cap = params
        .parse::<u64>("cap")
        .or_refuse("cap")?
        .unwrap_or(trace::DEFAULT_CAP);
    let out = params
        .value("out")
        .or_refuse("out")?
        .unwrap_or("trace")
        .into();
    let nodes = params.parse::<u64>("nodes").or_refuse("nodes")?;
    let streams = params
        .value("streams")
        .or_refuse("streams")?
        .map(|s| s.split(',').map(str::to_string).collect::<Vec<_>>());
    if let Some(unknown) = streams.iter().flatten().find(|s| !trace::is_stream(s)) {
        return Err(format!("streams: {unknown}"));
    }
    // before the suite is read, so a word past the file is named rather
    // than the file
    TRACE.claim(params)?;
    let (epd, positions) = suite(params)?;
    Ok(trace::Settings {
        depth,
        nodes,
        streams,
        every,
        window,
        cap,
        out,
        epd,
        positions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_file(path: &str) -> Vec<bench::Position> {
        bench::parse_epd(&std::fs::read_to_string(path).expect(path))
    }

    /// A file that opens and holds no position, written for one test and
    /// removed after. The name carries the test's so parallel tests do not
    /// share a file.
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

    /// A suite other than the bench's, for a line that names one.
    const SUITE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/arche-core/tactics.epd");

    /// A value each keyword takes, so a line can name the keyword and still
    /// be read. It has to be one the reader accepts: the readers parse a
    /// value before `claim`, so a refused one would be named as
    /// `<kw>: <value>` rather than as given twice. A keyword added without
    /// one here fails the tests that ask.
    fn a_value_for(keyword: &str) -> &'static str {
        match keyword {
            "every" | "cap" | "budget" | "hash" | "window" => "1",
            "out" => "trace",
            "nodes" => "1",
            "streams" => "makes",
            "epd" => SUITE,
            "taint" => "trust",
            "off" => "null_move",
            _ => panic!("no value to give {keyword}"),
        }
    }

    /// What the reader refused the line as. A read line is not run.
    fn refusal(instrument: &Instrument, line: &str) -> String {
        match (instrument.read)(&Params::of(line)) {
            Ok(_) => panic!("{line} was read"),
            Err(what) => what,
        }
    }

    /// A misspelt stream would otherwise record the nodes alone, in silence.
    #[cfg(feature = "trace")]
    #[test]
    fn the_trace_refuses_a_stream_it_does_not_write() {
        let trace = INSTRUMENTS
            .iter()
            .find(|instrument| instrument.command.name == "trace")
            .expect("the trace is an instrument");
        assert_eq!(refusal(trace, "trace streams serach"), "streams: serach");
        assert_eq!(refusal(trace, "trace streams makes,x"), "streams: x");
        assert!((trace.read)(&Params::of("trace streams makes,nodes")).is_ok());
    }

    #[test]
    fn every_instrument_reads_its_name_alone() {
        for instrument in &INSTRUMENTS {
            let name = instrument.command.name;
            assert!((instrument.read)(&Params::of(name)).is_ok(), "{name}");
        }
    }

    /// Past the depth's place, where a word is refused as `depth:`.
    #[test]
    fn every_instrument_refuses_a_word_it_does_not_know() {
        for instrument in &INSTRUMENTS {
            let command = instrument.command;
            assert!(!command.takes("spare"), "{}", command.name);
            let line = if command.depth {
                format!("{} 1 spare", command.name)
            } else {
                format!("{} spare", command.name)
            };
            assert_eq!(refusal(instrument, &line), "word: spare", "{line}");
        }
    }

    /// The reader takes the first, so the second would be read by nobody.
    #[test]
    fn every_instrument_refuses_a_keyword_given_twice() {
        for instrument in &INSTRUMENTS {
            let name = instrument.command.name;
            for keyword in instrument.command.keywords {
                let (word, value) = (keyword.word, a_value_for(keyword.word));
                for line in [
                    format!("{name} {word} {value} {word} {value}"),
                    format!("{name} {word} {value} {word}"),
                ] {
                    assert_eq!(
                        refusal(instrument, &line),
                        format!("{word}: given twice"),
                        "{line}"
                    );
                }
            }
        }
    }

    /// A word typed with nothing after it used to run at the default in
    /// silence.
    #[test]
    fn every_keyword_given_no_value_is_refused_by_the_reader_that_takes_it() {
        for instrument in &INSTRUMENTS {
            for keyword in instrument.command.keywords {
                let line = format!("{} {}", instrument.command.name, keyword.word);
                let what = refusal(instrument, &line);
                assert!(
                    what.starts_with(&format!("{}: no value", keyword.word)),
                    "{line} was refused as {what}"
                );
            }
        }
    }

    /// The manifest is the case worth pinning: it opens and parses into
    /// positions whose fens no board will take. A reader that skipped the
    /// suite would run the bench's positions in its place.
    #[test]
    fn every_instrument_that_takes_a_suite_refuses_one_that_is_no_suite() {
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let empty = Unpositioned::written("instruments");
        for instrument in &INSTRUMENTS {
            let command = instrument.command;
            if !command.keywords.iter().any(|keyword| keyword.word == "epd") {
                continue;
            }
            for path in ["no/such/file.epd", manifest, &empty.path] {
                let line = format!("{} epd {path}", command.name);
                assert_eq!(refusal(instrument, &line), format!("epd: {path}"), "{line}");
            }
            // the line is refused before the suite is read, so a word past
            // the file is named rather than the file
            let line = format!("{} epd no/such/file.epd spare", command.name);
            assert_eq!(refusal(instrument, &line), "word: spare", "{line}");
        }
    }

    /// The bench's report leaves its last line open for the uci loop to
    /// end, so the argument's run ends it. Depth one takes milliseconds.
    #[test]
    fn the_bench_argument_ends_its_last_line() {
        let bench = INSTRUMENTS
            .iter()
            .find(|instrument| instrument.command.name == "bench")
            .expect("the bench is an instrument");
        let Ok(run) = (bench.read)(&Params::of("bench 1 hash 1")) else {
            panic!("bench 1 hash 1 was refused");
        };
        let Ok(report) = run() else {
            panic!("bench 1 hash 1 found no memory");
        };
        let printed = report.to_string();
        assert!(printed.ends_with(" nps\n"), "{printed}");
        assert!(!printed.ends_with("\n\n"), "{printed}");
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

        let path = SUITE;
        let line = format!("residuals 4 epd {path}");
        let named = residual_settings(&Params::of(&line)).expect(&line);
        assert_eq!(named.depth, 4);
        assert_eq!(named.epd.as_deref(), Some(path));
        // a file other than the bench's, so handing back the bench's own
        // positions is caught
        assert_eq!(named.positions, from_file(path));
        assert_ne!(named.positions, bench::positions());
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

        let path = SUITE;
        let line = format!("reductions 4 epd {path}");
        let named = reduction_settings(&Params::of(&line)).expect(&line);
        assert_eq!(named.depth, 4);
        assert_eq!(named.epd.as_deref(), Some(path));
        assert_eq!(named.positions, from_file(path));
        assert_ne!(named.positions, bench::positions());
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
                settings.off.map(Ablation::name),
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
            (bench::DEPTH, 1000, CAP, Some("null_move".to_string()), None)
        );
        assert_eq!(
            read("effort 4 off quiet_futility budget 4000000 cap 500"),
            (
                4,
                1000,
                500,
                Some("quiet_futility".to_string()),
                Some(4_000_000)
            )
        );
        // a pair is one word, and the header gives it back in the order typed
        assert_eq!(
            read("effort 4 off late_move_count,quiet_futility"),
            (
                4,
                1000,
                CAP,
                Some("late_move_count,quiet_futility".to_string()),
                None
            )
        );
        // every field the run can turn off is one the argument takes
        for (switch, _) in SearchConfig::SWITCHES {
            let line = format!("effort 2 off {switch}");
            let settings = effort_settings(&Params::of(&line)).expect(&line);
            assert_eq!(settings.off.map(Ablation::name), Some(switch.to_string()));
        }
    }

    #[test]
    fn an_effort_argument_reads_the_suite_it_was_given() {
        let bench = effort_settings(&Params::of("effort")).expect("effort");
        assert_eq!(bench.epd, None);
        assert_eq!(bench.positions, bench::positions());

        let path = SUITE;
        let line = format!("effort 4 epd {path}");
        let named = effort_settings(&Params::of(&line)).expect(&line);
        assert_eq!(named.depth, 4);
        assert_eq!(named.epd.as_deref(), Some(path));
        assert_eq!(named.positions, from_file(path));
        assert_ne!(named.positions, bench::positions());
    }

    #[test]
    fn an_unreadable_effort_setting_is_named_rather_than_run() {
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
            // either half of a pair is refused as a single word would be,
            // and the refusal echoes the whole of what was typed
            (
                "effort 4 off null_move,quiet_futilty".to_string(),
                format!("off: null_move,quiet_futilty (a switch is one of {switches})"),
            ),
            (
                "effort 4 off null_move,".to_string(),
                format!("off: null_move, (a switch is one of {switches})"),
            ),
            (
                "effort 4 off null_move,null_move".to_string(),
                "off: null_move,null_move (a pair is two different switches)".to_string(),
            ),
            (
                "effort 4 off null_move,aspiration,move_memory".to_string(),
                "off: null_move,aspiration,move_memory (a pair is two switches)".to_string(),
            ),
            (
                "effort 4 budget lots".to_string(),
                "budget: lots".to_string(),
            ),
        ] {
            assert_eq!(
                effort_settings(&Params::of(&line)).err(),
                Some(what),
                "{line}"
            );
        }
    }

    /// Read as absent, `effort 4 off` would run the null for as long as the
    /// run asked for. `off` carries the switch names on this refusal as well
    /// as on a misspelling, for the reason on `no_such_switch`.
    #[test]
    fn an_off_given_no_value_names_the_switches() {
        let switches = SearchConfig::SWITCHES.map(|(name, _)| name).join(", ");
        assert_eq!(
            effort_settings(&Params::of("effort 4 off")).err(),
            Some(format!("off: no value (a switch is one of {switches})"))
        );
    }

    #[test]
    fn a_terms_argument_reads_the_suite_it_was_given() {
        let bench = term_settings(&Params::of("terms")).expect("terms");
        assert_eq!(bench.epd, None);
        assert_eq!(bench.positions, bench::positions());

        let path = SUITE;
        let line = format!("terms epd {path}");
        let named = term_settings(&Params::of(&line)).expect(&line);
        assert_eq!(named.epd.as_deref(), Some(path));
        assert_eq!(named.positions, from_file(path));
        assert_ne!(named.positions, bench::positions());
    }

    /// This argument has no depth, so a number where one would stand is a
    /// word it does not know.
    #[test]
    fn an_unreadable_terms_setting_is_named_rather_than_run() {
        for (line, what) in [("terms 4", "word: 4"), ("terms every 50", "word: every")] {
            assert_eq!(
                term_settings(&Params::of(line)).err(),
                Some(what.to_string()),
                "{line}"
            );
        }
    }

    /// The four rate defaults are the same number today, so the assertions
    /// on them cannot tell which one a reader passed. The loop only shows
    /// that `sampling` uses the rate it is handed.
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
