// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use arche::UCI;
use arche::instruments;
use arche::params::Params;
use arche::uci;
use arche_core::AlphaBeta;
use arche_core::Board;
use std::process::ExitCode;

/// What the binary takes, for whoever ran it to find out. Short because there
/// is little to say: the engine speaks uci on stdin, and four of the six
/// arguments it takes are measurements, one of which is a uci command too.
const USAGE: &str = "\
arche, a chess engine speaking uci on stdin.

Usage:
  arche                 start the uci loop and read commands from stdin
  arche bench [depth] [hash <MB>] [taint refuse|trust|skip|rule50] [audit]
                        search a fixed suite and print what each search counted,
                        with audit adding what the table's key signature cost
  arche residuals [depth] [every <n>] [cap <n>] [taint refuse|trust|skip|rule50]
                        search the same suite, then ask the reference search
                        what the nodes the shortcuts answered were worth
  arche cutoffs [depth] [every <n>] [cap <n>]
                        search the same suite and print which move cut each
                        sampled node off, or that none did
  arche reductions [depth] [every <n>] [cap <n>]
                        search the same suite, sample the reduced scouts, and
                        ask a full depth search whether each trusted fail low
                        threw a move away
  arche --version, -V   print the version
  arche --help, -h      print this

The uci loop answers bench and perft as commands as well.
Documentation: https://github.com/aywrite/arche
";

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
            print!("{}", USAGE);
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
    use super::USAGE;

    #[test]
    fn the_usage_names_every_form_the_binary_takes() {
        for form in [
            "arche bench",
            "arche residuals",
            "arche cutoffs",
            "arche reductions",
            "--version",
            "--help",
        ] {
            assert!(USAGE.contains(form), "the usage does not mention {}", form);
        }
    }

    #[test]
    fn the_usage_ends_in_a_newline() {
        // printed with print! rather than println!, so the trailing newline
        // has to be in the string or a shell prompt lands on the last line
        assert!(USAGE.ends_with('\n'));
    }
}
