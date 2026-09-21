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

/// The arguments that take words, in the order the usage lists them. The
/// two flags take none and are spelled in `usage` itself.
const COMMANDS: [&Command; 6] = [
    &uci::BENCH,
    &instruments::RESIDUALS,
    &instruments::CUTOFFS,
    &instruments::REDUCTIONS,
    &instruments::EFFORT,
    &instruments::TERMS,
];

/// Where a summary starts, so they line up under each other.
const SUMMARY_COLUMN: usize = 24;

/// The usage, built from the commands so a word added to one reaches the
/// help through the same list that parses it.
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

/// One of the five research commands: its report on stdout, or the setting
/// that could not be read on stderr with exit code 2, which the measuring
/// scripts check.
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
        // no argument starts the uci loop
        None => {
            let game = Board::new();
            let (e, asked) = AlphaBeta::new(game);
            // before the session, so an interface reading a smaller table
            // than the handshake advertises finds the reason above it in
            // the log
            if let Some(said) = uci::table_shortfall(asked, e.table_bytes()) {
                println!("{}", said);
            }
            let mut uci = UCI::new_with_engine(e);
            // a panic must reach the interface's log before the process goes
            uci.report_panics();
            uci.read_loop();
            ExitCode::SUCCESS
        }
        // not through `answer`, because the bench is the one command that
        // can be given settings it understands and still have no report to
        // make: the audit needs memory it may not get
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
        // asked for, so answered on stdout and succeeding; an unrecognised
        // argument keeps stderr and the failing code below
        Some("--version" | "-V") => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            print!("{}", usage());
            ExitCode::SUCCESS
        }
        // refused rather than fallen through to the uci loop, which would
        // sit waiting for input in silence
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

    /// The other way round, a dispatch arm with no entry in the table, is not
    /// caught: the match is code rather than data.
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
        // printed with print!, so the newline has to be in the string
        assert!(usage().ends_with('\n'));
    }
}
