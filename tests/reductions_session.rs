// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The reductions argument, run against the real binary.
//!
//! An argument and not a uci command, so nothing in the protocol suite
//! reaches it: what it prints is only ever printed by the program. This
//! spawns the executable cargo built, reads the whole run, and asserts on
//! the three parts a reader parses, which are the header, the rows and the
//! summary. The replay behind the fail lows runs a ply under the sampled
//! nodes, so at this depth it costs a stream of shallow reference searches
//! and stays quick.

use std::io::Read;
use std::process::{Command, Stdio};

/// A shallow run at a rate that still records plenty. Only a late quiet
/// move at depth offers an event, so the stream is sparser than the
/// census's and the rate is lower for it.
const ARGUMENTS: [&str; 4] = ["reductions", "4", "every", "5"];

#[test]
fn the_reductions_argument_prints_a_header_rows_and_a_summary() {
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
        header.starts_with("reductions depth 4 every 5 positions "),
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
    // the rows sit between the header and the blank line before the
    // summary, and there is at least one or the run measured nothing
    let rows = &lines[1..summary_at - 1];
    assert!(!rows.is_empty(), "no rows in:\n{}", printed);
    let mut low = 0;
    let mut labelled = 0;
    for row in rows {
        let words: Vec<&str> = row.split(' ').collect();
        assert!(words.len() > 16, "row: {}", row);
        // the depth, the counts, the history pair, the three distances,
        // and the cost are each a number, and the fen comes after them: a
        // row parses left to right
        for at in [0, 2, 3, 4, 5, 6, 9, 10, 11, 13] {
            assert!(words[at].parse::<i64>().is_ok(), "field {} of {}", at, row);
        }
        assert!(words[1] == "zw" || words[1] == "open", "row: {}", row);
        assert!(words[7] == "killer" || words[7] == "plain", "row: {}", row);
        assert!(
            words[8] == "miss" || words[8] == "move" || words[8] == "score_only",
            "row: {}",
            row
        );
        // a late move is what the ledger records, so no index is under
        // the threshold
        assert!(words[2].parse::<usize>().unwrap() >= 4, "row: {}", row);
        // a fail low carries the replay's answer and its label; a fail
        // high carries neither, said with dashes so the columns stand
        // still
        match words[12] {
            "low" => {
                low += 1;
                assert!(words[14].parse::<i64>().is_ok(), "row: {}", row);
                assert!(
                    words[15] == "harmful" || words[15] == "harmless",
                    "row: {}",
                    row
                );
                labelled += 1;
            }
            "high" => {
                assert_eq!(words[14], "-", "row: {}", row);
                assert_eq!(words[15], "-", "row: {}", row);
            }
            other => panic!("scout {} in: {}", other, row),
        }
    }
    // the fail lows are what the replay labels, so a run that kept none
    // has measured nothing
    assert!(low > 0, "no fail low rows in:\n{}", printed);
    assert!(labelled > 0, "no labelled rows in:\n{}", printed);

    // a line a depth, each carrying the whole shape
    let summary = &lines[summary_at + 1..];
    assert!(!summary.is_empty(), "empty summary in:\n{}", printed);
    for line in summary {
        assert!(line.starts_with("depth "), "summary line: {}", line);
        for word in [
            " scouts ",
            " low ",
            " share ",
            " replayed ",
            " harmful ",
            " rate ",
            " index4-7 ",
            " index8-15 ",
            " index16+ ",
            " hist0 ",
            " hist<0.1 ",
            " hist<0.5 ",
            " hist0.5+ ",
        ] {
            assert!(line.contains(word), "no {} in: {}", word.trim(), line);
        }
    }
}

#[test]
fn an_unreadable_setting_fails_with_the_code_the_scripts_check() {
    let status = Command::new(env!("CARGO_BIN_EXE_arche"))
        .args(["reductions", "2", "every", "lots"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the binary cargo built should start");
    assert_eq!(status.code(), Some(2));
}
