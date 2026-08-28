// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use arche::UCI;
use arche::params::Params;
use arche::uci;
use arche_core::AlphaBeta;
use arche_core::Board;
use arche_core::bench;
use std::process::ExitCode;

/// What the binary takes, for whoever ran it to find out. Short because there
/// is little to say: the engine speaks uci on stdin, and the one argument it
/// takes is a uci command too.
const USAGE: &str = "\
arche, a chess engine speaking uci on stdin.

Usage:
  arche                 start the uci loop and read commands from stdin
  arche bench [depth] [hash <MB>] [taint refuse|trust|skip|rule50]
                        search a fixed suite and print what each search counted
  arche --version, -V   print the version
  arche --help, -h      print this

The uci loop answers bench and perft as commands as well.
Documentation: https://github.com/aywrite/arche
";

fn main() -> ExitCode {
    // `arche bench [depth] [hash <MB>] [taint refuse|trust|skip|rule50]`
    // prints the bench and exits, which is how the match tools measure an
    // engine's speed and how a commit states what its search change did to
    // the tree. No argument starts the uci loop; anything else is a mistake,
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
                println!(
                    "{}",
                    bench::run_suite(
                        &bench::positions(),
                        settings.depth,
                        settings.table_bytes,
                        settings.config
                    )
                );
                ExitCode::SUCCESS
            }
            Err(what) => {
                eprintln!("unrecognised bench {}", what);
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
        for form in ["arche bench", "--version", "--help"] {
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
