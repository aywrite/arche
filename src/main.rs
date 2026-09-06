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

/// The arguments the binary takes, in the order the usage lists them. Four of
/// the six are measurements, one of which is a uci command too; the other two
/// are the flags below, which take no words and so are not commands.
const COMMANDS: [&Command; 4] = [
    &uci::BENCH,
    &instruments::RESIDUALS,
    &instruments::CUTOFFS,
    &instruments::REDUCTIONS,
];

/// Where a summary starts, so they line up under each other whatever the
/// spelling above them is as long as.
const SUMMARY_COLUMN: usize = 24;

/// What the binary takes, for whoever ran it to find out.
///
/// Built from the commands rather than written out beside them. It was a
/// string kept in step by hand, and the test that guarded it held it against
/// a list written out by hand as well, so the two could drift together and
/// pass. A word added to a command now reaches this because it is the same
/// list that parses it.
///
/// The three lines that are not commands take no words, so they are spelled
/// here: the bare form that starts the loop, and the two flags.
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

/// One of the three research commands: its report on stdout, or the setting
/// that could not be read on stderr and the code the measuring scripts check.
///
/// Each is an argument and not a uci command because it takes minutes and
/// answers a research question, and nothing about a live session wants
/// either. What each one measures is on its module.
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
        // no argument starts the uci loop, which is what an interface runs
        None => {
            let game = Board::new();
            let e = AlphaBeta::new(game);
            let mut uci = UCI::new_with_engine(e);
            // a panic must reach the interface's log before the process goes:
            // stderr is where the backtrace lands and where no gui looks
            uci.report_panics();
            uci.read_loop();
            ExitCode::SUCCESS
        }
        // the bench prints and exits, which is how the match tools measure
        // an engine's speed and how a commit states what its search change
        // did to the tree. It answers its own way rather than through
        // `answer` because it is the one command that can be given
        // settings it understands and still have no report to make: the
        // audit needs memory it may not get
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
        // `--version` and `--help` were asked for, so both are answered on
        // stdout and succeed. An argument that really is unrecognised keeps
        // stderr and the failing code below: the difference is whether
        // anybody wanted the output
        Some("--version" | "-V") => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            print!("{}", usage());
            ExitCode::SUCCESS
        }
        // said and refused rather than fallen through to the uci loop,
        // which would sit waiting for input in silence
        Some(other) => {
            eprintln!("unrecognised argument: {}", other);
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What generating the usage buys: the help cannot describe a line the
    /// parser does not take, or leave out a word it does, because one list
    /// is the source of both.
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

    /// The table drives the usage, so this is what stops it naming something
    /// no parser answers to. The other way round, a dispatch arm with no
    /// entry here, is not caught: the match is code rather than data, and
    /// making it data would cost more than the hole is worth.
    #[test]
    fn every_command_the_usage_names_is_one_a_parser_answers_to() {
        assert!(uci::bench_settings(&Params::of(uci::BENCH.name)).is_ok());
        assert!(instruments::residual_settings(&Params::of(instruments::RESIDUALS.name)).is_ok());
        assert!(instruments::cutoff_settings(&Params::of(instruments::CUTOFFS.name)).is_ok());
        assert!(instruments::reduction_settings(&Params::of(instruments::REDUCTIONS.name)).is_ok());
    }

    #[test]
    fn the_usage_ends_in_a_newline() {
        // printed with print! rather than println!, so the trailing newline
        // has to be in the string or a shell prompt lands on the last line
        assert!(usage().ends_with('\n'));
    }
}
