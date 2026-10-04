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
use arche_core::{
    Ablation, Board, SearchConfig, bench, census, effort, forced, recorder, reduction, residual,
    tune,
};
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
pub const INSTRUMENTS: [Instrument; 7] = [
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
        command: &FORCED,
        read: |params| report(forced_settings(params)?, ForcedSettings::run),
    },
    Instrument {
        command: &TERMS,
        read: |params| report(term_settings(params)?, TermSettings::run),
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

pub const FORCED: Command = Command {
    name: "forced",
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
            word: "kinds",
            value: "<kind>[,<kind>]",
        },
        Keyword {
            word: "from",
            value: "<depth>",
        },
    ],
    flags: &[],
    summary: &[
        "search the bench's suite, or the one named, sample the",
        "shortcut decisions taken, and search each root again with",
        "one of them inverted",
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
    if positions.is_empty()
        || positions
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
        // `value` never reads Unreadable, since any word is a word
        Param::Bare | Param::Unreadable(_) => return Err(no_such_switch(NO_VALUE)),
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

/// What a forced argument asked for. `kinds` narrows the decisions sampled
/// to those named, comma separated, and is absent for all four. `from` is
/// the shallowest depth a decision is sampled at, since the shallow ones
/// are most of them and a run may be after the deep.
pub struct ForcedSettings {
    pub depth: u8,
    pub every: u32,
    pub cap: usize,
    pub kinds: forced::Kinds,
    pub from: u8,
    pub epd: Option<String>,
    pub positions: Vec<bench::Position>,
}

/// The refusal for a `kinds` that names no kind, listing them.
fn no_such_kind(word: &str) -> String {
    let kinds = forced::Kind::ALL.map(forced::Kind::word).join(", ");
    format!("kinds: {word} (a kind is one of {kinds})")
}

/// The kinds `kinds` names. A refusal echoes the whole word, and a kind
/// named twice is refused, since it is a word the reader did not mean.
fn kinds(word: &str) -> Result<forced::Kinds, String> {
    let mut named = Vec::new();
    for part in word.split(',') {
        let kind = forced::Kind::of_word(part).ok_or_else(|| no_such_kind(word))?;
        if named.contains(&kind) {
            return Err(format!("kinds: {word} (a kind named twice)"));
        }
        named.push(kind);
    }
    Ok(forced::Kinds::of(&named))
}

pub fn forced_settings(params: &Params) -> Result<ForcedSettings, String> {
    let Sampling { depth, every, cap } = sampling(params, &FORCED, forced::DEFAULT_EVERY)?;
    let kinds = match params.value("kinds") {
        Param::Absent => forced::Kinds::ALL,
        Param::Read(word) => kinds(word)?,
        Param::Bare | Param::Unreadable(_) => return Err(no_such_kind(NO_VALUE)),
    };
    let from = params.parse::<u8>("from").or_refuse("from")?.unwrap_or(0);
    let (epd, positions) = suite(params)?;
    Ok(ForcedSettings {
        depth,
        every,
        cap,
        kinds,
        from,
        epd,
        positions,
    })
}

impl ForcedSettings {
    pub fn run(&self) -> forced::Report {
        forced::run(
            &self.positions,
            self.epd.as_deref(),
            self.depth,
            self.every,
            self.cap,
            self.kinds,
            self.from,
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
            "every" | "cap" | "budget" | "hash" => "1",
            "epd" => SUITE,
            "taint" => "trust",
            "off" => "null_move",
            "kinds" => "skip",
            "from" => "1",
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

    /// A sampling reader by name, as the depth, rate and cap it read off a
    /// line or its refusal.
    type SamplingReader = (&'static str, fn(&str) -> Result<(u8, u32, usize), String>);

    /// The four share `sampling`, so what it reads is tested through this
    /// table and each reader's own settings beside it.
    fn sampling_readers() -> [SamplingReader; 4] {
        [
            ("residuals", |line| {
                residual_settings(&Params::of(line)).map(|s| (s.depth, s.every, s.cap))
            }),
            ("cutoffs", |line| {
                cutoff_settings(&Params::of(line)).map(|s| (s.depth, s.every, s.cap))
            }),
            ("reductions", |line| {
                reduction_settings(&Params::of(line)).map(|s| (s.depth, s.every, s.cap))
            }),
            ("effort", |line| {
                effort_settings(&Params::of(line)).map(|s| (s.depth, s.every, s.cap))
            }),
        ]
    }

    /// The settings alone, not a run: the run costs minutes.
    #[test]
    fn every_sampling_instrument_reads_its_depth_rate_and_cap() {
        const CAP: usize = recorder::DEFAULT_CAP;
        for (name, read) in sampling_readers() {
            let settings = |rest: &str| {
                let line = format!("{name} {rest}");
                let line = line.trim_end();
                read(line).expect(line)
            };
            assert_eq!(settings(""), (bench::DEPTH, 1000, CAP), "{name}");
            assert_eq!(settings("4"), (4, 1000, CAP), "{name}");
            assert_eq!(settings("4 every 50"), (4, 50, CAP), "{name}");
            // a keyword where the depth would be means the depth was left out
            assert_eq!(
                settings("every 50 cap 500"),
                (bench::DEPTH, 50, 500),
                "{name}"
            );
            assert_eq!(settings("cap 500"), (bench::DEPTH, 1000, 500), "{name}");
            // zero records every event, up to the cap
            assert_eq!(settings("2 every 0"), (2, 0, CAP), "{name}");
            assert_eq!(
                settings("4 every 50 cap 200000"),
                (4, 50, 200_000),
                "{name}"
            );
        }
    }

    #[test]
    fn an_unreadable_sampling_setting_is_named_rather_than_run() {
        for (name, read) in sampling_readers() {
            for (rest, what) in [
                ("abc", "depth: abc"),
                ("300", "depth: 300"),
                ("4 every lots", "every: lots"),
                ("4 cap lots", "cap: lots"),
            ] {
                let line = format!("{name} {rest}");
                assert_eq!(read(&line).err(), Some(what.to_string()), "{line}");
            }
        }
    }

    /// The policy is the residuals argument's own, the bench's when absent.
    #[test]
    fn a_residuals_argument_reads_its_taint_policy() {
        let policy = |line: &str| {
            let settings = residual_settings(&Params::of(line)).expect(line);
            settings.config.taint_word().to_string()
        };
        assert_eq!(policy("residuals"), "rule50");
        assert_eq!(policy("residuals every 50 taint trust"), "trust");
        assert_eq!(
            residual_settings(&Params::of("residuals 4 taint maybe")).err(),
            Some("taint: maybe".to_string())
        );
    }

    /// What a reader that takes a suite read of one: the depth, where the
    /// command takes one, the file named and its positions.
    type ReadSuite = (Option<u8>, Option<String>, Vec<bench::Position>);

    /// A reader that takes a suite, with its command.
    type SuiteReader = (&'static Command, fn(&str) -> ReadSuite);

    #[test]
    fn every_instrument_that_takes_a_suite_reads_the_one_it_was_given() {
        let readers: [SuiteReader; 5] = [
            (&RESIDUALS, |line| {
                let s = residual_settings(&Params::of(line)).expect(line);
                (Some(s.depth), s.epd, s.positions)
            }),
            (&REDUCTIONS, |line| {
                let s = reduction_settings(&Params::of(line)).expect(line);
                (Some(s.depth), s.epd, s.positions)
            }),
            (&EFFORT, |line| {
                let s = effort_settings(&Params::of(line)).expect(line);
                (Some(s.depth), s.epd, s.positions)
            }),
            (&FORCED, |line| {
                let s = forced_settings(&Params::of(line)).expect(line);
                (Some(s.depth), s.epd, s.positions)
            }),
            (&TERMS, |line| {
                let s = term_settings(&Params::of(line)).expect(line);
                (None, s.epd, s.positions)
            }),
        ];
        for (command, read) in readers {
            let name = command.name;
            let (_, epd, positions) = read(name);
            assert_eq!(epd, None, "{name}");
            assert_eq!(positions, bench::positions(), "{name}");

            let path = SUITE;
            // a depth before the file, where the command takes one, is read
            // with it
            let depth = command.depth.then_some(4);
            let line = match depth {
                Some(depth) => format!("{name} {depth} epd {path}"),
                None => format!("{name} epd {path}"),
            };
            let (read_depth, epd, positions) = read(&line);
            assert_eq!(read_depth, depth, "{line}");
            assert_eq!(epd.as_deref(), Some(path), "{line}");
            // a file other than the bench's, so handing back the bench's own
            // positions is caught
            assert_eq!(positions, from_file(path), "{line}");
            assert_ne!(positions, bench::positions(), "{line}");
        }
    }

    /// The switch and the budget are the effort argument's own.
    #[test]
    fn an_effort_argument_reads_its_switch_and_budget() {
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
    fn an_unreadable_effort_switch_or_budget_is_named_rather_than_run() {
        let switches = SearchConfig::SWITCHES.map(|(name, _)| name).join(", ");
        for (line, what) in [
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

    #[test]
    fn forced_reads_the_kinds_named_and_refuses_the_rest() {
        let read = |line: &str| forced_settings(&Params::of(line)).map(|s| s.kinds);
        assert_eq!(
            read("forced kinds skip,trusted_scout"),
            Ok(forced::Kinds::of(&[
                forced::Kind::Skip,
                forced::Kind::TrustedScout
            ]))
        );
        let listed = "(a kind is one of reverse_futility, null_move, skip, trusted_scout)";
        for (line, refused) in [
            ("forced kinds skips", format!("kinds: skips {listed}")),
            (
                "forced kinds skip,nul_move",
                format!("kinds: skip,nul_move {listed}"),
            ),
            (
                "forced kinds skip,skip",
                "kinds: skip,skip (a kind named twice)".to_string(),
            ),
        ] {
            assert_eq!(read(line).err(), Some(refused), "{line}");
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
        let forced = forced_settings(&Params::of("forced")).unwrap();

        for depth in [
            residuals.depth,
            cutoffs.depth,
            reductions.depth,
            effort.depth,
            forced.depth,
        ] {
            assert_eq!(depth, bench::DEPTH);
        }
        for cap in [
            residuals.cap,
            cutoffs.cap,
            reductions.cap,
            effort.cap,
            forced.cap,
        ] {
            assert_eq!(cap, DEFAULT_CAP);
        }
        assert_eq!(forced.every, forced::DEFAULT_EVERY);
        assert_eq!(forced.kinds, forced::Kinds::ALL);
        assert_eq!(forced.from, 0);
        assert_eq!(
            forced_settings(&Params::of("forced from 4")).map(|s| s.from),
            Ok(4)
        );
        assert_eq!(residuals.every, residual::DEFAULT_EVERY);
        assert_eq!(cutoffs.every, census::DEFAULT_EVERY);
        assert_eq!(reductions.every, reduction::DEFAULT_EVERY);
        assert_eq!(effort.every, effort::DEFAULT_EVERY);

        for (command, word, default) in [
            (&RESIDUALS, "residuals", 11),
            (&CUTOFFS, "cutoffs", 22),
            (&REDUCTIONS, "reductions", 33),
            (&EFFORT, "effort", 44),
            (&FORCED, "forced", 55),
        ] {
            let read = sampling(&Params::of(word), command, default).expect(word);
            assert_eq!(read.every, default, "{word} took a rate not its own");
        }
    }
}
