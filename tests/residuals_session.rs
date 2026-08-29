// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The residuals argument, run against the real binary.
//!
//! It is an argument and not a uci command, so nothing in the protocol suite
//! reaches it: what it prints is only ever printed by the program. This
//! spawns the executable cargo built, reads the whole run, and asserts on the
//! three parts a reader parses, which are the header, the rows and the
//! summary.
//!
//! The wait is generous because the command searches the suite and then
//! searches every sample it took under the reference, which is minutes at the
//! depths the command is really used at and seconds at the depth here.

use std::io::Read;
use std::process::{Command, Stdio};

/// A shallow run at a rate that still records plenty: enough to have rows
/// without spending the reference search on thousands of them.
const ARGUMENTS: [&str; 4] = ["residuals", "4", "every", "50"];

#[test]
fn the_residuals_argument_prints_a_header_rows_and_a_summary() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_arche"))
        .args(ARGUMENTS)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the binary cargo built should start");
    let mut printed = String::new();
    child
        .stdout
        .take()
        .expect("stdout was piped")
        .read_to_string(&mut printed)
        .expect("the run prints text");
    let status = child.wait().expect("the child can be waited on");
    assert!(status.success(), "exit {:?}: {}", status.code(), printed);

    let lines: Vec<&str> = printed.lines().collect();
    let header = lines.first().unwrap_or(&"");
    assert!(
        header.starts_with("residuals depth 4 every 50 taint rule50 positions "),
        "header: {}",
        header
    );

    let summary_at = lines
        .iter()
        .position(|line| *line == "summary")
        .unwrap_or_else(|| panic!("no summary in:\n{}", printed));
    // the rows sit between the header and the blank line before the summary,
    // and there is at least one or the run measured nothing
    let rows = &lines[1..summary_at - 1];
    assert!(!rows.is_empty(), "no rows in:\n{}", printed);
    for row in rows {
        let words: Vec<&str> = row.split(' ').collect();
        assert!(words.len() > 7, "row: {}", row);
        assert!(
            words[0] == "reverse_futility" || words[0] == "null_move",
            "row: {}",
            row
        );
        // depth, then the four scores, each a number, with the fen after
        // them: a row parses left to right
        for (at, word) in words[1..6].iter().enumerate() {
            assert!(word.parse::<i32>().is_ok(), "field {} of {}", at + 1, row);
        }
        // and the delta is the reference less the claim, worked out here
        // rather than taken on trust
        let claimed: i32 = words[3].parse().unwrap();
        let reference: i32 = words[4].parse().unwrap();
        assert_eq!(
            words[5].parse::<i32>().unwrap(),
            reference - claimed,
            "{row}"
        );
    }

    // a line a kind, whichever of them the run happened to record
    let summary = &lines[summary_at + 1..];
    assert_eq!(summary.len(), 2, "summary: {:?}", summary);
    assert!(
        summary[0].starts_with("reverse_futility "),
        "{}",
        summary[0]
    );
    assert!(summary[1].starts_with("null_move "), "{}", summary[1]);
    assert!(
        summary.iter().any(|line| line.contains(" median ")),
        "no percentiles in: {:?}",
        summary
    );
}

#[test]
fn an_unreadable_setting_fails_with_the_code_the_scripts_check() {
    let status = Command::new(env!("CARGO_BIN_EXE_arche"))
        .args(["residuals", "2", "every", "lots"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the binary cargo built should start");
    assert_eq!(status.code(), Some(2));
}
