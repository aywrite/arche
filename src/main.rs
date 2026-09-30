// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

use arche::UCI;
use arche::instruments::{INSTRUMENTS, Instrument};
use arche::params::Params;
use arche::uci;
use arche_core::AlphaBeta;
use arche_core::Board;
use std::process::ExitCode;

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
    for Instrument { command, .. } in &INSTRUMENTS {
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

/// A command's report on stdout, or on stderr with exit code 2, which the
/// measuring scripts check: the setting that could not be read, or the
/// failure the run met after reading.
fn measure(instrument: &Instrument, params: &Params) -> ExitCode {
    let run = match (instrument.read)(params) {
        Ok(run) => run,
        Err(what) => {
            eprintln!("unrecognised {} {}", instrument.command.name, what);
            return ExitCode::from(2);
        }
    };
    match run() {
        Ok(report) => {
            print!("{}", report);
            ExitCode::SUCCESS
        }
        Err(failure) => {
            eprintln!("{}", failure);
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
        Some("--version" | "-V") => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            print!("{}", usage());
            ExitCode::SUCCESS
        }
        #[cfg(not(feature = "trace"))]
        Some("trace") => {
            eprintln!("trace: this build has no trace mode; build it with --features trace");
            ExitCode::from(2)
        }
        Some(word) => match INSTRUMENTS.iter().find(|i| i.command.name == word) {
            Some(instrument) => measure(instrument, &params),
            // refused rather than left to the uci loop, which would wait in
            // silence
            None => {
                eprintln!("unrecognised argument: {}", word);
                ExitCode::from(2)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_usage_spells_every_word_every_command_takes() {
        let usage = usage();
        for Instrument { command, .. } in &INSTRUMENTS {
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

    #[test]
    fn the_usage_ends_in_a_newline() {
        // printed with print!
        assert!(usage().ends_with('\n'));
    }
}
