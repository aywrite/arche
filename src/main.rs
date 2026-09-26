// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use arche::UCI;
use arche::command::Command;
use arche::instruments;
use arche::params::Params;
use arche::uci;
use arche_core::AlphaBeta;
use arche_core::Board;
use std::process::ExitCode;

/// The arguments that take words, in usage order.
const COMMANDS: [&Command; 6] = [
    &uci::BENCH,
    &instruments::RESIDUALS,
    &instruments::CUTOFFS,
    &instruments::REDUCTIONS,
    &instruments::EFFORT,
    &instruments::TERMS,
];

/// The column the summaries start at.
const SUMMARY_COLUMN: usize = 24;

/// The usage, built from the same declarations that parse the arguments.
fn usage() -> String {
    let mut out = String::new();
    out.push_str("arche, a chess engine speaking uci on stdin.\n\nUsage:\n");
    out.push_str(&format!(
        "{:<SUMMARY_COLUMN$}{}\n",
        "  arche", "start the uci loop and read commands from stdin",
    ));
    for command in COMMANDS {
        out.push_str(&format!("  arche {}\n", command.spelling()));
        for summary in command.summary {
            out.push_str(&format!("{}{}\n", " ".repeat(SUMMARY_COLUMN), summary));
        }
    }
    for (form, what) in [
        ("  arche --version, -V", "print the version"),
        ("  arche --help, -h", "print this"),
    ] {
        out.push_str(&format!("{:<SUMMARY_COLUMN$}{}\n", form, what));
    }
    out.push_str("\nThe uci loop answers bench and perft as commands as well.\n");
    out.push_str("Documentation: https://github.com/aywrite/arche\n");
    out
}

/// A research command's report on stdout, or the setting that could not be
/// read on stderr with exit code 2, which the measuring scripts check.
fn answer<S, R: std::fmt::Display>(
    command: &str,
    settings: Result<S, String>,
    run: impl FnOnce(S) -> R,
) -> ExitCode {
    match settings {
        Ok(settings) => {
            print!("{}", run(settings));
            ExitCode::SUCCESS
        }
        Err(what) => {
            eprintln!("unrecognised {} {}", command, what);
            ExitCode::from(2)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let line = args.join(" ");
    let params = Params::of(&line);
    match args.first().map(String::as_str) {
        None => {
            let game = Board::new();
            let (e, asked) = AlphaBeta::new(game);
            // before the handshake, so the log explains a table smaller than
            // the one advertised
            if let Some(said) = uci::table_shortfall(asked, e.table_bytes()) {
                println!("{}", said);
            }
            let mut uci = UCI::new_with_engine(e);
            uci.report_panics();
            uci.read_loop();
            ExitCode::SUCCESS
        }
        // not through `answer`: the audit may not get its memory, which is
        // a failure after the settings were read
        Some("bench") => match uci::bench_settings(&params) {
            Ok(settings) => match settings.run() {
                Some(report) => {
                    println!("{}", report);
                    ExitCode::SUCCESS
                }
                None => {
                    eprintln!("{}", uci::NO_AUDIT_MEMORY);
                    ExitCode::from(2)
                }
            },
            Err(what) => {
                eprintln!("unrecognised bench {}", what);
                ExitCode::from(2)
            }
        },
        Some("residuals") => answer("residuals", instruments::residual_settings(&params), |s| {
            s.run()
        }),
        Some("cutoffs") => answer("cutoffs", instruments::cutoff_settings(&params), |s| {
            s.run()
        }),
        Some("reductions") => answer(
            "reductions",
            instruments::reduction_settings(&params),
            |s| s.run(),
        ),
        Some("effort") => answer("effort", instruments::effort_settings(&params), |s| s.run()),
        Some("terms") => answer("terms", instruments::term_settings(&params), |s| s.run()),
        Some("--version" | "-V") => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            print!("{}", usage());
            ExitCode::SUCCESS
        }
        // refused rather than left to the uci loop, which would wait in
        // silence
        Some(other) => {
            eprintln!("unrecognised argument: {}", other);
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_usage_spells_every_word_every_command_takes() {
        let usage = usage();
        for command in COMMANDS {
            assert!(usage.contains(command.name), "{}", command.name);
            for keyword in command.keywords {
                assert!(
                    usage.contains(keyword.word),
                    "{} does not spell {}",
                    command.name,
                    keyword.word
                );
            }
            for flag in command.flags {
                assert!(
                    usage.contains(flag),
                    "{} does not spell {}",
                    command.name,
                    flag
                );
            }
        }
        for form in ["--version", "--help"] {
            assert!(usage.contains(form), "{form}");
        }
    }

    type Reader = fn(&Params) -> Result<(), String>;

    /// Every command beside its settings reader. Written out, because the
    /// dispatch in `main` runs the report rather than returning settings.
    fn readers() -> [(&'static Command, Reader); 6] {
        [
            (&uci::BENCH, |p| uci::bench_settings(p).map(|_| ())),
            (&instruments::RESIDUALS, |p| {
                instruments::residual_settings(p).map(|_| ())
            }),
            (&instruments::CUTOFFS, |p| {
                instruments::cutoff_settings(p).map(|_| ())
            }),
            (&instruments::REDUCTIONS, |p| {
                instruments::reduction_settings(p).map(|_| ())
            }),
            (&instruments::EFFORT, |p| {
                instruments::effort_settings(p).map(|_| ())
            }),
            (&instruments::TERMS, |p| {
                instruments::term_settings(p).map(|_| ())
            }),
        ]
    }

    /// Every keyword of every command, so a keyword added later is covered.
    /// A word typed with nothing after it used to run at the default in
    /// silence.
    #[test]
    fn every_keyword_given_no_value_is_refused_by_the_reader_that_takes_it() {
        for (command, read) in readers() {
            for keyword in command.keywords {
                let line = format!("{} {}", command.name, keyword.word);
                let what = read(&Params::of(&line)).expect_err(&line);
                assert!(
                    what.starts_with(&format!("{}: no value", keyword.word)),
                    "{line} was refused as {what}"
                );
            }
        }
    }

    #[test]
    fn every_command_in_the_usage_has_a_reader_beside_it() {
        let named: Vec<&str> = readers().iter().map(|(c, _)| c.name).collect();
        let listed: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        assert_eq!(named, listed);
    }

    /// A dispatch arm with no entry in `COMMANDS` is not caught: the match is
    /// code rather than data.
    #[test]
    fn every_command_the_usage_names_is_one_a_parser_answers_to() {
        assert!(uci::bench_settings(&Params::of(uci::BENCH.name)).is_ok());
        assert!(instruments::residual_settings(&Params::of(instruments::RESIDUALS.name)).is_ok());
        assert!(instruments::cutoff_settings(&Params::of(instruments::CUTOFFS.name)).is_ok());
        assert!(instruments::reduction_settings(&Params::of(instruments::REDUCTIONS.name)).is_ok());
        assert!(instruments::effort_settings(&Params::of(instruments::EFFORT.name)).is_ok());
        assert!(instruments::term_settings(&Params::of(instruments::TERMS.name)).is_ok());
    }

    #[test]
    fn the_usage_ends_in_a_newline() {
        // printed with print!
        assert!(usage().ends_with('\n'));
    }
}
