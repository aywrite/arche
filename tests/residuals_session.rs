// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2022-2026 Andrew Wright

//! What the residuals argument prints, run against the real binary.
//!
//! The spawning and the splitting are in `report_command`; here is what the
//! shortcuts' own header, rows and summary say. This is the slowest of the
//! three, because the command searches the suite and then searches every
//! sample it took under the reference: minutes at the depths the command is
//! really used at and seconds at the depth here.

mod report_command;

/// A shallow run at a rate that still records plenty: enough to have rows
/// without spending the reference search on thousands of them. About one
/// node in fifty, since the rate is a key rather than a count.
const ARGUMENTS: [&str; 4] = ["residuals", "4", "every", "50"];

#[test]
fn the_residuals_argument_prints_a_header_rows_and_a_summary() {
    let printed = report_command::run(&ARGUMENTS);
    assert!(
        printed
            .header
            .starts_with("residuals depth 4 every 50 taint rule50 positions "),
        "header: {}",
        printed.header
    );
    assert!(printed.events() > 0, "header: {}", printed.header);

    for row in &printed.rows {
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

    // a line a kind at a depth, the kinds in order and each of them
    // together, with the depths a run happened to reach
    let summary = &printed.summary;
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
    order.dedup();
    assert_eq!(order, vec![0, 1, 2], "kinds out of order: {:?}", summary);
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
