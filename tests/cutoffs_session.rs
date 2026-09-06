// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! The cutoffs argument, run against the real binary.
//!
//! An argument and not a uci command, so nothing in the protocol suite
//! reaches it: what it prints is only ever printed by the program. This
//! spawns the executable cargo built, reads the whole run, and asserts on
//! the three parts a reader parses, which are the header, the rows and the
//! summary. There is no replay behind this command, so the depth here costs
//! one pass over the suite and nothing more.

use std::io::Read;
use std::process::{Command, Stdio};

/// A shallow run at a rate that still records plenty. The census offers an
/// event at every full width node the move loop answers, so fifty keeps
/// hundreds of rows at this depth.
const ARGUMENTS: [&str; 4] = ["cutoffs", "4", "every", "50"];

#[test]
fn the_cutoffs_argument_prints_a_header_rows_and_a_summary() {
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
        header.starts_with("cutoffs depth 4 every 50 positions "),
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
    let mut cut = 0;
    let mut held = 0;
    for row in rows {
        let words: Vec<&str> = row.split(' ').collect();
        assert!(words.len() > 15, "row: {}", row);
        // the depth, the two counts, the history denominator, the eval
        // distance and the cost are each a number, and the fen comes after
        // them: a row parses left to right
        for at in [0, 4, 5, 9, 12, 13] {
            assert!(words[at].parse::<i64>().is_ok(), "field {} of {}", at, row);
        }
        assert!(words[1] == "zw" || words[1] == "open", "row: {}", row);
        assert!(words[2] == "check" || words[2] == "calm", "row: {}", row);
        assert!(
            words[10] == "scored" || words[10] == "unscored",
            "row: {}",
            row
        );
        assert!(
            words[11] == "miss" || words[11] == "move" || words[11] == "score_only",
            "row: {}",
            row
        );
        // a cut row names the cutting move's place, class, history and
        // search; a held row has none of the four, said with dashes so
        // the columns stand still. The index column is defined as the
        // last move searched, so this checks the printer, not the loop
        let searched: usize = words[5].parse().unwrap();
        match words[3] {
            "cut" => {
                cut += 1;
                assert_eq!(words[6].parse::<usize>().unwrap(), searched - 1, "{row}");
                assert!(
                    ["table", "capture", "promotion", "killer", "quiet"].contains(&words[7]),
                    "row: {}",
                    row
                );
                assert!(words[8].parse::<u32>().is_ok(), "row: {}", row);
                assert!(
                    words[14] == "reduced" || words[14] == "full",
                    "row: {}",
                    row
                );
            }
            "held" => {
                held += 1;
                for at in [6, 7, 8, 14] {
                    assert_eq!(words[at], "-", "field {} of {}", at, row);
                }
            }
            other => panic!("outcome {} in: {}", other, row),
        }
    }
    // the contrast between the two outcomes is what the census is for, so
    // a run that kept only one of them has measured nothing
    assert!(cut > 0, "no cut rows in:\n{}", printed);
    assert!(held > 0, "no held rows in:\n{}", printed);

    // a line a depth, each carrying the whole shape
    let summary = &lines[summary_at + 1..];
    assert!(!summary.is_empty(), "empty summary in:\n{}", printed);
    for line in summary {
        assert!(line.starts_with("depth "), "summary line: {}", line);
        for word in [
            " records ",
            " cuts ",
            " rate ",
            " index0 ",
            " index1-3 ",
            " index4+ ",
            " searched cut ",
            " held ",
            " table ",
            " capture ",
            " promotion ",
            " killer ",
            " quiet ",
            " unscored ",
        ] {
            assert!(line.contains(word), "no {} in: {}", word.trim(), line);
        }
    }
}

#[test]
fn an_unreadable_setting_fails_with_the_code_the_scripts_check() {
    let status = Command::new(env!("CARGO_BIN_EXE_arche"))
        .args(["cutoffs", "2", "every", "lots"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the binary cargo built should start");
    assert_eq!(status.code(), Some(2));
}
