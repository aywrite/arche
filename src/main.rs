// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use arche::UCI;
use arche::params::Params;
use arche::uci;
use arche_core::AlphaBeta;
use arche_core::Board;
use arche_core::bench;
use arche_core::residual;
use std::process::ExitCode;

/// What the binary takes, for whoever ran it to find out. Short because there
/// is little to say: the engine speaks uci on stdin, and the one argument it
/// takes is a uci command too.
const USAGE: &str = "\
arche, a chess engine speaking uci on stdin.

Usage:
  arche                 start the uci loop and read commands from stdin
  arche bench [depth] [hash <MB>] [taint refuse|trust|skip|rule50] [audit]
                        search a fixed suite and print what each search counted,
                        with audit adding what the table's key signature cost
  arche residuals [depth] [every <n>] [taint refuse|trust|skip|rule50]
                        search the same suite, then ask the reference search
                        what the nodes the shortcuts answered were worth
  arche --version, -V   print the version
  arche --help, -h      print this

The uci loop answers bench and perft as commands as well.
Documentation: https://github.com/aywrite/arche
";

fn main() -> ExitCode {
    // `arche bench [depth] [hash <MB>] [taint refuse|trust|skip|rule50]
    // [audit]` prints the bench and exits, which is how the match tools
    // measure an engine's speed and how a commit states what its search
    // change did to the tree. The audit is the one word that adds a figure
    // rather than changing what is searched, and it is left out of every
    // run that is not asking about the table's key signature.
    // No argument starts the uci loop; anything else is a mistake,
    // and a mistake that started the uci loop would sit waiting for input in
    // silence
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {
            // a panic must reach the interface's log before the process goes:
            // stderr is where the backtrace lands and where no gui looks
            uci::report_panics_to(std::io::stdout());
            let game = Board::new();
            let e = AlphaBeta::new(game);
            UCI::new_with_engine(e).read_loop();
            ExitCode::SUCCESS
        }
        Some("bench") => match uci::bench_settings(&Params::of(&args.join(" "))) {
            Ok(settings) => {
                let positions = bench::positions();
                let report = if settings.audit {
                    bench::run_audited_suite(
                        &positions,
                        settings.depth,
                        settings.table_bytes,
                        settings.config,
                    )
                } else {
                    Some(bench::run_suite(
                        &positions,
                        settings.depth,
                        settings.table_bytes,
                        settings.config,
                    ))
                };
                match report {
                    Some(report) => {
                        println!("{}", report);
                        ExitCode::SUCCESS
                    }
                    // said and refused rather than run unaudited: the figures
                    // are what was asked for, and a report without them
                    // reads like a run that found nothing
                    None => {
                        eprintln!("no memory for the audit's keys, which are half the table again");
                        ExitCode::from(2)
                    }
                }
            }
            Err(what) => {
                eprintln!("unrecognised bench {}", what);
                ExitCode::from(2)
            }
        },
        // `arche residuals [depth] [every <n>] [taint <policy>]` measures
        // what the search's shortcuts cost in accuracy. An argument and not
        // a uci command: it takes minutes and answers a research question,
        // and nothing about a live session wants either
        Some("residuals") => match uci::residual_settings(&Params::of(&args.join(" "))) {
            Ok(settings) => {
                print!(
                    "{}",
                    residual::run(
                        &bench::positions(),
                        settings.depth,
                        settings.every,
                        settings.config
                    )
                );
                ExitCode::SUCCESS
            }
            Err(what) => {
                eprintln!("unrecognised residuals {}", what);
                ExitCode::from(2)
            }
        },
        // asked for, so both are answered on stdout and succeed. An argument
        // that really is unrecognised keeps stderr and the failing code
        // below: the difference is whether anybody wanted the output
        Some("--version" | "-V") => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            print!("{}", USAGE);
            ExitCode::SUCCESS
        }
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
        for form in ["arche bench", "arche residuals", "--version", "--help"] {
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
