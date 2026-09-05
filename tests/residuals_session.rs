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
/// without spending the reference search on thousands of them. About one
/// node in fifty, since the rate is a key rather than a count.
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
    // the denominator, always stated: the rows below say nothing about a
    // rate without it
    let events: u64 = header
        .split(' ')
        .skip_while(|word| *word != "events")
        .nth(1)
        .unwrap_or_else(|| panic!("no events in header: {}", header))
        .parse()
        .unwrap_or_else(|e| panic!("events is not a number in {}: {}", header, e));
    assert!(events > 0, "header: {}", header);

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
        assert!(words.len() > 12, "row: {}", row);
        assert!(
            words[0] == "reverse_futility"
                || words[0] == "null_move"
                || words[0] == "shadow_futility",
            "row: {}",
            row
        );
        assert!(words[2] == "zw" || words[2] == "open", "row: {}", row);
        // the depth, the halfmove clock and the five scores are each a
        // number, and the fen comes after them: a row parses left to right
        for at in [1, 3, 4, 5, 6, 7, 8] {
            assert!(words[at].parse::<i32>().is_ok(), "field {} of {}", at, row);
        }
        // the delta is the reference less the claim, the crossing is the
        // reference against beta and the overstatement is the claim against
        // the reference, all worked out here rather than taken on trust
        let beta: i32 = words[4].parse().unwrap();
        let claimed: i32 = words[6].parse().unwrap();
        let reference: i32 = words[7].parse().unwrap();
        assert_eq!(
            words[8].parse::<i32>().unwrap(),
            reference - claimed,
            "{row}"
        );
        let crossed = if reference < beta { "crossed" } else { "clear" };
        assert_eq!(words[9], crossed, "{row}");
        let overstated = if claimed > reference {
            "overstated"
        } else {
            "held"
        };
        assert_eq!(words[10], overstated, "{row}");
    }

    // a line a kind at a depth, in kind order, with the depths a run
    // happened to reach
    let summary = &lines[summary_at + 1..];
    assert!(summary.len() >= 3, "summary: {:?}", summary);
    let kind_at = |line: &str| {
        if line.starts_with("reverse_futility") {
            0
        } else if line.starts_with("null_move") {
            1
        } else if line.starts_with("shadow_futility") {
            2
        } else {
            panic!("summary line names no kind: {}", line)
        }
    };
    let mut order: Vec<usize> = summary.iter().map(|line| kind_at(line)).collect();
    let grouped = order.clone();
    order.dedup();
    assert_eq!(order, vec![0, 1, 2], "kinds out of order: {:?}", summary);
    assert_eq!(grouped.len(), summary.len());
    assert!(
        summary.iter().any(|line| line.contains(" median ")),
        "no percentiles in: {:?}",
        summary
    );
    // the crossing rate against a named depth is the headline the command
    // exists to print
    assert!(
        summary.iter().any(|line| line.contains(" depth ")
            && line.contains(" crossed ")
            && line.contains(" overstated ")
            && line.contains(" mates ")),
        "no per-depth crossing rate in: {:?}",
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
