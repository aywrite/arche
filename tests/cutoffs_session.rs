// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What the cutoffs argument prints, run against the real binary.
//!
//! The spawning and the splitting are in `report_command`; here is what the
//! census's own header, rows and summary say. There is no replay behind this
//! command, so the depth here costs one pass over the suite and nothing
//! more.

mod report_command;

/// A shallow run at a rate that still records plenty. The census offers an
/// event at every full width node the move loop answers, so fifty keeps
/// hundreds of rows at this depth.
const ARGUMENTS: [&str; 4] = ["cutoffs", "4", "every", "50"];

#[test]
fn the_cutoffs_argument_prints_a_header_rows_and_a_summary() {
    let printed = report_command::run(&ARGUMENTS);
    assert!(
        printed
            .header
            .starts_with("cutoffs depth 4 every 50 positions "),
        "header: {}",
        printed.header
    );
    assert!(printed.events() > 0, "header: {}", printed.header);

    let mut cut = 0;
    let mut held = 0;
    for row in &printed.rows {
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
                // the history column is signed: a move tried more often
                // than it has cut prints under zero
                assert!(words[8].parse::<i32>().is_ok(), "row: {}", row);
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
    assert!(cut > 0, "no cut rows in:\n{}", printed.all);
    assert!(held > 0, "no held rows in:\n{}", printed.all);

    // a line a depth, each carrying the whole shape
    for line in &printed.summary {
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
